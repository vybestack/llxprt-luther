//! Daemon workflow launch with claim + concurrency enforcement.
//!
//! `claim_and_launch` is the authoritative duplicate-prevention path: it atomically
//! claims an issue via the lease table, re-checks the per-config concurrency
//! ceiling, then delegates the actual workflow execution to a [`WorkflowLauncher`]
//! seam (the binary wires the real engine runner; tests inject a mock). Lease
//! status is advanced to `Running` before launch and to a terminal
//! `Completed`/`Failed` afterward.
//!
//! @plan:PLAN-20260415-DAEMON-DISCOVERY.P06
//! @requirement:REQ-DAEMON-DISCOVERY-005,REQ-DAEMON-DISCOVERY-006

use rusqlite::Connection;

use crate::adapters::github_issues::GithubIssue;
use crate::daemon::discovery::SkipReason;
use crate::persistence::leases::{
    count_active_leases_for_config, try_claim, update_lease_status, LeaseStatus,
};
use crate::workflow::schema::DiscoveryConfig;

/// Terminal result of a launch attempt.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P06
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchOutcome {
    /// The run was launched and completed (success path); carries the run id.
    Launched { run_id: String, success: bool },
    /// The launch was skipped before any run started.
    Skipped(SkipReason),
}

/// Request passed to a [`WorkflowLauncher`] to start a single workflow run.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P06
#[derive(Debug, Clone)]
pub struct LaunchRequest {
    pub config_id: String,
    pub run_id: String,
    pub repo: String,
    pub issue_number: u64,
}

/// Seam for executing a workflow run for a claimed issue.
///
/// The production implementation (in the binary) builds the durable engine
/// runner with `issue_number`/`repo` overrides and executes it; tests inject a
/// deterministic mock. Returns `Ok(true)` on success, `Ok(false)` on a
/// non-fatal failed run, and `Err(_)` only for launch-infrastructure errors.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P06
pub trait WorkflowLauncher {
    /// Execute a workflow run; return whether it succeeded.
    fn launch(&self, request: &LaunchRequest) -> Result<bool, String>;
}

/// Generate a fresh run id for a launch.
fn new_run_id() -> String {
    format!("run-{}", uuid::Uuid::new_v4())
}

