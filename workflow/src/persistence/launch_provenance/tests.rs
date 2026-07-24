//! Unit tests for launch provenance: digest determinism, encoding round-trip,
//! and resume verification semantics.

use super::*;
use crate::workflow::schema::{
    GuardConfig, GuardLimits, ParentOrchestrationConfig, RepoConfig, RuntimeConfig, StepDef,
    TransitionDef, WorkflowConfig, WorkflowType,
};
use std::collections::HashMap;

fn sample_workflow_type() -> WorkflowType {
    WorkflowType {
        workflow_type_id: "test-workflow".to_string(),
        steps: vec![StepDef {
            step_id: "step1".to_string(),
            step_type: "noop".to_string(),
            description: Some("first step".to_string()),
            parameters: Some(serde_json::json!({"key": "value"})),
            produces: Some(vec!["out1".to_string()]),
            consumes: None,
            terminal: Some(false),
            recovery_policy: None,
        }],
        transitions: vec![TransitionDef {
            from: "step1".to_string(),
            to: "step2".to_string(),
            condition: Some("success".to_string()),
            max_iterations: Some(3),
        }],
        guards: GuardConfig {
            max_retries: Some(2),
            timeout_seconds: Some(60),
            require_approval: Some(false),
        },
    }
}

fn sample_config() -> WorkflowConfig {
    let mut variables = HashMap::new();
    variables.insert("target_repo".to_string(), "owner/repo".to_string());
    WorkflowConfig {
        config_id: "test-config".to_string(),
        workflow_type_id: "test-workflow".to_string(),
        runtime: RuntimeConfig {
            timeout_seconds: 120,
            max_retries: 3,
            parallel_steps: None,
            log_level: Some("info".to_string()),
        },
        repo: RepoConfig {
            workspace_strategy: "temp_clone".to_string(),
            branch_template: "workflow-{run_id}".to_string(),
            base_branch: Some("main".to_string()),
            workspace_root: None,
            project_subdir: None,
            artifact_path_base: None,
            diff_path_base: None,
            diff_path_normalization: crate::workflow::schema::DiffPathNormalization::RepoRelative,
        },
        guard_limits: GuardLimits {
            max_iterations: Some(5),
            max_file_changes: Some(50),
            max_tokens: Some(10_000),
            max_cost: Some(5.00),
        },
        variables,
        discovery: None,
        parent_orchestration: ParentOrchestrationConfig::default(),
        merge_required: false,
        merge_strategy: None,
        command_manifest: None,
        target_profile: None,
    }
}

