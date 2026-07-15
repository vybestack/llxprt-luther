use super::super::*;

// ============================================================================
// Canonical overwrite: cross-head evidence from history after a second
// remediation cycle overwrites the canonical envelope.
// ============================================================================

/// Writes a remediation-result artifact for `b` with the given input/output
/// heads, returning the artifact sequence metadata so the test can use it as
/// an exact-identity evidence reference (mimicking what a pending marker
/// action carries).
pub(crate) fn write_result_and_get_sequence(
    store: &PrFollowupArtifactStore,
    b: &PrFollowupBinding,
    input_head: &str,
    output_head: &str,
    clock: &dyn ClockSleeper,
) -> ArtifactSequenceMetadata {
    let payload = json!({
        "validation_state": "valid",
        "overall_status": "success",
        "input_head_sha": input_head,
        "output_head_sha": output_head,
        "results": [{
            "source_type": "coderabbit_feedback",
            "source_id": "item-1",
            "stable_marker_key": "thread:PRRT_1",
            "body_hash": "fnv64:abc",
            "input_head_sha": input_head,
            "output_head_sha": output_head,
            "status": "fixed",
            "action": "test",
            "evidence": { "commands": [] }
        }],
        "retry_scope": {
            "scope_kind": "remediation_result_validation",
            "run_id": b.run_id,
            "repository_owner": b.repository_owner,
            "repository_name": b.repository_name,
            "pr_number": b.pr_number,
            "input_head_sha": input_head,
            "output_head_sha": output_head,
            "plan_artifact_sequence": 1,
            "remediation_attempt_index": 0,
            "max_remediation_attempts": 3,
            "validation_retry_index": 0,
            "max_validation_retries": 2,
            "stale_artifact_retry_index": 0,
            "max_stale_artifact_retries": 2
        }
    });
    let record = store
        .write_json_artifact(JsonArtifactWriteRequest::new(
            ArtifactWriteContext::new(
                b,
                "pr-remediation-result",
                "pr_remediation_result",
                9,
                clock,
            ),
            &payload,
            None,
        ))
        .expect("write remediation result");
    record.sequence
}

/// Canonical overwrite regression: a second remediation cycle on a new head
/// overwrites the canonical `pr-remediation-result` envelope. A pending
/// marker action from the FIRST cycle (carrying the first cycle's exact
/// artifact sequence identity and source_head=A) must locate its evidence
/// from immutable history by exact sequence identity — never from the
/// overwritten canonical envelope.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
#[test]
fn cross_head_evidence_after_canonical_overwrite_finds_prior_from_history() {
    let temp = TempDir::new().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let clock = FixedClock;

    let prior_head = binding("aaa", Some("ccc"));
    let current_head = binding("bbb", Some("ccc"));

    // Cycle 1: remediation result for head A. The pending marker action
    // will carry this exact sequence identity.
    let first_sequence = write_result_and_get_sequence(&store, &prior_head, "aaa", "bbb", &clock);

    // Cycle 2: a second remediation overwrites the canonical envelope with
    // head B's result.
    let _second_sequence =
        write_result_and_get_sequence(&store, &current_head, "bbb", "ccc", &clock);

    // The canonical envelope now carries head B, not head A.
    let canonical = store
        .read_current_json(&current_head, "pr-remediation-result")
        .expect("canonical read");
    assert_eq!(
        canonical.get("input_head_sha").and_then(Value::as_str),
        Some("bbb"),
        "canonical must be overwritten with head B's result"
    );

    // The pending action from cycle 1 carries source_head=A and the first
    // cycle's exact sequence identity. The cross-head evidence lookup must
    // find the first cycle's result from immutable history, not the
    // overwritten canonical.
    let evidence = store
        .read_history_evidence_by_sequence(
            &current_head,
            "pr-remediation-result",
            "aaa",
            Some("bbb"),
            &first_sequence,
        )
        .expect("cross-head evidence lookup should not error")
        .expect("must find prior-head evidence from history");
    assert_eq!(
        evidence.get("input_head_sha").and_then(Value::as_str),
        Some("aaa"),
        "cross-head evidence must be the first cycle's result (input_head=A)"
    );
    assert_eq!(
        evidence.get("artifact_sequence").and_then(Value::as_u64),
        Some(first_sequence.artifact_sequence),
        "cross-head evidence must match the exact artifact_sequence reference"
    );
}

