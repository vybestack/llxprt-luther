//! Failure-cleanup and recovery helpers extracted from the runner.
//!
//! These methods govern the durable `FailureCleanupState` lifecycle: capturing
//! provenance when a step routes into a `failure_cleanup` terminal step,
//! protecting the issue lease during cleanup, persisting terminal/intermediate
//! failure state, classifying public failure reasons (never raw diagnostics),
//! and resolving the terminal `RunOutcome` when a step has no transition.
//!
//! The methods are `impl super::EngineRunner` blocks so callers see no API
//! change; they remain addressable as `self.method()` / `EngineRunner::method`.
use rusqlite::Connection;

use crate::engine::transition::StepOutcome;
use crate::persistence::{
    append_typed_event_with_conn, persist_run_with_conn, save_checkpoint_with_conn, EventType,
    FailureCleanupState, RunMetadata, RunStatus, CHECKPOINT_STATUS_WAITING,
};

use super::support::run_outcome_without_transition;
use super::{EngineError, RunOutcome};

impl super::EngineRunner {
    pub(super) fn persist_registry_step_state(
        &self,
        conn: &Connection,
        current_step_id: &str,
        outcome: &StepOutcome,
        failure: Option<&FailureCleanupState>,
        terminal_failure: Option<&FailureCleanupState>,
    ) -> Result<(), EngineError> {
        let Some(mut metadata) = crate::persistence::get_run_with_conn(conn, &self.instance.run_id)
            .map_err(|error| EngineError::PersistenceError(error.to_string()))?
        else {
            return Err(EngineError::PersistenceError(format!(
                "persistent run {} is missing from the registry",
                self.instance.run_id
            )));
        };
        metadata.set_previous_step_and_outcome(current_step_id, outcome.to_string());
        metadata.set_next_step_candidates(self.compute_next_step_candidates(current_step_id));
        if let Some(state) = terminal_failure {
            metadata.failure_cleanup = Some(state.clone());
            metadata.status = if *outcome == StepOutcome::Success {
                RunStatus::Abandoned
            } else {
                RunStatus::Failed
            };
            metadata.set_current_step(&state.failed_step);
            append_typed_event_with_conn(
                conn,
                &self.instance.run_id,
                &state.failed_step,
                &metadata.status.to_string(),
                EventType::TerminalState,
                None,
                chrono::Utc::now(),
            )?;
        } else if let Some(state) = failure {
            metadata.failure_cleanup = Some(state.clone());
        }
        if failure.is_some() || terminal_failure.is_some() {
            self.protect_failure_cleanup_lease(conn, &metadata)?;
        }
        persist_run_with_conn(conn, &metadata)
            .map_err(|error| EngineError::PersistenceError(error.to_string()))
    }

    pub(super) fn protect_failure_cleanup_lease(
        &self,
        conn: &Connection,
        metadata: &RunMetadata,
    ) -> Result<(), EngineError> {
        let Some(repository) = metadata.repository.as_deref() else {
            return Ok(());
        };
        let Some(issue_number) = metadata
            .issue_number
            .or(metadata.pr_number)
            .and_then(|number| u64::try_from(number).ok())
        else {
            return Ok(());
        };
        let Some(lease) = crate::persistence::get_lease_for_issue(conn, repository, issue_number)
            .map_err(|error| EngineError::PersistenceError(error.to_string()))?
        else {
            return Err(EngineError::PersistenceError(format!(
                "issue lease for {repository}#{issue_number} is missing; cannot protect \
                 failure cleanup for run {} without a durable lease",
                self.instance.run_id
            )));
        };
        if lease.run_id.as_deref() != Some(self.instance.run_id.as_str()) {
            return Err(EngineError::PersistenceError(format!(
                "issue lease {} is not owned by run {}",
                lease.lease_id, self.instance.run_id
            )));
        }
        let protected = crate::persistence::update_lease_status_conditional(
            conn,
            &lease.lease_id,
            crate::persistence::LeaseStatus::CleanupAbandoned,
            &[
                crate::persistence::LeaseStatus::Running,
                crate::persistence::LeaseStatus::CleanupAbandoned,
            ],
            None,
            Some(&self.instance.run_id),
        )
        .map_err(|error| EngineError::PersistenceError(error.to_string()))?;
        if !protected {
            return Err(EngineError::PersistenceError(format!(
                "issue lease {} could not be protected for failure cleanup",
                lease.lease_id
            )));
        }
        Ok(())
    }

