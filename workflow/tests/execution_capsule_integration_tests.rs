//! Integration tests for the immutable canonical execution capsule
//! (`ExecutionCapsuleV1`): build/verify, canonicalization failure, immutable
//! persist/load, overwrite preservation, tampering, V1/unknown adapter
//! dispatch, envelope digest binding for every authority field, and component
//! digest metadata independence.
//!
//! These tests exercise the **real durable store (SQLite)** directly for
//! persist/load, and assert real invariants for the capsule envelope digest
//! and object-safe adapter dispatch. They are the **RED phase** for P07: they
//! compile and assert real invariants, but tests that reach the designated
//! P08 stubs (`todo!()` in `build_capsule_v1`, `verify_envelope_digest`,
//! `persist_capsule_v1`, `load_capsule_v1`) fail. Tests that exercise
//! already-implemented P06 behavior (`adapter_for`, `build_envelope_frame`,
//! `V1Adapter::envelope_digest`) pass.
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P07

use std::collections::HashMap;
use std::path::Path;

use chrono::Utc;
use rusqlite::Connection;
use sha2::{Digest, Sha256};

use luther_workflow::engine::recovery::adapters::{adapter_for, AdapterError, CapsuleAdapter};
use luther_workflow::engine::recovery::capsule::{
    build_capsule_v1, build_envelope_frame, verify_envelope_digest, CapsuleAuthorityFieldsRef,
    CapsuleError, ExecutionCapsuleV1, CURRENT_CANONICALIZATION_VERSION, CURRENT_DOMAIN_VERSION,
    CURRENT_PROVENANCE_VERSION, CURRENT_SCHEMA_VERSION,
};
use luther_workflow::persistence::capsule_store::{
    init_capsules_table, load_capsule_v1, persist_capsule_v1,
};
use luther_workflow::persistence::launch_provenance::LaunchProvenance;
use luther_workflow::workflow::schema::{
    DiffPathNormalization, GuardConfig, GuardLimits, ParentOrchestrationConfig, RepoConfig,
    RuntimeConfig, StepDef, TransitionDef, WorkflowConfig, WorkflowType,
};

// ===========================================================================
// Test helpers
// ===========================================================================

/// Independently compute a lowercase-hex SHA-256 digest of a byte slice.
///
/// Mirrors `hex_digest` in `launch_provenance.rs` so tests can verify stored
/// digests **without** relying on the implementation under test.
fn independent_sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// A minimal `WorkflowType` for capsule construction.
fn sample_workflow_type() -> WorkflowType {
    WorkflowType {
        workflow_type_id: "capsule-integration-test".to_string(),
        steps: vec![StepDef {
            step_id: "step1".to_string(),
            step_type: "noop".to_string(),
            description: None,
            parameters: None,
            produces: None,
            consumes: None,
            terminal: None,
            recovery_policy: None,
        }],
        transitions: vec![TransitionDef {
            from: "step1".to_string(),
            to: "step2".to_string(),
            condition: None,
            max_iterations: None,
        }],
        guards: GuardConfig {
            max_retries: None,
            timeout_seconds: None,
            require_approval: None,
        },
    }
}

/// A minimal `WorkflowConfig` for capsule construction.
fn sample_config() -> WorkflowConfig {
    WorkflowConfig {
        config_id: "capsule-integration-test-config".to_string(),
        workflow_type_id: "capsule-integration-test".to_string(),
        runtime: RuntimeConfig {
            timeout_seconds: 60,
            max_retries: 1,
            parallel_steps: None,
            log_level: None,
        },
        repo: RepoConfig {
            workspace_strategy: "temp_clone".to_string(),
            branch_template: "wf-{run_id}".to_string(),
            base_branch: Some("main".to_string()),
            workspace_root: None,
            project_subdir: None,
            artifact_path_base: None,
            diff_path_base: None,
            diff_path_normalization: DiffPathNormalization::RepoRelative,
        },
        guard_limits: GuardLimits {
            max_iterations: None,
            max_file_changes: None,
            max_tokens: None,
            max_cost: None,
        },
        variables: HashMap::new(),
        discovery: None,
        parent_orchestration: ParentOrchestrationConfig::default(),
        merge_required: false,
        merge_strategy: None,
        command_manifest: None,
        target_profile: None,
    }
}

