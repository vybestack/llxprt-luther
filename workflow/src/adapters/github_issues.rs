//! GitHub issue query adapter for daemon discovery.
//!
//! Provides a testable seam over `gh` for listing issues, checking for open PRs
//! that reference an issue, and listing milestones. The system implementation
//! shells `gh` via the existing [`GithubCommandRunner`] seam; tests inject a
//! mock runner that returns canned JSON so no network access is required.
//!
//! @plan:PLAN-20260415-DAEMON-DISCOVERY.P03
//! @requirement:REQ-DAEMON-DISCOVERY-003

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::adapters::github::{GithubCommandRunner, GithubError};

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
}

impl<R: GithubCommandRunner> SystemGithubIssueQuery<R> {
    /// Wrap a command runner.
    pub fn new(runner: R) -> Self {
        Self { runner }
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

#[derive(Debug, Deserialize)]
struct GraphqlResponse {
    data: GraphqlData,
}

#[derive(Debug, Deserialize)]
struct GraphqlData {
    repository: GraphqlRepository,
}

#[derive(Debug, Deserialize)]
struct GraphqlRepository {
    issue: Option<GraphqlIssue>,
}

#[derive(Debug, Deserialize)]
struct GraphqlParentResponse {
    data: GraphqlParentData,
}

#[derive(Debug, Deserialize)]
struct GraphqlParentData {
    repository: GraphqlParentRepository,
}

#[derive(Debug, Deserialize)]
struct GraphqlParentRepository {
    issue: Option<GraphqlParentLinkIssue>,
}

#[derive(Debug, Deserialize)]
struct GraphqlParentLinkIssue {
    #[serde(default)]
    parent: Option<GraphqlIssue>,
}

#[derive(Debug, Deserialize)]
struct GraphqlIssue {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    labels: GraphqlNodeList<GraphqlLabel>,
    #[serde(default)]
    assignees: GraphqlNodeList<GraphqlAssignee>,
    #[serde(default)]
    milestone: Option<GraphqlMilestone>,
    #[serde(default, rename = "subIssues")]
    sub_issues: GraphqlSubIssueConnection,
}

#[derive(Debug, Default, Deserialize)]
struct GraphqlSubIssueConnection {
    #[serde(default)]
    edges: Vec<GraphqlSubIssueEdge>,
    #[serde(default, rename = "pageInfo")]
    page_info: GraphqlPageInfo,
}

#[derive(Debug, Deserialize)]
struct GraphqlSubIssueEdge {
    node: GraphqlIssue,
}

#[derive(Debug, Default, Deserialize)]
struct GraphqlPageInfo {
    #[serde(default, rename = "hasNextPage")]
    has_next_page: bool,
}

#[derive(Debug, Deserialize)]
struct GraphqlNodeList<T> {
    #[serde(default)]
    nodes: Vec<T>,
}

impl<T> Default for GraphqlNodeList<T> {
    fn default() -> Self {
        Self { nodes: Vec::new() }
    }
}

#[derive(Debug, Deserialize, Default)]
struct GraphqlLabel {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize, Default)]
struct GraphqlAssignee {
    #[serde(default)]
    login: String,
}

#[derive(Debug, Deserialize)]
struct GraphqlMilestone {
    #[serde(default)]
    title: String,
}

#[derive(Debug, Deserialize)]
struct RawPrState {
    number: u64,
    #[serde(default)]
    state: String,
    #[serde(default)]
    merged: bool,
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
        body: None,
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

const SUB_ISSUES_QUERY: &str = "query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name){issue(number:$number){number title state labels(first:50){nodes{name}} assignees(first:10){nodes{login}} milestone{title} subIssues(first:100){edges{node{number title state labels(first:50){nodes{name}} assignees(first:10){nodes{login}} milestone{title}}} pageInfo{hasNextPage}}}}}";

const PARENT_ISSUE_QUERY: &str = "query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name){issue(number:$number){parent{number title state labels(first:50){nodes{name}} assignees(first:10){nodes{login}} milestone{title}}}}}";

pub fn parse_sub_issue_response(json: &str) -> Result<Vec<GithubSubIssue>, GithubError> {
    let response: GraphqlResponse =
        serde_json::from_str(json).map_err(|e| GithubError::CommandFailed {
            argv: vec!["gh".into(), "api".into(), "graphql".into()],
            exit_code: None,
            stderr: format!("failed to parse sub-issue GraphQL JSON: {e}"),
        })?;
    let Some(issue) = response.data.repository.issue else {
        return Ok(Vec::new());
    };
    if issue.sub_issues.page_info.has_next_page {
        return Err(GithubError::CommandFailed {
            argv: vec!["gh".into(), "api".into(), "graphql".into()],
            exit_code: None,
            stderr: "parent issue has more than 100 sub-issues; pagination is required".to_string(),
        });
    }
    let mut seen = BTreeSet::new();
    let mut children = Vec::new();
    for (idx, edge) in issue.sub_issues.edges.into_iter().enumerate() {
        if seen.insert(edge.node.number) {
            children.push(GithubSubIssue {
                issue: graphql_issue_to_issue(edge.node),
                position: Some(idx as u64),
                source: SubIssueSource::Native,
            });
        }
    }
    Ok(children)
}

pub fn parse_parent_issue_response(json: &str) -> Result<Option<GithubParentIssue>, GithubError> {
    let response: GraphqlParentResponse =
        serde_json::from_str(json).map_err(|e| GithubError::CommandFailed {
            argv: vec!["gh".into(), "api".into(), "graphql".into()],
            exit_code: None,
            stderr: format!("failed to parse parent-issue GraphQL JSON: {e}"),
        })?;
    Ok(response
        .data
        .repository
        .issue
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
    if trimmed.starts_with("- [") || trimmed.starts_with("* [") {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("sub-issue")
        || lower.starts_with("subissue")
        || lower.starts_with("child issue")
        || lower.starts_with("child:")
        || lower.starts_with("children:")
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
    Ok(prs.into_iter().next().map(raw_pr_state_to_issue_pr_state))
}

fn parse_issue_pr_references(
    json: &str,
    argv: &[String],
) -> Result<Option<GithubIssuePrState>, GithubError> {
    let refs: RawIssuePrReferences =
        serde_json::from_str(json).map_err(|e| GithubError::CommandFailed {
            argv: argv.to_vec(),
            exit_code: None,
            stderr: format!("failed to parse gh issue PR references JSON: {e}"),
        })?;
    Ok(refs
        .closed_by_pull_requests_references
        .into_iter()
        .next()
        .map(raw_pr_state_to_issue_pr_state))
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
    if checks.is_empty() {
        return None;
    }
    if checks.iter().any(status_check_failed) {
        return Some("failed".to_string());
    }
    if checks.iter().all(status_check_passed) {
        return Some("passed".to_string());
    }
    Some("pending".to_string())
}

fn status_check_failed(check: &RawStatusCheck) -> bool {
    matches!(
        check.conclusion.as_deref(),
        Some("FAILURE" | "TIMED_OUT" | "ACTION_REQUIRED")
    ) || matches!(check.state.as_deref(), Some("FAILURE" | "ERROR"))
}

fn status_check_passed(check: &RawStatusCheck) -> bool {
    check.conclusion.as_deref() == Some("SUCCESS") || check.state.as_deref() == Some("SUCCESS")
}

#[derive(Debug, Deserialize)]
struct RawMilestone {
    #[serde(default)]
    title: String,
}

#[derive(Debug, Deserialize)]
struct RawPr {
    #[allow(dead_code)]
    number: u64,
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
        parse_issue_list(&out)
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
            "number".to_string(),
        ];
        let out = self.runner.run(&argv)?;
        let prs: Vec<RawPr> =
            serde_json::from_str(&out).map_err(|e| GithubError::CommandFailed {
                argv: argv.clone(),
                exit_code: None,
                stderr: format!("failed to parse gh pr list JSON: {e}"),
            })?;
        Ok(!prs.is_empty())
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
        let out = self.runner.run(&argv)?;
        let raw: RawIssueView =
            serde_json::from_str(&out).map_err(|e| GithubError::CommandFailed {
                argv: argv.clone(),
                exit_code: None,
                stderr: format!("failed to parse gh issue view JSON: {e}"),
            })?;
        Ok(Some(raw_issue_view_to_issue(raw)))
    }

    fn list_sub_issues(&self, repo: &str, number: u64) -> Result<Vec<GithubSubIssue>, GithubError> {
        let argv = graphql_issue_argv(repo, number, SUB_ISSUES_QUERY)?;
        let out = self.runner.run(&argv)?;
        let children = parse_sub_issue_response(&out)?;
        if children.is_empty() {
            return self.issue_body_reference_children(repo, number);
        }
        Ok(children)
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
        if let Some(pr) = parse_issue_pr_references(&issue_refs_out, &issue_refs_argv)? {
            return Ok(Some(pr));
        }

        let search_argv = vec![
            "gh".to_string(),
            "pr".to_string(),
            "list".to_string(),
            "--repo".to_string(),
            repo.to_string(),
            "--state".to_string(),
            "all".to_string(),
            "--search".to_string(),
            format!("issue:{number}"),
            "--json".to_string(),
            "number,state,merged,mergeCommit,reviewDecision,statusCheckRollup".to_string(),
        ];

        let search_out = self.runner.run(&search_argv)?;
        parse_pr_state(&search_out, &search_argv)
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
        let argv = vec![
            "gh".to_string(),
            "pr".to_string(),
            "merge".to_string(),
            pr_number.to_string(),
            "--repo".to_string(),
            repo.to_string(),
            "--auto".to_string(),
            "--squash".to_string(),
        ];
        self.runner.run(&argv).map(|_| ())
    }
}

impl<R: GithubCommandRunner> SystemGithubIssueQuery<R> {
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
        let mut children = Vec::new();
        for (idx, child_number) in
            parse_body_issue_references(raw.body.as_deref().unwrap_or_default())
                .into_iter()
                .enumerate()
        {
            if let Some(issue) = self.get_issue(repo, child_number)? {
                children.push(GithubSubIssue {
                    issue,
                    position: Some(idx as u64),
                    source: SubIssueSource::FallbackChecklist,
                });
            }
        }
        Ok(children)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Records argv and returns canned per-call results.
    struct MockRunner {
        results: RefCell<Vec<Result<String, GithubError>>>,
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl MockRunner {
        fn new(results: Vec<Result<String, GithubError>>) -> Self {
            Self {
                results: RefCell::new(results),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl GithubCommandRunner for MockRunner {
        fn run(&self, argv: &[String]) -> Result<String, GithubError> {
            self.calls.borrow_mut().push(argv.to_vec());
            if self.results.borrow().is_empty() {
                return Ok("[]".to_string());
            }
            self.results.borrow_mut().remove(0)
        }
    }

    const SAMPLE: &str = r#"[
        {"number":12,"title":"Fix bug","state":"OPEN",
         "labels":[{"name":"OK for Luther"}],
         "assignees":[{"login":"acoliver"}],
         "milestone":{"title":"v1.2.0"}},
        {"number":5,"title":"No labels","state":"OPEN",
         "labels":[],"assignees":[],"milestone":null}
    ]"#;

    #[test]
    fn parse_issue_list_maps_fields() {
        let issues = parse_issue_list(SAMPLE).unwrap();
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].number, 12);
        assert_eq!(issues[0].state, "open");
        assert_eq!(issues[0].labels, vec!["OK for Luther"]);
        assert_eq!(issues[0].assignee.as_deref(), Some("acoliver"));
        assert_eq!(issues[0].milestone.as_deref(), Some("v1.2.0"));
        assert_eq!(issues[1].assignee, None);
        assert_eq!(issues[1].milestone, None);
    }

