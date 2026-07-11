//! Review-thread reply transport for CodeRabbit marker actions.
//!
//! Extracted from `marker_resolve` so the resolution state machine and the
//! GitHub reply-delivery mechanics (REST review-comment replies, issue-comment
//! fallbacks, and GraphQL review-thread replies) remain separately cohesive.
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
//! @requirement:REQ-PRFU-015,REQ-PRFU-016
use super::*;
use crate::engine::executors::github_pr::GithubPrCommandRunner;
use crate::engine::executors::pr_followup_types::PrFollowupBinding;
use crate::engine::runner::EngineError;
use serde_json::{json, Value};
use std::path::Path;

/// Post the agent-authored reply on the original review thread when thread
/// identity is available. Older REST-only actions fall back to the timeline only
/// when no review-thread identity exists.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016
pub(super) fn post_marker_reply(
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

pub(super) fn marker_action_claims_review_thread_identity(action: &PendingMarkerAction) -> bool {
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

pub(super) fn raw_thread_id_declares_review_thread_identity(value: &Value) -> bool {
    [
        "/thread_id",
        "/evidence/thread_id",
        "/original_feedback_identity/thread_id",
    ]
    .into_iter()
    .filter_map(|path| value.pointer(path).and_then(Value::as_str))
    .any(|thread_id| thread_id.trim().starts_with(REVIEW_THREAD_NODE_ID_PREFIX))
}

pub(super) fn raw_stable_marker_declares_review_thread_identity(stable_marker_key: &str) -> bool {
    stable_marker_key
        .trim()
        .starts_with(STABLE_MARKER_THREAD_PREFIX)
}

pub(super) fn raw_graphql_item_declares_review_thread_identity(item_id: &str) -> bool {
    item_id
        .trim()
        .strip_prefix(GRAPHQL_NODE_ID_PREFIX)
        .is_some_and(|suffix| suffix.starts_with(REVIEW_THREAD_NODE_ID_PREFIX))
}

pub(super) fn post_marker_reply_via_rest_review_comment(
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
    let github_response_summary = rest_response_summary(&parsed);
    let github_response_preview = rest_response_preview(&parsed);
    Ok(marker_reply_record(MarkerReplyRecordInput {
        action,
        comment_key,
        body,
        body_path,
        comment_id: parsed.get("id").cloned().unwrap_or(Value::Null),
        comment_url: parsed.get("html_url").cloned().unwrap_or(Value::Null),
        in_thread_reply: true,
        in_reply_to_id: parsed.get("in_reply_to_id").cloned().unwrap_or(Value::Null),
        warnings: rest_reply_warnings(&parsed, None),
        github_response_summary: github_response_summary.as_deref(),
        github_response_preview,
    }))
}

pub(super) fn post_marker_reply_via_issue_comment(
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
    let github_response_summary = rest_response_summary(&parsed);
    let github_response_preview = rest_response_preview(&parsed);
    Ok(marker_reply_record(MarkerReplyRecordInput {
        action,
        comment_key,
        body,
        body_path,
        comment_id: parsed.get("id").cloned().unwrap_or(Value::Null),
        comment_url: parsed.get("html_url").cloned().unwrap_or(Value::Null),
        in_thread_reply: false,
        in_reply_to_id: Value::Null,
        warnings: rest_reply_warnings(&parsed, Some(WARN_NO_REVIEW_THREAD_IDENTITY_TOP_LEVEL)),
        github_response_summary: github_response_summary.as_deref(),
        github_response_preview,
    }))
}

pub(super) fn post_marker_reply_via_graphql_thread(
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
    let parsed: Value = serde_json::from_str(&response)
        .unwrap_or_else(|err| json!({ "raw_response": response, "parse_error": err.to_string() }));
    let Some(comment) = parsed
        .pointer("/data/addPullRequestReviewThreadReply/comment")
        .filter(|comment| comment.is_object())
    else {
        return marker_reply_record_for_missing_graphql_comment(
            action,
            comment_key,
            body,
            body_path,
            thread_id,
            &parsed,
        );
    };
    let graphql_errors_present = parsed
        .get("errors")
        .and_then(Value::as_array)
        .is_some_and(|errors| !errors.is_empty());
    let comment_id = comment.get("databaseId").cloned();
    let comment_url = comment.get("url").cloned();
    let mut warnings = vec![WARN_POSTED_REVIEW_THREAD_REPLY_GRAPHQL];
    let github_response_summary = if graphql_errors_present {
        warnings.push(WARN_PARTIAL_SUCCESS_GRAPHQL_ERRORS_PRESENT);
        Some(graphql_error_summary(&parsed))
    } else {
        None
    };
    if comment_id.is_none() {
        warnings.push(WARN_MISSING_DATABASE_ID_GRAPHQL_THREAD_REPLY);
    }
    if comment_url.is_none() {
        warnings.push(WARN_MISSING_URL_GRAPHQL_THREAD_REPLY);
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
        github_response_summary: github_response_summary.as_deref(),
        github_response_preview: None,
    }))
}
