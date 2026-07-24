/// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
/// Workflow execution engine - runs workflow instances step by step.
use std::cell::RefCell;
use std::collections::HashMap;

use rusqlite::Connection;
use thiserror::Error;

use crate::engine::executor::{ExecutorRegistry, StepContext};
use crate::engine::instance::WorkflowInstance;
use crate::engine::transition::{resolve_transition_schema, StepOutcome};
use crate::persistence::{
    load_checkpoint_with_conn, save_checkpoint_with_conn, Checkpoint, EventType,
    FailureCleanupState, PersistenceError, StateSnapshot,
};
use crate::workflow::schema::{StepDef, TransitionDef};

mod target_path_context;

mod diagnostic_events;
mod support;
use support::preview_for_log;

mod completion;
mod construction;
mod failure_cleanup;
mod lease_coordination;

pub use completion::status_for_completion;

/// Contextual paths, GitHub references, and ephemeral authorities for a run.
/// Workspace authorization is reconstructed from verified ownership on every
/// process entry and is never persisted in run metadata.
#[derive(Debug, Clone, Default)]
pub struct RunContext {
    pub daemon_managed: bool,
    pub log_path: Option<String>,
    pub artifact_root: Option<String>,
    pub workspace_path: Option<String>,
    pub repository: Option<String>,
    pub issue_number: Option<i64>,
    pub pr_number: Option<i64>,
    pub head_sha: Option<String>,
    /// Ephemeral workspace dev/inode authorization, reconstructed by resume
    /// surfaces from a freshly-verified workspace descriptor. Not persisted.
    /// `None` on fresh launches until the `workspace_ownership_verify` step
    /// captures it; reconstructed by resume paths so resumed shell steps
    /// retain descriptor-anchored authorization without re-running the
    /// verify graph step.
    pub workspace_authorization: Option<crate::engine::workspace_ownership::WorkspaceAuthorization>,
    /// Exact launch provenance recorded for new runs at launch time. The launch
    /// surfaces (CLI `run`, daemon launch, child launch) compute this from the
    /// resolved workflow type/config + config root and inject it here so the
    /// engine persists it in the initial `RunMetadata` row. Resume surfaces do
    /// NOT set this; they recompute and verify against the persisted value.
    /// `None` on resume paths means `build_metadata` preserves the existing
    /// persisted provenance rather than overwriting it.
    /// @plan:PLAN-20260722-ISSUE158-LAUNCH-PROVENANCE
    pub launch_provenance: Option<crate::persistence::launch_provenance::LaunchProvenance>,
}

/// Errors that can occur during workflow execution.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
/// @requirement:REQ-EARS-ENG-003,REQ-EARS-PERSIST-004
#[derive(Error, Debug)]
pub enum EngineError {
    #[error("step execution failed: {step_id} - {message}")]
    StepExecutionError { step_id: String, message: String },

    #[error("transition not found from {step_id} with outcome {outcome:?}")]
    TransitionNotFound {
        step_id: String,
        outcome: StepOutcome,
    },

    #[error("loop limit exceeded at step {step_id}")]
    LoopLimitExceeded { step_id: String },

    #[error("retry limit exceeded for step {step_id}")]
    RetryLimitExceeded { step_id: String },

    #[error("persistence error: {0}")]
    PersistenceError(String),

    #[error("invalid state: {0}")]
    InvalidState(String),

    #[error("step not found: {0}")]
    StepNotFound(String),

    #[error("llxprt binary not found at `{path}`")]
    LlxprtBinaryNotFound { path: String },

    #[error("llxprt binary at `{path}` failed version check: {message}")]
    LlxprtVersionError { path: String, message: String },

    #[error("llxprt profile `{profile}` could not be resolved: {message}")]
    LlxprtProfileError { profile: String, message: String },

