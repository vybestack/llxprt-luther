//! PR follow-through remediation, verification, push, guard, and terminal contracts.
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
//! @requirement:REQ-PRFU-013,REQ-PRFU-020
//! @pseudocode lines 1-53

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::engine::executor::{interpolate_string, StepContext, StepExecutor};
use crate::engine::executors::pr_followup_artifacts::{
    ArtifactWriter, ClockSleeper, PrFollowupArtifactStore, SystemClockSleeper,
    SystemPrFollowupFilesystem,
};
use crate::engine::executors::pr_followup_types::{
    is_summary_marker_key, PlanState, PrFollowupBinding, ValidationState,
    PR_FOLLOWUP_SCHEMA_VERSION,
};
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;

/// Remediation plan executor for `pr_remediation_plan`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013,REQ-PRFU-020
/// @pseudocode lines 1-11
#[derive(Debug, Default)]
pub struct PrRemediationPlanExecutor;

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013,REQ-PRFU-020
/// @pseudocode lines 1-11
impl StepExecutor for PrRemediationPlanExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        build_remediation_plan(context, params, &SystemClockSleeper)
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 1-11
#[derive(Clone, Debug, serde::Serialize)]
struct RemediationPlanArtifact {
    plan_state: PlanState,
    must_fix: Vec<Value>,
    mark_invalid: Vec<Value>,
    needs_user_judgment: Vec<Value>,
    pending_or_unknown: Vec<Value>,
    source_artifacts: Vec<Value>,
    built_at: String,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 9-10
#[derive(Clone, Debug, serde::Serialize)]
struct PendingMarkerActionsArtifact {
    pending_actions: Vec<Value>,
    carry_forward_from_artifact_sequence: Option<u64>,
    marker_policy: Value,
    updated_at: String,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 1-11
// Pre-existing remediation planning flow; split in a dedicated refactor stage.
#[allow(clippy::too_many_lines)]
fn build_remediation_plan(
    context: &StepContext,
    params: &Value,
    clock: &dyn ClockSleeper,
) -> Result<StepOutcome, EngineError> {
    let artifact_root = artifact_root(context, params)?;
    let store =
        PrFollowupArtifactStore::with_filesystem(&artifact_root, &SystemPrFollowupFilesystem)?;
    let fallback = binding_from_params(context, params);
    let step_id = current_step_id(context, "build_remediation_plan");
    let step_order = u64_param(params, "step_order_index", 7);

    ensure_legacy_harness_inputs(&store, &fallback, clock)?;

    let pr = match store.read_current_json(&fallback, "pr") {
        Ok(value) => value,
        Err(err) => {
            return write_fatal_plan(
                &store,
                &fallback,
                &step_id,
                step_order,
                clock,
                "missing_or_unbindable_pr",
                vec![json!({ "artifact_family": "pr", "error": err.to_string() })],
                json!({ "error": err.to_string() }),
            );
        }
    };
    let binding = match binding_from_value(&pr) {
        Ok(binding) => binding,
        Err(err) => {
            return write_fatal_plan(
                &store,
                &fallback,
                &step_id,
                step_order,
                clock,
                "missing_or_unbindable_pr",
                vec![source_artifact(&pr, "pr")],
                json!({ "error": err.to_string() }),
            );
        }
    };

    let inputs = match read_plan_inputs(&store, &binding) {
        Ok(inputs) => inputs,
        Err(err) => {
            return write_fatal_plan(
                &store,
                &binding,
                &step_id,
                step_order,
                clock,
                "missing_or_unbindable_plan_input",
                vec![
                    source_artifact(&pr, "pr"),
                    json!({ "error": err.to_string() }),
                ],
                json!({ "error": err.to_string() }),
            );
        }
    };

    let mut must_fix = Vec::new();
    let mut mark_invalid = Vec::new();
    let mut needs_user_judgment = Vec::new();
    let mut pending_or_unknown = Vec::new();

    if let Some(pending) = inputs
        .ci_failures
        .get("pending_or_unknown")
        .and_then(Value::as_array)
    {
        for entry in pending {
            pending_or_unknown.push(entry.clone());
            needs_user_judgment.push(json!({
                "source_type": "ci_pending_or_unknown",
                "source_id": stable_source_id(entry, "pending_or_unknown"),
                "reason": "pending_or_unknown_check_state",
                "recommended_action": "human_review_required",
                "input_head_sha": binding.head_sha,
                "source_artifact_sequence": artifact_sequence(&inputs.ci_failures),
                "evidence": entry
            }));
        }
    }

    if inputs
        .coderabbit_feedback
        .get("readiness_state")
        .and_then(Value::as_str)
        != Some("ready")
    {
        needs_user_judgment.push(json!({
            "source_type": "coderabbit_feedback_readiness",
            "source_id": "coderabbit-feedback",
            "reason": "coderabbit_feedback_not_ready_or_fatal",
            "recommended_action": "human_review_required",
            "input_head_sha": binding.head_sha,
            "source_artifact_sequence": artifact_sequence(&inputs.coderabbit_feedback),
            "evidence": inputs.coderabbit_feedback.get("readiness_state").cloned().unwrap_or(Value::Null)
        }));
    }

    for (index, failure) in inputs
        .ci_failures
        .get("failures")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        must_fix.push(ci_must_fix_item(
            failure,
            index,
            &binding,
            &inputs.ci_failures,
        ));
    }

    if inputs
        .evaluations
        .get("evaluation_state")
        .and_then(Value::as_str)
        != Some("complete")
    {
        needs_user_judgment.push(json!({
            "source_type": "feedback_evaluation_state",
            "source_id": "feedback-evaluations",
            "reason": "feedback_evaluation_not_complete",
            "recommended_action": "human_review_required",
            "input_head_sha": binding.head_sha,
            "source_artifact_sequence": artifact_sequence(&inputs.evaluations),
            "evidence": inputs.evaluations.get("evaluation_state").cloned().unwrap_or(Value::Null)
        }));
    }

    append_evaluation_blockers(
        &inputs.evaluations,
        "unevaluated_items",
        "feedback_item_unevaluated",
        &binding,
        &mut needs_user_judgment,
    );
    append_evaluation_blockers(
        &inputs.evaluations,
        "budget_exhausted_items",
        "feedback_evaluation_budget_exhausted",
        &binding,
        &mut needs_user_judgment,
    );

    for result in inputs
        .evaluations
        .get("accepted_results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match result.get("decision").and_then(Value::as_str) {
            Some("valid") => must_fix.push(feedback_plan_item(
                result,
                "coderabbit_feedback",
                &binding,
                &inputs.evaluations,
            )),
            Some("invalid" | "out_of_scope") => {
                // CodeRabbit summary/walkthrough comments are deterministically
                // classified "invalid" purely as a readiness signal. They are
                // informational only and must not become mark_invalid plan
                // entries (which would later materialize a top-level PR comment).
                if !is_summary_plan_item(result) {
                    mark_invalid.push(feedback_plan_item(
                        result,
                        "coderabbit_feedback",
                        &binding,
                        &inputs.evaluations,
                    ));
                }
            }
            Some("needs_user_judgment") => needs_user_judgment.push(feedback_plan_item(
                result,
                "coderabbit_feedback",
                &binding,
                &inputs.evaluations,
            )),
            Some(other) => needs_user_judgment.push(json!({
                "source_type": "coderabbit_feedback",
                "source_id": string_field(result, "item_id", "unknown-feedback-item"),
                "reason": format!("unknown_feedback_decision:{other}"),
                "recommended_action": "human_review_required",
                "input_head_sha": binding.head_sha,
                "source_artifact_sequence": artifact_sequence(&inputs.evaluations),
                "evidence": result
            })),
            None => needs_user_judgment.push(json!({
                "source_type": "coderabbit_feedback",
                "source_id": string_field(result, "item_id", "unknown-feedback-item"),
                "reason": "missing_feedback_decision",
                "recommended_action": "human_review_required",
                "input_head_sha": binding.head_sha,
                "source_artifact_sequence": artifact_sequence(&inputs.evaluations),
                "evidence": result
            })),
        }
    }

    let plan_state = if !needs_user_judgment.is_empty() {
        PlanState::BlockedNeedsUserJudgment
    } else if must_fix.is_empty() {
        PlanState::Clean
    } else {
        PlanState::NeedsRemediation
    };

    let payload = RemediationPlanArtifact {
        plan_state,
        must_fix,
        mark_invalid: mark_invalid.clone(),
        needs_user_judgment,
        pending_or_unknown,
        source_artifacts: vec![
            source_artifact(&pr, "pr"),
            source_artifact(&inputs.ci_failures, "ci-failures"),
            source_artifact(&inputs.coderabbit_feedback, "coderabbit-feedback"),
            source_artifact(&inputs.evaluations, "feedback-evaluations"),
        ],
        built_at: clock.now_rfc3339(),
    };

    let failure = if matches!(plan_state, PlanState::BlockedNeedsUserJudgment) {
        Some((
            plan_state.as_str(),
            "needs_user_judgment_required",
            json!({
                "needs_user_judgment_count": payload.needs_user_judgment.len(),
                "pending_or_unknown_count": payload.pending_or_unknown.len()
            }),
        ))
    } else {
        None
    };
    write_plan_artifact(
        &store, &binding, &step_id, step_order, &payload, clock, failure,
    )?;

    if matches!(plan_state, PlanState::Clean) {
        write_pending_marker_actions_for_invalid_feedback(
            &store,
            &binding,
            &step_id,
            step_order,
            &mark_invalid,
            clock,
        )?;
    }

