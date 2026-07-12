//! Database application layer for poll decisions.
//!
//! These functions take a [`PollDecision`] produced by the polling layer
//! (see the parent module) and apply the corresponding run, lease, and
//! wait-state transitions inside a single SQLite transaction. The domain
//! error [`PollApplyError`] replaces earlier uses of fabricated
//! `rusqlite::Error` variants that were semantically misleading for
//! concurrent-modification rejections.

use std::borrow::Cow;

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde_json::{json, Value};

use super::{next_poll_time, write_pr_check_status_snapshot, PollClassification, PollDecision};
use crate::persistence::checkpoint::{set_resume_point, PersistenceError};
use crate::persistence::leases::{update_lease_status_conditional, LeaseStatus};
use crate::persistence::run_metadata::RunStatus;
use crate::persistence::sqlite::{
    persist_run_status_from_expected_in_transaction, ExpectedRunStatusOutcome,
};
use crate::persistence::wait_state::{
    delete_wait_state_for_suspension, update_wait_state_after_poll, WaitKind, WaitStateRecord,
};
use crate::persistence::{
    write_poll_result_artifact, write_resume_decision_artifact, write_wait_state_artifact,
};

const READY_TRANSITION_REJECTED: &str = "lease_ready_transition_rejected: lease status not in [WaitingExternal, ReadyToResume] or run_id mismatch";
const TERMINAL_TRANSITION_REJECTED: &str = "lease_terminal_transition_rejected: lease status not in [WaitingExternal, ReadyToResume, Running, Claimed] or run_id mismatch";
const STILL_WAITING_TRANSITION_REJECTED: &str = "lease_still_waiting_transition_rejected: lease has advanced past waiting or owned by another run";

/// Domain error for the external-wait polling lifecycle.
///
/// Replaces the misleading use of `rusqlite::Error::SqliteFailure` with
/// `SQLITE_CONSTRAINT` and `rusqlite::Error::QueryReturnedNoRows` for
/// concurrent-modification rejections. Those rusqlite variants semantically
/// signal database-level constraint violations or read-path invariant
/// violations, not domain-level ownership/status rejections, and their use as
/// business-logic errors is incompatible with transaction rollback semantics.
#[derive(Debug, thiserror::Error)]
pub enum PollApplyError {
    /// The lease's `run_id` no longer matches the run that produced the poll
    /// decision, or the lease has advanced past the expected status set.
    #[error("lease transition rejected for run {run_id}: lease {lease_id} ({reason})")]
    LeaseTransitionRejected {
        /// Run whose poll decision attempted the transition.
        run_id: String,
        /// Lease rejected by the conditional ownership/status guard.
        lease_id: String,
        /// Stable diagnostic describing the rejected transition's guard.
        reason: &'static str,
    },
    /// A wait-state record that should exist was concurrently removed or
    /// already transitioned by another path.
    #[error("wait-state for run {0} was concurrently removed or already transitioned")]
    WaitStateConcurrentTransition(String),
    /// The run metadata record backing a pollable wait-state is missing — an
    /// integrity failure, not a benign concurrent transition. The scheduler
    /// treats this as a visible integrity violation rather than silently
    /// skipping it.
    #[error("run metadata for run {run_id} at step {step_id} is missing — integrity failure")]
    RunMissing {
        /// Run whose poll decision encountered the missing metadata.
        run_id: String,
        /// Resume step associated with the polled wait.
        step_id: String,
    },
    /// The run has advanced beyond the status expected by this poll decision,
    /// so the stale status update was rejected by the conditional guard. This
    /// is a benign concurrent transition (similar to lease rejection) rather
    /// than an integrity failure.
    #[error("run {run_id} status advanced at step {step_id} — stale status update rejected")]
    RunStatusConcurrentTransition {
        /// Run whose stale status transition was rejected.
        run_id: String,
        /// Resume step associated with the polled wait.
        step_id: String,
    },
    /// Underlying database error from the transaction.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// Persistence-layer error (artifact I/O, checkpoint failure, etc.).
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
}

