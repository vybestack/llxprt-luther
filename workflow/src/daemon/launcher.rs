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
    count_active_leases_for_config, try_claim, update_lease_status, IssueLease, LeaseStatus,
};
use crate::workflow::schema::DiscoveryConfig;

/// Terminal result of a launch attempt.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P06
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchOutcome {
    /// The run was launched and completed (success path); carries the run id.
    Launched { run_id: String, success: bool },
    /// The run checkpointed at an external wait and released active capacity.
    WaitingExternal { run_id: String },
    /// The launch was skipped before any run started.
    Skipped(SkipReason),
}

/// Result returned by a workflow runner after it has started executing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowLaunchResult {
    CompletedSuccess,
    CompletedFailure,
    SuspendedExternalWait,
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
/// deterministic mock. The result preserves terminal completions separately
/// from capacity-free external waits.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P06
pub trait WorkflowLauncher: Sync {
    /// Execute a workflow run and report terminal vs suspended outcomes.
    fn launch(&self, request: &LaunchRequest) -> Result<WorkflowLaunchResult, String>;

    /// Resume an existing workflow run/checkpoint and report terminal vs suspended outcomes.
    fn resume(&self, request: &LaunchRequest) -> Result<WorkflowLaunchResult, String> {
        self.launch(request)
    }
}

/// Generate a fresh run id for a launch.
fn new_run_id() -> String {
    format!("run-{}", uuid::Uuid::new_v4())
}

pub struct ClaimedLaunch {
    pub lease_id: String,
    pub request: LaunchRequest,
}

pub fn claim_for_launch(
    issue: &GithubIssue,
    cfg: &DiscoveryConfig,
    conn: &Connection,
    config_id: &str,
) -> Result<Result<ClaimedLaunch, SkipReason>, rusqlite::Error> {
    let repo = cfg.repo.clone().unwrap_or_default();
    let lease = match try_claim(conn, &repo, issue.number, config_id)? {
        Some(lease) => lease,
        None => return Ok(Err(SkipReason::HasActiveLease)),
    };

    let max = cfg
        .max_concurrent_runs_per_config
        .or(cfg.max_concurrent_runs)
        .unwrap_or(1) as usize;
    let active = count_active_leases_for_config(conn, config_id)?;
    if active > max {
        update_lease_status(conn, &lease.lease_id, LeaseStatus::Abandoned, None)?;
        return Ok(Err(SkipReason::ConcurrencyLimitReached));
    }

    let run_id = new_run_id();
    update_lease_status(conn, &lease.lease_id, LeaseStatus::Running, Some(&run_id))?;
    Ok(Ok(ClaimedLaunch {
        lease_id: lease.lease_id,
        request: LaunchRequest {
            config_id: config_id.to_string(),
            run_id,
            repo,
            issue_number: issue.number,
        },
    }))
}

pub fn finish_lease_after_result(
    conn: &Connection,
    lease_id: &str,
    run_id: &str,
    result: Result<WorkflowLaunchResult, String>,
) -> Result<LaunchOutcome, rusqlite::Error> {
    match result {
        Ok(WorkflowLaunchResult::CompletedSuccess) => {
            update_lease_status(conn, lease_id, LeaseStatus::Completed, Some(run_id))?;
            Ok(LaunchOutcome::Launched {
                run_id: run_id.to_string(),
                success: true,
            })
        }
        Ok(WorkflowLaunchResult::CompletedFailure) => {
            update_lease_status(conn, lease_id, LeaseStatus::Failed, Some(run_id))?;
            Ok(LaunchOutcome::Launched {
                run_id: run_id.to_string(),
                success: false,
            })
        }
        Ok(WorkflowLaunchResult::SuspendedExternalWait) => {
            update_lease_status(conn, lease_id, LeaseStatus::WaitingExternal, Some(run_id))?;
            Ok(LaunchOutcome::WaitingExternal {
                run_id: run_id.to_string(),
            })
        }
        Err(error) => {
            eprintln!("workflow launch failed for run {run_id}: {error}");
            update_lease_status(conn, lease_id, LeaseStatus::Failed, Some(run_id))?;
            Ok(LaunchOutcome::Launched {
                run_id: run_id.to_string(),
                success: false,
            })
        }
    }
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
    let claimed = match claim_for_launch(issue, cfg, conn, config_id)? {
        Ok(claimed) => claimed,
        Err(reason) => return Ok(LaunchOutcome::Skipped(reason)),
    };
    finish_lease_after_result(
        conn,
        &claimed.lease_id,
        &claimed.request.run_id,
        launcher.launch(&claimed.request),
    )
}

