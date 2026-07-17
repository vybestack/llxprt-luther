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

use std::path::PathBuf;

use rusqlite::Connection;

use crate::adapters::github_issues::GithubIssue;
use crate::daemon::discovery::SkipReason;
use crate::persistence::leases::{
    update_lease_status, update_lease_status_conditional_outcome, ConditionalLeaseStatusOutcome,
    IssueLease, LeaseStatus,
};
use crate::workflow::schema::DiscoveryConfig;

mod lease_finalization;
mod paths;

#[cfg(test)]
mod tests;

pub use lease_finalization::finish_lease_after_result;
pub use paths::{DaemonPathBases, PerRunPaths};

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

pub(crate) use super::claim::claim_for_launch_pending;
pub use super::claim::{claim_for_launch, ClaimedLaunch};

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
