//! Direct classification tests for PR follow-up cross-head artifact binding.
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
//! @requirement:REQ-PRFU-002
//!
//! These tests validate the architectural invariants that distinguish
//! "stale prior-head artifact for the same PR" from "artifact for a genuinely
//! different PR," and the cross-head evidence lookup that locates immutable
//! remediation evidence from history by source-head/output-head identity.

use luther_workflow::engine::executors::{
    ArtifactSequenceMetadata, ArtifactWriter, ClockSleeper, PrFollowupArtifactStore,
    PrFollowupBinding, PR_FOLLOWUP_SCHEMA_VERSION,
};
use serde_json::{json, Value};
use std::path::PathBuf;
use tempfile::TempDir;

struct FixedClock;

impl ClockSleeper for FixedClock {
    fn now_rfc3339(&self) -> String {
        "2026-07-12T00:00:00Z".to_string()
    }

    fn sleep(&self, _duration: std::time::Duration) {}
}

fn binding(head_sha: &str, base_sha: Option<&str>) -> PrFollowupBinding {
    PrFollowupBinding {
        schema_version: PR_FOLLOWUP_SCHEMA_VERSION,
        run_id: "run-1".to_string(),
        repository_owner: "example".to_string(),
        repository_name: "workflow".to_string(),
        pr_number: 42,
        head_ref: "feature".to_string(),
        head_sha: head_sha.to_string(),
        base_ref: "main".to_string(),
        base_sha: base_sha.map(ToString::to_string),
    }
}

fn binding_different_pr(head_sha: &str) -> PrFollowupBinding {
    PrFollowupBinding {
        schema_version: PR_FOLLOWUP_SCHEMA_VERSION,
        run_id: "run-1".to_string(),
        repository_owner: "example".to_string(),
        repository_name: "workflow".to_string(),
        pr_number: 99,
        head_ref: "other-feature".to_string(),
        head_sha: head_sha.to_string(),
        base_ref: "main".to_string(),
        base_sha: Some("base-a".to_string()),
    }
}

fn history_root_for(
    store: &PrFollowupArtifactStore,
    b: &PrFollowupBinding,
    artifact_family: &str,
) -> PathBuf {
    store
        .root()
        .join("pr-followup/history")
        .join(&b.run_id)
        .join(&b.repository_owner)
        .join(&b.repository_name)
        .join(b.pr_number.to_string())
        .join(artifact_family)
}

fn write_remediation_result(
    store: &PrFollowupArtifactStore,
    b: &PrFollowupBinding,
    input_head: &str,
    output_head: &str,
    clock: &dyn ClockSleeper,
) {
    let _ = cross_head_binding_classification::cross_head_evidence::write_result_and_get_sequence(
        store,
        b,
        input_head,
        output_head,
        clock,
    );
}

#[cfg(unix)]
#[test]
fn artifacts_written_through_root_alias_recover_from_canonical_root() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().expect("tempdir");
    let real_root = temp.path().join("real-artifacts");
    std::fs::create_dir(&real_root).expect("create real artifact root");
    let alias_root = temp.path().join("artifact-alias");
    symlink(&real_root, &alias_root).expect("create artifact root alias");

    let prior = binding("aaa", Some("ccc"));
    let current = binding("bbb", Some("ccc"));
    let alias_store = PrFollowupArtifactStore::new(alias_root.clone());
    write_remediation_result(&alias_store, &prior, "aaa", "bbb", &FixedClock);

    let canonical_path = alias_store.canonical_path(&prior, "pr-remediation-result");
    let alias_history =
        cross_head_binding_classification::provenance_validation::history_files(&alias_store)
            .into_iter()
            .next()
            .expect("history snapshot");
    let persisted: Value = serde_json::from_str(
        &std::fs::read_to_string(&alias_history).expect("read aliased history snapshot"),
    )
    .expect("parse aliased history snapshot");
    assert_eq!(
        persisted
            .pointer("/history_metadata/canonical_path")
            .and_then(Value::as_str),
        Some(canonical_path.to_string_lossy().as_ref()),
        "the regression must exercise a persisted alias rather than a pre-canonicalized path"
    );
    assert_eq!(
        persisted
            .pointer("/history_metadata/history_path")
            .and_then(Value::as_str),
        Some(alias_history.to_string_lossy().as_ref())
    );

    let canonical_root = real_root.canonicalize().expect("canonical artifact root");
    let reopened = PrFollowupArtifactStore::new(canonical_root);
    let recovered = reopened
        .next_sequence_for_step(&prior, "pr-remediation-result", "next_step")
        .expect("recover sequence through equivalent root path");
    assert_eq!(recovered.artifact_sequence, 2);
    assert_eq!(recovered.write_sequence, 2);

    let history = reopened
        .read_history_json_by_head(&current, "pr-remediation-result", "aaa", Some("bbb"))
        .expect("recover history through equivalent root path")
        .expect("history evidence");
    assert_eq!(
        history.get("input_head_sha").and_then(Value::as_str),
        Some("aaa")
    );

    let carried = reopened
        .read_carried_forward_json(&current, "pr-remediation-result")
        .expect("carry canonical artifact through equivalent root path")
        .expect("carried artifact");
    assert_eq!(
        carried.get("output_head_sha").and_then(Value::as_str),
        Some("bbb")
    );
}

