//! GitHub issue query adapter for daemon discovery.
//!
//! Provides a testable seam over `gh` for listing issues, checking for open PRs
//! that reference an issue, and listing milestones. The system implementation
//! shells `gh` via the existing [`GithubCommandRunner`] seam; tests inject a
//! mock runner that returns canned JSON so no network access is required.
//!
//! @plan:PLAN-20260415-DAEMON-DISCOVERY.P03
//! @requirement:REQ-DAEMON-DISCOVERY-003

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Condvar, Mutex};

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::adapters::github::{GithubCommandRunner, GithubError};

fn default_merge_method_flag(method: &str) -> Option<&'static str> {
    match method {
        "MERGE" | "merge" => Some("--merge"),
        "REBASE" | "rebase" => Some("--rebase"),
        "SQUASH" | "squash" => Some("--squash"),
        _ => None,
    }
}

fn merge_method_allowed(value: &serde_json::Value, method: &str) -> bool {
    let key = match method {
        "--merge" => "mergeCommitAllowed",
        "--rebase" => "rebaseMergeAllowed",
        "--squash" => "squashMergeAllowed",
        _ => return false,
    };
    value
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn no_supported_merge_method_error(argv: &[String]) -> GithubError {
    GithubError::CommandFailed {
        argv: argv.to_vec(),
        exit_code: None,
        stderr: "repository does not report any enabled auto-merge method".to_string(),
    }
}
fn auto_merge_cache_lock_error<T>(err: std::sync::PoisonError<T>) -> GithubError {
    GithubError::CacheLock {
        context: "resolving auto-merge method".to_string(),
        error: err.to_string(),
    }
}

fn is_issue_not_found_error(error: &GithubError) -> bool {
    match error {
        GithubError::CommandFailed {
            stderr, exit_code, ..
        } => *exit_code == Some(1) && issue_not_found_stderr(stderr),
        _ => false,
    }
}

fn issue_not_found_stderr(stderr: &str) -> bool {
    let trimmed = stderr.trim();
    trimmed.starts_with("GraphQL: Could not resolve to an Issue with the number of ")
        || trimmed.starts_with("could not resolve to issue or pull request ")
        || trimmed.starts_with("no issue found for ")
}

const MAX_NATIVE_SUB_ISSUE_PAGES: usize = 100;

/// A GitHub issue as needed by discovery/eligibility.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P03
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct GithubIssue {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub labels: Vec<String>,
    pub assignee: Option<String>,
    pub milestone: Option<String>,
    pub body: Option<String>,
}

/// A native GitHub sub-issue with stable ordering metadata when available.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct GithubSubIssue {
    pub issue: GithubIssue,
    pub position: Option<u64>,
    pub source: SubIssueSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub enum SubIssueSource {
    Native,
    FallbackChecklist,
}

/// A GitHub parent issue link for a child issue.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct GithubParentIssue {
    pub issue: GithubIssue,
}

/// Pull request state used by parent orchestration merge waits.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct GithubIssuePrState {
    pub number: u64,
    pub state: String,
    pub merged: bool,
    pub merge_commit_sha: Option<String>,
    pub review_decision: Option<String>,
    pub status_check_rollup: Option<String>,
}

/// Query seam for GitHub issue discovery.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P03
/// @requirement:REQ-DAEMON-DISCOVERY-003
pub trait GithubIssueQuery {
    /// List issues in `repo` filtered by labels and states.
    fn list_issues(
        &self,
        repo: &str,
        include_labels: &[String],
        states: &[String],
    ) -> Result<Vec<GithubIssue>, GithubError>;

    /// Whether an open PR references the given issue number.
    fn has_open_pr_for_issue(&self, repo: &str, number: u64) -> Result<bool, GithubError>;

    /// List milestone titles for a repo (used for semver ordering).
    fn list_milestones(&self, repo: &str) -> Result<Vec<String>, GithubError>;

