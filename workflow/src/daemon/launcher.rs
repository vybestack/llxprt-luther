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
    update_lease_status, update_lease_status_conditional_outcome, ConditionalLeaseStatusOutcome,
    IssueLease, LeaseStatus,
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
    CleanupAbandoned,
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
    pub daemon_managed_claim: bool,
    pub claim_assignment_added: bool,
    pub claim_label_added: bool,
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

pub(crate) use super::claim::claim_for_launch_pending;
pub use super::claim::{claim_for_launch, ClaimedLaunch};

pub fn finish_lease_after_result(
    conn: &Connection,
    lease_id: &str,
    run_id: &str,
    result: Result<WorkflowLaunchResult, String>,
) -> Result<LaunchOutcome, rusqlite::Error> {
    match result {
        Ok(WorkflowLaunchResult::CompletedSuccess) => {
            finalize_terminal_lease(conn, lease_id, run_id, LeaseStatus::Completed, true)
        }
        Ok(WorkflowLaunchResult::CompletedFailure) => {
            finalize_terminal_lease(conn, lease_id, run_id, LeaseStatus::Failed, false)
        }
        Ok(WorkflowLaunchResult::CleanupAbandoned) => {
            match update_lease_status_conditional_outcome(
                conn,
                lease_id,
                LeaseStatus::CleanupAbandoned,
                &[LeaseStatus::Running, LeaseStatus::CleanupAbandoned],
                None,
                Some(run_id),
            )? {
                ConditionalLeaseStatusOutcome::Applied => Ok(launched(run_id, false)),
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

/// Finalize a terminal `Completed`/`Failed` transition with an exact-owner
/// `Running` CAS so a stale launcher returning from a long engine call cannot
/// overwrite a newer durable state written by the poller or a concurrent
/// reclaim.
///
/// The CAS only applies when the lease is exactly `Running` **and** owned by
/// `run_id`. A rejected (status advanced, owner changed) or missing lease
/// yields [`LaunchOutcome::LeaseStatePreserved`], preserving the durable state.
fn finalize_terminal_lease(
    conn: &Connection,
    lease_id: &str,
    run_id: &str,
    target_status: LeaseStatus,
    success: bool,
) -> Result<LaunchOutcome, rusqlite::Error> {
    match update_lease_status_conditional_outcome(
        conn,
        lease_id,
        target_status,
        &[LeaseStatus::Running],
        Some(run_id),
        Some(run_id),
    )? {
        ConditionalLeaseStatusOutcome::Applied => Ok(launched(run_id, success)),
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

/// Prepare a ready-to-resume lease for dispatch by validating durable state
/// before acquiring ownership.
///
/// All fallible reads (claim receipt, run metadata/workflow type) are performed
/// and validated **before** the conditional lease acquisition. Once the CAS
/// transitions the lease to `Running`, no fallible operation remains — the
/// `ClaimedLaunch` is constructed from values already loaded. This eliminates
/// the transaction-blocker window where a post-acquisition read failure would
/// strand the lease in `Running` without compensation.
///
/// The CAS acquires only when the lease is exactly `ReadyToResume` **and**
/// owned by the expected `run_id`, so a concurrent writer that reassigned the
/// lease cannot be overwritten by this stale preparation.
pub fn prepare_resume_lease(
    lease: &IssueLease,
    conn: &Connection,
) -> Result<Result<ClaimedLaunch, SkipReason>, rusqlite::Error> {
    let Some(run_id) = lease.run_id.clone() else {
        update_lease_status(conn, &lease.lease_id, LeaseStatus::Failed, None)?;
        return Ok(Err(SkipReason::InvalidLeaseState));
    };

    // Load and validate the claim receipt before any state mutation.
    let Some(receipt) =
        crate::persistence::claim_metadata::get_claim_metadata(conn, &lease.lease_id)?
    else {
        return Ok(Err(SkipReason::InvalidLeaseState));
    };

    // Load and validate run metadata/workflow type before the CAS acquisition
    // so no fallible read remains after the lease is acquired. A missing or
    // corrupt run row skips the resume without touching the lease; a DB error
    // propagates before any write occurs.
    let Some(workflow_type_id) = workflow_type_id_for_resume(conn, &run_id)? else {
        return Ok(Err(SkipReason::InvalidLeaseState));
    };

    // Acquire exact ReadyToResume ownership via conditional update. The
    // expected_run_id guard rejects a stale writer whose run_id was superseded
    // by a concurrent reclaim, preserving the durable ReadyToResume state.
    let acquired = update_lease_status_conditional_outcome(
        conn,
        &lease.lease_id,
        LeaseStatus::Running,
        &[LeaseStatus::ReadyToResume],
        Some(&run_id),
        Some(&run_id),
    )?;
    if !matches!(acquired, ConditionalLeaseStatusOutcome::Applied) {
        return Ok(Err(SkipReason::InvalidLeaseState));
    }

    Ok(Ok(ClaimedLaunch {
        lease_id: lease.lease_id.clone(),
        request: LaunchRequest {
            config_id: lease.config_id.clone(),
            workflow_type_id: Some(workflow_type_id),
            run_id,
            repo: lease.issue_repo.clone(),
            issue_number: lease.issue_number,
            daemon_managed_claim: true,
            claim_assignment_added: receipt.assignment_added,
            claim_label_added: receipt.label_added,
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
    use crate::persistence::leases::{count_active_leases_for_config, try_claim};
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
            approval_label: None,
            approval_actor: None,
            claim_assignee: None,
            claim_label: None,
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
            assignees: vec![],
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

    // ---- prepare_resume_lease transaction-safety tests (issue-137) ----

    /// Seed a `ReadyToResume` lease for `issue_number` owned by `run_id`,
    /// including a claim-metadata receipt and a run row with `workflow_type`.
    /// Returns the lease id so the caller can query durable state afterwards.
    fn seed_ready_to_resume(
        c: &Connection,
        issue_number: u64,
        run_id: &str,
        workflow_type: &str,
    ) -> String {
        let lease = try_claim(c, "o/r", issue_number, "cfg").unwrap().unwrap();
        update_lease_status(c, &lease.lease_id, LeaseStatus::Running, Some(run_id)).unwrap();
        crate::persistence::claim_metadata::upsert_claim_metadata(
            c,
            &crate::persistence::claim_metadata::ClaimMetadataReceipt {
                lease_id: lease.lease_id.clone(),
                assignee: "bot".to_string(),
                label: "claimed".to_string(),
                assignment_added: true,
                label_added: false,
                cleanup_pending: false,
            },
        )
        .unwrap();
        let metadata = crate::persistence::RunMetadata::new(run_id, workflow_type, "cfg");
        crate::persistence::persist_run_with_conn(c, &metadata).unwrap();
        update_lease_status(c, &lease.lease_id, LeaseStatus::ReadyToResume, Some(run_id)).unwrap();
        lease.lease_id
    }

    #[test]
    fn resume_preparation_acquires_ready_to_resume_lease() {
        let c = conn();
        let lease_id = seed_ready_to_resume(&c, 20, "resume-ok", "wf-type");
        let lease = get_lease_for_issue(&c, "o/r", 20).unwrap().unwrap();
        let claimed = prepare_resume_lease(&lease, &c)
            .unwrap()
            .expect("should acquire");
        assert_eq!(claimed.lease_id, lease_id);
        assert_eq!(claimed.request.run_id, "resume-ok");
        assert_eq!(claimed.request.workflow_type_id.as_deref(), Some("wf-type"));
        assert!(claimed.request.daemon_managed_claim);
        assert!(claimed.request.claim_assignment_added);
        assert!(!claimed.request.claim_label_added);
        let updated = get_lease_for_issue(&c, "o/r", 20).unwrap().unwrap();
        assert_eq!(updated.status, LeaseStatus::Running);
    }

    #[test]
    fn resume_preparation_skips_when_run_metadata_missing() {
        // Issue-137: a missing run row must skip the resume *before* the CAS
        // acquisition, leaving the lease in ReadyToResume rather than
        // stranding it in Running without a valid workflow type.
        let c = conn();
        let lease = try_claim(&c, "o/r", 21, "cfg").unwrap().unwrap();
        update_lease_status(
            &c,
            &lease.lease_id,
            LeaseStatus::Running,
            Some("no-metadata"),
        )
        .unwrap();
        crate::persistence::claim_metadata::upsert_claim_metadata(
            &c,
            &crate::persistence::claim_metadata::ClaimMetadataReceipt {
                lease_id: lease.lease_id.clone(),
                assignee: "bot".to_string(),
                label: "claimed".to_string(),
                assignment_added: true,
                label_added: false,
                cleanup_pending: false,
            },
        )
        .unwrap();
        update_lease_status(
            &c,
            &lease.lease_id,
            LeaseStatus::ReadyToResume,
            Some("no-metadata"),
        )
        .unwrap();

        let lease = get_lease_for_issue(&c, "o/r", 21).unwrap().unwrap();
        let result = prepare_resume_lease(&lease, &c).unwrap();
        assert!(result.is_err(), "missing run metadata must skip the resume");

        let durable = get_lease_for_issue(&c, "o/r", 21).unwrap().unwrap();
        assert_eq!(
            durable.status,
            LeaseStatus::ReadyToResume,
            "the lease must remain ReadyToResume, not stranded in Running"
        );
    }

    #[test]
    fn resume_preparation_skips_when_run_metadata_corrupt() {
        // Issue-137: a corrupt run row (unparseable status) must cause
        // workflow_type_id_for_resume to fail, and the error must propagate
        // *before* the CAS acquisition so the lease stays ReadyToResume.
        let c = conn();
        seed_ready_to_resume(&c, 22, "corrupt-meta", "wf-type");
        // Corrupt the run status to an unparseable value so that
        // get_run_with_conn returns a decode error.
        c.execute(
            "UPDATE runs SET status = 'not-a-valid-status' WHERE run_id = 'corrupt-meta'",
            [],
        )
        .unwrap();

        let lease = get_lease_for_issue(&c, "o/r", 22).unwrap().unwrap();
        let result = prepare_resume_lease(&lease, &c);
        assert!(
            result.is_err(),
            "a corrupt run row must propagate a DB decode error"
        );

        let durable = get_lease_for_issue(&c, "o/r", 22).unwrap().unwrap();
        assert_eq!(
            durable.status,
            LeaseStatus::ReadyToResume,
            "the lease must remain ReadyToResume when run metadata is corrupt"
        );
    }

    #[test]
    fn resume_preparation_preserves_ready_to_resume_on_stale_cas() {
        // Issue-137: a stale CAS (the lease was advanced away from
        // ReadyToResume by a concurrent writer) must skip the resume and leave
        // the durable state intact.
        let c = conn();
        seed_ready_to_resume(&c, 23, "stale-cas", "wf-type");
        // Simulate a concurrent writer advancing the lease to Completed before
        // prepare_resume_lease runs its CAS.
        let lease = get_lease_for_issue(&c, "o/r", 23).unwrap().unwrap();
        update_lease_status(
            &c,
            &lease.lease_id,
            LeaseStatus::Completed,
            Some("stale-cas"),
        )
        .unwrap();

        let result = prepare_resume_lease(&lease, &c).unwrap();
        assert!(
            result.is_err(),
            "a stale CAS must skip the resume rather than launch"
        );

        let durable = get_lease_for_issue(&c, "o/r", 23).unwrap().unwrap();
        assert_eq!(
            durable.status,
            LeaseStatus::Completed,
            "the durable Completed state must be preserved by the stale CAS rejection"
        );
    }

    #[test]
    fn resume_preparation_skips_when_claim_receipt_missing() {
        // A lease without a claim-metadata receipt cannot be safely resumed
        // because the claim-ownership flags are unknown. The resume must skip
        // before any acquisition.
        let c = conn();
        let lease = try_claim(&c, "o/r", 24, "cfg").unwrap().unwrap();
        update_lease_status(
            &c,
            &lease.lease_id,
            LeaseStatus::Running,
            Some("no-receipt"),
        )
        .unwrap();
        // Deliberately do NOT write claim_metadata.
        let metadata = crate::persistence::RunMetadata::new("no-receipt", "wf-type", "cfg");
        crate::persistence::persist_run_with_conn(&c, &metadata).unwrap();
        update_lease_status(
            &c,
            &lease.lease_id,
            LeaseStatus::ReadyToResume,
            Some("no-receipt"),
        )
        .unwrap();

        let lease = get_lease_for_issue(&c, "o/r", 24).unwrap().unwrap();
        let result = prepare_resume_lease(&lease, &c).unwrap();
        assert!(
            result.is_err(),
            "a missing claim receipt must skip the resume"
        );

        let durable = get_lease_for_issue(&c, "o/r", 24).unwrap().unwrap();
        assert_eq!(
            durable.status,
            LeaseStatus::ReadyToResume,
            "the lease must remain ReadyToResume when the receipt is missing"
        );
    }

    #[test]
    fn resume_preparation_skips_when_workflow_type_empty() {
        // A run row with an empty workflow_type_id cannot resolve a workflow,
        // so the resume must skip before acquisition.
        let c = conn();
        let lease = try_claim(&c, "o/r", 25, "cfg").unwrap().unwrap();
        update_lease_status(&c, &lease.lease_id, LeaseStatus::Running, Some("empty-wf")).unwrap();
        crate::persistence::claim_metadata::upsert_claim_metadata(
            &c,
            &crate::persistence::claim_metadata::ClaimMetadataReceipt {
                lease_id: lease.lease_id.clone(),
                assignee: "bot".to_string(),
                label: "claimed".to_string(),
                assignment_added: true,
                label_added: false,
                cleanup_pending: false,
            },
        )
        .unwrap();
        let metadata = crate::persistence::RunMetadata::new("empty-wf", "", "cfg");
        crate::persistence::persist_run_with_conn(&c, &metadata).unwrap();
        update_lease_status(
            &c,
            &lease.lease_id,
            LeaseStatus::ReadyToResume,
            Some("empty-wf"),
        )
        .unwrap();

        let lease = get_lease_for_issue(&c, "o/r", 25).unwrap().unwrap();
        let result = prepare_resume_lease(&lease, &c).unwrap();
        assert!(
            result.is_err(),
            "an empty workflow_type_id must skip the resume"
        );

        let durable = get_lease_for_issue(&c, "o/r", 25).unwrap().unwrap();
        assert_eq!(
            durable.status,
            LeaseStatus::ReadyToResume,
            "the lease must remain ReadyToResume when the workflow type is empty"
        );
    }

    // ---- CompletedSuccess/CompletedFailure exact-owner Running CAS (issue-137) ----

    #[test]
    fn stale_success_does_not_overwrite_terminal_lease() {
        // Issue-137: a stale CompletedSuccess result whose run_id no longer
        // owns the lease must not overwrite a newer durable terminal state.
        let c = conn();
        let claimed = claim_for_launch(&issue(30), &cfg(2), &c, "cfg", &DaemonPathBases::default())
            .unwrap()
            .unwrap();
        // Simulate a concurrent writer advancing the lease to Failed with the
        // same run_id before the stale launcher returns CompletedSuccess.
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
            Ok(WorkflowLaunchResult::CompletedSuccess),
        )
        .unwrap();

        assert_eq!(
            outcome,
            LaunchOutcome::LeaseStatePreserved {
                run_id: claimed.request.run_id.clone(),
                current_status: Some(LeaseStatus::Failed),
                current_run_id: Some(claimed.request.run_id.clone()),
            },
            "a stale CompletedSuccess must not overwrite a terminal Failed lease"
        );
        let lease = get_lease_for_issue(&c, "o/r", 30).unwrap().unwrap();
        assert_eq!(
            lease.status,
            LeaseStatus::Failed,
            "the durable Failed state must be preserved"
        );
    }

    #[test]
    fn stale_failure_does_not_overwrite_terminal_lease() {
        // Issue-137: a stale CompletedFailure result whose run_id no longer
        // owns the lease must not overwrite a newer durable terminal state.
        let c = conn();
        let claimed = claim_for_launch(&issue(31), &cfg(2), &c, "cfg", &DaemonPathBases::default())
            .unwrap()
            .unwrap();
        // Simulate a concurrent writer advancing the lease to Completed with
        // the same run_id before the stale launcher returns CompletedFailure.
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
            Ok(WorkflowLaunchResult::CompletedFailure),
        )
        .unwrap();

        assert_eq!(
            outcome,
            LaunchOutcome::LeaseStatePreserved {
                run_id: claimed.request.run_id.clone(),
                current_status: Some(LeaseStatus::Completed),
                current_run_id: Some(claimed.request.run_id.clone()),
            },
            "a stale CompletedFailure must not overwrite a terminal Completed lease"
        );
        let lease = get_lease_for_issue(&c, "o/r", 31).unwrap().unwrap();
        assert_eq!(
            lease.status,
            LeaseStatus::Completed,
            "the durable Completed state must be preserved"
        );
    }

    #[test]
    fn stale_success_on_wrong_owner_preserves_lease() {
        // Issue-137: when the lease run_id was superseded by a concurrent
        // reclaim, the stale success result must not apply.
        let c = conn();
        let claimed = claim_for_launch(&issue(32), &cfg(2), &c, "cfg", &DaemonPathBases::default())
            .unwrap()
            .unwrap();
        // Simulate a concurrent reclaim assigning a new run_id while leaving
        // the status Running.
        update_lease_status(
            &c,
            &claimed.lease_id,
            LeaseStatus::Running,
            Some("new-owner-run"),
        )
        .unwrap();

        let outcome = finish_lease_after_result(
            &c,
            &claimed.lease_id,
            &claimed.request.run_id,
            Ok(WorkflowLaunchResult::CompletedSuccess),
        )
        .unwrap();

        assert_eq!(
            outcome,
            LaunchOutcome::LeaseStatePreserved {
                run_id: claimed.request.run_id.clone(),
                current_status: Some(LeaseStatus::Running),
                current_run_id: Some("new-owner-run".to_string()),
            },
            "a stale success must not apply when the owner changed"
        );
        let lease = get_lease_for_issue(&c, "o/r", 32).unwrap().unwrap();
        assert_eq!(lease.status, LeaseStatus::Running);
        assert_eq!(lease.run_id.as_deref(), Some("new-owner-run"));
    }

    #[test]
    fn success_on_missing_lease_reports_preserved() {
        // Issue-137: a terminal result for a missing lease must report
        // LeaseStatePreserved rather than silently succeeding.
        let c = conn();
        let outcome = finish_lease_after_result(
            &c,
            "missing-lease-success",
            "run-missing-success",
            Ok(WorkflowLaunchResult::CompletedSuccess),
        )
        .unwrap();
        assert_eq!(
            outcome,
            LaunchOutcome::LeaseStatePreserved {
                run_id: "run-missing-success".to_string(),
                current_status: None,
                current_run_id: None,
            }
        );
    }
}
