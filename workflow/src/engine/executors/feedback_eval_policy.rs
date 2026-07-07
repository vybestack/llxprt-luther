use serde_json::{json, Value};

use super::feedback_eval::{
    required_value_string, FeedbackEvaluationRequest, FeedbackEvaluationResponse, RejectReason,
};
use crate::engine::runner::EngineError;

/// LLM invocation adapter seam for feedback evaluation behavior.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017,REQ-PRFU-020
/// @pseudocode lines 8-17
pub trait FeedbackEvaluationAdapter: Send + Sync {
    fn evaluate(&self, request: &FeedbackEvaluationRequest) -> Result<String, EngineError>;
}

const DOWNRANK_REASON: &str = "This is a low-confidence optional nitpick/speculative suggestion, not a concrete product or scope decision requiring maintainer judgment.";
const MAX_EMBEDDED_JSON_CANDIDATES: usize = 1024;
const DOWNRANK_ACTION: &str = "Do not block PR follow-up on this item; leave it for optional future design documentation if maintainers want to expand the scope.";
const DOWNRANK_RESPONSE: &str = "This item is not being treated as needs-user-judgment because it is framed as an optional/speculative nitpick rather than a concrete blocker. It can be revisited as optional design documentation outside this PR follow-up, but it should not block automated remediation.";

