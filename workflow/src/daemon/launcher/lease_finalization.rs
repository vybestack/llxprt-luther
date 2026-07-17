//! Lease-state advancement after a workflow runner result.
//!
//! [`finish_lease_after_result`] is the authoritative seam that maps a
//! [`WorkflowLaunchResult`] (or launch error) to a terminal `Completed`/`Failed`
//! state or to non-terminal `WaitingExternal` when the engine suspends. Every
//! transition uses a conditional (CAS) lease update so a stale launcher cannot
//! overwrite a newer durable state written by the poller or a concurrent
//! reclaim.

use rusqlite::Connection;

use crate::persistence::leases::{
    update_lease_status_conditional_outcome, ConditionalLeaseStatusOutcome, LeaseStatus,
};

use super::{LaunchOutcome, WorkflowLaunchResult};

/// Resolve a workflow runner result into a durable lease transition.
///
/// Terminal completions (`CompletedSuccess`/`CompletedFailure`) use an
/// exact-owner `Running` CAS; a rejected (status advanced or owner changed) or
/// missing lease yields [`LaunchOutcome::LeaseStatePreserved`].
/// `SuspendedExternalWait` and launch errors use broader expected-state sets and
/// the [`has_pollable_external_wait`] invariant to avoid stranding capacity.
///
/// [`has_pollable_external_wait`]: crate::persistence::has_pollable_external_wait
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
        Ok(WorkflowLaunchResult::CleanupAbandoned) => finalize_exact_owner_lease(
            conn,
            lease_id,
            run_id,
            LeaseStatus::CleanupAbandoned,
            &[LeaseStatus::Running, LeaseStatus::CleanupAbandoned],
            launched(run_id, false),
        ),
        Ok(WorkflowLaunchResult::SuspendedExternalWait) => finalize_exact_owner_lease(
            conn,
            lease_id,
            run_id,
            LeaseStatus::WaitingExternal,
            &[LeaseStatus::Running, LeaseStatus::WaitingExternal],
            LaunchOutcome::WaitingExternal {
                run_id: run_id.to_string(),
            },
        ),
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

/// Finalize a non-terminal or cleanup-abandonment lease transition using an
/// exact-owner CAS keyed on `run_id` for both `new_run_id` and
/// `expected_run_id`.
///
/// Unlike [`finalize_terminal_lease`], the expected-status set for these
/// transitions includes the target status itself so an idempotent re-apply of
/// the same transition by the same owner is accepted. The exact-owner guard
/// (`expected_run_id == Some(run_id)`) is the critical fencing property: a
/// stale launcher whose `run_id` was superseded by a concurrent reclaim cannot
/// overwrite the newer durable state, even when the lease remains in a
/// matching status. A rejected or missing lease yields
/// [`LaunchOutcome::LeaseStatePreserved`], preserving the durable state.
fn finalize_exact_owner_lease(
    conn: &Connection,
    lease_id: &str,
    run_id: &str,
    target_status: LeaseStatus,
    expected_statuses: &[LeaseStatus],
    applied_outcome: LaunchOutcome,
) -> Result<LaunchOutcome, rusqlite::Error> {
    match update_lease_status_conditional_outcome(
        conn,
        lease_id,
        target_status,
        expected_statuses,
        Some(run_id),
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
        Some(run_id),
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
