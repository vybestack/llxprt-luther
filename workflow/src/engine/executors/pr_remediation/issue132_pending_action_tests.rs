use super::*;
use crate::engine::executors::pr_followup_types::PR_FOLLOWUP_SCHEMA_VERSION;
use crate::engine::executors::SystemClockSleeper;
use std::collections::BTreeSet;

fn binding(head_sha: &str) -> PrFollowupBinding {
    PrFollowupBinding {
        schema_version: PR_FOLLOWUP_SCHEMA_VERSION,
        run_id: "issue-132-producer".to_string(),
        repository_owner: "example".to_string(),
        repository_name: "workflow".to_string(),
        pr_number: 132,
        head_ref: "issue-132".to_string(),
        head_sha: head_sha.to_string(),
        base_ref: "main".to_string(),
        base_sha: Some("base".to_string()),
    }
}

fn marker_item(item_id: &str, marker_key: &str, decision: &str) -> Value {
    json!({
        "source_type": "coderabbit_feedback",
        "source_id": item_id,
        "item_id": item_id,
        "stable_marker_key": marker_key,
        "body_hash": format!("hash-{item_id}"),
        "decision": decision,
        "marker_action": if decision == "valid" { "comment_fixed" } else { "comment_invalid" },
        "reason": decision,
        "response_text": format!("Recorded {decision} feedback."),
        "thread_id": format!("thread-{item_id}"),
        "comment_database_id": 7001,
        "remediation_input_head_sha": if item_id == "A" { "aaa" } else { "bbb" },
        "remediation_result_status": "fixed",
        "remediation_result_evidence": {"commands": []},
        "remediation_result_artifact_sequence": if item_id == "A" { 1 } else { 3 },
        "remediation_result_write_sequence": if item_id == "A" { 1 } else { 2 },
        "remediation_result_producer_step_id": "pr_remediation_result",
        "plan_artifact_sequence": 1,
        "remediation_attempt_index": 0
    })
}

fn pending_actions(store: &PrFollowupArtifactStore, binding: &PrFollowupBinding) -> Vec<Value> {
    store
        .read_carried_forward_json(binding, "pending-feedback-marker-actions")
        .expect("read pending actions")
        .expect("pending actions exist")
        .get("pending_actions")
        .and_then(Value::as_array)
        .cloned()
        .expect("pending actions array")
}

#[test]
fn two_actual_remediation_cycles_preserve_a_and_b_pending_actions() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    write_pending_marker_actions(
        &store,
        &binding("aaa"),
        "validate_remediation_result",
        11,
        &[marker_item("A", "thread:A", "valid")],
        Some("bbb"),
        &SystemClockSleeper,
    )
    .expect("first remediation cycle");
    write_pending_marker_actions(
        &store,
        &binding("bbb"),
        "validate_remediation_result",
        11,
        &[marker_item("B", "thread:B", "valid")],
        Some("ccc"),
        &SystemClockSleeper,
    )
    .expect("second remediation cycle");

    let actions = pending_actions(&store, &binding("bbb"));
    assert_eq!(actions.len(), 2);
    assert!(actions
        .iter()
        .any(|action| action.get("item_id") == Some(&json!("A"))));
    assert!(actions
        .iter()
        .any(|action| action.get("item_id") == Some(&json!("B"))));
}

#[test]
fn clean_invalid_write_preserves_prior_fixed_action() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    write_pending_marker_actions(
        &store,
        &binding("aaa"),
        "validate_remediation_result",
        11,
        &[marker_item("A", "thread:A", "valid")],
        Some("bbb"),
        &SystemClockSleeper,
    )
    .expect("fixed action write");
    write_pending_marker_actions(
        &store,
        &binding("bbb"),
        "build_remediation_plan",
        10,
        &[marker_item("invalid", "thread:invalid", "invalid")],
        None,
        &SystemClockSleeper,
    )
    .expect("clean invalid write");

    let actions = pending_actions(&store, &binding("bbb"));
    assert_eq!(actions.len(), 2);
    assert!(actions.iter().any(|action| {
        action.get("action_kind").and_then(Value::as_str) == Some("comment_fixed")
    }));
    assert!(actions.iter().any(|action| {
        action.get("action_kind").and_then(Value::as_str) == Some("comment_invalid")
    }));
}

#[test]
fn repeated_out_of_scope_item_across_heads_keeps_unique_action_ids() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    for head in ["aaa", "bbb"] {
        write_pending_marker_actions(
            &store,
            &binding(head),
            "build_remediation_plan",
            10,
            &[marker_item("same", "thread:same", "out_of_scope")],
            None,
            &SystemClockSleeper,
        )
        .expect("out-of-scope action write");
    }

    let actions = pending_actions(&store, &binding("bbb"));
    let action_ids = actions
        .iter()
        .filter_map(|action| action.get("action_id").and_then(Value::as_str))
        .collect::<BTreeSet<_>>();
    assert_eq!(actions.len(), 2);
    assert_eq!(action_ids.len(), 2);
    assert!(action_ids.iter().any(|id| id.contains(":aaa:none")));
    assert!(action_ids.iter().any(|id| id.contains(":bbb:none")));
}

#[test]
fn pending_action_producer_does_not_swallow_corrupt_prior() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    write_pending_marker_actions(
        &store,
        &binding("aaa"),
        "validate_remediation_result",
        11,
        &[marker_item("A", "thread:A", "valid")],
        Some("bbb"),
        &SystemClockSleeper,
    )
    .expect("first write");
    std::fs::write(
        store.canonical_path(&binding("aaa"), "pending-feedback-marker-actions"),
        b"not-json",
    )
    .expect("corrupt prior");

    let result = write_pending_marker_actions(
        &store,
        &binding("bbb"),
        "validate_remediation_result",
        11,
        &[marker_item("B", "thread:B", "valid")],
        Some("ccc"),
        &SystemClockSleeper,
    );
    assert!(
        result.is_err(),
        "producer must propagate prior artifact errors"
    );
}

#[test]
fn fixed_action_supersedes_prior_classification_for_same_feedback() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let binding = binding("aaa");
    let invalid = marker_item("same", "thread:same", "invalid");
    write_pending_marker_actions(
        &store,
        &binding,
        "build_remediation_plan",
        10,
        &[invalid],
        None,
        &SystemClockSleeper,
    )
    .expect("invalid action write");
    let fixed = marker_item("same", "thread:same", "valid");
    write_pending_marker_actions(
        &store,
        &binding,
        "validate_remediation_result",
        11,
        &[fixed],
        Some("bbb"),
        &SystemClockSleeper,
    )
    .expect("fixed action write");

    let actions = pending_actions(&store, &binding);
    assert_eq!(actions.len(), 1);
    assert_eq!(
        actions[0].get("action_kind").and_then(Value::as_str),
        Some("comment_fixed")
    );
}
