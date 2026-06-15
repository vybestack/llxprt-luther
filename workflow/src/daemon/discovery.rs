//! Daemon issue discovery and eligibility (dry-run core).
//!
//! Lifts the legacy `select_issue` shell logic into first-class, testable Rust:
//! given a [`DiscoveryConfig`] and a [`GithubIssueQuery`], compute which issues
//! are eligible for a workflow run and, for the rest, *why* they were skipped.
//!
//! @plan:PLAN-20260415-DAEMON-DISCOVERY.P04
//! @requirement:REQ-DAEMON-DISCOVERY-004

use rusqlite::Connection;

use crate::adapters::github::GithubError;
use crate::adapters::github_issues::{GithubIssue, GithubIssueQuery};
use crate::persistence::leases::get_lease_for_issue;
use crate::workflow::schema::DiscoveryConfig;

/// Reason an issue was deemed ineligible.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P04
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    MissingRequiredLabel(String),
    HasExcludedLabel(String),
    WrongState(String),
    AssigneeMismatch(String),
    HasActiveLease,
    HasOpenPr,
    ConcurrencyLimitReached,
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkipReason::MissingRequiredLabel(l) => write!(f, "missing required label '{l}'"),
            SkipReason::HasExcludedLabel(l) => write!(f, "has excluded label '{l}'"),
            SkipReason::WrongState(s) => write!(f, "wrong state '{s}'"),
            SkipReason::AssigneeMismatch(a) => write!(f, "assignee mismatch (wanted '{a}')"),
            SkipReason::HasActiveLease => write!(f, "issue already has an active lease"),
            SkipReason::HasOpenPr => write!(f, "issue already has an open PR"),
            SkipReason::ConcurrencyLimitReached => {
                write!(f, "per-config concurrency limit reached")
            }
        }
    }
}

/// A static identifier for the skip reason, for JSON output.
impl SkipReason {
    /// Machine-readable reason code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            SkipReason::MissingRequiredLabel(_) => "missing_required_label",
            SkipReason::HasExcludedLabel(_) => "has_excluded_label",
            SkipReason::WrongState(_) => "wrong_state",
            SkipReason::AssigneeMismatch(_) => "assignee_mismatch",
            SkipReason::HasActiveLease => "has_active_lease",
            SkipReason::HasOpenPr => "has_open_pr",
            SkipReason::ConcurrencyLimitReached => "concurrency_limit_reached",
        }
    }
}

/// Result of a discovery pass: eligible issues and skipped issues with reasons.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P04
#[derive(Debug, Clone, Default)]
pub struct DiscoveryResult {
    pub eligible: Vec<GithubIssue>,
    pub skipped: Vec<(GithubIssue, SkipReason)>,
}

/// Check label/state/assignee filters; return the first failing skip reason.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P04
fn static_filter(cfg: &DiscoveryConfig, issue: &GithubIssue) -> Option<SkipReason> {
    for required in &cfg.include_labels {
        if !issue.labels.contains(required) {
            return Some(SkipReason::MissingRequiredLabel(required.clone()));
        }
    }
    for excluded in &cfg.exclude_labels {
        if issue.labels.contains(excluded) {
            return Some(SkipReason::HasExcludedLabel(excluded.clone()));
        }
    }
    if !cfg.issue_states.is_empty() && !cfg.issue_states.contains(&issue.state) {
        return Some(SkipReason::WrongState(issue.state.clone()));
    }
    if let Some(wanted) = &cfg.assignee_filter {
        if !assignee_matches(wanted, issue.assignee.as_deref()) {
            return Some(SkipReason::AssigneeMismatch(wanted.clone()));
        }
    }
    None
}

/// Whether an issue's assignee satisfies the filter. `""` means unassigned.
fn assignee_matches(wanted: &str, actual: Option<&str>) -> bool {
    if wanted.is_empty() {
        actual.is_none()
    } else {
        actual == Some(wanted)
    }
}

/// Parse a milestone title like `v1.2.3` (or `1.2.3`) into a sortable tuple.
/// Non-semver titles sort last.
fn semver_key(milestone: Option<&str>) -> (u8, u64, u64, u64) {
    let Some(raw) = milestone else {
        return (1, 0, 0, 0);
    };
    let trimmed = raw.trim_start_matches('v').trim_start_matches('V');
    let mut parts = trimmed.split('.').map(|p| p.parse::<u64>().ok());
    match (parts.next(), parts.next(), parts.next()) {
        (Some(Some(a)), b, c) => (0, a, b.flatten().unwrap_or(0), c.flatten().unwrap_or(0)),
        _ => (1, 0, 0, 0),
    }
}

