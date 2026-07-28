//! Two-axis correctness/delivery_scope validation tests (issue 142).

use super::super::*;
use super::support::*;

#[test]
fn validate_response_requires_correctness_on_new_responses() {
    let it = item("a");
    let b = binding();
    let req = request(&it, &b);
    let raw = json!({
        "item_id": "a",
        "stable_marker_key": "thread:a",
        "body_hash": "hash-a",
        "head_sha": "sha-head",
        "decision": "valid",
        "delivery_scope": "required_acceptance_criterion",
        "reason": "",
        "recommended_action": "fix it",
        "response_text": "Valid finding."
    })
    .to_string();
    let err = validate_response(&raw, &req).unwrap_err();
    assert_eq!(err.reason, "missing_correctness");
}

#[test]
fn validate_response_requires_delivery_scope_on_new_responses() {
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
        "reason": "",
        "recommended_action": "fix it",
        "response_text": "Valid finding."
    })
    .to_string();
    let err = validate_response(&raw, &req).unwrap_err();
    assert_eq!(err.reason, "missing_delivery_scope");
}

#[test]
fn validate_response_rejects_invalid_correctness_value() {
    let it = item("a");
    let b = binding();
    let req = request(&it, &b);
    let raw = json!({
        "item_id": "a",
        "stable_marker_key": "thread:a",
        "body_hash": "hash-a",
        "head_sha": "sha-head",
        "decision": "valid",
        "correctness": "critical",
        "delivery_scope": "required_acceptance_criterion",
        "reason": "",
        "recommended_action": "fix it",
        "response_text": "Valid finding."
    })
    .to_string();
    let err = validate_response(&raw, &req).unwrap_err();
    assert_eq!(err.reason, "invalid_correctness");
}

#[test]
fn validate_response_rejects_invalid_delivery_scope_value() {
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
        "delivery_scope": "whenever",
        "reason": "",
        "recommended_action": "fix it",
        "response_text": "Valid finding."
    })
    .to_string();
    let err = validate_response(&raw, &req).unwrap_err();
    assert_eq!(err.reason, "invalid_delivery_scope");
}

#[test]
fn validate_response_accepts_all_legal_correctness_values() {
    for correctness in LEGAL_CORRECTNESS_VALUES {
        let it = item("a");
        let b = binding();
        let req = request(&it, &b);
        let raw = json!({
            "item_id": "a",
            "stable_marker_key": "thread:a",
            "body_hash": "hash-a",
            "head_sha": "sha-head",
            "decision": "valid",
            "correctness": correctness,
            "delivery_scope": "required_acceptance_criterion",
            "reason": "",
            "recommended_action": "fix it",
            "response_text": "Valid finding."
        })
        .to_string();
        let resp = validate_response(&raw, &req)
            .unwrap_or_else(|err| panic!("correctness={correctness} should be accepted: {err:?}"));
        assert_eq!(resp.correctness.as_deref(), Some(*correctness));
    }
}

#[test]
fn validate_response_accepts_all_legal_delivery_scope_values() {
    for delivery_scope in LEGAL_DELIVERY_SCOPE_VALUES {
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
            "delivery_scope": delivery_scope,
            "reason": "",
            "recommended_action": "fix it",
            "response_text": "Valid finding."
        })
        .to_string();
        let resp = validate_response(&raw, &req).unwrap_or_else(|err| {
            panic!("delivery_scope={delivery_scope} should be accepted: {err:?}")
        });
        assert_eq!(resp.delivery_scope.as_deref(), Some(*delivery_scope));
    }
}

#[test]
fn validate_response_rejects_empty_correctness_string() {
    let it = item("a");
    let b = binding();
    let req = request(&it, &b);
    let raw = json!({
        "item_id": "a",
        "stable_marker_key": "thread:a",
        "body_hash": "hash-a",
        "head_sha": "sha-head",
        "decision": "valid",
        "correctness": "",
        "delivery_scope": "required_acceptance_criterion",
        "reason": "",
        "recommended_action": "fix it",
        "response_text": "Valid finding."
    })
    .to_string();
    let err = validate_response(&raw, &req).unwrap_err();
    assert_eq!(err.reason, "missing_correctness");
}

