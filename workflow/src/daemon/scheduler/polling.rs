//! External-wait polling pass.
//!
//! [`poll_due_waits`] lists pollable wait-state rows, applies each poller
//! decision, and records structured details for skips/warnings into the
//! resulting [`RunSummary`]. Skip recorders below translate the domain
//! `PollApplyError` variants into observable `SkippedPollDetail` entries so
//! benign concurrent transitions never silently degrade the applied count.

use rusqlite::Connection;

use super::{ArtifactWarningDetail, RunSummary, SkippedPollDetail, SkippedPollReason};
use crate::daemon::poller::{
    apply_poll_decision, ExternalWaitPoller, PollApplyError, PollApplyOutcome,
};
use crate::persistence::wait_state::list_pollable_wait_states;

/// Poll every due external wait and fold the outcomes into a [`RunSummary`].
///
/// Fatal SQLite/persistence errors abort the pass early; benign concurrent
/// transitions are recorded as skipped polls so they remain observable without
/// being counted in `polls_applied`.
pub(super) fn poll_due_waits(
    conn: &Connection,
    poller: &dyn ExternalWaitPoller,
) -> Result<RunSummary, PollApplyError> {
    let waits = list_pollable_wait_states(conn, chrono::Utc::now())?;
    let mut summary = RunSummary {
        pollable_waits: waits.len(),
        ..RunSummary::default()
    };
    for wait in waits {
        let decision = poller.poll(&wait);
        match apply_poll_decision(conn, &wait, &decision) {
            Ok(PollApplyOutcome::Committed) => summary.polls_applied += 1,
            Ok(PollApplyOutcome::CommittedWithArtifactWarnings(warnings)) => {
                summary.polls_applied += 1;
                for warning in warnings {
                    summary.record_artifact_warning(ArtifactWarningDetail {
                        run_id: wait.run_id.clone(),
                        phase: warning.phase,
                        error: warning.error,
                    });
                }
            }
            Err(PollApplyError::LeaseTransitionRejected {
                run_id,
                lease_id,
                reason,
            }) => record_lease_transition_skip(
                &mut summary,
                run_id,
                lease_id,
                wait.resume_step.clone(),
                reason,
            ),
            Err(PollApplyError::WaitStateConcurrentTransition(run_id)) => {
                record_wait_state_transition_skip(&mut summary, &wait, run_id);
            }
            Err(PollApplyError::RunStatusConcurrentTransition { run_id, step_id }) => {
                record_run_status_transition_skip(&mut summary, &wait, run_id, step_id);
            }
            Err(PollApplyError::RunMissing { run_id, step_id }) => {
                record_run_missing_skip(&mut summary, &wait, run_id, step_id);
            }
            Err(err @ PollApplyError::Sqlite(_)) => return Err(err),
            Err(err @ PollApplyError::Persistence(_)) => return Err(err),
        }
    }
    Ok(summary)
}

fn record_lease_transition_skip(
    summary: &mut RunSummary,
    run_id: String,
    lease_id: String,
    step_id: String,
    reason: &'static str,
) {
    tracing::warn!(
        run_id = %run_id,
        lease_id = %lease_id,
        step_id = %step_id,
        reason,
        "poll skipped: lease transition rejected"
    );
    summary.record_skipped_poll(SkippedPollDetail {
        run_id,
        lease_id: Some(lease_id),
        step_id,
        reason: SkippedPollReason::LeaseTransitionRejected,
        lease_transition_reason: Some(reason),
    });
}

fn record_wait_state_transition_skip(
    summary: &mut RunSummary,
    wait: &crate::persistence::wait_state::WaitStateRecord,
    run_id: String,
) {
    tracing::warn!(
        run_id = %run_id,
        step_id = %wait.resume_step,
        "poll skipped: wait-state was concurrently transitioned"
    );
    summary.record_skipped_poll(SkippedPollDetail {
        run_id,
        lease_id: wait.lease_id.clone(),
        step_id: wait.resume_step.clone(),
        reason: SkippedPollReason::WaitStateConcurrentTransition,
        lease_transition_reason: None,
    });
}

fn record_run_status_transition_skip(
    summary: &mut RunSummary,
    wait: &crate::persistence::wait_state::WaitStateRecord,
    run_id: String,
    step_id: String,
) {
    tracing::warn!(
        run_id = %run_id,
        step_id = %step_id,
        "poll skipped: run already terminal — stale status update rejected"
    );
    summary.record_skipped_poll(SkippedPollDetail {
        run_id,
        lease_id: wait.lease_id.clone(),
        step_id,
        reason: SkippedPollReason::RunStatusConcurrentTransition,
        lease_transition_reason: None,
    });
}

fn record_run_missing_skip(
    summary: &mut RunSummary,
    wait: &crate::persistence::wait_state::WaitStateRecord,
    run_id: String,
    step_id: String,
) {
    tracing::error!(
        run_id = %run_id,
        step_id = %step_id,
        lease_id = ?wait.lease_id,
        integrity_violation = "pollable_wait_missing_run_metadata",
        "poll skipped: orphaned wait-state row"
    );
    summary.record_skipped_poll(SkippedPollDetail {
        run_id,
        lease_id: wait.lease_id.clone(),
        step_id,
        reason: SkippedPollReason::RunMissing,
        lease_transition_reason: None,
    });
}
