use super::*;
use crate::engine::executors::github_pr::GithubPrCommandRunner;
use crate::engine::executors::pr_followup_artifacts::{ClockSleeper, PrFollowupArtifactStore};
use crate::engine::executors::pr_followup_types::{
    is_summary_marker_key, value_has_summary_marker_key, PrFollowupBinding,
    NO_REMEDIATION_OUTPUT_HEAD,
};
use crate::engine::runner::EngineError;
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub(super) fn current_evaluation_marker_action(
    binding: &PrFollowupBinding,
    evaluation: &Value,
    feedback_item: Option<&Value>,
    params: &Value,
    clock: &dyn ClockSleeper,
) -> Option<Value> {
    if value_has_summary_marker_key(evaluation) {
        return None;
    }
    let decision = evaluation.get("decision").and_then(Value::as_str)?;
    let action_kind = current_evaluation_action_kind(decision, params)?;
    let context =
        CurrentEvaluationMarkerContext::new(binding, evaluation, feedback_item, action_kind)?;
    Some(current_evaluation_marker_json(context, evaluation, clock))
}

pub(super) fn current_evaluation_action_kind(
    decision: &str,
    params: &Value,
) -> Option<&'static str> {
    match decision {
        "invalid" => Some("comment_invalid"),
        "out_of_scope" => Some("comment_out_of_scope"),
        "needs_user_judgment" => params
            .get("post_needs_user_judgment_comments")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            .then_some("comment_needs_user_judgment")
            .or(Some("skip_needs_user_judgment")),
        _ => None,
    }
}

pub(super) struct CurrentEvaluationMarkerContext<'a> {
    binding: &'a PrFollowupBinding,
    action_kind: &'a str,
    item_id: String,
    stable_marker_key: String,
    body_hash: String,
    source_head_sha: &'a str,
    reason: &'a str,
    response_text: Option<&'a str>,
    thread_id: Option<&'a str>,
    comment_database_id: Option<i64>,
}

impl<'a> CurrentEvaluationMarkerContext<'a> {
    pub fn new(
        binding: &'a PrFollowupBinding,
        evaluation: &'a Value,
        feedback_item: Option<&'a Value>,
        action_kind: &'a str,
    ) -> Option<Self> {
        let decision = evaluation.get("decision").and_then(Value::as_str)?;
        Some(Self {
            binding,
            action_kind,
            item_id: string_field(evaluation, "item_id"),
            stable_marker_key: string_field(evaluation, "stable_marker_key"),
            body_hash: string_field(evaluation, "body_hash"),
            source_head_sha: evaluation
                .get("head_sha")
                .and_then(Value::as_str)
                .unwrap_or(&binding.head_sha),
            reason: evaluation
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or(decision),
            response_text: evaluation
                .get("response_text")
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty()),
            thread_id: feedback_item
                .and_then(|item| item.get("thread_id"))
                .and_then(Value::as_str),
            comment_database_id: feedback_item
                .and_then(|item| item.get("comment_database_id"))
                .and_then(Value::as_i64),
        })
    }

    pub fn idempotency_key(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}:{}:{}:{}",
            self.binding.run_id,
            self.binding.repository_owner,
            self.binding.repository_name,
            self.binding.pr_number,
            self.source_head_sha,
            NO_REMEDIATION_OUTPUT_HEAD,
            self.stable_marker_key,
            self.action_kind
        )
    }
}

pub(super) fn current_evaluation_marker_json(
    context: CurrentEvaluationMarkerContext<'_>,
    evaluation: &Value,
    clock: &dyn ClockSleeper,
) -> Value {
    json!({
        "action_id": format!("{}:{}:{}:{}:{}", context.action_kind, context.stable_marker_key, context.body_hash, context.source_head_sha, NO_REMEDIATION_OUTPUT_HEAD),
        "action_kind": context.action_kind,
        "item_id": context.item_id,
        "original_feedback_identity": {
            "item_id": context.item_id,
            "stable_marker_key": context.stable_marker_key,
            "body_hash": context.body_hash,
            "source_head_sha": context.source_head_sha,
            "thread_id": context.thread_id,
            "comment_database_id": context.comment_database_id
        },
        "stable_marker_key": context.stable_marker_key,
        "source_head_sha": context.source_head_sha,
        "remediation_input_head_sha": context.source_head_sha,
        "remediation_output_head_sha": Value::Null,
        "remediation_output_head": NO_REMEDIATION_OUTPUT_HEAD,
        "body_hash": context.body_hash,
        "idempotency_key": context.idempotency_key(),
        "comment_body_template_id": context.action_kind,
        "comment_body_artifact_path": Value::Null,
        "resolution_required": false,
        "status": "pending",
        "reason": context.reason,
        "response_text": context.response_text,
        "thread_id": context.thread_id,
        "comment_database_id": context.comment_database_id,
        "evidence": evaluation,
        "derived_from_current_artifacts": true,
        "derived_at": clock.now_rfc3339()
    })
}