/// Order issues by milestone (semver) then ascending issue number, matching the
/// legacy `select_issue` behavior.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P04
fn order_issues(cfg: &DiscoveryConfig, issues: &mut [GithubIssue]) {
    let by_semver = cfg.milestone_order.as_deref() != Some("none");
    issues.sort_by(|a, b| {
        if by_semver {
            let ka = semver_key(a.milestone.as_deref());
            let kb = semver_key(b.milestone.as_deref());
            ka.cmp(&kb).then(a.number.cmp(&b.number))
        } else {
            a.number.cmp(&b.number)
        }
    });
}

/// Discover eligible issues for a config.
///
/// Pipeline: list issues -> static label/state/assignee filter -> skip if an
/// active lease exists -> skip if an open PR references the issue -> apply the
/// per-config concurrency limit (`active_count` already-running plus newly
/// eligible). Remaining over-limit issues are reported as
/// [`SkipReason::ConcurrencyLimitReached`].
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P04
/// @requirement:REQ-DAEMON-DISCOVERY-004
pub fn discover(
    cfg: &DiscoveryConfig,
    q: &dyn GithubIssueQuery,
    conn: &Connection,
    active_count: usize,
) -> Result<DiscoveryResult, GithubError> {
    let repo = cfg.repo.as_deref().unwrap_or("");
    let mut issues = q.list_issues(repo, &cfg.include_labels, &cfg.issue_states)?;
    order_issues(cfg, &mut issues);

    let max = cfg.max_concurrent_runs.unwrap_or(1) as usize;
    let mut result = DiscoveryResult::default();

    for issue in issues {
        if let Some(reason) = static_filter(cfg, &issue) {
            result.skipped.push((issue, reason));
            continue;
        }
        if let Some(reason) = dynamic_skip(cfg, q, conn, &issue, repo)? {
            result.skipped.push((issue, reason));
            continue;
        }
        if result.eligible.len() + active_count >= max {
            result
                .skipped
                .push((issue, SkipReason::ConcurrencyLimitReached));
            continue;
        }
        result.eligible.push(issue);
    }

    Ok(result)
}

