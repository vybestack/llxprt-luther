//! Authorization for resuming checkpoints outside `SAFE_RERUN_STEPS`.
//!
//! Architecturally typed authorization distinguishing operator-initiated
//! continuations from trusted-internal engine resumptions.
//!
//! @plan:PLAN-20260623-LUTHER-CONTINUATION

use rusqlite::Connection;

use crate::persistence::{Checkpoint, RunMetadata, RunStatus};

use super::{checkpoint_identity, ContinuationKind, ContinuationRequest};

/// Architecturally typed authorization distinguishing operator-initiated
/// continuations from trusted-internal engine resumptions.
///
/// This type permits a non-`SAFE_RERUN_STEPS` step only when continuation is
/// bound to an exact durable current wait for the same run. It is never
/// constructable from a CLI `--force` flag, so ambiguous replays continue to
/// fail closed.
///
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeAuthorization {
    /// Operator-initiated continuation (CLI `runs resume/retry/rewind`).
    /// Subject to all rerun-safety rules; `--force` does not bypass
    /// `safe_step`.
    Operator,
    /// Operator authorization for the exact current waiting checkpoint. This
    /// is not a generic force bypass: run status, current step, checkpoint
    /// status, and exact identity are all revalidated transactionally.
    OperatorCurrentWait {
        checkpoint_identity: String,
        run_id: String,
    },
    /// Engine-internal authorization bound to an exact persisted waiting
    /// checkpoint identity and run. Permits resuming a valid durable wait
    /// whose step is not in `SAFE_RERUN_STEPS` without exposing a generic
    /// operator bypass. The binding is verified against the durable
    /// `wait_states` row at validation and again inside the commit
    /// transaction so a stale or substituted checkpoint cannot be elevated.
    TrustedInternalWait {
        checkpoint_identity: String,
        run_id: String,
    },
}

impl ResumeAuthorization {
    /// Resolve the strongest authorization applicable to resuming `checkpoint`
    /// for `run_id` from the persisted durable wait state.
    ///
    /// Returns [`ResumeAuthorization::TrustedInternalWait`] only when a
    /// complete `wait_states` row exists for the exact `run_id`, its
    /// `checkpoint_id` matches the selected checkpoint identity, and its
    /// `resume_step` matches the checkpoint's step. Otherwise the caller is
    /// treated as a plain [`ResumeAuthorization::Operator`].
    ///
    /// @plan:PLAN-20260623-LUTHER-CONTINUATION
    fn for_resume(
        conn: &Connection,
        metadata: &RunMetadata,
        request: &ContinuationRequest,
        checkpoint: &Checkpoint,
    ) -> ResumeAuthorization {
        if !matches!(request.kind, ContinuationKind::Resume) {
            return ResumeAuthorization::Operator;
        }
        let identity = checkpoint_identity(checkpoint);
        if !request.trusted_internal
            && metadata.current_step.as_deref() == Some(checkpoint.step_id.as_str())
            && metadata.status == RunStatus::WaitingExternal
            && checkpoint.state_snapshot.status == crate::persistence::CHECKPOINT_STATUS_WAITING
        {
            return ResumeAuthorization::OperatorCurrentWait {
                checkpoint_identity: identity,
                run_id: request.run_id.clone(),
            };
        }
        // TrustedInternalWait requires an explicit internal-trust capability
        // (`request.trusted_internal`) in addition to a matching durable wait
        // state. This ensures an ordinary CLI `runs resume` cannot infer
        // internal trust from durable wait state alone — only the daemon
        // launcher and parent-orchestration child-resume paths set this flag.
        if !request.trusted_internal {
            return ResumeAuthorization::Operator;
        }
        let trusted = crate::persistence::get_wait_state(conn, &request.run_id)
            .ok()
            .flatten()
            .is_some_and(|wait| {
                wait.run_id == request.run_id
                    && wait.checkpoint_id == identity
                    && wait.resume_step == checkpoint.step_id
            });
        if trusted {
            ResumeAuthorization::TrustedInternalWait {
                checkpoint_identity: identity,
                run_id: request.run_id.clone(),
            }
        } else {
            ResumeAuthorization::Operator
        }
    }

    /// Whether this authorization permits resuming `checkpoint` despite its
    /// step not being in `SAFE_RERUN_STEPS`.
    ///
    /// Only a typed grant bound to the exact checkpoint identity and run
    /// authorizes the bypass. Generic [`ResumeAuthorization::Operator`]
    /// authorization never does.
    fn permits_non_safe_rerun(&self, checkpoint: &Checkpoint) -> bool {
        match self {
            ResumeAuthorization::OperatorCurrentWait {
                checkpoint_identity: bound_identity,
                run_id: bound_run_id,
            }
            | ResumeAuthorization::TrustedInternalWait {
                checkpoint_identity: bound_identity,
                run_id: bound_run_id,
            } => {
                *bound_identity == checkpoint_identity(checkpoint)
                    && bound_run_id == &checkpoint.run_id
            }
            ResumeAuthorization::Operator => false,
        }
    }
}

pub(super) fn authorizes_cleanup_resume(
    metadata: &RunMetadata,
    request: &ContinuationRequest,
    checkpoint: &Checkpoint,
) -> bool {
    metadata.failure_cleanup.as_ref().is_some_and(|failure| {
        if !matches!(request.kind, ContinuationKind::Resume) {
            return false;
        }
        let exact_failed_checkpoint =
            checkpoint_identity(checkpoint) == failure.failed_checkpoint_id;
        (!failure.cleanup_succeeded
            && (exact_failed_checkpoint || checkpoint.step_id == failure.cleanup_step))
            || (metadata.status == RunStatus::Running
                && failure.is_complete()
                && failure.recovery_consumed_at.is_some()
                && exact_failed_checkpoint)
    })
}

/// Compute whether `checkpoint` is authorized to re-run outside
/// `SAFE_RERUN_STEPS` via either cleanup-recovery provenance or a
/// trusted-internal durable-wait authorization.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub(super) fn checkpoint_is_authorized(
    conn: &Connection,
    metadata: &RunMetadata,
    request: &ContinuationRequest,
    checkpoint: &Checkpoint,
) -> bool {
    let authorized_failed_checkpoint = metadata
        .failure_cleanup
        .as_ref()
        .filter(|failure| {
            metadata.is_cleanup_failure_abandonment() && failure.recovery_is_available()
        })
        .is_some_and(|failure| checkpoint_identity(checkpoint) == failure.failed_checkpoint_id)
        || authorizes_cleanup_resume(metadata, request, checkpoint);
    authorized_failed_checkpoint
        || ResumeAuthorization::for_resume(conn, metadata, request, checkpoint)
            .permits_non_safe_rerun(checkpoint)
}
