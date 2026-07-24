//! Production capsule-backed recovery wiring. [C8/B8]
//!
//! This module is the cohesive adapter/executor wiring that connects the
//! [`RecoveryProtocolV1`][crate::engine::recovery::RecoveryProtocolV1]
//! execute phase to the actual resume surfaces
//! (`EngineRunner::resume_from_checkpoint`, `continuation_execution`,
//! `resume_child_workflow`, `resume_daemon_workflow`). The atomic
//! fresh-launch path (`persist_launch_atomically`) already builds and persists
//! capsules atomically and is owned by P08B; this module owns the resume-side
//! capsule-driven reconstruction and step execution.
//!
//! ## Production executor
//!
//! [`RunnerRecoveryExecutor`] implements [`RecoveryExecutor`] by reconstructing
//! a [`WorkflowInstance`] from the immutable capsule bytes (via
//! [`V1Adapter::build_instance`][crate::engine::recovery::adapters::V1Adapter]),
//! constructing an [`EngineRunner`] through the existing
//! `with_db_path_and_context` resume pattern, and executing the reserved step
//! (and any downstream transitions) via `EngineRunner::run_from_current_step` —
//! the same internal transition loop used by `EngineRunner::run`. The actual
//! [`RunOutcome`] plus the exact final runner state snapshot are mapped to a
//! [`RecoveryExecutionResult`]; no synthetic success is fabricated. Execution
//! happens on the executor's own database connection, outside the protocol's
//! SQLite writer transaction, and starts at the reserved step without
//! reopening or advancing the recovery epoch.
//!
//! ## Actual resume surfaces (call sites for P14)
//!
//! The following surfaces are where capsule-driven recovery will replace the
//! current ad-hoc reconstruction:
//!
//! - `src/app/runs/continuation_execution.rs` — `reconstruct_runner_with_config`
//!   / `reconstruct_runner_with_config_and_provenance`
//! - `src/app/daemon_run.rs` — `resume_daemon_workflow`
//! - `src/engine/executors/parent_orchestration/child_workflow.rs` —
//!   `resume_child_workflow`
//! - `src/engine/runner.rs` — `EngineRunner::resume_from_checkpoint` (private)
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14
//! @requirement:REQ-RP-002,REQ-RP-009

use std::path::{Path, PathBuf};

use crate::engine::executor::ExecutorRegistry;
use crate::engine::recovery::adapters::{adapter_for, AdapterError, CapsuleAdapter};
use crate::engine::recovery::capsule::ExecutionCapsuleV1;
use crate::engine::recovery::protocol::{
    RecoveryExecutionError, RecoveryExecutionInvocation, RecoveryExecutionResult, RecoveryExecutor,
};
use crate::engine::runner::{EngineRunner, RunContext, RunOutcome};

/// Capsule-backed recovery wiring entry point. [C8/B8]
///
/// Provides the dispatch seams for the object-safe `Box<dyn CapsuleAdapter>`
/// and the production [`RunnerRecoveryExecutor`]. Resume call sites use this
/// to load a capsule, verify its envelope digest, obtain the adapter, and
/// reconstruct the [`WorkflowInstance`] through `build_instance`.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14
/// @requirement:REQ-RP-009
#[derive(Debug, Clone, Default)]
pub struct RecoveryWiring;

impl RecoveryWiring {
    /// Resolve the object-safe capsule adapter for resume. [C8/B9]
    ///
    /// Dispatches through [`adapter_for`] to obtain the `Box<dyn
    /// CapsuleAdapter>` needed to reconstruct the [`WorkflowInstance`] via
    /// `build_instance`. Resume call sites load the capsule, verify its
    /// envelope digest, then call this before reconstructing the runner.
    ///
    /// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14
    /// @requirement:REQ-RP-009
    pub fn adapter_for_resume(
        &self,
        capsule: &ExecutionCapsuleV1,
    ) -> Result<Box<dyn CapsuleAdapter>, AdapterError> {
        adapter_for(capsule)
    }

