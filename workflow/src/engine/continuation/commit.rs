//! Transactional commit of a continuation: re-stamp the resume point and reopen
//! the run record.
//!
//! Extracted from the parent continuation module to keep the transactional
//! commit path (including the continuation lease acquisition) in a single
//! cohesive unit.
//!
//! @plan:PLAN-20260623-LUTHER-CONTINUATION

use std::path::Path;

use chrono::Utc;
use rusqlite::{Connection, Transaction, TransactionBehavior};

use crate::persistence::{
    append_typed_event_with_conn, delete_wait_state_for_suspension, get_run_with_conn,
    get_wait_state, persist_run_with_conn, set_resume_point, EventType, RunMetadata, RunStatus,
};

use super::{
    authorization::checkpoint_is_authorized,
    validation::{check_cleanup_workspace_ownership, check_safe_step, reopen_status_is_allowed},
    ContinuationError, ContinuationKind, ContinuationRequest,
};

/// Re-stamp the selected checkpoint as the resume point and reopen the run
/// record, appending an audit event. History (events, prior checkpoint rows)
/// is preserved.
///
/// `bound_identity` must be the exact `step_id@rfc3339` identity bound at
/// plan time (see [`super::ContinuationPlan::checkpoint_identity`]). It is compared
/// against the freshly re-selected checkpoint inside the `IMMEDIATE`
/// transaction **before any lease or run mutation**, so a concurrent same-step
/// checkpoint replacement between plan and commit is rejected with no durable
/// state change.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn commit_continuation(
    conn: &Connection,
    request: &ContinuationRequest,
    bound_identity: &str,
) -> Result<RunMetadata, ContinuationError> {
    crate::persistence::leases::init_leases_table(conn)?;
    crate::persistence::init_wait_states_table(conn)?;
    // `conn` is intentionally not reused until `tx` commits or rolls back.
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    match commit_continuation_in_transaction(&tx, request, bound_identity) {
        Ok(metadata) => {
            tx.commit()?;
            Ok(metadata)
        }
        Err(err) => match tx.rollback() {
            Ok(()) => Err(err),
            Err(rollback_err) => {
                tracing::warn!(
                    error = %err,
                    rollback_error = %rollback_err,
                    "rollback failed after continuation commit error"
                );
                Err(ContinuationError::Persistence(format!(
                    "rollback failed after continuation commit error: original={err}; rollback={rollback_err}"
                )))
            }
        },
    }
}

fn commit_continuation_in_transaction(
    tx: &Transaction<'_>,
    request: &ContinuationRequest,
    bound_identity: &str,
) -> Result<RunMetadata, ContinuationError> {
    let mut metadata = get_run_with_conn(tx, &request.run_id)?
        .ok_or_else(|| ContinuationError::RunNotFound(request.run_id.clone()))?;
    ensure_reopen_claim_is_available(tx, &metadata, request)?;
    let selected = super::select_checkpoint(tx, request, &metadata)?;
    // TOCTOU defense: compare the exact checkpoint identity (step@timestamp)
    // bound at plan time against the freshly re-selected checkpoint inside the
    // IMMEDIATE transaction, BEFORE any lease or run mutation. A concurrent
    // same-step checkpoint replacement produces a different timestamp and must
    // be rejected here so no durable state is mutated.
    let current_identity = super::checkpoint_identity(&selected);
    if current_identity != bound_identity {
        return Err(ContinuationError::InvalidTarget(format!(
            "continuation checkpoint identity changed before commit: expected {bound_identity}, selected {current_identity}"
        )));
    }
    if matches!(request.kind, ContinuationKind::Resume) && !request.trusted_internal {
        if let Some(wait) = get_wait_state(tx, &request.run_id)? {
            if wait.checkpoint_id != current_identity {
                return Err(ContinuationError::InvalidTarget(format!(
                    "durable wait checkpoint identity changed before commit: expected {current_identity}, found {}",
                    wait.checkpoint_id
                )));
            }
            if wait.resume_step != selected.step_id {
                return Err(ContinuationError::InvalidTarget(format!(
                    "durable wait resume step changed before commit: expected {}, found {}",
                    selected.step_id, wait.resume_step
                )));
            }
            if !delete_wait_state_for_suspension(tx, &request.run_id, &wait.suspension_id)? {
                return Err(ContinuationError::InvalidTarget(
                    "durable wait suspension changed before commit".to_string(),
                ));
            }
        }
    }
    let authorized = checkpoint_is_authorized(tx, &metadata, request, &selected);
    let safety = check_safe_step(&selected.step_id, request.force, authorized);
    if !safety.passed {
        return Err(ContinuationError::InvalidTarget(safety.detail));
    }
    let resume_timestamp = set_resume_point(tx, &request.run_id, &selected.step_id)?;
    if let Some(failure) = metadata.failure_cleanup.as_mut() {
        if failure.failed_checkpoint_id == current_identity {
            failure.failed_checkpoint_id =
                format!("{}@{}", selected.step_id, resume_timestamp.to_rfc3339());
        }
    }
    reopen_run(tx, request, &selected.step_id, metadata)
}