    /// Fetch one issue by number.
    fn get_issue(&self, _repo: &str, _number: u64) -> Result<Option<GithubIssue>, GithubError> {
        Ok(None)
    }

    /// List native GitHub sub-issues, preserving native order when available.
    fn list_sub_issues(
        &self,
        _repo: &str,
        _number: u64,
    ) -> Result<Vec<GithubSubIssue>, GithubError> {
        Ok(Vec::new())
    }

    /// Return the native parent issue for a child issue, if present.
    fn get_parent_issue(
        &self,
        _repo: &str,
        _number: u64,
    ) -> Result<Option<GithubParentIssue>, GithubError> {
        Ok(None)
    }

    /// Apply an issue label.
    fn add_label(&self, _repo: &str, _number: u64, _label: &str) -> Result<(), GithubError> {
        Ok(())
    }

    /// Remove an issue label.
    fn remove_label(&self, _repo: &str, _number: u64, _label: &str) -> Result<(), GithubError> {
        Ok(())
    }

    /// Find the current PR state for an issue, when a linked PR exists.
    fn pr_state_for_issue(
        &self,
        _repo: &str,
        _number: u64,
    ) -> Result<Option<GithubIssuePrState>, GithubError> {
        Ok(None)
    }

    /// Post a coordination comment on an issue.
    fn comment_issue(&self, _repo: &str, _number: u64, _body: &str) -> Result<(), GithubError> {
        Ok(())
    }

    /// Close an issue after orchestration proves completion.
    fn close_issue(&self, _repo: &str, _number: u64) -> Result<(), GithubError> {
        Ok(())
    }

    /// Request auto-merge for a child PR when explicitly enabled and permitted.
    fn enable_pr_auto_merge(&self, _repo: &str, _pr_number: u64) -> Result<(), GithubError> {
        Ok(())
    }
}

/// `gh`-backed implementation wrapping a [`GithubCommandRunner`].
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P03
pub struct SystemGithubIssueQuery<R: GithubCommandRunner> {
    runner: R,
    auto_merge_methods: Mutex<BTreeMap<String, Arc<AutoMergeMethodCacheEntry>>>,
}

#[derive(Debug, Default)]
struct AutoMergeMethodCacheEntry {
    state: Mutex<AutoMergeMethodCacheState>,
    ready: Condvar,
}

