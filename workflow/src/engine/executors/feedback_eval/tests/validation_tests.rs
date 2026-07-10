//! Feedback evaluation validation and helper tests (part 2).

use super::super::*;
use super::support::*;

#[test]
fn validate_response_rejects_array_batch() {
    let it = item("a");
    let b = binding();
    let req = request(&it, &b);
    let raw = json!([{"item_id":"a"}]).to_string();
    let err = validate_response(&raw, &req).unwrap_err();
    assert_eq!(err.reason, "response_array_or_batch");
}

#[test]
fn validate_response_rejects_unknown_decision() {
    let it = item("a");
    let b = binding();
    let req = request(&it, &b);
    let raw = json!({
        "item_id": "a",
        "stable_marker_key": "thread:a",
        "body_hash": "hash-a",
        "head_sha": "sha-head",
        "decision": "maybe",
        "reason": "r",
        "recommended_action": "x",
        "response_text": "y"
    })
    .to_string();
    let err = validate_response(&raw, &req).unwrap_err();
    assert_eq!(err.reason, "unknown_decision");
}

#[test]
fn validate_reusable_accepted_checks_identity_and_binding() {
    let it = item("a");
    let b = binding();
    let value = json!({
        "item_id": "a",
        "stable_marker_key": "thread:a",
        "body_hash": "hash-a",
        "head_sha": "sha-head",
        "decision": "invalid",
        "reason": "r",
        "response_text": "y",
        "repository_owner": "acme",
        "repository_name": "widget",
        "pr_number": 42
    });
    assert!(validate_reusable_accepted(&b, &it, &value).is_ok());
}

#[test]
fn validate_reusable_accepted_rejects_binding_mismatch() {
    let it = item("a");
    let b = binding();
    let value = json!({
        "item_id": "a",
        "stable_marker_key": "thread:a",
        "body_hash": "hash-a",
        "head_sha": "sha-head",
        "decision": "invalid",
        "reason": "r",
        "response_text": "y",
        "repository_owner": "acme",
        "repository_name": "widget",
        "pr_number": 999
    });
    assert!(validate_reusable_accepted(&b, &it, &value).is_err());
}

#[test]
fn exactly_one_accepted_per_item_true_when_balanced() {
    let items = vec![item("a"), item("b")];
    let accepted = vec![
        json!({"item_id":"a","body_hash":"hash-a","head_sha":"sha-head"}),
        json!({"item_id":"b","body_hash":"hash-b","head_sha":"sha-head"}),
    ];
    assert!(exactly_one_accepted_per_item(&items, &accepted));
}

#[test]
fn exactly_one_accepted_per_item_false_on_duplicate() {
    let items = vec![item("a")];
    let accepted = vec![
        json!({"item_id":"a","body_hash":"hash-a","head_sha":"sha-head"}),
        json!({"item_id":"a","body_hash":"hash-a","head_sha":"sha-head"}),
    ];
    assert!(!exactly_one_accepted_per_item(&items, &accepted));
}

#[test]
fn exactly_one_accepted_per_item_false_on_missing() {
    let items = vec![item("a"), item("b")];
    let accepted = vec![json!({"item_id":"a","body_hash":"hash-a","head_sha":"sha-head"})];
    assert!(!exactly_one_accepted_per_item(&items, &accepted));
}

#[test]
fn upsert_state_entry_replaces_matching_entry() {
    let it = item("a");
    let b = binding();
    let mut entries = vec![json!({
        "stable_marker_key": "thread:a",
        "body_hash": "hash-a",
        "head_sha": "sha-head",
        "evaluation_status": "stale"
    })];
    let accepted = accepted_result(
        &FeedbackEvaluationResponse {
            item_id: "a".to_string(),
            stable_marker_key: "thread:a".to_string(),
            body_hash: "hash-a".to_string(),
            head_sha: "sha-head".to_string(),
            decision: "valid".to_string(),
            reason: String::new(),
            recommended_action: Some("x".to_string()),
            response_text: "ok".to_string(),
        },
        "t".to_string(),
        1,
        "llm",
        "not_reused",
    );
    upsert_state_entry(&mut entries, &b, &it, &accepted, "t2".to_string());
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].get("evaluation_status").unwrap(), "accepted");
    let acc = entries[0].get("accepted_evaluation").unwrap();
    assert_eq!(acc.get("repository_owner").unwrap(), "acme");
    assert_eq!(acc.get("pr_number").unwrap(), 42);
}