fn reopen_run(
    conn: &Connection,
    request: &ContinuationRequest,
    step_id: &str,
    mut metadata: RunMetadata,
) -> Result<RunMetadata, ContinuationError> {
    let prior_status = metadata.status.clone();
    if metadata.is_cleanup_failure_abandonment() {
        if let Some(failure) = metadata.failure_cleanup.as_mut() {
            failure.recovery_consumed_at = Some(Utc::now());
        }
    }
    metadata.reopen();
    // Finding #4: When reopening from ReadyToResume (the daemon handoff
    // path), clear child PIDs from the prior lifecycle to avoid a live PID
    // handoff race. The prior run's child/agent processes are no longer
    // owned by this continuation; leaving them would let a concurrent daemon
    // launcher observe a live child PID and conclude the run is still being
    // actively processed, creating ambiguous ownership semantics.
    if prior_status == RunStatus::ReadyToResume {
        metadata.clear_child_pids();
    }
    metadata.set_current_step(step_id);
    persist_run_with_conn(conn, &metadata)?;
    let failure_identity = metadata
        .failure_cleanup
        .as_ref()
        .map_or("none", |failure| failure.failed_checkpoint_id.as_str());
    let detail = format!(
        "continuation={} force={} from_status={prior_status} resume_step={step_id} failure_checkpoint={failure_identity}",
        request.kind.verb(), request.force
    );
    append_typed_event_with_conn(
        conn,
        &request.run_id,
        step_id,
        "reopened",
        EventType::StepStart,
        Some(&detail),
        Utc::now(),
    )?;
    Ok(metadata)
}

fn ensure_reopen_claim_is_available(
    conn: &Connection,
    metadata: &RunMetadata,
    request: &ContinuationRequest,
) -> Result<(), ContinuationError> {
    // This runs inside the IMMEDIATE transaction after re-reading metadata, so
    // a second concurrent continuation attempt observes the first claim's PID
    // before deciding whether the Running record is still available.
    if !reopen_status_is_allowed(metadata, request) {
        return Err(ContinuationError::InvalidTarget(format!(
            "run {} status {} is not resumable; terminal states other than failed cannot be continued",
            request.run_id, metadata.status
        )));
    }
    acquire_continuation_lease(conn, metadata, request)?;
    if let Some(pid) = metadata
        .process_pid
        .filter(|_| metadata.status == RunStatus::Running)
        .filter(|pid| !crate::persistence::is_pid_stale(*pid))
    {
        return Err(ContinuationError::InvalidTarget(format!(
            "run {} is already running with live workflow PID {pid}",
            request.run_id
        )));
    }
    if let Some(pid) = metadata
        .child_pids
        .iter()
        .copied()
        .find(|pid| !crate::persistence::is_pid_stale(*pid))
    {
        return Err(ContinuationError::InvalidTarget(format!(
            "run {} still has live child PID {pid}",
            request.run_id
        )));
    }
    if metadata.is_cleanup_failure_abandonment() {
        if let Some(path_str) = metadata.workspace_path.as_ref() {
            let workspace_check = check_cleanup_workspace_ownership(Path::new(path_str), metadata);
            if !workspace_check.passed {
                return Err(ContinuationError::InvalidTarget(format!(
                    "run {}: {}",
                    request.run_id, workspace_check.detail
                )));
            }
        } else {
            return Err(ContinuationError::InvalidTarget(format!(
                "run {} preserved workspace is missing or invalid",
                request.run_id
            )));
        }
    }
    Ok(())
}

fn acquire_continuation_lease(
    conn: &Connection,
    metadata: &RunMetadata,
    request: &ContinuationRequest,
) -> Result<(), ContinuationError> {
    let (Some(repository), Some(issue_number)) = (
        metadata.repository.as_deref(),
        metadata
            .issue_number
            .and_then(|number| u64::try_from(number).ok()),
    ) else {
        return Ok(());
    };
    let Some(lease) =
        crate::persistence::leases::get_lease_for_issue(conn, repository, issue_number)?
    else {
        // The run has repository + issue identity, so a lease is expected. A
        // missing lease means the durable claim was lost (DB corruption, manual
        // deletion, or a race that allowed another dispatcher to reclaim it).
        // Fail closed rather than allowing an untracked continuation: every
        // continuation of an issue-bound run must be backed by a lease that this
        // transaction authoritatively acquires.
        return Err(ContinuationError::InvalidTarget(format!(
            "run {} references {repository}#{issue_number} but no issue lease exists; \
             the durable claim is missing and continuation cannot proceed authoritatively",
            request.run_id
        )));
    };
    let expected_owner = lease.run_id.as_deref().ok_or_else(|| {
        ContinuationError::InvalidTarget(format!(
            "issue lease {} is active without a run owner",
            lease.lease_id
        ))
    })?;
    if expected_owner != request.run_id {
        return Err(ContinuationError::InvalidTarget(format!(
            "issue lease {} belongs to run {} rather than {}",
            lease.lease_id, expected_owner, request.run_id
        )));
    }
    let expected_statuses = [
        crate::persistence::LeaseStatus::Claimed,
        crate::persistence::LeaseStatus::Running,
        crate::persistence::LeaseStatus::WaitingExternal,
        crate::persistence::LeaseStatus::ReadyToResume,
        crate::persistence::LeaseStatus::Failed,
        crate::persistence::LeaseStatus::Abandoned,
        crate::persistence::LeaseStatus::CleanupAbandoned,
        crate::persistence::LeaseStatus::Stale,
    ];
    let target_status = if matches!(request.kind, ContinuationKind::Rewind { .. }) {
        crate::persistence::LeaseStatus::ReadyToResume
    } else {
        crate::persistence::LeaseStatus::Running
    };
    let acquired = crate::persistence::leases::update_lease_status_conditional(
        conn,
        &lease.lease_id,
        target_status,
        &expected_statuses,
        Some(&request.run_id),
        Some(&request.run_id),
    )?;
    if !acquired {
        return Err(ContinuationError::InvalidTarget(format!(
            "issue lease {} could not be acquired for continuation",
            lease.lease_id
        )));
    }
    Ok(())
}