#[derive(Debug, Default)]
enum AutoMergeMethodCacheState {
    #[default]
    Empty,
    Resolving,
    Ready(&'static str),
}

impl<R: GithubCommandRunner> SystemGithubIssueQuery<R> {
    /// Wrap a command runner.
    pub fn new(runner: R) -> Self {
        Self {
            runner,
            auto_merge_methods: Mutex::new(BTreeMap::new()),
        }
    }
}

/// Raw shape of a `gh issue list --json` element.
#[derive(Debug, Deserialize)]
struct RawIssue {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    labels: Vec<RawLabel>,
    #[serde(default)]
    assignees: Vec<RawAssignee>,
    #[serde(default)]
    milestone: Option<RawMilestone>,
}

#[derive(Debug, Deserialize)]
struct RawLabel {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct RawAssignee {
    #[serde(default)]
    login: String,
}
#[derive(Debug, Deserialize)]
struct RawIssueView {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    labels: Vec<RawLabel>,
    #[serde(default)]
    assignees: Vec<RawAssignee>,
    #[serde(default)]
    milestone: Option<RawMilestone>,
    #[serde(default)]
    body: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawIssueBody {
    #[serde(default)]
    body: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawIssuePrReferences {
    #[serde(default, rename = "closedByPullRequestsReferences")]
    closed_by_pull_requests_references: Vec<RawPrState>,
}

include!("github_issues_graphql.rs");
include!("github_issues_subissues.rs");

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

fn graphql_issue_to_issue(issue: GraphqlIssue) -> GithubIssue {
    GithubIssue {
        number: issue.number,
        title: issue.title,
        state: normalize_state(&issue.state),
        labels: issue
            .labels
            .nodes
            .into_iter()
            .map(|label| label.name)
            .collect(),
        assignee: issue.assignees.nodes.into_iter().next().map(|a| a.login),
        milestone: issue.milestone.map(|m| m.title),
        body: issue.body,
    }
}

fn raw_issue_view_to_issue(issue: RawIssueView) -> GithubIssue {
    GithubIssue {
        number: issue.number,
        title: issue.title,
        state: normalize_state(&issue.state),
        labels: issue.labels.into_iter().map(|label| label.name).collect(),
        assignee: issue.assignees.into_iter().next().map(|a| a.login),
        milestone: issue.milestone.map(|m| m.title),
        body: issue.body,
    }
}

fn repo_owner_name(repo: &str) -> Result<(String, String), GithubError> {
    let Some((owner, name)) = repo.split_once('/') else {
        return Err(invalid_repo_error(repo));
    };
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return Err(invalid_repo_error(repo));
    }
    Ok((owner.to_string(), name.to_string()))
}

fn invalid_repo_error(repo: &str) -> GithubError {
    GithubError::CommandFailed {
        argv: vec!["gh".into(), "api".into(), "graphql".into()],
        exit_code: None,
        stderr: format!("repository must be in owner/name form: {repo}"),
    }
}

fn graphql_issue_argv(repo: &str, number: u64, query: &str) -> Result<Vec<String>, GithubError> {
    let (owner, name) = repo_owner_name(repo)?;
    Ok(vec![
        "gh".to_string(),
        "api".to_string(),
        "graphql".to_string(),
        "-f".to_string(),
        format!("query={query}"),
        "-F".to_string(),
        format!("owner={owner}"),
        "-F".to_string(),
        format!("name={name}"),
        "-F".to_string(),
        format!("number={number}"),
    ])
}

fn graphql_response_error(argv: &[String], errors: &[GraphqlError]) -> Option<GithubError> {
    if errors.is_empty() {
        return None;
    }
    Some(GithubError::CommandFailed {
        argv: argv.to_vec(),
        exit_code: None,
        stderr: format!(
            "GitHub GraphQL returned errors: {}",
            errors
                .iter()
                .map(graphql_error_context)
                .collect::<Vec<_>>()
                .join("; ")
        ),
    })
}

fn graphql_error_context(error: &GraphqlError) -> String {
    let mut parts = vec![error.message.clone()];
    if let Some(path) = non_empty_json(&error.path) {
        parts.push(format!("path={path}"));
    }
    if let Some(locations) = non_empty_json(&error.locations) {
        parts.push(format!("locations={locations}"));
    }
    if let Some(extensions) = non_empty_json(&error.extensions) {
        parts.push(format!("extensions={extensions}"));
    }
    parts.join(" ")
}

fn non_empty_json(value: &Option<serde_json::Value>) -> Option<String> {
    let value = value.as_ref()?;
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::Array(values) if values.is_empty() => None,
        serde_json::Value::Object(values) if values.is_empty() => None,
        _ => Some(value.to_string()),
    }
}

pub fn parse_parent_issue_response(json: &str) -> Result<Option<GithubParentIssue>, GithubError> {
    let argv = vec!["gh".into(), "api".into(), "graphql".into()];
    let response: GraphqlParentResponse =
        serde_json::from_str(json).map_err(|e| GithubError::CommandFailed {
            argv: argv.clone(),
            exit_code: None,
            stderr: format!("failed to parse parent-issue GraphQL JSON: {e}"),
        })?;
    if let Some(err) = graphql_response_error(&argv, &response.errors) {
        return Err(err);
    }
    Ok(response
        .data
        .and_then(|data| data.repository.issue)
        .and_then(|issue| issue.parent)
        .map(|parent| GithubParentIssue {
            issue: graphql_issue_to_issue(parent),
        }))
}

pub fn parse_body_issue_references(body: &str) -> Vec<u64> {
    let mut refs = Vec::new();
    let mut seen = BTreeSet::new();
    for line in body.lines().filter(|line| is_subissue_reference_line(line)) {
        collect_issue_references_from_line(line, &mut seen, &mut refs);
    }
    refs
}

fn is_subissue_reference_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if let Some(item) = checklist_item(trimmed) {
        return issue_reference_tokens(item) == 1;
    }
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("sub-issue")
        || lower.starts_with("subissue")
        || lower.starts_with("child issue")
        || lower.starts_with("child:")
        || lower.starts_with("children:")
}

fn parse_body_reference_children<F>(
    repo: &str,
    body: &str,
    mut lookup: F,
) -> Result<Vec<GithubSubIssue>, GithubError>
where
    F: FnMut(u64) -> Result<Option<GithubIssue>, GithubError>,
{
    let mut children = Vec::new();
    for (idx, child_number) in parse_body_issue_references(body).into_iter().enumerate() {
        let Some(issue) = lookup(child_number)? else {
            warn!(repo, child_number, "fallback child issue was not found");
            continue;
        };
        children.push(GithubSubIssue {
            issue,
            position: Some(idx as u64),
            source: SubIssueSource::FallbackChecklist,
        });
    }
    Ok(children)
}

fn checklist_item(trimmed: &str) -> Option<&str> {
    ["- [ ]", "- [x]", "- [X]", "* [ ]", "* [x]", "* [X]"]
        .into_iter()
        .find_map(|prefix| trimmed.strip_prefix(prefix).map(str::trim))
}

fn issue_reference_tokens(item: &str) -> usize {
    item.split(|c: char| c.is_whitespace() || c == ')' || c == ']' || c == ',')
        .filter(|token| token.trim_start_matches(['-', '*', '[']).starts_with('#'))
        .count()
}

fn collect_issue_references_from_line(line: &str, seen: &mut BTreeSet<u64>, refs: &mut Vec<u64>) {
    for token in line.split(|c: char| c.is_whitespace() || c == ')' || c == ']' || c == ',') {
        let Some(rest) = token.trim_start_matches(['-', '*', '[']).strip_prefix('#') else {
            continue;
        };
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            continue;
        }
        if let Ok(number) = digits.parse::<u64>() {
            if seen.insert(number) {
                refs.push(number);
            }
        }
    }
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
    matches!(
        check_conclusion(check).as_deref(),
        Some("success" | "skipped" | "neutral")
    ) || check_state(check).as_deref() == Some("success")
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

#[derive(Debug, Deserialize)]
struct RawMilestone {
    #[serde(default)]
    title: String,
}

/// Parse the JSON returned by `gh issue list --json ...` into `GithubIssue`s.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P03
pub fn parse_issue_list(json: &str) -> Result<Vec<GithubIssue>, GithubError> {
    let raw: Vec<RawIssue> =
        serde_json::from_str(json).map_err(|e| GithubError::CommandFailed {
            argv: vec!["gh".into(), "issue".into(), "list".into()],
            exit_code: None,
            stderr: format!("failed to parse gh issue list JSON: {e}"),
        })?;
    Ok(raw
        .into_iter()
        .map(|r| GithubIssue {
            number: r.number,
            title: r.title,
            state: normalize_state(&r.state),
            labels: r.labels.into_iter().map(|l| l.name).collect(),
            assignee: r.assignees.into_iter().next().map(|a| a.login),
            milestone: r.milestone.map(|m| m.title),
            body: None,
        })
        .collect())
}

/// Normalize `gh`'s uppercase states (e.g. "OPEN") to lowercase.
fn normalize_state(state: &str) -> String {
    state.to_lowercase()
}

/// Build the argv for a `gh issue list` query.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P03
fn build_issue_list_argv(repo: &str, include_labels: &[String], states: &[String]) -> Vec<String> {
    let mut argv = vec![
        "gh".to_string(),
        "issue".to_string(),
        "list".to_string(),
        "--repo".to_string(),
        repo.to_string(),
        "--limit".to_string(),
        "200".to_string(),
        "--json".to_string(),
        "number,title,state,labels,assignees,milestone".to_string(),
    ];
    // `gh issue list --state` accepts a single state; default to open when the
    // caller requests the common single open state, otherwise pass "all" and
    // let downstream filtering narrow by the requested states.
    let state_arg = if states.len() == 1 {
        states[0].clone()
    } else {
        "all".to_string()
    };
    argv.push("--state".to_string());
    argv.push(state_arg);
    for label in include_labels {
        argv.push("--label".to_string());
        argv.push(label.clone());
    }
    argv
}

impl<R: GithubCommandRunner> GithubIssueQuery for SystemGithubIssueQuery<R> {
    fn list_issues(
        &self,
        repo: &str,
        include_labels: &[String],
        states: &[String],
    ) -> Result<Vec<GithubIssue>, GithubError> {
        let argv = build_issue_list_argv(repo, include_labels, states);
        let out = self.runner.run(&argv)?;
        let issues = parse_issue_list(&out)?;
        if states.len() <= 1 {
            return Ok(issues);
        }
        Ok(issues
            .into_iter()
            .filter(|issue| {
                states
                    .iter()
                    .any(|state| state.eq_ignore_ascii_case(&issue.state))
            })
            .collect())
    }