#[test]
fn validate_response_rejects_empty_delivery_scope_string() {
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
        "delivery_scope": "",
        "reason": "",
        "recommended_action": "fix it",
        "response_text": "Valid finding."
    })
    .to_string();
    let err = validate_response(&raw, &req).unwrap_err();
    assert_eq!(err.reason, "missing_delivery_scope");
}

#[test]
fn validate_reusable_accepted_accepts_missing_axes_for_legacy_artifacts() {
    let it = item("a");
    let b = binding();
    let value = json!({
        "item_id": "a",
        "stable_marker_key": "thread:a",
        "body_hash": "hash-a",
        "head_sha": "sha-head",
        "decision": "valid",
        "reason": "",
        "recommended_action": "do the thing",
        "response_text": "y",
        "repository_owner": "acme",
        "repository_name": "widget",
        "pr_number": 42
    });
    assert!(
        validate_reusable_accepted(&b, &it, &value).is_ok(),
        "historical artifacts without axes must remain readable for backward compatibility"
    );
}

#[test]
fn validate_reusable_accepted_accepts_partial_axes_for_transitional_artifacts() {
    let it = item("a");
    let b = binding();
    let value = json!({
        "item_id": "a",
        "stable_marker_key": "thread:a",
        "body_hash": "hash-a",
        "head_sha": "sha-head",
        "decision": "valid",
        "correctness": "medium",
        "reason": "",
        "recommended_action": "do the thing",
        "response_text": "y",
        "repository_owner": "acme",
        "repository_name": "widget",
        "pr_number": 42
    });
    assert!(
        validate_reusable_accepted(&b, &it, &value).is_ok(),
        "transitional artifacts with partial axes must remain readable"
    );
}

#[test]
fn validate_reusable_accepted_accepts_invalid_axis_values_for_legacy_artifacts() {
    let it = item("a");
    let b = binding();
    let value = json!({
        "item_id": "a",
        "stable_marker_key": "thread:a",
        "body_hash": "hash-a",
        "head_sha": "sha-head",
        "decision": "valid",
        "correctness": "bogus_severity",
        "delivery_scope": "nonsense_scope",
        "reason": "",
        "recommended_action": "do the thing",
        "response_text": "y",
        "repository_owner": "acme",
        "repository_name": "widget",
        "pr_number": 42
    });
    assert!(
        validate_reusable_accepted(&b, &it, &value).is_ok(),
        "historical artifacts with unrecognized axis values must remain readable; \
         the legacy projection handles them downstream"
    );
}

#[test]
fn accepted_result_preserves_axes_in_output_value() {
    let resp = FeedbackEvaluationResponse {
        item_id: "a".to_string(),
        stable_marker_key: "thread:a".to_string(),
        body_hash: "hash-a".to_string(),
        head_sha: "sha-head".to_string(),
        decision: "valid".to_string(),
        correctness: Some("blocker".to_string()),
        delivery_scope: Some("regression_from_current_patch".to_string()),
        reason: String::new(),
        recommended_action: Some("fix".to_string()),
        response_text: "response".to_string(),
    };
    let value = accepted_result(&resp, "t".to_string(), 1, "new", "not_reused");
    assert_eq!(
        value.get("correctness").and_then(Value::as_str),
        Some("blocker")
    );
    assert_eq!(
        value.get("delivery_scope").and_then(Value::as_str),
        Some("regression_from_current_patch")
    );
}

#[test]
fn deterministic_summary_evaluation_carries_valid_axes() {
    let mut it = item("s");
    it.stable_marker_key = "summary:x".to_string();
    let value = deterministic_feedback_evaluation(&it, "2026-01-01T00:00:00Z".to_string())
        .expect("summary yields deterministic result");
    assert_eq!(
        value.get("correctness").and_then(Value::as_str),
        Some("invalid")
    );
    assert_eq!(
        value.get("delivery_scope").and_then(Value::as_str),
        Some("user_decision")
    );
}