/// Check the dynamic (DB/network) skip reasons: active lease, then open PR.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P04
fn dynamic_skip(
    _cfg: &DiscoveryConfig,
    q: &dyn GithubIssueQuery,
    conn: &Connection,
    issue: &GithubIssue,
    repo: &str,
) -> Result<Option<SkipReason>, GithubError> {
    if let Ok(Some(lease)) = get_lease_for_issue(conn, repo, issue.number) {
        if lease.status.is_active() {
            return Ok(Some(SkipReason::HasActiveLease));
        }
    }
    if q.has_open_pr_for_issue(repo, issue.number)? {
        return Ok(Some(SkipReason::HasOpenPr));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::leases::{init_leases_table, try_claim};

    fn cfg() -> DiscoveryConfig {
        DiscoveryConfig {
            enabled: true,
            repo: Some("o/r".to_string()),
            include_labels: vec!["OK for Luther".to_string()],
            exclude_labels: vec!["Luther working".to_string()],
            issue_states: vec!["open".to_string()],
            assignee_filter: None,
            milestone_order: Some("semver".to_string()),
            max_concurrent_runs: Some(2),
            poll_interval_secs: Some(300),
        }
    }

    fn issue(number: u64, labels: &[&str]) -> GithubIssue {
        GithubIssue {
            number,
            title: format!("Issue {number}"),
            state: "open".to_string(),
            labels: labels.iter().map(|s| (*s).to_string()).collect(),
            assignee: None,
            milestone: None,
        }
    }

    /// Mock query returning preset issues and PR answers.
    struct MockQuery {
        issues: Vec<GithubIssue>,
        open_pr_for: Vec<u64>,
    }

    impl GithubIssueQuery for MockQuery {
        fn list_issues(
            &self,
            _repo: &str,
            _labels: &[String],
            _states: &[String],
        ) -> Result<Vec<GithubIssue>, GithubError> {
            Ok(self.issues.clone())
        }
        fn has_open_pr_for_issue(&self, _repo: &str, number: u64) -> Result<bool, GithubError> {
            Ok(self.open_pr_for.contains(&number))
        }
        fn list_milestones(&self, _repo: &str) -> Result<Vec<String>, GithubError> {
            Ok(vec![])
        }
    }

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        init_leases_table(&c).unwrap();
        c
    }

    #[test]
    fn missing_required_label_skipped() {
        let q = MockQuery {
            issues: vec![issue(1, &[])],
            open_pr_for: vec![],
        };
        let r = discover(&cfg(), &q, &conn(), 0).unwrap();
        assert!(r.eligible.is_empty());
        assert_eq!(
            r.skipped[0].1,
            SkipReason::MissingRequiredLabel("OK for Luther".to_string())
        );
    }

    #[test]
    fn excluded_label_skipped() {
        let q = MockQuery {
            issues: vec![issue(1, &["OK for Luther", "Luther working"])],
            open_pr_for: vec![],
        };
        let r = discover(&cfg(), &q, &conn(), 0).unwrap();
        assert_eq!(
            r.skipped[0].1,
            SkipReason::HasExcludedLabel("Luther working".to_string())
        );
    }

    #[test]
    fn wrong_state_skipped() {
        let mut i = issue(1, &["OK for Luther"]);
        i.state = "closed".to_string();
        let q = MockQuery {
            issues: vec![i],
            open_pr_for: vec![],
        };
        let r = discover(&cfg(), &q, &conn(), 0).unwrap();
        assert_eq!(r.skipped[0].1, SkipReason::WrongState("closed".to_string()));
    }

    #[test]
    fn assignee_mismatch_skipped() {
        let mut c = cfg();
        c.assignee_filter = Some("acoliver".to_string());
        let q = MockQuery {
            issues: vec![issue(1, &["OK for Luther"])],
            open_pr_for: vec![],
        };
        let r = discover(&c, &q, &conn(), 0).unwrap();
        assert_eq!(
            r.skipped[0].1,
            SkipReason::AssigneeMismatch("acoliver".to_string())
        );
    }

    #[test]
    fn active_lease_skipped() {
        let c = conn();
        try_claim(&c, "o/r", 1, "cfg").unwrap();
        let q = MockQuery {
            issues: vec![issue(1, &["OK for Luther"])],
            open_pr_for: vec![],
        };
        let r = discover(&cfg(), &q, &c, 0).unwrap();
        assert_eq!(r.skipped[0].1, SkipReason::HasActiveLease);
    }

    #[test]
    fn open_pr_skipped() {
        let q = MockQuery {
            issues: vec![issue(1, &["OK for Luther"])],
            open_pr_for: vec![1],
        };
        let r = discover(&cfg(), &q, &conn(), 0).unwrap();
        assert_eq!(r.skipped[0].1, SkipReason::HasOpenPr);
    }

    #[test]
    fn concurrency_limit_reached() {
        let q = MockQuery {
            issues: vec![
                issue(1, &["OK for Luther"]),
                issue(2, &["OK for Luther"]),
                issue(3, &["OK for Luther"]),
            ],
            open_pr_for: vec![],
        };
        // max=2, active=0 => 2 eligible, 1 over limit.
        let r = discover(&cfg(), &q, &conn(), 0).unwrap();
        assert_eq!(r.eligible.len(), 2);
        assert_eq!(r.skipped[0].1, SkipReason::ConcurrencyLimitReached);
    }

    #[test]
    fn active_count_reduces_available_slots() {
        let q = MockQuery {
            issues: vec![issue(1, &["OK for Luther"]), issue(2, &["OK for Luther"])],
            open_pr_for: vec![],
        };
        // max=2, active=1 => only 1 eligible.
        let r = discover(&cfg(), &q, &conn(), 1).unwrap();
        assert_eq!(r.eligible.len(), 1);
        assert_eq!(r.skipped.len(), 1);
    }

    #[test]
    fn happy_path_orders_by_milestone_then_number() {
        let mut i1 = issue(10, &["OK for Luther"]);
        i1.milestone = Some("v2.0.0".to_string());
        let mut i2 = issue(20, &["OK for Luther"]);
        i2.milestone = Some("v1.0.0".to_string());
        let mut i3 = issue(5, &["OK for Luther"]);
        i3.milestone = Some("v1.0.0".to_string());
        let mut c = cfg();
        c.max_concurrent_runs = Some(10);
        let q = MockQuery {
            issues: vec![i1, i2, i3],
            open_pr_for: vec![],
        };
        let r = discover(&c, &q, &conn(), 0).unwrap();
        let order: Vec<u64> = r.eligible.iter().map(|i| i.number).collect();
        // v1.0.0 issues (5, 20) before v2.0.0 (10); within milestone by number.
        assert_eq!(order, vec![5, 20, 10]);
    }

    #[test]
    fn unassigned_filter_matches_none() {
        assert!(assignee_matches("", None));
        assert!(!assignee_matches("", Some("x")));
        assert!(assignee_matches("x", Some("x")));
        assert!(!assignee_matches("x", None));
    }
}
