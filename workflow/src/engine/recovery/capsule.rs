//! Immutable canonical execution capsule (`ExecutionCapsuleV1`) with one
//! envelope digest over all replay authority fields. [C8/B9]
//!
//! The capsule is the immutable canonical launch record. It carries explicit
//! canonicalization/schema/domain/provenance versions and **one** envelope
//! digest computed over a framed canonical envelope byte format covering all
//! replay authority fields. Component digests (workflow/config) are metadata,
//! not authority. [C8]
//!
//! The envelope digest is computed over a **framed canonical envelope byte
//! format** with fixed supported version fields followed by length-prefixed
//! authority fields, so the digest is stable and unambiguous regardless of
//! serialization library key ordering. [B9] Adapter dispatch is fail-closed:
//! an unsupported version is rejected before any step executes.
//!
//! `build_capsule_v1` and `verify_envelope_digest` build and verify the
//! immutable capsule at launch and on resume, with fail-closed version
//! dispatch.
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08
//! @requirement:REQ-RP-002

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use sha2::Digest;
use thiserror::Error;

/// Fixed supported capsule schema versions. [B9]
///
/// The envelope digest is only valid over a supported version. A capsule with
/// an unsupported version is rejected fail-closed before any step executes.
pub const SUPPORTED_SCHEMA_VERSIONS: &[u32] = &[1];

/// Fixed supported canonicalization algorithm versions. [B9]
pub const SUPPORTED_CANONICALIZATION_VERSIONS: &[u32] = &[1];

/// Fixed supported workflow/config domain schema versions. [B9]
pub const SUPPORTED_DOMAIN_VERSIONS: &[u32] = &[1];

/// Fixed supported launch-provenance canonical digest versions. [B9]
pub const SUPPORTED_PROVENANCE_VERSIONS: &[u32] = &[1];

/// Current capsule schema version.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Current canonicalization version.
pub const CURRENT_CANONICALIZATION_VERSION: u32 = 1;

/// Current domain version.
pub const CURRENT_DOMAIN_VERSION: u32 = 1;

/// Current provenance version.
pub const CURRENT_PROVENANCE_VERSION: u32 = 1;

/// Immutable canonical launch record. [C8/B9]
///
/// Carries explicit versioning, replay authority fields (all covered by
/// [`Self::envelope_digest`]), the envelope digest (THE authority), and
/// component digests (metadata only). Construction is owned by
/// [`build_capsule_v1`].
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P06
/// @requirement:REQ-RP-002
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExecutionCapsuleV1 {
    // --- Versioning ---
    /// Capsule format version (always 1).
    pub schema_version: u32,
    /// Canonicalization algorithm version.
    pub canonicalization_version: u32,
    /// Workflow/config domain schema version.
    pub domain_version: u32,
    /// LaunchProvenance canonical digest version. [B9]
    pub provenance_version: u32,
    // --- Replay authority fields (all covered by envelope_digest) ---
    /// Unique run identifier.
    pub run_id: String,
    /// Canonical config root, encoded.
    pub config_root_encoding: String,
    /// Canonical serialization of the resolved `WorkflowType`.
    pub resolved_workflow_bytes: Vec<u8>,
    /// Canonical serialization of the resolved `WorkflowConfig`.
    pub resolved_config_bytes: Vec<u8>,
    /// Canonical digest of the actual `LaunchProvenance`.
    pub launch_provenance_digest: String,
    /// Base ref the workflow runs against.
    pub base_ref: String,
    // --- Envelope digest (THE authority) ---
    /// SHA-256 over the framed canonical envelope. [B9]
    pub envelope_digest: String,
    // --- Component digests (metadata only, NOT authority) ---
    /// SHA-256 of `resolved_workflow_bytes`.
    pub workflow_digest: String,
    /// SHA-256 of `resolved_config_bytes`.
    pub config_digest: String,
    // ---
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
}