/// Construct a `LaunchProvenance` from the current directory.
fn sample_provenance() -> LaunchProvenance {
    LaunchProvenance::from_resolved(&sample_workflow_type(), &sample_config(), Path::new("."))
        .expect("canonicalize '.'")
}

/// Create an in-memory SQLite connection with the capsule table initialized.
fn capsule_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    init_capsules_table(&conn).expect("init capsules table");
    conn
}

/// Owned authority-field data used to build frames and hand-built capsules.
struct AuthorityData {
    run_id: String,
    config_root_encoding: String,
    resolved_workflow_bytes: Vec<u8>,
    resolved_config_bytes: Vec<u8>,
    launch_provenance_digest: String,
    base_ref: String,
}

impl AuthorityData {
    /// Base values for field-sensitivity and capsule-construction tests.
    fn base() -> Self {
        Self {
            run_id: "run-p07-001".to_string(),
            config_root_encoding: "2f657463".to_string(),
            resolved_workflow_bytes: vec![0xDE, 0xAD, 0xBE, 0xEF],
            resolved_config_bytes: vec![0xCA, 0xFE],
            launch_provenance_digest: "aabbccdd11223344".to_string(),
            base_ref: "main".to_string(),
        }
    }

    /// Build a borrowed authority-fields ref from the owned data.
    fn fields_ref(&self) -> CapsuleAuthorityFieldsRef<'_> {
        CapsuleAuthorityFieldsRef {
            schema_version: CURRENT_SCHEMA_VERSION,
            canonicalization_version: CURRENT_CANONICALIZATION_VERSION,
            domain_version: CURRENT_DOMAIN_VERSION,
            provenance_version: CURRENT_PROVENANCE_VERSION,
            run_id: &self.run_id,
            config_root_encoding: &self.config_root_encoding,
            resolved_workflow_bytes: &self.resolved_workflow_bytes,
            resolved_config_bytes: &self.resolved_config_bytes,
            launch_provenance_digest: &self.launch_provenance_digest,
            base_ref: &self.base_ref,
        }
    }

    /// Compute the envelope digest by building the frame and hashing it
    /// independently of the capsule implementation.
    fn envelope_digest(&self) -> String {
        let frame = build_envelope_frame(&self.fields_ref());
        independent_sha256_hex(&frame)
    }
}

/// Hand-build an `ExecutionCapsuleV1` with a correctly-computed envelope digest
/// derived from its own authority fields.
fn handbuilt_capsule(data: &AuthorityData) -> ExecutionCapsuleV1 {
    let digest = data.envelope_digest();
    ExecutionCapsuleV1 {
        schema_version: CURRENT_SCHEMA_VERSION,
        canonicalization_version: CURRENT_CANONICALIZATION_VERSION,
        domain_version: CURRENT_DOMAIN_VERSION,
        provenance_version: CURRENT_PROVENANCE_VERSION,
        run_id: data.run_id.clone(),
        config_root_encoding: data.config_root_encoding.clone(),
        resolved_workflow_bytes: data.resolved_workflow_bytes.clone(),
        resolved_config_bytes: data.resolved_config_bytes.clone(),
        launch_provenance_digest: data.launch_provenance_digest.clone(),
        base_ref: data.base_ref.clone(),
        envelope_digest: digest,
        workflow_digest: "wf-component-metadata".to_string(),
        config_digest: "cfg-component-metadata".to_string(),
        created_at: Utc::now(),
    }
}

// ===========================================================================
// REQ-RP-002: build_capsule_v1 + verify_envelope_digest  [C8]
// ===========================================================================