/// Outcome of applying a poll decision to the database.
///
/// The database transaction either commits ([`PollApplyOutcome::Committed`])
/// or fails ([`PollApplyError`]). When the transaction commits but one or more
/// post-commit artifact writes fail, the outcome carries **all** accumulated
/// [`ArtifactWarning`]s so callers can surface every failure without masking
/// the committed DB state. The DB commit cannot roll back post-commit artifact
/// failures, so the committed fact is authoritative and the warnings are
/// advisory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollApplyOutcome {
    /// The poll decision was committed and all post-commit artifacts succeeded.
    Committed,
    /// The poll decision was committed but one or more post-commit artifact
    /// writes failed. The warning collection is non-empty by construction.
    /// The DB state is authoritative; the warnings are advisory.
    CommittedWithArtifactWarnings(NonEmptyArtifactWarnings),
}

/// Advisory warning that a post-commit artifact write failed.
///
/// The DB transaction has already committed, so this cannot roll back the
/// run/lease/wait-state fact. The warning preserves the error string so
/// operators and metrics can detect incomplete observability artifacts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactWarning {
    /// The artifact phase that failed.
    pub phase: ArtifactPhase,
    /// The error message from the failed write.
    pub error: String,
}

/// A post-commit artifact warning collection that is non-empty by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonEmptyArtifactWarnings(Vec<ArtifactWarning>);

impl NonEmptyArtifactWarnings {
    fn new(warnings: Vec<ArtifactWarning>) -> Option<Self> {
        (!warnings.is_empty()).then_some(Self(warnings))
    }

    /// Borrow all accumulated artifact warnings.
    #[must_use]
    pub fn as_slice(&self) -> &[ArtifactWarning] {
        &self.0
    }
}

impl IntoIterator for NonEmptyArtifactWarnings {
    type Item = ArtifactWarning;
    type IntoIter = std::vec::IntoIter<ArtifactWarning>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a NonEmptyArtifactWarnings {
    type Item = &'a ArtifactWarning;
    type IntoIter = std::slice::Iter<'a, ArtifactWarning>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

/// Identifies which post-commit artifact phase failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactPhase {
    /// The PR-check status snapshot write failed.
    PrCheckSnapshot,
    /// The poll-result artifact write failed.
    PollResult,
    /// The wait-state artifact write failed.
    WaitState,
    /// The resume-decision artifact write failed.
    ResumeDecision,
}

/// Apply a poll [`PollDecision`] to the database: transition the run, lease,
/// and wait-state inside a single transaction, then persist post-commit
/// artifacts (poll result, wait-state snapshot, resume decision).
///
/// Concurrent-modification rejections (lease no longer in the expected state
/// or owned by this run, wait-state concurrently removed) are returned as
/// domain-level [`PollApplyError`] variants so the caller can distinguish them
/// from genuine database failures and skip the record without aborting the
/// poll loop.
///
/// # Transaction boundary
///
/// This function owns its transaction and must receive a connection with no
/// active transaction. Rusqlite 0.34's `unchecked_transaction` issues `BEGIN
/// DEFERRED` (not a savepoint), so an active transaction causes SQLite to
/// reject the nested `BEGIN` and is returned as [`PollApplyError::Sqlite`].
pub fn apply_poll_decision(
    conn: &Connection,
    record: &WaitStateRecord,
    decision: &PollDecision,
) -> Result<PollApplyOutcome, PollApplyError> {
    apply_poll_decision_at(conn, record, decision, Utc::now())
}

fn apply_poll_decision_at(
    conn: &Connection,
    record: &WaitStateRecord,
    decision: &PollDecision,
    now: DateTime<Utc>,
) -> Result<PollApplyOutcome, PollApplyError> {
    let tx = conn.unchecked_transaction()?;
    match decision.classification {
        PollClassification::ReadyToResume => {
            apply_ready_to_resume(&tx, record)?;
        }
        PollClassification::TerminalFailure | PollClassification::TimedOut => {
            apply_terminal_failure(&tx, record)?;
        }
        PollClassification::StillWaiting | PollClassification::TransientFailure => {
            apply_still_waiting(&tx, record, decision, now)?;
        }
    }
    tx.commit()?;
    // Post-commit artifact persistence: the DB transaction has already
    // committed, so these failures cannot roll back the committed run/lease/
    // wait-state fact. Surface the failure programmatically through
    // PollApplyOutcome::CommittedWithArtifactWarnings so callers (the
    // scheduler) can record it in RunSummary without masking the committed
    // state.
    //
    // Aggregate ALL post-commit artifact failures so none are dropped: if
    // both the PR-check snapshot and the poll-result artifacts fail, both
    // warnings are collected. Previously, the early return on snapshot
    // failure dropped the poll-artifacts error when both paths failed.
    let mut warnings: Vec<ArtifactWarning> = Vec::new();

    // Clone lazily: the PollDecision may carry large observed_state JSON, and
    // the clone is only needed when the PR-check snapshot write fails and we
    // must annotate the error into a mutable copy.
    let snapshot_err = write_committed_pr_check_snapshot(record, &decision.observed_state).err();
    let effective_decision: Cow<'_, PollDecision> = match &snapshot_err {
        Some(err) => {
            tracing::warn!(
                run_id = %record.run_id,
                error = %err,
                "failed to write committed PR check snapshot"
            );
            warnings.push(ArtifactWarning {
                phase: ArtifactPhase::PrCheckSnapshot,
                error: err.to_string(),
            });
            let mut annotated = decision.clone();
            annotate_artifact_error(&record.run_id, &mut annotated.observed_state, err);
            Cow::Owned(annotated)
        }
        None => Cow::Borrowed(decision),
    };