#[test]
fn digest_is_deterministic_for_same_workflow() {
    let workflow = sample_workflow_type();
    let digest1 = compute_workflow_digest(&workflow);
    let digest2 = compute_workflow_digest(&workflow);
    assert_eq!(digest1, digest2);
    assert_eq!(digest1.len(), 64);
    assert!(digest1.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn digest_is_deterministic_for_same_config() {
    let config = sample_config();
    let digest1 = compute_config_digest(&config);
    let digest2 = compute_config_digest(&config);
    assert_eq!(digest1, digest2);
    assert_eq!(digest1.len(), 64);
}

#[test]
fn digest_changes_when_workflow_changes() {
    let mut workflow = sample_workflow_type();
    let original = compute_workflow_digest(&workflow);
    workflow.workflow_type_id = "changed-workflow".to_string();
    let changed = compute_workflow_digest(&workflow);
    assert_ne!(original, changed);
}

#[test]
fn digest_changes_when_config_changes() {
    let mut config = sample_config();
    let original = compute_config_digest(&config);
    config.config_id = "changed-config".to_string();
    let changed = compute_config_digest(&config);
    assert_ne!(original, changed);
}

#[test]
fn digest_independent_of_variable_map_insertion_order() {
    let mut config_a = sample_config();
    let mut config_b = sample_config();
    // Insert in a different order.
    config_a.variables.clear();
    config_b.variables.clear();
    config_a.variables.insert("z".to_string(), "1".to_string());
    config_a.variables.insert("a".to_string(), "2".to_string());
    config_b.variables.insert("a".to_string(), "2".to_string());
    config_b.variables.insert("z".to_string(), "1".to_string());
    assert_eq!(
        compute_config_digest(&config_a),
        compute_config_digest(&config_b)
    );
}

#[test]
fn from_resolved_records_current_schema_and_root() {
    let workflow = sample_workflow_type();
    let config = sample_config();
    let root = Path::new(".");
    let provenance =
        LaunchProvenance::from_resolved(&workflow, &config, root).expect("canonicalize '.'");
    assert!(provenance.schema_is_current());
    assert_eq!(
        decode_config_root(&provenance.canonical_config_root).expect("decode root"),
        std::env::current_dir().unwrap()
    );
    assert_eq!(
        provenance.workflow_digest,
        compute_workflow_digest(&workflow)
    );
    assert_eq!(provenance.config_digest, compute_config_digest(&config));
}

#[test]
fn from_resolved_errors_when_config_root_is_missing() {
    let workflow = sample_workflow_type();
    let config = sample_config();
    let root = Path::new("/this/path/does/not/exist/issue158-provenance");
    let error = LaunchProvenance::from_resolved(&workflow, &config, root)
        .expect_err("missing config root must be a Canonicalize error");
    assert!(
        matches!(error, LaunchProvenanceError::Canonicalize { .. }),
        "expected Canonicalize, got {error:?}"
    );
}

#[test]
fn verify_matches_when_recomputed_identically() {
    let workflow = sample_workflow_type();
    let config = sample_config();
    let root = Path::new(".");
    let provenance =
        LaunchProvenance::from_resolved(&workflow, &config, root).expect("canonicalize '.'");
    let result = verify_provenance(
        &Some(provenance),
        &workflow,
        &config,
        root,
        LegacyAllowed::Denied,
    );
    assert_eq!(result, ProvenanceVerification::Match);
}

#[test]
fn verify_mismatches_on_workflow_change() {
    let workflow = sample_workflow_type();
    let config = sample_config();
    let root = Path::new(".");
    let provenance =
        LaunchProvenance::from_resolved(&workflow, &config, root).expect("canonicalize '.'");
    let mut changed_workflow = workflow;
    changed_workflow.workflow_type_id = "different".to_string();
    let result = verify_provenance(
        &Some(provenance),
        &changed_workflow,
        &config,
        root,
        LegacyAllowed::Denied,
    );
    assert!(matches!(result, ProvenanceVerification::Mismatch(_)));
}

#[test]
fn verify_mismatches_on_config_root_change() {
    let workflow = sample_workflow_type();
    let config = sample_config();
    let root = Path::new(".");
    let provenance =
        LaunchProvenance::from_resolved(&workflow, &config, root).expect("canonicalize '.'");
    let result = verify_provenance(
        &Some(provenance),
        &workflow,
        &config,
        Path::new("/different/config"),
        LegacyAllowed::Denied,
    );
    assert!(matches!(result, ProvenanceVerification::Mismatch(_)));
}

#[test]
fn verify_mismatches_on_schema_version_change() {
    let workflow = sample_workflow_type();
    let config = sample_config();
    let root = Path::new(".");
    let mut provenance =
        LaunchProvenance::from_resolved(&workflow, &config, root).expect("canonicalize '.'");
    provenance.schema_version = 999;
    let result = verify_provenance(
        &Some(provenance),
        &workflow,
        &config,
        root,
        LegacyAllowed::Denied,
    );
    match result {
        ProvenanceVerification::Mismatch(reason) => {
            assert!(reason.contains("schema version"), "reason: {reason}");
        }
        other => panic!("expected Mismatch, got {other:?}"),
    }
}

#[test]
fn verify_legacy_allowed_admits_absent_provenance_with_warning() {
    let workflow = sample_workflow_type();
    let config = sample_config();
    let root = Path::new(".");
    let result = verify_provenance(&None, &workflow, &config, root, LegacyAllowed::Allowed);
    match result {
        ProvenanceVerification::Legacy(warning) => {
            assert!(warning.contains("legacy"), "warning: {warning}");
        }
        other => panic!("expected Legacy, got {other:?}"),
    }
}

#[test]
fn verify_legacy_denied_refuses_absent_provenance() {
    let workflow = sample_workflow_type();
    let config = sample_config();
    let root = Path::new(".");
    let result = verify_provenance(&None, &workflow, &config, root, LegacyAllowed::Denied);
    assert!(matches!(result, ProvenanceVerification::Mismatch(_)));
}

#[test]
fn legacy_allowed_default_is_denied() {
    assert_eq!(LegacyAllowed::default(), LegacyAllowed::Denied);
}

#[test]
fn config_root_round_trips_through_encoding() {
    let root = Path::new("/some/config/root");
    let encoded = encode_config_root(root);
    let decoded = decode_config_root(&encoded).expect("valid encoding round-trips");
    assert_eq!(decoded, root);
}

#[cfg(unix)]
#[test]
fn decode_config_root_rejects_empty_encoding() {
    let error = decode_config_root("").expect_err("empty encoding must be rejected");
    assert!(
        matches!(error, LaunchProvenanceError::InvalidEncoding { reason, .. } if reason.contains("empty")),
        "expected InvalidEncoding(empty), got {error:?}"
    );
}

#[cfg(unix)]
#[test]
fn decode_config_root_rejects_odd_length_hex() {
    let error = decode_config_root("abc").expect_err("odd-length encoding must be rejected");
    assert!(
        matches!(error, LaunchProvenanceError::InvalidEncoding { reason, .. } if reason.contains("odd length")),
        "expected InvalidEncoding(odd length), got {error:?}"
    );
}

#[cfg(unix)]
#[test]
fn decode_config_root_rejects_non_hex_character() {
    let error = decode_config_root("2g").expect_err("non-hex character must be rejected");
    assert!(
        matches!(error, LaunchProvenanceError::InvalidEncoding { reason, .. } if reason.contains("non-hex")),
        "expected InvalidEncoding(non-hex), got {error:?}"
    );
}

#[cfg(unix)]
#[test]
fn decode_config_root_accepts_uppercase_hex() {
    let root = Path::new("/A");
    let encoded = encode_config_root(root);
    // encode produces lowercase; decode must accept both cases.
    let upper = encoded.to_uppercase();
    let decoded = decode_config_root(&upper).expect("uppercase hex must decode");
    assert_eq!(decoded, root);
}

#[test]
fn provenance_serializes_and_round_trips() {
    let workflow = sample_workflow_type();
    let config = sample_config();
    let root = Path::new(".");
    let original =
        LaunchProvenance::from_resolved(&workflow, &config, root).expect("canonicalize '.'");
    let json = serde_json::to_string(&original).expect("serialize");
    let restored: LaunchProvenance = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(restored, original);
}

#[cfg(not(unix))]
#[test]
fn decode_config_root_returns_plain_path_on_non_unix() {
    // On non-Unix targets the config root is stored and decoded as a
    // plain path string, not hex. A path containing non-hex characters
    // must decode successfully.
    let plain = "C:\\config\\root";
    let decoded = decode_config_root(plain).expect("plain path must decode on non-Unix");
    assert_eq!(decoded, Path::new(plain));
}
