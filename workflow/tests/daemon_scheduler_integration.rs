//! Integration tests for the daemon scheduler (issue #49).
//!
//! Drives `run_once` against a mock issue source + temp database asserting that
//! launches respect the concurrency limit, leases are created, a second pass
//! does not duplicate launches, and `mark_stale_leases` recovers a stale lease
//! on restart.
//!
//! @plan:PLAN-20260415-DAEMON-DISCOVERY.P06
//! @requirement:REQ-DAEMON-DISCOVERY-005,REQ-DAEMON-DISCOVERY-006,REQ-DAEMON-DISCOVERY-007
use std::sync::Mutex;

use luther_workflow::adapters::github::GithubError;
use luther_workflow::adapters::github_issues::{
    GithubIssue, GithubIssueQuery, GithubSubIssue, SubIssueSource,
};
use luther_workflow::daemon::launcher::{LaunchRequest, WorkflowLaunchResult, WorkflowLauncher};
use luther_workflow::daemon::scheduler::run_once;
use luther_workflow::persistence::leases::{
    count_active_leases_for_config, init_leases_table, list_all_leases, mark_stale_leases,
    try_claim, update_lease_status, LeaseStatus,
};
use luther_workflow::workflow::config_loader::parse_daemon_scheduler_config_toml;
use luther_workflow::workflow::schema::DiscoveryConfig;
use rusqlite::Connection;

struct MockQuery {
    issues: Vec<GithubIssue>,
    parent_issue_numbers: Vec<u64>,
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

    fn has_open_pr_for_issue(&self, _repo: &str, _number: u64) -> Result<bool, GithubError> {
        Ok(false)
    }

    fn list_milestones(&self, _repo: &str) -> Result<Vec<String>, GithubError> {
        Ok(vec![])
    }

    fn list_sub_issues(
        &self,
        _repo: &str,
        number: u64,
    ) -> Result<Vec<GithubSubIssue>, GithubError> {
        if self.parent_issue_numbers.contains(&number) {
            return Ok(vec![GithubSubIssue {
                issue: issue(99),
                position: Some(0),
                source: SubIssueSource::Native,
            }]);
        }
        Ok(vec![])
    }
}

/// Records every launch request and always reports success.
#[derive(Default)]
struct RecordingLauncher {
    launched: Mutex<Vec<LaunchRequest>>,
}

impl WorkflowLauncher for RecordingLauncher {
    fn launch(&self, request: &LaunchRequest) -> Result<WorkflowLaunchResult, String> {
        self.launched.lock().unwrap().push(request.clone());
        Ok(WorkflowLaunchResult::CompletedSuccess)
    }
}

fn issue(number: u64) -> GithubIssue {
    GithubIssue {
        number,
        title: format!("Issue {number}"),
        state: "open".to_string(),
        labels: vec!["OK for Luther".to_string()],
        assignee: None,
        milestone: None,
        body: None,
    }
}

