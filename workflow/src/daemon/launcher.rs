//! Daemon workflow launch with claim + concurrency enforcement.
//!
//! `claim_and_launch` is the authoritative duplicate-prevention path: it atomically
//! claims an issue via the lease table, re-checks the per-config concurrency
//! ceiling, then delegates the actual workflow execution to a [`WorkflowLauncher`]
//! seam (the binary wires the real engine runner; tests inject a mock). Lease status is
//! advanced to `Running` before launch, then to a terminal `Completed`/`Failed` state or to
//! non-terminal `WaitingExternal` when the engine suspends.
//!
//! @plan:PLAN-20260415-DAEMON-DISCOVERY.P06
//! @requirement:REQ-DAEMON-DISCOVERY-005,REQ-DAEMON-DISCOVERY-006

use std::path::{Component, PathBuf};

use rusqlite::Connection;

use crate::adapters::github_issues::GithubIssue;
use crate::daemon::discovery::SkipReason;
use crate::persistence::leases::{
    count_active_leases_for_config, try_claim, update_lease_status,
    update_lease_status_conditional_outcome, ConditionalLeaseStatusOutcome, IssueLease,
    LeaseStatus,
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
    /// A concurrent writer advanced or reassigned the lease before a stale
    /// engine result could be applied. The durable lease state was preserved.
    LeaseStatePreserved {
        run_id: String,
        current_status: Option<LeaseStatus>,
        current_run_id: Option<String>,
    },
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
    pub workflow_type_id: Option<String>,
    pub run_id: String,
    pub repo: String,
    pub issue_number: u64,
    /// Resolved per-run work directory (`base/issue-N/run-id`), or `None` when
    /// no daemon path base is available (one-shot CLI runs).
    pub work_dir: Option<PathBuf>,
    /// Resolved per-run artifact directory (`base/issue-N/run-id`), or `None`
    /// when no daemon path base is available.
    pub artifact_dir: Option<PathBuf>,
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

/// Structured daemon base roots used to construct isolated per-run paths.
///
/// Configured `work_dir`/`artifact_dir` values from the resolved
/// workflow config variables are treated as base roots; each daemon-launched
/// run gets `base / issue-N / run-id` so concurrent runs for the same config
/// cannot collide. Bases are optional: a one-shot CLI run has no daemon bases,
/// so existing engine fallbacks continue to apply.
/// @plan:issue-117
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DaemonPathBases {
    pub work_dir_base: Option<PathBuf>,
    pub artifact_dir_base: Option<PathBuf>,
}

impl DaemonPathBases {
    /// Build the per-run work and artifact directories for an issue + run id.
    ///
    /// Returns `None` for a directory when its base is absent. The `run_id` must
    /// be a single relative path component before it is joined under the daemon
    /// base root.
    /// @plan:issue-117
    pub fn per_run_paths(&self, issue_number: u64, run_id: &str) -> Result<PerRunPaths, String> {
        validate_run_id_path_component(run_id)?;
        let issue_segment = format!("issue-{issue_number}");
        Ok(PerRunPaths {
            work_dir: self
                .work_dir_base
                .as_ref()
                .map(|base| base.join(&issue_segment).join(run_id)),
            artifact_dir: self
                .artifact_dir_base
                .as_ref()
                .map(|base| base.join(&issue_segment).join(run_id)),
        })
    }
}
fn validate_run_id_path_component(run_id: &str) -> Result<(), String> {
    if run_id.is_empty() {
        return Err("run_id must not be empty".to_string());
    }
    let run_id_path = PathBuf::from(run_id);
    let mut components = run_id_path.components();
    if run_id.contains('\\') {
        return Err(format!(
            "run_id must be a single safe path component: {run_id}"
        ));
    }
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(()),
        _ => Err(format!(
            "run_id must be a single safe path component: {run_id}"
        )),
    }
}