    warnings.extend(persist_poll_artifacts(record, &effective_decision));

    match NonEmptyArtifactWarnings::new(warnings) {
        Some(warnings) => Ok(PollApplyOutcome::CommittedWithArtifactWarnings(warnings)),
        None => Ok(PollApplyOutcome::Committed),
    }
}

/// Record an artifact-write failure into `observed_state` without corrupting
/// non-object values.
///
/// `serde_json::Value` index-assignment (`value["key"] = ...`) panics when
/// `value` is a non-object, non-null scalar or array. Guard the mutation by
/// promoting `Null` to an empty object and skipping the annotation for
/// scalars/arrays, preserving the original observed state intact.
fn annotate_artifact_error(
    run_id: &str,
    observed_state: &mut Value,
    err: &crate::engine::runner::EngineError,
) {
    match observed_state {
        Value::Object(map) => {
            map.insert("artifact_error".to_string(), json!(err.to_string()));
        }
        Value::Null => {
            let mut map = serde_json::Map::new();
            map.insert("artifact_error".to_string(), json!(err.to_string()));
            *observed_state = Value::Object(map);
        }
        // Non-object, non-null values (String, Number, Bool, Array) cannot be
        // annotated via key-index without panicking; preserve the original
        // value and log the artifact error instead.
        _ => {
            tracing::warn!(
                run_id,
                error = %err,
                "cannot annotate artifact_error into non-object observed_state"
            );
        }
    }
}

