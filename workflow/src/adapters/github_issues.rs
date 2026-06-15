//! GitHub issue query adapter for daemon discovery.
//!
//! Provides a testable seam over `gh` for listing issues, checking for open PRs
//! that reference an issue, and listing milestones. The system implementation
//! shells `gh` via the existing [`GithubCommandRunner`] seam; tests inject a
//! mock runner that returns canned JSON so no network access is required.
//!
//! @plan:PLAN-20260415-DAEMON-DISCOVERY.P03
//! @requirement:REQ-DAEMON-DISCOVERY-003

use serde::Deserialize;

use crate::adapters::github::{GithubCommandRunner, GithubError};

/// A GitHub issue as needed by discovery/eligibility.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P03
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubIssue {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub labels: Vec<String>,
    pub assignee: Option<String>,
    pub milestone: Option<String>,
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
    fn multi_state_uses_all() {
        let argv = build_issue_list_argv("o/r", &[], &["open".into(), "closed".into()]);
        let idx = argv.iter().position(|a| a == "--state").unwrap();
        assert_eq!(argv[idx + 1], "all");
    }
}