/// GIVEN: a freshly resolved workflow type + config + provenance + base ref
/// WHEN: `build_capsule_v1(...)` is called
/// THEN: returns a capsule whose `verify_envelope_digest` succeeds [C8]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P07
/// @requirement:REQ-RP-002
#[test]
fn build_capsule_v1_envelope_digest_verifies() {
    let workflow = sample_workflow_type();
    let config = sample_config();
    let provenance = sample_provenance();

    let capsule = build_capsule_v1(
        "run-build-001".to_string(),
        &workflow,
        &config,
        Path::new("."),
        &provenance,
        "main".to_string(),
    )
    .expect("build_capsule_v1 must produce a capsule");

    assert_eq!(
        capsule.envelope_digest.len(),
        64,
        "envelope digest must be a 64-char SHA-256 hex"
    );
    assert!(
        capsule
            .envelope_digest
            .chars()
            .all(|c| c.is_ascii_hexdigit()),
        "envelope digest must be lowercase hex"
    );

    verify_envelope_digest(&capsule)
        .expect("verify_envelope_digest must succeed for a freshly-built capsule");
}

// ===========================================================================
// REQ-RP-002: canonicalization failure
// ===========================================================================

/// GIVEN: a config root path that does not exist on the filesystem
/// WHEN: `build_capsule_v1(...)` is called with that root
/// THEN: returns `CapsuleError::Canonicalize`
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P07
/// @requirement:REQ-RP-002
#[test]
fn build_capsule_v1_non_canonicalizable_config_root_errors() {
    let workflow = sample_workflow_type();
    let config = sample_config();
    let provenance = sample_provenance();
    let missing_root = Path::new("/this/path/does/not/exist/p07-capsule");

    let error = build_capsule_v1(
        "run-build-bad-root".to_string(),
        &workflow,
        &config,
        missing_root,
        &provenance,
        "main".to_string(),
    )
    .expect_err("non-canonicalizable config root must return an error");

    assert!(
        matches!(error, CapsuleError::Canonicalize { ref config_root, .. } if config_root == missing_root),
        "expected CapsuleError::Canonicalize for missing config root, got {error:?}"
    );
}

// ===========================================================================
// REQ-RP-002: immutable persist + load (byte-identical envelope digest)
// ===========================================================================

/// GIVEN: a built capsule persisted to SQLite
/// WHEN: `load_capsule_v1(conn, run_id)` is called
/// THEN: returns a capsule with a byte-identical envelope digest [C8]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P07
/// @requirement:REQ-RP-002
#[test]
fn persist_and_load_returns_byte_identical_envelope_digest() {
    let conn = capsule_conn();
    let workflow = sample_workflow_type();
    let config = sample_config();
    let provenance = sample_provenance();

    let original = build_capsule_v1(
        "run-persist-001".to_string(),
        &workflow,
        &config,
        Path::new("."),
        &provenance,
        "main".to_string(),
    )
    .expect("build_capsule_v1");

    persist_capsule_v1(&conn, &original).expect("persist_capsule_v1");

    let loaded = load_capsule_v1(&conn, "run-persist-001").expect("load_capsule_v1");

    assert_eq!(
        loaded.envelope_digest, original.envelope_digest,
        "loaded capsule must have byte-identical envelope digest"
    );
    assert_eq!(
        loaded.run_id, original.run_id,
        "loaded capsule must preserve run_id"
    );
}

// ===========================================================================
// REQ-RP-002: overwrite preservation (immutability)
// ===========================================================================

/// GIVEN: a capsule persisted for run R
/// WHEN: `persist_capsule_v1` is called again with a modified capsule for R
/// THEN: returns an error (immutable; no overwrite)
/// AND: `load_capsule_v1(conn, R)` still returns the ORIGINAL capsule
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P07
/// @requirement:REQ-RP-002
#[test]
fn re_persist_same_run_id_is_rejected_and_original_preserved() {
    let conn = capsule_conn();
    let workflow = sample_workflow_type();
    let config = sample_config();
    let provenance = sample_provenance();

    let original = build_capsule_v1(
        "run-immutable-001".to_string(),
        &workflow,
        &config,
        Path::new("."),
        &provenance,
        "main".to_string(),
    )
    .expect("build original capsule");
    persist_capsule_v1(&conn, &original).expect("first persist");

    // Build a second capsule with the same run_id but a different base_ref.
    let modified = build_capsule_v1(
        "run-immutable-001".to_string(),
        &workflow,
        &config,
        Path::new("."),
        &provenance,
        "feature/different".to_string(),
    )
    .expect("build modified capsule");

    let re_persist_result = persist_capsule_v1(&conn, &modified);
    assert!(
        re_persist_result.is_err(),
        "re-persisting a capsule for an existing run_id must be rejected (immutable)"
    );

    let loaded = load_capsule_v1(&conn, "run-immutable-001").expect("load original");
    assert_eq!(
        loaded.envelope_digest, original.envelope_digest,
        "load must return the ORIGINAL envelope digest after rejected overwrite"
    );
    assert_ne!(
        loaded.envelope_digest, modified.envelope_digest,
        "loaded capsule must NOT reflect the modified capsule"
    );
}