pub fn prepare_resume_lease(
    lease: &IssueLease,
    conn: &Connection,
) -> Result<Result<ClaimedLaunch, SkipReason>, rusqlite::Error> {
    let Some(run_id) = lease.run_id.clone() else {
        update_lease_status(conn, &lease.lease_id, LeaseStatus::Failed, None)?;
        return Ok(Err(SkipReason::InvalidLeaseState));
    };
    update_lease_status(conn, &lease.lease_id, LeaseStatus::Running, Some(&run_id))?;
    Ok(Ok(ClaimedLaunch {
        lease_id: lease.lease_id.clone(),
        request: LaunchRequest {
            config_id: lease.config_id.clone(),
            run_id,
            repo: lease.issue_repo.clone(),
            issue_number: lease.issue_number,
        },
    }))
}

/// Resume a ready lease using its existing run id/checkpoint.
pub fn resume_lease(
    lease: &IssueLease,
    conn: &Connection,
    launcher: &dyn WorkflowLauncher,
) -> Result<LaunchOutcome, rusqlite::Error> {
    let claimed = match prepare_resume_lease(lease, conn)? {
        Ok(claimed) => claimed,
        Err(reason) => return Ok(LaunchOutcome::Skipped(reason)),
    };
    finish_lease_after_result(
        conn,
        &claimed.lease_id,
        &claimed.request.run_id,
        launcher.resume(&claimed.request),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::leases::{get_lease_for_issue, init_leases_table};
    use std::sync::Mutex;

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
            max_concurrent_active_runs: None,
            max_concurrent_runs_per_repository: None,
            max_concurrent_runs_per_config: None,
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
        result: WorkflowLaunchResult,
        requests: Mutex<Vec<LaunchRequest>>,
    }

    impl MockLauncher {
        fn new(result: WorkflowLaunchResult) -> Self {
            Self {
                result,
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    impl WorkflowLauncher for MockLauncher {
        fn launch(&self, request: &LaunchRequest) -> Result<WorkflowLaunchResult, String> {
            self.requests.lock().unwrap().push(request.clone());
            Ok(self.result.clone())
        }
    }

    #[test]
    fn launch_wins_claim_and_completes() {
        let c = conn();
        let l = MockLauncher::new(WorkflowLaunchResult::CompletedSuccess);
        let outcome = claim_and_launch(&issue(1), &cfg(2), &c, &l, "cfg").unwrap();
        match outcome {
            LaunchOutcome::Launched { success, .. } => assert!(success),
            other => panic!("unexpected: {other:?}"),
        }
        let lease = get_lease_for_issue(&c, "o/r", 1).unwrap().unwrap();
        assert_eq!(lease.status, LeaseStatus::Completed);
        assert!(lease.run_id.is_some());
        assert_eq!(l.requests.lock().unwrap().len(), 1);
        assert_eq!(l.requests.lock().unwrap()[0].issue_number, 1);
    }

    #[test]
    fn second_claim_is_rejected() {
        let c = conn();
        let l = MockLauncher::new(WorkflowLaunchResult::CompletedSuccess);
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
        let l = MockLauncher::new(WorkflowLaunchResult::CompletedFailure);
        let outcome = claim_and_launch(&issue(3), &cfg(2), &c, &l, "cfg").unwrap();
        match outcome {
            LaunchOutcome::Launched { success, .. } => assert!(!success),
            other => panic!("unexpected: {other:?}"),
        }
        let lease = get_lease_for_issue(&c, "o/r", 3).unwrap().unwrap();
        assert_eq!(lease.status, LeaseStatus::Failed);
    }

    #[test]
    fn suspended_run_marks_lease_waiting_external() {
        let c = conn();
        let l = MockLauncher::new(WorkflowLaunchResult::SuspendedExternalWait);
        let outcome = claim_and_launch(&issue(5), &cfg(2), &c, &l, "cfg").unwrap();
        let run_id = match outcome {
            LaunchOutcome::WaitingExternal { run_id } => run_id,
            other => panic!("unexpected: {other:?}"),
        };
        let lease = get_lease_for_issue(&c, "o/r", 5).unwrap().unwrap();
        assert_eq!(lease.status, LeaseStatus::WaitingExternal);
        assert_eq!(lease.run_id.as_deref(), Some(run_id.as_str()));
        assert_eq!(count_active_leases_for_config(&c, "cfg").unwrap(), 0);
    }

    #[test]
    fn concurrency_limit_blocks_and_records() {
        let c = conn();
        let l = MockLauncher::new(WorkflowLaunchResult::CompletedSuccess);
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
        assert!(l.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn lease_running_during_launch() {
        let c = conn();
        let l = MockLauncher::new(WorkflowLaunchResult::CompletedSuccess);
        let outcome = claim_for_launch(&issue(5), &cfg(2), &c, "cfg")
            .unwrap()
            .unwrap();
        let lease = get_lease_for_issue(&c, "o/r", 5).unwrap().unwrap();
        assert_eq!(lease.status, LeaseStatus::Running);
        assert_eq!(
            lease.run_id.as_deref(),
            Some(outcome.request.run_id.as_str())
        );
        finish_lease_after_result(
            &c,
            &outcome.lease_id,
            &outcome.request.run_id,
            l.launch(&outcome.request),
        )
        .unwrap();
    }
}
