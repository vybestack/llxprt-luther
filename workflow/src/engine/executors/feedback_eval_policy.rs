use serde_json::{json, Value};

use super::feedback_eval::{
    required_value_string, FeedbackEvaluationRequest, FeedbackEvaluationResponse, RejectReason,
};

const DOWNRANK_REASON: &str = "This is a low-confidence optional nitpick/speculative suggestion, not a concrete product or scope decision requiring maintainer judgment.";
const DOWNRANK_ACTION: &str = "Do not block PR follow-up on this item; leave it for optional future design documentation if maintainers want to expand the scope.";
const DOWNRANK_RESPONSE: &str = "This item is not being treated as needs-user-judgment because it is framed as an optional/speculative nitpick rather than a concrete blocker. It can be revisited as optional design documentation outside this PR follow-up, but it should not block automated remediation.";

pub fn apply_low_confidence_accepted_policy(body: &str, accepted: &mut Value) {
    if accepted.get("decision").and_then(Value::as_str) != Some("needs_user_judgment")
        || !is_low_confidence_optional_feedback(body)
    {
        return;
    }
    if let Some(object) = accepted.as_object_mut() {
        object.insert("decision".to_string(), json!("out_of_scope"));
        object.insert("reason".to_string(), json!(DOWNRANK_REASON));
        object.insert("recommended_action".to_string(), json!(DOWNRANK_ACTION));
        object.insert("response_text".to_string(), json!(DOWNRANK_RESPONSE));
    }
}

pub fn apply_low_confidence_needs_judgment_policy(
    request: &FeedbackEvaluationRequest,
    response: &mut FeedbackEvaluationResponse,
) {
    if response.decision != "needs_user_judgment"
        || !is_low_confidence_optional_feedback(&request.body)
    {
        return;
    }

    response.decision = "out_of_scope".to_string();
    response.reason = DOWNRANK_REASON.to_string();
    response.recommended_action = Some(DOWNRANK_ACTION.to_string());
    response.response_text = DOWNRANK_RESPONSE.to_string();
}

pub fn is_forbidden_response_field(field: &str, field_value: &Value) -> bool {
    let lower = field.to_ascii_lowercase();
    let is_allowed_identity = matches!(
        field,
        "item_id" | "stable_marker_key" | "body_hash" | "head_sha"
    );
    let is_extra_identity = !is_allowed_identity
        && (lower.contains("item")
            || lower.contains("stable_marker")
            || lower.contains("body_hash")
            || lower.contains("head_sha")
            || lower.contains("marker_key"));
    let is_batch_field = matches!(
        field,
        "items"
            | "item_ids"
            | "feedback_items"
            | "feedback_item_ids"
            | "batch"
            | "batches"
            | "results"
            | "evaluations"
    );
    is_batch_field || is_extra_identity || field_value.is_array()
}

pub(super) fn feedback_response_from_value(
    value: &Value,
) -> Result<FeedbackEvaluationResponse, RejectReason> {
    Ok(FeedbackEvaluationResponse {
        item_id: required_value_string(value, "item_id")?,
        stable_marker_key: required_value_string(value, "stable_marker_key")?,
        body_hash: required_value_string(value, "body_hash")?,
        head_sha: required_value_string(value, "head_sha")?,
        decision: required_value_string(value, "decision")?,
        reason: optional_value_string(value, "reason"),
        recommended_action: optional_value_string_opt(value, "recommended_action"),
        response_text: optional_value_string(value, "response_text"),
    })
}

pub fn parse_feedback_evaluator_json(raw: &str) -> Result<Value, serde_json::Error> {
    match serde_json::from_str(raw) {
        Ok(value) => Ok(value),
        Err(original) => parse_embedded_json_object(raw).ok_or(original),
    }
}

fn is_low_confidence_optional_feedback(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    low_priority_feedback(&lower) && speculative_feedback(&lower)
}

fn low_priority_feedback(lower: &str) -> bool {
    lower.contains("_🧹 nitpick_")
        || lower.contains("nitpick")
        || lower.contains("cr-indicator-types:nitpick")
        || lower.contains("trivial")
}

fn speculative_feedback(lower: &str) -> bool {
    lower.contains(" if ")
        || lower.contains("could")
        || lower.contains("should consider")
        || lower.contains("confirm")
        || lower.contains("clarify")
        || lower.contains("otherwise document")
        || lower.contains("future")
        || lower.contains("potential")
}

fn optional_value_string(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn optional_value_string_opt(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn parse_embedded_json_object(raw: &str) -> Option<Value> {
    for (index, _) in raw.match_indices('{') {
        let mut stream = serde_json::Deserializer::from_str(&raw[index..]).into_iter::<Value>();
        if let Some(Ok(value)) = stream.next() {
            if value.is_object() {
                return Some(value);
            }
        }
    }
    None
}