    #[test]
    fn list_issues_builds_correct_argv() {
        let runner = MockRunner::new(vec![Ok(SAMPLE.to_string())]);
        let q = SystemGithubIssueQuery::new(runner);
        let issues = q
            .list_issues("o/r", &["OK for Luther".to_string()], &["open".to_string()])
            .unwrap();
        assert_eq!(issues.len(), 2);
        let calls = q.runner.calls.borrow();
        let argv = &calls[0];
        assert!(argv.contains(&"--repo".to_string()));
        assert!(argv.contains(&"o/r".to_string()));
        assert!(argv.contains(&"--label".to_string()));
        assert!(argv.contains(&"OK for Luther".to_string()));
        assert!(argv.contains(&"--state".to_string()));
        assert!(argv.contains(&"open".to_string()));
    }

    #[test]
    fn has_open_pr_true_and_false() {
        let runner_true = MockRunner::new(vec![Ok(r#"[{"number":99}]"#.to_string())]);
        let q_true = SystemGithubIssueQuery::new(runner_true);
        assert!(q_true.has_open_pr_for_issue("o/r", 12).unwrap());

        let runner_false = MockRunner::new(vec![Ok("[]".to_string())]);
        let q_false = SystemGithubIssueQuery::new(runner_false);
        assert!(!q_false.has_open_pr_for_issue("o/r", 12).unwrap());
    }

    #[test]
    fn has_open_pr_builds_search_argv() {
        let runner = MockRunner::new(vec![Ok("[]".to_string())]);
        let q = SystemGithubIssueQuery::new(runner);
        let _ = q.has_open_pr_for_issue("o/r", 7).unwrap();
        let calls = q.runner.calls.borrow();
        let argv = &calls[0];
        assert!(argv.contains(&"--search".to_string()));
        assert!(argv.contains(&"issue:7".to_string()));
    }

    #[test]
    fn list_milestones_parses_lines() {
        let runner = MockRunner::new(vec![Ok("v1.0.0\nv1.1.0\n\nv2.0.0\n".to_string())]);
        let q = SystemGithubIssueQuery::new(runner);
        let ms = q.list_milestones("o/r").unwrap();
        assert_eq!(ms, vec!["v1.0.0", "v1.1.0", "v2.0.0"]);
    }

    #[test]
    fn repo_owner_name_rejects_invalid_repo_slug() {
        assert!(repo_owner_name("repo-only").is_err());
        assert!(repo_owner_name("owner/").is_err());
        assert!(repo_owner_name("/repo").is_err());
        assert!(repo_owner_name("owner/repo/extra").is_err());
    }

    #[test]
    fn parse_sub_issue_response_rejects_truncated_connection() {
        let json = r#"{
            "data": {"repository": {"issue": {
                "number": 1,
                "subIssues": {
                    "edges": [],
                    "pageInfo": {"hasNextPage": true}
                }
            }}}
        }"#;

        assert!(parse_sub_issue_response(json).is_err());
    }

    #[test]
    fn parse_parent_issue_response_accepts_parent_only_query_shape() {
        let json = r#"{
            "data": {"repository": {"issue": {
                "parent": {
                    "number": 60,
                    "title": "Parent coordination issue",
                    "state": "OPEN",
                    "labels": {"nodes": [{"name": "OK for Luther"}, {"name": "Luther working"}]},
                    "assignees": {"nodes": [{"login": "acoliver"}]},
                    "milestone": {"title": "v1.0.0"}
                }
            }}}
        }"#;

        let parent = parse_parent_issue_response(json).unwrap().unwrap();

        assert_eq!(parent.issue.number, 60);
        assert_eq!(parent.issue.title, "Parent coordination issue");
        assert_eq!(parent.issue.state, "open");
        assert_eq!(
            parent.issue.labels,
            vec!["OK for Luther".to_string(), "Luther working".to_string()]
        );
        assert_eq!(parent.issue.assignee.as_deref(), Some("acoliver"));
        assert_eq!(parent.issue.milestone.as_deref(), Some("v1.0.0"));
    }
    #[test]
    fn list_sub_issues_empty_native_empty_body_returns_empty_children() {
        let native_empty = r#"{
            "data": {"repository": {"issue": {
                "number": 1,
                "subIssues": {"edges": [], "pageInfo": {"hasNextPage": false}}
            }}}
        }"#;
        let runner = MockRunner::new(vec![
            Ok(native_empty.to_string()),
            Ok(r#"{"body":"No child references here."}"#.to_string()),
        ]);
        let q = SystemGithubIssueQuery::new(runner);

        let children = q.list_sub_issues("o/r", 1).unwrap();

        assert!(children.is_empty());
    }

    #[test]
    fn pr_state_for_issue_prefers_closing_pr_references() {
        let issue_refs = r#"{
            "closedByPullRequestsReferences": [{
                "number": 17,
                "state": "MERGED",
                "merged": true,
                "mergeCommit": {"oid": "abc123"},
                "reviewDecision": "APPROVED",
                "statusCheckRollup": [{"conclusion": "SUCCESS"}]
            }]
        }"#;
        let runner = MockRunner::new(vec![Ok(issue_refs.to_string())]);
        let q = SystemGithubIssueQuery::new(runner);

        let pr = q.pr_state_for_issue("o/r", 7).unwrap().unwrap();

        assert_eq!(pr.number, 17);
        assert!(pr.merged);
        assert_eq!(pr.merge_commit_sha.as_deref(), Some("abc123"));
        assert_eq!(pr.review_decision.as_deref(), Some("approved"));
        assert_eq!(pr.status_check_rollup.as_deref(), Some("passed"));
        let calls = q.runner.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].contains(&"closedByPullRequestsReferences".to_string()));
    }

