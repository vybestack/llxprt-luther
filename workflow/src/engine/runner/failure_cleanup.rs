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
use super::{EngineError, OwnershipFailureDetails, RunOutcome};

/// Outcome of verifying workspace ownership before entering a
/// `failure_cleanup` step.
///
/// - [`OwnershipVerification::NotApplicable`]: no cleanup step is being
///   entered, no workspace is configured, or the workspace is not
///   ownership-managed and has no evidence. The caller proceeds normally.
/// - [`OwnershipVerification::Owned`]: the workspace is ownership-managed and
///   ownership verifies. The cleanup step may execute its shell script.
///
/// An ownership *failure* is returned as `Err(EngineError::OwnershipFailure)`,
/// never as a `NotApplicable`/`Owned` variant, so the caller can never mistake
/// an unreadable/foreign workspace for a trusted one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OwnershipVerification {
    /// Ownership verification is not applicable to this transition.
    NotApplicable,
    /// The workspace is ownership-managed and ownership verifies.
    Owned,
}

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
        // Issue 158: all cleanup lease operations must require the immutable
        // daemon_managed_claim authority. A one-shot (non-daemon) CLI run has
        // no daemon-managed claim and must never touch a lease. The flag is
        // read from the immutable runtime provenance accessor, not from the
        // mutable config variables, so a shell step cannot mutate it to gain
        // lease authority.
        if !self.context.daemon_managed_claim() {
            return Ok(());
        }
        let Some(repository) = metadata.repository.as_deref() else {
            return Ok(());
        };
        // Issue 158 slice 5: lease authority requires the immutable issue
        // number, never a PR number. A PR-only run has no issue lease and must
        // not advance one.
        let Some(issue_number) = metadata.issue_lease_number() else {
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

    /// Set the matching daemon lease directly to `Failed` for an
    /// ownership-denied terminal.
    ///
    /// Unlike [`protect_failure_cleanup_lease`], this never sets the lease to
    /// `CleanupAbandoned`, because an ownership-denied workspace is unowned
    /// and cleanup continuation must never be selected for it. The transition
    /// is an exact-owner `Running` CAS so a concurrent writer that reassigned
    /// the lease cannot be overwritten by this stale ownership-denied write.
    ///
    /// A missing lease is valid for one-shot (non-daemon) CLI runs: no lease
    /// is required for a non-daemon terminal, and the terminal metadata is not
    /// rolled back. For daemon-managed runs the lease is expected to exist;
    /// if it is missing or owned by a different run, the run still terminates
    /// with ownership-denied terminal metadata, and the missing/foreign lease
    /// is recorded as a persistence diagnostic rather than rolling back the
    /// terminal failure.
    fn fail_owned_lease_for_ownership_denial(
        &self,
        conn: &Connection,
        metadata: &RunMetadata,
    ) -> Result<(), EngineError> {
        // Issue 158 finding 5: gate the ownership-denial lease mutation on the
        // immutable daemon_managed flag plus the exact lease owner. A one-shot
        // (non-daemon) CLI run must never touch a lease, because it has no
        // daemon-managed claim authority. Only a daemon-managed run that owns
        // the exact lease may fail it. The daemon_managed flag is read from
        // the run context (the immutable runtime provenance set at runner
        // construction), not from the mutable config variables, so a shell
        // step cannot mutate it to gain lease authority.
        if !self.context.daemon_managed_claim() {
            return Ok(());
        }
        let Some(repository) = metadata.repository.as_deref() else {
            return Ok(());
        };
        // Issue 158 slice 5: lease authority requires the immutable issue
        // number, never a PR number. A PR-only run has no issue lease to fail.
        let Some(issue_number) = metadata.issue_lease_number() else {
            return Ok(());
        };
        let Some(lease) = crate::persistence::get_lease_for_issue(conn, repository, issue_number)
            .map_err(|error| EngineError::PersistenceError(error.to_string()))?
        else {
            // No lease for this issue. For a daemon-managed run the lease is
            // expected to exist; if it is missing, the run still terminates
            // with ownership-denied terminal metadata, and the missing lease
            // is not rolled back.
            return Ok(());
        };
        // Only the exact owner may fail its own lease. A lease owned by a
        // different run is left intact (it cannot be mutated by this run), and
        // the ownership-denied terminal metadata is preserved.
        if lease.run_id.as_deref() != Some(self.instance.run_id.as_str()) {
            return Ok(());
        }
        let failed = crate::persistence::update_lease_status_conditional(
            conn,
            &lease.lease_id,
            crate::persistence::LeaseStatus::Failed,
            &[crate::persistence::LeaseStatus::Running],
            None,
            Some(&self.instance.run_id),
        )
        .map_err(|error| EngineError::PersistenceError(error.to_string()))?;
        if !failed {
            // The lease already advanced past Running (e.g. a concurrent
            // writer classified it terminal). The ownership-denied terminal
            // metadata is still authoritative; do not roll it back.
            return Ok(());
        }
        Ok(())
    }

    /// Verify workspace ownership before entering a `failure_cleanup` step.
    ///
    /// Returns:
    /// - `Ok(None)` when ownership verification is not applicable (no cleanup
    ///   step is being entered, no workspace is configured, or the workspace is
    ///   not ownership-managed and has no evidence).
    /// - `Ok(owned)` when the workspace is ownership-managed and the ownership
    ///   evidence verifies: cleanup may proceed normally (the shell script runs
    ///   in a trusted workspace).
    /// - `Err(OwnershipFailure)` when ownership verification fails. The caller
    ///   must NOT route into the workspace-mutating cleanup step. Instead it
    ///   must protect the lease and record a terminal ownership failure so the
    ///   run terminates without pretending `abandon_and_log` will execute.
    ///
    /// This resolves the misleading ownership-fatal → failure-cleanup path: an
    /// ownership auth failure never pretends `abandon_and_log` will run. The
    /// run terminates with an explicit ownership failure that protects the
    /// lease and records provenance without workspace mutation.
    pub(super) fn verify_failure_cleanup_workspace(
        &self,
        outcome: &StepOutcome,
        next_step: Option<&str>,
    ) -> Result<OwnershipVerification, EngineError> {
        if !next_step.is_some_and(|next| {
            *outcome != StepOutcome::Success && self.is_failure_cleanup_step(next)
        }) {
            return Ok(OwnershipVerification::NotApplicable);
        }
        // Issue 158 finding 1: read the workspace from the immutable typed
        // `StepContext::work_dir()` field, never from the mutable context
        // variables (`context.get("work_dir")`). A shell step can overwrite
        // the `work_dir` variable via `context.set("work_dir", ...)`, which
        // would redirect cleanup ownership verification to a path of the step's
        // choosing. The typed field is set at runner construction from the
        // authoritative RunContext and is outside the mutable variable map.
        let workspace = self.context.work_dir().as_path();
        let evidence_exists =
            crate::engine::workspace_ownership::workspace_ownership_evidence_exists(workspace);
        // Issue 158 finding 7: the daemon-managed claim authority must be read
        // from the immutable runtime provenance accessor, never from the
        // mutable config variables. A shell step can set `daemon_managed_claim`
        // in the context variables; only the const accessor reading
        // `self.context.daemon_managed` (set at runner construction from
        // RunContext) is authoritative for deciding whether cleanup may proceed.
        let ownership_required = self.context.daemon_managed_claim();
        if !evidence_exists && !ownership_required {
            return Ok(OwnershipVerification::NotApplicable);
        }
        match crate::engine::workspace_ownership::verify_workspace_ownership(
            workspace,
            &self.instance.run_id,
        ) {
            None => Ok(OwnershipVerification::Owned),
            Some(reason) => Err(EngineError::OwnershipFailure(OwnershipFailureDetails {
                failed_step: next_step.unwrap_or("").to_string(),
                reason,
            })),
        }
    }

    pub(super) fn persist_step_result(
        &mut self,
        current_step_id: &str,
        outcome: &StepOutcome,
        next_step: Option<&str>,
    ) -> Result<(), EngineError> {
        // Verify workspace ownership before entering a failure_cleanup step.
        // An OwnershipFailure is handled as a terminal ownership failure: the
        // run protects its lease and records a terminal failure WITHOUT
        // executing the workspace-mutating cleanup shell script. This prevents
        // a misleading "ownership-fatal → abandon_and_log" path where an
        // ownership auth failure pretends cleanup will run.
        match self.verify_failure_cleanup_workspace(outcome, next_step) {
            Ok(OwnershipVerification::NotApplicable | OwnershipVerification::Owned) => {}
            Err(EngineError::OwnershipFailure(details)) => {
                return self.persist_terminal_ownership_failure(
                    current_step_id,
                    outcome,
                    next_step,
                    details,
                );
            }
            Err(other) => return Err(other),
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
        let diagnostic_details = super::diagnostic_events::details(&self.context, current_step_id);
        append_typed_event_with_conn(
            &tx,
            &self.instance.run_id,
            current_step_id,
            &outcome.to_string(),
            EventType::StepOutcome,
            diagnostic_details.as_deref(),
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

    /// Persist a terminal ownership failure: protect the issue lease and record
    /// a terminal failure outcome WITHOUT executing the workspace-mutating
    /// cleanup shell script, and set the daemon lease directly to `Failed`
    /// (exact-owner CAS) when a matching lease exists.
    ///
    /// This is the terminal ownership failure path. When workspace ownership
    /// verification fails while routing into a `failure_cleanup` step, the run
    /// must not pretend `abandon_and_log` (or any other cleanup step) will run,
    /// because the cleanup shell script executes `gh issue comment`/`edit`
    /// commands that must only run in a trusted workspace. Instead:
    ///
    /// 1. A terminal `FailureCleanupState` is persisted with
    ///    `ownership_denied = true` and `cleanup_succeeded = false`, recording
    ///    the failed step, outcome, and a bounded categorical rejection reason
    ///    as provenance. The cleanup step is set to the targeted
    ///    `failure_cleanup` step so recovery provenance is complete, but the
    ///    step itself never executes. Terminal metadata is committed
    ///    unconditionally: a later DB error does not roll it back.
    /// 2. For daemon-managed runs, the matching daemon lease is set directly to
    ///    `Failed` (exact-owner `Running` CAS) transactionally. A missing lease
    ///    (e.g. one-shot CLI runs) is valid: no lease is required for a
    ///    non-daemon terminal. The exact-owner guard rejects a stale launcher
    ///    whose `run_id` was superseded by a concurrent reclaim. **No generic
    ///    cleanup lease protection:** this terminal never sets the lease to
    ///    `CleanupAbandoned`, because an ownership-denied workspace is unowned
    ///    and cleanup continuation must never be selected for it.
    /// 3. A terminal failure event is appended.
    ///
    /// Because the lease is already `Failed` when the launcher returns, the
    /// `finish_lease_after_result` finalizer's `OwnershipDenied` CAS
    /// (`Running` → `Failed`) rejects idempotently, reporting
    /// `LeaseStatePreserved` with `current_status: Failed`. This is the correct
    /// idempotent outcome: the durable `Failed` state is preserved.
    ///
    /// The returned `Err(EngineError::OwnershipFailure)` signals the runner
    /// loop that the run must terminate immediately without advancing to the
    /// cleanup step. The `run` method maps this to a terminal `RunOutcome`.
    fn persist_terminal_ownership_failure(
        &mut self,
        current_step_id: &str,
        outcome: &StepOutcome,
        next_step: Option<&str>,
        details: OwnershipFailureDetails,
    ) -> Result<(), EngineError> {
        let checkpoint = self.create_checkpoint(current_step_id, "completed");
        let cleanup_step = next_step.unwrap_or(&details.failed_step).to_string();
        let failure = self.build_terminal_ownership_failure_state(
            current_step_id,
            outcome,
            &cleanup_step,
            &checkpoint,
        );
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
            self.persist_terminal_ownership_failure_registry(
                &tx,
                current_step_id,
                outcome,
                &cleanup_step,
                &failure,
            )?;
        }
        tx.commit()
            .map_err(|error| EngineError::PersistenceError(error.to_string()))?;
        self.pending_failure_cleanup = Some(failure);
        self.terminal_ownership_failure = true;
        Ok(())
    }

    /// Build the terminal `FailureCleanupState` for an ownership-denied
    /// terminal. Persists a categorical ownership-denied reason, NOT the raw
    /// ownership rejection reason, so durable public provenance cannot leak
    /// filesystem paths or diagnostic text.
    fn build_terminal_ownership_failure_state(
        &self,
        current_step_id: &str,
        outcome: &StepOutcome,
        cleanup_step: &str,
        checkpoint: &crate::persistence::Checkpoint,
    ) -> FailureCleanupState {
        FailureCleanupState {
            schema_version: FailureCleanupState::SCHEMA_VERSION,
            failed_step: current_step_id.to_string(),
            failure_outcome: outcome.to_string(),
            failure_reason: format!(
                "workspace ownership denied before cleanup step {cleanup_step}"
            ),
            failed_checkpoint_id: format!(
                "{current_step_id}@{}",
                checkpoint.timestamp.to_rfc3339()
            ),
            failed_state_snapshot: checkpoint.state_snapshot.clone(),
            cleanup_step: cleanup_step.to_string(),
            cleanup_succeeded: false,
            captured_at: chrono::Utc::now(),
            cleanup_completed_at: None,
            recovery_consumed_at: None,
            // Explicit typed marker: this terminal failure was caused by a
            // workspace ownership authentication failure. Continuation
            // validation rejects ownership-denied terminals as non-resumable
            // so the run can never be routed to the workspace-mutating
            // cleanup step.
            ownership_denied: true,
        }
    }

    /// Persist the registry metadata for a terminal ownership failure within
    /// the caller's transaction. Updates the run metadata to `Failed` with
    /// the ownership-denied `FailureCleanupState`, appends a terminal event,
    /// and fails the matching daemon lease via an exact-owner `Running` CAS.
    fn persist_terminal_ownership_failure_registry(
        &self,
        tx: &rusqlite::Transaction<'_>,
        current_step_id: &str,
        outcome: &StepOutcome,
        cleanup_step: &str,
        failure: &FailureCleanupState,
    ) -> Result<(), EngineError> {
        let mut metadata = crate::persistence::get_run_with_conn(tx, &self.instance.run_id)
            .map_err(|error| EngineError::PersistenceError(error.to_string()))?
            .ok_or_else(|| {
                EngineError::PersistenceError(format!(
                    "persistent run {} is missing from the registry",
                    self.instance.run_id
                ))
            })?;
        metadata.set_previous_step_and_outcome(current_step_id, outcome.to_string());
        metadata.set_next_step_candidates(vec![cleanup_step.to_string()]);
        metadata.failure_cleanup = Some(failure.clone());
        metadata.status = RunStatus::Failed;
        metadata.set_current_step(current_step_id);
        append_typed_event_with_conn(
            tx,
            &self.instance.run_id,
            current_step_id,
            &metadata.status.to_string(),
            EventType::TerminalState,
            None,
            chrono::Utc::now(),
        )?;
        // Set the matching daemon lease directly to Failed via an
        // exact-owner Running CAS. This is deliberately NOT the generic
        // cleanup lease protection: an ownership-denied terminal must
        // never be selected for cleanup continuation, so it cannot be
        // left in CleanupAbandoned. A missing lease (one-shot CLI runs)
        // is valid and does not roll back the terminal metadata.
        self.fail_owned_lease_for_ownership_denial(tx, &metadata)?;
        persist_run_with_conn(tx, &metadata)
            .map_err(|error| EngineError::PersistenceError(error.to_string()))
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
            ownership_denied: false,
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