/// Atomically claim an issue and launch a workflow run for it.
///
/// Steps: `try_claim` (lost => `Skipped(HasActiveLease)`); re-check
/// concurrency (`count_active_leases_for_config` vs `max_concurrent_runs`,
/// releasing the just-won claim to `Abandoned` and returning
/// `Skipped(ConcurrencyLimitReached)` if over limit); set lease `Running` with a
/// new run id; invoke the launcher; advance the lease to `Completed`/`Failed`.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P06
/// @requirement:REQ-DAEMON-DISCOVERY-005,REQ-DAEMON-DISCOVERY-006
pub fn claim_and_launch(
    issue: &GithubIssue,
    cfg: &DiscoveryConfig,
    conn: &Connection,
    launcher: &dyn WorkflowLauncher,
    config_id: &str,
) -> Result<LaunchOutcome, rusqlite::Error> {
    let repo = cfg.repo.clone().unwrap_or_default();
    let lease = match try_claim(conn, &repo, issue.number, config_id)? {
        Some(lease) => lease,
        None => return Ok(LaunchOutcome::Skipped(SkipReason::HasActiveLease)),
    };

    let max = cfg.max_concurrent_runs.unwrap_or(1) as usize;
    let active = count_active_leases_for_config(conn, config_id)?;
    if active > max {
        // We over-claimed; release the just-won claim and report the limit.
        update_lease_status(conn, &lease.lease_id, LeaseStatus::Abandoned, None)?;
        return Ok(LaunchOutcome::Skipped(SkipReason::ConcurrencyLimitReached));
    }

    let run_id = new_run_id();
    update_lease_status(conn, &lease.lease_id, LeaseStatus::Running, Some(&run_id))?;

    let request = LaunchRequest {
        config_id: config_id.to_string(),
        run_id: run_id.clone(),
        repo,
        issue_number: issue.number,
    };
    match launcher.launch(&request) {
        Ok(true) => {
            update_lease_status(conn, &lease.lease_id, LeaseStatus::Completed, None)?;
            Ok(LaunchOutcome::Launched {
                run_id,
                success: true,
            })
        }
        Ok(false) => {
            update_lease_status(conn, &lease.lease_id, LeaseStatus::Failed, None)?;
            Ok(LaunchOutcome::Launched {
                run_id,
                success: false,
            })
        }
        Err(_) => {
            update_lease_status(conn, &lease.lease_id, LeaseStatus::Failed, None)?;
            Ok(LaunchOutcome::Launched {
                run_id,
                success: false,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::leases::{get_lease_for_issue, init_leases_table};
    use std::cell::RefCell;

    fn cfg(max: u32) -> DiscoveryConfig {
        DiscoveryConfig {
            enabled: true,
            repo: Some("o/r".to_string()),
            include_labels: vec![],
            exclude_labels: vec![],
            issue_states: vec!["open".to_string()],
            assignee_filter: None,
            milestone_order: Some("semver".to_string()),
            max_concurrent_runs: Some(max),
            poll_interval_secs: Some(300),
        }
    }

    fn issue(number: u64) -> GithubIssue {
        GithubIssue {
            number,
            title: format!("Issue {number}"),
            state: "open".to_string(),
            labels: vec![],
            assignee: None,
            milestone: None,
        }
    }

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        init_leases_table(&c).unwrap();
        c
    }

    /// Records launch requests and returns a preset success flag.
    struct MockLauncher {
        success: bool,
        requests: RefCell<Vec<LaunchRequest>>,
    }

    impl MockLauncher {
        fn new(success: bool) -> Self {
            Self {
                success,
                requests: RefCell::new(Vec::new()),
            }
        }
    }

    impl WorkflowLauncher for MockLauncher {
        fn launch(&self, request: &LaunchRequest) -> Result<bool, String> {
            self.requests.borrow_mut().push(request.clone());
            Ok(self.success)
        }
    }

    #[test]
    fn launch_wins_claim_and_completes() {
        let c = conn();
        let l = MockLauncher::new(true);
        let outcome = claim_and_launch(&issue(1), &cfg(2), &c, &l, "cfg").unwrap();
        match outcome {
            LaunchOutcome::Launched { success, .. } => assert!(success),
            other => panic!("unexpected: {other:?}"),
        }
        let lease = get_lease_for_issue(&c, "o/r", 1).unwrap().unwrap();
        assert_eq!(lease.status, LeaseStatus::Completed);
        assert!(lease.run_id.is_some());
        assert_eq!(l.requests.borrow().len(), 1);
        assert_eq!(l.requests.borrow()[0].issue_number, 1);
    }

    #[test]
    fn second_claim_is_rejected() {
        let c = conn();
        let l = MockLauncher::new(true);
        // First wins and completes (lease no longer active).
        claim_and_launch(&issue(1), &cfg(2), &c, &l, "cfg").unwrap();
        // Pre-existing active claim from another config blocks relaunch.
        try_claim(&c, "o/r", 2, "other").unwrap();
        let outcome = claim_and_launch(&issue(2), &cfg(2), &c, &l, "cfg").unwrap();
        assert_eq!(outcome, LaunchOutcome::Skipped(SkipReason::HasActiveLease));
    }

    #[test]
    fn failed_run_marks_lease_failed() {
        let c = conn();
        let l = MockLauncher::new(false);
        let outcome = claim_and_launch(&issue(3), &cfg(2), &c, &l, "cfg").unwrap();
        match outcome {
            LaunchOutcome::Launched { success, .. } => assert!(!success),
            other => panic!("unexpected: {other:?}"),
        }
        let lease = get_lease_for_issue(&c, "o/r", 3).unwrap().unwrap();
        assert_eq!(lease.status, LeaseStatus::Failed);
    }

    #[test]
    fn concurrency_limit_blocks_and_records() {
        let c = conn();
        let l = MockLauncher::new(true);
        // Pre-fill one active running lease to occupy the only slot.
        let pre = try_claim(&c, "o/r", 100, "cfg").unwrap().unwrap();
        update_lease_status(&c, &pre.lease_id, LeaseStatus::Running, Some("r0")).unwrap();
        // max=1 => claiming a new issue over-claims and must be released.
        let outcome = claim_and_launch(&issue(4), &cfg(1), &c, &l, "cfg").unwrap();
        assert_eq!(
            outcome,
            LaunchOutcome::Skipped(SkipReason::ConcurrencyLimitReached)
        );
        let lease = get_lease_for_issue(&c, "o/r", 4).unwrap().unwrap();
        assert_eq!(lease.status, LeaseStatus::Abandoned);
        assert!(l.requests.borrow().is_empty());
    }

    #[test]
    fn lease_running_during_launch() {
        // Launcher that asserts the lease is Running at launch time.
        struct AssertingLauncher<'a> {
            conn: &'a Connection,
        }
        impl WorkflowLauncher for AssertingLauncher<'_> {
            fn launch(&self, request: &LaunchRequest) -> Result<bool, String> {
                let lease = get_lease_for_issue(self.conn, &request.repo, request.issue_number)
                    .unwrap()
                    .unwrap();
                assert_eq!(lease.status, LeaseStatus::Running);
                assert_eq!(lease.run_id.as_deref(), Some(request.run_id.as_str()));
                Ok(true)
            }
        }
        let c = conn();
        let l = AssertingLauncher { conn: &c };
        claim_and_launch(&issue(5), &cfg(2), &c, &l, "cfg").unwrap();
    }
}