impl ExecutionCapsuleV1 {
    /// Return the replay authority fields as a borrow suitable for
    /// [`build_envelope_frame`].
    ///
    /// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P06
    /// @requirement:REQ-RP-002
    #[must_use]
    pub fn authority_fields(&self) -> CapsuleAuthorityFieldsRef<'_> {
        CapsuleAuthorityFieldsRef {
            schema_version: self.schema_version,
            canonicalization_version: self.canonicalization_version,
            domain_version: self.domain_version,
            provenance_version: self.provenance_version,
            run_id: &self.run_id,
            config_root_encoding: &self.config_root_encoding,
            resolved_workflow_bytes: &self.resolved_workflow_bytes,
            resolved_config_bytes: &self.resolved_config_bytes,
            launch_provenance_digest: &self.launch_provenance_digest,
            base_ref: &self.base_ref,
        }
    }
}

/// Owned replay authority fields used to build a new capsule envelope frame.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P06
/// @requirement:REQ-RP-002
#[derive(Debug, Clone)]
pub struct CapsuleAuthorityFields {
    /// Capsule schema version.
    pub schema_version: u32,
    /// Canonicalization version.
    pub canonicalization_version: u32,
    /// Domain version.
    pub domain_version: u32,
    /// Provenance version. [B9]
    pub provenance_version: u32,
    /// Run id.
    pub run_id: String,
    /// Encoded canonical config root.
    pub config_root_encoding: String,
    /// Canonical resolved workflow bytes.
    pub resolved_workflow_bytes: Vec<u8>,
    /// Canonical resolved config bytes.
    pub resolved_config_bytes: Vec<u8>,
    /// Launch provenance digest.
    pub launch_provenance_digest: String,
    /// Base ref.
    pub base_ref: String,
}

impl CapsuleAuthorityFields {
    /// Borrow these owned authority fields as a
    /// [`CapsuleAuthorityFieldsRef`].
    pub(crate) fn as_ref(&self) -> CapsuleAuthorityFieldsRef<'_> {
        CapsuleAuthorityFieldsRef {
            schema_version: self.schema_version,
            canonicalization_version: self.canonicalization_version,
            domain_version: self.domain_version,
            provenance_version: self.provenance_version,
            run_id: &self.run_id,
            config_root_encoding: &self.config_root_encoding,
            resolved_workflow_bytes: &self.resolved_workflow_bytes,
            resolved_config_bytes: &self.resolved_config_bytes,
            launch_provenance_digest: &self.launch_provenance_digest,
            base_ref: &self.base_ref,
        }
    }
}

/// Borrowed replay authority fields, for verifying an existing capsule's frame.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P06
/// @requirement:REQ-RP-002
#[derive(Debug, Clone, Copy)]
pub struct CapsuleAuthorityFieldsRef<'a> {
    /// Capsule schema version.
    pub schema_version: u32,
    /// Canonicalization version.
    pub canonicalization_version: u32,
    /// Domain version.
    pub domain_version: u32,
    /// Provenance version. [B9]
    pub provenance_version: u32,
    /// Run id.
    pub run_id: &'a str,
    /// Encoded canonical config root.
    pub config_root_encoding: &'a str,
    /// Canonical resolved workflow bytes.
    pub resolved_workflow_bytes: &'a [u8],
    /// Canonical resolved config bytes.
    pub resolved_config_bytes: &'a [u8],
    /// Launch provenance digest.
    pub launch_provenance_digest: &'a str,
    /// Base ref.
    pub base_ref: &'a str,
}

