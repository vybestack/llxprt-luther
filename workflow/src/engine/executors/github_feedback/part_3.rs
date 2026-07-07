fn current_evaluation_marker_action(
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

fn current_evaluation_action_kind(decision: &str, params: &Value) -> Option<&'static str> {
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

struct CurrentEvaluationMarkerContext<'a> {
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
    fn new(
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

    fn idempotency_key(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}:{}:{}:{}",
            self.binding.run_id,
            self.binding.repository_owner,
            self.binding.repository_name,
            self.binding.pr_number,
            self.source_head_sha,
            "none",
            self.stable_marker_key,
            self.action_kind
        )
    }
}

fn current_evaluation_marker_json(
    context: CurrentEvaluationMarkerContext<'_>,
    evaluation: &Value,
    clock: &dyn ClockSleeper,
) -> Value {
    json!({
        "action_id": format!("{}:{}:{}:none", context.action_kind, context.stable_marker_key, context.body_hash),
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
        "remediation_output_head": "none",
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

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-026
/// @pseudocode lines 41-49
fn pending_marker_action_from_value(value: Value) -> Result<PendingMarkerAction, EngineError> {
    let action_kind = string_field(&value, "action_kind");
    let resolution_required = pending_action_resolution_required(&action_kind, &value);
    Ok(PendingMarkerAction {
        action_kind,
        item_id: string_field(&value, "item_id"),
        stable_marker_key: string_field(&value, "stable_marker_key"),
        source_head_sha: string_field(&value, "source_head_sha"),
        remediation_output_head: pending_action_remediation_output_head(&value),
        body_hash: string_field(&value, "body_hash"),
        reason: pending_action_reason(&value),
        response_text: pending_action_response_text(&value),
        thread_id: pending_action_thread_id(&value),
        comment_database_id: pending_action_comment_database_id(&value),
        status: pending_action_status(&value),
        resolution_required,
        value,
    })
}

fn pending_action_remediation_output_head(value: &Value) -> String {
    value
        .get("remediation_output_head")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            value
                .get("remediation_output_head_sha")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| "none".to_string())
}

fn pending_action_reason(value: &Value) -> String {
    value
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("recorded marker action")
        .to_string()
}

fn pending_action_response_text(value: &Value) -> Option<String> {
    value
        .get("response_text")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(ToString::to_string)
}

fn pending_action_thread_id(value: &Value) -> Option<String> {
    direct_review_thread_id(value)
        .or_else(|| review_thread_id_from_stable_marker_key(value))
        .or_else(|| review_thread_id_from_graphql_item_id(value))
}

fn pending_action_comment_database_id(value: &Value) -> Option<i64> {
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

fn pending_action_status(value: &Value) -> String {
    value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("pending")
        .to_string()
}

fn pending_action_resolution_required(action_kind: &str, value: &Value) -> bool {
    value
        .get("resolution_required")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| unified_status_requires_resolution(action_kind, value))
}

fn string_at_paths(value: &Value, paths: &[&str]) -> Option<String> {
    paths.iter().find_map(|path| {
        value
            .pointer(path)
            .and_then(Value::as_str)
            .map(ToString::to_string)
    })
}

/// Derive whether Luther must resolve the review thread from the unified
/// per-item status implied by the marker action and its remediation result.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-026
fn unified_status_requires_resolution(action_kind: &str, value: &Value) -> bool {
    let remediation_status = value
        .get("remediation_result_status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    matches!(action_kind, "comment_fixed")
        && matches!(
            remediation_status,
            "fixed" | "changed" | "already_satisfied" | "not_reproduced"
        )
}

/// Post the agent-authored reply on the original review thread when thread
/// identity is available. Older REST-only actions fall back to the timeline only
/// when no review-thread identity exists.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016
fn post_marker_reply(
    binding: &PrFollowupBinding,
    runner: &dyn GithubPrCommandRunner,
    action: &PendingMarkerAction,
    comment_key: &str,
    body: &str,
    body_path: &Path,
) -> Result<Value, EngineError> {
    if let Some(comment_database_id) = action.comment_database_id {
        return post_marker_reply_via_rest_review_comment(
            binding,
            runner,
            action,
            comment_key,
            body,
            body_path,
            comment_database_id,
        );
    }
    if let Some(thread_id) = action.thread_id.as_deref() {
        return post_marker_reply_via_graphql_thread(
            runner,
            action,
            comment_key,
            body,
            body_path,
            thread_id,
        );
    }
    if marker_action_claims_review_thread_identity(action) {
        return Err(github_feedback_error(format!(
            "review-thread marker action {} has no usable comment_database_id or thread_id",
            marker_action_id_for_display(action)
        )));
    }
    post_marker_reply_via_issue_comment(binding, runner, action, comment_key, body, body_path)
}

fn marker_action_claims_review_thread_identity(action: &PendingMarkerAction) -> bool {
    raw_thread_id_declares_review_thread_identity(&action.value)
        || raw_stable_marker_declares_review_thread_identity(&action.stable_marker_key)
        || raw_graphql_item_declares_review_thread_identity(&action.item_id)
        || action
            .value
            .pointer("/source_id")
            .and_then(Value::as_str)
            .is_some_and(raw_graphql_item_declares_review_thread_identity)
        || action
            .value
            .pointer("/original_feedback_identity/item_id")
            .and_then(Value::as_str)
            .is_some_and(raw_graphql_item_declares_review_thread_identity)
}

fn raw_thread_id_declares_review_thread_identity(value: &Value) -> bool {
    string_at_paths(
        value,
        &[
            "/thread_id",
            "/evidence/thread_id",
            "/original_feedback_identity/thread_id",
        ],
    )
    .is_some_and(|thread_id| is_review_thread_node_id(thread_id.trim()))
}

fn raw_stable_marker_declares_review_thread_identity(stable_marker_key: &str) -> bool {
    stable_marker_key
        .strip_prefix(STABLE_MARKER_THREAD_PREFIX)
        .and_then(|suffix| suffix.split(':').next())
        .is_some_and(is_review_thread_node_id)
}

fn raw_graphql_item_declares_review_thread_identity(item_id: &str) -> bool {
    item_id
        .strip_prefix(GRAPHQL_NODE_ID_PREFIX)
        .and_then(|suffix| suffix.split(':').next())
        .is_some_and(is_review_thread_node_id)
}

fn post_marker_reply_via_rest_review_comment(
    binding: &PrFollowupBinding,
    runner: &dyn GithubPrCommandRunner,
    action: &PendingMarkerAction,
    comment_key: &str,
    body: &str,
    body_path: &Path,
    comment_database_id: i64,
) -> Result<Value, EngineError> {
    let endpoint = format!(
        "/repos/{}/{}/pulls/{}/comments/{}/replies",
        binding.repository_owner, binding.repository_name, binding.pr_number, comment_database_id
    );
    let parsed = post_marker_reply_rest(runner, endpoint, body_path)?;
    Ok(marker_reply_record(MarkerReplyRecordInput {
        action,
        comment_key,
        body,
        body_path,
        comment_id: parsed.get("id").cloned().unwrap_or(Value::Null),
        comment_url: parsed.get("html_url").cloned().unwrap_or(Value::Null),
        in_thread_reply: true,
        in_reply_to_id: parsed.get("in_reply_to_id").cloned().unwrap_or(Value::Null),
        warnings: json!([]),
    }))
}

fn post_marker_reply_via_issue_comment(
    binding: &PrFollowupBinding,
    runner: &dyn GithubPrCommandRunner,
    action: &PendingMarkerAction,
    comment_key: &str,
    body: &str,
    body_path: &Path,
) -> Result<Value, EngineError> {
    let endpoint = format!(
        "/repos/{}/{}/issues/{}/comments",
        binding.repository_owner, binding.repository_name, binding.pr_number
    );
    let parsed = post_marker_reply_rest(runner, endpoint, body_path)?;
    Ok(marker_reply_record(MarkerReplyRecordInput {
        action,
        comment_key,
        body,
        body_path,
        comment_id: parsed.get("id").cloned().unwrap_or(Value::Null),
        comment_url: parsed.get("html_url").cloned().unwrap_or(Value::Null),
        in_thread_reply: false,
        in_reply_to_id: Value::Null,
        warnings: json!(["no_review_thread_identity_posted_top_level_comment"]),
    }))
}

fn post_marker_reply_rest(
    runner: &dyn GithubPrCommandRunner,
    endpoint: String,
    body_path: &Path,
) -> Result<Value, EngineError> {
    let response = runner.run_github_command(&[
        "gh".to_string(),
        "api".to_string(),
        endpoint,
        "--method".to_string(),
        "POST".to_string(),
        "--field".to_string(),
        format!("body=@{}", body_path.display()),
    ])?;
    Ok(serde_json::from_str(&response).unwrap_or_else(|err| {
        json!({ "raw_response": response, "parse_error": err.to_string() })
    }))
}

fn post_marker_reply_via_graphql_thread(
    runner: &dyn GithubPrCommandRunner,
    action: &PendingMarkerAction,
    comment_key: &str,
    body: &str,
    body_path: &Path,
    thread_id: &str,
) -> Result<Value, EngineError> {
    let response = runner.run_github_command(&[
        "gh".to_string(),
        "api".to_string(),
        "graphql".to_string(),
        "-f".to_string(),
        format!("query={ADD_REVIEW_THREAD_REPLY_MUTATION}"),
        "-f".to_string(),
        format!("threadId={thread_id}"),
        "--field".to_string(),
        format!("body=@{}", body_path.display()),
    ])?;
    let parsed: Value = serde_json::from_str(&response).unwrap_or_else(|err| {
        json!({ "raw_response": response, "parse_error": err.to_string() })
    });
    let comment = parsed.pointer("/data/addPullRequestReviewThreadReply/comment");
    if comment.is_none() {
        return Err(github_feedback_error(format!(
            "GraphQL addPullRequestReviewThreadReply failed for thread {thread_id}; mutation may have partially succeeded, inspect response before retrying; {}",
            graphql_error_summary(&parsed)
        )));
    }
    let graphql_errors_present = parsed
        .get("errors")
        .and_then(Value::as_array)
        .is_some_and(|errors| !errors.is_empty());
    let comment_id = comment.and_then(|value| value.get("databaseId")).cloned();
    let comment_url = comment.and_then(|value| value.get("url")).cloned();
    let mut warnings = vec!["posted_review_thread_reply_via_graphql"];
    if graphql_errors_present {
        warnings.push("graphql_errors_present_with_posted_thread_reply");
    }
    if comment_id.is_none() {
        warnings.push("missing_database_id_in_graphql_thread_reply_response");
    }
    if comment_url.is_none() {
        warnings.push("missing_url_in_graphql_thread_reply_response");
    }
    Ok(marker_reply_record(MarkerReplyRecordInput {
        action,
        comment_key,
        body,
        body_path,
        comment_id: comment_id.unwrap_or(Value::Null),
        comment_url: comment_url.unwrap_or(Value::Null),
        in_thread_reply: true,
        in_reply_to_id: Value::Null,
        warnings: json!(warnings),
    }))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-016,REQ-PRFU-026
/// @pseudocode lines 42-44,47-49
fn read_local_marker_completions(
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
fn discover_marker_remote_comments(
    runner: &dyn GithubPrCommandRunner,
    binding: &PrFollowupBinding,
) -> Result<Vec<Value>, EngineError> {
    let mut comments = query_issue_comments(runner, binding)?;
    // Also scan in-thread review (pull) comments so previously posted in-thread
    // reply markers are detected for idempotency on retry/resume.
    match query_pull_review_comments(runner, binding) {
        Ok(review_comments) => comments.extend(review_comments),
        Err(err) => eprintln!(
            "warning: failed to query pull review comments for idempotency: {err}"
        ),
    }
    Ok(comments)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-016,REQ-PRFU-017
fn query_pull_review_comments(
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
fn validate_marker_actions_before_mutation(actions: &[PendingMarkerAction]) -> Option<Vec<Value>> {
    let mut violations = Vec::new();
    let mut seen_item_ids = BTreeSet::new();
    for action in actions {
        if action.action_kind == "skip_needs_user_judgment" {
            continue;
        }
        // Informational summary/walkthrough markers are skipped at the mutation
        // gate (post nothing, resolve nothing), so they cannot violate any
        // mutation precondition.
        if is_summary_marker_key(&action.stable_marker_key) {
            continue;
        }
        if !action.item_id.is_empty() && !seen_item_ids.insert(action.item_id.clone()) {
            violations.push(json!({
                "item_id": action.item_id,
                "stable_marker_key": action.stable_marker_key,
                "violation": "duplicate_result_for_item"
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
    }
    if violations.is_empty() {
        None
    } else {
        Some(violations)
    }
}

// Pre-existing marker action workflow; split in a dedicated refactor stage.
fn process_marker_action(
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

fn marker_action_early_outcome(
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
    validate_marker_action_evidence(
        processor.binding,
        processor.store,
        action,
        comment_key,
        resolution_key,
        processor.clock,
    )
}

fn handle_marker_comment(
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

fn marker_comment_already_done(
    processor: &MarkerActionProcessor<'_>,
    action: &PendingMarkerAction,
    comment_key: &str,
) -> bool {
    processor.local_completed.contains(comment_key)
        || processor.remote_completed.contains(comment_key)
        || action.status == "completed"
}

fn marker_comment_skip_record(
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

fn handle_marker_resolution(
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

enum MarkerResolutionPlan {
    Skip(&'static str),
    PartialUnavailable,
    Resolve(String),
}

fn marker_resolution_plan(
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

fn marker_resolution_skip_record(
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

fn set_resolution_unavailable_partial(
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

fn apply_marker_resolution(
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

fn apply_marker_resolution_response(
    action: &PendingMarkerAction,
    resolution_key: &str,
    thread_id: String,
    output: String,
    state: &mut MarkerActionMutationState,
) {
    let parsed: Value = serde_json::from_str(&output).unwrap_or_else(|err| {
        json!({ "raw_response": output, "parse_error": err.to_string() })
    });
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

fn set_resolution_failed_partial(
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

fn apply_needs_user_judgment_partial(
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

fn apply_unhandled_marker_failure(
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

fn build_marker_action_outcome(
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

fn marker_updated_action(
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

fn marker_action_id(action: &PendingMarkerAction) -> Value {
    action
        .value
        .get("action_id")
        .cloned()
        .unwrap_or(Value::Null)
}

fn marker_action_id_for_display(action: &PendingMarkerAction) -> &str {
    action
        .value
        .get("action_id")
        .and_then(Value::as_str)
        .unwrap_or(&action.stable_marker_key)
}

