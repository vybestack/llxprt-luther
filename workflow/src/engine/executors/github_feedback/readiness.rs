use super::*;
use crate::engine::executor::StepContext;
use crate::engine::executors::github_pr::GithubPrCommandRunner;
use crate::engine::executors::pr_followup_artifacts::{ClockSleeper, PrFollowupArtifactStore};
use crate::engine::executors::pr_followup_types::{PrFollowupBinding, PR_FOLLOWUP_SCHEMA_VERSION};
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn is_coderabbit_summary_feedback_item(item: &FeedbackItem) -> bool {
    if item.source != "issue_comment" || item.stale {
        return false;
    }
    coderabbit_body_is_non_actionable_notice(&item.body)
}

/// Recognizes CodeRabbit auto-generated issue comments that signal a completed
/// (or unavailable) review and therefore carry no actionable feedback: the
/// summary/walkthrough comment, the "finished reviewing" notice, and the
/// rate-limit / out-of-credits "review limit reached" warning. Any of these
/// confirms CodeRabbit has reported on the current head, so readiness can be
/// satisfied without an actual review thread.
pub(super) fn coderabbit_body_is_non_actionable_notice(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    body.contains("summary by coderabbit")
        || body.contains("summarize by coderabbit")
        || (body.contains("walkthrough") && body.contains("coderabbit"))
        || body.contains("coderabbit finished reviewing this pull request")
        || body.contains("rate limited by coderabbit")
        || body.contains("review limit reached")
        || (body.contains("coderabbit") && body.contains("run out of usage credits"))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009
/// @pseudocode lines 7-14
pub(super) fn push_current_or_stale(item: FeedbackItem, observation: &mut FeedbackObservation) {
    if item.stale {
        observation.stale_items.push(item_json(&item));
    } else {
        observation.items.push(item);
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017
/// @pseudocode lines 4-6
pub(super) const GRAPHQL_REVIEW_THREADS_QUERY: &str = r#"
query($owner: String!, $name: String!, $number: Int!, $page: String) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      reviewThreads(first: 100, after: $page) {
        nodes {
          id
          isResolved
          isOutdated
          path
          line
          startLine
          comments(first: 100) {
            nodes {
              id
              databaseId
              body
              author { login __typename }
              url
              path
              line
              originalLine
              createdAt
              updatedAt
              commit { oid }
            }
          }
        }
        pageInfo {
          hasNextPage
          endCursor
        }
      }
    }
  }
}
"#;

pub(super) fn query_review_threads(
    runner: &dyn GithubPrCommandRunner,
    binding: &PrFollowupBinding,
) -> Result<Value, EngineError> {
    let mut all_nodes = Vec::new();
    let mut cursor: Option<String> = None;
    let mut page_count = 0;
    loop {
        page_count += 1;
        let value = query_review_thread_page(runner, binding, cursor.as_deref())?;
        if is_permission_or_schema_error(&value) {
            return Ok(value);
        }
        let thread_value = review_thread_page_value(&value);
        all_nodes.extend(review_thread_page_nodes(&thread_value));
        if !review_thread_page_has_next(&thread_value, page_count) {
            break;
        }
        cursor = review_thread_page_cursor(&thread_value);
        if cursor.is_none() {
            break;
        }
    }

    Ok(json!({
        "data": { "repository": { "pullRequest": { "reviewThreads": { "nodes": all_nodes } } } }
    }))
}

pub(super) fn query_review_thread_page(
    runner: &dyn GithubPrCommandRunner,
    binding: &PrFollowupBinding,
    cursor: Option<&str>,
) -> Result<Value, EngineError> {
    let output = runner.run_github_command(&review_thread_page_argv(binding, cursor))?;
    serde_json::from_str(&output)
        .map_err(|err| github_feedback_error(format!("parse review threads response: {err}")))
}

pub(super) fn review_thread_page_argv(
    binding: &PrFollowupBinding,
    cursor: Option<&str>,
) -> Vec<String> {
    let mut argv = vec![
        "gh".to_string(),
        "api".to_string(),
        "graphql".to_string(),
        "-f".to_string(),
        format!("query={GRAPHQL_REVIEW_THREADS_QUERY}"),
        "-f".to_string(),
        format!("owner={}", binding.repository_owner),
        "-f".to_string(),
        format!("name={}", binding.repository_name),
        "-F".to_string(),
        format!("number={}", binding.pr_number),
    ];
    if let Some(cursor) = cursor {
        argv.push("-f".to_string());
        argv.push(format!("page={cursor}"));
    }
    argv
}