    fn has_open_pr_for_issue(&self, repo: &str, number: u64) -> Result<bool, GithubError> {
        let argv = vec![
            "gh".to_string(),
            "pr".to_string(),
            "list".to_string(),
            "--repo".to_string(),
            repo.to_string(),
            "--state".to_string(),
            "open".to_string(),
            "--search".to_string(),
            format!("issue:{number}"),
            "--json".to_string(),
            "number,state,updatedAt".to_string(),
        ];
        let out = self.runner.run(&argv)?;
        parse_pr_state(&out, &argv).map(|pr| pr.is_some())
    }

    fn list_milestones(&self, repo: &str) -> Result<Vec<String>, GithubError> {
        let argv = vec![
            "gh".to_string(),
            "api".to_string(),
            format!("repos/{repo}/milestones"),
            "--jq".to_string(),
            ".[].title".to_string(),
        ];
        let out = self.runner.run(&argv)?;
        Ok(out
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect())
    }

    fn get_issue(&self, repo: &str, number: u64) -> Result<Option<GithubIssue>, GithubError> {
        let argv = vec![
            "gh".to_string(),
            "issue".to_string(),
            "view".to_string(),
            number.to_string(),
            "--repo".to_string(),
            repo.to_string(),
            "--json".to_string(),
            "number,title,state,labels,assignees,milestone,body".to_string(),
        ];
        let out = match self.runner.run(&argv) {
            Ok(out) => out,
            Err(err) if is_issue_not_found_error(&err) => return Ok(None),
            Err(err) => return Err(err),
        };
        let raw: RawIssueView =
            serde_json::from_str(&out).map_err(|e| GithubError::CommandFailed {
                argv: argv.clone(),
                exit_code: None,
                stderr: format!("failed to parse gh issue view JSON: {e}"),
            })?;
        Ok(Some(raw_issue_view_to_issue(raw)))
    }

