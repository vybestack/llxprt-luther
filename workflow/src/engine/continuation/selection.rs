//! Checkpoint selection for operator continuations.
//!
//! Extracted from the parent continuation module to keep the resume/retry/rewind
//! checkpoint selection logic in a single cohesive unit.
//!
//! @plan:PLAN-20260623-LUTHER-CONTINUATION

use rusqlite::Connection;

use crate::persistence::{
    get_checkpoint_for_step, is_resumable_checkpoint_status, list_checkpoints,
    load_checkpoint_before_step, Checkpoint,
};

use super::{
    checkpoint_identity, parse_checkpoint_identity_target, ContinuationError, ContinuationKind,
    ContinuationRequest, RewindTarget, RunMetadata,
};

/// Retryable external-wait steps selected by `retry --from-failed-step`.
pub(super) const RETRYABLE_WAIT_STEPS: &[&str] = &["watch_pr_checks"];

/// Default terminal sink step for PR-followup workflows.
pub(crate) const TERMINAL_STEP: &str = "post_pr_failure_terminal";

pub(super) fn newest_resumable(checkpoints: &[Checkpoint]) -> Option<Checkpoint> {
    checkpoints
        .iter()
        .rev()
        .find(|c| is_resumable_checkpoint_status(&c.state_snapshot.status))
        .cloned()
}

pub(super) fn select_resume_checkpoint(
    conn: &Connection,
    run_id: &str,
    metadata: &RunMetadata,
) -> Result<Checkpoint, ContinuationError> {
    // Finding #2: When failure cleanup provenance records a
    // `failed_checkpoint_id`, select and verify it before falling back to
    // generic resume selection. This ensures incomplete cleanup failures
    // target the actual failed step rather than whatever happens to be the
    // newest resumable checkpoint.
    if let Some(cp) = select_failed_cleanup_checkpoint(conn, run_id, metadata)? {
        return Ok(cp);
    }
    let checkpoints = list_checkpoints(conn, run_id)?;
    if checkpoints.is_empty() {
        return Err(ContinuationError::NoResumableCheckpoint(run_id.to_string()));
    }
    if let Some(cp) = newest_resumable(&checkpoints) {
        return Ok(cp);
    }
    // Terminal failed run: rewind to the checkpoint just before the terminal step.
    let terminal_step = metadata.current_step.as_deref().unwrap_or(TERMINAL_STEP);
    if let Some(cp) = load_checkpoint_before_step(conn, run_id, terminal_step)? {
        return Ok(cp);
    }
    checkpoints
        .last()
        .cloned()
        .ok_or_else(|| ContinuationError::NoResumableCheckpoint(run_id.to_string()))
}

/// Select and verify the `failed_checkpoint_id` from failure-cleanup
/// provenance before generic resume/retry selection.
///
/// Returns `Ok(Some)` when a valid failure-cleanup record exists with a
/// `failed_checkpoint_id` that resolves to an actual persisted checkpoint.
/// Returns `Ok(None)` when there is no failure-cleanup provenance or the
/// `failed_checkpoint_id` does not resolve, allowing the caller to fall
/// back to its standard selection path.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub(super) fn select_failed_cleanup_checkpoint(
    conn: &Connection,
    run_id: &str,
    metadata: &RunMetadata,
) -> Result<Option<Checkpoint>, ContinuationError> {
    let Some(failure) = metadata.failure_cleanup.as_ref() else {
        return Ok(None);
    };
    if failure.failed_checkpoint_id.is_empty() {
        return Ok(None);
    }
    // Verify the persisted checkpoint actually exists and matches the recorded
    // identity before returning it, so a stale or tampered failed_checkpoint_id
    // cannot select an arbitrary resume point.
    let cp = select_rewind_checkpoint(
        conn,
        run_id,
        &RewindTarget::ToCheckpoint(failure.failed_checkpoint_id.clone()),
    )?;
    if checkpoint_identity(&cp) == failure.failed_checkpoint_id {
        Ok(Some(cp))
    } else {
        Ok(None)
    }
}

pub(super) fn select_retry_checkpoint(
    conn: &Connection,
    run_id: &str,
    metadata: &RunMetadata,
    from_failed_step: bool,
) -> Result<Checkpoint, ContinuationError> {
    // Finding #2: Prefer the verified failed_checkpoint_id from incomplete
    // cleanup provenance before generic retry selection. This applies to runs
    // where cleanup was attempted but did not fully succeed, ensuring the
    // retry targets the actual failure point.
    if let Some(cp) = select_failed_cleanup_checkpoint(conn, run_id, metadata)? {
        return Ok(cp);
    }
    if let Some(failure) = metadata.failure_cleanup.as_ref().filter(|failure| {
        metadata.is_cleanup_failure_abandonment() && failure.recovery_is_available()
    }) {
        return select_rewind_checkpoint(
            conn,
            run_id,
            &RewindTarget::ToCheckpoint(failure.failed_checkpoint_id.clone()),
        );
    }
    if from_failed_step {
        let checkpoints = list_checkpoints(conn, run_id)?;
        if let Some(cp) = checkpoints
            .iter()
            .rev()
            .find(|c| RETRYABLE_WAIT_STEPS.contains(&c.step_id.as_str()))
            .cloned()
        {
            return Ok(cp);
        }
        return Err(ContinuationError::NoResumableCheckpoint(format!(
            "{run_id} has no retryable external-wait checkpoint"
        )));
    }
    select_resume_checkpoint(conn, run_id, metadata)
}

pub(crate) fn select_rewind_checkpoint(
    conn: &Connection,
    run_id: &str,
    target: &RewindTarget,
) -> Result<Checkpoint, ContinuationError> {
    let (step, expected_ts) = match target {
        RewindTarget::ToStep(step) => (step.clone(), None),
        RewindTarget::ToCheckpoint(id) => {
            let (step, ts) = parse_checkpoint_identity_target(id)?;
            (step, Some(ts))
        }
    };
    let cp = get_checkpoint_for_step(conn, run_id, &step)?
        .ok_or_else(|| ContinuationError::CheckpointNotFound(format!("{run_id}:{step}")))?;
    if let Some(expected) = expected_ts {
        if cp.timestamp != expected {
            return Err(ContinuationError::InvalidTarget(format!(
                "checkpoint timestamp mismatch for {step}: stored {}, requested {}",
                cp.timestamp.to_rfc3339(),
                expected.to_rfc3339()
            )));
        }
    }
    Ok(cp)
}

/// Select the checkpoint a continuation request should resume from.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn select_checkpoint(
    conn: &Connection,
    request: &ContinuationRequest,
    metadata: &RunMetadata,
) -> Result<Checkpoint, ContinuationError> {
    match &request.kind {
        ContinuationKind::Resume => select_resume_checkpoint(conn, &request.run_id, metadata),
        ContinuationKind::Retry { from_failed_step } => {
            select_retry_checkpoint(conn, &request.run_id, metadata, *from_failed_step)
        }
        ContinuationKind::Rewind { target } => {
            select_rewind_checkpoint(conn, &request.run_id, target)
        }
    }
}