    /// Workspace ownership verification failed while routing into a
    /// `failure_cleanup` step. This is a terminal ownership failure: the run
    /// must not execute the workspace-mutating cleanup shell script (e.g.
    /// `abandon_and_log`) because the workspace is not owned by this run.
    /// Instead, the runner protects the issue lease and records a terminal
    /// failure outcome without workspace mutation. This prevents a misleading
    /// "ownership-fatal → abandon_and_log" path where an ownership auth
    /// failure pretends cleanup will run.
    #[error("{0}")]
    OwnershipFailure(OwnershipFailureDetails),
}

/// Details of a terminal workspace ownership failure encountered while routing
/// into a `failure_cleanup` step. Carries the targeted cleanup step and the
/// ownership verification rejection reason so the runner can record a terminal
/// failure outcome without executing the workspace-mutating cleanup.
#[derive(Debug, Clone)]
pub struct OwnershipFailureDetails {
    /// The `failure_cleanup` step that would have been entered.
    pub failed_step: String,
    /// The ownership verification rejection reason.
    pub reason: String,
}

impl std::fmt::Display for OwnershipFailureDetails {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "workspace ownership failure before cleanup step {}: {}",
            self.failed_step, self.reason
        )
    }
}

impl From<PersistenceError> for EngineError {
    fn from(err: PersistenceError) -> Self {
        EngineError::PersistenceError(err.to_string())
    }
}

impl From<crate::adapters::llxprt::LlxprtError> for EngineError {
    fn from(err: crate::adapters::llxprt::LlxprtError) -> Self {
        use crate::adapters::llxprt::LlxprtError;
        match err {
            LlxprtError::BinaryNotFound { path } => EngineError::LlxprtBinaryNotFound { path },
            LlxprtError::VersionCheckFailed { path, message }
            | LlxprtError::NotExecutable { path, message } => {
                EngineError::LlxprtVersionError { path, message }
            }
        }
    }
}

/// Outcome of a workflow run.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
/// @requirement:REQ-EARS-ENG-003,REQ-EARS-ROUTE-003
#[derive(Debug, Clone, PartialEq)]
pub enum RunOutcome {
    /// All steps completed successfully.
    Success,
    /// Run terminated due to fatal error.
    Failure { step_id: String, reason: String },
    /// Run was abandoned due to loop limits.
    Abandoned { step_id: String, reason: String },
    /// Run was interrupted and can be resumed.
    Interrupted { step_id: String },
    /// Run paused on a recoverable external wait condition (e.g. PR checks
    /// still pending when the watch window closed). The run is non-terminal
    /// and can be resumed once the external state changes.
    /// @plan:PLAN-20260623-LUTHER-CONTINUATION
    WaitingExternal { step_id: String, reason: String },
}

/// The workflow execution engine.
/// Manages the execution lifecycle of a workflow instance.
///
/// `EngineRunner` is the sole supported execution engine. It implements a
/// durable, resumable state machine: step outcomes (`StepOutcome`) route
/// transitions at runtime, runs are checkpointed to SQLite and can be resumed,
/// and remediation edges are loop-limited per edge. This dynamic,
/// outcome-routed model is deliberately not built on a static DAG executor
/// (such as `dagrs`), whose parallel task-graph semantics do not match
/// Luther's resumable, transition-driven execution.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
/// @plan:PLAN-20260408-STEP-EXEC.P06
/// @plan:PLAN-20260408-LLXPRT-FIRST.P12
/// @requirement:REQ-EARS-ENG-002,REQ-EARS-ENG-004,REQ-EARS-ROUTE-004,REQ-LF-LOOP-002
pub struct EngineRunner {
    /// The workflow instance being executed.
    instance: WorkflowInstance,
    /// Current retry count for the current step.
    retry_count: u32,
    /// Per-edge loop counter keyed by "from:to" step pair.
    edge_loop_counts: HashMap<String, u32>,
    /// Maximum retries allowed from config.
    /// Retained for the configured retry policy while retry transitions are expanded.
    #[allow(dead_code)]
    max_retries: u32,
    /// Maximum remediation loops allowed.
    max_loops: u32,
    /// SQLite connection for persistence.
    conn: RefCell<Connection>,
    /// Flag indicating if an interrupt was received.
    interrupted: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Executor registry for dispatching step execution.
    registry: ExecutorRegistry,
    /// Step execution context for variable storage and interpolation.
    context: StepContext,
    /// Contextual run metadata (paths, GitHub refs) for the run registry.
    run_context: RunContext,
    /// Whether to persist run-registry metadata (only when a real DB path is set).
    persist_registry: bool,
    /// In-memory causal failure for non-registry runners and immediate cleanup.
    pending_failure_cleanup: Option<FailureCleanupState>,
    /// Set when `persist_step_result` recorded a terminal ownership failure
    /// (workspace ownership verification failed while routing into a
    /// `failure_cleanup` step). The runner loop checks this after
    /// `persist_step_result` to terminate the run with a terminal failure
    /// without advancing to the workspace-mutating cleanup step.
    terminal_ownership_failure: bool,
}