#[cfg(unix)]
#[test]
fn history_provenance_rejects_existing_path_that_escapes_store() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().expect("tempdir");
    let outside = TempDir::new().expect("outside tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let b = binding("aaa", Some("ccc"));
    write_remediation_result(&store, &b, "aaa", "bbb", &FixedClock);

    let history_path =
        cross_head_binding_classification::provenance_validation::history_files(&store).remove(0);
    let outside_path = outside
        .path()
        .join(history_path.file_name().expect("history filename"));
    std::fs::copy(&history_path, &outside_path).expect("copy snapshot outside store");
    let escape_alias = temp.path().join("escape");
    symlink(outside.path(), &escape_alias).expect("create escaping alias");

    let mut value: Value = serde_json::from_str(
        &std::fs::read_to_string(&history_path).expect("read history snapshot"),
    )
    .expect("parse history snapshot");
    value["history_metadata"]["history_path"] = json!(escape_alias
        .join(outside_path.file_name().expect("outside filename"))
        .display()
        .to_string());
    std::fs::write(
        &history_path,
        serde_json::to_vec_pretty(&value).expect("serialize corrupt snapshot"),
    )
    .expect("write corrupt snapshot");

    let error = store
        .read_history_json_by_head(&b, "pr-remediation-result", "aaa", Some("bbb"))
        .expect_err("an existing escaping path must not establish provenance");
    assert!(error.to_string().contains("history path mismatch"));
}

#[test]
fn history_provenance_rejects_nonexistent_embedded_path() {
    let temp = TempDir::new().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let b = binding("aaa", Some("ccc"));
    write_remediation_result(&store, &b, "aaa", "bbb", &FixedClock);

    let history_path =
        cross_head_binding_classification::provenance_validation::history_files(&store).remove(0);
    let mut value: Value = serde_json::from_str(
        &std::fs::read_to_string(&history_path).expect("read history snapshot"),
    )
    .expect("parse history snapshot");
    value["history_metadata"]["history_path"] = json!(temp
        .path()
        .join("missing/history.json")
        .display()
        .to_string());
    std::fs::write(
        &history_path,
        serde_json::to_vec_pretty(&value).expect("serialize corrupt snapshot"),
    )
    .expect("write corrupt snapshot");

    let error = store
        .read_history_json_by_head(&b, "pr-remediation-result", "aaa", Some("bbb"))
        .expect_err("a nonexistent embedded path must fail closed");
    assert!(error.to_string().contains("history path mismatch"));
}

#[test]
fn canonical_provenance_rejects_existing_mismatched_path() {
    let temp = TempDir::new().expect("tempdir");
    let outside = TempDir::new().expect("outside tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let b = binding("aaa", Some("ccc"));
    write_remediation_result(&store, &b, "aaa", "bbb", &FixedClock);

    let canonical_path = store.canonical_path(&b, "pr-remediation-result");
    let outside_path = outside.path().join("copied-result.json");
    std::fs::copy(&canonical_path, &outside_path).expect("copy canonical artifact outside store");
    let mut value: Value = serde_json::from_str(
        &std::fs::read_to_string(&canonical_path).expect("read canonical artifact"),
    )
    .expect("parse canonical artifact");
    value["history_metadata"]["canonical_path"] = json!(outside_path.display().to_string());
    std::fs::write(
        &canonical_path,
        serde_json::to_vec_pretty(&value).expect("serialize corrupt artifact"),
    )
    .expect("write corrupt artifact");

    let error = store
        .read_carried_forward_json(&b, "pr-remediation-result")
        .expect_err("an existing different path must not establish canonical provenance");
    assert!(error.to_string().contains("canonical path mismatch"));
}

mod cross_head_binding_classification {
    pub(super) mod binding_and_reads;
    pub(super) mod cross_head_evidence;
    pub(super) mod provenance_validation;
}