/// Build the framed canonical envelope byte format. [B9]
///
/// The frame is a deterministic byte sequence with fixed-width (big-endian
/// `u32`) version fields followed by length-prefixed (big-endian `u32`)
/// authority fields, so the digest is stable and unambiguous regardless of
/// serialization library key ordering.
///
/// Frame layout (capsule pseudocode lines 35–55):
/// ```text
/// [schema_version][canonicalization_version][domain_version][provenance_version]
/// [len(run_id)][run_id bytes]
/// [len(config_root_encoding)][config_root_encoding bytes]
/// [len(resolved_workflow_bytes)][resolved_workflow_bytes]
/// [len(resolved_config_bytes)][resolved_config_bytes]
/// [len(launch_provenance_digest)][launch_provenance_digest bytes]
/// [len(base_ref)][base_ref bytes]
/// ```
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P06
/// @requirement:REQ-RP-002
#[must_use]
pub fn build_envelope_frame(fields: &CapsuleAuthorityFieldsRef<'_>) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&fields.schema_version.to_be_bytes());
    buf.extend_from_slice(&fields.canonicalization_version.to_be_bytes());
    buf.extend_from_slice(&fields.domain_version.to_be_bytes());
    buf.extend_from_slice(&fields.provenance_version.to_be_bytes());
    write_len_prefixed(&mut buf, fields.run_id.as_bytes());
    write_len_prefixed(&mut buf, fields.config_root_encoding.as_bytes());
    write_len_prefixed(&mut buf, fields.resolved_workflow_bytes);
    write_len_prefixed(&mut buf, fields.resolved_config_bytes);
    write_len_prefixed(&mut buf, fields.launch_provenance_digest.as_bytes());
    write_len_prefixed(&mut buf, fields.base_ref.as_bytes());
    buf
}

/// Append a length-prefixed byte slice (big-endian `u32` length) to the buffer.
fn write_len_prefixed(buf: &mut Vec<u8>, bytes: &[u8]) {
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(bytes);
}

/// Build a capsule from a freshly resolved type+config+provenance (launch
/// surfaces only). [C8/B9]
///
/// Performs a fail-closed version check before building, canonicalizes the
/// config root, encodes it, computes the resolved workflow/config bytes and
/// digests, computes the launch provenance digest, builds the envelope frame,
/// and computes the envelope digest.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08
/// @requirement:REQ-RP-002
pub fn build_capsule_v1(
    run_id: String,
    workflow_type: &crate::workflow::schema::WorkflowType,
    config: &crate::workflow::schema::WorkflowConfig,
    config_root: &std::path::Path,
    launch_provenance: &crate::persistence::launch_provenance::LaunchProvenance,
    base_ref: String,
) -> Result<ExecutionCapsuleV1, CapsuleError> {
    // Fail-closed version checks before any work. [B9]
    require_supported_schema(CURRENT_SCHEMA_VERSION)?;
    require_supported_canonicalization(CURRENT_CANONICALIZATION_VERSION)?;
    require_supported_domain(CURRENT_DOMAIN_VERSION)?;
    require_supported_provenance(CURRENT_PROVENANCE_VERSION)?;

    // Canonicalize config root and encode for persistence. Fail-closed if the
    // config root cannot be canonicalized (e.g. it was removed after
    // resolution). [B9]
    let config_root_encoding = config_root
        .canonicalize()
        .map_err(|io_error| CapsuleError::Canonicalize {
            config_root: config_root.to_path_buf(),
            io_error: io_error.to_string(),
        })
        .map(|p| crate::persistence::launch_provenance::encode_config_root(&p))?;

    // Canonical resolved workflow/config bytes and component digests
    // (metadata only). [C8]
    let resolved_workflow_bytes =
        crate::persistence::launch_provenance::canonicalize_workflow_type(workflow_type);
    let resolved_config_bytes =
        crate::persistence::launch_provenance::canonicalize_workflow_config(config);
    let workflow_digest =
        crate::persistence::launch_provenance::compute_workflow_digest(workflow_type);
    let config_digest = crate::persistence::launch_provenance::compute_config_digest(config);
    let launch_provenance_digest =
        crate::persistence::launch_provenance::compute_provenance_digest(launch_provenance);

    // Build the envelope frame and compute THE authority digest over it. [B9]
    let fields = CapsuleAuthorityFields {
        schema_version: CURRENT_SCHEMA_VERSION,
        canonicalization_version: CURRENT_CANONICALIZATION_VERSION,
        domain_version: CURRENT_DOMAIN_VERSION,
        provenance_version: CURRENT_PROVENANCE_VERSION,
        run_id: run_id.clone(),
        config_root_encoding: config_root_encoding.clone(),
        resolved_workflow_bytes: resolved_workflow_bytes.clone(),
        resolved_config_bytes: resolved_config_bytes.clone(),
        launch_provenance_digest: launch_provenance_digest.clone(),
        base_ref: base_ref.clone(),
    };
    let envelope_digest = compute_envelope_digest(&fields);

    Ok(ExecutionCapsuleV1 {
        schema_version: CURRENT_SCHEMA_VERSION,
        canonicalization_version: CURRENT_CANONICALIZATION_VERSION,
        domain_version: CURRENT_DOMAIN_VERSION,
        provenance_version: CURRENT_PROVENANCE_VERSION,
        run_id,
        config_root_encoding,
        resolved_workflow_bytes,
        resolved_config_bytes,
        launch_provenance_digest,
        base_ref,
        envelope_digest,
        workflow_digest,
        config_digest,
        created_at: chrono::Utc::now(),
    })
}