mod pending_action_parse;
pub(crate) use pending_action_parse::*;

pub(super) fn pending_action_reason(value: &Value) -> String {
    value
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("recorded marker action")
        .to_string()
}

pub(super) fn pending_action_response_text(value: &Value) -> Option<String> {
    value
        .get("response_text")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(ToString::to_string)
}

pub(super) fn pending_action_thread_id(value: &Value) -> Option<String> {
    direct_review_thread_id(value)
        .or_else(|| review_thread_id_from_stable_marker_key(value))
        .or_else(|| review_thread_id_from_graphql_item_id(value))
}

pub(super) fn pending_action_comment_database_id(value: &Value) -> Option<i64> {
    value
        .pointer("/comment_database_id")
        .and_then(Value::as_i64)
        .or_else(|| {
            value
                .pointer("/evidence/comment_database_id")
                .and_then(Value::as_i64)
        })
        .or_else(|| {
            value
                .pointer("/original_feedback_identity/comment_database_id")
                .and_then(Value::as_i64)
        })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-016,REQ-PRFU-026
/// @pseudocode lines 42-44,47-49
pub(super) fn read_local_marker_completions(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    let report = match store.read_current_json(binding, MARKER_ARTIFACT_FAMILY) {
        Ok(report) => report,
        Err(err) => {
            eprintln!("warning: failed to read local marker completions artifact: {err}");
            Value::Null
        }
    };
    for section in ["posted_comments", "resolved_threads", "skipped_actions"] {
        for entry in report
            .get(section)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(key) = entry.get("idempotency_key").and_then(Value::as_str) {
                keys.insert(key.to_string());
            }
        }
    }
    keys
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-016,REQ-PRFU-017
/// @pseudocode lines 42-45
pub(super) fn discover_marker_remote_comments(
    runner: &dyn GithubPrCommandRunner,
    binding: &PrFollowupBinding,
) -> Result<Vec<Value>, EngineError> {
    let mut comments = query_issue_comments(runner, binding)?;
    // Also scan in-thread review (pull) comments so previously posted in-thread
    // reply markers are detected for idempotency on retry/resume.
    match query_pull_review_comments(runner, binding) {
        Ok(review_comments) => comments.extend(review_comments),
        Err(err) => {
            eprintln!("warning: failed to query pull review comments for idempotency: {err}")
        }
    }
    Ok(comments)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-016,REQ-PRFU-017
pub(super) fn query_pull_review_comments(
    runner: &dyn GithubPrCommandRunner,
    binding: &PrFollowupBinding,
) -> Result<Vec<Value>, EngineError> {
    query_paginated_array(
        runner,
        &format!(
            "/repos/{}/{}/pulls/{}/comments?per_page=100&page=",
            binding.repository_owner, binding.repository_name, binding.pr_number
        ),
    )
}

/// Deterministically validate every pending marker action before issuing any
/// GitHub side effect. Returns `Some(violations)` when at least one action is
/// malformed so the caller can stop before mutating GitHub; `None` when all
/// actions are safe to execute.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-026
pub(super) fn validate_marker_actions_before_mutation(
    actions: &[PendingMarkerAction],
) -> Option<Vec<Value>> {
    let mut violations = Vec::new();
    let mut seen_action_ids = BTreeSet::new();
    let mut seen_cycle_identities = BTreeSet::new();
    for action in actions {
        // Policy-disabled judgment actions never reach a GitHub mutation and
        // retain their legacy exemption from mutation-cycle uniqueness checks.
        if action.action_kind == "skip_needs_user_judgment" {
            continue;
        }
        if !seen_action_ids.insert(action.action_id.clone()) {
            violations.push(json!({
                "action_id": action.action_id,
                "item_id": action.item_id,
                "stable_marker_key": action.stable_marker_key,
                "source_head_sha": action.source_head_sha,
                "remediation_output_head": action.remediation_output_head,
                "violation": "duplicate_action_id"
            }));
        }
        // Informational summary/walkthrough markers are skipped at the mutation
        // gate (post nothing, resolve nothing), so they cannot violate any
        // mutation precondition.
        if is_summary_marker_key(&action.stable_marker_key) {
            continue;
        }
        let cycle_identity = marker_action_cycle_identity(action);
        if !seen_cycle_identities.insert(cycle_identity) {
            violations.push(json!({
                "item_id": action.item_id,
                "stable_marker_key": action.stable_marker_key,
                "source_head_sha": action.source_head_sha,
                "remediation_output_head": action.remediation_output_head,
                "violation": "duplicate_result_for_remediation_cycle"
            }));
        }
        let response_text_present = action
            .response_text
            .as_deref()
            .map(str::trim)
            .is_some_and(|text| !text.is_empty());
        if !response_text_present {
            violations.push(json!({
                "item_id": action.item_id,
                "stable_marker_key": action.stable_marker_key,
                "action_kind": action.action_kind,
                "violation": "missing_response_text"
            }));
        }
        if action.resolution_required
            && action.thread_id.is_none()
            && marker_action_claims_review_thread_identity(action)
        {
            violations.push(json!({
                "item_id": action.item_id,
                "stable_marker_key": action.stable_marker_key,
                "action_kind": action.action_kind,
                "violation": "resolution_required_without_thread_id"
            }));
        }
        if marker_action_claims_review_thread_identity(action)
            && action.comment_database_id.is_none()
            && action.thread_id.is_none()
        {
            violations.push(json!({
                "item_id": action.item_id,
                "stable_marker_key": action.stable_marker_key,
                "action_kind": action.action_kind,
                "violation": "review_thread_identity_without_reply_target"
            }));
        }
    }
    if violations.is_empty() {
        None
    } else {
        Some(violations)
    }
}

fn marker_action_cycle_identity(action: &PendingMarkerAction) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}",
        action.item_id,
        action.stable_marker_key,
        action.source_head_sha,
        action.remediation_output_head,
        action.body_hash,
        action.action_kind
    )
}