    pub(super) fn persist_step_result(
        &mut self,
        current_step_id: &str,
        outcome: &StepOutcome,
        next_step: Option<&str>,
    ) -> Result<(), EngineError> {
        if next_step.is_some_and(|next| {
            *outcome != StepOutcome::Success && self.is_failure_cleanup_step(next)
        }) {
            if let Some(workspace) = self.context.get("work_dir").map(std::path::Path::new) {
                crate::engine::continuation::write_workspace_owner_marker(
                    workspace,
                    &self.instance.run_id,
                )
                .map_err(|error| {
                    EngineError::PersistenceError(format!(
                        "establish workspace ownership before failure cleanup: {error}"
                    ))
                })?;
            }
        }
        let checkpoint = self.create_checkpoint(current_step_id, "completed");
        let failure = next_step
            .filter(|next| *outcome != StepOutcome::Success && self.is_failure_cleanup_step(next))
            .map(|next| {
                self.build_failure_cleanup_state(current_step_id, next, outcome, &checkpoint)
            });
        let terminal_failure = if self.is_failure_cleanup_step(current_step_id) {
            let mut state = self
                .load_metadata()
                .and_then(|metadata| metadata.failure_cleanup)
                .or_else(|| self.pending_failure_cleanup.clone())
                .ok_or_else(|| {
                    EngineError::PersistenceError(
                        "failure cleanup completed without failed-work provenance".to_string(),
                    )
                })?;
            if *outcome == StepOutcome::Success {
                state.cleanup_succeeded = true;
                state.cleanup_completed_at = Some(chrono::Utc::now());
            }
            Some(state)
        } else {
            None
        };
        let conn = self.conn.borrow();
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|error| EngineError::PersistenceError(error.to_string()))?;
        save_checkpoint_with_conn(&tx, &checkpoint)?;
        append_typed_event_with_conn(
            &tx,
            &self.instance.run_id,
            current_step_id,
            &outcome.to_string(),
            EventType::StepOutcome,
            None,
            chrono::Utc::now(),
        )?;
        if self.persist_registry {
            self.persist_registry_step_state(
                &tx,
                current_step_id,
                outcome,
                failure.as_ref(),
                terminal_failure.as_ref(),
            )?;
        }
        tx.commit()
            .map_err(|error| EngineError::PersistenceError(error.to_string()))?;
        if let Some(state) = terminal_failure {
            self.pending_failure_cleanup = Some(state);
        } else if failure.is_some() {
            self.pending_failure_cleanup = failure;
        }
        Ok(())
    }

    /// Produce a typed, bounded public failure reason from executor-provided
    /// categorical context, never from raw diagnostic text.
    ///
    /// Only structurally-safe categorical `llxprt_failure_reason` values
    /// (lowercase `snake_case` identifiers controlled by executor source code)
    /// are eligible for durable public provenance. Raw executor diagnostic text
    /// is never persisted in `FailureCleanupState`; it remains only in protected
    /// event logs and ephemeral runtime output. This prevents durable secret
    /// leakage regardless of the secret format (bearer headers, URL userinfo,
    /// JSON fields, mixed case, Unicode, quoted whitespace, etc.) because the
    /// raw text never enters the public provenance path.
    pub(super) fn public_failure_reason(
        failed_step: &str,
        outcome: &StepOutcome,
        llxprt_category: Option<&str>,
    ) -> String {
        match llxprt_category {
            Some(category) if Self::is_safe_failure_category(category) => {
                format!("{category} ({outcome} at {failed_step})")
            }
            _ => format!("{outcome} outcome at {failed_step}"),
        }
    }

    /// Validate that a failure category is a known, bounded identifier emitted
    /// by executor source code — never raw diagnostic text that could carry
    /// secrets.
    ///
    /// Uses an **explicit allowlist** of the categories actually set by the
    /// executors (see `engine/executors/llxprt.rs`), rather than a structural
    /// `snake_case` check. Any new executor that needs a new category must add
    /// it here, which forces an explicit review rather than silently accepting
    /// arbitrary `snake_case` text. This is stricter than the structural check:
    /// a secret that happens to be lowercase `snake_case` (e.g. `bearer_abc`)
    /// is rejected because it is not in the allowlist.
    pub(super) fn is_safe_failure_category(category: &str) -> bool {
        matches!(
            category,
            "process_error"
                | "agent_failure"
                | "no_diff"
                | "idle_timeout"
                | "timeout"
                | "push_failure"
                | "validation_failure"
        )
    }

    pub(super) fn build_failure_cleanup_state(
        &self,
        failed_step: &str,
        next_step: &str,
        outcome: &StepOutcome,
        checkpoint: &crate::persistence::Checkpoint,
    ) -> FailureCleanupState {
        let llxprt_category = self
            .context
            .get(&format!("{failed_step}.llxprt_failure_reason"))
            .map(String::as_str);
        FailureCleanupState {
            schema_version: FailureCleanupState::SCHEMA_VERSION,
            failed_step: failed_step.to_string(),
            failure_outcome: outcome.to_string(),
            failure_reason: Self::public_failure_reason(failed_step, outcome, llxprt_category),
            failed_checkpoint_id: format!("{failed_step}@{}", checkpoint.timestamp.to_rfc3339()),
            failed_state_snapshot: checkpoint.state_snapshot.clone(),
            cleanup_step: next_step.to_string(),
            cleanup_succeeded: false,
            captured_at: chrono::Utc::now(),
            cleanup_completed_at: None,
            recovery_consumed_at: None,
        }
    }

    pub(super) fn is_failure_cleanup_step(&self, step_id: &str) -> bool {
        self.instance.workflow_type.steps.iter().any(|step| {
            step.step_id == step_id
                && step.terminal == Some(true)
                && step.step_type == "failure_cleanup"
        })
    }

    pub(super) fn finish_without_transition(
        &mut self,
        current_step_id: &str,
        outcome: &StepOutcome,
    ) -> Result<RunOutcome, EngineError> {
        if self.is_failure_cleanup_step(current_step_id) {
            let failure = self
                .load_metadata()
                .and_then(|md| md.failure_cleanup)
                .or_else(|| self.pending_failure_cleanup.clone())
                .ok_or_else(|| {
                    EngineError::PersistenceError(
                        "failure cleanup completed without failed-work provenance".to_string(),
                    )
                })?;
            if *outcome == StepOutcome::Success {
                let run_outcome = RunOutcome::Abandoned {
                    step_id: failure.failed_step.clone(),
                    reason: failure.failure_reason.clone(),
                };
                self.pending_failure_cleanup = Some(failure);
                return Ok(run_outcome);
            }
            let run_outcome = RunOutcome::Failure {
                step_id: failure.failed_step.clone(),
                reason: format!(
                    "failure cleanup step {} ended with {outcome}; original failure: {}",
                    failure.cleanup_step, failure.failure_reason
                ),
            };
            self.pending_failure_cleanup = Some(failure);
            return Ok(run_outcome);
        }
        if *outcome == StepOutcome::Wait {
            return self.pause_for_external_wait(current_step_id);
        }
        if *outcome == StepOutcome::Retryable {
            let run_outcome = RunOutcome::Failure {
                step_id: current_step_id.to_string(),
                reason: "Retryable error with no recovery transition".to_string(),
            };
            self.record_run_completion(&run_outcome, current_step_id)?;
            return Ok(run_outcome);
        }

        let run_outcome = run_outcome_without_transition(current_step_id, outcome);
        self.record_run_completion(&run_outcome, current_step_id)?;
        Ok(run_outcome)
    }

    /// Persist a resumable `waiting` checkpoint at the current step and return
    /// a non-advancing `WaitingExternal` outcome. The resume point is the wait
    /// step itself, so a later resume re-enters it and refreshes external state.
    /// @plan:PLAN-20260623-LUTHER-CONTINUATION
    pub(super) fn pause_for_external_wait(&self, step_id: &str) -> Result<RunOutcome, EngineError> {
        let checkpoint = self.create_checkpoint(step_id, CHECKPOINT_STATUS_WAITING);
        {
            let conn = self.conn.borrow();
            save_checkpoint_with_conn(&conn, &checkpoint)?;
        }
        let run_outcome = RunOutcome::WaitingExternal {
            step_id: step_id.to_string(),
            reason: "External condition still pending at watch limit".to_string(),
        };
        self.record_run_completion(&run_outcome, step_id)?;
        Ok(run_outcome)
    }
}