    #[test]
    fn pr_state_for_issue_falls_back_to_search_when_issue_has_no_closing_pr() {
        let issue_refs = r#"{"closedByPullRequestsReferences": []}"#;
        let pr_search = r#"[{
            "number": 19,
            "state": "OPEN",
            "merged": false,
            "mergeCommit": null,
            "reviewDecision": null,
            "statusCheckRollup": [{"conclusion": "FAILURE"}]
        }]"#;
        let runner = MockRunner::new(vec![Ok(issue_refs.to_string()), Ok(pr_search.to_string())]);
        let q = SystemGithubIssueQuery::new(runner);

        let pr = q.pr_state_for_issue("o/r", 7).unwrap().unwrap();

        assert_eq!(pr.number, 19);
        assert_eq!(pr.state, "open");
        assert!(!pr.merged);
        assert_eq!(pr.status_check_rollup.as_deref(), Some("failed"));
        let calls = q.runner.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert!(calls[1].contains(&"issue:7".to_string()));
    }

    #[test]
    fn status_check_rollup_accepts_commit_status_states() {
        let argv = vec!["gh".to_string()];
        let passed = parse_pr_state(
            r#"[{"number":20,"state":"OPEN","merged":false,"statusCheckRollup":[{"state":"SUCCESS"}]}]"#,
            &argv,
        )
        .unwrap()
        .unwrap();
        assert_eq!(passed.status_check_rollup.as_deref(), Some("passed"));

        let failed = parse_pr_state(
            r#"[{"number":20,"state":"OPEN","merged":false,"statusCheckRollup":[{"state":"ERROR"}]}]"#,
            &argv,
        )
        .unwrap()
        .unwrap();
        assert_eq!(failed.status_check_rollup.as_deref(), Some("failed"));
    }

    #[test]
    fn pr_state_for_issue_reports_absent_pr_when_no_reference_or_search_hit() {
        let runner = MockRunner::new(vec![
            Ok(r#"{"closedByPullRequestsReferences": []}"#.to_string()),
            Ok("[]".to_string()),
        ]);
        let q = SystemGithubIssueQuery::new(runner);

        assert!(q.pr_state_for_issue("o/r", 7).unwrap().is_none());
    }

    #[test]
    fn pr_state_parser_distinguishes_closed_unmerged_pr() {
        let argv = vec!["gh".to_string()];
        let pr = parse_pr_state(
            r#"[{"number":20,"state":"CLOSED","merged":false,"statusCheckRollup":[]}]"#,
            &argv,
        )
        .unwrap()
        .unwrap();

        assert_eq!(pr.state, "closed");
        assert!(!pr.merged);
    }

    #[test]
    fn body_reference_fallback_requires_checklist_or_subissue_context() {
        let body = "This ordinary issue mentions #12 in discussion.\n- [ ] #13 child work\nSub-issue: #14\nchild issue #15";

        assert_eq!(parse_body_issue_references(body), vec![13, 14, 15]);
    }

    #[test]
    fn sub_issues_query_uses_only_issue_edge_schema_fields() {
        assert!(SUB_ISSUES_QUERY.contains("subIssues(first:100){edges{node{"));
        assert!(!SUB_ISSUES_QUERY.contains("subIssues(first:100){nodes"));
        assert!(!SUB_ISSUES_QUERY.contains("edges{position"));
        assert!(!SUB_ISSUES_QUERY.contains(" position"));
    }

    #[test]
    fn parse_sub_issue_response_derives_positions_from_connection_order() {
        let json = r#"{
            "data": {"repository": {"issue": {
                "number": 1,
                "subIssues": {
                    "edges": [
                        {"node": {"number": 42, "title": "Second", "state": "OPEN"}},
                        {"node": {"number": 7, "title": "First", "state": "OPEN"}}
                    ],
                    "pageInfo": {"hasNextPage": false}
                }
            }}}
        }"#;

        let children = parse_sub_issue_response(json).unwrap();
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].issue.number, 42);
        assert_eq!(children[0].position, Some(0));
        assert_eq!(children[1].issue.number, 7);
        assert_eq!(children[1].position, Some(1));
    }

    #[test]
    fn multi_state_uses_all() {
        let argv = build_issue_list_argv("o/r", &[], &["open".into(), "closed".into()]);
        let idx = argv.iter().position(|a| a == "--state").unwrap();
        assert_eq!(argv[idx + 1], "all");
    }
}