fn cfg(max: u32) -> DiscoveryConfig {
    DiscoveryConfig {
        enabled: true,
        repo: Some("owner/repo".to_string()),
        include_labels: vec!["OK for Luther".to_string()],
        exclude_labels: vec!["Luther working".to_string()],
        active_parent_label: Some("Luther working".to_string()),
        issue_states: vec!["open".to_string()],
        assignee_filter: None,
        milestone_order: Some("none".to_string()),
        max_concurrent_runs: Some(max),
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
    let conn = Connection::open_in_memory().expect("open db");
    luther_workflow::persistence::sqlite::init_runs_schema(&conn).expect("init runs");
    init_leases_table(&conn).expect("init");
    luther_workflow::persistence::wait_state::init_wait_states_table(&conn).expect("init waits");
    conn
}

#[test]
fn run_once_launches_up_to_concurrency_limit() {
    let query = MockQuery {
        issues: vec![issue(1), issue(2), issue(3)],
        parent_issue_numbers: vec![],
    };
    let launcher = RecordingLauncher::default();
    let conn = memory_db();

    let summary = run_once(&cfg(2), &query, &conn, &launcher, "cfg-a").expect("run once");
    assert_eq!(
        summary.launched, 2,
        "only two issues launched under limit 2"
    );
    assert_eq!(launcher.launched.lock().unwrap().len(), 2);
    // Completed launches free the slot, so no active leases remain.
    let all = list_all_leases(&conn).expect("leases");
    assert_eq!(all.len(), 2, "one lease created per launched issue");
}

#[test]
fn second_pass_does_not_duplicate_completed_work() {
    let query = MockQuery {
        issues: vec![issue(1)],
        parent_issue_numbers: vec![],
    };
    let launcher = RecordingLauncher::default();
    let conn = memory_db();

    run_once(&cfg(1), &query, &conn, &launcher, "cfg-a").expect("pass 1");
    assert_eq!(launcher.launched.lock().unwrap().len(), 1);

    // The lease is Completed; a second discovery pass must not relaunch it
    // because the issue already has a (terminal) lease record.
    run_once(&cfg(1), &query, &conn, &launcher, "cfg-a").expect("pass 2");
    assert_eq!(
        launcher.launched.lock().unwrap().len(),
        1,
        "completed issue is not relaunched"
    );
}

#[test]
fn parent_routing_launches_with_parent_config_and_workflow_type() {
    let query = MockQuery {
        issues: vec![issue(61)],
        parent_issue_numbers: vec![61],
    };
    let launcher = RecordingLauncher::default();
    let conn = memory_db();
    let mut discovery = cfg(1);
    discovery.route_parent_issues = true;
    discovery.parent_config_id = Some("parent-orchestrator-luther".to_string());
    discovery.parent_workflow_type_id = Some("parent-issue-orchestrator-v1".to_string());

    let summary = run_once(&discovery, &query, &conn, &launcher, "llxprt-luther")
        .expect("run once with routed parent issue");

    assert_eq!(summary.launched, 1);
    let launched = launcher.launched.lock().unwrap();
    assert_eq!(launched.len(), 1);
    assert_eq!(launched[0].issue_number, 61);
    assert_eq!(launched[0].config_id, "parent-orchestrator-luther");
    assert_eq!(
        launched[0].workflow_type_id.as_deref(),
        Some("parent-issue-orchestrator-v1")
    );
}
#[test]
fn parent_routed_ready_lease_resumes_from_originating_scheduler_target() {
    let query = MockQuery {
        issues: vec![],
        parent_issue_numbers: vec![],
    };
    let launcher = RecordingLauncher::default();
    let conn = memory_db();
    let mut discovery = cfg(1);
    discovery.route_parent_issues = true;
    discovery.parent_config_id = Some("parent-orchestrator-luther".to_string());
    discovery.parent_workflow_type_id = Some("parent-issue-orchestrator-v1".to_string());
    let lease = try_claim(&conn, "owner/repo", 61, "parent-orchestrator-luther")
        .expect("claim parent lease")
        .expect("parent lease won");
    update_lease_status(
        &conn,
        &lease.lease_id,
        LeaseStatus::ReadyToResume,
        Some("run-parent-61"),
    )
    .expect("mark parent ready");
    let mut metadata = luther_workflow::persistence::RunMetadata::new(
        "run-parent-61",
        "parent-issue-orchestrator-v1",
        "parent-orchestrator-luther",
    );
    metadata.status = luther_workflow::persistence::RunStatus::ReadyToResume;
    metadata.set_current_step("wait_for_child_merge");
    metadata.repository = Some("owner/repo".to_string());
    metadata.issue_number = Some(61);
    luther_workflow::persistence::persist_run_with_conn(&conn, &metadata)
        .expect("persist parent metadata");
    luther_workflow::persistence::checkpoint::save_checkpoint_with_conn(
        &conn,
        &luther_workflow::persistence::checkpoint::Checkpoint::new(
            "run-parent-61",
            "wait_for_child_merge",
        ),
    )
    .expect("persist parent checkpoint");

    let summary = run_once(&discovery, &query, &conn, &launcher, "llxprt-luther")
        .expect("run once resumes parent lease");

    assert_eq!(summary.resumed, 1);
    assert_eq!(summary.launched, 0);
    let launched = launcher.launched.lock().unwrap();
    assert_eq!(launched.len(), 1);
    assert_eq!(launched[0].config_id, "parent-orchestrator-luther");
    assert_eq!(launched[0].run_id, "run-parent-61");
    assert_eq!(launched[0].issue_number, 61);
    assert_eq!(
        launched[0].workflow_type_id.as_deref(),
        Some("parent-issue-orchestrator-v1")
    );
}

#[test]
fn mark_stale_recovers_lease_on_restart() {
    let query = MockQuery {
        issues: vec![issue(5)],
        parent_issue_numbers: vec![],
    };
    let launcher = RecordingLauncher::default();
    let conn = memory_db();

    // First pass launches issue 5; force its lease back to Running so it counts

    // as active for the stale sweep.
    run_once(&cfg(1), &query, &conn, &launcher, "cfg-a").expect("pass 1");
    let lease = &list_all_leases(&conn).expect("leases")[0];
    update_lease_status(
        &conn,
        &lease.lease_id,
        LeaseStatus::Running,
        lease.run_id.as_deref(),
    )
    .expect("force running");
    assert_eq!(count_active_leases_for_config(&conn, "cfg-a").unwrap(), 1);

    // A zero-second timeout treats every active lease as stale (restart sweep).
    let recovered = mark_stale_leases(&conn, 0).expect("sweep");
    assert_eq!(recovered, 1, "the running lease is marked stale");
    assert_eq!(count_active_leases_for_config(&conn, "cfg-a").unwrap(), 0);
}

#[test]
fn parse_daemon_scheduler_config_toml_reads_limits_and_targets() {
    let cfg = parse_daemon_scheduler_config_toml(
        r#"
max_concurrent_active_runs = 5
max_concurrent_runs_per_config = 2
max_concurrent_runs_per_repository = 3
poll_interval_seconds = 300

[[targets]]
config_id = "llxprt-code"

[[targets]]
config_id = "llxprt-luther"
"#,
    )
    .unwrap();

    assert_eq!(cfg.max_concurrent_active_runs, Some(5));
    assert_eq!(cfg.max_concurrent_runs_per_config, Some(2));
    assert_eq!(cfg.max_concurrent_runs_per_repository, Some(3));
    assert_eq!(cfg.poll_interval_seconds, Some(300));
    assert_eq!(cfg.targets.len(), 2);
    assert_eq!(cfg.targets[0].config_id, "llxprt-code");
    assert_eq!(cfg.targets[1].config_id, "llxprt-luther");
}

#[test]
fn parse_daemon_scheduler_config_toml_empty_returns_defaults() {
    let cfg = parse_daemon_scheduler_config_toml("").unwrap();

    assert_eq!(cfg.max_concurrent_active_runs, None);
    assert_eq!(cfg.max_concurrent_runs_per_config, None);
    assert_eq!(cfg.max_concurrent_runs_per_repository, None);
    assert_eq!(cfg.poll_interval_seconds, None);
    assert!(cfg.targets.is_empty());
}

#[test]
fn parse_daemon_scheduler_config_toml_malformed_returns_error() {
    assert!(parse_daemon_scheduler_config_toml("not valid toml =").is_err());
}