// ===========================================================================
// REQ-RP-002: tampering detection  [C8]
// ===========================================================================

/// GIVEN: a capsule with a valid envelope digest
/// WHEN: an authority field is tampered with after digest computation
/// THEN: `verify_envelope_digest` returns `CapsuleError::EnvelopeDigestMismatch` [C8]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P07
/// @requirement:REQ-RP-002
#[test]
fn tampered_capsule_envelope_digest_mismatch() {
    let data = AuthorityData::base();
    let mut capsule = handbuilt_capsule(&data);

    // Tamper: change the run_id after the envelope digest was computed.
    capsule.run_id = "run-tampered-different".to_string();

    let error = verify_envelope_digest(&capsule)
        .expect_err("tampered capsule must fail envelope verification");

    assert_eq!(
        error,
        CapsuleError::EnvelopeDigestMismatch,
        "tampered capsule must yield EnvelopeDigestMismatch"
    );
}

// ===========================================================================
// REQ-RP-009: V1 adapter dispatch  [C8]
// ===========================================================================

/// GIVEN: a V1 capsule (schema_version == 1)
/// WHEN: `adapter_for(capsule)` is called
/// THEN: returns a `Box<dyn CapsuleAdapter>` where `.version() == 1` [C8]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P07
/// @requirement:REQ-RP-009
#[test]
fn adapter_for_v1_capsule_returns_version_one() {
    let capsule = handbuilt_capsule(&AuthorityData::base());

    // The type annotation confirms `adapter_for` returns a boxed trait object
    // (object-safe dispatch). [C8]
    let adapter: Box<dyn CapsuleAdapter> = adapter_for(&capsule).expect("adapter_for V1 capsule");

    assert_eq!(
        adapter.version(),
        1,
        "V1 adapter must report version() == 1"
    );
}

// ===========================================================================
// REQ-RP-009: unknown version dispatch (fail-closed)
// ===========================================================================

/// GIVEN: a capsule with `schema_version = 99`
/// WHEN: `adapter_for(capsule)` is called
/// THEN: returns `AdapterError::UnsupportedCapsuleVersion(99)`
///
/// This test would FAIL if `adapter_for` returned V1 for any version.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P07
/// @requirement:REQ-RP-009
#[test]
fn adapter_for_unknown_version_returns_unsupported_error() {
    let mut capsule = handbuilt_capsule(&AuthorityData::base());
    capsule.schema_version = 99;

    let error = match adapter_for(&capsule) {
        Ok(_) => panic!("unknown version must error, got an adapter"),
        Err(e) => e,
    };

    assert_eq!(
        error,
        AdapterError::UnsupportedCapsuleVersion(99),
        "schema_version 99 must yield UnsupportedCapsuleVersion(99)"
    );
}

// ===========================================================================
// REQ-RP-009: V1 adapter envelope_digest matches capsule field  [C8]
// ===========================================================================

/// GIVEN: a V1 capsule with a known envelope digest
/// WHEN: the V1 adapter's `envelope_digest` is called
/// THEN: it returns the capsule's embedded envelope digest [C8]
/// AND: that digest matches the independently-computed digest over the frame
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P07
/// @requirement:REQ-RP-009
#[test]
fn v1_adapter_envelope_digest_matches_capsule_field() {
    let data = AuthorityData::base();
    let capsule = handbuilt_capsule(&data);

    let adapter = adapter_for(&capsule).expect("adapter_for V1 capsule");

    assert_eq!(
        adapter.envelope_digest(&capsule),
        capsule.envelope_digest,
        "V1 adapter envelope_digest must match the capsule's embedded digest"
    );
    assert_eq!(
        adapter.envelope_digest(&capsule),
        data.envelope_digest(),
        "V1 adapter envelope_digest must match the independently-computed digest"
    );
}