/// Verify the one envelope digest over the framed canonical envelope. [C8/B9]
///
/// Performs a fail-closed version dispatch (schema/canonicalization/domain/
/// provenance), rebuilds the envelope frame from the capsule authority fields,
/// recomputes the SHA-256, and compares to the stored digest.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08
/// @requirement:REQ-RP-002
pub fn verify_envelope_digest(capsule: &ExecutionCapsuleV1) -> Result<(), CapsuleError> {
    // Fail-closed version dispatch: reject any unsupported version before any
    // digest computation. [B9]
    require_supported_schema(capsule.schema_version)?;
    require_supported_canonicalization(capsule.canonicalization_version)?;
    require_supported_domain(capsule.domain_version)?;
    require_supported_provenance(capsule.provenance_version)?;

    let recomputed = compute_envelope_digest_from_ref(&capsule.authority_fields());
    if recomputed == capsule.envelope_digest {
        Ok(())
    } else {
        Err(CapsuleError::EnvelopeDigestMismatch)
    }
}

/// Reject a schema version not present in [`SUPPORTED_SCHEMA_VERSIONS`]. [B9]
fn require_supported_schema(version: u32) -> Result<(), CapsuleError> {
    if SUPPORTED_SCHEMA_VERSIONS.contains(&version) {
        Ok(())
    } else {
        Err(CapsuleError::UnsupportedSchema(version))
    }
}

/// Reject a canonicalization version not present in
/// [`SUPPORTED_CANONICALIZATION_VERSIONS`]. [B9]
fn require_supported_canonicalization(version: u32) -> Result<(), CapsuleError> {
    if SUPPORTED_CANONICALIZATION_VERSIONS.contains(&version) {
        Ok(())
    } else {
        Err(CapsuleError::UnsupportedCanonicalization(version))
    }
}

/// Reject a domain version not present in [`SUPPORTED_DOMAIN_VERSIONS`]. [B9]
fn require_supported_domain(version: u32) -> Result<(), CapsuleError> {
    if SUPPORTED_DOMAIN_VERSIONS.contains(&version) {
        Ok(())
    } else {
        Err(CapsuleError::UnsupportedDomain(version))
    }
}

/// Reject a provenance version not present in
/// [`SUPPORTED_PROVENANCE_VERSIONS`]. [B9]
fn require_supported_provenance(version: u32) -> Result<(), CapsuleError> {
    if SUPPORTED_PROVENANCE_VERSIONS.contains(&version) {
        Ok(())
    } else {
        Err(CapsuleError::UnsupportedProvenance(version))
    }
}

/// Compute the envelope digest from owned authority fields. [B9]
fn compute_envelope_digest(fields: &CapsuleAuthorityFields) -> String {
    let frame = build_envelope_frame(&fields.as_ref());
    let mut hasher = sha2::Sha256::new();
    hasher.update(&frame);
    format!("{:x}", hasher.finalize())
}

/// Compute the envelope digest from borrowed authority fields. [B9]
fn compute_envelope_digest_from_ref(fields: &CapsuleAuthorityFieldsRef<'_>) -> String {
    let frame = build_envelope_frame(fields);
    let mut hasher = sha2::Sha256::new();
    hasher.update(&frame);
    format!("{:x}", hasher.finalize())
}