impl EngineRunner {
    /// Compute candidate next steps for the given step across all outcomes.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    fn compute_next_step_candidates(&self, step_id: &str) -> Vec<String> {
        let transitions = &self.instance.workflow_type.transitions;
        let outcomes = [
            StepOutcome::Success,
            StepOutcome::Fixable,
            StepOutcome::Fatal,
            StepOutcome::Retryable,
            StepOutcome::Abandon,
            StepOutcome::Wait,
        ];
        let mut candidates = Vec::new();
        for outcome in outcomes {
            if let Some(next) = resolve_transition_schema(step_id, &outcome, transitions) {
                if !candidates.contains(&next) {
                    candidates.push(next);
                }
            }
        }
        candidates
    }

    /// Execute the workflow instance.
    /// Runs through steps, handling transitions and outcomes.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P14
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
    /// @requirement:REQ-EARS-ENG-002,REQ-EARS-ENG-003,REQ-EARS-ROUTE-001,REQ-LF-LOOP-001,REQ-LF-LOOP-002,REQ-LF-LOOP-003,REQ-LF-LOOP-004,REQ-LF-FAIL-001,REQ-LF-FAIL-005
    pub fn run(&mut self) -> Result<RunOutcome, EngineError> {
        self.resume_from_checkpoint()?;
        self.mark_run_started();
        self.run_transition_loop()
    }

    /// Recovery-only execution entry point.
    ///
    /// Runs the **normal transition loop** from the runner's already
    /// capsule-reconstructed/reserved current step (`self.instance.current_state`)
    /// WITHOUT calling [`resume_from_checkpoint`](Self::resume_from_checkpoint)
    /// and WITHOUT reopening or advancing the recovery epoch. This is the same
    /// internal loop used by [`run`](Self::run), so recovery execution preserves
    /// the exact ownership-denied handling, failure-cleanup routing, per-edge
    /// loop limits, checkpoint persistence, interrupt checks, external waits,
    /// and lease heartbeat semantics of a normal run.
    ///
    /// The caller (the capsule-backed recovery executor) is responsible for
    /// reconstructing the [`WorkflowInstance`] from the immutable capsule,
    /// transitioning it to the reserved step, and constructing the runner
    /// through the resume construction path (which loads checkpoint state and
    /// builds the step context). This method only executes the transition loop.
    ///
    /// Use [`state_snapshot`](Self::state_snapshot) after this returns to obtain
    /// the exact final runner state snapshot (no fabricated default). Recovery
    /// provenance must be derived from the actual [`RunOutcome`] plus that
    /// exact snapshot; no synthetic success is ever produced.
    ///
    /// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14
    /// @requirement:REQ-RP-002,REQ-RP-009
    pub fn run_from_current_step(&mut self) -> Result<RunOutcome, EngineError> {
        self.run_transition_loop()
    }

    /// Return the exact final runner state snapshot.
    ///
    /// Captures the live `retry_count`, `edge_loop_counts`, checkpointable
    /// context, and the provided status string into a [`StateSnapshot`]. This
    /// is the truthful snapshot of the runner's current state — never a
    /// fabricated default. The capsule-backed recovery executor uses this to
    /// map the actual post-execution runner state into the durable attempt row.
    ///
    /// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14
    /// @requirement:REQ-RP-002
    #[must_use]
    pub fn state_snapshot(&self, status: &str) -> StateSnapshot {
        StateSnapshot {
            retry_count: self.retry_count,
            loop_count: self.loop_count(),
            edge_loop_counts: self.edge_loop_counts.clone(),
            context: self.context.checkpoint_values(),
            status: status.to_string(),
        }
    }

    /// The shared transition loop.
    ///
    /// This is the single control-flow loop used by both [`run`](Self::run) and
    /// [`run_from_current_step`](Self::run_from_current_step). It begins at
    /// `self.instance.current_state` and runs the normal step → transition →
    /// persist cycle to a terminal [`RunOutcome`], preserving all run-loop
    /// semantics: interrupt checks, ownership-denied terminal handling,
    /// failure-cleanup routing, per-edge loop limits, checkpoint persistence,
    /// external waits, and lease heartbeats.
    ///
    /// Callers are responsible for pre-loop setup: [`run`](Self::run) calls
    /// [`resume_from_checkpoint`](Self::resume_from_checkpoint) and
    /// [`mark_run_started`](Self::mark_run_started) first, while
    /// [`run_from_current_step`](Self::run_from_current_step) skips both
    /// because the caller has already reconstructed/reserved the current step.
    fn run_transition_loop(&mut self) -> Result<RunOutcome, EngineError> {
        let mut current_step_id = self.instance.current_state.clone();

        loop {
            if self.interrupted.load(std::sync::atomic::Ordering::SeqCst) {
                return self.interrupt_at_step(&current_step_id);
            }

            self.prepare_step_start(&current_step_id)?;
            let outcome = match self.execute_current_step(&current_step_id) {
                Ok(outcome) => outcome,
                Err(error @ EngineError::StepExecutionError { .. })
                    if self.is_registered_step_type(&current_step_id) =>
                {
                    self.context.set("diagnostic", &error.to_string());
                    StepOutcome::Fatal
                }
                Err(error @ EngineError::LlxprtBinaryNotFound { .. })
                | Err(error @ EngineError::LlxprtVersionError { .. })
                | Err(error @ EngineError::LlxprtProfileError { .. }) => {
                    self.context.set("diagnostic", &error.to_string());
                    StepOutcome::Fatal
                }
                Err(error) => return Err(error),
            };
            if self.interrupted.load(std::sync::atomic::Ordering::SeqCst) {
                return self.interrupt_at_step(&current_step_id);
            }
            let next_step = if outcome == StepOutcome::Abandon {
                None
            } else {
                self.resolve_next_step(&current_step_id, &outcome)?
            };
            self.persist_step_result(&current_step_id, &outcome, next_step.as_deref())?;

            // A terminal ownership failure was recorded by persist_step_result:
            // workspace ownership verification failed while routing into a
            // failure_cleanup step. The run must terminate immediately with a
            // terminal failure WITHOUT advancing to the workspace-mutating
            // cleanup step (e.g. abandon_and_log). The lease is already
            // protected and the failure provenance is persisted.
            if self.terminal_ownership_failure {
                let failure = self.pending_failure_cleanup.clone().ok_or_else(|| {
                    EngineError::InvalidState(
                        "terminal ownership failure recorded without provenance".to_string(),
                    )
                })?;
                return Ok(RunOutcome::Failure {
                    step_id: failure.failed_step,
                    reason: failure.failure_reason,
                });
            }

            if outcome == StepOutcome::Abandon {
                if self.is_failure_cleanup_step(&current_step_id) {
                    return self.finish_without_transition(&current_step_id, &outcome);
                }
                return Ok(self.abandon_at_step(&current_step_id));
            }

            match next_step {
                Some(next_step_id) => {
                    if let Some(run_outcome) =
                        self.advance_to_next_step(&mut current_step_id, next_step_id, &outcome)
                    {
                        return Ok(run_outcome);
                    }
                }
                None => return self.finish_without_transition(&current_step_id, &outcome),
            }
        }
    }

    fn resume_from_checkpoint(&mut self) -> Result<(), EngineError> {
        // P12 call site: capsule-driven resume wiring (designated P14 implementation).
        // Once V1Adapter::build_instance is implemented (P14), the recovery
        // protocol's capsule-backed executor will own the external step
        // execution; this private method remains the engine-internal checkpoint
        // restoration used by run() and reconstruct_runner.
        // @plan:PLAN-20260723-SELFHOST-RELIABILITY.P12
        if let Some(failure) = self
            .load_metadata()
            .and_then(|metadata| metadata.failure_cleanup)
            .filter(|failure| !failure.cleanup_succeeded)
        {
            // An ownership-denied terminal must NEVER be routed to the
            // workspace-mutating cleanup step. The ownership-denied terminal is
            // a distinct non-resumable state, but as a defense-in-depth guard
            // we refuse to transition into the cleanup step even if the run is
            // somehow reconstructed: cleanup executes shell commands that must
            // only run in a trusted workspace.
            if failure.ownership_denied {
                return Err(EngineError::InvalidState(format!(
                    "run {} terminated with a workspace ownership denial and cannot resume into \
                     cleanup step {}; cleanup cannot execute in an unowned workspace",
                    self.instance.run_id, failure.cleanup_step
                )));
            }
            self.context
                .restore_checkpoint_values(failure.failed_state_snapshot.context.clone())
                .map_err(|error| EngineError::PersistenceError(error.to_string()))?;
            self.instance.transition_to(&failure.cleanup_step);
            self.retry_count = failure.failed_state_snapshot.retry_count;
            self.edge_loop_counts = failure.failed_state_snapshot.edge_loop_counts.clone();
            self.pending_failure_cleanup = Some(failure);
            return Ok(());
        }
        let conn = self.conn.borrow();
        if let Some(checkpoint) = load_checkpoint_with_conn(&conn, &self.instance.run_id)? {
            self.context
                .restore_checkpoint_values(checkpoint.state_snapshot.context.clone())
                .map_err(|error| EngineError::PersistenceError(error.to_string()))?;
            self.instance.transition_to(&checkpoint.step_id);
            self.retry_count = checkpoint.state_snapshot.retry_count;
            self.edge_loop_counts = checkpoint.state_snapshot.edge_loop_counts.clone();
        }
        Ok(())
    }

    fn mark_run_started(&self) {
        if self.persist_registry {
            if let Some(mut md) = self.load_metadata() {
                md.mark_started();
                self.persist_metadata(&md);
            }
        }
    }

    fn interrupt_at_step(&self, current_step_id: &str) -> Result<RunOutcome, EngineError> {
        let checkpoint = self.create_checkpoint(current_step_id, "interrupted");
        let conn = self.conn.borrow();
        save_checkpoint_with_conn(&conn, &checkpoint)?;
        drop(conn);
        let details = diagnostic_events::details(&self.context, current_step_id);
        self.record_event(
            EventType::StepOutcome,
            current_step_id,
            "interrupted",
            details.as_deref(),
        );

        let run_outcome = RunOutcome::Interrupted {
            step_id: current_step_id.to_string(),
        };
        let _ = self.record_run_completion(&run_outcome, current_step_id);
        Ok(run_outcome)
    }

    fn prepare_step_start(&mut self, current_step_id: &str) -> Result<(), EngineError> {
        self.context.set_current_step_id(current_step_id);
        if self.is_failure_cleanup_step(current_step_id) {
            let failure = self
                .load_metadata()
                .and_then(|metadata| metadata.failure_cleanup)
                .or_else(|| self.pending_failure_cleanup.clone())
                .ok_or_else(|| {
                    EngineError::PersistenceError(
                        "failure cleanup cannot start without failed-work provenance".to_string(),
                    )
                })?;
            self.context.set("failed_work_step", &failure.failed_step);
            self.context
                .set("failed_work_outcome", &failure.failure_outcome);
        }
        self.heartbeat_owned_lease();
        if self.persist_registry {
            if let Some(mut md) = self.load_metadata() {
                md.set_current_step(current_step_id);
                md.set_next_step_candidates(self.compute_next_step_candidates(current_step_id));
                self.persist_metadata(&md);
            }
            self.record_event(EventType::StepStart, current_step_id, "started", None);
        }
        Ok(())
    }

    fn is_registered_step_type(&self, step_id: &str) -> bool {
        self.instance
            .workflow_type
            .steps
            .iter()
            .find(|step| step.step_id == step_id)
            .is_some_and(|step| self.registry.contains_step_type(&step.step_type))
    }

    fn execute_current_step(&mut self, current_step_id: &str) -> Result<StepOutcome, EngineError> {
        eprintln!("[engine] Executing step: {}", current_step_id);
        let outcome = self.execute_step(current_step_id).inspect_err(|e| {
            if self.persist_registry {
                self.record_event(
                    EventType::Error,
                    current_step_id,
                    "error",
                    Some(&e.to_string()),
                );
            }
        })?;

        eprintln!("[engine] Step '{}' outcome: {}", current_step_id, outcome);
        self.log_non_success_output(&outcome);
        Ok(outcome)
    }

    fn log_non_success_output(&self, outcome: &StepOutcome) {
        if *outcome == StepOutcome::Success {
            return;
        }
        self.log_context_preview("stderr");
        self.log_context_preview("stdout");
    }

    fn log_context_preview(&self, key: &str) {
        if let Some(value) = self.context.get(key) {
            if !value.is_empty() {
                eprintln!("[engine] {}: {}", key, preview_for_log(value, 500));
            }
        }
    }

    fn abandon_at_step(&self, current_step_id: &str) -> RunOutcome {
        let run_outcome = RunOutcome::Abandoned {
            step_id: current_step_id.to_string(),
            reason: "Loop limit exceeded".to_string(),
        };
        let _ = self.record_run_completion(&run_outcome, current_step_id);
        run_outcome
    }

    fn advance_to_next_step(
        &mut self,
        current_step_id: &mut String,
        next_step_id: String,
        outcome: &StepOutcome,
    ) -> Option<RunOutcome> {
        if let Some(run_outcome) = self.enforce_edge_limit(current_step_id, &next_step_id, outcome)
        {
            return Some(run_outcome);
        }
        *current_step_id = next_step_id;
        self.instance.transition_to(current_step_id.as_str());
        None
    }

    fn enforce_edge_limit(
        &mut self,
        current_step_id: &str,
        next_step_id: &str,
        outcome: &StepOutcome,
    ) -> Option<RunOutcome> {
        let transition_def = self.find_transition(current_step_id, outcome);
        if !self.is_limited_transition(current_step_id, next_step_id, transition_def) {
            return None;
        }

        let edge_key = format!("{}:{}", current_step_id, next_step_id);
        let edge_limit = transition_def
            .and_then(|t| t.max_iterations)
            .unwrap_or(self.max_loops);
        let current_count = self.edge_loop_counts.get(&edge_key).copied().unwrap_or(0);
        if current_count >= edge_limit {
            let run_outcome = RunOutcome::Abandoned {
                step_id: current_step_id.to_string(),
                reason: format!(
                    "Per-edge loop limit ({}) exceeded on edge {}",
                    edge_limit, edge_key
                ),
            };
            let _ = self.record_run_completion(&run_outcome, current_step_id);
            return Some(run_outcome);
        }

        self.edge_loop_counts.insert(edge_key, current_count + 1);
        None
    }

    /// Find the transition definition matching the given from step and outcome.
    /// Returns Option<&TransitionDef> to access max_iterations for per-edge loop limits.
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P14
    /// @requirement:REQ-LF-LOOP-001,REQ-LF-LOOP-004
    fn find_transition(&self, from: &str, outcome: &StepOutcome) -> Option<&TransitionDef> {
        let outcome_str = outcome.to_string();

        let transitions = &self.instance.workflow_type.transitions;

        for t in transitions {
            if t.from == from {
                // Match by condition or default to Success when condition is None
                if let Some(ref cond) = t.condition {
                    if cond == &outcome_str {
                        return Some(t);
                    }
                } else if *outcome == StepOutcome::Success {
                    return Some(t);
                }
            }
        }
        None
    }

    /// Execute a single step and return its outcome.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    /// @plan:PLAN-20260408-STEP-EXEC.P06
    /// @requirement:REQ-EARS-ENG-002
    pub fn execute_step(&mut self, step_id: &str) -> Result<StepOutcome, EngineError> {
        // Look up the StepDef by step_id from the workflow type
        let step_def = self
            .instance
            .workflow_type
            .steps
            .iter()
            .find(|s| s.step_id == step_id)
            .ok_or_else(|| EngineError::StepNotFound(step_id.to_string()))?;

        // Get the step_type and parameters from the StepDef
        let step_type = &step_def.step_type;
        let owned_params = self.step_parameters_with_config_manifest(step_def)?;
        let params = owned_params
            .as_ref()
            .or(step_def.parameters.as_ref())
            .unwrap_or(&serde_json::Value::Null);

        // Dispatch to the registry for execution
        self.registry.dispatch(step_type, &mut self.context, params)
    }

    /// Handle an interrupt signal and prepare for clean shutdown.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    /// @requirement:REQ-EARS-ENG-004
    pub fn handle_interrupt(&mut self) -> Result<RunOutcome, EngineError> {
        let current_step_id = self.instance.current_state.clone();

        // Mark as interrupted
        self.interrupted
            .store(true, std::sync::atomic::Ordering::SeqCst);

        // Persist interrupt checkpoint
        let checkpoint = self.create_checkpoint(&current_step_id, "interrupted");
        let conn = self.conn.borrow();
        save_checkpoint_with_conn(&conn, &checkpoint)?;
        drop(conn);

        Ok(RunOutcome::Interrupted {
            step_id: current_step_id,
        })
    }

    /// Return a signal handle that can request interruption from another thread.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    /// @requirement:REQ-EARS-ENG-004
    pub fn interrupt_handle(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        self.interrupted.clone()
    }

    /// Get the current step being executed.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    pub fn current_step(&self) -> &str {
        &self.instance.current_state
    }

    /// Get the run_id of this execution.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    pub fn run_id(&self) -> &str {
        &self.instance.run_id
    }

    /// Get the workflow type id for this execution.
    /// @plan:issue-117
    pub fn workflow_type_id(&self) -> &str {
        self.instance.workflow_type_id()
    }

    /// Get the workflow config id for this execution.
    /// @plan:issue-117
    pub fn config_id(&self) -> &str {
        self.instance.config_id()
    }

    fn step_parameters_with_config_manifest(
        &self,
        step_def: &StepDef,
    ) -> Result<Option<serde_json::Value>, EngineError> {
        let Some(manifest) = &self.instance.config.command_manifest else {
            return Ok(None);
        };
        let mut params = step_def
            .parameters
            .clone()
            .unwrap_or(serde_json::Value::Null);
        if matches!(params, serde_json::Value::Null) {
            params = serde_json::json!({});
        }
        let Some(object) = params.as_object_mut() else {
            return Err(EngineError::StepExecutionError {
                step_id: step_def.step_id.clone(),
                message: "step parameters must be an object to attach command_manifest".to_string(),
            });
        };
        if !object.contains_key("command_manifest") {
            let value =
                serde_json::to_value(manifest).map_err(|err| EngineError::StepExecutionError {
                    step_id: step_def.step_id.clone(),
                    message: format!("failed to serialize command_manifest: {err}"),
                })?;
            object.insert("command_manifest".to_string(), value);
        }
        Ok(Some(params))
    }

    /// Export a normalized smoke trace of the recorded step/outcome sequence
    /// sequence plus the terminal `final_outcome`, suitable for deterministic
    /// offline replay. Borrows the private persistence connection so callers
    /// using the default in-memory `::new()` path can still capture a trace.
    /// @plan:PLAN-LUTHER-ISSUE-19-SMOKE-REPLAY
    /// @requirement:REQ-SMOKE-REPLAY-001
    pub fn export_trace(
        &self,
        final_outcome: &RunOutcome,
    ) -> Result<crate::persistence::trace::SmokeTrace, EngineError> {
        let conn = self.conn.borrow();
        let trace = crate::persistence::trace::export_trace(
            &conn,
            &self.instance.run_id,
            self.instance.workflow_type_id(),
            self.instance.config_id(),
            final_outcome,
        )?;
        Ok(trace)
    }

    /// Get the total loop count across all edges (for backward compat and testing).
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P12
    pub fn loop_count(&self) -> u32 {
        self.edge_loop_counts.values().sum()
    }

    /// Try to resume from a checkpoint in the shared default database.
    /// Returns true if a checkpoint was found and loaded, false otherwise.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    /// @requirement:REQ-EARS-ENG-004
    pub fn try_resume(&mut self) -> Result<bool, EngineError> {
        // Try to load checkpoint from the shared default connection
        let checkpoint = crate::persistence::load_checkpoint(&self.instance.run_id)
            .map_err(|e| EngineError::PersistenceError(e.to_string()))?;

        if let Some(cp) = checkpoint {
            // Resume from checkpoint
            self.instance.transition_to(&cp.step_id);
            self.retry_count = cp.state_snapshot.retry_count;
            self.edge_loop_counts = cp.state_snapshot.edge_loop_counts.clone();
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Create a checkpoint for the current state.
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P12
    fn create_checkpoint(&self, step_id: &str, status: &str) -> Checkpoint {
        let snapshot = StateSnapshot {
            retry_count: self.retry_count,
            loop_count: self.loop_count(),
            edge_loop_counts: self.edge_loop_counts.clone(),
            context: self.context.checkpoint_values(),
            status: status.to_string(),
        };
        Checkpoint::with_snapshot(&self.instance.run_id, step_id, snapshot)
    }

    /// Resolve the next step based on the current step and outcome.
    fn resolve_next_step(
        &self,
        step_id: &str,
        outcome: &StepOutcome,
    ) -> Result<Option<String>, EngineError> {
        // Use the workflow type transitions directly
        // The workflow type uses workflow::schema::TransitionDef which has the same structure
        let transitions = &self.instance.workflow_type.transitions;

        // Use the transition resolver for schema transitions
        let next_step = resolve_transition_schema(step_id, outcome, transitions);

        Ok(next_step)
    }

    fn is_limited_transition(
        &self,
        current_step: &str,
        next_step: &str,
        transition: Option<&TransitionDef>,
    ) -> bool {
        self.is_loop_back(current_step, next_step)
            || transition
                .and_then(|transition| transition.max_iterations)
                .is_some()
    }

    /// Check if transitioning to the next step is a loop back.
    fn is_loop_back(&self, current_step: &str, next_step: &str) -> bool {
        // Get the index of each step in the workflow
        let steps = &self.instance.workflow_type.steps;

        let current_idx = steps.iter().position(|s| s.step_id == current_step);
        let next_idx = steps.iter().position(|s| s.step_id == next_step);

        match (current_idx, next_idx) {
            (Some(curr), Some(next)) => next <= curr,
            _ => false,
        }
    }
}

#[cfg(test)]
mod runner_tests;