/// Advance a ready-to-resume wait: restore the resume point, mark the run and
/// lease `ReadyToResume`, and delete the consumed wait row.
///
/// The conditional lease transition must succeed before the run/wait-state
/// writes are committed. If the lease has already advanced to a terminal or
/// stale state (e.g. a concurrent poller pass classified it), the transaction
/// is rolled back so no partial state survives.
fn apply_ready_to_resume(
    tx: &rusqlite::Transaction<'_>,
    record: &WaitStateRecord,
) -> Result<(), PollApplyError> {
    if !delete_wait_state_for_suspension(tx, &record.run_id, &record.suspension_id)? {
        return Err(PollApplyError::WaitStateConcurrentTransition(
            record.run_id.clone(),
        ));
    }

    set_resume_point(tx, &record.run_id, &record.resume_step)?;
    mark_run_status(
        tx,
        &record.run_id,
        RunStatus::ReadyToResume,
        &record.resume_step,
        &[RunStatus::WaitingExternal],
    )?;
    if let Some(lease_id) = record.lease_id.as_deref() {
        let applied = update_lease_status_conditional(
            tx,
            lease_id,
            LeaseStatus::ReadyToResume,
            &[LeaseStatus::WaitingExternal, LeaseStatus::ReadyToResume],
            None,
            Some(&record.run_id),
        )?;
        if !applied {
            return Err(PollApplyError::LeaseTransitionRejected {
                run_id: record.run_id.clone(),
                lease_id: lease_id.to_string(),
                reason: READY_TRANSITION_REJECTED,
            });
        }
    }
    Ok(())
}

/// Apply a terminal (failure/timeout) classification: mark the run `Failed`,
/// conditionally advance the lease to `Failed`, and delete the wait row.
///
/// The run must still be `WaitingExternal`; if it has advanced, the stale
/// poll decision is rejected. The lease guard additionally rejects terminal or
/// foreign-owned leases, and either rejection rolls back the transaction.
fn apply_terminal_failure(
    tx: &rusqlite::Transaction<'_>,
    record: &WaitStateRecord,
) -> Result<(), PollApplyError> {
    if !delete_wait_state_for_suspension(tx, &record.run_id, &record.suspension_id)? {
        return Err(PollApplyError::WaitStateConcurrentTransition(
            record.run_id.clone(),
        ));
    }

    mark_run_status(
        tx,
        &record.run_id,
        RunStatus::Failed,
        &record.resume_step,
        &[RunStatus::WaitingExternal],
    )?;
    if let Some(lease_id) = record.lease_id.as_deref() {
        let applied = update_lease_status_conditional(
            tx,
            lease_id,
            LeaseStatus::Failed,
            &[
                LeaseStatus::WaitingExternal,
                LeaseStatus::ReadyToResume,
                LeaseStatus::Running,
                LeaseStatus::Claimed,
            ],
            None,
            Some(&record.run_id),
        )?;
        if !applied {
            return Err(PollApplyError::LeaseTransitionRejected {
                run_id: record.run_id.clone(),
                lease_id: lease_id.to_string(),
                reason: TERMINAL_TRANSITION_REJECTED,
            });
        }
    }
    Ok(())
}

/// Apply a still-waiting/transient-failure classification: refresh the wait
/// row's next-poll time and observed state, and conditionally hold the lease
/// at `WaitingExternal`. Returns `WaitStateConcurrentTransition` if the wait row was
/// concurrently deleted or a stale poller's optimistic `poll_count` version
/// guard rejected the refresh.
///
/// The wait-row update is guarded by an optimistic `poll_count` version so
/// that two concurrent still-waiting pollers reading the same `WaitingExternal`
/// lease cannot both commit their refreshes (last-writer-wins). Only the first
/// poller increments `poll_count`; the second (stale) poller's update matches
/// zero rows and the transaction rolls back.
///
/// The conditional lease transition must succeed before the transaction
/// commits. A still-waiting poll must not regress a lease that has already
/// advanced to `Running`, `ReadyToResume`, or a terminal state; the expected
/// source status is `WaitingExternal` only. If the lease has advanced, the
/// transaction is rolled back so the stale wait row is not refreshed.
fn apply_still_waiting(
    tx: &rusqlite::Transaction<'_>,
    record: &WaitStateRecord,
    decision: &PollDecision,
    now: DateTime<Utc>,
) -> Result<(), PollApplyError> {
    mark_run_status(
        tx,
        &record.run_id,
        RunStatus::WaitingExternal,
        &record.resume_step,
        &[RunStatus::WaitingExternal],
    )?;
    let next_poll_at = validated_next_poll_time(record, decision.next_poll_at, now);
    if !update_wait_state_after_poll(
        tx,
        &record.run_id,
        &decision.observed_state,
        next_poll_at,
        record.poll_count,
        &record.suspension_id,
    )? {
        return Err(PollApplyError::WaitStateConcurrentTransition(
            record.run_id.clone(),
        ));
    }
    if let Some(lease_id) = record.lease_id.as_deref() {
        // A still-waiting poll must only reaffirm a lease already in
        // WaitingExternal. Allowing Running here would let a stale poller
        // pull an actively-executing lease back to WaitingExternal, which
        // is inconsistent with the launcher's forward-only lifecycle.
        let applied = update_lease_status_conditional(
            tx,
            lease_id,
            LeaseStatus::WaitingExternal,
            &[LeaseStatus::WaitingExternal],
            None,
            Some(&record.run_id),
        )?;
        if !applied {
            return Err(PollApplyError::LeaseTransitionRejected {
                run_id: record.run_id.clone(),
                lease_id: lease_id.to_string(),
                reason: STILL_WAITING_TRANSITION_REJECTED,
            });
        }
    }
    Ok(())
}

