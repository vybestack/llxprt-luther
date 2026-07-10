//! Pull-request state parsing: deserialize shapes for `closedByPullRequests
//! References` and `gh pr list`, relevance selection, and status-check rollup
//! summarization.
use serde::Deserialize;

use crate::adapters::github::GithubError;

use super::{normalize_state, GithubIssuePrState};

#[derive(Debug, Deserialize)]
pub(super) struct RawIssuePrReferences {
    #[serde(default, rename = "closedByPullRequestsReferences")]
    pub(super) closed_by_pull_requests_references: Vec<RawPrState>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawPrState {
    pub(super) number: u64,
    #[serde(default)]
    pub(super) state: String,
    #[serde(default)]
    pub(super) merged: bool,
    #[serde(rename = "updatedAt")]
    pub(super) updated_at: Option<String>,
    #[serde(rename = "mergeCommit")]
    pub(super) merge_commit: Option<RawCommit>,
    #[serde(rename = "reviewDecision")]
    pub(super) review_decision: Option<String>,
    #[serde(default, rename = "statusCheckRollup")]
    pub(super) status_check_rollup: Vec<RawStatusCheck>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawCommit {
    pub(super) oid: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawStatusCheck {
    pub(super) conclusion: Option<String>,
    pub(super) state: Option<String>,
}

pub(super) fn parse_pr_state(
    json: &str,
    argv: &[String],
) -> Result<Option<GithubIssuePrState>, GithubError> {
    let prs: Vec<RawPrState> =
        serde_json::from_str(json).map_err(|e| GithubError::CommandFailed {
            argv: argv.to_vec(),
            exit_code: None,
            stderr: format!("failed to parse gh pr list JSON: {e}"),
        })?;
    Ok(select_relevant_pr(prs).map(raw_pr_state_to_issue_pr_state))
}

pub(super) fn parse_issue_pr_references(
    json: &str,
    argv: &[String],
) -> Result<Vec<RawPrState>, GithubError> {
    let refs: RawIssuePrReferences =
        serde_json::from_str(json).map_err(|e| GithubError::CommandFailed {
            argv: argv.to_vec(),
            exit_code: None,
            stderr: format!("failed to parse gh issue PR references JSON: {e}"),
        })?;
    Ok(refs.closed_by_pull_requests_references)
}

pub(super) fn select_relevant_pr(mut prs: Vec<RawPrState>) -> Option<RawPrState> {
    prs.sort_by(|left, right| {
        pr_relevance_key(right)
            .cmp(&pr_relevance_key(left))
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| right.number.cmp(&left.number))
    });
    prs.into_iter().next()
}

fn pr_relevance_key(pr: &RawPrState) -> u8 {
    let state = normalize_state(&pr.state);
    if state == "open" {
        2
    } else if pr.merged || state == "merged" {
        1
    } else {
        0
    }
}

pub(super) fn pr_reference_usable(pr: &RawPrState) -> bool {
    pr_relevance_key(pr) > 0
}

pub(super) fn raw_pr_state_to_issue_pr_state(pr: RawPrState) -> GithubIssuePrState {
    let state = normalize_state(&pr.state);
    GithubIssuePrState {
        number: pr.number,
        merged: pr.merged || state == "merged",
        state,
        merge_commit_sha: pr.merge_commit.and_then(|commit| commit.oid),
        review_decision: pr.review_decision.map(|decision| decision.to_lowercase()),
        status_check_rollup: summarize_status_check_rollup(&pr.status_check_rollup),
    }
}

fn summarize_status_check_rollup(checks: &[RawStatusCheck]) -> Option<String> {
    let blocking: Vec<&RawStatusCheck> = checks
        .iter()
        .filter(|check| !status_check_ignored(check))
        .collect();
    if blocking.is_empty() {
        return None;
    }
    if blocking
        .iter()
        .any(|check| status_check_failed(check) || status_check_unknown_terminal(check))
    {
        return Some("failed".to_string());
    }
    if blocking.iter().all(|check| status_check_passed(check)) {
        return Some("passed".to_string());
    }
    Some("pending".to_string())
}

fn status_check_failed(check: &RawStatusCheck) -> bool {
    matches!(
        check_conclusion(check).as_deref(),
        Some(
            "failure" | "timed_out" | "action_required" | "cancelled" | "stale" | "startup_failure"
        )
    ) || matches!(check_state(check).as_deref(), Some("failure" | "error"))
}

fn status_check_passed(check: &RawStatusCheck) -> bool {
    check_conclusion(check).as_deref() == Some("success")
        || check_state(check).as_deref() == Some("success")
}

fn status_check_unknown_terminal(check: &RawStatusCheck) -> bool {
    check
        .conclusion
        .as_deref()
        .is_some_and(|conclusion| !conclusion.trim().is_empty())
        && !status_check_passed(check)
        && !status_check_ignored(check)
}

fn status_check_ignored(check: &RawStatusCheck) -> bool {
    matches!(
        check_conclusion(check).as_deref(),
        Some("skipped" | "neutral")
    )
}

fn check_conclusion(check: &RawStatusCheck) -> Option<String> {
    check
        .conclusion
        .as_ref()
        .map(|value| value.to_ascii_lowercase())
}

fn check_state(check: &RawStatusCheck) -> Option<String> {
    check.state.as_ref().map(|value| value.to_ascii_lowercase())
}
