use super::{EngineError, EngineRunner, EventType, RunOutcome};
use crate::persistence::persist_run_with_conn;
use crate::persistence::RunStatus;

/// Map a [`RunOutcome`] to the terminal [`RunStatus`] for a run, honoring the
/// merge-required completion semantics. [B12/C11]
///
/// A merge-required run reaches [`RunStatus::ReviewReady`] (NOT `Completed`)
/// after all steps succeed. `complete_typed_merge` then transitions
/// `ReviewReady → Merged` atomically with the typed artifact. Non-success
/// outcomes are unaffected by `merge_required`.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
#[must_use]
pub fn status_for_completion(outcome: &RunOutcome, merge_required: bool) -> RunStatus {
    match outcome {
        RunOutcome::Success => {
            if merge_required {
                crate::engine::recovery::typed_merge::runner_completion_for_merge_required()
            } else {
                RunStatus::Completed
            }
        }
        RunOutcome::Failure { .. } => RunStatus::Failed,
        RunOutcome::Abandoned { .. } => RunStatus::Abandoned,
        RunOutcome::Interrupted { .. } => RunStatus::Paused,
        RunOutcome::WaitingExternal { .. } => RunStatus::WaitingExternal,
    }
}

impl EngineRunner {
    /// Record run completion metadata to the persistence store.
    pub(super) fn record_run_completion(
        &self,
        outcome: &RunOutcome,
        final_step_id: &str,
    ) -> Result<(), EngineError> {
        if !self.persist_registry {
            return Ok(());
        }
        let mut status = status_for_completion(outcome, self.instance.config.merge_required);
        let mut metadata = self
            .load_metadata()
            .unwrap_or_else(|| self.build_metadata(status.clone()));
        metadata.status = status.clone();
        metadata.set_current_step(final_step_id);
        {
            let conn = self.conn.borrow();
            persist_run_with_conn(&conn, &metadata).map_err(|error| {
                EngineError::PersistenceError(format!("Failed to record run completion: {error}"))
            })?;
            if status == RunStatus::ReviewReady {
                use crate::engine::recovery::merge_completion::{
                    complete_merge_required_run, MergeCompletionOutcome, SystemMergeProbeFactory,
                };
                match complete_merge_required_run(
                    &conn,
                    &self.instance.run_id,
                    self.context.work_dir(),
                    &SystemMergeProbeFactory::new(),
                ) {
                    MergeCompletionOutcome::Merged => status = RunStatus::Merged,
                    MergeCompletionOutcome::NotYetMerged => return Ok(()),
                    other => {
                        return Err(EngineError::PersistenceError(format!(
                            "typed merge completion failed: {other:?}"
                        )))
                    }
                }
            }
        }
        self.record_event(
            EventType::TerminalState,
            final_step_id,
            &status.to_string(),
            None,
        );
        Ok(())
    }
}
