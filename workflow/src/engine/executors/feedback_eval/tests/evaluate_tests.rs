//! Feedback evaluation orchestration tests (part 1).

use super::super::*;
use super::support::*;

#[test]
fn default_argv_is_nonempty_and_starts_with_llxprt() {
    let argv = default_feedback_evaluator_argv();
    assert!(!argv.is_empty());
    assert_eq!(argv[0], "llxprt");
}

#[test]
fn build_request_copies_identity_and_binding() {
    let it = item("a");
    let b = binding();
    let req = build_request(&b, &it);
    assert_eq!(req.item_id, "a");
    assert_eq!(req.stable_marker_key, "thread:a");
    assert_eq!(req.body_hash, "hash-a");
    assert_eq!(req.head_sha, "sha-head");
    assert_eq!(req.repository_owner, "acme");
    assert_eq!(req.repository_name, "widget");
    assert_eq!(req.pr_number, 42);
    assert_eq!(req.author_login, "coderabbitai");
    assert_eq!(
        req.allowed_decisions,
        vec!["valid", "invalid", "out_of_scope", "needs_user_judgment"]
    );
}

#[test]
fn feedback_items_parses_valid_items() {
    let feedback = json!({
        "items": [
            {
                "item_id": "i1",
                "stable_marker_key": "thread:i1",
                "body_hash": "h1",
                "commit_sha": "sha1",
                "author_login": "coderabbitai",
                "author_kind": "bot",
                "body": "body text",
                "path": "a.rs",
                "url": "u"
            }
        ]
    });
    let items = feedback_items(&feedback).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].item_id, "i1");
    assert_eq!(items[0].head_sha, "sha1");
    assert_eq!(items[0].path.as_deref(), Some("a.rs"));
}

#[test]
fn feedback_items_prefers_commit_sha_then_head_sha() {
    // Both fields present: commit_sha must win over head_sha.
    let feedback = json!({
        "items": [
            {
                "item_id":"i",
                "stable_marker_key":"k",
                "body_hash":"h",
                "commit_sha":"preferred",
                "head_sha":"fallback",
                "body":"b"
            }
        ]
    });
    let items = feedback_items(&feedback).unwrap();
    assert_eq!(items[0].head_sha, "preferred");
}

#[test]
fn feedback_items_falls_back_to_head_sha_when_commit_sha_absent() {
    let feedback = json!({
        "items": [
            {"item_id":"i","stable_marker_key":"k","body_hash":"h","head_sha":"fallback","body":"b"}
        ]
    });
    let items = feedback_items(&feedback).unwrap();
    assert_eq!(items[0].head_sha, "fallback");
}

#[test]
fn feedback_items_missing_array_errors() {
    let err = feedback_items(&json!({})).unwrap_err();
    assert!(format!("{err:?}").contains("missing items array"));
}

#[test]
fn feedback_items_missing_sha_errors() {
    let feedback =
        json!({"items":[{"item_id":"i","stable_marker_key":"k","body_hash":"h","body":"b"}]});
    let err = feedback_items(&feedback).unwrap_err();
    assert!(format!("{err:?}").contains("commit_sha/head_sha"));
}

#[test]
fn is_coderabbit_summary_item_detects_summary_prefix() {
    let mut it = item("s");
    it.stable_marker_key = "summary:xyz".to_string();
    assert!(is_coderabbit_summary_item(&it));
}

#[test]
fn is_coderabbit_summary_item_detects_body_markers() {
    let mut it = item("s");
    it.stable_marker_key = "thread:s".to_string();
    it.body = "Summary by CodeRabbit".to_string();
    assert!(is_coderabbit_summary_item(&it));
    it.body = "review limit reached".to_string();
    assert!(is_coderabbit_summary_item(&it));
    it.body = "Walkthrough from coderabbit here".to_string();
    assert!(is_coderabbit_summary_item(&it));
}

#[test]
fn is_coderabbit_summary_item_rejects_regular_feedback() {
    let it = item("s");
    assert!(!is_coderabbit_summary_item(&it));
}

#[test]
fn deterministic_evaluation_returns_invalid_for_summary() {
    let mut it = item("s");
    it.stable_marker_key = "summary:x".to_string();
    let value = deterministic_feedback_evaluation(&it, "2026-01-01T00:00:00Z".to_string())
        .expect("summary yields deterministic result");
    assert_eq!(value.get("decision").unwrap(), "invalid");
    assert_eq!(value.get("source").unwrap(), "deterministic");
    assert_eq!(value.get("attempt_count").unwrap(), 0);
}

#[test]
fn deterministic_evaluation_none_for_regular_item() {
    let it = item("s");
    assert!(deterministic_feedback_evaluation(&it, "t".to_string()).is_none());
}

#[test]
fn validate_response_accepts_well_formed_valid_decision() {
    let it = item("a");
    let b = binding();
    let req = request(&it, &b);
    let raw = json!({
        "item_id": "a",
        "stable_marker_key": "thread:a",
        "body_hash": "hash-a",
        "head_sha": "sha-head",
        "decision": "valid",
        "correctness": "high",
        "delivery_scope": "required_acceptance_criterion",
        "reason": "",
        "recommended_action": "do the thing",
        "response_text": "Valid finding."
    })
    .to_string();
    let resp = validate_response(&raw, &req).expect("valid response");
    assert_eq!(resp.decision, "valid");
    assert_eq!(resp.item_id, "a");
    assert_eq!(resp.correctness.as_deref(), Some("high"));
    assert_eq!(
        resp.delivery_scope.as_deref(),
        Some("required_acceptance_criterion")
    );
}

#[test]
fn validate_response_rejects_wrong_item_id() {
    let it = item("a");
    let b = binding();
    let req = request(&it, &b);
    let raw = json!({
        "item_id": "WRONG",
        "stable_marker_key": "thread:a",
        "body_hash": "hash-a",
        "head_sha": "sha-head",
        "decision": "invalid",
        "correctness": "invalid",
        "delivery_scope": "follow_up_issue",
        "reason": "r",
        "recommended_action": "x",
        "response_text": "y"
    })
    .to_string();
    let err = validate_response(&raw, &req).unwrap_err();
    assert_eq!(err.reason, "wrong_item_id");
}

#[test]
fn validate_response_rejects_missing_reason_for_non_valid() {
    let it = item("a");
    let b = binding();
    let req = request(&it, &b);
    let raw = json!({
        "item_id": "a",
        "stable_marker_key": "thread:a",
        "body_hash": "hash-a",
        "head_sha": "sha-head",
        "decision": "invalid",
        "correctness": "invalid",
        "delivery_scope": "follow_up_issue",
        "reason": "   ",
        "recommended_action": "x",
        "response_text": "y"
    })
    .to_string();
    let err = validate_response(&raw, &req).unwrap_err();
    assert_eq!(err.reason, "missing_required_reason");
}

#[test]
fn validate_response_rejects_missing_response_text() {
    let it = item("a");
    let b = binding();
    let req = request(&it, &b);
    let raw = json!({
        "item_id": "a",
        "stable_marker_key": "thread:a",
        "body_hash": "hash-a",
        "head_sha": "sha-head",
        "decision": "valid",
        "correctness": "high",
        "delivery_scope": "required_acceptance_criterion",
        "reason": "",
        "recommended_action": "x",
        "response_text": "  "
    })
    .to_string();
    let err = validate_response(&raw, &req).unwrap_err();
    assert_eq!(err.reason, "missing_response_text");
}
