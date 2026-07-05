//! Integration tests for daemon issue discovery (issue #49).
//!
//! Exercises the discovery pipeline end-to-end against a mock
//! [`GithubIssueQuery`] and a temp in-memory SQLite database, asserting the
//! eligible/skipped partitioning including the open-PR and active-lease skips.
//!
//! @plan:PLAN-20260415-DAEMON-DISCOVERY.P04
//! @requirement:REQ-DAEMON-DISCOVERY-004
use luther_workflow::adapters::github::GithubError;
use luther_workflow::adapters::github_issues::{GithubIssue, GithubIssueQuery, GithubParentIssue};
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
    parent_by_child: Vec<(u64, GithubIssue)>,
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

    fn get_parent_issue(
        &self,
        _repo: &str,
        number: u64,
    ) -> Result<Option<GithubParentIssue>, GithubError> {
        Ok(self
            .parent_by_child
            .iter()
            .find(|(child, _)| *child == number)
            .map(|(_, issue)| GithubParentIssue {
                issue: issue.clone(),
            }))
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
        body: None,
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
        max_concurrent_active_runs: None,
        max_concurrent_runs_per_repository: None,
        max_concurrent_runs_per_config: None,
        route_parent_issues: false,
        parent_workflow_type_id: Some("parent-issue-orchestrator-v1".to_string()),
        parent_config_id: None,
        skip_children_of_active_parents: false,
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
        parent_by_child: vec![],
    };
    let conn = memory_db();
    let result = discover(&discovery_cfg(), &query, &conn, 0).expect("discover");

    let eligible: Vec<u64> = result.eligible.iter().map(|i| i.issue.number).collect();
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
        parent_by_child: vec![],
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
        parent_by_child: vec![],
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

#[test]
fn discover_skips_child_when_parent_has_active_lease() {
    let mut cfg = discovery_cfg();
    cfg.skip_children_of_active_parents = true;
    let query = MockQuery {
        issues: vec![issue(61, &["OK for Luther"])],
        open_pr_numbers: vec![],
        parent_by_child: vec![(61, issue(60, &["OK for Luther"]))],
    };
    let conn = memory_db();
    let lease = IssueLease {
        lease_id: "lease-parent-60".to_string(),
        issue_repo: "owner/repo".to_string(),
        issue_number: 60,
        config_id: "parent-orchestrator-luther".to_string(),
        run_id: Some("run-parent-60".to_string()),
        status: LeaseStatus::WaitingExternal,
        claimed_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        heartbeat_at: chrono::Utc::now(),
    };
    create_lease(&conn, &lease).expect("seed parent lease");

    let result = discover(&cfg, &query, &conn, 0).expect("discover");

    assert!(
        result.eligible.is_empty(),
        "child owned by parent lease is skipped"
    );
    assert!(matches!(
        result.skipped[0].1,
        SkipReason::ChildOfActiveParent
    ));
}

#[test]
fn discover_skips_child_when_parent_has_luther_working_label() {
    let mut cfg = discovery_cfg();
    cfg.skip_children_of_active_parents = true;
    let query = MockQuery {
        issues: vec![issue(62, &["OK for Luther"])],
        open_pr_numbers: vec![],
        parent_by_child: vec![(62, issue(60, &["OK for Luther", "Luther working"]))],
    };
    let conn = memory_db();

    let result = discover(&cfg, &query, &conn, 0).expect("discover");

    assert!(
        result.eligible.is_empty(),
        "child owned by labeled active parent is skipped"
    );
    assert!(matches!(
        result.skipped[0].1,
        SkipReason::ChildOfActiveParent
    ));
}

#[test]
fn discover_allows_child_when_parent_is_not_active() {
    let mut cfg = discovery_cfg();
    cfg.skip_children_of_active_parents = true;
    let query = MockQuery {
        issues: vec![issue(63, &["OK for Luther"])],
        open_pr_numbers: vec![],
        parent_by_child: vec![(63, issue(60, &["OK for Luther"]))],
    };
    let conn = memory_db();

    let result = discover(&cfg, &query, &conn, 0).expect("discover");

    assert_eq!(
        result
            .eligible
            .iter()
            .map(|routed| routed.issue.number)
            .collect::<Vec<_>>(),
        vec![63]
    );
    assert!(result.skipped.is_empty());
}

#[test]
fn discover_allows_parentless_issue_when_parent_skip_enabled() {
    let mut cfg = discovery_cfg();
    cfg.skip_children_of_active_parents = true;
    let query = MockQuery {
        issues: vec![issue(64, &["OK for Luther"])],
        open_pr_numbers: vec![],
        parent_by_child: vec![],
    };
    let conn = memory_db();

    let result = discover(&cfg, &query, &conn, 0).expect("discover");

    assert_eq!(
        result
            .eligible
            .iter()
            .map(|routed| routed.issue.number)
            .collect::<Vec<_>>(),
        vec![64]
    );
    assert!(result.skipped.is_empty());
}

#[test]
fn discover_allows_child_of_active_parent_when_parent_skip_disabled() {
    let cfg = discovery_cfg();
    let query = MockQuery {
        issues: vec![issue(65, &["OK for Luther"])],
        open_pr_numbers: vec![],
        parent_by_child: vec![(65, issue(60, &["OK for Luther", "Luther working"]))],
    };
    let conn = memory_db();

    let result = discover(&cfg, &query, &conn, 0).expect("discover");

    assert_eq!(
        result
            .eligible
            .iter()
            .map(|routed| routed.issue.number)
            .collect::<Vec<_>>(),
        vec![65]
    );
    assert!(result.skipped.is_empty());
}