// Pre-existing marker action workflow; split in a dedicated refactor stage.
pub(super) fn process_marker_action(
    processor: &MarkerActionProcessor<'_>,
    action: PendingMarkerAction,
) -> Result<MarkerActionOutcome, EngineError> {
    let comment_key = marker_action_key(processor.binding, &action, "comment");
    let resolution_key = marker_action_key(processor.binding, &action, "resolution");
    if let Some(outcome) = marker_action_early_outcome(
        processor,
        action.clone(),
        comment_key.clone(),
        resolution_key.clone(),
    )? {
        return Ok(outcome);
    }

    let mut state = MarkerActionMutationState::default();
    handle_marker_comment(processor, &action, &comment_key, &mut state)?;
    handle_marker_resolution(processor, &action, &resolution_key, &mut state);
    apply_needs_user_judgment_partial(&action, &comment_key, &mut state);
    apply_unhandled_marker_failure(&action, &comment_key, &mut state);
    Ok(build_marker_action_outcome(
        action,
        comment_key,
        resolution_key,
        state,
        processor.clock,
    ))
}

pub(super) fn marker_action_early_outcome(
    processor: &MarkerActionProcessor<'_>,
    action: PendingMarkerAction,
    comment_key: String,
    resolution_key: String,
) -> Result<Option<MarkerActionOutcome>, EngineError> {
    if is_summary_marker_key(&action.stable_marker_key) {
        return Ok(Some(skipped_summary_marker_outcome(
            action,
            comment_key,
            resolution_key,
            processor.clock,
        )));
    }
    if action.action_kind == "skip_needs_user_judgment" {
        return Ok(Some(skipped_needs_user_judgment_outcome(
            action,
            comment_key,
            resolution_key,
            processor.clock,
        )));
    }
    Ok(None)
}

pub(super) fn handle_marker_comment(
    processor: &MarkerActionProcessor<'_>,
    action: &PendingMarkerAction,
    comment_key: &str,
    state: &mut MarkerActionMutationState,
) -> Result<(), EngineError> {
    if marker_comment_already_done(processor, action, comment_key) {
        state
            .skipped
            .push(marker_comment_skip_record(processor, action, comment_key));
        return Ok(());
    }
    let body = render_marker_comment_body(processor.binding, action);
    let body_path = write_marker_comment_body_file(
        processor.store,
        processor.binding,
        processor.step_id,
        processor.step_order,
        action,
        &body,
        processor.clock,
    )?;
    state.posted_comment = Some(post_marker_reply(
        processor.binding,
        processor.runner,
        action,
        comment_key,
        &body,
        &body_path,
    )?);
    Ok(())
}

pub(super) fn marker_comment_already_done(
    processor: &MarkerActionProcessor<'_>,
    action: &PendingMarkerAction,
    comment_key: &str,
) -> bool {
    processor.local_completed.contains(comment_key)
        || processor.remote_completed.contains(comment_key)
        || action.status == "completed"
}

