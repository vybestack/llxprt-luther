use super::{EngineError, EngineRunner, EventType, RunOutcome};
use crate::persistence::persist_run_with_conn;
use crate::persistence::RunStatus;

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
        let status = match outcome {
            RunOutcome::Success => RunStatus::Completed,
            RunOutcome::Failure { .. } => RunStatus::Failed,
            RunOutcome::Abandoned { .. } => RunStatus::Abandoned,
            RunOutcome::Interrupted { .. } => RunStatus::Paused,
            RunOutcome::WaitingExternal { .. } => RunStatus::WaitingExternal,
        };
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