    fn list_sub_issues(&self, repo: &str, number: u64) -> Result<Vec<GithubSubIssue>, GithubError> {
        match self.native_sub_issues(repo, number) {
            Ok(children) => Ok(children),
            Err(err) if is_native_sub_issue_fallback_error(&err) => {
                warn!(
                    repo,
                    issue_number = number,
                    error = %err,
                    "falling back to issue body references after native sub-issue lookup failed"
                );
                self.issue_body_reference_children(repo, number)
            }
            Err(err) => Err(err),
        }
    }

    fn get_parent_issue(
        &self,
        repo: &str,
        number: u64,
    ) -> Result<Option<GithubParentIssue>, GithubError> {
        let argv = graphql_issue_argv(repo, number, PARENT_ISSUE_QUERY)?;
        let out = self.runner.run(&argv)?;
        parse_parent_issue_response(&out)
    }

    fn add_label(&self, repo: &str, number: u64, label: &str) -> Result<(), GithubError> {
        let argv = vec![
            "gh".to_string(),
            "issue".to_string(),
            "edit".to_string(),
            number.to_string(),
            "--repo".to_string(),
            repo.to_string(),
            "--add-label".to_string(),
            label.to_string(),
        ];
        self.runner.run(&argv).map(|_| ())
    }

    fn remove_label(&self, repo: &str, number: u64, label: &str) -> Result<(), GithubError> {
        let argv = vec![
            "gh".to_string(),
            "issue".to_string(),
            "edit".to_string(),
            number.to_string(),
            "--repo".to_string(),
            repo.to_string(),
            "--remove-label".to_string(),
            label.to_string(),
        ];
        self.runner.run(&argv).map(|_| ())
    }