    Ok(match plan_state {
        PlanState::Clean => StepOutcome::Success,
        PlanState::NeedsRemediation => StepOutcome::Fixable,
        PlanState::BlockedNeedsUserJudgment | PlanState::Fatal => StepOutcome::Fatal,
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 1-3
#[derive(Clone, Debug)]
struct PlanInputs {
    ci_failures: Value,
    coderabbit_feedback: Value,
    evaluations: Value,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 1-3
fn read_plan_inputs(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
) -> Result<PlanInputs, EngineError> {
    Ok(PlanInputs {
        ci_failures: store.read_current_json(binding, "ci-failures")?,
        coderabbit_feedback: store.read_current_json(binding, "coderabbit-feedback")?,
        evaluations: store.read_current_json(binding, "feedback-evaluations")?,
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 5-8
fn ci_must_fix_item(
    failure: &Value,
    index: usize,
    binding: &PrFollowupBinding,
    ci_failures: &Value,
) -> Value {
    json!({
        "source_type": "ci_failure",
        "source_id": failure.get("failure_id")
            .or_else(|| failure.get("check_id"))
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("ci_failure_{index}")),
        "stable_marker_key": Value::Null,
        "reason": failure.get("check_name").or_else(|| failure.get("conclusion")).cloned().unwrap_or_else(|| json!("ci_failure")),
        "recommended_action": "fix_ci_failure",
        "input_head_sha": binding.head_sha,
        "source_artifact_sequence": artifact_sequence(ci_failures),
        "evidence": failure
    })
}

/// Returns true when a remediation-flow JSON value (an accepted evaluation result
/// or a materialized plan item) is a CodeRabbit summary/walkthrough marker.
/// Summary items are informational readiness signals and must be suppressed from
/// `mark_invalid` routing and pending marker actions.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-020
fn is_summary_plan_item(value: &Value) -> bool {
    value
        .get("stable_marker_key")
        .and_then(Value::as_str)
        .is_some_and(is_summary_marker_key)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 6-8
fn feedback_plan_item(
    result: &Value,
    source_type: &str,
    binding: &PrFollowupBinding,
    evaluations: &Value,
) -> Value {
    json!({
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
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 8-10
fn append_evaluation_blockers(
    evaluations: &Value,
    field: &str,
    reason: &str,
    binding: &PrFollowupBinding,
    needs_user_judgment: &mut Vec<Value>,
) {
    for item in evaluations
        .get(field)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        needs_user_judgment.push(json!({
            "source_type": "coderabbit_feedback",
            "source_id": stable_source_id(item, field),
            "stable_marker_key": item.get("stable_marker_key").cloned().unwrap_or(Value::Null),
            "reason": reason,
            "recommended_action": "human_review_required",
            "input_head_sha": binding.head_sha,
            "source_artifact_sequence": artifact_sequence(evaluations),
            "evidence": item
        }));
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 9-10
fn write_pending_marker_actions_for_invalid_feedback(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    mark_invalid: &[Value],
    clock: &dyn ClockSleeper,
) -> Result<(), EngineError> {
    if mark_invalid.is_empty() {
        return Ok(());
    }

    write_pending_marker_actions(
        store,
        binding,
        step_id,
        step_order,
        mark_invalid,
        None,
        clock,
    )
}

fn write_pending_marker_actions(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    items: &[Value],
    remediation_output_head_sha: Option<&str>,
    clock: &dyn ClockSleeper,
) -> Result<(), EngineError> {
    let prior = store
        .read_current_json(binding, "pending-feedback-marker-actions")
        .ok();
    let carry_forward = prior
        .as_ref()
        .and_then(|value| value.get("artifact_sequence"))
        .and_then(Value::as_u64);
    let mut pending_actions: Vec<Value> = prior
        .as_ref()
        .and_then(|value| value.get("pending_actions"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // Carry-forward pruning: drop any prior summary-keyed action so stale
    // pre-fix pending summary actions are removed instead of re-persisted.
    pending_actions.retain(|action| !is_summary_plan_item(action));
    let mut seen: BTreeSet<String> = pending_actions
        .iter()
        .filter_map(|action| {
            action
                .get("idempotency_key")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .collect();

    for item in items {
        // Defensive second gate: never materialize a pending action for an
        // informational summary/walkthrough marker, regardless of upstream
        // routing.
        if is_summary_plan_item(item) {
            continue;
        }
        let action = pending_marker_action(binding, item, remediation_output_head_sha);
        if let Some(key) = action.get("idempotency_key").and_then(Value::as_str) {
            if seen.insert(key.to_string()) {
                pending_actions.push(action);
            }
        }
    }

    pending_actions.sort_by(|left, right| {
        left.get("idempotency_key")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .cmp(
                right
                    .get("idempotency_key")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            )
    });

    let remediation_output_head_sha_value = remediation_output_head_sha
        .map(|head| json!(head))
        .unwrap_or(Value::Null);
    let payload = PendingMarkerActionsArtifact {
        pending_actions,
        carry_forward_from_artifact_sequence: carry_forward,
        marker_policy: json!({
            "invalid": "comment_invalid",
            "out_of_scope": "comment_out_of_scope",
            "fixed": "comment_fixed",
            "changed": "comment_fixed",
            "remediation_output_head_sha": remediation_output_head_sha_value
        }),
        updated_at: clock.now_rfc3339(),
    };
    store.write_json_artifact(
        binding,
        "pending-feedback-marker-actions",
        step_id,
        step_order,
        &payload,
        None,
        clock,
    )?;
    Ok(())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 7-9
fn pending_marker_action(
    binding: &PrFollowupBinding,
    item: &Value,
    remediation_output_head_sha: Option<&str>,
) -> Value {
    let source_id = string_field(item, "source_id", "unknown-feedback-item");
    let stable_marker_key = item
        .get("stable_marker_key")
        .and_then(Value::as_str)
        .unwrap_or(&source_id)
        .to_string();
    let decision = item
        .get("decision")
        .and_then(Value::as_str)
        .unwrap_or("invalid");
    let marker_action = item
        .get("marker_action")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let action_kind = if marker_action == "comment_fixed" {
        "comment_fixed"
    } else if decision == "out_of_scope" {
        "comment_out_of_scope"
    } else {
        "comment_invalid"
    };
    let body_hash = item
        .get("body_hash")
        .and_then(Value::as_str)
        .unwrap_or("no-body-hash")
        .to_string();
    let remediation_output_head_value = remediation_output_head_sha
        .map(|head| json!(head))
        .unwrap_or(Value::Null);
    let remediation_output_head = remediation_output_head_sha.unwrap_or("none");
    let remediation_input_head_sha = item
        .get("remediation_input_head_sha")
        .cloned()
        .unwrap_or_else(|| json!(binding.head_sha));
    let idempotency_key = format!(
        "{}:{}:{}:{}:{}:{}:{}:{}",
        binding.run_id,
        binding.repository_owner,
        binding.repository_name,
        binding.pr_number,
        binding.head_sha,
        remediation_output_head,
        stable_marker_key,
        action_kind
    );

    json!({
        "action_id": format!("{action_kind}:{stable_marker_key}:{body_hash}:{remediation_output_head}"),
        "action_kind": action_kind,
        "item_id": source_id,
        "original_feedback_identity": {
            "item_id": source_id,
            "stable_marker_key": stable_marker_key,
            "body_hash": body_hash,
            "source_head_sha": binding.head_sha,
            "thread_id": item.get("thread_id").cloned().unwrap_or(Value::Null),
            "comment_database_id": item.get("comment_database_id").cloned().unwrap_or(Value::Null)
        },
        "thread_id": item.get("thread_id").cloned().unwrap_or(Value::Null),
        "comment_database_id": item.get("comment_database_id").cloned().unwrap_or(Value::Null),
        "stable_marker_key": stable_marker_key,
        "source_head_sha": binding.head_sha,
        "remediation_input_head_sha": remediation_input_head_sha,
        "remediation_output_head_sha": remediation_output_head_value,
        "remediation_output_head": remediation_output_head,
        "body_hash": body_hash,
        "idempotency_key": idempotency_key,
        "comment_body_template_id": action_kind,
        "comment_body_artifact_path": Value::Null,
        "resolution_required": action_kind == "comment_fixed",
        "status": "pending",
        "reason": string_field(item, "reason", decision),
        "response_text": item.get("response_text").cloned().unwrap_or(Value::Null),
        "remediation_result_status": item.get("remediation_result_status").cloned().unwrap_or(Value::Null),
        "remediation_result_evidence": item.get("remediation_result_evidence").cloned().unwrap_or(Value::Null),
        "evidence": item.get("evidence").cloned().unwrap_or_else(|| item.clone()),
        "source_artifact_sequence": item.get("source_artifact_sequence").cloned().unwrap_or(Value::Null)
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P11
/// @requirement:REQ-PRFU-014
/// @pseudocode lines 24-28
fn write_pending_marker_actions_for_fixed_feedback(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    plan: &Value,
    validation_payload: &RemediationResultValidationArtifact,
    clock: &dyn ClockSleeper,
) -> Result<(), EngineError> {
    let fixed_items = fixed_feedback_marker_items(plan, validation_payload);
    if fixed_items.is_empty() {
        return Ok(());
    }
    write_pending_marker_actions(
        store,
        binding,
        step_id,
        step_order,
        &fixed_items,
        Some(validation_payload.output_head_sha.as_str()),
        clock,
    )
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P11
/// @requirement:REQ-PRFU-014
/// @pseudocode lines 24-28
fn fixed_feedback_marker_items(
    plan: &Value,
    validation_payload: &RemediationResultValidationArtifact,
) -> Vec<Value> {
    let mut plan_items = plan_items_by_key(plan);
    let mut items = Vec::new();
    for result in &validation_payload.results {
        let source_type = string_field(result, "source_type", "");
        let source_id = string_field(result, "source_id", "");
        if source_type != "coderabbit_feedback" {
            continue;
        }
        let status = string_field(result, "status", "");
        if !matches!(
            status.as_str(),
            "fixed" | "changed" | "already_satisfied" | "not_reproduced"
        ) {
            continue;
        }
        let key = format!("{source_type}:{source_id}");
        let Some(mut item) = plan_items.remove(&key) else {
            continue;
        };
        if let Some(thread_id) = result.get("thread_id").cloned() {
            item["thread_id"] = thread_id;
        }
        if let Some(comment_database_id) = result.get("comment_database_id").cloned() {
            item["comment_database_id"] = comment_database_id;
        }
        item["decision"] = json!("valid");
        item["marker_action"] = json!("comment_fixed");
        item["remediation_result_status"] = json!(status);
        item["remediation_input_head_sha"] = json!(validation_payload.input_head_sha.clone());
        item["remediation_output_head_sha"] = json!(validation_payload.output_head_sha.clone());
        if let Some(response_text) = result.get("response_text").cloned() {
            item["response_text"] = response_text;
        }
        item["remediation_result_evidence"] = result
            .get("evidence")
            .cloned()
            .unwrap_or_else(|| result.clone());
        items.push(item);
    }
    items
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 3,10-11
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 3
// Pre-existing artifact writer shape shared by remediation executors.
#[allow(clippy::too_many_arguments)]
fn write_fatal_plan(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    clock: &dyn ClockSleeper,
    failure_reason: &str,
    source_artifacts: Vec<Value>,
    failure_details: Value,
) -> Result<StepOutcome, EngineError> {
    let payload = fatal_plan_payload(clock, source_artifacts);
    write_plan_artifact(
        store,
        binding,
        step_id,
        step_order,
        &payload,
        clock,
        Some(("fatal", failure_reason, failure_details)),
    )?;
    Ok(StepOutcome::Fatal)
}

fn write_plan_artifact(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    payload: &RemediationPlanArtifact,
    clock: &dyn ClockSleeper,
    failure: Option<(&str, &str, Value)>,
) -> Result<(), EngineError> {
    store.write_json_artifact(
        binding,
        "pr-remediation-plan",
        step_id,
        step_order,
        payload,
        failure,
        clock,
    )?;
    Ok(())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 3
fn fatal_plan_payload(
    clock: &dyn ClockSleeper,
    source_artifacts: Vec<Value>,
) -> RemediationPlanArtifact {
    RemediationPlanArtifact {
        plan_state: PlanState::Fatal,
        must_fix: Vec::new(),
        mark_invalid: Vec::new(),
        needs_user_judgment: Vec::new(),
        pending_or_unknown: Vec::new(),
        source_artifacts,
        built_at: clock.now_rfc3339(),
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 1-3
fn artifact_root(context: &StepContext, params: &Value) -> Result<PathBuf, EngineError> {
    let raw = params
        .get("artifact_root")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| pr_remediation_error("missing artifact_root"))?;
    let interpolated = interpolate_string(raw, context);
    if interpolated.contains('{') || interpolated.contains('}') {
        return Err(pr_remediation_error(format!(
            "artifact_root contains unresolved template token: {interpolated}"
        )));
    }
    let path = PathBuf::from(interpolated);
    Ok(if path.is_absolute() {
        path
    } else {
        context.work_dir().join(path)
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 1-3
fn binding_from_params(context: &StepContext, params: &Value) -> PrFollowupBinding {
    PrFollowupBinding {
        schema_version: PR_FOLLOWUP_SCHEMA_VERSION,
        run_id: context.run_id().to_string(),
        repository_owner: string_param(context, params, "repository_owner", "example"),
        repository_name: string_param(context, params, "repository_name", "workflow"),
        pr_number: string_param(context, params, "pr_number", "1910")
            .parse()
            .unwrap_or(1910),
        head_ref: string_param(context, params, "head_ref", "feature"),
        head_sha: string_param(
            context,
            params,
            "head_sha",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
        base_ref: string_param(context, params, "base_ref", "main"),
        base_sha: Some(string_param(context, params, "base_sha", "base-a")),
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 1-11
// Pre-existing legacy harness compatibility flow.
#[allow(clippy::too_many_lines)]
fn ensure_legacy_harness_inputs(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    clock: &dyn ClockSleeper,
) -> Result<(), EngineError> {
    if store.canonical_path(binding, "pr").exists() {
        return Ok(());
    }
    store.write_json_artifact(
        binding,
        "pr",
        "capture_pr_identity",
        1,
        &json!({
            "pr_url": "https://github.com/example/workflow/pull/1910",
            "capture_state": "captured",
            "captured_at": clock.now_rfc3339(),
            "source": "legacy_harness",
            "source_pr_node_id": "PR_legacy_harness",
            "source_head_repository_owner": Value::Null,
            "source_head_repository_name": Value::Null
        }),
        None,
        clock,
    )?;
    store.write_json_artifact(
        binding,
        "ci-failures",
        "collect_ci_failures",
        4,
        &json!({
            "collection_state": "collected",
            "failures": [{
                "failure_id": "ci-build",
                "check_id": "check-build",
                "check_name": "build",
                "state": "completed",
                "conclusion": "failure",
                "url": "https://github.com/example/workflow/actions/runs/1",
                "run_id": "1",
                "job_id": "2",
                "log_status": "available",
                "log_excerpt": "build failed",
                "log_excerpt_path": Value::Null,
                "raw_log_path": Value::Null,
                "collection_error": Value::Null
            }],
            "pending_or_unknown": [],
            "watcher_fatal_source": Value::Null,
            "fatal_source": Value::Null,
            "log_artifacts": [],
            "source_check_status_artifact_sequence": 1
        }),
        None,
        clock,
    )?;
    store.write_json_artifact(
        binding,
        "coderabbit-feedback",
        "collect_coderabbit_feedback",
        5,
        &json!({
            "readiness_state": "ready",
            "stable_observation_count": 2,
            "required_stable_observations": 2,
            "max_observations": 6,
            "observation_interval_seconds": 300,
            "observations": [],
            "items": [{
                "item_id": "cr-valid",
                "stable_marker_key": "thread-valid",
                "body_hash": "hash-valid",
                "commit_sha": binding.head_sha
            }],
            "included_bot_identities": ["coderabbitai[bot]"],
            "feedback_item_set_hash": "fnv64:p10-legacy"
        }),
        None,
        clock,
    )?;
    store.write_json_artifact(
        binding,
        "feedback-evaluations",
        "evaluate_coderabbit_feedback",
        6,
        &json!({
            "evaluation_state": "complete",
            "items_seen": 1,
            "accepted_results": [{
                "item_id": "cr-valid",
                "stable_marker_key": "thread-valid",
                "body_hash": "hash-valid",
                "head_sha": binding.head_sha,
                "decision": "valid",
                "reason": "valid feedback",
                "recommended_action": "fix valid feedback",
                "accepted_at": clock.now_rfc3339(),
                "attempt_count": 1,
                "source": "legacy_harness",
                "reuse_state": "not_reused"
            }],
            "rejected_attempts": [],
            "unevaluated_items": [],
            "budget_exhausted_items": [],
            "max_attempts_per_item": 3,
            "reused_results_count": 0
        }),
        None,
        clock,
    )?;
    Ok(())
}

/// @pseudocode lines 1-3
fn binding_from_value(value: &Value) -> Result<PrFollowupBinding, EngineError> {
    Ok(PrFollowupBinding {
        schema_version: u32::try_from(require_u64(value, "schema_version")?)
            .map_err(|err| pr_remediation_error(format!("schema_version out of range: {err}")))?,
        run_id: require_string(value, "run_id")?,
        repository_owner: require_string(value, "repository_owner")?,
        repository_name: require_string(value, "repository_name")?,
        pr_number: require_u64(value, "pr_number")?,
        head_ref: require_string(value, "head_ref")?,
        head_sha: require_string(value, "head_sha")?,
        base_ref: require_string(value, "base_ref")?,
        base_sha: value
            .get("base_sha")
            .and_then(Value::as_str)
            .map(ToString::to_string),
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 1-3
fn source_artifact(value: &Value, family: &str) -> Value {
    json!({
        "artifact_family": family,
        "artifact_sequence": value.get("artifact_sequence").cloned().unwrap_or(Value::Null),
        "write_sequence": value.get("write_sequence").cloned().unwrap_or(Value::Null),
        "producer_step_id": value.get("producer_step_id").cloned().unwrap_or(Value::Null),
        "canonical_path": value.pointer("/history_metadata/canonical_path").cloned().unwrap_or(Value::Null)
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 5-8
fn artifact_sequence(value: &Value) -> Value {
    value
        .get("artifact_sequence")
        .cloned()
        .unwrap_or(Value::Null)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 5-8
fn stable_source_id(value: &Value, fallback: &str) -> String {
    value
        .get("item_id")
        .or_else(|| value.get("failure_id"))
        .or_else(|| value.get("check_id"))
        .and_then(Value::as_str)
        .unwrap_or(fallback)
        .to_string()
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 5-8
fn string_field(value: &Value, field: &str, default: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_string()
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 1-3
fn require_string(value: &Value, field: &str) -> Result<String, EngineError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| pr_remediation_error(format!("missing string field {field}")))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 1-3
fn require_u64(value: &Value, field: &str) -> Result<u64, EngineError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| pr_remediation_error(format!("missing integer field {field}")))
}

fn string_param(context: &StepContext, params: &Value, key: &str, default: &str) -> String {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(|value| interpolate_string(value, context))
        .filter(|value| !value.is_empty() && !has_unresolved_template(value))
        .or_else(|| {
            context
                .get(key)
                .filter(|value| !value.is_empty() && !has_unresolved_template(value))
                .cloned()
        })
        .unwrap_or_else(|| default.to_string())
}

fn has_unresolved_template(value: &str) -> bool {
    value.contains('{') || value.contains('}')
}

/// @requirement:REQ-PRFU-013
/// @pseudocode lines 1-3
fn u64_param(params: &Value, key: &str, default: u64) -> u64 {
    params.get(key).and_then(Value::as_u64).unwrap_or(default)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 1-3
fn current_step_id(context: &StepContext, default: &str) -> String {
    context
        .get("current_step_id")
        .cloned()
        .unwrap_or_else(|| default.to_string())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 1-3
fn pr_remediation_error(message: impl Into<String>) -> EngineError {
    EngineError::StepExecutionError {
        step_id: "pr_remediation".to_string(),
        message: message.into(),
    }
}

/// PR follow-up remediation wrapper executor for `pr_followup_remediation`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013,REQ-PRFU-017,REQ-PRFU-020
/// @pseudocode lines 12-17
#[derive(Debug, Default)]
pub struct PrFollowupRemediationExecutor;

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013,REQ-PRFU-017
/// @pseudocode lines 12-17
/// Owned PR follow-up llxprt invocation seam.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013,REQ-PRFU-017
/// @pseudocode lines 14-16
pub trait PrFollowupLlxprtCommandRunner: Send + Sync {
    fn invoke(&self, request: LlxprtInvocationRequest) -> LlxprtInvocationResult;
}

/// Owned argv-safe remediation invocation request.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013,REQ-PRFU-017
/// @pseudocode lines 13-14
#[derive(Clone, Debug)]
pub struct LlxprtInvocationRequest {
    pub argv: Vec<String>,
    pub working_directory: PathBuf,
    pub timeout_seconds: u64,
    pub stdout_log_path: PathBuf,
    pub stderr_log_path: PathBuf,
    pub remediation_plan_path: PathBuf,
    pub remediation_result_path: PathBuf,
    pub success_file_path: Option<PathBuf>,
}

/// Owned PR follow-up llxprt invocation result with process and artifact evidence.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013,REQ-PRFU-017
/// @pseudocode lines 15-17
#[derive(Clone, Debug, Default)]
pub struct LlxprtInvocationResult {
    pub argv: Vec<String>,
    pub working_directory: PathBuf,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub process_class: String,
    pub bounded_stdout: String,
    pub bounded_stderr: String,
    pub stdout_log_path: Option<PathBuf>,
    pub stderr_log_path: Option<PathBuf>,
    pub success_file_present: bool,
    pub success_file_size: Option<u64>,
    pub result_file_present: bool,
    pub result_file_size: Option<u64>,
    pub result_file_path: Option<PathBuf>,

    pub changed_paths: Vec<String>,
    pub spawn_error: Option<String>,
}

/// Production command runner for PR follow-up llxprt remediation.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013,REQ-PRFU-017
/// @pseudocode lines 14-16
#[derive(Debug, Default)]
pub struct SystemPrFollowupLlxprtCommandRunner;

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013,REQ-PRFU-017
/// @pseudocode lines 14-16
impl PrFollowupLlxprtCommandRunner for SystemPrFollowupLlxprtCommandRunner {
    fn invoke(&self, request: LlxprtInvocationRequest) -> LlxprtInvocationResult {
        invoke_llxprt_process(request)
    }
}

/// Testable executor wrapper that injects owned PR follow-up llxprt results.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013,REQ-PRFU-017
/// @pseudocode lines 14-16
pub struct PrFollowupRemediationExecutorWithRunner<R, C> {
    runner: R,
    clock: C,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013,REQ-PRFU-017
/// @pseudocode lines 14-16
impl<R, C> PrFollowupRemediationExecutorWithRunner<R, C> {
    #[must_use]
    pub fn new(runner: R, clock: C) -> Self {
        Self { runner, clock }
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013,REQ-PRFU-017
/// @pseudocode lines 12-17
impl<R, C> StepExecutor for PrFollowupRemediationExecutorWithRunner<R, C>
where
    R: PrFollowupLlxprtCommandRunner,
    C: ClockSleeper,
{
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        remediate_pr_followup(context, params, &self.clock, &self.runner)
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013,REQ-PRFU-017
/// @pseudocode lines 12-17
// Pre-existing remediation orchestration flow; split in a dedicated refactor stage.
#[allow(clippy::too_many_lines)]
fn remediate_pr_followup(
    context: &StepContext,
    params: &Value,
    clock: &dyn ClockSleeper,
    runner: &dyn PrFollowupLlxprtCommandRunner,
) -> Result<StepOutcome, EngineError> {
    let artifact_root = artifact_root(context, params)?;
    let store =
        PrFollowupArtifactStore::with_filesystem(&artifact_root, &SystemPrFollowupFilesystem)?;
    let fallback = binding_from_params(context, params);
    ensure_legacy_harness_inputs(&store, &fallback, clock)?;
    let pr = store.read_current_json(&fallback, "pr")?;
    let binding = binding_from_value(&pr)?;
    let plan = match store.read_current_json(&binding, "pr-remediation-plan") {
        Ok(plan) => plan,
        Err(_) => {
            build_remediation_plan(context, params, clock)?;
            store.read_current_json(&binding, "pr-remediation-plan")?
        }
    };
    let step_id = current_step_id(context, "remediate_pr_followup");
    let step_order = u64_param(params, "step_order_index", 8);
    let mut result_path = store.canonical_path(&binding, "pr-remediation-result");
    let run_path = store.canonical_path(&binding, "pr-remediation-llxprt-run");
    let stdout_log_path = run_path.with_file_name("pr-remediation-llxprt-stdout.log");
    let stderr_log_path = run_path.with_file_name("pr-remediation-llxprt-stderr.log");
    let success_file_path = params
        .get("success_file")
        .and_then(Value::as_str)
        .map(|path| resolve_path(context.work_dir(), path));
    let previous_result = store
        .read_current_raw_json(&binding, "pr-remediation-result")
        .ok();
    let prompt = render_remediation_prompt(&binding, &plan, &result_path, previous_result.as_ref());
    let argv = remediation_argv(params, &prompt, context);

    let request = LlxprtInvocationRequest {
        argv: argv.clone(),
        working_directory: context.work_dir().clone(),
        timeout_seconds: u64_param(params, "timeout_seconds", 900),
        stdout_log_path: stdout_log_path.clone(),
        stderr_log_path: stderr_log_path.clone(),
        remediation_plan_path: store.canonical_path(&binding, "pr-remediation-plan"),
        remediation_result_path: result_path.clone(),
        success_file_path,
    };
    let mut invocation = runner.invoke(request);
    if invocation.argv.is_empty() {
        invocation.argv = argv;
    }
    if invocation.working_directory.as_os_str().is_empty() {
        invocation.working_directory = context.work_dir().clone();
    }
    if let Some(path) = &invocation.result_file_path {
        result_path = path.clone();
    }

    let result_present = result_path
        .metadata()
        .is_ok_and(|metadata| metadata.len() > 0)
        || invocation.result_file_present;
    let process_class_before_result_reclassification = invocation.process_class.clone();
    if result_present && invocation.process_class == "timeout" {
        invocation.process_class = "success".to_string();
        invocation.spawn_error = None;
    }

    let validator_readable = result_path
        .metadata()
        .is_ok_and(|metadata| metadata.len() > 0);
    if !result_present {
        write_validator_readable_remediation_failure_result(
            &store,
            &binding,
            &step_id,
            step_order,
            &plan,
            &invocation,
            &result_path,
            clock,
        )?;
    } else if validator_readable && process_class_before_result_reclassification == "timeout" {
        repair_timeout_wrapper_failure_result_if_needed(
            &store,
            &binding,
            &step_id,
            step_order,
            &plan,
            &invocation,
            &result_path,
            clock,
        )?;
    }
    let validator_readable = result_path
        .metadata()
        .is_ok_and(|metadata| metadata.len() > 0);
    let state = invocation_state(&invocation, validator_readable);
    write_llxprt_run_artifact(
        &store,
        &binding,
        &step_id,
        step_order,
        &plan,
        &result_path,
        &invocation,
        &state,
        validator_readable,
        clock,
    )?;
    Ok(if validator_readable {
        StepOutcome::Success
    } else {
        StepOutcome::Fatal
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013,REQ-PRFU-017
/// @pseudocode lines 13-14
fn remediation_argv(params: &Value, prompt: &str, context: &StepContext) -> Vec<String> {
    let mut argv = vec![
        "llxprt".to_string(),
        "--set".to_string(),
        "reasoning.includeInResponse=false".to_string(),
    ];
    if let Some(profile) = params
        .get("profile")
        .and_then(Value::as_str)
        .map(|value| interpolate_string(value, context))
        .filter(|value| !value.is_empty() && !has_unresolved_template(value))
    {
        argv.push("--profile-load".to_string());
        argv.push(profile);
    }

    argv.extend(["--yolo".to_string(), "-p".to_string(), prompt.to_string()]);

    argv
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 13-14
fn render_remediation_prompt(
    binding: &PrFollowupBinding,
    plan: &Value,
    result_path: &Path,
    previous_result: Option<&Value>,
) -> String {
    let plan_path = plan
        .pointer("/history_metadata/canonical_path")
        .and_then(Value::as_str)
        .unwrap_or("pr-remediation-plan.json");
    let validation_feedback = previous_result
        .and_then(|result| result.get("validation_errors"))
        .and_then(Value::as_array)
        .filter(|errors| !errors.is_empty())
        .map(|errors| {
            let details = serde_json::to_string_pretty(errors).unwrap_or_else(|_| {
                errors
                    .iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            format!(
                "\n\nPrevious pr-remediation-result.json validation_errors must be corrected before finishing:\n{details}"
            )
        })
        .unwrap_or_default();
    format!(
        "PR follow-up remediation for {}/{}, PR #{} at head {}.\n\nRead {}. Fix only pr-remediation-plan.json.must_fix. Do not fix pr-remediation-plan.json.mark_invalid, out_of_scope feedback, or pr-remediation-plan.json.needs_user_judgment. Write {}. Use only canonical statuses fixed | changed | already_satisfied | not_reproduced | not_fixed | skipped | failed. Include structured evidence for every result item. Required result schema: every result item must include input_head_sha set to {} and output_head_sha set to the current PR head after remediation; every result item must also include response_text, a non-empty reviewer-facing message that Luther will post verbatim on the original review thread (do not post it yourself); fixed or changed results must include evidence.current_head_sha equal to the current PR head; already_satisfied or not_reproduced results must include evidence.current_head_sha equal to {}. already_satisfied results must also include evidence.commands with at least one command object whose status is passed and whose argv array is non-empty. Free-form-only completion is not acceptable; pr-remediation-result.json is required. Write only the requested canonical current pr-remediation-result.json path; do not create, copy, or modify any pr-followup/history files or artifact metadata fields.{}",
        binding.repository_owner,
        binding.repository_name,
        binding.pr_number,
        binding.head_sha,
        plan_path,
        result_path.display(),
        binding.head_sha,
        binding.head_sha,
        validation_feedback
    )
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 15-17
// Pre-existing artifact writer shape shared by remediation executors.
#[allow(clippy::too_many_arguments)]
fn write_validator_readable_remediation_failure_result(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    plan: &Value,
    invocation: &LlxprtInvocationResult,
    result_path: &Path,
    clock: &dyn ClockSleeper,
) -> Result<(), EngineError> {
    let must_fix = plan
        .get("must_fix")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let results = must_fix
        .iter()
        .map(|item| {
            json!({
                "source_type": item.get("source_type").cloned().unwrap_or(Value::Null),
                "source_id": item.get("source_id").cloned().unwrap_or(Value::Null),
                "stable_marker_key": item.get("stable_marker_key").cloned().unwrap_or(Value::Null),
                "body_hash": item.get("body_hash").cloned().unwrap_or(Value::Null),
                "input_head_sha": binding.head_sha,
                "status": "failed",
                "action": "llxprt_invocation_failed_before_result",
                "response_text": "Luther could not complete remediation for this item because the remediation agent invocation failed before producing a result. This thread is left open pending a retry.",
                "evidence": {
                    "kind": "llxprt_invocation",
                    "current_head_sha": binding.head_sha,
                    "process_class": invocation.process_class,
                    "exit_code": invocation.exit_code,
                    "signal": invocation.signal,
                    "stdout_excerpt": invocation.bounded_stdout,
                    "stderr_excerpt": invocation.bounded_stderr,
                    "changed_paths": invocation.changed_paths,
                    "argv": invocation.argv,
                    "working_directory": invocation.working_directory.display().to_string()
                },
                "evidence_paths": [
                    invocation.stdout_log_path.as_ref().map(|path| path.display().to_string()),
                    invocation.stderr_log_path.as_ref().map(|path| path.display().to_string())
                ]
            })
        })
        .collect::<Vec<_>>();
    let payload = json!({
        "input_head_sha": binding.head_sha,
        "output_head_sha": current_git_head(&invocation.working_directory).unwrap_or_else(|| binding.head_sha.clone()),
        "head_sha": binding.head_sha,
        "overall_status": "failed",
        "results": results,
        "verification_commands": [],
        "success_file_path": Value::Null,
        "validation_state": ValidationState::Unvalidated.as_str(),
        "validation_retry_index": 0,
        "max_validation_retries": 2,
        "remediation_attempt_index": 0,
        "max_remediation_attempts": 2,
        "retry_scope": { "run_id": binding.run_id, "input_head_sha": binding.head_sha, "plan_artifact_sequence": plan.get("artifact_sequence") },
        "plan_artifact_sequence": plan.get("artifact_sequence"),
        "unsuccessful_statuses": ["failed"],
        "no_change_after_remediation": true,
        "wrapper_failure_result_path": result_path.display().to_string(),
        "written_at": clock.now_rfc3339()
    });
    store.write_json_artifact(
        binding,
        "pr-remediation-result",
        step_id,
        step_order,
        &payload,
        None,
        clock,
    )?;
    Ok(())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013,REQ-PRFU-017
/// @pseudocode lines 15-17
// Pre-existing artifact writer shape shared by remediation executors.
#[allow(clippy::too_many_arguments)]
fn write_llxprt_run_artifact(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    plan: &Value,
    result_path: &Path,
    invocation: &LlxprtInvocationResult,
    state: &str,
    validator_readable: bool,
    clock: &dyn ClockSleeper,
) -> Result<(), EngineError> {
    let payload = json!({
        "remediation_invocation_state": state,
        "remediation_plan_path": plan.pointer("/history_metadata/canonical_path").and_then(Value::as_str).unwrap_or(""),
        "remediation_result_path": result_path.display().to_string(),
        "argv": invocation.argv,
        "working_directory": invocation.working_directory.display().to_string(),
        "exit_code": invocation.exit_code,
        "signal": invocation.signal,
        "process_class": invocation.process_class,
        "spawn_error": invocation.spawn_error,
        "stdout_artifact_path": invocation.stdout_log_path.as_ref().map(|path| path.display().to_string()),
        "stderr_artifact_path": invocation.stderr_log_path.as_ref().map(|path| path.display().to_string()),
        "bounded_stdout": invocation.bounded_stdout,
        "bounded_stderr": invocation.bounded_stderr,
        "success_file_present": invocation.success_file_present,
        "success_file_size": invocation.success_file_size,
        "result_file_present": invocation.result_file_present,
        "result_file_size": invocation.result_file_size,
        "result_file_path": invocation.result_file_path.as_ref().map(|path| path.display().to_string()),

        "validator_readable_result_written": validator_readable,
        "changed_paths": invocation.changed_paths,
        "changed_path_evidence": {
            "paths": invocation.changed_paths,
            "source": "git_status_porcelain"
        },
        "artifact_binding": {
            "run_id": binding.run_id,
            "repository_owner": binding.repository_owner,
            "repository_name": binding.repository_name,
            "pr_number": binding.pr_number,
            "head_ref": binding.head_ref,
            "head_sha": binding.head_sha,
            "base_ref": binding.base_ref,
            "base_sha": binding.base_sha,
            "plan_artifact_sequence": plan.get("artifact_sequence")
        },
        "recorded_at": clock.now_rfc3339()
    });
    let failure = if state == "success" {
        None
    } else {
        Some((
            state,
            "llxprt_remediation_invocation_non_success",
            json!({ "process_class": invocation.process_class, "validator_readable_result_written": validator_readable }),
        ))
    };
    store.write_json_artifact(
        binding,
        "pr-remediation-llxprt-run",
        step_id,
        step_order,
        &payload,
        failure,
        clock,
    )?;
    Ok(())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 15-16
fn read_json_file(path: &Path) -> Result<Value, EngineError> {
    let content = std::fs::read_to_string(path)
        .map_err(|err| pr_remediation_error(format!("read artifact {}: {err}", path.display())))?;
    serde_json::from_str(&content)
        .map_err(|err| pr_remediation_error(format!("parse artifact {}: {err}", path.display())))
}

// Pre-existing artifact repair helper shape.
#[allow(clippy::too_many_arguments)]
fn repair_timeout_wrapper_failure_result_if_needed(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    plan: &Value,
    invocation: &LlxprtInvocationResult,
    result_path: &Path,
    clock: &dyn ClockSleeper,
) -> Result<(), EngineError> {
    let Ok(result) = read_json_file(result_path) else {
        return Ok(());
    };
    let wrapper_failure_only =
        result
            .get("results")
            .and_then(Value::as_array)
            .is_some_and(|results| {
                !results.is_empty()
                    && results.iter().all(|item| {
                        item.get("action").and_then(Value::as_str)
                            == Some("llxprt_invocation_failed_before_result")
                    })
            });
    if !wrapper_failure_only {
        return Ok(());
    }

    write_validator_readable_remediation_failure_result(
        store,
        binding,
        step_id,
        step_order,
        plan,
        invocation,
        result_path,
        clock,
    )
}

fn invocation_state(invocation: &LlxprtInvocationResult, validator_readable: bool) -> String {
    match invocation.process_class.as_str() {
        "timeout" => "timeout".to_string(),
        "spawn_failed" => "spawn_failed".to_string(),
        "fatal" => "fatal".to_string(),
        "retryable_failed" => "retryable_failed".to_string(),
        _ if validator_readable => "success".to_string(),
        _ => "success_without_result".to_string(),
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-017
/// @pseudocode lines 14-16
fn invoke_llxprt_process(request: LlxprtInvocationRequest) -> LlxprtInvocationResult {
    let mut command = Command::new(&request.argv[0]);
    command.args(&request.argv[1..]);
    command.current_dir(&request.working_directory);
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.stdin(Stdio::null());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            return invocation_result_from_request(
                &request,
                None,
                None,
                "spawn_failed",
                String::new(),
                err.to_string(),
                Some(err.to_string()),
            );
        }
    };
    let stdout_buffer = Arc::new(Mutex::new(String::new()));
    let stderr_buffer = Arc::new(Mutex::new(String::new()));
    let stdout_reader = child.stdout.take().map(|mut stdout| {
        let buffer = Arc::clone(&stdout_buffer);
        thread::spawn(move || read_stream_into_string(&mut stdout, &buffer))
    });
    let stderr_reader = child.stderr.take().map(|mut stderr| {
        let buffer = Arc::clone(&stderr_buffer);
        thread::spawn(move || read_stream_into_string(&mut stderr, &buffer))
    });
    let start = Instant::now();
    let timeout = Duration::from_secs(request.timeout_seconds);
    let mut timed_out = false;
    let mut exit_code = None;
    while start.elapsed() < timeout {
        match child.try_wait() {
            Ok(Some(status)) => {
                exit_code = status.code();
                break;
            }
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(err) => {
                return invocation_result_from_request(
                    &request,
                    None,
                    None,
                    "fatal",
                    String::new(),
                    err.to_string(),
                    Some(err.to_string()),
                );
            }
        }
    }
    if exit_code.is_none() {
        timed_out = true;
        let _ = child.kill();
        let _ = child.wait();
    }
    if let Some(reader) = stdout_reader {
        let _ = reader.join();
    }
    if let Some(reader) = stderr_reader {
        let _ = reader.join();
    }
    let stdout = stdout_buffer
        .lock()
        .map_or_else(|_| String::new(), |text| text.clone());
    let stderr = stderr_buffer
        .lock()
        .map_or_else(|_| String::new(), |text| text.clone());
    write_optional_log(&request.stdout_log_path, &stdout);
    write_optional_log(&request.stderr_log_path, &stderr);
    let process_class = if timed_out {
        "timeout"
    } else if exit_code == Some(0) {
        "success"
    } else {
        "retryable_failed"
    };
    invocation_result_from_request(
        &request,
        exit_code,
        None,
        process_class,
        stdout,
        stderr,
        None,
    )
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-017
/// @pseudocode lines 15-16
fn invocation_result_from_request(
    request: &LlxprtInvocationRequest,
    exit_code: Option<i32>,
    signal: Option<i32>,
    process_class: &str,
    stdout: String,
    stderr: String,
    spawn_error: Option<String>,
) -> LlxprtInvocationResult {
    let success_file_size = request
        .success_file_path
        .as_ref()
        .and_then(|path| path.metadata().ok())
        .map(|metadata| metadata.len());
    let result_file_size = request
        .remediation_result_path
        .metadata()
        .ok()
        .map(|metadata| metadata.len());
    LlxprtInvocationResult {
        argv: request.argv.clone(),
        working_directory: request.working_directory.clone(),
        exit_code,
        signal,
        process_class: process_class.to_string(),
        bounded_stdout: bounded_excerpt(&stdout, 4096),
        bounded_stderr: bounded_excerpt(&stderr, 4096),
        stdout_log_path: Some(request.stdout_log_path.clone()),
        stderr_log_path: Some(request.stderr_log_path.clone()),
        success_file_present: success_file_size.is_some_and(|size| size > 0),
        success_file_size,
        result_file_path: Some(request.remediation_result_path.clone()),

        result_file_present: result_file_size.is_some_and(|size| size > 0),
        result_file_size,
        changed_paths: changed_paths_for_dir(&request.working_directory),
        spawn_error,
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-017
/// @pseudocode lines 15-16
fn read_stream_into_string<R: Read>(reader: &mut R, buffer: &Arc<Mutex<String>>) {
    let mut bytes = [0_u8; 4096];
    loop {
        match reader.read(&mut bytes) {
            Ok(0) => break,
            Ok(n) => {
                if let Ok(mut output) = buffer.lock() {
                    output.push_str(&String::from_utf8_lossy(&bytes[..n]));
                }
            }
            Err(_) => break,
        }
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-017
/// @pseudocode lines 14-16
fn resolve_path(work_dir: &Path, path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        work_dir.join(path)
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 15-16
fn bounded_excerpt(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013,REQ-PRFU-017
/// @pseudocode lines 15-16
fn write_optional_log(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, text);
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 15
fn changed_paths_for_dir(work_dir: &Path) -> Vec<String> {
    let Ok(output) = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(work_dir)
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.get(3..).map(str::trim))
        .filter(|path| !path.is_empty())
        .map(|path| path.split(" -> ").last().unwrap_or(path).to_string())
        .collect()
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 18
fn current_git_head(work_dir: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(work_dir)
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

/// Remediation result validator executor contract for `pr_remediation_result`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-020
/// @pseudocode lines 18-28
#[derive(Debug, Default)]
pub struct PrRemediationResultExecutor;

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P11
/// @requirement:REQ-PRFU-012,REQ-PRFU-014,REQ-PRFU-020
/// @pseudocode lines 18-28
impl StepExecutor for PrRemediationResultExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        validate_remediation_result(context, params, &SystemClockSleeper)
    }
}

const REMEDIATION_RESULT_VALID_STATUSES: &[&str] = &[
    "fixed",
    "changed",
    "already_satisfied",
    "not_reproduced",
    "not_fixed",
    "skipped",
    "failed",
];
const REMEDIATION_RESULT_SUCCESS_STATUSES: &[&str] =
    &["fixed", "changed", "already_satisfied", "not_reproduced"];
const REMEDIATION_RESULT_UNSUCCESSFUL_STATUSES: &[&str] = &["not_fixed", "skipped", "failed"];

#[derive(Clone, Debug, serde::Serialize)]
struct RemediationResultValidationArtifact {
    input_head_sha: String,
    output_head_sha: String,
    overall_status: String,
    results: Vec<Value>,
    verification_commands: Value,
    success_file_path: Value,
    validation_state: ValidationState,
    validation_retry_index: u64,
    max_validation_retries: u64,
    remediation_attempt_index: u64,
    max_remediation_attempts: u64,
    retry_scope: Value,
    plan_artifact_sequence: Value,
    unsuccessful_statuses: Vec<String>,
    no_change_after_remediation: bool,
    validation_errors: Vec<String>,
    validated_at: String,
}

#[derive(Clone, Debug)]
struct RemediationResultValidation {
    outcome: StepOutcome,
    state: ValidationState,
    failure_reason: String,
    errors: Vec<String>,
    unsuccessful_statuses: Vec<String>,
    no_change_after_remediation: bool,
    validation_retry_index: u64,
    max_validation_retries: u64,
    remediation_attempt_index: u64,
    max_remediation_attempts: u64,
}

fn validate_remediation_result(
    context: &StepContext,
    params: &Value,
    clock: &dyn ClockSleeper,
) -> Result<StepOutcome, EngineError> {
    let artifact_root = artifact_root(context, params)?;
    let store =
        PrFollowupArtifactStore::with_filesystem(&artifact_root, &SystemPrFollowupFilesystem)?;
    let fallback = binding_from_params(context, params);
    let pr = store.read_current_json(&fallback, "pr")?;
    let binding = binding_from_value(&pr)?;
    let plan = store.read_current_json(&binding, "pr-remediation-plan")?;
    let result = read_remediation_result_for_validation(&store, &binding)?;
    let step_id = current_step_id(context, "validate_remediation_result");
    let step_order = u64_param(params, "step_order_index", 9);

    let validation = evaluate_remediation_result(&binding, &plan, &result);
    let payload = remediation_result_payload(&binding, &result, &validation, clock);
    let failure = if validation.outcome == StepOutcome::Fatal {
        Some((
            validation.state.as_str(),
            validation.failure_reason.as_str(),
            json!({
                "validation_errors": validation.errors,
                "unsuccessful_statuses": validation.unsuccessful_statuses,
                "no_change_after_remediation": validation.no_change_after_remediation,
                "remediation_attempt_index": validation.remediation_attempt_index,
                "max_remediation_attempts": validation.max_remediation_attempts,
                "validation_retry_index": validation.validation_retry_index,
                "max_validation_retries": validation.max_validation_retries
            }),
        ))
    } else {
        None
    };
    store.write_json_artifact(
        &binding,
        "pr-remediation-result",
        &step_id,
        step_order,
        &payload,
        failure,
        clock,
    )?;
    if validation.outcome == StepOutcome::Success {
        write_pending_marker_actions_for_fixed_feedback(
            &store, &binding, &step_id, step_order, &plan, &payload, clock,
        )?;
    }

    Ok(validation.outcome)
}

fn read_remediation_result_for_validation(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
) -> Result<Value, EngineError> {
    let result = store.read_current_raw_json(binding, "pr-remediation-result")?;
    if store
        .validate_artifact_value(binding, "pr-remediation-result", &result)
        .is_ok()
    {
        return Ok(result);
    }
    Ok(result)
}

// Pre-existing result validation flow; split in a dedicated refactor stage.
#[allow(clippy::too_many_lines)]
fn evaluate_remediation_result(
    binding: &PrFollowupBinding,
    plan: &Value,
    result: &Value,
) -> RemediationResultValidation {
    let mut errors = Vec::new();
    let plan_items = plan_items_by_key(plan);
    let input_head_sha = string_field(result, "input_head_sha", "");
    let output_head_sha = string_field(result, "output_head_sha", "");
    let no_change_after_remediation = output_head_sha == input_head_sha;
    let validation_retry_index = result
        .get("validation_retry_index")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let max_validation_retries = result
        .get("max_validation_retries")
        .and_then(Value::as_u64)
        .unwrap_or(2);
    let mut remediation_attempt_index = result
        .get("remediation_attempt_index")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let max_remediation_attempts = result
        .get("max_remediation_attempts")
        .and_then(Value::as_u64)
        .unwrap_or(2);

    if input_head_sha != binding.head_sha {
        errors.push(format!(
            "input_head_sha mismatch: expected {}, got {}",
            binding.head_sha, input_head_sha
        ));
    }
    if result.get("plan_artifact_sequence") != plan.get("artifact_sequence") {
        errors.push("plan_artifact_sequence mismatch".to_string());
    }

    let results = result
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if results.is_empty() {
        errors.push("missing remediation results".to_string());
    }

    let mut unsuccessful_statuses = Vec::new();
    let mut successful_count = 0usize;
    let mut result_counts: BTreeMap<String, usize> = BTreeMap::new();
    for item in &results {
        let source_type = string_field(item, "source_type", "");
        let source_id = string_field(item, "source_id", "");
        let status = string_field(item, "status", "");
        let key = format!("{source_type}:{source_id}");
        let plan_item = plan_items.get(&key);
        *result_counts.entry(key.clone()).or_default() += 1;
        if plan_item.is_none() {
            errors.push(format!(
                "result item {key} does not match current plan item"
            ));
        }
        if let Some(plan_item) = plan_item {
            validate_result_binding(binding, plan_item, item, &key, &mut errors);
        }
        if string_field(item, "response_text", "").trim().is_empty() {
            errors.push(format!("result item {key} missing response_text"));
        }
        if !REMEDIATION_RESULT_VALID_STATUSES.contains(&status.as_str()) {
            errors.push(format!("unknown remediation status for {key}: {status}"));
            continue;
        }
        if REMEDIATION_RESULT_UNSUCCESSFUL_STATUSES.contains(&status.as_str()) {
            unsuccessful_statuses.push(status.clone());
            continue;
        }
        if REMEDIATION_RESULT_SUCCESS_STATUSES.contains(&status.as_str()) {
            successful_count += 1;
        }
        if matches!(status.as_str(), "already_satisfied" | "not_reproduced") {
            validate_deterministic_evidence(binding, plan_item, item, &status, &key, &mut errors);
        } else if matches!(status.as_str(), "fixed" | "changed") {
            validate_fixed_evidence(&output_head_sha, item, &key, &mut errors);
        }
    }
    validate_complete_result_coverage(&plan_items, &result_counts, &mut errors);

    if !errors.is_empty() {
        let exhausted = validation_retry_index >= max_validation_retries;
        return RemediationResultValidation {
            outcome: if exhausted {
                StepOutcome::Fatal
            } else {
                StepOutcome::Fixable
            },
            state: if exhausted {
                ValidationState::MalformedCapExhausted
            } else {
                ValidationState::FixableMalformed
            },
            failure_reason: "remediation_result_validation_failed".to_string(),
            errors,
            unsuccessful_statuses,
            no_change_after_remediation,
            validation_retry_index: validation_retry_index + 1,
            max_validation_retries,
            remediation_attempt_index,
            max_remediation_attempts,
        };
    }

    if !unsuccessful_statuses.is_empty() || successful_count != results.len() {
        remediation_attempt_index += 1;
        let exhausted = remediation_attempt_index >= max_remediation_attempts;
        return RemediationResultValidation {
            outcome: if exhausted {
                StepOutcome::Fatal
            } else {
                StepOutcome::Fixable
            },
            state: if exhausted {
                ValidationState::UnsuccessfulRemediationCapExhausted
            } else {
                ValidationState::ValidButUnsuccessful
            },
            failure_reason: "remediation_unsuccessful".to_string(),
            errors,
            unsuccessful_statuses,
            no_change_after_remediation,
            validation_retry_index,
            max_validation_retries,
            remediation_attempt_index,
            max_remediation_attempts,
        };
    }

    RemediationResultValidation {
        outcome: StepOutcome::Success,
        state: ValidationState::Valid,
        failure_reason: String::new(),
        errors,
        unsuccessful_statuses,
        no_change_after_remediation,
        validation_retry_index,
        max_validation_retries,
        remediation_attempt_index,
        max_remediation_attempts,
    }
}

fn plan_items_by_key(plan: &Value) -> std::collections::BTreeMap<String, Value> {
    let mut items = std::collections::BTreeMap::new();
    for item in plan
        .get("must_fix")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let source_type = string_field(item, "source_type", "");
        let source_id = string_field(item, "source_id", "");
        if !source_type.is_empty() && !source_id.is_empty() {
            items.insert(format!("{source_type}:{source_id}"), item.clone());
        }
    }
    items
}

fn validate_complete_result_coverage(
    plan_items: &BTreeMap<String, Value>,
    result_counts: &BTreeMap<String, usize>,
    errors: &mut Vec<String>,
) {
    for key in plan_items.keys() {
        match result_counts.get(key).copied().unwrap_or_default() {
            0 => errors.push(format!(
                "missing remediation result for current plan item {key}"
            )),
            1 => {}
            count => errors.push(format!(
                "duplicate remediation results for current plan item {key}: {count}"
            )),
        }
    }
}

fn validate_result_binding(
    binding: &PrFollowupBinding,
    plan_item: &Value,
    item: &Value,
    key: &str,
    errors: &mut Vec<String>,
) {
    let item_input_head = item
        .get("input_head_sha")
        .or_else(|| item.get("head_sha"))
        .and_then(Value::as_str);
    if item_input_head != Some(binding.head_sha.as_str()) {
        errors.push(format!(
            "result item {key} is not tied to current input head"
        ));
    }
    if plan_item.get("input_head_sha").and_then(Value::as_str) != Some(binding.head_sha.as_str()) {
        errors.push(format!("plan item {key} is not tied to current input head"));
    }
    if item.get("source_type").and_then(Value::as_str)
        != plan_item.get("source_type").and_then(Value::as_str)
        || item.get("source_id").and_then(Value::as_str)
            != plan_item.get("source_id").and_then(Value::as_str)
    {
        errors.push(format!(
            "result item {key} identity does not match current plan item"
        ));
    }
    let plan_marker = plan_item.get("stable_marker_key").and_then(Value::as_str);
    let item_marker = item.get("stable_marker_key").and_then(Value::as_str);
    if plan_marker.is_some() && item_marker != plan_marker {
        errors.push(format!(
            "result item {key} stable_marker_key does not match current plan item"
        ));
    }
    let plan_body_hash = plan_item.get("body_hash").and_then(Value::as_str);
    let item_body_hash = item.get("body_hash").and_then(Value::as_str);
    if plan_body_hash.is_some() && item_body_hash != plan_body_hash {
        errors.push(format!(
            "result item {key} body_hash does not match current plan item"
        ));
    }
}

fn validate_fixed_evidence(
    output_head_sha: &str,
    item: &Value,
    key: &str,
    errors: &mut Vec<String>,
) {
    // A genuine fixed/changed remediation commits a new change, so the PR head
    // advances past the pre-remediation input head. The authoritative
    // post-remediation head is the result's output_head_sha (derived from the
    // working tree by the engine), and the agent contract requires
    // evidence.current_head_sha to equal that post-remediation head. Validating
    // against the input head here would make every real fix unverifiable.
    let evidence = item.get("evidence").unwrap_or(&Value::Null);
    if evidence.get("current_head_sha").and_then(Value::as_str) != Some(output_head_sha) {
        errors.push(format!(
            "fixed evidence for {key} is not tied to current head"
        ));
    }
}

fn validate_deterministic_evidence(
    binding: &PrFollowupBinding,
    plan_item: Option<&Value>,
    item: &Value,
    status: &str,
    key: &str,
    errors: &mut Vec<String>,
) {
    let Some(plan_item) = plan_item else {
        return;
    };
    if plan_item.get("input_head_sha").and_then(Value::as_str) != Some(binding.head_sha.as_str()) {
        errors.push(format!("plan item {key} is not tied to current input head"));
    }
    let evidence = item.get("evidence").unwrap_or(&Value::Null);
    if evidence.get("current_head_sha").and_then(Value::as_str) != Some(binding.head_sha.as_str()) {
        errors.push(format!(
            "{status} evidence for {key} is not tied to current input head"
        ));
    }
    match status {
        "already_satisfied" => {
            let commands = evidence
                .get("commands")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let has_passed_command = commands.iter().any(|command| {
                command.get("status").and_then(Value::as_str) == Some("passed")
                    && command
                        .get("argv")
                        .and_then(Value::as_array)
                        .is_some_and(|argv| !argv.is_empty())
            });
            if !has_passed_command {
                errors.push(format!(
                    "already_satisfied result {key} lacks deterministic passed command evidence"
                ));
            }
        }
        "not_reproduced" => {
            let lookups = evidence
                .get("api_lookups")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let has_lookup = lookups.iter().any(|lookup| {
                lookup.get("endpoint").and_then(Value::as_str).is_some()
                    && lookup.get("normalized_status").and_then(Value::as_str) == Some("not_found")
            });
            if evidence.get("kind").and_then(Value::as_str) != Some("api_lookup") || !has_lookup {
                errors.push(format!(
                    "not_reproduced result {key} lacks deterministic api lookup evidence"
                ));
            }
        }
        _ => {}
    }
}

fn remediation_result_payload(
    binding: &PrFollowupBinding,
    result: &Value,
    validation: &RemediationResultValidation,
    clock: &dyn ClockSleeper,
) -> RemediationResultValidationArtifact {
    RemediationResultValidationArtifact {
        input_head_sha: string_field(result, "input_head_sha", &binding.head_sha),
        output_head_sha: string_field(result, "output_head_sha", &binding.head_sha),
        overall_status: string_field(result, "overall_status", "unknown"),
        results: result
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        verification_commands: result
            .get("verification_commands")
            .cloned()
            .unwrap_or_else(|| json!([])),
        success_file_path: result
            .get("success_file_path")
            .cloned()
            .unwrap_or(Value::Null),
        validation_state: validation.state,
        validation_retry_index: validation.validation_retry_index,
        max_validation_retries: validation.max_validation_retries,
        remediation_attempt_index: validation.remediation_attempt_index,
        max_remediation_attempts: validation.max_remediation_attempts,
        retry_scope: result
            .get("retry_scope")
            .cloned()
            .unwrap_or_else(|| json!({})),
        plan_artifact_sequence: result
            .get("plan_artifact_sequence")
            .cloned()
            .unwrap_or(Value::Null),
        unsuccessful_statuses: validation.unsuccessful_statuses.clone(),
        no_change_after_remediation: validation.no_change_after_remediation,
        validation_errors: validation.errors.clone(),
        validated_at: clock.now_rfc3339(),
    }
}

/// Dedicated post-PR local verification executor for `run_post_pr_tests`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017,REQ-PRFU-020
/// @pseudocode lines 29-33
#[derive(Debug, Default)]
pub struct RunPostPrTestsExecutor;

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-33
impl StepExecutor for RunPostPrTestsExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        run_post_pr_tests(
            context,
            params,
            &SystemClockSleeper,
            &SystemPostPrTestCommandRunner,
        )
    }
}

/// Safe argv-only command runner used by post-PR local verification.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-30
pub trait PostPrTestCommandRunner: Send + Sync {
    fn run(&self, request: PostPrTestCommandRequest) -> PostPrTestCommandResult;
}

/// Owned post-PR test command request.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-30
#[derive(Clone, Debug)]
pub struct PostPrTestCommandRequest {
    pub command_id: String,
    pub argv: Vec<String>,
    pub working_directory: PathBuf,
    pub timeout_seconds: u64,
    pub stdout_log_path: PathBuf,
    pub stderr_log_path: PathBuf,
}

/// Owned post-PR test command result.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 30-33
#[derive(Clone, Debug, Default)]
pub struct PostPrTestCommandResult {
    pub command_id: String,
    pub argv: Vec<String>,
    pub working_directory: PathBuf,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub status: String,
    pub bounded_stdout: String,
    pub bounded_stderr: String,
    pub stdout_log_path: Option<PathBuf>,
    pub stderr_log_path: Option<PathBuf>,
    pub spawn_error: Option<String>,
}

/// Production post-PR test command runner.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-30
#[derive(Debug, Default)]
pub struct SystemPostPrTestCommandRunner;

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-30
impl PostPrTestCommandRunner for SystemPostPrTestCommandRunner {
    fn run(&self, request: PostPrTestCommandRequest) -> PostPrTestCommandResult {
        run_post_pr_test_process(request)
    }
}

/// Testable post-PR verification executor with injected runner and clock.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-33
pub struct RunPostPrTestsExecutorWithRunner<R, C> {
    runner: R,
    clock: C,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-33
impl<R, C> RunPostPrTestsExecutorWithRunner<R, C> {
    #[must_use]
    pub fn new(runner: R, clock: C) -> Self {
        Self { runner, clock }
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-33
impl<R, C> StepExecutor for RunPostPrTestsExecutorWithRunner<R, C>
where
    R: PostPrTestCommandRunner,
    C: ClockSleeper,
{
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        run_post_pr_tests(context, params, &self.clock, &self.runner)
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-33
// Pre-existing post-PR test orchestration flow; split in a dedicated refactor stage.
#[allow(clippy::too_many_lines)]
fn run_post_pr_tests(
    context: &StepContext,
    params: &Value,
    clock: &dyn ClockSleeper,
    runner: &dyn PostPrTestCommandRunner,
) -> Result<StepOutcome, EngineError> {
    let artifact_root = artifact_root(context, params)?;
    let store =
        PrFollowupArtifactStore::with_filesystem(&artifact_root, &SystemPrFollowupFilesystem)?;
    let fallback = binding_from_params(context, params);
    ensure_legacy_harness_inputs(&store, &fallback, clock)?;
    let pr = store.read_current_json(&fallback, "pr")?;
    let binding = binding_from_value(&pr)?;
    let plan = store.read_current_json(&binding, "pr-remediation-plan")?;
    let result = store.read_current_json(&binding, "pr-remediation-result")?;
    let step_id = current_step_id(context, "run_post_pr_tests");
    let step_order = u64_param(params, "step_order_index", 10);
    let max_retries = match u64_required_param(params, "max_verification_retries", 2) {
        Ok(value) => value,
        Err(err) => {
            return write_post_pr_test_fatal(
                &store,
                &binding,
                &step_id,
                step_order,
                &plan,
                &result,
                0,
                0,
                "malformed_retry_cap",
                vec![err.to_string()],
                clock,
            );
        }
    };
    let commands = match post_pr_test_commands(context, params) {
        Ok(commands) => commands,
        Err(err) => {
            return write_post_pr_test_fatal(
                &store,
                &binding,
                &step_id,
                step_order,
                &plan,
                &result,
                0,
                max_retries,
                "invalid_command_configuration",
                vec![err.to_string()],
                clock,
            );
        }
    };

    let mut command_results = Vec::new();
    let mut infrastructure_errors = Vec::new();
    let mut any_failed = false;
    let log_dir = store
        .canonical_path(&binding, "post-pr-test-result")
        .with_file_name("post-pr-test-logs");
    for command in commands {
        let request = PostPrTestCommandRequest {
            command_id: command.command_id.clone(),
            argv: command.argv.clone(),
            working_directory: command.working_directory.clone(),
            timeout_seconds: command.timeout_seconds,
            stdout_log_path: log_dir.join(format!(
                "{}-stdout.log",
                sanitize_command_id(&command.command_id)
            )),
            stderr_log_path: log_dir.join(format!(
                "{}-stderr.log",
                sanitize_command_id(&command.command_id)
            )),
        };
        let result = runner.run(request);
        let status = result.status.as_str();
        if status == "fatal" {
            infrastructure_errors.push(format!("command {} reported fatal", result.command_id));
        } else if status != "passed" {
            any_failed = true;
        }
        command_results.push(command_result_json(&result));
    }

    if !infrastructure_errors.is_empty() {
        return write_post_pr_test_fatal(
            &store,
            &binding,
            &step_id,
            step_order,
            &plan,
            &result,
            0,
            max_retries,
            "infrastructure_failure",
            infrastructure_errors,
            clock,
        );
    }

    let retry_index = current_verification_retry_index(&store, &binding, &plan)?;
    let exhausted = any_failed && retry_index >= max_retries;
    let test_state = if any_failed { "failed" } else { "passed" };
    let payload = post_pr_test_payload(
        &binding,
        test_state,
        command_results,
        retry_index,
        max_retries,
        exhausted,
        &plan,
        &result,
        Vec::new(),
        clock,
    );
    let failure = if exhausted {
        Some((
            "failed",
            "verification_retry_cap_exhausted",
            json!({ "verification_retry_index": retry_index, "max_verification_retries": max_retries }),
        ))
    } else {
        None
    };
    store.write_json_artifact(
        &binding,
        "post-pr-test-result",
        &step_id,
        step_order,
        &payload,
        failure,
        clock,
    )?;

    Ok(if !any_failed {
        StepOutcome::Success
    } else if exhausted {
        StepOutcome::Fatal
    } else {
        StepOutcome::Fixable
    })
}

#[derive(Clone, Debug)]
struct PostPrTestCommandConfig {
    command_id: String,
    argv: Vec<String>,
    working_directory: PathBuf,
    timeout_seconds: u64,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-30
fn post_pr_test_commands(
    context: &StepContext,
    params: &Value,
) -> Result<Vec<PostPrTestCommandConfig>, EngineError> {
    let commands_value = params
        .get("commands")
        .or_else(|| params.get("post_pr_test_commands"))
        .ok_or_else(|| pr_remediation_error("missing post-PR test commands"))?;
    let commands = commands_value
        .as_array()
        .ok_or_else(|| pr_remediation_error("post-PR test commands must be an array"))?;
    if commands.is_empty() {
        return Err(pr_remediation_error(
            "post-PR test commands must not be empty",
        ));
    }
    let mut configured = Vec::new();
    for (index, value) in commands.iter().enumerate() {
        configured.push(post_pr_test_command(context, params, value, index)?);
    }
    Ok(configured)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-30
fn post_pr_test_command(
    context: &StepContext,
    params: &Value,
    value: &Value,
    index: usize,
) -> Result<PostPrTestCommandConfig, EngineError> {
    let object = value
        .as_object()
        .ok_or_else(|| pr_remediation_error("post-PR command entries must be objects"))?;
    if object.contains_key("command") || object.contains_key("shell") {
        return Err(pr_remediation_error(
            "shell-string post-PR commands are forbidden",
        ));
    }
    let command_id = object
        .get("id")
        .or_else(|| object.get("command_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("post-pr-test-{index}"));
    let argv = if let Some(argv) = object.get("argv") {
        string_array(argv, "argv")?
    } else if let Some(id) = object.get("command_id").and_then(Value::as_str) {
        configured_command_argv(params, id)?
    } else {
        return Err(pr_remediation_error(
            "post-PR command requires argv or command_id",
        ));
    };
    if argv.is_empty() || argv.iter().any(|arg| arg.is_empty()) {
        return Err(pr_remediation_error(
            "post-PR command argv must not be empty",
        ));
    }
    let working_directory = object
        .get("working_directory")
        .or_else(|| object.get("work_dir"))
        .and_then(Value::as_str)
        .map(|path| resolve_path(context.work_dir(), path))
        .unwrap_or_else(|| context.work_dir().clone());
    validate_safe_working_directory(context.work_dir(), &working_directory)?;
    let timeout_seconds = object
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| u64_param(params, "test_timeout_seconds", 900));
    if timeout_seconds == 0 {
        return Err(pr_remediation_error(
            "post-PR command timeout_seconds must be positive",
        ));
    }
    Ok(PostPrTestCommandConfig {
        command_id,
        argv,
        working_directory,
        timeout_seconds,
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-30
fn configured_command_argv(params: &Value, id: &str) -> Result<Vec<String>, EngineError> {
    let registry = params
        .get("command_registry")
        .or_else(|| params.get("commands_by_id"))
        .and_then(Value::as_object)
        .ok_or_else(|| pr_remediation_error("command_id used without command registry"))?;
    let Some(value) = registry.get(id) else {
        return Err(pr_remediation_error(format!(
            "unrecognized command_id {id}"
        )));
    };
    string_array(value, "command_registry entry")
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-30
fn string_array(value: &Value, field: &str) -> Result<Vec<String>, EngineError> {
    value
        .as_array()
        .ok_or_else(|| pr_remediation_error(format!("{field} must be an argv array")))?
        .iter()
        .map(|arg| {
            arg.as_str()
                .filter(|text| !text.is_empty())
                .map(ToString::to_string)
                .ok_or_else(|| {
                    pr_remediation_error(format!("{field} contains a non-string or empty arg"))
                })
        })
        .collect()
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-30
fn validate_safe_working_directory(work_dir: &Path, candidate: &Path) -> Result<(), EngineError> {
    let base = work_dir
        .canonicalize()
        .map_err(|err| pr_remediation_error(format!("canonicalize work_dir: {err}")))?;
    let candidate = candidate.canonicalize().map_err(|err| {
        pr_remediation_error(format!(
            "canonicalize post-PR test working_directory: {err}"
        ))
    })?;
    if candidate.starts_with(&base) {
        Ok(())
    } else {
        Err(pr_remediation_error(
            "post-PR test working_directory must stay under workflow work_dir",
        ))
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014
/// @pseudocode lines 32-33
fn current_verification_retry_index(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    plan: &Value,
) -> Result<u64, EngineError> {
    match store.read_current_json(binding, "post-pr-test-result") {
        Ok(value) if same_plan_sequence(&value, plan) => Ok(value
            .get("verification_retry_index")
            .and_then(Value::as_u64)
            .unwrap_or_default()
            + 1),
        Ok(_) | Err(_) => Ok(0),
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014
/// @pseudocode lines 32-33
fn same_plan_sequence(value: &Value, plan: &Value) -> bool {
    value.get("plan_artifact_sequence") == plan.get("artifact_sequence")
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 30-33
// Pre-existing artifact writer shape shared by post-PR test executors.
#[allow(clippy::too_many_arguments)]
fn write_post_pr_test_fatal(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    plan: &Value,
    result: &Value,
    retry_index: u64,
    max_retries: u64,
    reason: &str,
    errors: Vec<String>,
    clock: &dyn ClockSleeper,
) -> Result<StepOutcome, EngineError> {
    let payload = post_pr_test_payload(
        binding,
        "fatal",
        Vec::new(),
        retry_index,
        max_retries,
        true,
        plan,
        result,
        errors.clone(),
        clock,
    );
    store.write_json_artifact(
        binding,
        "post-pr-test-result",
        step_id,
        step_order,
        &payload,
        Some(("fatal", reason, json!({ "errors": errors }))),
        clock,
    )?;
    Ok(StepOutcome::Fatal)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 30-33
// Pre-existing payload shape shared with downstream artifacts.
#[allow(clippy::too_many_arguments)]
fn post_pr_test_payload(
    binding: &PrFollowupBinding,
    test_state: &str,
    commands: Vec<Value>,
    retry_index: u64,
    max_retries: u64,
    exhausted: bool,
    plan: &Value,
    result: &Value,
    errors: Vec<String>,
    clock: &dyn ClockSleeper,
) -> Value {
    json!({
        "test_state": test_state,
        "commands": commands,
        "verification_retry_index": retry_index,
        "max_verification_retries": max_retries,
        "retry_scope": {
            "run_id": binding.run_id,
            "repository_owner": binding.repository_owner,
            "repository_name": binding.repository_name,
            "pr_number": binding.pr_number,
            "head_sha": binding.head_sha,
            "plan_artifact_sequence": plan.get("artifact_sequence")
        },
        "plan_artifact_sequence": plan.get("artifact_sequence"),
        "remediation_result_artifact_sequence": result.get("artifact_sequence"),
        "verification_retry_exhausted": exhausted,
        "configuration_errors": errors,
        "verified_at": clock.now_rfc3339()
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 30-31
fn command_result_json(result: &PostPrTestCommandResult) -> Value {
    json!({
        "command_id": result.command_id,
        "argv": result.argv,
        "working_directory": result.working_directory.display().to_string(),
        "status": result.status,
        "exit_code": result.exit_code,
        "signal": result.signal,
        "bounded_stdout": result.bounded_stdout,
        "bounded_stderr": result.bounded_stderr,
        "stdout_artifact_path": result.stdout_log_path.as_ref().map(|path| path.display().to_string()),
        "stderr_artifact_path": result.stderr_log_path.as_ref().map(|path| path.display().to_string()),
        "spawn_error": result.spawn_error
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-017
/// @pseudocode lines 29-30
// Pre-existing process orchestration flow; split in a dedicated refactor stage.
#[allow(clippy::too_many_lines)]
fn run_post_pr_test_process(request: PostPrTestCommandRequest) -> PostPrTestCommandResult {
    let mut command = Command::new(&request.argv[0]);
    command.args(&request.argv[1..]);
    command.current_dir(&request.working_directory);
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.stdin(Stdio::null());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            write_optional_log(&request.stdout_log_path, "");
            write_optional_log(&request.stderr_log_path, &err.to_string());
            return PostPrTestCommandResult {
                command_id: request.command_id,
                argv: request.argv,
                working_directory: request.working_directory,
                exit_code: None,
                signal: None,
                status: "fatal".to_string(),
                bounded_stdout: String::new(),
                bounded_stderr: bounded_excerpt(&err.to_string(), 4096),
                stdout_log_path: Some(request.stdout_log_path),
                stderr_log_path: Some(request.stderr_log_path),
                spawn_error: Some(err.to_string()),
            };
        }
    };
    let stdout_buffer = Arc::new(Mutex::new(String::new()));
    let stderr_buffer = Arc::new(Mutex::new(String::new()));
    let stdout_reader = child.stdout.take().map(|mut stdout| {
        let buffer = Arc::clone(&stdout_buffer);
        thread::spawn(move || read_stream_into_string(&mut stdout, &buffer))
    });
    let stderr_reader = child.stderr.take().map(|mut stderr| {
        let buffer = Arc::clone(&stderr_buffer);
        thread::spawn(move || read_stream_into_string(&mut stderr, &buffer))
    });
    let start = Instant::now();
    let timeout = Duration::from_secs(request.timeout_seconds);
    let mut timed_out = false;
    let mut exit_code = None;
    while start.elapsed() < timeout {
        match child.try_wait() {
            Ok(Some(status)) => {
                exit_code = status.code();
                break;
            }
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(err) => {
                write_optional_log(&request.stdout_log_path, "");
                write_optional_log(&request.stderr_log_path, &err.to_string());
                return PostPrTestCommandResult {
                    command_id: request.command_id,
                    argv: request.argv,
                    working_directory: request.working_directory,
                    exit_code: None,
                    signal: None,
                    status: "fatal".to_string(),
                    bounded_stdout: String::new(),
                    bounded_stderr: bounded_excerpt(&err.to_string(), 4096),
                    stdout_log_path: Some(request.stdout_log_path),
                    stderr_log_path: Some(request.stderr_log_path),
                    spawn_error: Some(err.to_string()),
                };
            }
        }
    }
    if exit_code.is_none() {
        timed_out = true;
        let _ = child.kill();
        let _ = child.wait();
    }
    if let Some(reader) = stdout_reader {
        let _ = reader.join();
    }
    if let Some(reader) = stderr_reader {
        let _ = reader.join();
    }
    let stdout = stdout_buffer
        .lock()
        .map_or_else(|_| String::new(), |text| text.clone());
    let stderr = stderr_buffer
        .lock()
        .map_or_else(|_| String::new(), |text| text.clone());
    write_optional_log(&request.stdout_log_path, &stdout);
    write_optional_log(&request.stderr_log_path, &stderr);
    PostPrTestCommandResult {
        command_id: request.command_id,
        argv: request.argv,
        working_directory: request.working_directory,
        exit_code,
        signal: None,
        status: if timed_out {
            "fatal"
        } else if exit_code == Some(0) {
            "passed"
        } else {
            "failed"
        }
        .to_string(),
        bounded_stdout: bounded_excerpt(&stdout, 4096),
        bounded_stderr: bounded_excerpt(&stderr, 4096),
        stdout_log_path: Some(request.stdout_log_path),
        stderr_log_path: Some(request.stderr_log_path),
        spawn_error: timed_out.then(|| "post-PR test command timed out".to_string()),
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-017
/// @pseudocode lines 29-30
fn sanitize_command_id(command_id: &str) -> String {
    command_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014
/// @pseudocode lines 32-33
fn u64_required_param(params: &Value, key: &str, default: u64) -> Result<u64, EngineError> {
    match params.get(key) {
        Some(value) => value
            .as_u64()
            .filter(|value| *value > 0)
            .ok_or_else(|| pr_remediation_error(format!("{key} must be a positive integer"))),
        None => Ok(default),
    }
}

/// Dedicated remediation push executor for `push_remediation_changes`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017,REQ-PRFU-020
/// @pseudocode lines 34-40
#[derive(Debug, Default)]
pub struct PushRemediationChangesExecutor;

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 34-40
impl StepExecutor for PushRemediationChangesExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        push_remediation_changes(
            context,
            params,
            &SystemClockSleeper,
            &SystemPushRemediationCommandRunner,
        )
    }
}

/// Safe argv-only command runner used by remediation push.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 35-39
pub trait PushRemediationCommandRunner: Send + Sync {
    fn run(&self, request: PushRemediationCommandRequest) -> PushRemediationCommandResult;
}

/// Owned remediation push command request.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 35-39
#[derive(Clone, Debug)]
pub struct PushRemediationCommandRequest {
    pub command_id: String,
    pub argv: Vec<String>,
    pub working_directory: PathBuf,
    pub timeout_seconds: u64,
    pub stdout_log_path: PathBuf,
    pub stderr_log_path: PathBuf,
}

/// Owned remediation push command result.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 35-40
#[derive(Clone, Debug, Default)]
pub struct PushRemediationCommandResult {
    pub command_id: String,
    pub argv: Vec<String>,
    pub working_directory: PathBuf,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub status: String,
    pub bounded_stdout: String,
    pub bounded_stderr: String,
    pub stdout_log_path: Option<PathBuf>,
    pub stderr_log_path: Option<PathBuf>,
    pub spawn_error: Option<String>,
}

/// Production remediation push command runner.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 35-39
#[derive(Debug, Default)]
pub struct SystemPushRemediationCommandRunner;

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 35-39
impl PushRemediationCommandRunner for SystemPushRemediationCommandRunner {
    fn run(&self, request: PushRemediationCommandRequest) -> PushRemediationCommandResult {
        run_push_remediation_process(request)
    }
}

/// Testable remediation push executor with injected runner and clock.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 34-40
pub struct PushRemediationChangesExecutorWithRunner<R, C> {
    runner: R,
    clock: C,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 34-40
impl<R, C> PushRemediationChangesExecutorWithRunner<R, C> {
    #[must_use]
    pub fn new(runner: R, clock: C) -> Self {
        Self { runner, clock }
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 34-40
impl<R, C> StepExecutor for PushRemediationChangesExecutorWithRunner<R, C>
where
    R: PushRemediationCommandRunner,
    C: ClockSleeper,
{
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        push_remediation_changes(context, params, &self.clock, &self.runner)
    }
}

#[derive(Clone, Debug)]
struct PushInspection {
    pre_push_local_head_sha: String,
    pre_push_remote_head_sha: String,
    included_paths: Vec<String>,
    excluded_paths: Vec<String>,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 34-40
// Pre-existing push orchestration flow; split in a dedicated refactor stage.
#[allow(clippy::too_many_lines)]
fn push_remediation_changes(
    context: &StepContext,
    params: &Value,
    clock: &dyn ClockSleeper,
    runner: &dyn PushRemediationCommandRunner,
) -> Result<StepOutcome, EngineError> {
    let artifact_root = artifact_root(context, params)?;
    let store =
        PrFollowupArtifactStore::with_filesystem(&artifact_root, &SystemPrFollowupFilesystem)?;
    let fallback = binding_from_params(context, params);
    ensure_legacy_harness_inputs(&store, &fallback, clock)?;
    let pr = store.read_current_json(&fallback, "pr")?;
    let binding = binding_from_value(&pr)?;
    let plan = store.read_current_json(&binding, "pr-remediation-plan")?;
    let result = store.read_current_json(&binding, "pr-remediation-result")?;
    let step_id = current_step_id(context, "push_remediation_changes");
    let step_order = u64_param(params, "step_order_index", 11);
    let max_push_retries = u64_required_param(params, "max_push_retries", 1)?;
    let timeout_seconds = u64_required_param(params, "push_timeout_seconds", 900)?;
    let working_directory = push_working_directory(context, params)?;
    let remote_ref = push_remote_ref(context, params, &binding);
    let remote_name = string_param(context, params, "remote_name", "origin");

    let log_dir = store
        .canonical_path(&binding, "push-remediation-result")
        .with_file_name("push-remediation-logs");
    let test_result = match store.read_current_json(&binding, "post-pr-test-result") {
        Ok(test_result) => test_result,
        Err(err) => {
            return write_push_config_fatal(
                &store,
                &binding,
                &step_id,
                step_order,
                max_push_retries,
                &remote_ref,
                "missing_or_unreadable_post_pr_test_result",
                json!({ "error": err.to_string() }),
                &plan,
                &result,
                Value::Null,
                clock,
            );
        }
    };
    if let Err(errors) =
        validate_push_local_verification_result(&binding, &plan, &result, &test_result)
    {
        return write_push_config_fatal(
            &store,
            &binding,
            &step_id,
            step_order,
            max_push_retries,
            &remote_ref,
            "post_pr_local_verification_not_passed",
            json!({ "errors": errors }),
            &plan,
            &result,
            test_result,
            clock,
        );
    }

    if !must_fix_success_evidence_is_acceptable(&plan, &result) {
        let payload = push_payload(
            &binding,
            "fatal",
            0,
            max_push_retries,
            &remote_ref,
            "unknown",
            "unknown",
            &binding.head_sha,
            None,
            "unknown",
            None,
            "unknown",
            false,
            Vec::new(),
            Vec::new(),
            None,
            Some("missing_validator_success_evidence"),
            Vec::new(),
            &plan,
            &result,
            &test_result,
            clock,
        );
        write_push_result(
            &store,
            &binding,
            &step_id,
            step_order,
            payload,
            Some(("fatal", "missing_validator_success_evidence", json!({}))),
            clock,
        )?;
        return Ok(StepOutcome::Fatal);
    }

    let mut commands = Vec::new();
    let inspection = match inspect_push_worktree(
        runner,
        &working_directory,
        &log_dir,
        timeout_seconds,
        &remote_name,
        &remote_ref,
    ) {
        Ok((inspection, observed)) => {
            commands.extend(observed);
            inspection
        }
        Err((reason, observed)) => {
            commands.extend(observed);
            return write_push_failure_from_observation(
                &store,
                &binding,
                &step_id,
                step_order,
                max_push_retries,
                &remote_ref,
                "fatal",
                reason.as_str(),
                commands,
                &plan,
                &result,
                &test_result,
                clock,
            );
        }
    };

    let retry_index = current_push_retry_index(
        &store,
        &binding,
        &inspection.pre_push_local_head_sha,
        &remote_ref,
    )?;
    if inspection.included_paths.is_empty() {
        if inspection.pre_push_remote_head_sha != inspection.pre_push_local_head_sha {
            let push = push_runner_command(
                runner,
                "push",
                vec![
                    "git".to_string(),
                    "push".to_string(),
                    remote_name.clone(),
                    format!("HEAD:{remote_ref}"),
                ],
                &working_directory,
                &log_dir,
                timeout_seconds,
            );
            let push_ok = push.status == "passed";
            commands.push(push_command_result_json(&push));
            if !push_ok {
                return write_retryable_push_failure(
                    &store,
                    &binding,
                    &step_id,
                    step_order,
                    retry_index,
                    max_push_retries,
                    &remote_ref,
                    "push_failed",
                    commands,
                    &inspection,
                    &plan,
                    &result,
                    &test_result,
                    clock,
                );
            }
            let remote_after = match remote_head_sha(
                runner,
                &working_directory,
                &log_dir,
                timeout_seconds,
                &remote_name,
                &remote_ref,
            ) {
                Ok((head, observed)) => {
                    commands.extend(observed);
                    head
                }
                Err((reason, observed)) => {
                    commands.extend(observed);
                    return write_push_failure_from_observation(
                        &store,
                        &binding,
                        &step_id,
                        step_order,
                        max_push_retries,
                        &remote_ref,
                        "retryable_failed",
                        reason.as_str(),
                        commands,
                        &plan,
                        &result,
                        &test_result,
                        clock,
                    );
                }
            };
            let verified = remote_after == inspection.pre_push_local_head_sha;
            let payload = push_payload(
                &binding,
                if verified {
                    "pushed_existing_head"
                } else {
                    "retryable_failed"
                },
                retry_index,
                max_push_retries,
                &remote_ref,
                &inspection.pre_push_local_head_sha,
                &inspection.pre_push_remote_head_sha,
                &binding.head_sha,
                Some(&inspection.pre_push_local_head_sha),
                &inspection.pre_push_local_head_sha,
                Some(&remote_after),
                &inspection.pre_push_local_head_sha,
                verified,
                Vec::new(),
                inspection.excluded_paths,
                None,
                (!verified).then_some("remote_head_mismatch_after_push"),
                commands,
                &plan,
                &result,
                &test_result,
                clock,
            );
            let failure = (!verified).then(|| {
                (
                    "retryable_failed",
                    "remote_head_mismatch_after_push",
                    json!({ "committed_head": inspection.pre_push_local_head_sha, "remote_head": remote_after }),
                )
            });
            write_push_result(
                &store, &binding, &step_id, step_order, payload, failure, clock,
            )?;
            return Ok(if verified {
                StepOutcome::Success
            } else if retry_index >= max_push_retries {
                StepOutcome::Fatal
            } else {
                StepOutcome::Retryable
            });
        }

        let state = if inspection.excluded_paths.is_empty() {
            "no_change"
        } else {
            "no_change_excluded_only"
        };
        let verified = inspection.pre_push_remote_head_sha == inspection.pre_push_local_head_sha;
        let payload = push_payload(
            &binding,
            state,
            retry_index,
            max_push_retries,
            &remote_ref,
            &inspection.pre_push_local_head_sha,
            &inspection.pre_push_remote_head_sha,
            &binding.head_sha,
            None,
            &inspection.pre_push_local_head_sha,
            Some(&inspection.pre_push_remote_head_sha),
            &inspection.pre_push_local_head_sha,
            verified,
            Vec::new(),
            inspection.excluded_paths,
            None,
            (!verified).then_some("remote_head_mismatch"),
            commands,
            &plan,
            &result,
            &test_result,
            clock,
        );
        let failure = (!verified).then(|| {
            (
                "fatal",
                "remote_head_mismatch",
                json!({ "local_head": inspection.pre_push_local_head_sha, "remote_head": inspection.pre_push_remote_head_sha }),
            )
        });
        write_push_result(
            &store, &binding, &step_id, step_order, payload, failure, clock,
        )?;
        return Ok(if verified {
            StepOutcome::Fixable
        } else {
            StepOutcome::Fatal
        });
    }

    let stage = push_runner_command(
        runner,
        "stage",
        vec!["git".to_string(), "add".to_string(), "--".to_string()]
            .into_iter()
            .chain(inspection.included_paths.iter().cloned())
            .collect(),
        &working_directory,
        &log_dir,
        timeout_seconds,
    );
    let stage_ok = stage.status == "passed";
    commands.push(push_command_result_json(&stage));
    if !stage_ok {
        return write_retryable_push_failure(
            &store,
            &binding,
            &step_id,
            step_order,
            retry_index,
            max_push_retries,
            &remote_ref,
            "stage_failed",
            commands,
            &inspection,
            &plan,
            &result,
            &test_result,
            clock,
        );
    }

    let commit_message = push_commit_message(params, &binding, &plan);
    let commit = push_runner_command(
        runner,
        "commit",
        vec![
            "git".to_string(),
            "commit".to_string(),
            "-m".to_string(),
            commit_message.clone(),
        ],
        &working_directory,
        &log_dir,
        timeout_seconds,
    );
    let commit_ok = commit.status == "passed";
    commands.push(push_command_result_json(&commit));
    if !commit_ok {
        return write_retryable_push_failure(
            &store,
            &binding,
            &step_id,
            step_order,
            retry_index,
            max_push_retries,
            &remote_ref,
            "commit_failed",
            commands,
            &inspection,
            &plan,
            &result,
            &test_result,
            clock,
        );
    }

    let committed_head = match local_head_sha(runner, &working_directory, &log_dir, timeout_seconds)
    {
        Ok((head, observed)) => {
            commands.extend(observed);
            head
        }
        Err((reason, observed)) => {
            commands.extend(observed);
            return write_push_failure_from_observation(
                &store,
                &binding,
                &step_id,
                step_order,
                max_push_retries,
                &remote_ref,
                "retryable_failed",
                reason.as_str(),
                commands,
                &plan,
                &result,
                &test_result,
                clock,
            );
        }
    };

    let push = push_runner_command(
        runner,
        "push",
        vec![
            "git".to_string(),
            "push".to_string(),
            remote_name.clone(),
            format!("HEAD:{remote_ref}"),
        ],
        &working_directory,
        &log_dir,
        timeout_seconds,
    );
    let push_ok = push.status == "passed";
    commands.push(push_command_result_json(&push));
    if !push_ok {
        return write_retryable_push_failure(
            &store,
            &binding,
            &step_id,
            step_order,
            retry_index,
            max_push_retries,
            &remote_ref,
            "push_failed",
            commands,
            &inspection,
            &plan,
            &result,
            &test_result,
            clock,
        );
    }

    let remote_after = match remote_head_sha(
        runner,
        &working_directory,
        &log_dir,
        timeout_seconds,
        &remote_name,
        &remote_ref,
    ) {
        Ok((head, observed)) => {
            commands.extend(observed);
            head
        }
        Err((reason, observed)) => {
            commands.extend(observed);
            return write_push_failure_from_observation(
                &store,
                &binding,
                &step_id,
                step_order,
                max_push_retries,
                &remote_ref,
                "retryable_failed",
                reason.as_str(),
                commands,
                &plan,
                &result,
                &test_result,
                clock,
            );
        }
    };
    let verified = remote_after == committed_head;
    let payload = push_payload(
        &binding,
        if verified {
            "pushed"
        } else {
            "retryable_failed"
        },
        retry_index,
        max_push_retries,
        &remote_ref,
        &inspection.pre_push_local_head_sha,
        &inspection.pre_push_remote_head_sha,
        &binding.head_sha,
        Some(&committed_head),
        &committed_head,
        Some(&remote_after),
        &committed_head,
        verified,
        inspection.included_paths,
        inspection.excluded_paths,
        Some(&commit_message),
        (!verified).then_some("remote_head_mismatch_after_push"),
        commands,
        &plan,
        &result,
        &test_result,
        clock,
    );
    let failure = (!verified).then(|| {
        (
            "retryable_failed",
            "remote_head_mismatch_after_push",
            json!({ "committed_head": committed_head, "remote_head": remote_after }),
        )
    });
    write_push_result(
        &store, &binding, &step_id, step_order, payload, failure, clock,
    )?;
    Ok(if verified {
        StepOutcome::Success
    } else if retry_index >= max_push_retries {
        StepOutcome::Fatal
    } else {
        StepOutcome::Retryable
    })
}

fn push_working_directory(context: &StepContext, params: &Value) -> Result<PathBuf, EngineError> {
    let working_directory = params
        .get("working_directory")
        .or_else(|| params.get("work_dir"))
        .and_then(Value::as_str)
        .map(|path| resolve_path(context.work_dir(), path))
        .unwrap_or_else(|| context.work_dir().clone());
    validate_safe_working_directory(context.work_dir(), &working_directory)?;
    Ok(working_directory)
}
fn push_remote_ref(context: &StepContext, params: &Value, binding: &PrFollowupBinding) -> String {
    string_param(
        context,
        params,
        "remote_ref",
        &format!("refs/heads/{}", binding.head_ref),
    )
}

fn push_commit_message(params: &Value, binding: &PrFollowupBinding, plan: &Value) -> String {
    params
        .get("commit_message")
        .and_then(Value::as_str)
        .filter(|message| !message.trim().is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| {
            let count = plan
                .get("must_fix")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            format!(
                "Apply PR follow-up remediation for #{} ({count} item{})",
                binding.pr_number,
                if count == 1 { "" } else { "s" }
            )
        })
}

fn inspect_push_worktree(
    runner: &dyn PushRemediationCommandRunner,
    working_directory: &Path,
    log_dir: &Path,
    timeout_seconds: u64,
    remote_name: &str,
    remote_ref: &str,
) -> Result<(PushInspection, Vec<Value>), (String, Vec<Value>)> {
    let mut commands = Vec::new();
    let local = local_head_sha(runner, working_directory, log_dir, timeout_seconds)?;
    commands.extend(local.1);
    let remote = remote_head_sha(
        runner,
        working_directory,
        log_dir,
        timeout_seconds,
        remote_name,
        remote_ref,
    )?;
    commands.extend(remote.1);
    let status = push_runner_command(
        runner,
        "status-porcelain",
        vec![
            "git".to_string(),
            "status".to_string(),
            "--porcelain=v1".to_string(),
            "-z".to_string(),
            "--untracked-files=all".to_string(),
        ],
        working_directory,
        log_dir,
        timeout_seconds,
    );
    let status_ok = status.status == "passed";
    let status_stdout = status.bounded_stdout.clone();
    commands.push(push_command_result_json(&status));
    if !status_ok {
        return Err(("status_failed".to_string(), commands));
    }
    let mut changed_paths = parse_porcelain_z(&status_stdout);
    changed_paths.sort();
    changed_paths.dedup();
    let mut included_paths = Vec::new();
    let mut excluded_paths = Vec::new();
    for path in &changed_paths {
        if push_path_is_excluded(path) {
            excluded_paths.push(path.clone());
        } else {
            included_paths.push(path.clone());
        }
    }
    Ok((
        PushInspection {
            pre_push_local_head_sha: local.0,
            pre_push_remote_head_sha: remote.0,
            included_paths,
            excluded_paths,
        },
        commands,
    ))
}

fn local_head_sha(
    runner: &dyn PushRemediationCommandRunner,
    working_directory: &Path,
    log_dir: &Path,
    timeout_seconds: u64,
) -> Result<(String, Vec<Value>), (String, Vec<Value>)> {
    let command = push_runner_command(
        runner,
        "local-head",
        vec![
            "git".to_string(),
            "rev-parse".to_string(),
            "HEAD".to_string(),
        ],
        working_directory,
        log_dir,
        timeout_seconds,
    );
    let ok = command.status == "passed";
    let head = command.bounded_stdout.trim().to_string();
    let value = push_command_result_json(&command);
    if ok && !head.is_empty() {
        Ok((head, vec![value]))
    } else {
        Err(("local_head_unavailable".to_string(), vec![value]))
    }
}

fn remote_head_sha(
    runner: &dyn PushRemediationCommandRunner,
    working_directory: &Path,
    log_dir: &Path,
    timeout_seconds: u64,
    remote_name: &str,
    remote_ref: &str,
) -> Result<(String, Vec<Value>), (String, Vec<Value>)> {
    let command = push_runner_command(
        runner,
        "remote-head",
        vec![
            "git".to_string(),
            "ls-remote".to_string(),
            "--heads".to_string(),
            remote_name.to_string(),
            remote_ref.to_string(),
        ],
        working_directory,
        log_dir,
        timeout_seconds,
    );
    let ok = command.status == "passed";
    let head = command
        .bounded_stdout
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string();
    let value = push_command_result_json(&command);
    if ok && !head.is_empty() {
        Ok((head, vec![value]))
    } else {
        Err(("remote_head_unavailable".to_string(), vec![value]))
    }
}

fn push_runner_command(
    runner: &dyn PushRemediationCommandRunner,
    command_id: &str,
    argv: Vec<String>,
    working_directory: &Path,
    log_dir: &Path,
    timeout_seconds: u64,
) -> PushRemediationCommandResult {
    runner.run(PushRemediationCommandRequest {
        command_id: command_id.to_string(),
        argv,
        working_directory: working_directory.to_path_buf(),
        timeout_seconds,
        stdout_log_path: log_dir.join(format!("{}-stdout.log", sanitize_command_id(command_id))),
        stderr_log_path: log_dir.join(format!("{}-stderr.log", sanitize_command_id(command_id))),
    })
}

fn parse_porcelain_z(output: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut entries = output.split('\0').filter(|entry| !entry.is_empty());
    while let Some(entry) = entries.next() {
        if entry.len() < 4 {
            continue;
        }
        let code = &entry[..2];
        let path = entry[3..].to_string();
        paths.push(path);
        if code.starts_with('R')
            || code.ends_with('R')
            || code.starts_with('C')
            || code.ends_with('C')
        {
            if let Some(next_path) = entries.next() {
                paths.push(next_path.to_string());
            }
        }
    }
    paths
}

fn push_path_is_excluded(path: &str) -> bool {
    let normalized = path.trim_start_matches("./");
    normalized == ".llxprt"
        || normalized.starts_with(".llxprt/")
        || normalized == "LLXPRT.md"
        || normalized.ends_with("LLXPRT.md")
        || normalized.ends_with(".generated-notice")
        || normalized.ends_with(".generated-notice.md")
        || normalized.ends_with("GENERATED_NOTICE.md")
        || normalized.contains("/generated-notice/")
}

fn must_fix_success_evidence_is_acceptable(plan: &Value, result: &Value) -> bool {
    let must_fix = plan
        .get("must_fix")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let results = result
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if must_fix.is_empty() {
        return true;
    }
    must_fix.iter().all(|item| {
        let source_type = string_field(item, "source_type", "");
        let source_id = string_field(item, "source_id", "");
        results.iter().any(|entry| {
            string_field(entry, "source_type", "") == source_type
                && string_field(entry, "source_id", "") == source_id
                && matches!(
                    string_field(entry, "status", "").as_str(),
                    "fixed" | "changed" | "already_satisfied" | "not_reproduced"
                )
                && entry.get("evidence").is_some()
        })
    })
}

fn validate_push_local_verification_result(
    binding: &PrFollowupBinding,
    plan: &Value,
    result: &Value,
    test_result: &Value,
) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    if test_result.get("test_state").and_then(Value::as_str) != Some("passed") {
        errors.push("test_state must be passed".to_string());
    }
    if !binding_from_value(test_result)
        .map(|test_binding| &test_binding == binding)
        .unwrap_or(false)
    {
        errors.push("post-pr-test-result binding mismatch".to_string());
    }
    if test_result.get("plan_artifact_sequence") != plan.get("artifact_sequence") {
        errors.push("plan_artifact_sequence mismatch".to_string());
    }
    if test_result.get("remediation_result_artifact_sequence") != result.get("artifact_sequence") {
        errors.push("remediation_result_artifact_sequence mismatch".to_string());
    }
    let scope = test_result.get("retry_scope").and_then(Value::as_object);
    if scope
        .and_then(|scope| scope.get("run_id"))
        .and_then(Value::as_str)
        != Some(binding.run_id.as_str())
    {
        errors.push("retry_scope.run_id mismatch".to_string());
    }
    if scope
        .and_then(|scope| scope.get("repository_owner"))
        .and_then(Value::as_str)
        != Some(binding.repository_owner.as_str())
    {
        errors.push("retry_scope.repository_owner mismatch".to_string());
    }
    if scope
        .and_then(|scope| scope.get("repository_name"))
        .and_then(Value::as_str)
        != Some(binding.repository_name.as_str())
    {
        errors.push("retry_scope.repository_name mismatch".to_string());
    }
    if scope
        .and_then(|scope| scope.get("pr_number"))
        .and_then(Value::as_u64)
        != Some(binding.pr_number)
    {
        errors.push("retry_scope.pr_number mismatch".to_string());
    }
    if scope
        .and_then(|scope| scope.get("head_sha"))
        .and_then(Value::as_str)
        != Some(binding.head_sha.as_str())
    {
        errors.push("retry_scope.head_sha mismatch".to_string());
    }
    if scope.and_then(|scope| scope.get("plan_artifact_sequence")) != plan.get("artifact_sequence")
    {
        errors.push("retry_scope.plan_artifact_sequence mismatch".to_string());
    }
    let commands = test_result.get("commands").and_then(Value::as_array);
    if commands.is_none_or(Vec::is_empty) {
        errors.push("commands must contain local verification evidence".to_string());
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn current_push_retry_index(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    local_head: &str,
    remote_ref: &str,
) -> Result<u64, EngineError> {
    match store.read_current_json(binding, "push-remediation-result") {
        Ok(value) => {
            let same_scope = value
                .get("retry_scope")
                .and_then(Value::as_object)
                .map(|scope| {
                    scope.get("head_sha").and_then(Value::as_str) == Some(local_head)
                        && scope.get("remote_ref").and_then(Value::as_str) == Some(remote_ref)
                })
                .unwrap_or(false);
            Ok(if same_scope {
                value
                    .get("push_retry_index")
                    .and_then(Value::as_u64)
                    .unwrap_or_default()
                    + 1
            } else {
                0
            })
        }
        Err(_) => Ok(0),
    }
}

#[allow(clippy::too_many_arguments)]
fn write_push_config_fatal(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    max_push_retries: u64,
    remote_ref: &str,
    reason: &str,
    details: Value,
    plan: &Value,
    result: &Value,
    test_result: Value,
    clock: &dyn ClockSleeper,
) -> Result<StepOutcome, EngineError> {
    let payload = push_payload(
        binding,
        "fatal",
        0,
        max_push_retries,
        remote_ref,
        "unknown",
        "unknown",
        &binding.head_sha,
        None,
        "unknown",
        None,
        "unknown",
        false,
        Vec::new(),
        Vec::new(),
        None,
        Some(reason),
        Vec::new(),
        plan,
        result,
        &test_result,
        clock,
    );
    write_push_result(
        store,
        binding,
        step_id,
        step_order,
        payload,
        Some(("fatal", reason, details)),
        clock,
    )?;
    Ok(StepOutcome::Fatal)
}

// Pre-existing artifact writer shape shared by push remediation.
#[allow(clippy::too_many_arguments)]
fn write_retryable_push_failure(
    store: &PrFollowupArtifactStore,

    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    retry_index: u64,
    max_push_retries: u64,
    remote_ref: &str,
    reason: &str,
    commands: Vec<Value>,
    inspection: &PushInspection,
    plan: &Value,
    result: &Value,
    test_result: &Value,
    clock: &dyn ClockSleeper,
) -> Result<StepOutcome, EngineError> {
    let exhausted = retry_index >= max_push_retries;
    let state = if exhausted {
        "retry_exhausted"
    } else {
        "retryable_failed"
    };
    let payload = push_payload(
        binding,
        state,
        retry_index,
        max_push_retries,
        remote_ref,
        &inspection.pre_push_local_head_sha,
        &inspection.pre_push_remote_head_sha,
        &binding.head_sha,
        None,
        &inspection.pre_push_local_head_sha,
        Some(&inspection.pre_push_remote_head_sha),
        &inspection.pre_push_local_head_sha,
        false,
        inspection.included_paths.clone(),
        inspection.excluded_paths.clone(),
        None,
        Some(reason),
        commands,
        plan,
        result,
        test_result,
        clock,
    );
    write_push_result(
        store,
        binding,
        step_id,
        step_order,
        payload,
        Some((state, reason, json!({ "push_retry_index": retry_index }))),
        clock,
    )?;
    Ok(if exhausted {
        StepOutcome::Fatal
    } else {
        StepOutcome::Retryable
    })
}

// Pre-existing artifact writer shape shared by push remediation.
#[allow(clippy::too_many_arguments)]
fn write_push_failure_from_observation(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    max_push_retries: u64,
    remote_ref: &str,
    state: &str,
    reason: &str,
    commands: Vec<Value>,
    plan: &Value,
    result: &Value,
    test_result: &Value,
    clock: &dyn ClockSleeper,
) -> Result<StepOutcome, EngineError> {
    let fatal = state == "fatal";
    let payload = push_payload(
        binding,
        if fatal { "fatal" } else { "retryable_failed" },
        0,
        max_push_retries,
        remote_ref,
        "unknown",
        "unknown",
        &binding.head_sha,
        None,
        "unknown",
        None,
        "unknown",
        false,
        Vec::new(),
        Vec::new(),
        None,
        Some(reason),
        commands,
        plan,
        result,
        test_result,
        clock,
    );
    write_push_result(
        store,
        binding,
        step_id,
        step_order,
        payload,
        Some((state, reason, json!({}))),
        clock,
    )?;
    Ok(if fatal {
        StepOutcome::Fatal
    } else {
        StepOutcome::Retryable
    })
}

#[allow(clippy::too_many_arguments)]
fn push_payload(
    binding: &PrFollowupBinding,
    push_state: &str,
    retry_index: u64,
    max_push_retries: u64,
    remote_ref: &str,
    pre_push_local_head_sha: &str,
    pre_push_remote_head_sha: &str,
    pre_push_pr_head_sha: &str,
    committed_head_sha: Option<&str>,
    post_push_local_head_sha: &str,
    post_push_remote_head_sha: Option<&str>,
    expected_head_sha: &str,
    verified_remote_matches_expected: bool,
    staged_paths: Vec<String>,
    excluded_paths: Vec<String>,
    commit_message: Option<&str>,
    push_error_class: Option<&str>,
    commands: Vec<Value>,
    plan: &Value,
    result: &Value,
    test_result: &Value,
    clock: &dyn ClockSleeper,
) -> Value {
    json!({
        "push_state": push_state,
        "push_retry_index": retry_index,
        "max_push_retries": max_push_retries,
        "retry_scope": {
            "run_id": binding.run_id,
            "repository_owner": binding.repository_owner,
            "repository_name": binding.repository_name,
            "pr_number": binding.pr_number,
            "head_sha": pre_push_local_head_sha,
            "remote_ref": remote_ref
        },
        "remote_ref": remote_ref,
        "pre_push_local_head_sha": pre_push_local_head_sha,
        "pre_push_remote_head_sha": pre_push_remote_head_sha,
        "pre_push_pr_head_sha": pre_push_pr_head_sha,
        "committed_head_sha": committed_head_sha,
        "post_push_local_head_sha": post_push_local_head_sha,
        "post_push_remote_head_sha": post_push_remote_head_sha,
        "expected_head_sha": expected_head_sha,
        "verified_remote_matches_expected": verified_remote_matches_expected,
        "staged_paths": staged_paths,
        "excluded_paths": excluded_paths,
        "commit_message": commit_message,
        "push_error_class": push_error_class,
        "commands": commands,
        "stdout_artifact_path": Value::Null,
        "stderr_artifact_path": Value::Null,
        "source_artifacts": [
            source_artifact(plan, "pr-remediation-plan"),
            source_artifact(result, "pr-remediation-result"),
            source_artifact(test_result, "post-pr-test-result")
        ],
        "pushed_at": clock.now_rfc3339()
    })
}

fn write_push_result(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    payload: Value,
    failure: Option<(&str, &str, Value)>,
    clock: &dyn ClockSleeper,
) -> Result<(), EngineError> {
    store.write_json_artifact(
        binding,
        "push-remediation-result",
        step_id,
        step_order,
        &payload,
        failure,
        clock,
    )?;
    Ok(())
}

fn push_command_result_json(result: &PushRemediationCommandResult) -> Value {
    json!({
        "command_id": result.command_id,
        "argv": result.argv,
        "working_directory": result.working_directory.display().to_string(),
        "status": result.status,
        "exit_code": result.exit_code,
        "signal": result.signal,
        "bounded_stdout": result.bounded_stdout,
        "bounded_stderr": result.bounded_stderr,
        "stdout_artifact_path": result.stdout_log_path.as_ref().map(|path| path.display().to_string()),
        "stderr_artifact_path": result.stderr_log_path.as_ref().map(|path| path.display().to_string()),
        "spawn_error": result.spawn_error
    })
}

// Pre-existing process orchestration flow; split in a dedicated refactor stage.
#[allow(clippy::too_many_lines)]
fn run_push_remediation_process(
    request: PushRemediationCommandRequest,
) -> PushRemediationCommandResult {
    let mut command = Command::new(&request.argv[0]);
    command.args(&request.argv[1..]);
    command.current_dir(&request.working_directory);
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.stdin(Stdio::null());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            write_optional_log(&request.stdout_log_path, "");
            write_optional_log(&request.stderr_log_path, &err.to_string());
            return PushRemediationCommandResult {
                command_id: request.command_id,
                argv: request.argv,
                working_directory: request.working_directory,
                exit_code: None,
                signal: None,
                status: "fatal".to_string(),
                bounded_stdout: String::new(),
                bounded_stderr: bounded_excerpt(&err.to_string(), 4096),
                stdout_log_path: Some(request.stdout_log_path),
                stderr_log_path: Some(request.stderr_log_path),
                spawn_error: Some(err.to_string()),
            };
        }
    };
    let stdout_buffer = Arc::new(Mutex::new(String::new()));
    let stderr_buffer = Arc::new(Mutex::new(String::new()));
    let stdout_reader = child.stdout.take().map(|mut stdout| {
        let buffer = Arc::clone(&stdout_buffer);
        thread::spawn(move || read_stream_into_string(&mut stdout, &buffer))
    });
    let stderr_reader = child.stderr.take().map(|mut stderr| {
        let buffer = Arc::clone(&stderr_buffer);
        thread::spawn(move || read_stream_into_string(&mut stderr, &buffer))
    });
    let start = Instant::now();
    let timeout = Duration::from_secs(request.timeout_seconds);
    let mut timed_out = false;
    let mut exit_code = None;
    while start.elapsed() < timeout {
        match child.try_wait() {
            Ok(Some(status)) => {
                exit_code = status.code();
                break;
            }
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(err) => {
                write_optional_log(&request.stdout_log_path, "");
                write_optional_log(&request.stderr_log_path, &err.to_string());
                return PushRemediationCommandResult {
                    command_id: request.command_id,
                    argv: request.argv,
                    working_directory: request.working_directory,
                    exit_code: None,
                    signal: None,
                    status: "fatal".to_string(),
                    bounded_stdout: String::new(),
                    bounded_stderr: bounded_excerpt(&err.to_string(), 4096),
                    stdout_log_path: Some(request.stdout_log_path),
                    stderr_log_path: Some(request.stderr_log_path),
                    spawn_error: Some(err.to_string()),
                };
            }
        }
    }
    if exit_code.is_none() {
        timed_out = true;
        let _ = child.kill();
        let _ = child.wait();
    }
    if let Some(reader) = stdout_reader {
        let _ = reader.join();
    }
    if let Some(reader) = stderr_reader {
        let _ = reader.join();
    }
    let stdout = stdout_buffer
        .lock()
        .map_or_else(|_| String::new(), |text| text.clone());
    let stderr = stderr_buffer
        .lock()
        .map_or_else(|_| String::new(), |text| text.clone());
    write_optional_log(&request.stdout_log_path, &stdout);
    write_optional_log(&request.stderr_log_path, &stderr);
    PushRemediationCommandResult {
        command_id: request.command_id,
        argv: request.argv,
        working_directory: request.working_directory,
        exit_code,
        signal: None,
        status: if timed_out {
            "fatal"
        } else if exit_code == Some(0) {
            "passed"
        } else {
            "failed"
        }
        .to_string(),
        bounded_stdout: bounded_excerpt(&stdout, 4096),
        bounded_stderr: bounded_excerpt(&stderr, 4096),
        stdout_log_path: Some(request.stdout_log_path),
        stderr_log_path: Some(request.stderr_log_path),
        spawn_error: timed_out.then(|| "push remediation command timed out".to_string()),
    }
}

/// Post-PR iteration guard executor for `post_pr_iteration_guard`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-020
/// @pseudocode lines 8-15
#[derive(Debug, Default)]
pub struct PostPrIterationGuardExecutor;

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-020
/// @pseudocode lines 8-15
impl StepExecutor for PostPrIterationGuardExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        let artifact_root = artifact_root(context, params)?;
        let store = PrFollowupArtifactStore::new(artifact_root);
        ensure_legacy_harness_inputs(
            &store,
            &binding_from_params(context, params),
            &SystemClockSleeper,
        )?;
        let pr = store.read_current_json(&binding_from_params(context, params), "pr")?;
        let binding = binding_from_value(&pr)?;
        let max_iterations = u64_param(params, "max_post_pr_remediation_iterations", 3);
        let previous = latest_guard_for_current_run(&store, &binding)?;
        let (iteration_index, previous_head_sha, reason) = match previous.as_ref() {
            None => (0, Value::Null, "initial_entry"),
            Some(guard)
                if guard.get("head_sha").and_then(Value::as_str)
                    == Some(binding.head_sha.as_str()) =>
            {
                (
                    guard
                        .get("iteration_index")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    Value::String(binding.head_sha.clone()),
                    "same_head_reentry",
                )
            }
            Some(guard) => (
                guard
                    .get("iteration_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    + 1,
                guard.get("head_sha").cloned().unwrap_or(Value::Null),
                "head_sha_changed_after_remediation_push",
            ),
        };
        let exceeded = iteration_index > max_iterations;
        let payload = json!({
            "guard_state": if exceeded { "max_iterations_exceeded" } else { "proceed" },
            "iteration_index": iteration_index,
            "max_post_pr_remediation_iterations": max_iterations,
            "previous_head_sha": previous_head_sha,
            "reason": if exceeded { "max_iterations_exceeded" } else { reason },
            "ignored_stale_artifacts": [],
            "updated_at": SystemClockSleeper.now_rfc3339()
        });
        let failure = exceeded.then(|| {
            (
                "fatal",
                "max_iterations_exceeded",
                json!({
                    "iteration_index": iteration_index,
                    "max_post_pr_remediation_iterations": max_iterations
                }),
            )
        });
        store.write_json_artifact(
            &binding,
            "post-pr-iteration-guard",
            "post_pr_iteration_guard",
            u64_param(params, "step_order_index", 2),
            &payload,
            failure,
            &SystemClockSleeper,
        )?;
        if exceeded {
            Ok(StepOutcome::Fatal)
        } else {
            Ok(StepOutcome::Success)
        }
    }
}

fn latest_guard_for_current_run(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
) -> Result<Option<Value>, EngineError> {
    let root = store
        .root()
        .join("pr-followup")
        .join("history")
        .join(&binding.run_id)
        .join(&binding.repository_owner)
        .join(&binding.repository_name)
        .join(binding.pr_number.to_string())
        .join("post-pr-iteration-guard");
    if !root.exists() {
        return Ok(None);
    }
    let mut values = Vec::new();
    for entry in std::fs::read_dir(&root)
        .map_err(|err| pr_remediation_error(format!("read guard history: {err}")))?
    {
        let path = entry
            .map_err(|err| pr_remediation_error(format!("read guard history entry: {err}")))?
            .path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let content = std::fs::read_to_string(&path).map_err(|err| {
            pr_remediation_error(format!("read guard artifact {}: {err}", path.display()))
        })?;
        let value: Value = serde_json::from_str(&content).map_err(|err| {
            pr_remediation_error(format!("parse guard artifact {}: {err}", path.display()))
        })?;
        if binding_from_value(&value).is_ok_and(|actual| {
            actual.run_id == binding.run_id
                && actual.repository_owner == binding.repository_owner
                && actual.repository_name == binding.repository_name
                && actual.pr_number == binding.pr_number
        }) {
            values.push(value);
        }
    }
    values.sort_by_key(|value| {
        value
            .get("artifact_sequence")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    });
    Ok(values.pop())
}

/// Post-PR failure terminal executor contract for `post_pr_failure_terminal`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-020
/// @pseudocode lines 50-53
#[derive(Debug, Default)]
pub struct PostPrFailureTerminalExecutor;

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-020
/// @pseudocode lines 50-53
impl StepExecutor for PostPrFailureTerminalExecutor {
    fn execute(
        &self,
        _context: &mut StepContext,
        _params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        Ok(StepOutcome::Fatal)
    }
}
