//! Two-axis finding disposition routing for remediation plan building.
//!
//! When an accepted feedback-evaluation result carries explicit `correctness`
//! and `delivery_scope` fields (issue 142), the plan routes it through the
//! deterministic [`disposition_action`] matrix instead of the legacy
//! single-axis `decision` field. Historical artifacts that lack the two-axis
//! fields project compatibly via [`disposition_from_accepted_result`].
//!
//! Key invariants:
//! - `RemediateNow` → `must_fix`
//! - `DeferToFollowUp` → `deferred_followups` (durably recorded, does not fail)
//! - `BlockForUserDecision` → `needs_user_judgment` (blocks delivery)
//! - `Ignore` → silently dropped (never enters any plan list)

use serde_json::{json, Value};

use crate::engine::executors::pr_followup_types::value_has_summary_marker_key;
use crate::engine::executors::scope_control::finding_disposition::{
    disposition_action, disposition_from_accepted_result, DispositionAction,
};

/// Routes a single accepted feedback-evaluation result into the appropriate
/// plan list based on its two-axis disposition.
///
/// Summary/walkthrough markers (detected via [`value_has_summary_marker_key`])
/// are always suppressed from `mark_invalid` regardless of disposition, to
/// preserve the existing readiness-signal-only semantics.
pub(super) fn route_accepted_result(
    result: &Value,
    binding: &crate::engine::executors::pr_followup_types::PrFollowupBinding,
    evaluations: &Value,
    must_fix: &mut Vec<Value>,
    mark_invalid: &mut Vec<Value>,
    needs_user_judgment: &mut Vec<Value>,
    deferred_followups: &mut Vec<Value>,
) {
    let has_explicit_axes = result.get("correctness").and_then(Value::as_str).is_some()
        || result
            .get("delivery_scope")
            .and_then(Value::as_str)
            .is_some();
    let legacy_decision = result.get("decision").and_then(Value::as_str);
    if !has_explicit_axes
        && matches!(legacy_decision, Some("invalid" | "out_of_scope"))
        && !value_has_summary_marker_key(result)
    {
        mark_invalid.push(feedback_plan_item(
            result,
            "coderabbit_feedback",
            binding,
            evaluations,
        ));
        return;
    }

    let disposition = disposition_from_accepted_result(result);
    let action = disposition_action(disposition.correctness, disposition.delivery_scope);

    let is_summary = value_has_summary_marker_key(result);

    match action {
        DispositionAction::RemediateNow => {
            must_fix.push(feedback_plan_item(result, "coderabbit_feedback", binding, evaluations));
        }
        DispositionAction::DeferToFollowUp => {
            if disposition.correctness
                == crate::engine::executors::scope_control::finding_disposition::FindingCorrectness::Invalid
                && !is_summary
            {
                mark_invalid.push(feedback_plan_item(
                    result,
                    "coderabbit_feedback",
                    binding,
                    evaluations,
                ));
            } else {
                deferred_followups.push(feedback_plan_item(
                    result,
                    "coderabbit_feedback",
                    binding,
                    evaluations,
                ));
            }
        }
        DispositionAction::BlockForUserDecision => {
            needs_user_judgment.push(feedback_plan_item(
                result,
                "coderabbit_feedback",
                binding,
                evaluations,
            ));
        }
        DispositionAction::Ignore => {
            // Invalid findings with summary marker keys are readiness signals
            // only; never enter any plan list.
        }
    }
}

/// Build a remediation plan item from a feedback-evaluation result.
fn feedback_plan_item(
    result: &Value,
    source_type: &str,
    binding: &crate::engine::executors::pr_followup_types::PrFollowupBinding,
    evaluations: &Value,
) -> Value {
    let mut item = json!({
        "source_type": source_type,
        "source_id": string_field(result, "item_id", "unknown-feedback-item"),
        "stable_marker_key": result.get("stable_marker_key").cloned().unwrap_or(Value::Null),
        "reason": string_field(result, "reason", "no_reason_provided"),
        "recommended_action": string_field(result, "recommended_action", "human_review_required"),
        "response_text": result.get("response_text").cloned().unwrap_or(Value::Null),
        "thread_id": result.get("thread_id").cloned().unwrap_or(Value::Null),
        "input_head_sha": binding.head_sha,
        "source_artifact_sequence": artifact_sequence(evaluations),
        "decision": result.get("decision").cloned().unwrap_or(Value::Null),
        "body_hash": result.get("body_hash").cloned().unwrap_or(Value::Null),
        "evidence": result
    });
    if let Some(obj) = item.as_object_mut() {
        if let Some(correctness) = result.get("correctness").and_then(Value::as_str) {
            obj.insert(
                "correctness".to_string(),
                Value::String(correctness.to_string()),
            );
        }
        if let Some(delivery_scope) = result.get("delivery_scope").and_then(Value::as_str) {
            obj.insert(
                "delivery_scope".to_string(),
                Value::String(delivery_scope.to_string()),
            );
        }
    }
    item
}

fn string_field(value: &Value, field: &str, default: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| default.to_string())
}

fn artifact_sequence(value: &Value) -> Value {
    value
        .get("artifact_sequence")
        .cloned()
        .unwrap_or(Value::Null)
}
