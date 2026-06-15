//! Integration tests for daemon issue discovery (issue #49).
//!
//! Exercises the discovery pipeline end-to-end against a mock
//! [`GithubIssueQuery`] and a temp in-memory SQLite database, asserting the
//! eligible/skipped partitioning including the open-PR and active-lease skips.
//!
//! @plan:PLAN-20260415-DAEMON-DISCOVERY.P04
//! @requirement:REQ-DAEMON-DISCOVERY-004
use luther_workflow::adapters::github::GithubError;
use luther_workflow::adapters::github_issues::{GithubIssue, GithubIssueQuery};
use luther_workflow::daemon::discovery::{discover, SkipReason};
use luther_workflow::persistence::leases::{
    create_lease, init_leases_table, IssueLease, LeaseStatus,
};
use luther_workflow::workflow::schema::DiscoveryConfig;
use rusqlite::Connection;

/// Mock query returning canned issues, with configurable open-PR numbers.
struct MockQuery {
    issues: Vec<GithubIssue>,
    open_pr_numbers: Vec<u64>,
}

impl GithubIssueQuery for MockQuery {
    fn list_issues(
        &self,
        _repo: &str,
        _include_labels: &[String],
        _states: &[String],
    ) -> Result<Vec<GithubIssue>, GithubError> {
        Ok(self.issues.clone())
    }

    fn has_open_pr_for_issue(&self, _repo: &str, number: u64) -> Result<bool, GithubError> {
        Ok(self.open_pr_numbers.contains(&number))
    }

    fn list_milestones(&self, _repo: &str) -> Result<Vec<String>, GithubError> {
        Ok(vec![])
    }
}

fn issue(number: u64, labels: &[&str]) -> GithubIssue {
    GithubIssue {
        number,
        title: format!("Issue {number}"),
        state: "open".to_string(),
        labels: labels.iter().map(|s| s.to_string()).collect(),
        assignee: None,
        milestone: None,
    }
}

fn discovery_cfg() -> DiscoveryConfig {
    DiscoveryConfig {
        enabled: true,
        repo: Some("owner/repo".to_string()),
        include_labels: vec!["OK for Luther".to_string()],
        exclude_labels: vec!["Luther working".to_string()],
        issue_states: vec!["open".to_string()],
        assignee_filter: None,
        milestone_order: Some("semver".to_string()),
        max_concurrent_runs: Some(5),
        poll_interval_secs: Some(300),
    }
}

fn memory_db() -> Connection {
    let conn = Connection::open_in_memory().expect("open memory db");
    init_leases_table(&conn).expect("init leases");
    conn
}

#[test]
fn discover_partitions_eligible_and_label_skips() {
    let query = MockQuery {
        issues: vec![
            issue(1, &["OK for Luther"]),
            issue(2, &["needs triage"]),
            issue(3, &["OK for Luther", "Luther working"]),
        ],
        open_pr_numbers: vec![],
    };
    let conn = memory_db();
    let result = discover(&discovery_cfg(), &query, &conn, 0).expect("discover");

    let eligible: Vec<u64> = result.eligible.iter().map(|i| i.number).collect();
    assert_eq!(eligible, vec![1], "only issue 1 is eligible");

    let reasons: Vec<&'static str> = result.skipped.iter().map(|(_, r)| r.code()).collect();
    assert!(reasons.contains(&SkipReason::MissingRequiredLabel(String::new()).code()));
    assert!(reasons.contains(&SkipReason::HasExcludedLabel(String::new()).code()));
}

#[test]
fn discover_skips_issue_with_open_pr() {
    let query = MockQuery {
        issues: vec![issue(7, &["OK for Luther"])],
        open_pr_numbers: vec![7],
    };
    let conn = memory_db();
    let result = discover(&discovery_cfg(), &query, &conn, 0).expect("discover");

    assert!(
        result.eligible.is_empty(),
        "issue with open PR is not eligible"
    );
    assert_eq!(result.skipped.len(), 1);
    assert!(matches!(result.skipped[0].1, SkipReason::HasOpenPr));
}

#[test]
fn discover_skips_issue_with_active_lease() {
    let query = MockQuery {
        issues: vec![issue(9, &["OK for Luther"])],
        open_pr_numbers: vec![],
    };
    let conn = memory_db();
    let lease = IssueLease {
        lease_id: "lease-9".to_string(),
        issue_repo: "owner/repo".to_string(),
        issue_number: 9,
        config_id: "cfg".to_string(),
        run_id: None,
        status: LeaseStatus::Running,
        claimed_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        heartbeat_at: chrono::Utc::now(),
    };
    create_lease(&conn, &lease).expect("seed lease");

    let result = discover(&discovery_cfg(), &query, &conn, 0).expect("discover");
    assert!(result.eligible.is_empty(), "leased issue is not eligible");
    assert!(matches!(result.skipped[0].1, SkipReason::HasActiveLease));
}