    fn pr_state_for_issue(
        &self,
        repo: &str,
        number: u64,
    ) -> Result<Option<GithubIssuePrState>, GithubError> {
        let issue_refs_argv = vec![
            "gh".to_string(),
            "issue".to_string(),
            "view".to_string(),
            number.to_string(),
            "--repo".to_string(),
            repo.to_string(),
            "--json".to_string(),
            "closedByPullRequestsReferences".to_string(),
        ];
        let issue_refs_out = self.runner.run(&issue_refs_argv)?;
        let issue_refs = parse_issue_pr_references(&issue_refs_out, &issue_refs_argv)?;
        if let Some(pr) = select_relevant_pr(issue_refs) {
            return Ok(Some(raw_pr_state_to_issue_pr_state(pr)));
        }

        Ok(None)
    }

    fn comment_issue(&self, repo: &str, number: u64, body: &str) -> Result<(), GithubError> {
        let argv = vec![
            "gh".to_string(),
            "issue".to_string(),
            "comment".to_string(),
            number.to_string(),
            "--repo".to_string(),
            repo.to_string(),
            "--body".to_string(),
            body.to_string(),
        ];
        self.runner.run(&argv).map(|_| ())
    }

    fn close_issue(&self, repo: &str, number: u64) -> Result<(), GithubError> {
        let argv = vec![
            "gh".to_string(),
            "issue".to_string(),
            "close".to_string(),
            number.to_string(),
            "--repo".to_string(),
            repo.to_string(),
        ];
        self.runner.run(&argv).map(|_| ())
    }

    fn enable_pr_auto_merge(&self, repo: &str, pr_number: u64) -> Result<(), GithubError> {
        let merge_method = self.cached_auto_merge_method(repo)?;
        let argv = vec![
            "gh".to_string(),
            "pr".to_string(),
            "merge".to_string(),
            pr_number.to_string(),
            "--repo".to_string(),
            repo.to_string(),
            "--auto".to_string(),
            merge_method.to_string(),
        ];
        self.runner.run(&argv).map(|_| ())
    }
}

impl<R: GithubCommandRunner> SystemGithubIssueQuery<R> {
    fn native_sub_issues(
        &self,
        repo: &str,
        number: u64,
    ) -> Result<Vec<GithubSubIssue>, GithubError> {
        let argv = graphql_issue_argv(repo, number, SUB_ISSUES_QUERY)?;
        let out = self.runner.run(&argv)?;
        let mut page = parse_first_sub_issue_page(&out, &argv)?;
        let mut seen = page
            .children
            .iter()
            .map(|child| child.issue.number)
            .collect::<BTreeSet<_>>();
        let mut page_count = 1;
        while let Some(cursor) = page.next_cursor {
            if page_count >= MAX_NATIVE_SUB_ISSUE_PAGES {
                return Err(native_sub_issue_page_limit_error(repo, number, &cursor));
            }
            let argv = graphql_sub_issue_page_argv(repo, number, &cursor)?;
            let out = self.runner.run(&argv)?;
            page.next_cursor =
                parse_sub_issue_page_response(&out, &mut seen, &mut page.children, &argv)?;
            page_count += 1;
        }
        Ok(page.children)
    }