pub(super) fn review_thread_page_value(value: &Value) -> Value {
    value
        .pointer("/data/repository/pullRequest/reviewThreads")
        .cloned()
        .unwrap_or_else(|| json!({}))
}

pub(super) fn review_thread_page_nodes(thread_value: &Value) -> Vec<Value> {
    thread_value
        .get("nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

pub(super) fn review_thread_page_has_next(thread_value: &Value, page_count: u64) -> bool {
    let has_next = thread_value
        .pointer("/pageInfo/hasNextPage")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if has_next && page_count >= 20 {
        eprintln!("warning: pagination limit reached for review threads; results may be truncated");
        return false;
    }
    has_next
}

pub(super) fn review_thread_page_cursor(thread_value: &Value) -> Option<String> {
    thread_value
        .pointer("/pageInfo/endCursor")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017
/// @pseudocode lines 5,8-9
pub(super) fn query_rest_review_comments(
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

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017
/// @pseudocode lines 5,9
pub(super) fn query_issue_comments(
    runner: &dyn GithubPrCommandRunner,
    binding: &PrFollowupBinding,
) -> Result<Vec<Value>, EngineError> {
    query_paginated_array(
        runner,
        &format!(
            "/repos/{}/{}/issues/{}/comments?per_page=100&page=",
            binding.repository_owner, binding.repository_name, binding.pr_number
        ),
    )
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-017
/// @pseudocode lines 6,15-17
pub(super) fn query_readiness_signals(
    runner: &dyn GithubPrCommandRunner,
    binding: &PrFollowupBinding,
) -> Result<Vec<Value>, EngineError> {
    let checks = query_paginated_check_runs(runner, binding)?;
    Ok(checks
        .iter()
        .map(|check| json!({
            "source": "check_run",
            "head_sha": readiness_signal_head_sha(check, binding),
            "bot_login": check.pointer("/app/slug").and_then(Value::as_str).unwrap_or_default(),
            "status": check.get("status").and_then(Value::as_str).unwrap_or_default(),
            "conclusion": check.get("conclusion").cloned().unwrap_or(Value::Null),
            "summary_body": check.get("output").and_then(|output| output.get("summary")).and_then(Value::as_str).unwrap_or_default()
        }))
        .collect())
}

pub(super) fn query_paginated_check_runs(
    runner: &dyn GithubPrCommandRunner,
    binding: &PrFollowupBinding,
) -> Result<Vec<Value>, EngineError> {
    let mut page = 1;
    let mut checks = Vec::new();
    loop {
        let output = runner.run_github_command(&[
            "gh".to_string(),
            "api".to_string(),
            format!(
                "/repos/{}/{}/commits/{}/check-runs?per_page=100&page={page}",
                binding.repository_owner, binding.repository_name, binding.head_sha
            ),
        ])?;
        let value: Value = serde_json::from_str(&output).map_err(|err| {
            github_feedback_error(format!("parse readiness check-runs response: {err}"))
        })?;
        let page_checks = value
            .get("check_runs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let page_len = page_checks.len();
        checks.extend(page_checks);
        if page_len < 100 || page >= 20 {
            if page >= 20 && page_len == 100 {
                eprintln!(
                    "warning: pagination limit reached for check-runs; results may be truncated"
                );
            }
            break;
        }
        page += 1;
    }
    Ok(checks)
}

pub(super) fn readiness_signal_head_sha(check: &Value, binding: &PrFollowupBinding) -> String {
    check
        .get("head_sha")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            let name = check
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let lower_name = name.to_ascii_lowercase();
            let check_run_url = check
                .get("check_run_url")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let html_url = check
                .get("html_url")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if lower_name.contains("coderabbit")
                || check_run_url.contains("/commits/")
                || html_url.contains("/checks")
            {
                binding.head_sha.as_str()
            } else {
                ""
            }
        })
        .to_string()
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017
/// @pseudocode lines 5
pub(super) fn query_paginated_array(
    runner: &dyn GithubPrCommandRunner,
    endpoint_prefix: &str,
) -> Result<Vec<Value>, EngineError> {
    let mut page = 1;
    let mut values = Vec::new();
    loop {
        let output = runner.run_github_command(&[
            "gh".to_string(),
            "api".to_string(),
            format!("{endpoint_prefix}{page}"),
        ])?;
        let page_values: Value = serde_json::from_str(&output)
            .map_err(|err| github_feedback_error(format!("parse paginated response: {err}")))?;
        let Some(array) = page_values.as_array() else {
            break;
        };
        if array.is_empty() {
            break;
        }
        values.extend(array.iter().cloned());
        if array.len() < 100 || page >= 20 {
            if page >= 20 && array.len() == 100 {
                eprintln!("warning: pagination limit reached for REST feedback query; results may be truncated");
            }
            break;
        }
        page += 1;
    }
    Ok(values)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-009,REQ-PRFU-016
/// @pseudocode lines 4-5,13,20,26
pub(super) fn record_remote_marker_parse(
    body: &str,
    source: &str,
    comment_id: Value,
    observation: &mut FeedbackObservation,
) {
    if !body.contains(MARKER_NAMESPACE) {
        return;
    }
    match parse_hidden_marker(body) {
        Ok(marker) => observation.remote_markers.push(marker),
        Err(err) => observation.malformed_remote_markers.push(json!({
            "source": source,
            "comment_id": comment_id,
            "diagnostic": err.diagnostic,
        })),
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-009,REQ-PRFU-016
/// @pseudocode lines 13,20,26-28
pub(super) fn resolve_remote_markers(
    binding: &PrFollowupBinding,
    observation: &mut FeedbackObservation,
) {
    let grouped = group_remote_markers(&observation.remote_markers);
    let mut accepted = Vec::new();
    for (identity, markers) in grouped {
        let Some(marker) = resolve_remote_marker_group(&identity, &markers, observation) else {
            // Fatal conflicting remote marker already recorded in observation;
            // fail fast so callers do not act on a partial remote-marker view.
            return;
        };
        audit_current_head_remote_marker(&identity, &marker, binding, observation);
        accepted.push(marker);
    }
    observation.remote_markers = accepted;
    ignore_feedback_completed_remotely(binding, observation);
}

pub(super) fn group_remote_markers(
    markers: &[RemoteFeedbackMarker],
) -> BTreeMap<String, Vec<RemoteFeedbackMarker>> {
    let mut by_identity: BTreeMap<String, Vec<RemoteFeedbackMarker>> = BTreeMap::new();
    for marker in markers.iter().cloned() {
        by_identity
            .entry(remote_marker_identity(&marker))
            .or_default()
            .push(marker);
    }
    by_identity
}

pub(super) fn resolve_remote_marker_group(
    identity: &str,
    markers: &[RemoteFeedbackMarker],
    observation: &mut FeedbackObservation,
) -> Option<RemoteFeedbackMarker> {
    let first = markers.first()?.clone();
    if has_conflicting_remote_marker_duplicates(&first, markers) {
        observation.fatal = Some(remote_marker_conflict_json(identity, markers));
        return None;
    }
    if markers.len() > 1 {
        observation.remote_marker_audit.push(json!({
            "event": "duplicate_identical_remote_marker_already_complete",
            "identity": identity,
            "count": markers.len(),
            "marker": remote_marker_json(&first),
        }));
    }
    Some(first)
}

pub(super) fn has_conflicting_remote_marker_duplicates(
    first: &RemoteFeedbackMarker,
    markers: &[RemoteFeedbackMarker],
) -> bool {
    !markers.iter().all(|marker| marker == first)
}

pub(super) fn remote_marker_conflict_json(
    identity: &str,
    markers: &[RemoteFeedbackMarker],
) -> Value {
    json!({
        "class": "conflicting_remote_marker_duplicates",
        "identity": identity,
        "markers": markers.iter().map(remote_marker_json).collect::<Vec<_>>(),
    })
}

pub(super) fn audit_current_head_remote_marker(
    identity: &str,
    marker: &RemoteFeedbackMarker,
    binding: &PrFollowupBinding,
    observation: &mut FeedbackObservation,
) {
    if marker.status == "completed" && marker.source_head_sha == binding.head_sha {
        observation.remote_marker_audit.push(json!({
            "event": "matching_remote_completed_marker_current_head",
            "identity": identity,
            "marker": remote_marker_json(marker),
        }));
    }
}

pub(super) fn ignore_feedback_completed_remotely(
    binding: &PrFollowupBinding,
    observation: &mut FeedbackObservation,
) {
    let completed_keys = current_head_completed_remote_keys(binding, observation);
    if completed_keys.is_empty() {
        return;
    }
    retain_uncompleted_feedback_items(binding, observation, &completed_keys);
    retain_uncompleted_stale_feedback_items(binding, observation, &completed_keys);
}

pub(super) fn current_head_completed_remote_keys(
    binding: &PrFollowupBinding,
    observation: &FeedbackObservation,
) -> BTreeSet<(String, String)> {
    observation
        .remote_markers
        .iter()
        .filter(|marker| marker.status == "completed" && marker.source_head_sha == binding.head_sha)
        .map(|marker| (marker.stable_marker_key.clone(), marker.body_hash.clone()))
        .collect()
}

pub(super) fn retain_uncompleted_feedback_items(
    binding: &PrFollowupBinding,
    observation: &mut FeedbackObservation,
    completed_keys: &BTreeSet<(String, String)>,
) {
    observation.items.retain(|item| {
        let complete =
            completed_keys.contains(&(item.stable_marker_key.clone(), item.body_hash.clone()));
        if complete {
            observation.remote_marker_audit.push(json!({
                "event": "local_feedback_ignored_remote_completed",
                "stable_marker_key": item.stable_marker_key,
                "body_hash": item.body_hash,
                "source_head_sha": binding.head_sha,
            }));
        }
        !complete
    });
}

pub(super) fn retain_uncompleted_stale_feedback_items(
    binding: &PrFollowupBinding,
    observation: &mut FeedbackObservation,
    completed_keys: &BTreeSet<(String, String)>,
) {
    observation.stale_items.retain(|item| {
        let key = string_field(item, "stable_marker_key");
        let hash = string_field(item, "body_hash");
        let complete = completed_keys.contains(&(key.clone(), hash.clone()));
        if complete {
            observation.remote_marker_audit.push(json!({
                "event": "stale_local_feedback_ignored_remote_completed",
                "stable_marker_key": key,
                "body_hash": hash,
                "source_head_sha": binding.head_sha,
            }));
        }
        !complete
    });
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-009,REQ-PRFU-016
/// @pseudocode lines 13,20,26
pub(super) fn remote_marker_identity(marker: &RemoteFeedbackMarker) -> String {
    format!(
        "{}|{}|{}|{}",
        marker.stable_marker_key, marker.source_head_sha, marker.run_id, marker.action_kind
    )
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017,REQ-PRFU-026
/// @pseudocode lines 41-49
// Pre-existing marker orchestration flow; split in a dedicated refactor stage.
pub(super) fn mark_coderabbit_feedback(
    context: &mut StepContext,
    params: &Value,
    runner: &dyn GithubPrCommandRunner,
    clock: &dyn ClockSleeper,
) -> Result<StepOutcome, EngineError> {
    let artifact_root = artifact_root(context, params)?;
    let store = PrFollowupArtifactStore::new(artifact_root);
    let binding = read_or_build_binding(context, params, &store)?;
    let step_id = current_step_id(context, "mark_coderabbit_feedback");
    let step_order = u64_param(params, "step_order_index", 15);
    let mut pending_artifact = read_pending_marker_artifact(&store, &binding)?;
    refresh_pending_marker_actions_from_current_artifacts(
        &store,
        &binding,
        &mut pending_artifact,
        params,
        clock,
    );
    let pending_actions = pending_marker_actions_from_artifact(&store, &binding, &pending_artifact);
    if write_marker_validation_failure(
        &store,
        &binding,
        &step_id,
        step_order,
        &pending_actions,
        clock,
    )? {
        return Ok(StepOutcome::Fatal);
    }

    let local_completed = read_local_marker_completions(&store, &binding);
    let remote_scan = scan_remote_marker_completions(runner, &binding)?;
    let processor = MarkerActionProcessor {
        binding: &binding,
        store: &store,
        step_id: &step_id,
        step_order,
        runner,
        clock,
        local_completed: &local_completed,
        remote_completed: &remote_scan.completed,
        params,
    };
    let outcomes = process_pending_marker_actions(&processor, pending_actions)?;
    write_updated_pending_actions(
        &store,
        &binding,
        &step_id,
        step_order,
        &pending_artifact,
        &outcomes,
        clock,
    )?;
    write_marker_report(
        &store,
        &binding,
        &step_id,
        step_order,
        &outcomes,
        remote_scan.malformed,
        clock,
    )
}

pub(super) fn pending_marker_actions_from_artifact(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    pending_artifact: &Value,
) -> Vec<PendingMarkerAction> {
    let thread_identifiers = collect_thread_identifiers_by_action_key(store, binding);
    pending_artifact
        .get("pending_actions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|value| backfill_thread_identifiers(value, &thread_identifiers))
        .filter_map(|value| pending_marker_action_from_value(value).ok())
        .collect()
}

pub(super) fn write_marker_validation_failure(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    pending_actions: &[PendingMarkerAction],
    clock: &dyn ClockSleeper,
) -> Result<bool, EngineError> {
    let Some(violations) = validate_marker_actions_before_mutation(pending_actions) else {
        return Ok(false);
    };
    let report = json!({
        "schema_version": PR_FOLLOWUP_SCHEMA_VERSION,
        "marker_state": "fatal",
        "validation_state": "invalid",
        "validation_violations": violations,
        "github_side_effects_performed": false,
        "generated_at": clock.now_rfc3339()
    });
    store.write_json_artifact(
        binding,
        MARKER_ARTIFACT_FAMILY,
        step_id,
        step_order,
        &report,
        Some((
            "fatal",
            "marker_actions_failed_pre_mutation_validation",
            json!({ "validation_violations": report["validation_violations"].clone() }),
        )),
        clock,
    )?;
    Ok(true)
}

pub(super) struct RemoteMarkerScan {
    pub completed: BTreeSet<String>,
    pub malformed: Vec<Value>,
}

pub(super) fn scan_remote_marker_completions(
    runner: &dyn GithubPrCommandRunner,
    binding: &PrFollowupBinding,
) -> Result<RemoteMarkerScan, EngineError> {
    let mut scan = RemoteMarkerScan {
        completed: BTreeSet::new(),
        malformed: Vec::new(),
    };
    let comments = discover_marker_remote_comments(runner, binding)
        .map_err(|err| github_feedback_error(format!("discover remote marker comments: {err}")))?;
    for comment in comments {
        scan_remote_marker_comment(binding, comment, &mut scan);
    }
    Ok(scan)
}

pub(super) fn scan_remote_marker_comment(
    binding: &PrFollowupBinding,
    comment: Value,
    scan: &mut RemoteMarkerScan,
) {
    let body = comment
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match parse_marker_from_comment_body(body) {
        Ok(marker) => record_completed_remote_marker(binding, &marker, &mut scan.completed),
        Err(err) if body.contains(MARKER_NAMESPACE) => scan.malformed.push(json!({
            "comment_id": comment.get("id").cloned().unwrap_or(Value::Null),
            "diagnostic": err.diagnostic
        })),
        Err(_) => {}
    }
}

pub(super) fn record_completed_remote_marker(
    binding: &PrFollowupBinding,
    marker: &RemoteFeedbackMarker,
    completed: &mut BTreeSet<String>,
) {
    if marker.status == "completed" {
        completed.insert(marker_action_key_from_marker(binding, marker, "comment"));
        completed.insert(marker_action_key_from_marker(binding, marker, "resolution"));
    }
}

pub(super) fn process_pending_marker_actions(
    processor: &MarkerActionProcessor<'_>,
    pending_actions: Vec<PendingMarkerAction>,
) -> Result<Vec<MarkerActionOutcome>, EngineError> {
    pending_actions
        .into_iter()
        .map(|action| process_marker_action(processor, action))
        .collect()
}

pub(super) fn write_marker_report(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    outcomes: &[MarkerActionOutcome],
    malformed_remote_markers: Vec<Value>,
    clock: &dyn ClockSleeper,
) -> Result<StepOutcome, EngineError> {
    let mut report = marker_report_payload(
        binding,
        outcomes,
        malformed_remote_markers,
        clock.now_rfc3339(),
    );
    let state = marker_report_state(&report);
    report["marker_state"] = json!(state);
    let failure = marker_report_failure(state, &report);
    store.write_json_artifact(
        binding,
        MARKER_ARTIFACT_FAMILY,
        step_id,
        step_order,
        &report,
        failure,
        clock,
    )?;
    Ok(if state == "complete" {
        StepOutcome::Success
    } else {
        StepOutcome::Fatal
    })
}

pub(super) fn marker_report_state(report: &Value) -> &'static str {
    let has_failure = report
        .get("failed_actions")
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty());
    let has_partial = report
        .get("partial_actions")
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty());
    if has_failure || has_partial {
        "partial"
    } else {
        "complete"
    }
}

pub(super) fn marker_report_failure<'a>(
    state: &'a str,
    report: &Value,
) -> Option<(&'a str, &'static str, Value)> {
    (state != "complete").then(|| {
        (
            state,
            "marker_actions_incomplete",
            json!({
                "partial_actions": report["partial_actions"].clone(),
                "failed_actions": report["failed_actions"].clone()
            }),
        )
    })
}