    /// Resolve the production capsule-backed recovery executor. [C8/B8]
    ///
    /// Returns a [`RunnerRecoveryExecutor`] ready for injection into
    /// [`crate::engine::recovery::RecoveryProtocolV1::recover_with_executor`].
    /// The executor is constructed per-recovery-call with the resolved
    /// `db_path` and [`RunContext`], matching the existing resume-surface
    /// construction patterns. [B8]
    ///
    /// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14
    /// @requirement:REQ-RP-009
    pub fn runner_executor(
        &self,
        db_path: PathBuf,
        run_context: RunContext,
    ) -> RunnerRecoveryExecutor {
        RunnerRecoveryExecutor::new(db_path, run_context)
    }
}

/// Production capsule-backed recovery executor. [C8/B8]
///
/// Implements [`RecoveryExecutor`] by reconstructing a [`WorkflowInstance`]
/// from the immutable capsule bytes (via the object-safe adapter dispatch),
/// constructing an [`EngineRunner`] through the existing
/// `with_db_path_and_context` resume pattern, and executing the reserved step
/// (and any downstream transitions) via `EngineRunner::run_from_current_step`
/// — the same internal transition loop used by `EngineRunner::run`. The actual
/// [`RunOutcome`] plus the exact final runner state snapshot are mapped to a
/// [`RecoveryExecutionResult`]; no synthetic success is fabricated.
///
/// Execution happens on the executor's own database connection, outside the
/// protocol's SQLite writer transaction, and starts at the reserved step
/// without reopening or advancing the recovery epoch. [C5/C12]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14
/// @requirement:REQ-RP-001,REQ-RP-009
#[derive(Debug, Clone)]
pub struct RunnerRecoveryExecutor {
    /// The SQLite database path for the run being recovered.
    db_path: PathBuf,
    /// The run context (workspace authorization, provenance, identity).
    run_context: RunContext,
}

impl RunnerRecoveryExecutor {
    /// Construct a `RunnerRecoveryExecutor` for a specific run database and
    /// context.
    ///
    /// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14
    /// @requirement:REQ-RP-009
    pub fn new(db_path: PathBuf, run_context: RunContext) -> Self {
        Self {
            db_path,
            run_context,
        }
    }

    /// Reconstruct an [`EngineRunner`] from the capsule and execute the
    /// reserved step (and any downstream transitions) via the **normal
    /// transition loop**, mapping the actual [`RunOutcome`] and exact final
    /// runner state snapshot to a [`RecoveryExecutionResult`]. [B8/C8/C5]
    ///
    /// The instance is reconstructed from the immutable capsule bytes via
    /// [`adapter_for`] → `Box<dyn CapsuleAdapter>::build_instance`, carrying
    /// the capsule's exact `run_id`. The runner is constructed through the
    /// existing [`EngineRunner::with_db_path_and_context`] resume pattern
    /// (loads checkpoint state, builds the step context), then the reserved
    /// step is executed via [`EngineRunner::run_from_current_step`] — the
    /// same internal transition loop used by `EngineRunner::run`. The runner
    /// operates on its own database connection — outside the protocol's SQLite
    /// writer transaction — and starts at the reserved step without reopening
    /// or advancing the recovery epoch.
    ///
    /// The actual [`RunOutcome`] is mapped truthfully into `step_status` and
    /// the exact final state snapshot (captured from the live runner via
    /// [`EngineRunner::state_snapshot`], never a fabricated default):
    /// - [`RunOutcome::Success`] → `step_status = "completed"`
    /// - [`RunOutcome::Interrupted`] / [`RunOutcome::WaitingExternal`] →
    ///   `step_status = "interrupted"` (resumable)
    /// - any other outcome / engine error → `step_status = "failed"`
    ///
    /// No synthetic success is fabricated: a step that did not reach
    /// [`RunOutcome::Success`] is reported as failed or interrupted, and an
    /// engine error is surfaced as a [`RecoveryExecutionError::Failed`].
    ///
    /// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14
    /// @requirement:REQ-RP-001,REQ-RP-009
    fn build_resume_runner(
        &self,
        invocation: &RecoveryExecutionInvocation<'_>,
        workspace: &Path,
    ) -> Result<RecoveryExecutionResult, RecoveryExecutionError> {
        let _ = workspace;
        let adapter = adapter_for(invocation.capsule)
            .map_err(|e| RecoveryExecutionError::Failed(e.to_string()))?;
        let mut instance = adapter
            .build_instance(invocation.capsule)
            .map_err(|e| RecoveryExecutionError::Failed(e.to_string()))?;
        instance.transition_to(invocation.step_id);
        let registry = ExecutorRegistry::with_defaults();
        let mut runner = EngineRunner::with_db_path_and_context(
            instance,
            registry,
            &self.db_path,
            self.run_context.clone(),
        )
        .map_err(|e| RecoveryExecutionError::Failed(e.to_string()))?;
        let outcome = runner
            .run_from_current_step()
            .map_err(|e| RecoveryExecutionError::Failed(e.to_string()))?;
        let (step_status, snapshot_status) = map_run_outcome_status(&outcome);
        let snapshot = runner.state_snapshot(snapshot_status);
        let runner_result = durable_runner_result(&outcome, invocation.step_id);
        Ok(RecoveryExecutionResult {
            step_status: step_status.to_string(),
            state_snapshot: snapshot,
            runner_result: Some(runner_result),
        })
    }
}

