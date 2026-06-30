/// Build the per-item idempotent audit record linking the feedback item,
/// review thread, reply, and resolve result.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-016,REQ-PRFU-026
/// Resolve-side audit fields grouped to keep the audit builder within argument
/// limits.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-016,REQ-PRFU-026
struct ResolveAudit<'a> {
    resolve_attempted: bool,
    resolve_succeeded: bool,
    resolve_error: Option<&'a str>,
    final_thread_resolved_state: Option<bool>,
}

fn marker_action_audit(
    action: &PendingMarkerAction,
    status: &str,
    comment_key: &str,
    posted_comment: Option<&Value>,
    resolve: &ResolveAudit,
) -> Value {
    json!({
        "item_id": action.item_id,
        "stable_marker_key": action.stable_marker_key,
        "review_thread_id": action.thread_id,
        "comment_database_id": action.comment_database_id,
        "action_kind": action.action_kind,
        "status": status,
        "idempotency_key": comment_key,
        "reply_comment_id": posted_comment
            .and_then(|comment| comment.get("comment_id").cloned())
            .unwrap_or(Value::Null),
        "reply_comment_url": posted_comment
            .and_then(|comment| comment.get("comment_url").cloned())
            .unwrap_or(Value::Null),
        "in_thread_reply": posted_comment
            .and_then(|comment| comment.get("in_thread_reply").and_then(Value::as_bool))
            .unwrap_or(false),
        "resolve_attempted": resolve.resolve_attempted,
        "resolve_succeeded": resolve.resolve_succeeded,
        "resolve_error": resolve.resolve_error,
        "final_thread_resolved_state": resolve.final_thread_resolved_state
    })
}