#[test]
fn accepted_result_shape_has_expected_fields() {
    let resp = FeedbackEvaluationResponse {
        item_id: "a".to_string(),
        stable_marker_key: "thread:a".to_string(),
        body_hash: "hash-a".to_string(),
        head_sha: "sha-head".to_string(),
        decision: "invalid".to_string(),
        reason: "because".to_string(),
        recommended_action: None,
        response_text: "resp".to_string(),
    };
    let value = accepted_result(&resp, "t".to_string(), 2, "llm", "reused");
    assert_eq!(value.get("recommended_action").unwrap(), "");
    assert_eq!(value.get("attempt_count").unwrap(), 2);
    assert_eq!(value.get("source").unwrap(), "llm");
    assert_eq!(value.get("reuse_state").unwrap(), "reused");
}

#[test]
fn unevaluated_item_shape() {
    let it = item("a");
    let value = unevaluated_item(&it, "budget");
    assert_eq!(value.get("item_id").unwrap(), "a");
    assert_eq!(value.get("reason").unwrap(), "budget");
}

#[test]
fn source_artifact_extracts_sequence_fields() {
    let value = json!({
        "artifact_sequence": 5,
        "write_sequence": 1,
        "producer_step_id": "collect"
    });
    let art = source_artifact(&value, "coderabbit-feedback");
    assert_eq!(art.get("artifact_family").unwrap(), "coderabbit-feedback");
    assert_eq!(art.get("artifact_sequence").unwrap(), 5);
    assert_eq!(art.get("producer_step_id").unwrap(), "collect");
}

#[test]
fn require_string_and_u64() {
    let value = json!({"name":"x","count":7});
    assert_eq!(require_string(&value, "name").unwrap(), "x");
    assert!(require_string(&value, "missing").is_err());
    assert!(require_string(&json!({"name":""}), "name").is_err());
    assert_eq!(require_u64(&value, "count").unwrap(), 7);
    assert!(require_u64(&value, "name").is_err());
}

#[test]
fn required_value_string_rejects_empty() {
    let value = json!({"a":"", "b":"ok"});
    assert!(required_value_string(&value, "a").is_err());
    assert_eq!(required_value_string(&value, "b").unwrap(), "ok");
}

#[test]
fn has_unresolved_template_detects_braces() {
    assert!(has_unresolved_template("path/{var}"));
    assert!(has_unresolved_template("open{"));
    assert!(!has_unresolved_template("plain/path"));
}

#[test]
fn u64_param_uses_default_when_absent() {
    let params = json!({"a": 9});
    assert_eq!(u64_param(&params, "a", 1), 9);
    assert_eq!(u64_param(&params, "missing", 3), 3);
}

#[test]
fn sanitize_path_segment_replaces_unsafe_chars() {
    assert_eq!(sanitize_path_segment("a/b:c d"), "a_b_c_d");
    assert_eq!(sanitize_path_segment("keep-._09AZ"), "keep-._09AZ");
}

#[test]
fn stable_json_hash_is_deterministic_and_prefixed() {
    let a = stable_json_hash(&json!({"x":1,"y":2}));
    let b = stable_json_hash(&json!({"x":1,"y":2}));
    assert_eq!(a, b);
    assert!(a.starts_with("fnv64:"));
    let c = stable_json_hash(&json!({"x":2}));
    assert_ne!(a, c);
}

#[test]
fn binding_from_value_roundtrip() {
    let value = json!({
        "schema_version": PR_FOLLOWUP_SCHEMA_VERSION,
        "run_id": "r",
        "repository_owner": "o",
        "repository_name": "n",
        "pr_number": 3,
        "head_ref": "h",
        "head_sha": "hs",
        "base_ref": "b",
        "base_sha": "bs"
    });
    let b = binding_from_value(&value).unwrap();
    assert_eq!(b.run_id, "r");
    assert_eq!(b.pr_number, 3);
    assert_eq!(b.base_sha.as_deref(), Some("bs"));
}

#[test]
fn binding_from_value_missing_field_errors() {
    let value = json!({"schema_version": PR_FOLLOWUP_SCHEMA_VERSION});
    assert!(binding_from_value(&value).is_err());
}

#[test]
fn empty_artifact_defaults() {
    let art = empty_artifact(EvaluationState::Complete, 4, 3, vec![]);
    assert_eq!(art.items_seen, 4);
    assert_eq!(art.max_attempts_per_item, 3);
    assert!(art.accepted_results.is_empty());
    assert_eq!(art.reused_results_count, 0);
}

#[test]
fn reject_batch_response_fields_rejects_non_object() {
    let err = reject_batch_response_fields(&json!("string")).unwrap_err();
    assert_eq!(err.reason, "response_not_object");
}

#[test]
fn reject_captures_decision_and_head_sha() {
    let value = json!({"decision":"invalid","head_sha":"abc"});
    let r = reject("some_reason", &value);
    assert_eq!(r.reason, "some_reason");
    assert_eq!(r.parsed_decision.as_deref(), Some("invalid"));
    assert_eq!(r.observed_head_sha.as_deref(), Some("abc"));
}
