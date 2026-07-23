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
use crate::persistence::leases::{IssueLease, LeaseStatus};
#[cfg(test)]
use crate::persistence::update_lease_status;
use crate::workflow::schema::DiscoveryConfig;

mod lease_finalization;
mod paths;
mod resume_preparation;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod transaction_tests;

pub use lease_finalization::finish_lease_after_result;
pub use paths::{DaemonPathBases, PerRunPaths};
pub use resume_preparation::{prepare_resume_lease, PreparedResume};

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
    /// The run terminated because workspace ownership verification failed
    /// (ownership-denied terminal). This is a distinct terminal state from
    /// [`Self::CleanupAbandoned`]: it must never be selected for cleanup
    /// continuation, because cleanup executes shell commands that must only
    /// run in a trusted workspace. An ownership-denied workspace is unowned
    /// (or owned by a foreign run), so cleanup cannot run there.
    OwnershipDenied,
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
    /// The config root the workflow was resolved from at launch. For fresh
    /// daemon launches this is always `"config"`. For resumes prepared via
    /// [`prepare_resume_lease`] this carries the **persisted** canonical config
    /// root (decoded from the launch provenance), so the resume re-resolves
    /// from exactly the same root the run was launched from.
    /// @plan:PLAN-20260722-ISSUE158-LAUNCH-PROVENANCE
    pub config_root: PathBuf,
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
    config_root: &std::path::Path,
) -> Result<LaunchOutcome, rusqlite::Error> {
    let claimed = match claim_for_launch(issue, cfg, conn, config_id, bases, config_root)? {
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

/// Resume a ready lease using its existing run id/checkpoint.
///
/// Delegates to [`prepare_resume_lease`] (which performs all read-only
/// validation before the CAS) and then finalizes the lease based on the
/// launcher's resume result. On any validation skip, the lease is left in
/// `ReadyToResume` without mutation.
pub fn resume_lease(
    lease: &IssueLease,
    conn: &Connection,
    launcher: &dyn WorkflowLauncher,
) -> Result<LaunchOutcome, rusqlite::Error> {
    let prepared = match prepare_resume_lease(lease, conn)? {
        Ok(prepared) => prepared,
        Err(reason) => return Ok(LaunchOutcome::Skipped(reason)),
    };
    let claimed = prepared.into_claimed_launch(lease);
    finish_lease_after_result(
        conn,
        &claimed.lease_id,
        &claimed.request.run_id,
        launcher.resume(&claimed.request),
    )
}