/// Errors produced by capsule construction and verification. [C8/B9]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P06
/// @requirement:REQ-RP-002
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CapsuleError {
    /// The recomputed envelope digest does not match the stored digest. [C8]
    #[error("envelope digest mismatch")]
    EnvelopeDigestMismatch,
    /// The capsule schema version is unsupported. [B9]
    #[error("unsupported capsule schema version: {0}")]
    UnsupportedSchema(u32),
    /// The canonicalization version is unsupported. [B9]
    #[error("unsupported canonicalization version: {0}")]
    UnsupportedCanonicalization(u32),
    /// The domain version is unsupported. [B9]
    #[error("unsupported domain version: {0}")]
    UnsupportedDomain(u32),
    /// The provenance version is unsupported. [B9]
    #[error("unsupported provenance version: {0}")]
    UnsupportedProvenance(u32),
    /// The config root could not be canonicalized.
    #[error(
        "failed to canonicalize config root '{}': {io_error}",
        config_root.display()
    )]
    Canonicalize {
        /// The config root path that could not be canonicalized.
        config_root: PathBuf,
        /// The underlying I/O error message.
        io_error: String,
    },
    /// The persisted config-root encoding is invalid.
    #[error("invalid encoding: {reason} (encoded value was '{encoded}')")]
    InvalidEncoding {
        /// The offending encoded string.
        encoded: String,
        /// Why the encoding is invalid.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_frame_is_deterministic_for_same_fields() {
        let fields = CapsuleAuthorityFieldsRef {
            schema_version: 1,
            canonicalization_version: 1,
            domain_version: 1,
            provenance_version: 1,
            run_id: "run-1",
            config_root_encoding: "2f657463",
            resolved_workflow_bytes: &[0x01, 0x02, 0x03],
            resolved_config_bytes: &[0x04, 0x05],
            launch_provenance_digest: "abc123",
            base_ref: "main",
        };
        let frame_a = build_envelope_frame(&fields);
        let frame_b = build_envelope_frame(&fields);
        assert_eq!(frame_a, frame_b);
    }

    #[test]
    fn envelope_frame_starts_with_four_big_endian_version_u32s() {
        let fields = CapsuleAuthorityFieldsRef {
            schema_version: 1,
            canonicalization_version: 1,
            domain_version: 1,
            provenance_version: 1,
            run_id: "",
            config_root_encoding: "",
            resolved_workflow_bytes: &[],
            resolved_config_bytes: &[],
            launch_provenance_digest: "",
            base_ref: "",
        };
        let frame = build_envelope_frame(&fields);
        assert_eq!(frame.len(), 4 * 4 + 6 * 4);
        for word in frame[..16].chunks_exact(4) {
            assert_eq!(word, [0, 0, 0, 1]);
        }
        for length in frame[16..].chunks_exact(4) {
            assert_eq!(length, [0, 0, 0, 0]);
        }
    }

    #[test]
    fn envelope_frame_distinguishes_concatenation_via_length_prefixes() {
        // Without length prefixes "ab"+"c" would equal "a"+"bc". The frame
        // must differ because the field boundaries are explicit.
        let fields_ab_c = CapsuleAuthorityFieldsRef {
            schema_version: 1,
            canonicalization_version: 1,
            domain_version: 1,
            provenance_version: 1,
            run_id: "ab",
            config_root_encoding: "c",
            resolved_workflow_bytes: &[],
            resolved_config_bytes: &[],
            launch_provenance_digest: "",
            base_ref: "",
        };
        let fields_a_bc = CapsuleAuthorityFieldsRef {
            schema_version: 1,
            canonicalization_version: 1,
            domain_version: 1,
            provenance_version: 1,
            run_id: "a",
            config_root_encoding: "bc",
            resolved_workflow_bytes: &[],
            resolved_config_bytes: &[],
            launch_provenance_digest: "",
            base_ref: "",
        };
        assert_ne!(
            build_envelope_frame(&fields_ab_c),
            build_envelope_frame(&fields_a_bc)
        );
    }

    /// GIVEN: a freshly resolved workflow + config + provenance
    /// WHEN: `build_capsule_v1` is called
    /// THEN: the capsule's own `verify_envelope_digest` succeeds. [C8]
    ///
    /// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08
    /// @requirement:REQ-RP-002
    #[test]
    fn build_capsule_v1_envelope_verifies() {
        let capsule = build_capsule_v1(
            "run-unit-001".to_string(),
            &test_workflow_type(),
            &test_config(),
            std::path::Path::new("."),
            &test_provenance(),
            "main".to_string(),
        )
        .expect("build_capsule_v1 must produce a capsule");
        assert_eq!(capsule.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(
            capsule.envelope_digest.len(),
            64,
            "envelope digest must be a 64-char SHA-256 hex"
        );
        verify_envelope_digest(&capsule)
            .expect("verify_envelope_digest must succeed for a freshly-built capsule");
    }

    /// GIVEN: a config root that does not exist
    /// WHEN: `build_capsule_v1` is called
    /// THEN: returns `CapsuleError::Canonicalize`. [B9]
    ///
    /// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08
    /// @requirement:REQ-RP-002
    #[test]
    fn build_capsule_v1_missing_config_root_errors() {
        let missing = std::path::Path::new("/this/path/does/not/exist/p08-capsule-unit");
        let error = build_capsule_v1(
            "run-unit-bad-root".to_string(),
            &test_workflow_type(),
            &test_config(),
            missing,
            &test_provenance(),
            "main".to_string(),
        )
        .expect_err("missing config root must error");
        assert!(
            matches!(error, CapsuleError::Canonicalize { ref config_root, .. } if config_root == missing),
            "expected Canonicalize, got {error:?}"
        );
    }

    /// GIVEN: a capsule with a tampered authority field
    /// WHEN: `verify_envelope_digest` is called
    /// THEN: returns `EnvelopeDigestMismatch`. [C8]
    ///
    /// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08
    /// @requirement:REQ-RP-002
    #[test]
    fn verify_envelope_digest_rejects_tampered_run_id() {
        let mut capsule = build_capsule_v1(
            "run-unit-tamper".to_string(),
            &test_workflow_type(),
            &test_config(),
            std::path::Path::new("."),
            &test_provenance(),
            "main".to_string(),
        )
        .expect("build_capsule_v1");
        capsule.run_id = "run-tampered-different".to_string();
        let error =
            verify_envelope_digest(&capsule).expect_err("tampered capsule must fail verification");
        assert_eq!(error, CapsuleError::EnvelopeDigestMismatch);
    }

    /// GIVEN: a capsule with an unsupported schema version
    /// WHEN: `verify_envelope_digest` is called
    /// THEN: returns `UnsupportedSchema`. [B9]
    ///
    /// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08
    /// @requirement:REQ-RP-002
    #[test]
    fn verify_envelope_digest_rejects_unsupported_schema() {
        let mut capsule = build_capsule_v1(
            "run-unit-bad-schema".to_string(),
            &test_workflow_type(),
            &test_config(),
            std::path::Path::new("."),
            &test_provenance(),
            "main".to_string(),
        )
        .expect("build_capsule_v1");
        capsule.schema_version = 99;
        let error = verify_envelope_digest(&capsule)
            .expect_err("unsupported schema must fail verification");
        assert_eq!(error, CapsuleError::UnsupportedSchema(99));
    }

    use crate::workflow::schema::{
        GuardConfig, GuardLimits, ParentOrchestrationConfig, RepoConfig, RuntimeConfig, StepDef,
        TransitionDef, WorkflowConfig, WorkflowType,
    };
    use std::collections::HashMap;

    fn test_workflow_type() -> WorkflowType {
        WorkflowType {
            workflow_type_id: "capsule-test".to_string(),
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

    fn test_config() -> WorkflowConfig {
        WorkflowConfig {
            config_id: "capsule-test-config".to_string(),
            workflow_type_id: "capsule-test".to_string(),
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
                diff_path_normalization:
                    crate::workflow::schema::DiffPathNormalization::RepoRelative,
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

    fn test_provenance() -> crate::persistence::launch_provenance::LaunchProvenance {
        crate::persistence::launch_provenance::LaunchProvenance::from_resolved(
            &test_workflow_type(),
            &test_config(),
            std::path::Path::new("."),
        )
        .expect("canonicalize '.'")
    }
}