// ===========================================================================
// REQ-RP-002: envelope digest binding for every authority field  [C8]
// ===========================================================================

/// GIVEN: a base set of authority fields
/// WHEN: ANY single authority field is changed
/// THEN: the envelope digest changes
///
/// Covers: run_id, config_root_encoding, resolved_workflow_bytes,
/// resolved_config_bytes, launch_provenance_digest, base_ref.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P07
/// @requirement:REQ-RP-002
#[test]
fn envelope_digest_changes_for_every_authority_field() {
    let base = AuthorityData::base();
    let base_digest = base.envelope_digest();

    let mut changed = AuthorityData::base();
    changed.run_id = "run-p07-002".to_string();
    assert_ne!(
        base_digest,
        changed.envelope_digest(),
        "run_id change must alter the envelope digest"
    );

    let mut changed = AuthorityData::base();
    changed.config_root_encoding = "2f686f6d65".to_string();
    assert_ne!(
        base_digest,
        changed.envelope_digest(),
        "config_root_encoding change must alter the envelope digest"
    );

    let mut changed = AuthorityData::base();
    changed.resolved_workflow_bytes = vec![0x00, 0xFF];
    assert_ne!(
        base_digest,
        changed.envelope_digest(),
        "resolved_workflow_bytes change must alter the envelope digest"
    );

    let mut changed = AuthorityData::base();
    changed.resolved_config_bytes = vec![0xBA, 0xBE];
    assert_ne!(
        base_digest,
        changed.envelope_digest(),
        "resolved_config_bytes change must alter the envelope digest"
    );

    let mut changed = AuthorityData::base();
    changed.launch_provenance_digest = "ffeedd9988776655".to_string();
    assert_ne!(
        base_digest,
        changed.envelope_digest(),
        "launch_provenance_digest change must alter the envelope digest"
    );

    let mut changed = AuthorityData::base();
    changed.base_ref = "develop".to_string();
    assert_ne!(
        base_digest,
        changed.envelope_digest(),
        "base_ref change must alter the envelope digest"
    );
}

// ===========================================================================
// REQ-RP-002: component digests are metadata (envelope-digest independence)
// ===========================================================================

/// GIVEN: two capsules with identical authority fields but different component
///        digests (workflow_digest, config_digest)
/// WHEN: the envelope frame is built from their authority fields
/// THEN: the frames are byte-identical and the envelope digests match [C8]
///      (component digests are metadata, not authority)
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P07
/// @requirement:REQ-RP-002
#[test]
fn component_digests_do_not_affect_envelope_digest() {
    let data = AuthorityData::base();

    let mut capsule_a = handbuilt_capsule(&data);
    let mut capsule_b = handbuilt_capsule(&data);
    capsule_a.workflow_digest = "wf-digest-A".to_string();
    capsule_a.config_digest = "cfg-digest-A".to_string();
    capsule_b.workflow_digest = "wf-digest-B".to_string();
    capsule_b.config_digest = "cfg-digest-B".to_string();

    assert_ne!(
        capsule_a.workflow_digest, capsule_b.workflow_digest,
        "test setup: workflow digests must differ"
    );
    assert_ne!(
        capsule_a.config_digest, capsule_b.config_digest,
        "test setup: config digests must differ"
    );

    let frame_a = build_envelope_frame(&capsule_a.authority_fields());
    let frame_b = build_envelope_frame(&capsule_b.authority_fields());
    assert_eq!(
        frame_a, frame_b,
        "component digest changes must NOT alter the envelope frame"
    );

    assert_eq!(
        capsule_a.envelope_digest, capsule_b.envelope_digest,
        "capsules with different component digests must have identical envelope digests"
    );

    let digest_a = independent_sha256_hex(&frame_a);
    let digest_b = independent_sha256_hex(&frame_b);
    assert_eq!(
        digest_a, digest_b,
        "independently-computed envelope digests must match despite differing component digests"
    );
}