/// Fallback visible reply body used only when the agent supplied no
/// `response_text` for the item.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017
fn render_default_marker_visible(action: &PendingMarkerAction) -> String {
    match action.action_kind.as_str() {
        "comment_fixed" => format!(
            "Luther follow-up: fixed CodeRabbit feedback `{}`. Evidence: {}.",
            action.stable_marker_key,
            sanitize_visible_text(&action.reason)
        ),
        "comment_out_of_scope" => format!(
            "Luther follow-up: CodeRabbit feedback `{}` is out of scope. Reason: {}.",
            action.stable_marker_key,
            sanitize_visible_text(&action.reason)
        ),
        "comment_needs_user_judgment" => format!(
            "Luther follow-up: CodeRabbit feedback `{}` needs user judgment. Reason: {}.",
            action.stable_marker_key,
            sanitize_visible_text(&action.reason)
        ),
        _ => format!(
            "Luther follow-up: CodeRabbit feedback `{}` is not valid. Reason: {}.",
            action.stable_marker_key,
            sanitize_visible_text(&action.reason)
        ),
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017
/// @pseudocode lines 43-45
fn render_marker_comment_body(binding: &PrFollowupBinding, action: &PendingMarkerAction) -> String {
    let visible = action
        .response_text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(sanitize_visible_text)
        .unwrap_or_else(|| render_default_marker_visible(action));
    let marker = format!(
        "<!-- {MARKER_NAMESPACE} marker_key={} source_head={} remediation_output_head={} body={} action={} run_id={} -->",
        action.stable_marker_key,
        action.source_head_sha,
        action.remediation_output_head,
        action.body_hash,
        action.action_kind.as_str(),
        binding.run_id
    );
    format!("{visible}\n\n{marker}\n")
}

/// Final-safety-net outcome for an informational CodeRabbit summary/walkthrough
/// marker that reached the mutation gate. Posts nothing and resolves nothing,
/// recording a clean `skipped` entry so the marker report stays complete and no
/// top-level PR comment is ever created (even on reruns).
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-020
fn skipped_summary_marker_outcome(
    action: PendingMarkerAction,
    comment_key: String,
    resolution_key: String,
    clock: &dyn ClockSleeper,
) -> MarkerActionOutcome {
    let skipped = vec![
        json!({
            "idempotency_key": comment_key,
            "action_id": action.value.get("action_id").cloned().unwrap_or(Value::Null),
            "reason": "coderabbit_summary_informational_only",
            "action_kind": action.action_kind
        }),
        json!({
            "idempotency_key": resolution_key,
            "action_id": action.value.get("action_id").cloned().unwrap_or(Value::Null),
            "reason": "coderabbit_summary_informational_only",
            "action_kind": "resolve_thread"
        }),
    ];
    let mut updated_action = action.value.clone();
    if let Some(object) = updated_action.as_object_mut() {
        object.insert("status".to_string(), json!("skipped"));
        object.insert("comment_idempotency_key".to_string(), json!(comment_key));
        object.insert(
            "resolution_idempotency_key".to_string(),
            json!(resolution_key),
        );
        object.insert("updated_at".to_string(), json!(clock.now_rfc3339()));
        object.insert(
            "skipped_reason".to_string(),
            json!("coderabbit_summary_informational_only"),
        );
    }
    let audit = marker_action_audit(
        &action,
        "skipped",
        &comment_key,
        None,
        &ResolveAudit {
            resolve_attempted: false,
            resolve_succeeded: false,
            resolve_error: None,
            final_thread_resolved_state: None,
        },
    );
    MarkerActionOutcome {
        action,
        status: "skipped".to_string(),
        comment_key,
        resolution_key,
        posted_comment: None,
        resolved_thread: None,
        skipped,
        partial: None,
        retryable: None,
        failed: None,
        audit,
        updated_action,
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-017
/// @pseudocode lines 45
fn skipped_needs_user_judgment_outcome(
    action: PendingMarkerAction,
    comment_key: String,
    resolution_key: String,
    clock: &dyn ClockSleeper,
) -> MarkerActionOutcome {
    let skipped = vec![
        json!({
            "idempotency_key": comment_key,
            "action_id": action.value.get("action_id").cloned().unwrap_or(Value::Null),
            "reason": "needs_user_judgment_comments_disabled",
            "action_kind": "comment_needs_user_judgment"
        }),
        json!({
            "idempotency_key": resolution_key,
            "action_id": action.value.get("action_id").cloned().unwrap_or(Value::Null),
            "reason": "resolution_skipped_needs_user_judgment",
            "action_kind": "resolve_thread"
        }),
    ];
    let partial = Some(json!({
        "idempotency_key": comment_key,
        "action_id": action.value.get("action_id").cloned().unwrap_or(Value::Null),
        "reason": "unhandled_needs_user_judgment",
        "partial_state": "unhandled_needs_user_judgment"
    }));
    let mut updated_action = action.value.clone();
    if let Some(object) = updated_action.as_object_mut() {
        object.insert("status".to_string(), json!("failed"));
        object.insert("comment_idempotency_key".to_string(), json!(comment_key));
        object.insert(
            "resolution_idempotency_key".to_string(),
            json!(resolution_key),
        );
        object.insert("updated_at".to_string(), json!(clock.now_rfc3339()));
        object.insert(
            "skipped_reason".to_string(),
            json!("needs_user_judgment_comments_disabled"),
        );
    }
    let audit = marker_action_audit(
        &action,
        "failed",
        &comment_key,
        None,
        &ResolveAudit {
            resolve_attempted: false,
            resolve_succeeded: false,
            resolve_error: None,
            final_thread_resolved_state: None,
        },
    );
    MarkerActionOutcome {
        action,
        status: "failed".to_string(),
        comment_key,
        resolution_key,
        posted_comment: None,
        resolved_thread: None,
        skipped,
        retryable: partial.clone(),
        partial,
        failed: None,
        audit,
        updated_action,
    }
}

fn validate_marker_action_evidence(
    binding: &PrFollowupBinding,
    store: &PrFollowupArtifactStore,
    action: PendingMarkerAction,
    comment_key: String,
    resolution_key: String,
    clock: &dyn ClockSleeper,
) -> Result<Option<MarkerActionOutcome>, EngineError> {
    if !matches!(
        action.action_kind.as_str(),
        "comment_fixed" | "resolve_thread"
    ) {
        return Ok(None);
    }
    let result = store.read_current_json(binding, "pr-remediation-result");
    let valid = match result {
        Ok(payload) => marker_action_has_validator_success(binding, &action, &payload),
        Err(err) => {
            if store.canonical_path(binding, "pr-remediation-result").exists() {
                return Err(err);
            }
            false
        }
    };
    if valid {
        return Ok(None);
    }
    let failed = json!({
        "idempotency_key": comment_key,
        "resolution_idempotency_key": resolution_key,
        "action_id": action.value.get("action_id").cloned().unwrap_or(Value::Null),
        "reason": "missing_validator_success_evidence",
        "failure_state": "failed_fatal"
    });
    let mut updated_action = action.value.clone();
    if let Some(object) = updated_action.as_object_mut() {
        object.insert("status".to_string(), json!("failed"));
        object.insert("comment_idempotency_key".to_string(), json!(comment_key));
        object.insert(
            "resolution_idempotency_key".to_string(),
            json!(resolution_key),
        );
        object.insert(
            "failure_reason".to_string(),
            json!("missing_validator_success_evidence"),
        );
        object.insert("updated_at".to_string(), json!(clock.now_rfc3339()));
    }
    let audit = marker_action_audit(
        &action,
        "failed",
        &comment_key,
        None,
        &ResolveAudit {
            resolve_attempted: false,
            resolve_succeeded: false,
            resolve_error: None,
            final_thread_resolved_state: None,
        },
    );
    Ok(Some(MarkerActionOutcome {
        action,
        status: "failed".to_string(),
        comment_key,
        resolution_key,
        posted_comment: None,
        resolved_thread: None,
        skipped: Vec::new(),
        partial: None,
        retryable: None,
        failed: Some(failed),
        audit,
        updated_action,
    }))
}

fn marker_action_has_validator_success(
    binding: &PrFollowupBinding,
    action: &PendingMarkerAction,
    result: &Value,
) -> bool {
    if result.get("validation_state").and_then(Value::as_str) != Some("valid") {
        return false;
    }
    if action
        .value
        .get("remediation_result_evidence")
        .is_none_or(|evidence| evidence.is_null())
    {
        return false;
    }
    let result_input_head = result
        .get("input_head_sha")
        .and_then(Value::as_str)
        .unwrap_or(&binding.head_sha);
    if result_input_head != action.source_head_sha {
        return false;
    }
    if action.remediation_output_head != "none"
        && result.get("output_head_sha").and_then(Value::as_str)
            != Some(action.remediation_output_head.as_str())
    {
        return false;
    }
    result
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|item| {
            item.get("source_type").and_then(Value::as_str) == Some("coderabbit_feedback")
                && item.get("source_id").and_then(Value::as_str) == Some(action.item_id.as_str())
                && matches!(
                    item.get("status").and_then(Value::as_str),
                    Some("fixed" | "changed" | "already_satisfied" | "not_reproduced")
                )
                && item.get("input_head_sha").and_then(Value::as_str)
                    == Some(action.source_head_sha.as_str())
                && item.get("body_hash").and_then(Value::as_str) == Some(action.body_hash.as_str())
                && item.get("stable_marker_key").and_then(Value::as_str)
                    == Some(action.stable_marker_key.as_str())
                && item
                    .get("evidence")
                    .is_some_and(|evidence| !evidence.is_null())
                && result
                    .get("retry_scope")
                    .and_then(|scope| scope.get("run_id"))
                    .and_then(Value::as_str)
                    == Some(binding.run_id.as_str())
        })
}

fn write_marker_comment_body_file(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    action: &PendingMarkerAction,
    body: &str,
    clock: &dyn ClockSleeper,
) -> Result<PathBuf, EngineError> {
    let record = store.write_raw_text_artifact(
        binding,
        "feedback-marker-comment-body",
        step_id,
        step_order,
        &format!(
            "{}-{}",
            action.action_kind,
            stable_hash(&action.stable_marker_key)
        ),
        body,
        clock,
    )?;
    let body_path = record.history_path.with_extension("body.md");
    std::fs::write(&body_path, body).map_err(|err| {
        github_feedback_error(format!(
            "write marker comment body file {}: {err}",
            body_path.display()
        ))
    })?;
    Ok(body_path)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-026
/// @pseudocode lines 46-49
fn resolution_policy(action: &PendingMarkerAction, params: &Value) -> &'static str {
    // needs_user_judgment is always left open for a human to decide.
    if action.action_kind == "comment_needs_user_judgment" {
        return "skip";
    }
    if action.thread_id.is_none() {
        return "skip";
    }
    // Legacy explicit overrides remain honored for fixed feedback only.
    if action.action_kind == "comment_fixed" {
        if let Some(resolve_fixed) = params.get("resolve_fixed").and_then(Value::as_bool) {
            return if resolve_fixed { "required" } else { "skip" };
        }
    }
    if action.action_kind == "comment_out_of_scope" {
        return "skip";
    }
    // Default policy: resolve fixed/changed/already_satisfied/not_reproduced
    // (carried via resolution_required); leave discounted items open.
    if action.resolution_required {
        "required"
    } else {
        "skip"
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-016,REQ-PRFU-026
/// @pseudocode lines 43-49
fn marker_action_key(
    binding: &PrFollowupBinding,
    action: &PendingMarkerAction,
    operation: &str,
) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
        operation,
        binding.run_id,
        binding.repository_owner,
        binding.repository_name,
        binding.pr_number,
        action.source_head_sha,
        action.remediation_output_head,
        action.body_hash,
        action.action_kind.as_str(),
        action.stable_marker_key
    )
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-016
/// @pseudocode lines 43-44
fn marker_action_key_from_marker(
    binding: &PrFollowupBinding,
    marker: &RemoteFeedbackMarker,
    operation: &str,
) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
        operation,
        marker.run_id,
        binding.repository_owner,
        binding.repository_name,
        binding.pr_number,
        marker.source_head_sha,
        marker
            .remediation_output_head_sha
            .clone()
            .unwrap_or_else(|| "none".to_string()),
        marker.body_hash,
        marker.action_kind,
        marker.stable_marker_key
    )
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-026
/// @pseudocode lines 47-49
fn marker_report_payload(
    binding: &PrFollowupBinding,
    outcomes: &[MarkerActionOutcome],
    malformed_remote_markers: Vec<Value>,
    marked_at: String,
) -> Value {
    let posted_comments = outcomes
        .iter()
        .filter_map(|outcome| outcome.posted_comment.clone())
        .collect::<Vec<_>>();
    let resolved_threads = outcomes
        .iter()
        .filter_map(|outcome| outcome.resolved_thread.clone())
        .collect::<Vec<_>>();
    let skipped_actions = outcomes
        .iter()
        .flat_map(|outcome| outcome.skipped.clone())
        .collect::<Vec<_>>();
    let partial_actions = outcomes
        .iter()
        .filter_map(|outcome| outcome.partial.clone())
        .collect::<Vec<_>>();
    let retryable_actions = outcomes
        .iter()
        .filter_map(|outcome| outcome.retryable.clone())
        .collect::<Vec<_>>();
    let failed_actions = outcomes
        .iter()
        .filter_map(|outcome| outcome.failed.clone())
        .collect::<Vec<_>>();
    json!({
        "schema_version": PR_FOLLOWUP_SCHEMA_VERSION,
        "run_id": binding.run_id,
        "repository_owner": binding.repository_owner,
        "repository_name": binding.repository_name,
        "pr_number": binding.pr_number,
        "head_ref": binding.head_ref,
        "head_sha": binding.head_sha,
        "base_ref": binding.base_ref,
        "base_sha": binding.base_sha,
        "marked_at": marked_at,
        "posted_comments": posted_comments,
        "resolved_threads": resolved_threads,
        "skipped_actions": skipped_actions,
        "partial_actions": partial_actions,
        "retryable_actions": retryable_actions,
        "failed_actions": failed_actions,
        "malformed_remote_markers": malformed_remote_markers,
        "action_statuses": outcomes.iter().map(|outcome| json!({
            "action_id": outcome.action.value.get("action_id").cloned().unwrap_or(Value::Null),
            "item_id": outcome.action.item_id,
            "status": outcome.status,
            "comment_idempotency_key": outcome.comment_key,
            "resolution_idempotency_key": outcome.resolution_key
        })).collect::<Vec<_>>(),
        "action_audit": outcomes.iter().map(|outcome| outcome.audit.clone()).collect::<Vec<_>>()
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-016,REQ-PRFU-026
/// @pseudocode lines 47-49
fn write_updated_pending_actions(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    pending_artifact: &Value,
    outcomes: &[MarkerActionOutcome],
    clock: &dyn ClockSleeper,
) -> Result<(), EngineError> {
    let mut actions = pending_artifact
        .get("pending_actions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for outcome in outcomes {
        if let Some(action_id) = outcome
            .action
            .value
            .get("action_id")
            .and_then(Value::as_str)
        {
            if let Some(existing) = actions
                .iter_mut()
                .find(|action| action.get("action_id").and_then(Value::as_str) == Some(action_id))
            {
                *existing = outcome.updated_action.clone();
            }
        }
    }
    let payload = json!({
        "pending_actions": actions,
        "carry_forward_from_artifact_sequence": pending_artifact.get("artifact_sequence").cloned().unwrap_or(Value::Null),
        "marker_policy": pending_artifact.get("marker_policy").cloned().unwrap_or_else(|| json!({})),
        "updated_at": clock.now_rfc3339()
    });
    store.write_json_artifact(
        binding,
        PENDING_MARKER_ACTIONS_FAMILY,
        step_id,
        step_order,
        &payload,
        None,
        clock,
    )?;
    Ok(())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015
/// @pseudocode lines 45
fn sanitize_visible_text(text: &str) -> String {
    text.replace('`', "'")
        .replace("<!--", "&lt;!--")
        .replace("-->", "--&gt;")
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009
/// @pseudocode lines 20,26-28
fn write_feedback_artifacts(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    payload: &Value,
    clock: &dyn ClockSleeper,
    failure: Option<(&str, &str, Value)>,
) -> Result<(), EngineError> {
    store.write_json_artifact(
        binding,
        "coderabbit-feedback",
        step_id,
        step_order,
        payload,
        failure,
        clock,
    )?;
    let state_entries = payload
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|item| {
            json!({
                "item_id": item.get("item_id").cloned().unwrap_or(Value::Null),
                "stable_marker_key": item.get("stable_marker_key").cloned().unwrap_or(Value::Null),
                "body_hash": item.get("body_hash").cloned().unwrap_or(Value::Null),
                "head_sha": binding.head_sha,
                "first_seen_at": payload.get("observed_at").cloned().unwrap_or(Value::Null),
                "last_seen_at": payload.get("observed_at").cloned().unwrap_or(Value::Null),
                "evaluation_status": "unevaluated",
                "accepted_evaluation": null,
                "remediation_status": null,
                "marker_status": "pending",
                "resolution_status": null,
                "superseded": false,
                "stale": false,
                "reuse_eligible": false
            })
        })
        .collect::<Vec<_>>();
    store.write_json_artifact(binding, "coderabbit-feedback-state", step_id, step_order, &json!({
        "state_entries": state_entries,
        "state_index_hash": stable_hash(&serde_json::to_string(payload.get("items").unwrap_or(&Value::Null)).unwrap_or_default()),
        "superseded_entries": []
    }), None, clock)?;
    Ok(())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009
/// @pseudocode lines 14,20-23
#[allow(clippy::too_many_arguments)]
fn feedback_payload(
    binding: &PrFollowupBinding,
    readiness_state: &str,
    stable_count: u64,
    required_stable: u64,
    max_observations: u64,
    interval_seconds: u64,
    observations: &[Value],
    final_observation: &FeedbackObservation,
    identities: &BTreeSet<String>,
    budget_used: u64,
    observed_at: String,
    outcome_reason: &str,
) -> Value {
    let items = final_observation
        .items
        .iter()
        .map(item_json)
        .collect::<Vec<_>>();
    json!({
        "readiness_state": readiness_state,
        "stable_observation_count": stable_count,
        "required_stable_observations": required_stable,
        "max_observations": max_observations,
        "observation_interval_seconds": interval_seconds,
        "observations": observations,
        "items": items,
        "included_bot_identities": identities.iter().cloned().collect::<Vec<_>>(),
        "feedback_item_set_hash": item_set_hash(&final_observation.items),
        "items_count": final_observation.items.len(),
        "remote_marker_comments_seen": final_observation.remote_markers.iter().map(remote_marker_json).collect::<Vec<_>>(),
        "remote_marker_audit": final_observation.remote_marker_audit,
        "malformed_remote_markers": final_observation.malformed_remote_markers,
        "stale_feedback_items": final_observation.stale_items,
        "noise": final_observation.noise,
        "observed_at": observed_at,
        "budget_used": budget_used,
        "budget_remaining": max_observations.saturating_sub(budget_used),
        "outcome_reason": outcome_reason,
        "binding_head_sha": binding.head_sha
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 14-19
fn observation_json(
    observation: &FeedbackObservation,
    item_hash: &str,
    budget_used: u64,
    max_observations: u64,
    observed_at: &str,
    outcome_reason: &str,
) -> Value {
    json!({
        "observed_at": observed_at,
        "signals_seen": {
            "ready": observation.ready_signal,
            "in_progress": observation.in_progress_signal,
        },
        "bot_identities_matched": observation.matched_identities.iter().cloned().collect::<Vec<_>>(),
        "observation_hash": readiness_stability_hash(observation),
        "feedback_item_set_hash": item_hash,
        "readiness_stability_hash": readiness_stability_hash(observation),
        "budget_used": budget_used,
        "budget_remaining": max_observations.saturating_sub(budget_used),
        "items_count": observation.items.len(),
        "outcome_reason": outcome_reason,
        "current_head_ready_signal_seen": observation.ready_signal && !observation.in_progress_signal,
        "in_progress_signal_seen": observation.in_progress_signal,
        "readiness_signals": observation.readiness_signals,
        "stale_signals": observation.stale_signals,
        "noise_count": observation.noise.len()
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009
/// @pseudocode lines 7-14
fn item_json(item: &FeedbackItem) -> Value {
    json!({
        "item_id": item.item_id,
        "stable_marker_key": item.stable_marker_key,
        "thread_id": item.thread_id,
        "comment_id": item.comment_id,
        "comment_database_id": item.comment_database_id,
        "review_id": item.review_id,
        "author_login": item.author_login,
        "author_association": null,
        "bot_identity": item.author_login,
        "path": item.path,
        "line": item.line,
        "side": item.side,
        "body": item.body,
        "body_hash": item.body_hash,
        "url": item.url,
        "created_at": item.created_at,
        "updated_at": item.updated_at,
        "resolved": item.resolved,
        "outdated": item.outdated,
        "resolution_state_available": item.resolution_state_available,
        "source": item.source,
        "raw_node_id": item.raw_node_id,
        "commit_sha": item.commit_sha,
        "stale": item.stale
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-009
/// @pseudocode lines 13,20
fn remote_marker_json(marker: &RemoteFeedbackMarker) -> Value {
    json!({
        "stable_marker_key": marker.stable_marker_key,
        "source_head_sha": marker.source_head_sha,
        "remediation_output_head_sha": marker.remediation_output_head_sha,
        "body_hash": marker.body_hash,
        "action_kind": marker.action_kind,
        "run_id": marker.run_id,
        "status": marker.status
    })
}