/// Resolved per-run work/artifact directories for a single daemon launch.
/// @plan:issue-117
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PerRunPaths {
    pub work_dir: Option<PathBuf>,
    pub artifact_dir: Option<PathBuf>,
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
    bases: &DaemonPathBases,
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
    let paths = match bases.per_run_paths(issue.number, &run_id) {
        Ok(paths) => paths,
        Err(error) => {
            update_lease_status(conn, &lease.lease_id, LeaseStatus::Abandoned, None)?;
            return Ok(Err(SkipReason::InvalidPath(error)));
        }
    };
    update_lease_status(conn, &lease.lease_id, LeaseStatus::Running, Some(&run_id))?;
    Ok(Ok(ClaimedLaunch {
        lease_id: lease.lease_id,
        request: LaunchRequest {
            config_id: config_id.to_string(),
            workflow_type_id: None,
            run_id,
            repo,
            issue_number: issue.number,
            work_dir: paths.work_dir,
            artifact_dir: paths.artifact_dir,
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
            Ok(launched(run_id, true))
        }
        Ok(WorkflowLaunchResult::CompletedFailure) => {
            update_lease_status(conn, lease_id, LeaseStatus::Failed, Some(run_id))?;
            Ok(launched(run_id, false))
        }
        Ok(WorkflowLaunchResult::SuspendedExternalWait) => {
            match update_lease_status_conditional_outcome(
                conn,
                lease_id,
                LeaseStatus::WaitingExternal,
                &[LeaseStatus::Running, LeaseStatus::WaitingExternal],
                None,
                Some(run_id),
            )? {
                ConditionalLeaseStatusOutcome::Applied => Ok(LaunchOutcome::WaitingExternal {
                    run_id: run_id.to_string(),
                }),
                ConditionalLeaseStatusOutcome::Rejected {
                    current_status,
                    current_run_id,
                } => Ok(LaunchOutcome::LeaseStatePreserved {
                    run_id: run_id.to_string(),
                    current_status: Some(current_status),
                    current_run_id,
                }),
                ConditionalLeaseStatusOutcome::Missing => Ok(LaunchOutcome::LeaseStatePreserved {
                    run_id: run_id.to_string(),
                    current_status: None,
                    current_run_id: None,
                }),
            }
        }
        Err(error) => compensate_lease_after_launch_error(conn, lease_id, run_id, &error),
    }
}

/// Build the success-flagged [`LaunchOutcome::Launched`] variant for a run.
fn launched(run_id: &str, success: bool) -> LaunchOutcome {
    LaunchOutcome::Launched {
        run_id: run_id.to_string(),
        success,
    }
}

/// Resolve the lease outcome after a launch error.
///
/// The engine may have committed `WaitingExternal` before the error (e.g. it
/// persisted the wait state, then the launch wrapper hit a downstream
/// failure). We must neither strand capacity by leaving a `Running` lease nor
/// mark a genuinely waiting run `Failed`. The complete invariant check
/// (`has_pollable_external_wait`) verifies that run status, wait row, and
/// lease are all consistently `WaitingExternal`. If the check itself fails
/// (DB or decode error), compensate to `Failed` rather than propagating — a
/// `Running` lease is never an acceptable terminal state. The invariant-check
/// error itself is logged as a diagnostic but does not propagate; the
/// authoritative compensation write that follows propagates via `?`.
///
/// Every branch uses a conditional lease update so the poller's concurrent
/// terminal or ready classification cannot be overwritten by this stale
/// launcher write. When the conditional update is rejected (the lease has
/// already advanced past the expected states), the existing state is left
/// intact — no TOCTOU window remains. Database errors from the compensation
/// write itself propagate to the caller via `?` rather than being swallowed,
/// so a failed compensation is never silently masked.
fn compensate_lease_after_launch_error(
    conn: &Connection,
    lease_id: &str,
    run_id: &str,
    error: &str,
) -> Result<LaunchOutcome, rusqlite::Error> {
    eprintln!("workflow launch failed for run {run_id}: {error}");
    let (target_status, applied_outcome) = match crate::persistence::has_pollable_external_wait(
        conn, run_id,
    ) {
        Ok(true) => (
            LeaseStatus::WaitingExternal,
            LaunchOutcome::WaitingExternal {
                run_id: run_id.to_string(),
            },
        ),
        Ok(false) => (LeaseStatus::Failed, launched(run_id, false)),
        Err(check_error) => {
            eprintln!(
                "external-wait invariant check failed for run {run_id}, compensating lease to Failed: {check_error}"
            );
            (LeaseStatus::Failed, launched(run_id, false))
        }
    };

    match update_lease_status_conditional_outcome(
        conn,
        lease_id,
        target_status,
        &[LeaseStatus::Running, LeaseStatus::WaitingExternal],
        None,
        Some(run_id),
    )? {
        ConditionalLeaseStatusOutcome::Applied => Ok(applied_outcome),
        ConditionalLeaseStatusOutcome::Rejected {
            current_status,
            current_run_id,
        } => Ok(LaunchOutcome::LeaseStatePreserved {
            run_id: run_id.to_string(),
            current_status: Some(current_status),
            current_run_id,
        }),
        ConditionalLeaseStatusOutcome::Missing => Ok(LaunchOutcome::LeaseStatePreserved {
            run_id: run_id.to_string(),
            current_status: None,
            current_run_id: None,
        }),
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
    bases: &DaemonPathBases,
) -> Result<LaunchOutcome, rusqlite::Error> {
    let claimed = match claim_for_launch(issue, cfg, conn, config_id, bases)? {
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
            workflow_type_id: workflow_type_id_for_resume(conn, &run_id)?,
            run_id,
            repo: lease.issue_repo.clone(),
            issue_number: lease.issue_number,
            // Resumes reuse persisted RunMetadata paths; do not synthesize new
            // per-run paths for a resumed run. @plan:issue-117
            work_dir: None,
            artifact_dir: None,
        },
    }))
}

