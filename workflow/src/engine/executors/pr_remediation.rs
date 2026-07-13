//! PR follow-through remediation, verification, push, guard, and terminal contracts.
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
//! @requirement:REQ-PRFU-013,REQ-PRFU-015,REQ-PRFU-017,REQ-PRFU-020
//! @pseudocode lines 1-53

mod failure_terminal;
mod post_pr_plan_inputs;
mod post_pr_stages;
mod post_pr_test_process;
mod push_auth_preflight;
mod push_porcelain;
mod push_stages;
mod push_support;
mod result_freshness;
mod retry_state;
pub use self::failure_terminal::PostPrFailureTerminalExecutor;
use self::post_pr_plan_inputs::{
    append_pending_ci_judgment, append_post_pr_test_failures, read_plan_inputs,
    remediation_plan_covers_current_post_pr_test_result, remediation_plan_source_artifacts,
};
use self::post_pr_stages::{sanitize_command_id, validate_safe_working_directory};
pub use self::post_pr_stages::{
    PostPrIterationGuardExecutor, PostPrTestCommandRequest, PostPrTestCommandResult,
    PostPrTestCommandRunner, RunPostPrTestsExecutor, RunPostPrTestsExecutorWithRunner,
    SystemPostPrTestCommandRunner,
};
use self::post_pr_test_process::apply_allowed_command_environment;
pub use self::push_stages::{
    PushRemediationChangesExecutor, PushRemediationChangesExecutorWithRunner,
    PushRemediationCommandRequest, PushRemediationCommandResult, PushRemediationCommandRunner,
};
use self::result_freshness::{
    remediation_result_state, result_file_non_empty, PreviousResultSnapshot, RemediationResultState,
};
use self::retry_state::{
    load_current_state, record_launch_phase, record_validation, reserve_launch, LaunchPhase,
    RetryScopeKey, RetryState, ValidationTransition,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::engine::executor::{interpolate_string, StepContext, StepExecutor};
use crate::engine::executors::github_feedback::normalize_legacy_pending_marker_artifact;
use crate::engine::executors::pr_followup_artifacts::{
    ArtifactWriter, ClockSleeper, PrFollowupArtifactStore, SystemClockSleeper,
    SystemPrFollowupFilesystem,
};
use crate::engine::executors::pr_followup_types::{
    value_has_summary_marker_key, ArtifactSequenceMetadata, PlanState, PrFollowupBinding,
    ValidationState, PR_FOLLOWUP_SCHEMA_VERSION,
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
    let step_id = current_step_id(context, "build_remediation_plan");
    let step_order = u64_param(params, "step_order_index", 7);
    let fallback = binding_from_params(context, params);
    let binding = match binding_for_context(context, params, &store, clock) {
        Ok(binding) => binding,
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
    let pr = match store.read_current_json(&binding, "pr") {
        Ok(value) => value,
        Err(err) => {
            return write_fatal_plan(
                &store,
                &binding,
                &step_id,
                step_order,
                clock,
                "missing_or_unbindable_pr",
                vec![json!({ "artifact_family": "pr", "error": err.to_string() })],
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

    append_pending_ci_judgment(
        &inputs.ci_failures,
        &binding,
        &mut pending_or_unknown,
        &mut needs_user_judgment,
    );

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

    if let Some(post_pr_test_result) = inputs.post_pr_test_result.as_ref() {
        append_post_pr_test_failures(post_pr_test_result, &binding, &mut must_fix);
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
                if !value_has_summary_marker_key(result) {
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

    let plan_state = if !must_fix.is_empty() {
        PlanState::NeedsRemediation
    } else if !needs_user_judgment.is_empty() {
        PlanState::BlockedNeedsUserJudgment
    } else {
        PlanState::Clean
    };

    let source_artifacts = remediation_plan_source_artifacts(&pr, &inputs);

    let payload = RemediationPlanArtifact {
        plan_state,
        must_fix,
        mark_invalid: mark_invalid.clone(),
        needs_user_judgment,
        pending_or_unknown,
        source_artifacts,
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

#[cfg(test)]
mod issue132_pending_action_tests;
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 9-10
mod pending_marker_actions;
use pending_marker_actions::*;

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

fn binding_for_context(
    context: &StepContext,
    params: &Value,
    store: &PrFollowupArtifactStore,
    clock: &dyn ClockSleeper,
) -> Result<PrFollowupBinding, EngineError> {
    let requested = binding_from_params(context, params);
    if let Some(value) = store.find_current_pr_artifact_for_run(context.run_id(), &requested)? {
        return binding_from_value(&value);
    }
    ensure_legacy_harness_inputs(store, &requested, clock)?;
    let pr = store.read_current_json(&requested, "pr")?;
    binding_from_value(&pr)
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
fn remediate_pr_followup(
    context: &StepContext,
    params: &Value,
    clock: &dyn ClockSleeper,
    runner: &dyn PrFollowupLlxprtCommandRunner,
) -> Result<StepOutcome, EngineError> {
    let mut run = RemediationRun::prepare(context, params, clock)?;
    run.invoke(context, params, runner);
    run.handle_result_state(clock)?;
    run.write_llxprt_run_artifact(clock)?;
    run.complete_launch(clock)?;
    Ok(run.outcome())
}

struct RemediationRun {
    store: PrFollowupArtifactStore,
    binding: PrFollowupBinding,
    plan: Value,
    step_id: String,
    step_order: u64,
    result_path: PathBuf,
    previous_result: Option<Value>,
    previous_result_snapshot: PreviousResultSnapshot,
    retry_state: RetryState,
    argv: Vec<String>,
    invocation: Option<LlxprtInvocationResult>,
}

impl RemediationRun {
    fn prepare(
        context: &StepContext,
        params: &Value,
        clock: &dyn ClockSleeper,
    ) -> Result<Self, EngineError> {
        let artifact_root = artifact_root(context, params)?;
        let store =
            PrFollowupArtifactStore::with_filesystem(&artifact_root, &SystemPrFollowupFilesystem)?;
        let binding = binding_for_context(context, params, &store, clock)?;
        let plan = read_or_build_remediation_plan(context, params, clock, &store, &binding)?;
        let result_path = store.canonical_path(&binding, "pr-remediation-result");
        let step_id = current_step_id(context, "remediate_pr_followup");
        let step_order = u64_param(params, "step_order_index", 8);
        let mut retry_state =
            reserve_launch(&store, &binding, &plan, params, &step_id, step_order, clock)?;
        record_launch_phase(
            &store,
            &binding,
            &step_id,
            step_order,
            &mut retry_state,
            LaunchPhase::Launched,
            clock,
        )?;
        let previous_result = store
            .read_current_raw_json(&binding, "pr-remediation-result")
            .ok()
            .unwrap_or_else(|| json!({}));
        let previous_result = Some(project_engine_retry_state(previous_result, &retry_state));
        let previous_result_snapshot = PreviousResultSnapshot::capture(&result_path);
        let prompt =
            render_remediation_prompt(&binding, &plan, &result_path, previous_result.as_ref());
        Ok(Self {
            store,
            binding,
            plan,
            step_id,
            step_order,
            result_path,
            previous_result,
            previous_result_snapshot,
            retry_state,
            argv: remediation_argv(params, &prompt, context),
            invocation: None,
        })
    }

    fn invoke(
        &mut self,
        context: &StepContext,
        params: &Value,
        runner: &dyn PrFollowupLlxprtCommandRunner,
    ) {
        let mut invocation = runner.invoke(self.invocation_request(context, params));
        if invocation.argv.is_empty() {
            invocation.argv.clone_from(&self.argv);
        }
        if invocation.working_directory.as_os_str().is_empty() {
            invocation.working_directory = context.work_dir().clone();
        }
        if let Some(path) = &invocation.result_file_path {
            self.result_path = path.clone();
        }
        self.invocation = Some(invocation);
    }

    fn invocation_request(&self, context: &StepContext, params: &Value) -> LlxprtInvocationRequest {
        let run_path = self
            .store
            .canonical_path(&self.binding, "pr-remediation-llxprt-run");
        LlxprtInvocationRequest {
            argv: self.argv.clone(),
            working_directory: context.work_dir().clone(),
            timeout_seconds: u64_param(params, "timeout_seconds", 1800),
            stdout_log_path: run_path.with_file_name("pr-remediation-llxprt-stdout.log"),
            stderr_log_path: run_path.with_file_name("pr-remediation-llxprt-stderr.log"),
            remediation_plan_path: self
                .store
                .canonical_path(&self.binding, "pr-remediation-plan"),
            remediation_result_path: self.result_path.clone(),
            success_file_path: params
                .get("success_file")
                .and_then(Value::as_str)
                .map(|path| resolve_path(context.work_dir(), path)),
        }
    }

    fn handle_result_state(&mut self, clock: &dyn ClockSleeper) -> Result<(), EngineError> {
        let result_state = self.result_state();
        let invocation = self
            .invocation
            .as_mut()
            .expect("remediation invocation must be recorded before result handling");
        let process_class_before_result_reclassification = invocation.process_class.clone();
        if result_state.was_updated && invocation.process_class == "timeout" {
            invocation.process_class = "success".to_string();
            invocation.spawn_error = None;
        }
        if !result_state.available {
            return self.write_failure_result(clock);
        }
        if result_state.validator_readable
            && process_class_before_result_reclassification == "timeout"
        {
            self.repair_timeout_wrapper_failure_result(clock)?;
        }
        Ok(())
    }

    fn result_state(&self) -> RemediationResultState {
        remediation_result_state(
            self.invocation(),
            &self.result_path,
            &self.previous_result_snapshot,
        )
    }

    fn write_failure_result(&self, clock: &dyn ClockSleeper) -> Result<(), EngineError> {
        write_validator_readable_remediation_failure_result(
            &self.store,
            &self.binding,
            &self.step_id,
            self.step_order,
            &self.plan,
            self.invocation(),
            &self.result_path,
            self.previous_result.as_ref(),
            clock,
        )
    }

    fn repair_timeout_wrapper_failure_result(
        &self,
        clock: &dyn ClockSleeper,
    ) -> Result<(), EngineError> {
        repair_timeout_wrapper_failure_result_if_needed(
            &self.store,
            &self.binding,
            &self.step_id,
            self.step_order,
            &self.plan,
            self.invocation(),
            &self.result_path,
            self.previous_result.as_ref(),
            clock,
        )
    }

    fn write_llxprt_run_artifact(&self, clock: &dyn ClockSleeper) -> Result<(), EngineError> {
        let validator_readable = result_file_non_empty(&self.result_path);
        let state = invocation_state(self.invocation(), validator_readable);
        write_llxprt_run_artifact(
            &self.store,
            &self.binding,
            &self.step_id,
            self.step_order,
            &self.plan,
            &self.result_path,
            self.invocation(),
            &state,
            validator_readable,
            clock,
        )
    }

    fn complete_launch(&mut self, clock: &dyn ClockSleeper) -> Result<(), EngineError> {
        record_launch_phase(
            &self.store,
            &self.binding,
            &self.step_id,
            self.step_order,
            &mut self.retry_state,
            LaunchPhase::Completed,
            clock,
        )
    }

    fn outcome(&self) -> StepOutcome {
        if result_file_non_empty(&self.result_path) {
            StepOutcome::Success
        } else {
            StepOutcome::Fatal
        }
    }

    fn invocation(&self) -> &LlxprtInvocationResult {
        self.invocation
            .as_ref()
            .expect("remediation invocation must be recorded before artifact handling")
    }
}

fn project_engine_retry_state(mut result: Value, state: &RetryState) -> Value {
    let object = result
        .as_object_mut()
        .expect("engine retry projection must be a JSON object");
    object.insert(
        "remediation_attempt_index".to_string(),
        json!(state.counters.remediation_attempt_index),
    );
    object.insert(
        "max_remediation_attempts".to_string(),
        json!(state.budget.max_remediation_attempts),
    );
    object.insert(
        "validation_retry_index".to_string(),
        json!(state.counters.validation_retry_index),
    );
    object.insert(
        "max_validation_retries".to_string(),
        json!(state.budget.max_validation_retries),
    );
    object.insert(
        "stale_artifact_retry_index".to_string(),
        json!(state.counters.stale_artifact_retry_index),
    );
    object.insert(
        "max_stale_artifact_retries".to_string(),
        json!(state.budget.max_stale_artifact_retries),
    );
    let scope = object
        .entry("retry_scope".to_string())
        .or_insert_with(|| json!({}));
    if let Some(scope) = scope.as_object_mut() {
        scope.insert(
            "remediation_attempt_index".to_string(),
            json!(state.counters.remediation_attempt_index),
        );
        scope.insert(
            "max_remediation_attempts".to_string(),
            json!(state.budget.max_remediation_attempts),
        );
        scope.insert(
            "validation_retry_index".to_string(),
            json!(state.counters.validation_retry_index),
        );
        scope.insert(
            "max_validation_retries".to_string(),
            json!(state.budget.max_validation_retries),
        );
        scope.insert(
            "stale_artifact_retry_index".to_string(),
            json!(state.counters.stale_artifact_retry_index),
        );
        scope.insert(
            "max_stale_artifact_retries".to_string(),
            json!(state.budget.max_stale_artifact_retries),
        );
    }
    result
}

fn read_or_build_remediation_plan(
    context: &StepContext,
    params: &Value,
    clock: &dyn ClockSleeper,
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
) -> Result<Value, EngineError> {
    match store.read_current_json(binding, "pr-remediation-plan") {
        Ok(plan) => {
            if remediation_plan_covers_current_post_pr_test_result(store, binding, &plan)? {
                Ok(plan)
            } else {
                rebuild_and_read_remediation_plan(context, params, clock, store, binding)
            }
        }
        Err(_) => rebuild_and_read_remediation_plan(context, params, clock, store, binding),
    }
}

fn rebuild_and_read_remediation_plan(
    context: &StepContext,
    params: &Value,
    clock: &dyn ClockSleeper,
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
) -> Result<Value, EngineError> {
    build_remediation_plan(context, params, clock)?;
    store.read_current_json(binding, "pr-remediation-plan")
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
        "PR follow-up remediation for {}/{}, PR #{} at head {}.\n\nRead {}. Fix only pr-remediation-plan.json.must_fix. Do not fix pr-remediation-plan.json.mark_invalid, out_of_scope feedback, or pr-remediation-plan.json.needs_user_judgment. Write {}. Use only canonical statuses fixed | changed | already_satisfied | not_reproduced | not_fixed | skipped | failed. Include structured evidence for every result item. Required result schema: every result item must copy source_type, source_id, stable_marker_key, and body_hash exactly from the corresponding pr-remediation-plan.json.must_fix item; every result item must include input_head_sha set to {} and output_head_sha set to the current PR head after remediation; every result item must also include response_text, a non-empty reviewer-facing message that Luther will post verbatim on the original review thread (do not post it yourself); fixed or changed results must include evidence.current_head_sha equal to the current PR head; already_satisfied or not_reproduced results must include evidence.current_head_sha equal to {}. already_satisfied results must also include evidence.commands with at least one command object whose status is passed and whose argv array is non-empty. Copy pr-remediation-plan.json artifact_sequence into top-level plan_artifact_sequence and retry_scope.plan_artifact_sequence; include complete retry_scope fields: retry_scope.scope_kind remediation_result_validation, retry_scope.run_id, retry_scope.repository_owner, retry_scope.repository_name, retry_scope.pr_number, retry_scope.input_head_sha, retry_scope.output_head_sha after remediation, retry_scope.remediation_attempt_index, retry_scope.max_remediation_attempts, retry_scope.validation_retry_index, retry_scope.max_validation_retries, retry_scope.stale_artifact_retry_index, and retry_scope.max_stale_artifact_retries. Free-form-only completion is not acceptable; pr-remediation-result.json is required. Write only the requested canonical current pr-remediation-result.json path; do not create, copy, or modify any pr-followup/history files or artifact metadata fields.{}",
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
    previous_result: Option<&Value>,
    clock: &dyn ClockSleeper,
) -> Result<(), EngineError> {
    let context = FailureResultContext {
        binding,
        plan,
        invocation,
        result_path,
        previous_result,
        clock,
    };
    let payload = wrapper_failure_payload(&context);
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

struct FailureResultContext<'a> {
    binding: &'a PrFollowupBinding,
    plan: &'a Value,
    invocation: &'a LlxprtInvocationResult,
    result_path: &'a Path,
    previous_result: Option<&'a Value>,
    clock: &'a dyn ClockSleeper,
}

fn wrapper_failure_payload(context: &FailureResultContext<'_>) -> Value {
    let output_head_sha = current_git_head(&context.invocation.working_directory)
        .unwrap_or_else(|| context.binding.head_sha.clone());
    let stale_counters = stale_artifact_retry_counters_from_result(context.previous_result);
    let normal_counters = normal_retry_counters_from_result(context.previous_result);
    json!({
        "input_head_sha": context.binding.head_sha,
        "output_head_sha": output_head_sha,
        "head_sha": context.binding.head_sha,
        "overall_status": "failed",
        "results": wrapper_failure_results(context),
        "verification_commands": [],
        "success_file_path": Value::Null,
        "validation_state": ValidationState::Unvalidated.as_str(),
        "validation_retry_index": normal_counters.validation_retry_index,
        "max_validation_retries": normal_counters.max_validation_retries,
        "remediation_attempt_index": normal_counters.remediation_attempt_index,
        "max_remediation_attempts": normal_counters.max_remediation_attempts,
        "stale_artifact_retry_index": stale_counters.0,
        "max_stale_artifact_retries": stale_counters.1,
        "retry_scope": wrapper_failure_retry_scope(context, &output_head_sha, normal_counters, stale_counters),
        "plan_artifact_sequence": context.plan.get("artifact_sequence"),
        "unsuccessful_statuses": ["failed"],
        "no_change_after_remediation": true,
        "wrapper_failure_result_path": context.result_path.display().to_string(),
        "written_at": context.clock.now_rfc3339()
    })
}

fn wrapper_failure_results(context: &FailureResultContext<'_>) -> Vec<Value> {
    context
        .plan
        .get("must_fix")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|item| wrapper_failure_result_item(context, item))
        .collect()
}

fn wrapper_failure_result_item(context: &FailureResultContext<'_>, item: &Value) -> Value {
    json!({
        "source_type": item.get("source_type").cloned().unwrap_or(Value::Null),
        "source_id": item.get("source_id").cloned().unwrap_or(Value::Null),
        "stable_marker_key": item.get("stable_marker_key").cloned().unwrap_or(Value::Null),
        "body_hash": item.get("body_hash").cloned().unwrap_or(Value::Null),
        "input_head_sha": context.binding.head_sha,
        "status": "failed",
        "action": "llxprt_invocation_failed_before_result",
        "response_text": "Luther could not complete remediation for this item because the remediation agent invocation failed before producing a result. This thread is left open pending a retry.",
        "evidence": wrapper_failure_evidence(context),
        "evidence_paths": wrapper_failure_evidence_paths(context.invocation)
    })
}

fn wrapper_failure_evidence(context: &FailureResultContext<'_>) -> Value {
    json!({
        "kind": "llxprt_invocation",
        "current_head_sha": context.binding.head_sha,
        "process_class": context.invocation.process_class,
        "exit_code": context.invocation.exit_code,
        "signal": context.invocation.signal,
        "stdout_excerpt": context.invocation.bounded_stdout,
        "stderr_excerpt": context.invocation.bounded_stderr,
        "changed_paths": context.invocation.changed_paths,
        "argv": context.invocation.argv,
        "working_directory": context.invocation.working_directory.display().to_string()
    })
}

fn wrapper_failure_evidence_paths(invocation: &LlxprtInvocationResult) -> [Option<String>; 2] {
    [
        invocation
            .stdout_log_path
            .as_ref()
            .map(|path| path.display().to_string()),
        invocation
            .stderr_log_path
            .as_ref()
            .map(|path| path.display().to_string()),
    ]
}

fn wrapper_failure_retry_scope(
    context: &FailureResultContext<'_>,
    output_head_sha: &str,
    normal_counters: NormalRetryCounters,
    stale_counters: (u64, u64),
) -> Value {
    json!({
        "run_id": context.binding.run_id,
        "repository_owner": context.binding.repository_owner,
        "repository_name": context.binding.repository_name,
        "pr_number": context.binding.pr_number,
        "input_head_sha": context.binding.head_sha,
        "output_head_sha": output_head_sha,
        "plan_artifact_sequence": context.plan.get("artifact_sequence"),
        "remediation_attempt_index": normal_counters.remediation_attempt_index,
        "max_remediation_attempts": normal_counters.max_remediation_attempts,
        "validation_retry_index": normal_counters.validation_retry_index,
        "max_validation_retries": normal_counters.max_validation_retries,
        "stale_artifact_retry_index": stale_counters.0,
        "max_stale_artifact_retries": stale_counters.1,
        "scope_kind": "remediation_result_validation"
    })
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

#[allow(clippy::too_many_arguments)]
fn repair_timeout_wrapper_failure_result_if_needed(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    plan: &Value,
    invocation: &LlxprtInvocationResult,
    result_path: &Path,
    previous_result: Option<&Value>,
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
        previous_result,
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
    let mut child = match spawn_llxprt_child(&request) {
        Ok(child) => child,
        Err(err) => return llxprt_spawn_error_result(&request, err),
    };
    match wait_for_llxprt_child(&mut child, request.timeout_seconds) {
        Ok(capture) => llxprt_process_result(request, capture),
        Err(err) => llxprt_wait_error_result(&request, err),
    }
}

fn spawn_llxprt_child(request: &LlxprtInvocationRequest) -> std::io::Result<std::process::Child> {
    let mut command = Command::new(&request.argv[0]);
    command.args(&request.argv[1..]);
    command.current_dir(&request.working_directory);
    command.env_clear();
    apply_allowed_command_environment(&mut command);
    command.env("PWD", &request.working_directory);
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.stdin(Stdio::null());
    #[cfg(unix)]
    command.process_group(0);
    command.spawn()
}

fn wait_for_llxprt_child(
    child: &mut std::process::Child,
    timeout_seconds: u64,
) -> Result<ProcessOutputCapture, ProcessOutputCaptureError> {
    let stdout_buffer = Arc::new(Mutex::new(String::new()));
    let stderr_buffer = Arc::new(Mutex::new(String::new()));
    let stdout_reader = spawn_reader(child.stdout.take(), &stdout_buffer);
    let stderr_reader = spawn_reader(child.stderr.take(), &stderr_buffer);
    let wait_result = wait_for_child_exit(child, timeout_seconds);
    join_reader(stdout_reader);
    join_reader(stderr_reader);
    let capture = ProcessOutputCapture {
        stdout_buffer,
        stderr_buffer,
        exit_code: None,
        timed_out: false,
    };
    match wait_result {
        Ok((exit_code, timed_out)) => Ok(capture.with_status(exit_code, timed_out)),
        Err(err) => Err(ProcessOutputCaptureError { err, capture }),
    }
}

fn llxprt_process_result(
    request: LlxprtInvocationRequest,
    capture: ProcessOutputCapture,
) -> LlxprtInvocationResult {
    let stdout = capture.stdout_text();
    let stderr = capture.stderr_text();
    write_optional_log(&request.stdout_log_path, &stdout);
    write_optional_log(&request.stderr_log_path, &stderr);
    llxprt_invocation_result_with_output(
        &request,
        capture.exit_code,
        capture.timed_out,
        stdout,
        stderr,
        None,
    )
}

fn llxprt_process_class(timed_out: bool, exit_code: Option<i32>) -> &'static str {
    if timed_out {
        "timeout"
    } else if exit_code == Some(0) {
        "success"
    } else {
        "retryable_failed"
    }
}

fn llxprt_spawn_error_result(
    request: &LlxprtInvocationRequest,
    err: std::io::Error,
) -> LlxprtInvocationResult {
    invocation_result_from_request(
        request,
        None,
        None,
        "spawn_failed",
        String::new(),
        err.to_string(),
        Some(err.to_string()),
    )
}

fn llxprt_wait_error_result(
    request: &LlxprtInvocationRequest,
    err: ProcessOutputCaptureError,
) -> LlxprtInvocationResult {
    let stdout = err.capture.stdout_text();
    let mut stderr = err.capture.stderr_text();
    append_wait_error_to_stderr(&mut stderr, &err.err);
    write_optional_log(&request.stdout_log_path, &stdout);
    write_optional_log(&request.stderr_log_path, &stderr);
    invocation_result_from_request(
        request,
        None,
        None,
        "fatal",
        stdout,
        stderr,
        Some(err.err.to_string()),
    )
}

fn append_wait_error_to_stderr(stderr: &mut String, err: &std::io::Error) {
    if !stderr.is_empty() && !stderr.ends_with('\n') {
        stderr.push('\n');
    }
    stderr.push_str("llxprt wait failed: ");
    stderr.push_str(&err.to_string());
}

fn llxprt_invocation_result_with_output(
    request: &LlxprtInvocationRequest,
    exit_code: Option<i32>,
    timed_out: bool,
    stdout: String,
    stderr: String,
    spawn_error: Option<String>,
) -> LlxprtInvocationResult {
    invocation_result_from_request(
        request,
        exit_code,
        None,
        llxprt_process_class(timed_out, exit_code),
        stdout,
        stderr,
        spawn_error,
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

fn spawn_reader<R>(reader: Option<R>, buffer: &Arc<Mutex<String>>) -> Option<thread::JoinHandle<()>>
where
    R: Read + Send + 'static,
{
    reader.map(|mut reader| {
        let buffer = Arc::clone(buffer);
        thread::spawn(move || read_stream_into_string(&mut reader, &buffer))
    })
}

fn join_reader(reader: Option<thread::JoinHandle<()>>) {
    if let Some(reader) = reader {
        let _ = reader.join();
    }
}

fn wait_for_child_exit(
    child: &mut std::process::Child,
    timeout_seconds: u64,
) -> std::io::Result<(Option<i32>, bool)> {
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_seconds);
    while start.elapsed() < timeout {
        match child.try_wait() {
            Ok(Some(status)) => return Ok((status.code(), false)),
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(err) => {
                terminate_child_process_tree(child);
                return Err(err);
            }
        }
    }
    terminate_child_process_tree(child);
    Ok((None, true))
}

fn terminate_child_process_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        terminate_child_process_group(child.id());
        let _ = child.kill();
    }

    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }

    let _ = child.wait();
}

#[cfg(unix)]
fn terminate_child_process_group(child_pid: u32) {
    let process_group = format!("-{child_pid}");
    log_process_group_kill_failure(
        "TERM",
        &process_group,
        run_process_group_kill("TERM", &process_group),
    );
    thread::sleep(Duration::from_millis(250));
    log_process_group_kill_failure(
        "KILL",
        &process_group,
        run_process_group_kill("KILL", &process_group),
    );
}

#[cfg(unix)]
fn run_process_group_kill(
    signal: &str,
    process_group: &str,
) -> std::io::Result<std::process::ExitStatus> {
    let mut command = Command::new("/bin/kill");
    command.env_clear();
    apply_allowed_command_environment(&mut command);
    let signal_arg = format!("-{signal}");
    command.args([signal_arg.as_str(), process_group]);
    command.stdin(Stdio::null());
    command.stdout(Stdio::null());
    command.stderr(Stdio::null());
    command.status()
}

#[cfg(unix)]
fn log_process_group_kill_failure(
    signal: &str,
    process_group: &str,
    status: std::io::Result<std::process::ExitStatus>,
) {
    match status {
        Ok(status) if status.success() => {}
        Ok(status) => tracing::warn!(
            signal = %signal,
            process_group = %process_group,
            status = %status,
            "kill exited with non-zero status while terminating process group"
        ),
        Err(err) => tracing::warn!(
            signal = %signal,
            process_group = %process_group,
            error = %err,
            "failed to run kill while terminating process group"
        ),
    }
}

struct ProcessOutputCaptureError {
    err: std::io::Error,
    capture: ProcessOutputCapture,
}

struct ProcessOutputCapture {
    stdout_buffer: Arc<Mutex<String>>,
    stderr_buffer: Arc<Mutex<String>>,
    exit_code: Option<i32>,
    timed_out: bool,
}

impl ProcessOutputCapture {
    fn with_status(mut self, exit_code: Option<i32>, timed_out: bool) -> Self {
        self.exit_code = exit_code;
        self.timed_out = timed_out;
        self
    }

    fn stdout_text(&self) -> String {
        self.stdout_buffer
            .lock()
            .map_or_else(|_| String::new(), |text| text.clone())
    }

    fn stderr_text(&self) -> String {
        self.stderr_buffer
            .lock()
            .map_or_else(|_| String::new(), |text| text.clone())
    }
}

fn process_status(timed_out: bool, exit_code: Option<i32>) -> &'static str {
    if timed_out {
        "fatal"
    } else if exit_code == Some(0) {
        "passed"
    } else {
        "failed"
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

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
struct RemediationRetryScope {
    run_id: String,
    repository_owner: String,
    repository_name: String,
    pr_number: u64,
    input_head_sha: String,
    output_head_sha: Option<String>,
    plan_artifact_sequence: u64,
    remediation_attempt_index: u64,
    max_remediation_attempts: u64,
    validation_retry_index: u64,
    max_validation_retries: u64,
    stale_artifact_retry_index: u64,
    max_stale_artifact_retries: u64,
    scope_kind: String,
}

#[derive(Clone, Debug)]
struct StaleScopeClassification {
    expected_scope: RemediationRetryScope,
    observed_scope: Value,
    errors: Vec<String>,
    stale_artifact_retry_index: u64,
    max_stale_artifact_retries: u64,
}

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
    failure_reason: String,
    expected_scope: Value,
    observed_scope: Value,
    stale_scope_errors: Vec<String>,
    stale_artifact_retry_index: u64,
    max_stale_artifact_retries: u64,
    classified_as: Option<String>,
    validation_source_id: String,
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
    stale_scope: Option<StaleScopeClassification>,
}

fn validate_remediation_result(
    context: &StepContext,
    params: &Value,
    clock: &dyn ClockSleeper,
) -> Result<StepOutcome, EngineError> {
    let artifact_root = artifact_root(context, params)?;
    let store =
        PrFollowupArtifactStore::with_filesystem(&artifact_root, &SystemPrFollowupFilesystem)?;
    let binding = binding_for_context(context, params, &store, clock)?;
    let plan = store.read_current_json(&binding, "pr-remediation-plan")?;
    let mut result = read_remediation_result_for_validation(&store, &binding, &plan)?;
    let validation_source_id = remediation_validation_source_id(&result)?;
    let retry_scope = RetryScopeKey::new(&binding, &plan)?;
    let mut retry_state = load_current_state(&store, &binding, &retry_scope)?;
    let advance_validation = retry_state
        .as_ref()
        .is_none_or(|state| state.validation_source_id.as_deref() != Some(&validation_source_id));
    if let Some(retry_state) = &retry_state {
        result = project_engine_retry_state(result, retry_state);
    }
    let step_id = current_step_id(context, "validate_remediation_result");
    let step_order = u64_param(params, "step_order_index", 9);

    let expected_scope = remediation_retry_scope(&binding, &plan, &result, params);
    let validation = evaluate_remediation_result(
        &binding,
        &plan,
        &result,
        &expected_scope,
        retry_state.is_some(),
        advance_validation,
    );
    if let Some(state) = &mut retry_state {
        let stale_index = validation
            .stale_scope
            .as_ref()
            .map_or(state.counters.stale_artifact_retry_index, |scope| {
                scope.stale_artifact_retry_index
            });
        let transition = ValidationTransition {
            source_id: &validation_source_id,
            validation_retry_index: validation.validation_retry_index,
            stale_artifact_retry_index: stale_index,
            transition_type: validation.state.as_str(),
        };
        record_validation(
            &store,
            &binding,
            &step_id,
            step_order,
            state,
            &transition,
            clock,
        )?;
    }
    let payload =
        remediation_result_payload(&binding, &result, &validation, &validation_source_id, clock);
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
    let result_write_record = store.write_json_artifact(
        &binding,
        "pr-remediation-result",
        &step_id,
        step_order,
        &payload,
        failure,
        clock,
    )?;
    if validation.outcome == StepOutcome::Success {
        write_pending_marker_actions_for_fixed_feedback(&FixedFeedbackMarkerContext {
            store: &store,
            binding: &binding,
            step_id: &step_id,
            step_order,
            plan: &plan,
            validation_payload: &payload,
            result_sequence: &result_write_record.sequence,
            clock,
        })?;
    }

    Ok(validation.outcome)
}

fn remediation_validation_source_id(result: &Value) -> Result<String, EngineError> {
    if let Some(source_id) = result
        .get("validation_source_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        return Ok(source_id.to_string());
    }
    let bytes = serde_json::to_vec(result)
        .map_err(|error| pr_remediation_error(format!("serialize validation source: {error}")))?;
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    Ok(format!("fnv64:{hash:016x}"))
}

fn read_remediation_result_for_validation(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    plan: &Value,
) -> Result<Value, EngineError> {
    let result = store.read_current_raw_json(binding, "pr-remediation-result")?;
    Ok(normalize_remediation_result_for_validation(
        binding, plan, result,
    ))
}

fn normalize_remediation_result_for_validation(
    binding: &PrFollowupBinding,
    plan: &Value,
    mut result: Value,
) -> Value {
    let plan_sequence = plan
        .get("artifact_sequence")
        .cloned()
        .unwrap_or(Value::Null);
    let top_level_current = string_field(&result, "run_id", "") == binding.run_id
        && string_field(&result, "repository_owner", "") == binding.repository_owner
        && string_field(&result, "repository_name", "") == binding.repository_name
        && result
            .get("pr_number")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX)
            == binding.pr_number
        && string_field(&result, "input_head_sha", "") == binding.head_sha
        && result
            .get("plan_artifact_sequence")
            .is_none_or(|sequence| sequence.is_null() || sequence == &plan_sequence);
    let Some(object) = result.as_object_mut() else {
        return result;
    };
    if top_level_current
        && object
            .get("plan_artifact_sequence")
            .is_none_or(Value::is_null)
    {
        object.insert("plan_artifact_sequence".to_string(), plan_sequence.clone());
    }
    if top_level_current && !object.get("retry_scope").is_some_and(Value::is_object) {
        object.insert("retry_scope".to_string(), json!({}));
    }
    if top_level_current {
        backfill_current_retry_scope(binding, object, plan_sequence);
    }
    result
}

fn backfill_current_retry_scope(
    binding: &PrFollowupBinding,
    object: &mut serde_json::Map<String, Value>,
    plan_sequence: Value,
) {
    let output_head_sha = object.get("output_head_sha").cloned();
    let counters = retry_scope_counter_values(object);
    let Some(scope) = object.get_mut("retry_scope").and_then(Value::as_object_mut) else {
        return;
    };
    if scope.get("run_id").is_none_or(Value::is_null) {
        scope.insert("run_id".to_string(), Value::String(binding.run_id.clone()));
    }
    if scope.get("repository_owner").is_none_or(Value::is_null) {
        scope.insert(
            "repository_owner".to_string(),
            Value::String(binding.repository_owner.clone()),
        );
    }
    if scope.get("repository_name").is_none_or(Value::is_null) {
        scope.insert(
            "repository_name".to_string(),
            Value::String(binding.repository_name.clone()),
        );
    }
    if scope.get("pr_number").is_none_or(Value::is_null) {
        scope.insert("pr_number".to_string(), json!(binding.pr_number));
    }
    if scope.get("input_head_sha").is_none_or(Value::is_null) {
        scope.insert(
            "input_head_sha".to_string(),
            Value::String(binding.head_sha.clone()),
        );
    }
    if let Some(output_head_sha) = output_head_sha {
        if scope.get("output_head_sha").is_none_or(Value::is_null) {
            scope.insert("output_head_sha".to_string(), output_head_sha);
        }
    }
    if scope
        .get("plan_artifact_sequence")
        .is_none_or(Value::is_null)
    {
        scope.insert("plan_artifact_sequence".to_string(), plan_sequence);
    }
    backfill_retry_scope_counters(scope, &counters);
    if scope.get("scope_kind").is_none_or(Value::is_null) {
        scope.insert(
            "scope_kind".to_string(),
            Value::String("remediation_result_validation".to_string()),
        );
    }
}

fn retry_scope_counter_values(
    object: &serde_json::Map<String, Value>,
) -> Vec<(&'static str, Value)> {
    [
        ("remediation_attempt_index", 0),
        ("max_remediation_attempts", 2),
        ("validation_retry_index", 0),
        ("max_validation_retries", 2),
        ("stale_artifact_retry_index", 0),
        ("max_stale_artifact_retries", 2),
    ]
    .into_iter()
    .map(|(field, default)| {
        (
            field,
            object.get(field).cloned().unwrap_or_else(|| json!(default)),
        )
    })
    .collect()
}

fn backfill_retry_scope_counters(
    scope: &mut serde_json::Map<String, Value>,
    counters: &[(&'static str, Value)],
) {
    for (field, value) in counters {
        if scope.get(*field).is_none_or(Value::is_null) {
            scope.insert((*field).to_string(), value.clone());
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct NormalRetryCounters {
    validation_retry_index: u64,
    max_validation_retries: u64,
    remediation_attempt_index: u64,
    max_remediation_attempts: u64,
}

fn normal_retry_counters_from_result(result: Option<&Value>) -> NormalRetryCounters {
    result.map_or(
        NormalRetryCounters {
            validation_retry_index: 0,
            max_validation_retries: 2,
            remediation_attempt_index: 0,
            max_remediation_attempts: 2,
        },
        |result| NormalRetryCounters {
            validation_retry_index: retry_scope_u64(result, "validation_retry_index")
                .unwrap_or_default(),
            max_validation_retries: retry_scope_u64(result, "max_validation_retries").unwrap_or(2),
            remediation_attempt_index: retry_scope_u64(result, "remediation_attempt_index")
                .unwrap_or_default(),
            max_remediation_attempts: retry_scope_u64(result, "max_remediation_attempts")
                .unwrap_or(2),
        },
    )
}

fn stale_artifact_retry_counters_from_result(result: Option<&Value>) -> (u64, u64) {
    result.map_or((0, 2), |result| {
        (
            retry_scope_u64(result, "stale_artifact_retry_index").unwrap_or_default(),
            retry_scope_u64(result, "max_stale_artifact_retries").unwrap_or(2),
        )
    })
}

fn stale_artifact_retry_counters_for_scope(result: &Value, params: &Value) -> (u64, u64) {
    (
        retry_scope_u64(result, "stale_artifact_retry_index").unwrap_or_default(),
        retry_scope_u64(result, "max_stale_artifact_retries")
            .unwrap_or_else(|| u64_param(params, "max_stale_artifact_retries", 2)),
    )
}

fn retry_scope_u64(result: &Value, field: &str) -> Option<u64> {
    result
        .get("retry_scope")
        .and_then(|scope| scope.get(field))
        .and_then(Value::as_u64)
        .or_else(|| result.get(field).and_then(Value::as_u64))
}

fn remediation_retry_scope(
    binding: &PrFollowupBinding,
    plan: &Value,
    result: &Value,
    params: &Value,
) -> RemediationRetryScope {
    let stale_counters = stale_artifact_retry_counters_for_scope(result, params);
    RemediationRetryScope {
        run_id: binding.run_id.clone(),
        repository_owner: binding.repository_owner.clone(),
        repository_name: binding.repository_name.clone(),
        pr_number: binding.pr_number,
        input_head_sha: binding.head_sha.clone(),
        output_head_sha: result
            .get("output_head_sha")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        plan_artifact_sequence: plan
            .get("artifact_sequence")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        remediation_attempt_index: retry_scope_u64(result, "remediation_attempt_index")
            .unwrap_or_default(),
        max_remediation_attempts: retry_scope_u64(result, "max_remediation_attempts")
            .unwrap_or_else(|| u64_param(params, "max_remediation_attempts", 2)),
        validation_retry_index: retry_scope_u64(result, "validation_retry_index")
            .unwrap_or_default(),
        max_validation_retries: retry_scope_u64(result, "max_validation_retries")
            .unwrap_or_else(|| u64_param(params, "max_validation_retries", 2)),
        stale_artifact_retry_index: stale_counters.0,
        max_stale_artifact_retries: stale_counters.1,
        scope_kind: "remediation_result_validation".to_string(),
    }
}

fn retry_scope_json(scope: &RemediationRetryScope) -> Value {
    json!({
        "run_id": scope.run_id,
        "repository_owner": scope.repository_owner,
        "repository_name": scope.repository_name,
        "pr_number": scope.pr_number,
        "input_head_sha": scope.input_head_sha,
        "output_head_sha": scope.output_head_sha,
        "plan_artifact_sequence": scope.plan_artifact_sequence,
        "remediation_attempt_index": scope.remediation_attempt_index,
        "max_remediation_attempts": scope.max_remediation_attempts,
        "validation_retry_index": scope.validation_retry_index,
        "max_validation_retries": scope.max_validation_retries,
        "stale_artifact_retry_index": scope.stale_artifact_retry_index,
        "max_stale_artifact_retries": scope.max_stale_artifact_retries,
        "scope_kind": scope.scope_kind,
    })
}

fn classify_stale_remediation_scope(
    result: &Value,
    expected: &RemediationRetryScope,
    advance_validation: bool,
) -> Option<StaleScopeClassification> {
    if is_stale_scope_validation_result(result) {
        return None;
    }
    let observed = observed_retry_scope(result);
    let errors = stale_scope_errors(result, &observed, expected);
    (!errors.is_empty()).then(|| StaleScopeClassification {
        expected_scope: expected.clone(),
        observed_scope: observed,
        errors,
        stale_artifact_retry_index: expected.stale_artifact_retry_index
            + u64::from(advance_validation),
        max_stale_artifact_retries: expected.max_stale_artifact_retries,
    })
}

fn is_stale_scope_validation_result(result: &Value) -> bool {
    matches!(
        result.get("validation_state").and_then(Value::as_str),
        Some("stale_artifact" | "stale_artifact_cap_exhausted")
    )
}

fn observed_retry_scope(result: &Value) -> Value {
    result
        .get("retry_scope")
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}))
}

fn stale_scope_errors(
    result: &Value,
    observed: &Value,
    expected: &RemediationRetryScope,
) -> Vec<String> {
    let mut errors = Vec::new();
    compare_required_scope_fields(observed, expected, &mut errors);
    compare_optional_scope_fields(result, observed, expected, &mut errors);
    errors
}

fn compare_required_scope_fields(
    observed: &Value,
    expected: &RemediationRetryScope,
    errors: &mut Vec<String>,
) {
    compare_scope_string(observed, "run_id", &expected.run_id, errors);
    compare_scope_string(
        observed,
        "repository_owner",
        &expected.repository_owner,
        errors,
    );
    compare_scope_string(
        observed,
        "repository_name",
        &expected.repository_name,
        errors,
    );
    compare_scope_u64(observed, "pr_number", expected.pr_number, errors);
    compare_scope_string(observed, "input_head_sha", &expected.input_head_sha, errors);
    compare_scope_u64(
        observed,
        "plan_artifact_sequence",
        expected.plan_artifact_sequence,
        errors,
    );
}

fn compare_optional_scope_fields(
    result: &Value,
    observed: &Value,
    expected: &RemediationRetryScope,
    errors: &mut Vec<String>,
) {
    if let Some(output_head_sha) = expected.output_head_sha.as_deref() {
        compare_scope_string_if_present(
            result,
            observed,
            "output_head_sha",
            output_head_sha,
            errors,
        );
    }
    compare_scope_u64_if_present(
        result,
        observed,
        "remediation_attempt_index",
        expected.remediation_attempt_index,
        errors,
    );
    compare_scope_u64_if_present(
        result,
        observed,
        "max_remediation_attempts",
        expected.max_remediation_attempts,
        errors,
    );
    compare_scope_u64_if_present(
        result,
        observed,
        "validation_retry_index",
        expected.validation_retry_index,
        errors,
    );
    compare_scope_u64_if_present(
        result,
        observed,
        "max_validation_retries",
        expected.max_validation_retries,
        errors,
    );
    compare_scope_string_if_present(result, observed, "scope_kind", &expected.scope_kind, errors);
}

fn compare_scope_string(observed: &Value, field: &str, expected: &str, errors: &mut Vec<String>) {
    let value = observed.get(field).and_then(Value::as_str);
    if value != Some(expected) {
        errors.push(format!(
            "retry_scope.{field} mismatch: expected {expected}, got {}",
            value.unwrap_or("<missing>")
        ));
    }
}

fn compare_scope_string_if_present(
    result: &Value,
    observed: &Value,
    field: &str,
    expected: &str,
    errors: &mut Vec<String>,
) {
    let value = observed
        .get(field)
        .or_else(|| result.get(field))
        .and_then(Value::as_str);
    if value.is_some() && value != Some(expected) {
        errors.push(format!(
            "retry_scope.{field} mismatch: expected {expected}, got {}",
            value.unwrap_or("<missing>")
        ));
    }
}

fn compare_scope_u64(observed: &Value, field: &str, expected: u64, errors: &mut Vec<String>) {
    let value = observed.get(field).and_then(Value::as_u64);
    if value != Some(expected) {
        errors.push(format!(
            "retry_scope.{field} mismatch: expected {expected}, got {}",
            value.map_or_else(|| "<missing>".to_string(), |value| value.to_string())
        ));
    }
}

fn compare_scope_u64_if_present(
    result: &Value,
    observed: &Value,
    field: &str,
    expected: u64,
    errors: &mut Vec<String>,
) {
    let value = observed
        .get(field)
        .or_else(|| result.get(field))
        .and_then(Value::as_u64);
    if value.is_some() && value != Some(expected) {
        errors.push(format!(
            "retry_scope.{field} mismatch: expected {expected}, got {}",
            value.map_or_else(|| "<missing>".to_string(), |value| value.to_string())
        ));
    }
}

fn evaluate_remediation_result(
    binding: &PrFollowupBinding,
    plan: &Value,
    result: &Value,
    expected_scope: &RemediationRetryScope,
    engine_retry_state_present: bool,
    advance_validation: bool,
) -> RemediationResultValidation {
    if let Some(stale_scope) =
        classify_stale_remediation_scope(result, expected_scope, advance_validation)
    {
        return stale_scope_validation(stale_scope, expected_scope);
    }

    let semantic = semantic_remediation_validation(binding, plan, result);
    if !semantic.errors.is_empty() {
        return malformed_remediation_validation(semantic, advance_validation);
    }
    if !semantic.unsuccessful_statuses.is_empty()
        || semantic.successful_count != semantic.result_count
    {
        return unsuccessful_remediation_validation(semantic, engine_retry_state_present);
    }
    successful_remediation_validation(semantic)
}

#[derive(Clone, Debug)]
struct SemanticRemediationValidation {
    errors: Vec<String>,
    unsuccessful_statuses: Vec<String>,
    successful_count: usize,
    result_count: usize,
    no_change_after_remediation: bool,
    validation_retry_index: u64,
    max_validation_retries: u64,
    remediation_attempt_index: u64,
    max_remediation_attempts: u64,
}

fn stale_scope_validation(
    stale_scope: StaleScopeClassification,
    expected_scope: &RemediationRetryScope,
) -> RemediationResultValidation {
    let exhausted =
        stale_scope.stale_artifact_retry_index >= stale_scope.max_stale_artifact_retries;
    RemediationResultValidation {
        outcome: if exhausted {
            StepOutcome::Fatal
        } else {
            StepOutcome::Fixable
        },
        state: if exhausted {
            ValidationState::StaleArtifactCapExhausted
        } else {
            ValidationState::StaleArtifact
        },
        failure_reason: "stale_remediation_result_scope".to_string(),
        errors: stale_scope.errors.clone(),
        unsuccessful_statuses: Vec::new(),
        no_change_after_remediation: false,
        validation_retry_index: expected_scope.validation_retry_index,
        max_validation_retries: expected_scope.max_validation_retries,
        remediation_attempt_index: expected_scope.remediation_attempt_index,
        max_remediation_attempts: expected_scope.max_remediation_attempts,
        stale_scope: Some(stale_scope),
    }
}

fn semantic_remediation_validation(
    binding: &PrFollowupBinding,
    plan: &Value,
    result: &Value,
) -> SemanticRemediationValidation {
    let input_head_sha = string_field(result, "input_head_sha", "");
    let output_head_sha = string_field(result, "output_head_sha", "");
    let results = result
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut semantic = SemanticRemediationValidation {
        errors: top_level_remediation_errors(binding, plan, result, &input_head_sha, &results),
        unsuccessful_statuses: Vec::new(),
        successful_count: 0,
        result_count: results.len(),
        no_change_after_remediation: output_head_sha == input_head_sha,
        validation_retry_index: u64_field(result, "validation_retry_index", 0),
        max_validation_retries: u64_field(result, "max_validation_retries", 2),
        remediation_attempt_index: u64_field(result, "remediation_attempt_index", 0),
        max_remediation_attempts: u64_field(result, "max_remediation_attempts", 2),
    };
    validate_result_items(binding, plan, &output_head_sha, &results, &mut semantic);
    semantic
}

fn u64_field(value: &Value, field: &str, default: u64) -> u64 {
    value.get(field).and_then(Value::as_u64).unwrap_or(default)
}

fn top_level_remediation_errors(
    binding: &PrFollowupBinding,
    plan: &Value,
    result: &Value,
    input_head_sha: &str,
    results: &[Value],
) -> Vec<String> {
    let mut errors = Vec::new();
    const MISSING_IDENTITY_FIELD: &str = "<missing>";
    let observed_run_id = string_field(result, "run_id", MISSING_IDENTITY_FIELD);
    let observed_repository_owner =
        string_field(result, "repository_owner", MISSING_IDENTITY_FIELD);
    let observed_repository_name = string_field(result, "repository_name", MISSING_IDENTITY_FIELD);
    if observed_run_id != binding.run_id {
        errors.push(format!(
            "run_id mismatch: expected {}, got {}",
            binding.run_id, observed_run_id
        ));
    }
    if observed_repository_owner != binding.repository_owner {
        errors.push(format!(
            "repository_owner mismatch: expected {}, got {}",
            binding.repository_owner, observed_repository_owner
        ));
    }
    if observed_repository_name != binding.repository_name {
        errors.push(format!(
            "repository_name mismatch: expected {}, got {}",
            binding.repository_name, observed_repository_name
        ));
    }
    if result.get("pr_number").and_then(Value::as_u64) != Some(binding.pr_number) {
        errors.push(format!(
            "pr_number mismatch: expected {}, got {}",
            binding.pr_number,
            result
                .get("pr_number")
                .map_or_else(|| MISSING_IDENTITY_FIELD.to_string(), Value::to_string)
        ));
    }
    if input_head_sha != binding.head_sha {
        errors.push(format!(
            "input_head_sha mismatch: expected {}, got {}",
            binding.head_sha, input_head_sha
        ));
    }
    if result.get("plan_artifact_sequence") != plan.get("artifact_sequence") {
        errors.push("plan_artifact_sequence mismatch".to_string());
    }
    if results.is_empty() {
        errors.push("missing remediation results".to_string());
    }
    errors
}

fn validate_result_items(
    binding: &PrFollowupBinding,
    plan: &Value,
    output_head_sha: &str,
    results: &[Value],
    semantic: &mut SemanticRemediationValidation,
) {
    let plan_items = plan_items_by_key(plan);
    let mut result_counts: BTreeMap<String, usize> = BTreeMap::new();
    for item in results {
        validate_result_item(
            binding,
            &plan_items,
            item,
            output_head_sha,
            &mut result_counts,
            semantic,
        );
    }
    validate_complete_result_coverage(&plan_items, &result_counts, &mut semantic.errors);
}

fn validate_result_item(
    binding: &PrFollowupBinding,
    plan_items: &BTreeMap<String, Value>,
    item: &Value,
    output_head_sha: &str,
    result_counts: &mut BTreeMap<String, usize>,
    semantic: &mut SemanticRemediationValidation,
) {
    let source_type = string_field(item, "source_type", "");
    let source_id = string_field(item, "source_id", "");
    let status = string_field(item, "status", "");
    let key = format!("{source_type}:{source_id}");
    let plan_item = plan_items.get(&key);
    *result_counts.entry(key.clone()).or_default() += 1;
    validate_item_identity(binding, plan_item, item, &key, &mut semantic.errors);
    if !validate_item_status(&status, &key, semantic) {
        return;
    }
    validate_success_evidence(
        binding,
        plan_item,
        item,
        &status,
        &key,
        output_head_sha,
        &mut semantic.errors,
    );
}

fn validate_item_identity(
    binding: &PrFollowupBinding,
    plan_item: Option<&Value>,
    item: &Value,
    key: &str,
    errors: &mut Vec<String>,
) {
    if plan_item.is_none() {
        errors.push(format!(
            "result item {key} does not match current plan item"
        ));
    }
    if let Some(plan_item) = plan_item {
        validate_result_binding(binding, plan_item, item, key, errors);
    }
    if string_field(item, "response_text", "").trim().is_empty() {
        errors.push(format!("result item {key} missing response_text"));
    }
}

fn validate_item_status(
    status: &str,
    key: &str,
    semantic: &mut SemanticRemediationValidation,
) -> bool {
    if !REMEDIATION_RESULT_VALID_STATUSES.contains(&status) {
        semantic
            .errors
            .push(format!("unknown remediation status for {key}: {status}"));
        return false;
    }
    if REMEDIATION_RESULT_UNSUCCESSFUL_STATUSES.contains(&status) {
        semantic.unsuccessful_statuses.push(status.to_string());
        return false;
    }
    if REMEDIATION_RESULT_SUCCESS_STATUSES.contains(&status) {
        semantic.successful_count += 1;
    }
    true
}

fn validate_success_evidence(
    binding: &PrFollowupBinding,
    plan_item: Option<&Value>,
    item: &Value,
    status: &str,
    key: &str,
    output_head_sha: &str,
    errors: &mut Vec<String>,
) {
    if matches!(status, "already_satisfied" | "not_reproduced") {
        validate_deterministic_evidence(binding, plan_item, item, status, key, errors);
    } else if matches!(status, "fixed" | "changed") {
        validate_fixed_evidence(output_head_sha, item, key, errors);
    }
}

fn malformed_remediation_validation(
    semantic: SemanticRemediationValidation,
    advance_validation: bool,
) -> RemediationResultValidation {
    let validation_retry_index = semantic.validation_retry_index + u64::from(advance_validation);
    let exhausted = validation_retry_index >= semantic.max_validation_retries;
    RemediationResultValidation {
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
        errors: semantic.errors,
        unsuccessful_statuses: semantic.unsuccessful_statuses,
        no_change_after_remediation: semantic.no_change_after_remediation,
        validation_retry_index,
        max_validation_retries: semantic.max_validation_retries,
        remediation_attempt_index: semantic.remediation_attempt_index,
        max_remediation_attempts: semantic.max_remediation_attempts,
        stale_scope: None,
    }
}

fn unsuccessful_remediation_validation(
    mut semantic: SemanticRemediationValidation,
    engine_retry_state_present: bool,
) -> RemediationResultValidation {
    if !engine_retry_state_present {
        semantic.remediation_attempt_index += 1;
    }
    let exhausted = semantic.remediation_attempt_index >= semantic.max_remediation_attempts;
    RemediationResultValidation {
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
        errors: semantic.errors,
        unsuccessful_statuses: semantic.unsuccessful_statuses,
        no_change_after_remediation: semantic.no_change_after_remediation,
        validation_retry_index: semantic.validation_retry_index,
        max_validation_retries: semantic.max_validation_retries,
        remediation_attempt_index: semantic.remediation_attempt_index,
        max_remediation_attempts: semantic.max_remediation_attempts,
        stale_scope: None,
    }
}

fn successful_remediation_validation(
    semantic: SemanticRemediationValidation,
) -> RemediationResultValidation {
    RemediationResultValidation {
        outcome: StepOutcome::Success,
        state: ValidationState::Valid,
        failure_reason: String::new(),
        errors: semantic.errors,
        unsuccessful_statuses: semantic.unsuccessful_statuses,
        no_change_after_remediation: semantic.no_change_after_remediation,
        validation_retry_index: semantic.validation_retry_index,
        max_validation_retries: semantic.max_validation_retries,
        remediation_attempt_index: semantic.remediation_attempt_index,
        max_remediation_attempts: semantic.max_remediation_attempts,
        stale_scope: None,
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

fn remediation_retry_scope_payload(
    result: &Value,
    validation: &RemediationResultValidation,
) -> Value {
    let mut retry_scope = result
        .get("retry_scope")
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    if let Some(stale_scope) = validation.stale_scope.as_ref() {
        if let Some(object) = retry_scope.as_object_mut() {
            object.insert(
                "stale_artifact_retry_index".to_string(),
                json!(stale_scope.stale_artifact_retry_index),
            );
            object.insert(
                "max_stale_artifact_retries".to_string(),
                json!(stale_scope.max_stale_artifact_retries),
            );
        }
    }
    retry_scope
}

fn remediation_result_payload(
    binding: &PrFollowupBinding,
    result: &Value,
    validation: &RemediationResultValidation,
    validation_source_id: &str,
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
        retry_scope: remediation_retry_scope_payload(result, validation),
        plan_artifact_sequence: result
            .get("plan_artifact_sequence")
            .cloned()
            .unwrap_or(Value::Null),
        unsuccessful_statuses: validation.unsuccessful_statuses.clone(),
        no_change_after_remediation: validation.no_change_after_remediation,
        validation_errors: validation.errors.clone(),
        failure_reason: validation.failure_reason.clone(),
        expected_scope: validation
            .stale_scope
            .as_ref()
            .map(|scope| retry_scope_json(&scope.expected_scope))
            .unwrap_or(Value::Null),
        observed_scope: validation
            .stale_scope
            .as_ref()
            .map(|scope| scope.observed_scope.clone())
            .unwrap_or(Value::Null),
        stale_scope_errors: validation
            .stale_scope
            .as_ref()
            .map(|scope| scope.errors.clone())
            .unwrap_or_default(),
        stale_artifact_retry_index: validation.stale_scope.as_ref().map_or_else(
            || retry_scope_u64(result, "stale_artifact_retry_index").unwrap_or_default(),
            |scope| scope.stale_artifact_retry_index,
        ),
        max_stale_artifact_retries: validation.stale_scope.as_ref().map_or_else(
            || retry_scope_u64(result, "max_stale_artifact_retries").unwrap_or(2),
            |scope| scope.max_stale_artifact_retries,
        ),
        classified_as: validation.stale_scope.as_ref().map(|_| {
            if validation.state == ValidationState::StaleArtifactCapExhausted {
                "stale_artifact_cap_exhausted".to_string()
            } else {
                "stale_artifact_retryable".to_string()
            }
        }),
        validation_source_id: validation_source_id.to_string(),
        validated_at: clock.now_rfc3339(),
    }
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
