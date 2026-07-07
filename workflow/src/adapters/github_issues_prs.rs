#[derive(Debug, Deserialize)]
struct RawIssuePrReferences {
    #[serde(default, rename = "closedByPullRequestsReferences")]
    closed_by_pull_requests_references: Vec<RawPrState>,
}

#[derive(Debug, Deserialize)]
struct RawPrState {
    number: u64,
    #[serde(default)]
    state: String,
    #[serde(default)]
    merged: bool,
    #[serde(default, rename = "updatedAt")]
    updated_at: Option<String>,
    #[serde(default, rename = "mergeCommit")]
    merge_commit: Option<RawCommit>,
    #[serde(default, rename = "reviewDecision")]
    review_decision: Option<String>,
    #[serde(default, rename = "statusCheckRollup")]
    status_check_rollup: Vec<RawStatusCheck>,
}

#[derive(Debug, Deserialize)]
struct RawCommit {
    #[serde(default)]
    oid: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawStatusCheck {
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

fn parse_pr_state(json: &str, argv: &[String]) -> Result<Option<GithubIssuePrState>, GithubError> {
    let prs: Vec<RawPrState> =
        serde_json::from_str(json).map_err(|e| GithubError::CommandFailed {
            argv: argv.to_vec(),
            exit_code: None,
            stderr: format!("failed to parse gh pr list JSON: {e}"),
        })?;
    Ok(select_relevant_pr(prs).map(raw_pr_state_to_issue_pr_state))
}

fn parse_issue_pr_references(json: &str, argv: &[String]) -> Result<Vec<RawPrState>, GithubError> {
    let refs: RawIssuePrReferences =
        serde_json::from_str(json).map_err(|e| GithubError::CommandFailed {
            argv: argv.to_vec(),
            exit_code: None,
            stderr: format!("failed to parse gh issue PR references JSON: {e}"),
        })?;
    Ok(refs.closed_by_pull_requests_references)
}

fn select_relevant_pr(mut prs: Vec<RawPrState>) -> Option<RawPrState> {
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

fn raw_pr_state_to_issue_pr_state(pr: RawPrState) -> GithubIssuePrState {
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
    if blocking.iter().any(|check| status_check_failed(check)) {
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