pub(super) fn marker_comment_skip_record(
    processor: &MarkerActionProcessor<'_>,
    action: &PendingMarkerAction,
    comment_key: &str,
) -> Value {
    json!({
        "idempotency_key": comment_key,
        "action_id": marker_action_id(action),
        "reason": if processor.remote_completed.contains(comment_key) { "already_completed_remote" } else { "already_completed_local" },
        "action_kind": action.action_kind
    })
}

pub(super) fn handle_marker_resolution(
    processor: &MarkerActionProcessor<'_>,
    action: &PendingMarkerAction,
    resolution_key: &str,
    state: &mut MarkerActionMutationState,
) {
    match marker_resolution_plan(processor, action, resolution_key) {
        MarkerResolutionPlan::Skip(reason) => state.skipped.push(marker_resolution_skip_record(
            action,
            resolution_key,
            reason,
        )),
        MarkerResolutionPlan::PartialUnavailable => set_resolution_unavailable_partial(
            action,
            resolution_key,
            &mut state.partial,
            &mut state.retryable,
        ),
        MarkerResolutionPlan::Resolve(thread_id) => {
            apply_marker_resolution(processor, action, resolution_key, thread_id, state);
        }
    }
}

pub(super) enum MarkerResolutionPlan {
    Skip(&'static str),
    PartialUnavailable,
    Resolve(String),
}

pub(super) fn marker_resolution_plan(
    processor: &MarkerActionProcessor<'_>,
    action: &PendingMarkerAction,
    resolution_key: &str,
) -> MarkerResolutionPlan {
    let resolution_policy = resolution_policy(action, processor.params);
    if resolution_policy == "skip" {
        return MarkerResolutionPlan::Skip("resolution_skipped_by_policy");
    }
    let Some(thread_id) = action.thread_id.clone() else {
        return if resolution_policy == "required" {
            MarkerResolutionPlan::PartialUnavailable
        } else {
            MarkerResolutionPlan::Skip("handled_comment_only")
        };
    };
    if processor.local_completed.contains(resolution_key)
        || processor.remote_completed.contains(resolution_key)
    {
        MarkerResolutionPlan::Skip("resolution_already_completed")
    } else {
        MarkerResolutionPlan::Resolve(thread_id)
    }
}

pub(super) fn marker_resolution_skip_record(
    action: &PendingMarkerAction,
    resolution_key: &str,
    reason: &str,
) -> Value {
    json!({
        "idempotency_key": resolution_key,
        "action_id": marker_action_id(action),
        "reason": reason,
        "action_kind": "resolve_thread"
    })
}

pub(super) fn set_resolution_unavailable_partial(
    action: &PendingMarkerAction,
    resolution_key: &str,
    partial: &mut Option<Value>,
    retryable: &mut Option<Value>,
) {
    *partial = Some(json!({
        "idempotency_key": resolution_key,
        "action_id": marker_action_id(action),
        "reason": "resolution_unavailable",
        "partial_state": "comment_posted_resolution_pending"
    }));
    *retryable = partial.clone();
}

pub(super) fn apply_marker_resolution(
    processor: &MarkerActionProcessor<'_>,
    action: &PendingMarkerAction,
    resolution_key: &str,
    thread_id: String,
    state: &mut MarkerActionMutationState,
) {
    state.resolve_attempted = true;
    let response = processor.runner.run_github_command(&[
        "gh".to_string(),
        "api".to_string(),
        "graphql".to_string(),
        "-f".to_string(),
        format!("query={RESOLVE_REVIEW_THREAD_MUTATION}"),
        "-f".to_string(),
        format!("threadId={thread_id}"),
    ]);
    match response {
        Ok(output) => {
            apply_marker_resolution_response(action, resolution_key, thread_id, output, state)
        }
        Err(err) => set_resolution_failed_partial(
            action,
            resolution_key,
            "resolution_transport_error",
            err.to_string(),
            None,
            state,
        ),
    }
}

pub(super) fn apply_marker_resolution_response(
    action: &PendingMarkerAction,
    resolution_key: &str,
    thread_id: String,
    output: String,
    state: &mut MarkerActionMutationState,
) {
    let parsed: Value = serde_json::from_str(&output)
        .unwrap_or_else(|err| json!({ "raw_response": output, "parse_error": err.to_string() }));
    state.final_thread_resolved_state = parsed
        .pointer("/data/resolveReviewThread/thread/isResolved")
        .and_then(Value::as_bool);
    state.resolve_succeeded =
        parsed.get("errors").is_none() && state.final_thread_resolved_state == Some(true);
    let resolution_record = json!({
        "idempotency_key": resolution_key,
        "thread_id": thread_id,
        "response": parsed,
        "final_thread_resolved_state": state.final_thread_resolved_state,
        "action_id": marker_action_id(action)
    });
    if state.resolve_succeeded {
        state.resolved_thread = Some(resolution_record);
    } else {
        set_resolution_failed_partial(
            action,
            resolution_key,
            "resolution_failed_after_comment",
            "resolution_failed_after_comment".to_string(),
            Some(resolution_record),
            state,
        );
    }
}

pub(super) fn set_resolution_failed_partial(
    action: &PendingMarkerAction,
    resolution_key: &str,
    reason: &str,
    error: String,
    resolve_response: Option<Value>,
    state: &mut MarkerActionMutationState,
) {
    state.resolve_error = Some(error.clone());
    let mut record = json!({
        "idempotency_key": resolution_key,
        "action_id": marker_action_id(action),
        "reason": reason,
        "error": error,
        "partial_state": "comment_posted_resolution_pending"
    });
    if let Some(response) = resolve_response {
        record["resolve_response"] = response;
    }
    state.partial = Some(record);
    state.retryable = state.partial.clone();
}

pub(super) fn apply_needs_user_judgment_partial(
    action: &PendingMarkerAction,
    comment_key: &str,
    state: &mut MarkerActionMutationState,
) {
    if action.action_kind == "comment_needs_user_judgment" && state.partial.is_none() {
        let record = json!({
            "idempotency_key": comment_key,
            "action_id": marker_action_id(action),
            "reason": "unhandled_needs_user_judgment",
            "partial_state": "unhandled_needs_user_judgment"
        });
        state.partial = Some(record.clone());
        state.retryable = Some(record);
    }
}

pub(super) fn apply_unhandled_marker_failure(
    action: &PendingMarkerAction,
    comment_key: &str,
    state: &mut MarkerActionMutationState,
) {
    if state.failed.is_none()
        && state.partial.is_none()
        && state.posted_comment.is_none()
        && state.skipped.is_empty()
        && state.resolved_thread.is_none()
    {
        state.failed = Some(json!({
            "idempotency_key": comment_key,
            "action_id": marker_action_id(action),
            "reason": "marker_action_not_handled"
        }));
    }
}

pub(super) fn build_marker_action_outcome(
    action: PendingMarkerAction,
    comment_key: String,
    resolution_key: String,
    state: MarkerActionMutationState,
    clock: &dyn ClockSleeper,
) -> MarkerActionOutcome {
    let status = if state.failed.is_some() || state.partial.is_some() {
        "failed"
    } else {
        "completed"
    };
    let updated_action =
        marker_updated_action(&action, status, &comment_key, &resolution_key, clock);
    let audit = marker_action_audit(
        &action,
        status,
        &comment_key,
        state.posted_comment.as_ref(),
        &ResolveAudit {
            resolve_attempted: state.resolve_attempted,
            resolve_succeeded: state.resolve_succeeded,
            resolve_error: state.resolve_error.as_deref(),
            final_thread_resolved_state: state.final_thread_resolved_state,
        },
    );
    MarkerActionOutcome {
        action,
        status: status.to_string(),
        comment_key,
        resolution_key,
        posted_comment: state.posted_comment,
        resolved_thread: state.resolved_thread,
        skipped: state.skipped,
        partial: state.partial,
        retryable: state.retryable,
        failed: state.failed,
        audit,
        updated_action,
    }
}

pub(super) fn marker_updated_action(
    action: &PendingMarkerAction,
    status: &str,
    comment_key: &str,
    resolution_key: &str,
    clock: &dyn ClockSleeper,
) -> Value {
    let mut updated_action = action.value.clone();
    if let Some(object) = updated_action.as_object_mut() {
        object.insert("status".to_string(), json!(status));
        object.insert("comment_idempotency_key".to_string(), json!(comment_key));
        object.insert(
            "resolution_idempotency_key".to_string(),
            json!(resolution_key),
        );
        object.insert("updated_at".to_string(), json!(clock.now_rfc3339()));
    }
    updated_action
}

pub(super) fn marker_action_id(action: &PendingMarkerAction) -> Value {
    action
        .value
        .get("action_id")
        .cloned()
        .unwrap_or(Value::Null)
}

pub(super) fn marker_action_id_for_display(action: &PendingMarkerAction) -> &str {
    action
        .value
        .get("action_id")
        .and_then(Value::as_str)
        .unwrap_or(&action.stable_marker_key)
}