/// Map a [`RunOutcome`] to the attempt `step_status` and snapshot status
/// strings consumed by the finalize phase. [C5/C12]
///
/// [`RunOutcome::Success`] maps to `"completed"` (the reserved step and all
/// downstream transitions reached a terminal success). [`RunOutcome::Interrupted`]
/// and [`RunOutcome::WaitingExternal`] map to `"interrupted"` because the run
/// paused on a recoverable condition and remains resumable. Any other outcome
/// is a non-success terminal that the finalize phase records as `"failed"`.
/// No synthetic success is fabricated.
fn map_run_outcome_status(outcome: &RunOutcome) -> (&'static str, &'static str) {
    match outcome {
        RunOutcome::Success => ("completed", "completed"),
        RunOutcome::Interrupted { .. } | RunOutcome::WaitingExternal { .. } => {
            ("interrupted", "interrupted")
        }
        RunOutcome::Failure { .. } | RunOutcome::Abandoned { .. } => ("failed", "failed"),
    }
}

/// Serialize the actual runner outcome fields needed by daemon/child callers
/// after protocol finalization.
fn durable_runner_result(outcome: &RunOutcome, step_id: &str) -> serde_json::Value {
    match outcome {
        RunOutcome::WaitingExternal { step_id, reason } => serde_json::json!({
            "outcome": "waiting_external",
            "step_id": step_id,
            "reason": reason,
        }),
        RunOutcome::Interrupted { step_id } => serde_json::json!({
            "outcome": "interrupted",
            "step_id": step_id,
        }),
        RunOutcome::Failure { step_id, reason } => serde_json::json!({
            "outcome": "failure",
            "step_id": step_id,
            "reason": reason,
        }),
        RunOutcome::Abandoned { step_id, reason } => serde_json::json!({
            "outcome": "abandoned",
            "step_id": step_id,
            "reason": reason,
        }),
        RunOutcome::Success => serde_json::json!({
            "outcome": "success",
            "step_id": step_id,
        }),
    }
}

impl RecoveryExecutor for RunnerRecoveryExecutor {
    /// Execute the recovery step via the capsule-backed runner. [C5/C12]
    ///
    /// Delegates to [`Self::build_resume_runner`], which reconstructs the
    /// [`WorkflowInstance`] from the immutable capsule and executes the
    /// reserved step on the executor's own connection, outside the protocol's
    /// SQLite writer transaction.
    ///
    /// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14
    /// @requirement:REQ-RP-001,REQ-RP-009
    fn execute(
        &self,
        invocation: &RecoveryExecutionInvocation<'_>,
    ) -> Result<RecoveryExecutionResult, RecoveryExecutionError> {
        self.build_resume_runner(invocation, invocation.workspace)
    }
}