pub(super) fn apply_low_confidence_accepted_policy(
    body: &str,
    author_login: &str,
    author_kind: Option<&str>,
    accepted: &mut Value,
) {
    if accepted.get("decision").and_then(Value::as_str) != Some("needs_user_judgment")
        || !is_low_confidence_optional_feedback(body, author_login, author_kind)
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

pub(super) fn apply_low_confidence_needs_judgment_policy(
    request: &FeedbackEvaluationRequest,
    response: &mut FeedbackEvaluationResponse,
) {
    if response.decision != "needs_user_judgment"
        || !is_low_confidence_optional_feedback(
            &request.body,
            &request.author_login,
            request.author_kind.as_deref(),
        )
    {
        return;
    }

    response.decision = "out_of_scope".to_string();
    response.reason = DOWNRANK_REASON.to_string();
    response.recommended_action = Some(DOWNRANK_ACTION.to_string());
    response.response_text = DOWNRANK_RESPONSE.to_string();
}

pub(super) fn is_forbidden_response_field(field: &str, field_value: &Value) -> bool {
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
        lower.as_str(),
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

pub(super) fn parse_feedback_evaluator_json(raw: &str) -> Result<Value, serde_json::Error> {
    match serde_json::from_str(raw) {
        Ok(value) => Ok(value),
        Err(original) => parse_embedded_json_object(raw).ok_or(original),
    }
}

fn is_low_confidence_optional_feedback(
    body: &str,
    author_login: &str,
    author_kind: Option<&str>,
) -> bool {
    let lower = body.to_ascii_lowercase();
    (trusted_low_priority_author(author_login, author_kind)
        && low_priority_feedback(&lower)
        && speculative_feedback(&lower))
        || conditional_design_feedback(&lower, author_login, author_kind)
}

fn trusted_low_priority_author(author_login: &str, author_kind: Option<&str>) -> bool {
    matches!(
        author_login,
        "coderabbitai" | "coderabbitai[bot]" | "coderabbit[bot]"
    ) && matches!(author_kind, Some("Bot"))
}

fn conditional_design_feedback(lower: &str, author_login: &str, author_kind: Option<&str>) -> bool {
    trusted_inline_marker_author(author_login, author_kind)
        && lower.contains("<!-- luther-ocr-inline -->")
        && speculative_feedback(lower)
        && optional_intent_feedback(lower)
}

fn trusted_inline_marker_author(author_login: &str, author_kind: Option<&str>) -> bool {
    matches!(author_login, "github-actions" | "github-actions[bot]")
        && matches!(author_kind, Some("Bot"))
}

fn low_priority_feedback(lower: &str) -> bool {
    lower.contains("nitpick") || lower.contains("trivial")
}

fn optional_intent_feedback(lower: &str) -> bool {
    lower.contains("if cross-repo")
        || lower.contains("if this function is intended")
        || lower.contains("if it is only used internally")
        || lower.contains("if the intent")
        || lower.contains("if the current behavior")
        || lower.contains("if they are used directly")
        || lower.contains("unless overridden")
        || lower.contains("intended for external consumption")
        || (lower.contains("current behavior") && lower.contains("intentional"))
}

fn speculative_feedback(lower: &str) -> bool {
    lower.contains("should consider")
        || lower.contains("consider whether")
        || lower.contains("consider adding a")
        || lower.contains("otherwise document")
        || lower.contains("could potentially")
        || lower.contains("if this becomes")
        || lower.contains("future hardening")
        || lower.contains("optional future")
        || ((lower.contains(" if ") || lower.starts_with("if ") || lower.contains("\nif "))
            && (lower.contains("could") || lower.contains("clarify") || lower.contains("confirm")))
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
    let mut first_object = None;
    let mut search_from = 0usize;
    let mut candidates_seen = 0usize;
    while candidates_seen < MAX_EMBEDDED_JSON_CANDIDATES && search_from < raw.len() {
        let Some(offset) = raw[search_from..].find('{') else {
            break;
        };
        let index = search_from + offset;
        candidates_seen += 1;
        let mut stream = serde_json::Deserializer::from_str(&raw[index..]).into_iter::<Value>();
        let parsed = stream.next();
        search_from = index + stream.byte_offset().max(1);
        let Some(Ok(value)) = parsed else {
            continue;
        };
        if !value.is_object() {
            continue;
        }
        if value.get("item_id").is_some() && value.get("decision").is_some() {
            return Some(value);
        }
        first_object.get_or_insert(value);
    }
    first_object
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_marker_downrank_requires_trusted_author() {
        let body = "<!-- luther-ocr-inline --> if the current behavior is intentional, should consider documenting it";
        let mut accepted = json!({
            "decision": "needs_user_judgment",
            "reason": "model requested judgment"
        });

        apply_low_confidence_accepted_policy(body, "octocat", Some("User"), &mut accepted);

        assert_eq!(
            accepted.get("decision").and_then(Value::as_str),
            Some("needs_user_judgment")
        );
    }

    #[test]
    fn low_priority_downrank_requires_trusted_bot_author() {
        let body = "_🧹 Nitpick_ | _🔵 Trivial_\n\nIf this could affect future orchestration, clarify the timeout semantics.";
        let mut accepted = json!({
            "decision": "needs_user_judgment",
            "reason": "model requested judgment"
        });

        apply_low_confidence_accepted_policy(body, "octocat", Some("User"), &mut accepted);

        assert_eq!(
            accepted.get("decision").and_then(Value::as_str),
            Some("needs_user_judgment")
        );

        apply_low_confidence_accepted_policy(body, "coderabbitai", Some("Bot"), &mut accepted);

        assert_eq!(
            accepted.get("decision").and_then(Value::as_str),
            Some("out_of_scope")
        );
    }

    #[test]
    fn inline_marker_downrank_rejects_spoofed_github_actions_user() {
        let body = "<!-- luther-ocr-inline --> if the current behavior is intentional, should consider documenting it";
        let mut accepted = json!({
            "decision": "needs_user_judgment",
            "reason": "model requested judgment"
        });

        apply_low_confidence_accepted_policy(body, "github-actions", Some("User"), &mut accepted);

        assert_eq!(
            accepted.get("decision").and_then(Value::as_str),
            Some("needs_user_judgment")
        );
    }

    #[test]
    fn inline_marker_downranks_for_trusted_bot_author() {
        let body = "<!-- luther-ocr-inline --> if the current behavior is intentional, should consider documenting it";
        let mut accepted = json!({
            "decision": "needs_user_judgment",
            "reason": "model requested judgment"
        });

        apply_low_confidence_accepted_policy(body, "github-actions", Some("Bot"), &mut accepted);

        assert_eq!(
            accepted.get("decision").and_then(Value::as_str),
            Some("out_of_scope")
        );
    }

    #[test]
    fn inline_marker_downranks_conditional_placeholder_override_feedback() {
        let body = "<!-- luther-ocr-inline --> if they are used directly as-is, confirm these placeholders are always overridden by the launcher";
        let mut accepted = json!({
            "decision": "needs_user_judgment",
            "reason": "model requested judgment"
        });

        apply_low_confidence_accepted_policy(body, "github-actions", Some("Bot"), &mut accepted);

        assert_eq!(
            accepted.get("decision").and_then(Value::as_str),
            Some("out_of_scope")
        );
    }

    #[test]
    fn inline_marker_downranks_for_trusted_bracketed_bot_author() {
        let body = "<!-- luther-ocr-inline --> if the current behavior is intentional, should consider documenting it";
        let mut accepted = json!({
            "decision": "needs_user_judgment",
            "reason": "model requested judgment"
        });

        apply_low_confidence_accepted_policy(
            body,
            "github-actions[bot]",
            Some("Bot"),
            &mut accepted,
        );

        assert_eq!(
            accepted.get("decision").and_then(Value::as_str),
            Some("out_of_scope")
        );
    }

    #[test]
    fn forbidden_response_field_rejects_case_insensitive_batch_fields() {
        assert!(is_forbidden_response_field("RESULTS", &json!("value")));
        assert!(is_forbidden_response_field("Evaluations", &json!("value")));
    }

    #[test]
    fn embedded_json_parser_prefers_feedback_shaped_object() {
        let parsed = parse_embedded_json_object(
            r#"progress {"note":"not the response"} done {"item_id":"i","decision":"valid"}"#,
        )
        .expect("embedded object");

        assert_eq!(parsed.get("item_id").and_then(Value::as_str), Some("i"));
    }
}