/// Canonical overwrite ambiguity: if two history snapshots for the same PR
/// share the same head identity AND the same sequence identity (a corruption),
/// the exact-sequence lookup must error rather than silently picking one.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
#[test]
fn cross_head_evidence_after_overwrite_rejects_non_exact_filename_collision() {
    let temp = TempDir::new().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let clock = FixedClock;

    let prior_head = binding("aaa", Some("ccc"));
    let current_head = binding("bbb", Some("ccc"));

    let first_sequence = write_result_and_get_sequence(&store, &prior_head, "aaa", "bbb", &clock);

    // Clone the first cycle's history snapshot with the SAME embedded
    // sequence identity and a matching filename stem, creating a genuine
    // collision at the exact-identity level.
    let history_root = history_root_for(&store, &prior_head, "pr-remediation-result");
    let files: Vec<_> = std::fs::read_dir(&history_root)
        .expect("read history dir")
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();
    assert!(!files.is_empty());
    let src = &files[0];
    let stem = src.file_stem().and_then(|n| n.to_str()).expect("stem");
    let clone_path = history_root.join(format!("{stem}-dup.json"));
    clone_history_with_updated_path(src, &clone_path);

    let result = store.read_history_evidence_by_sequence(
        &current_head,
        "pr-remediation-result",
        "aaa",
        Some("bbb"),
        &first_sequence,
    );
    assert!(
        result.is_err(),
        "two snapshots with the same sequence identity must error as ambiguous"
    );
}

/// Canonical overwrite stale-as-absent: after the canonical is overwritten
/// with head B's result, the optional read for the current head returns the
/// head-B result (not absent), because it IS the current head's artifact.
/// But the prior-head canonical read (head A binding) must return None
/// because the canonical now carries head B — a stale prior head.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
#[test]
fn canonical_overwrite_optional_read_returns_current_not_prior() {
    let temp = TempDir::new().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let clock = FixedClock;

    let prior_head = binding("aaa", Some("ccc"));
    let current_head = binding("bbb", Some("ccc"));

    write_result_and_get_sequence(&store, &prior_head, "aaa", "bbb", &clock);
    write_result_and_get_sequence(&store, &current_head, "bbb", "ccc", &clock);

    // Optional read with the current head returns the head-B result.
    let current_result = store
        .read_optional_current_json_for_head(&current_head, "pr-remediation-result")
        .expect("optional read should not error");
    assert!(
        current_result.is_some(),
        "current-head optional read must return the head-B result"
    );
    assert_eq!(
        current_result
            .as_ref()
            .and_then(|v| v.get("input_head_sha"))
            .and_then(Value::as_str),
        Some("bbb"),
        "current-head optional read must carry input_head=B"
    );

    // Optional read with the prior head (A) returns None because the
    // canonical now carries head B — a stale prior head for binding A.
    let prior_result = store
        .read_optional_current_json_for_head(&prior_head, "pr-remediation-result")
        .expect("optional read should not error");
    assert!(
        prior_result.is_none(),
        "prior-head optional read must return None after canonical overwrite with head B"
    );
}

/// Clones a history snapshot to `dest`, updating the embedded
/// `history_metadata.history_path` to match `dest` so the clone passes
/// path-equality validation while preserving the same embedded sequence
/// identity (creating a genuine collision).
fn clone_history_with_updated_path(src: &std::path::Path, dest: &std::path::Path) {
    let content =
        std::fs::read_to_string(src).unwrap_or_else(|err| panic!("read {}: {err}", src.display()));
    let mut value: Value = serde_json::from_str(&content)
        .unwrap_or_else(|err| panic!("parse {}: {err}", src.display()));
    if let Some(meta) = value
        .get_mut("history_metadata")
        .and_then(Value::as_object_mut)
    {
        meta.insert(
            "history_path".to_string(),
            json!(dest.display().to_string()),
        );
    }
    std::fs::write(
        dest,
        serde_json::to_vec_pretty(&value)
            .unwrap_or_else(|err| panic!("serialize {}: {err}", dest.display())),
    )
    .unwrap_or_else(|err| panic!("write {}: {err}", dest.display()));
}

/// Files not written by the artifact ledger are untracked directory noise, not
/// history candidates. They must not affect exact cross-head evidence lookup.
#[test]
fn untracked_non_json_history_file_is_ignored_by_candidate_classification() {
    let temp = TempDir::new().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let prior = binding("aaa", Some("ccc"));
    let current = binding("bbb", Some("ccc"));
    let sequence = write_result_and_get_sequence(&store, &prior, "aaa", "bbb", &FixedClock);
    let history_root = history_root_for(&store, &prior, "pr-remediation-result");
    std::fs::write(
        history_root.join("review-notes.untracked"),
        b"not ledger JSON",
    )
    .expect("write untracked history noise");

    let evidence = store
        .read_history_evidence_by_sequence(
            &current,
            "pr-remediation-result",
            "aaa",
            Some("bbb"),
            &sequence,
        )
        .expect("untracked file must not poison classification")
        .expect("tracked evidence remains discoverable");
    assert_eq!(
        evidence.get("input_head_sha").and_then(Value::as_str),
        Some("aaa")
    );
}