    fn cached_auto_merge_method(&self, repo: &str) -> Result<&'static str, GithubError> {
        let entry = self.auto_merge_cache_entry(repo)?;
        let mut state = entry.state.lock().map_err(auto_merge_cache_lock_error)?;
        loop {
            match *state {
                AutoMergeMethodCacheState::Ready(method) => return Ok(method),
                AutoMergeMethodCacheState::Empty => {
                    *state = AutoMergeMethodCacheState::Resolving;
                    break;
                }
                AutoMergeMethodCacheState::Resolving => {
                    state = entry
                        .ready
                        .wait(state)
                        .map_err(auto_merge_cache_lock_error)?;
                }
            }
        }
        drop(state);
        let computed = match self.auto_merge_method(repo) {
            Ok(method) => method,
            Err(err) => {
                let mut state = entry.state.lock().map_err(auto_merge_cache_lock_error)?;
                *state = AutoMergeMethodCacheState::Empty;
                entry.ready.notify_all();
                return Err(err);
            }
        };
        let mut state = entry.state.lock().map_err(auto_merge_cache_lock_error)?;
        *state = AutoMergeMethodCacheState::Ready(computed);
        entry.ready.notify_all();
        Ok(computed)
    }

    fn auto_merge_cache_entry(
        &self,
        repo: &str,
    ) -> Result<Arc<AutoMergeMethodCacheEntry>, GithubError> {
        let mut methods = self
            .auto_merge_methods
            .lock()
            .map_err(auto_merge_cache_lock_error)?;
        Ok(methods
            .entry(repo.to_string())
            .or_insert_with(|| Arc::new(AutoMergeMethodCacheEntry::default()))
            .clone())
    }

    fn auto_merge_method(&self, repo: &str) -> Result<&'static str, GithubError> {
        let argv = vec![
            "gh".to_string(),
            "repo".to_string(),
            "view".to_string(),
            repo.to_string(),
            "--json".to_string(),
            "mergeCommitAllowed,rebaseMergeAllowed,squashMergeAllowed,viewerDefaultMergeMethod"
                .to_string(),
        ];
        let out = self.runner.run(&argv)?;
        let value: serde_json::Value =
            serde_json::from_str(&out).map_err(|e| GithubError::CommandFailed {
                argv: argv.clone(),
                exit_code: None,
                stderr: format!("failed to parse gh repo view JSON: {e}"),
            })?;
        if let Some(method) = value
            .get("viewerDefaultMergeMethod")
            .and_then(serde_json::Value::as_str)
            .and_then(default_merge_method_flag)
            .filter(|method| merge_method_allowed(&value, method))
        {
            return Ok(method);
        }
        for method in ["--merge", "--rebase", "--squash"] {
            if merge_method_allowed(&value, method) {
                return Ok(method);
            }
        }
        Err(no_supported_merge_method_error(&argv))
    }

    fn issue_body_reference_children(
        &self,
        repo: &str,
        number: u64,
    ) -> Result<Vec<GithubSubIssue>, GithubError> {
        let argv = vec![
            "gh".to_string(),
            "issue".to_string(),
            "view".to_string(),
            number.to_string(),
            "--repo".to_string(),
            repo.to_string(),
            "--json".to_string(),
            "body".to_string(),
        ];
        let out = self.runner.run(&argv)?;
        let raw: RawIssueBody =
            serde_json::from_str(&out).map_err(|e| GithubError::CommandFailed {
                argv: argv.clone(),
                exit_code: None,
                stderr: format!("failed to parse gh issue body JSON: {e}"),
            })?;
        parse_body_reference_children(
            repo,
            raw.body.as_deref().unwrap_or_default(),
            |child_number| self.get_issue(repo, child_number),
        )
    }
}

#[cfg(test)]
#[path = "github_issues_tests.rs"]
mod github_issues_tests;