pub(crate) fn validated_next_poll_time(
    record: &WaitStateRecord,
    candidate: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> DateTime<Utc> {
    candidate
        .filter(|next_poll_at| *next_poll_at > now)
        .unwrap_or_else(|| next_poll_time(record, now))
}

fn persist_poll_artifacts(
    record: &WaitStateRecord,
    decision: &PollDecision,
) -> Vec<ArtifactWarning> {
    let mut warnings = Vec::new();
    let mut attempt = |phase, result: Result<_, PersistenceError>| {
        if let Err(error) = result {
            tracing::warn!(
                run_id = %record.run_id,
                ?phase,
                error = %error,
                "failed to persist artifact after committed poll decision"
            );
            warnings.push(ArtifactWarning {
                phase,
                error: error.to_string(),
            });
        }
    };

    attempt(
        ArtifactPhase::PollResult,
        write_poll_result_artifact(&record.run_id, &json!(decision)),
    );
    if decision.classification == PollClassification::ReadyToResume {
        attempt(
            ArtifactPhase::WaitState,
            write_wait_state_artifact(&record.run_id, record),
        );
        attempt(
            ArtifactPhase::ResumeDecision,
            write_resume_decision_artifact(&record.run_id, decision),
        );
    }
    warnings
}

fn write_committed_pr_check_snapshot(
    record: &WaitStateRecord,
    observed_state: &Value,
) -> Result<(), crate::engine::runner::EngineError> {
    if record.wait_kind != WaitKind::PrChecks {
        return Ok(());
    }
    write_pr_check_status_snapshot(record, observed_state)
}

fn mark_run_status(
    conn: &rusqlite::Transaction<'_>,
    run_id: &str,
    status: RunStatus,
    step_id: &str,
    expected_statuses: &[RunStatus],
) -> Result<(), PollApplyError> {
    match persist_run_status_from_expected_in_transaction(
        conn,
        run_id,
        &status,
        Some(step_id),
        expected_statuses,
    )? {
        ExpectedRunStatusOutcome::Updated => Ok(()),
        ExpectedRunStatusOutcome::StatusMismatch => {
            Err(PollApplyError::RunStatusConcurrentTransition {
                run_id: run_id.to_string(),
                step_id: step_id.to_string(),
            })
        }
        ExpectedRunStatusOutcome::RunMissing => Err(PollApplyError::RunMissing {
            run_id: run_id.to_string(),
            step_id: step_id.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn next_poll_time_falls_back_from_non_future_candidates() {
        let now = DateTime::parse_from_rfc3339("2026-07-12T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut record = WaitStateRecord::new("run", "cfg");
        record.poll_interval_seconds = 1;

        for candidate in [None, Some(now - Duration::seconds(1)), Some(now)] {
            assert_eq!(
                validated_next_poll_time(&record, candidate, now),
                now + Duration::seconds(1)
            );
        }
        let future = now + Duration::minutes(10);
        assert_eq!(validated_next_poll_time(&record, Some(future), now), future);
    }
}