fn workflow_type_id_for_resume(
    conn: &Connection,
    run_id: &str,
) -> Result<Option<String>, rusqlite::Error> {
    Ok(crate::persistence::get_run_with_conn(conn, run_id)?
        .map(|metadata| metadata.workflow_type_id)
        .filter(|workflow_type_id| !workflow_type_id.is_empty()))
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
            active_parent_label: None,
            issue_states: vec!["open".to_string()],
            assignee_filter: None,
            milestone_order: Some("semver".to_string()),
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

    fn issue(number: u64) -> GithubIssue {
        GithubIssue {
            number,
            title: format!("Issue {number}"),
            state: "open".to_string(),
            labels: vec![],
            assignee: None,
            milestone: None,
            body: None,
        }
    }

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        init_leases_table(&c).unwrap();
        crate::persistence::sqlite::init_runs_schema(&c).unwrap();
        crate::persistence::wait_state::init_wait_states_table(&c).unwrap();
        c
    }

    /// Seed a complete, pollable external wait using the production-path
    /// `persist_external_wait` function, establishing the full invariant:
    /// run status, checkpoint, wait_states row, and waiting lease.
    fn seed_complete_external_wait(
        c: &Connection,
        issue_number: u64,
        run_id: &str,
        resume_step: &str,
    ) -> String {
        let lease = try_claim(c, "o/r", issue_number, "cfg").unwrap().unwrap();
        update_lease_status(c, &lease.lease_id, LeaseStatus::Running, Some(run_id)).unwrap();
        // Seed run metadata + checkpoint (required by persist_external_wait).
        let metadata = crate::persistence::RunMetadata::new(run_id, "wf", "cfg");
        crate::persistence::persist_run_with_conn(c, &metadata).unwrap();
        crate::persistence::checkpoint::save_checkpoint_with_conn(
            c,
            &crate::persistence::checkpoint::Checkpoint::new(run_id, resume_step),
        )
        .unwrap();
        let mut record = crate::persistence::wait_state::WaitStateRecord::new(run_id, "cfg");
        record.lease_id = Some(lease.lease_id.clone());
        record.repository = "o/r".to_string();
        record.issue_number = issue_number;
        record.resume_step = resume_step.to_string();
        crate::persistence::persist_external_wait(c, &record).unwrap();
        lease.lease_id
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
        let outcome = claim_and_launch(
            &issue(1),
            &cfg(2),
            &c,
            &l,
            "cfg",
            &DaemonPathBases::default(),
        )
        .unwrap();
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
        claim_and_launch(
            &issue(1),
            &cfg(2),
            &c,
            &l,
            "cfg",
            &DaemonPathBases::default(),
        )
        .unwrap();
        // Pre-existing active claim from another config blocks relaunch.
        try_claim(&c, "o/r", 2, "other").unwrap();
        let outcome = claim_and_launch(
            &issue(2),
            &cfg(2),
            &c,
            &l,
            "cfg",
            &DaemonPathBases::default(),
        )
        .unwrap();
        assert_eq!(outcome, LaunchOutcome::Skipped(SkipReason::HasActiveLease));
    }

    #[test]
    fn failed_run_marks_lease_failed() {
        let c = conn();
        let l = MockLauncher::new(WorkflowLaunchResult::CompletedFailure);
        let outcome = claim_and_launch(
            &issue(3),
            &cfg(2),
            &c,
            &l,
            "cfg",
            &DaemonPathBases::default(),
        )
        .unwrap();
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
        let outcome = claim_and_launch(
            &issue(5),
            &cfg(2),
            &c,
            &l,
            "cfg",
            &DaemonPathBases::default(),
        )
        .unwrap();
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
    fn error_after_complete_external_wait_keeps_lease_resumable() {
        // Issue 131 invariant: when the launcher returns an error after a
        // complete pollable external wait has been persisted via the
        // production path (`persist_external_wait`), the lease must stay
        // WaitingExternal so the daemon poller can resume it.
        let c = conn();
        let run_id = "run-complete-wait";
        let lease_id = seed_complete_external_wait(&c, 6, run_id, "watch_pr_checks");

        let outcome = finish_lease_after_result(
            &c,
            &lease_id,
            run_id,
            Err("downstream wrapper error after persist".to_string()),
        )
        .unwrap();

        assert_eq!(
            outcome,
            LaunchOutcome::WaitingExternal {
                run_id: run_id.to_string(),
            },
            "an error after a complete external wait must not mark the lease Failed"
        );
        let lease = get_lease_for_issue(&c, "o/r", 6).unwrap().unwrap();
        assert_eq!(lease.status, LeaseStatus::WaitingExternal);
    }

    #[test]
    fn error_with_incomplete_wait_marks_lease_failed() {
        // Issue 131 invariant: when only the run status is WaitingExternal
        // but no pollable wait_states row exists (incomplete invariant), the
        // lease must go Failed — never strand capacity on an un-pollable run.
        let c = conn();
        let claimed = claim_for_launch(&issue(7), &cfg(2), &c, "cfg", &DaemonPathBases::default())
            .unwrap()
            .unwrap();
        // Seed run status only — no wait_states row (incomplete invariant).
        let mut metadata =
            crate::persistence::RunMetadata::new(&claimed.request.run_id, "wf", "cfg");
        metadata.status = crate::persistence::RunStatus::WaitingExternal;
        crate::persistence::persist_run_with_conn(&c, &metadata).unwrap();

        let outcome = finish_lease_after_result(
            &c,
            &claimed.lease_id,
            &claimed.request.run_id,
            Err("persist wait state: missing checkpoint".to_string()),
        )
        .unwrap();

        match outcome {
            LaunchOutcome::Launched { success, .. } => assert!(!success),
            other => panic!("unexpected: {other:?}"),
        }
        let lease = get_lease_for_issue(&c, "o/r", 7).unwrap().unwrap();
        assert_eq!(lease.status, LeaseStatus::Failed);
    }

    #[test]
    fn error_when_invariant_check_fails_compensates_lease_to_failed() {
        // Issue 131 invariant: when `has_pollable_external_wait` itself
        // returns an error (e.g. a corrupt wait_states row causing a decode
        // failure), the launcher must compensate the lease to Failed rather
        // than propagating the error and leaving a Running lease stranded.
        let c = conn();
        let run_id = "run-decode-err";
        let lease_id = seed_complete_external_wait(&c, 9, run_id, "watch_pr_checks");
        c.execute("DROP TABLE runs", []).unwrap();

        let outcome = finish_lease_after_result(
            &c,
            &lease_id,
            run_id,
            Err("downstream wrapper error after persist".to_string()),
        )
        .unwrap();

        match outcome {
            LaunchOutcome::Launched { success, .. } => assert!(!success),
            other => panic!("unexpected: {other:?}"),
        }
        let lease = get_lease_for_issue(&c, "o/r", 9).unwrap().unwrap();
        assert_eq!(
            lease.status,
            LeaseStatus::Failed,
            "an invariant-query failure must compensate to Failed, not strand the lease"
        );
    }

    #[test]
    fn suspended_external_wait_does_not_overwrite_terminal_lease() {
        // Issue 131 invariant: a SuspendedExternalWait result must not
        // overwrite a lease that has already transitioned to a terminal
        // state (e.g. the poller classified it while the engine was still
        // running). The conditional lease update leaves the terminal lease
        // intact.
        let c = conn();
        let claimed = claim_for_launch(&issue(8), &cfg(2), &c, "cfg", &DaemonPathBases::default())
            .unwrap()
            .unwrap();
        // Simulate the poller marking the lease terminal while the engine ran.
        update_lease_status(
            &c,
            &claimed.lease_id,
            LeaseStatus::Failed,
            Some(&claimed.request.run_id),
        )
        .unwrap();

        let outcome = finish_lease_after_result(
            &c,
            &claimed.lease_id,
            &claimed.request.run_id,
            Ok(WorkflowLaunchResult::SuspendedExternalWait),
        )
        .unwrap();
        assert_eq!(
            outcome,
            LaunchOutcome::LeaseStatePreserved {
                run_id: claimed.request.run_id.clone(),
                current_status: Some(LeaseStatus::Failed),
                current_run_id: Some(claimed.request.run_id.clone()),
            },
            "a rejected stale suspend must report the durable lease state"
        );
        let lease = get_lease_for_issue(&c, "o/r", 8).unwrap().unwrap();
        assert_eq!(
            lease.status,
            LeaseStatus::Failed,
            "a terminal lease must not be overwritten by a stale suspend"
        );
    }

    #[test]
    fn suspended_external_wait_reports_and_preserves_ready_to_resume_lease() {
        let c = conn();
        let claimed = claim_for_launch(&issue(81), &cfg(2), &c, "cfg", &DaemonPathBases::default())
            .unwrap()
            .unwrap();
        update_lease_status(
            &c,
            &claimed.lease_id,
            LeaseStatus::ReadyToResume,
            Some(&claimed.request.run_id),
        )
        .unwrap();

        let outcome = finish_lease_after_result(
            &c,
            &claimed.lease_id,
            &claimed.request.run_id,
            Ok(WorkflowLaunchResult::SuspendedExternalWait),
        )
        .unwrap();

        assert_eq!(
            outcome,
            LaunchOutcome::LeaseStatePreserved {
                run_id: claimed.request.run_id.clone(),
                current_status: Some(LeaseStatus::ReadyToResume),
                current_run_id: Some(claimed.request.run_id.clone()),
            }
        );
        let lease = get_lease_for_issue(&c, "o/r", 81).unwrap().unwrap();
        assert_eq!(lease.status, LeaseStatus::ReadyToResume);
    }

    #[test]
    fn concurrency_limit_blocks_and_records() {
        let c = conn();
        let l = MockLauncher::new(WorkflowLaunchResult::CompletedSuccess);
        // Pre-fill one active running lease to occupy the only slot.
        let pre = try_claim(&c, "o/r", 100, "cfg").unwrap().unwrap();
        update_lease_status(&c, &pre.lease_id, LeaseStatus::Running, Some("r0")).unwrap();
        // max=1 => claiming a new issue over-claims and must be released.
        let outcome = claim_and_launch(
            &issue(4),
            &cfg(1),
            &c,
            &l,
            "cfg",
            &DaemonPathBases::default(),
        )
        .unwrap();
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
        let outcome = claim_for_launch(
            &issue(5),
            &cfg(2),
            &c,
            "cfg",
            &DaemonPathBases {
                work_dir_base: None,
                artifact_dir_base: None,
            },
        )
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

    #[test]
    fn per_run_paths_isolate_concurrent_issues() {
        let bases = DaemonPathBases {
            work_dir_base: Some(std::path::PathBuf::from(
                "/tmp/luther-workspaces/llxprt-luther",
            )),
            artifact_dir_base: Some(std::path::PathBuf::from(
                "/tmp/luther-artifacts/llxprt-luther",
            )),
        };
        let paths_109 = bases.per_run_paths(109, "run-aaa").unwrap();
        let paths_110 = bases.per_run_paths(110, "run-bbb").unwrap();
        assert_ne!(paths_109.work_dir, paths_110.work_dir);
        assert_ne!(paths_109.artifact_dir, paths_110.artifact_dir);
        assert_eq!(
            paths_109.work_dir.as_deref().unwrap().to_str().unwrap(),
            "/tmp/luther-workspaces/llxprt-luther/issue-109/run-aaa"
        );
        assert_eq!(
            paths_109.artifact_dir.as_deref().unwrap().to_str().unwrap(),
            "/tmp/luther-artifacts/llxprt-luther/issue-109/run-aaa"
        );
    }

    #[test]
    fn per_run_paths_rejects_unsafe_run_id_components() {
        let bases = DaemonPathBases {
            work_dir_base: Some(std::path::PathBuf::from("/tmp/work")),
            artifact_dir_base: Some(std::path::PathBuf::from("/tmp/artifacts")),
        };

        assert!(bases.per_run_paths(1, "../escape").is_err());
        assert!(bases.per_run_paths(1, "/tmp/escape").is_err());
    }

    #[test]
    fn per_run_paths_rejects_windows_style_separators() {
        let bases = DaemonPathBases {
            work_dir_base: Some(std::path::PathBuf::from("/tmp/work")),
            artifact_dir_base: Some(std::path::PathBuf::from("/tmp/artifacts")),
        };

        assert!(bases.per_run_paths(1, "foo\\..\\escape").is_err());
    }

    #[test]
    fn claim_for_launch_attaches_generated_run_id_paths() {
        let c = conn();
        let bases = DaemonPathBases {
            work_dir_base: Some(std::path::PathBuf::from(
                "/tmp/luther-workspaces/llxprt-luther",
            )),
            artifact_dir_base: Some(std::path::PathBuf::from(
                "/tmp/luther-artifacts/llxprt-luther",
            )),
        };
        let claimed = claim_for_launch(&issue(7), &cfg(2), &c, "cfg", &bases)
            .unwrap()
            .unwrap();
        let work = claimed.request.work_dir.as_deref().unwrap();
        let artifact = claimed.request.artifact_dir.as_deref().unwrap();
        // The run-id path component matches the generated internal run_id.
        assert!(work.to_str().unwrap().ends_with(&claimed.request.run_id));
        assert!(artifact
            .to_str()
            .unwrap()
            .ends_with(&claimed.request.run_id));
        // And contains the issue segment.
        assert!(work.to_str().unwrap().contains("issue-7"));
        assert!(artifact.to_str().unwrap().contains("issue-7"));
    }

    #[test]
    fn compensate_error_does_not_overwrite_terminal_lease() {
        let c = conn();
        let claimed = claim_for_launch(&issue(10), &cfg(2), &c, "cfg", &DaemonPathBases::default())
            .unwrap()
            .unwrap();
        update_lease_status(
            &c,
            &claimed.lease_id,
            LeaseStatus::Completed,
            Some(&claimed.request.run_id),
        )
        .unwrap();

        let outcome = finish_lease_after_result(
            &c,
            &claimed.lease_id,
            &claimed.request.run_id,
            Err("stale error after concurrent completion".to_string()),
        )
        .unwrap();

        assert_eq!(
            outcome,
            LaunchOutcome::LeaseStatePreserved {
                run_id: claimed.request.run_id.clone(),
                current_status: Some(LeaseStatus::Completed),
                current_run_id: Some(claimed.request.run_id.clone()),
            }
        );
        let lease = get_lease_for_issue(&c, "o/r", 10).unwrap().unwrap();
        assert_eq!(
            lease.status,
            LeaseStatus::Completed,
            "a Completed lease must not be overwritten by a stale error compensation"
        );
    }

    #[test]
    fn compensate_error_with_pollable_wait_does_not_overwrite_ready_lease() {
        // Race regression: when the launcher hits an error and the lease has
        // already been advanced to ReadyToResume by the poller, the invariant
        // check (has_pollable_external_wait) returns false (a ReadyToResume
        // lease is not "pollable"), so the launcher tries to fail it. The
        // conditional fail transition must reject because ReadyToResume is not
        // in the expected set [Running, WaitingExternal], preserving the
        // poller's classification.
        let c = conn();
        let run_id = "run-race-ready";
        let lease_id = seed_complete_external_wait(&c, 11, run_id, "watch_pr_checks");
        // Simulate the poller advancing to ReadyToResume while the engine ran.
        update_lease_status(&c, &lease_id, LeaseStatus::ReadyToResume, Some(run_id)).unwrap();

        let outcome = finish_lease_after_result(
            &c,
            &lease_id,
            run_id,
            Err("stale error after ready classification".to_string()),
        )
        .unwrap();

        assert_eq!(
            outcome,
            LaunchOutcome::LeaseStatePreserved {
                run_id: run_id.to_string(),
                current_status: Some(LeaseStatus::ReadyToResume),
                current_run_id: Some(run_id.to_string()),
            }
        );
        let lease = get_lease_for_issue(&c, "o/r", 11).unwrap().unwrap();
        assert_eq!(
            lease.status,
            LeaseStatus::ReadyToResume,
            "a ReadyToResume lease must not be overwritten by a stale error compensation"
        );
    }

    #[test]
    fn compensate_error_reports_missing_lease_as_preserved() {
        let c = conn();
        let outcome = finish_lease_after_result(
            &c,
            "missing-lease",
            "missing-run",
            Err("launch failed after lease deletion".to_string()),
        )
        .unwrap();

        assert_eq!(
            outcome,
            LaunchOutcome::LeaseStatePreserved {
                run_id: "missing-run".to_string(),
                current_status: None,
                current_run_id: None,
            }
        );
    }
}
