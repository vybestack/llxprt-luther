/// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
/// Workflow execution engine - runs workflow instances step by step.
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

use rusqlite::Connection;
use thiserror::Error;

use crate::engine::executor::{ExecutorRegistry, StepContext};
use crate::engine::instance::WorkflowInstance;
use crate::engine::transition::{resolve_transition_schema, StepOutcome};
use crate::persistence::{
    append_typed_event_with_conn, load_checkpoint_with_conn, persist_run_with_conn,
    save_checkpoint_with_conn, Checkpoint, EventType, PersistenceError, RunMetadata, RunStatus,
    StateSnapshot, CHECKPOINT_STATUS_WAITING,
};
use crate::workflow::schema::{StepDef, TransitionDef};

mod target_path_context;
use target_path_context::seed_target_paths;

/// Contextual metadata for a run: paths and GitHub references.
/// Used to populate the persistent run registry beyond the core identifiers.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
#[derive(Debug, Clone, Default)]
pub struct RunContext {
    pub log_path: Option<String>,
    pub artifact_root: Option<String>,
    pub workspace_path: Option<String>,
    pub repository: Option<String>,
    pub issue_number: Option<i64>,
    pub pr_number: Option<i64>,
    pub head_sha: Option<String>,
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
}

impl EngineRunner {
    /// Create a new engine runner for the given workflow instance.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    /// @plan:PLAN-20260408-STEP-EXEC.P06
    /// @requirement:REQ-EARS-ENG-001
    pub fn new(
        instance: WorkflowInstance,
        registry: ExecutorRegistry,
    ) -> Result<Self, EngineError> {
        let max_retries = instance.config.runtime.max_retries;
        let max_loops = instance.config.guard_limits.max_iterations.unwrap_or(10);
        let context = build_step_context(&instance)?;

        // Create an in-memory SQLite connection for persistence
        let conn = Connection::open_in_memory().map_err(|e| {
            EngineError::PersistenceError(format!("Failed to create in-memory database: {e}"))
        })?;

        // Initialize checkpoint schema
        crate::persistence::checkpoint::init_checkpoint_table(&conn).map_err(|e| {
            EngineError::PersistenceError(format!("Failed to initialize checkpoint schema: {e}"))
        })?;

        Ok(Self {
            instance,
            retry_count: 0,
            edge_loop_counts: HashMap::new(),
            max_retries,
            max_loops,
            conn: RefCell::new(conn),
            interrupted: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            registry,
            context,
            run_context: RunContext::default(),
            persist_registry: false,
        })
    }

    /// Create a new engine runner for the given workflow instance with a custom database path.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    /// @plan:PLAN-20260408-STEP-EXEC.P06
    /// @requirement:REQ-EARS-ENG-001
    pub fn with_db_path(
        instance: WorkflowInstance,
        registry: ExecutorRegistry,
        db_path: impl AsRef<Path>,
    ) -> Result<Self, EngineError> {
        Self::with_db_path_and_context(instance, registry, db_path, RunContext::default())
    }

    /// Create a new engine runner with a custom database path and run context.
    ///
    /// The provided [`RunContext`] is attached *before* the initial run record
    /// is persisted, so the first durable `Starting` row already includes path
    /// and GitHub metadata. Use this instead of chaining
    /// [`with_run_context`](Self::with_run_context) after `with_db_path` when the
    /// context is known up front.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    /// @requirement:REQ-EARS-ENG-001
    pub fn with_db_path_and_context(
        instance: WorkflowInstance,
        registry: ExecutorRegistry,
        db_path: impl AsRef<Path>,
        run_context: RunContext,
    ) -> Result<Self, EngineError> {
        let max_retries = instance.config.runtime.max_retries;
        let max_loops = instance.config.guard_limits.max_iterations.unwrap_or(10);
        let conn = open_initialized_connection(db_path.as_ref())?;
        let (retry_count, edge_loop_counts) = load_checkpoint_state(&conn, &instance.run_id);
        let context = build_step_context(&instance)?;

        let mut runner = Self {
            instance,
            retry_count,
            edge_loop_counts,
            max_retries,
            max_loops,
            conn: RefCell::new(conn),
            interrupted: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            registry,
            context,
            run_context,
            persist_registry: true,
        };

        // Persist an initial run record so in-flight runs are visible before
        // they complete. The run context is already attached above, so the
        // first durable `Starting` row includes path and GitHub metadata.
        // Best-effort: a persistence failure must not block execution.
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P05
        runner.persist_initial_run();

        Ok(runner)
    }

    /// Attach contextual run metadata (paths, GitHub refs) and persist it.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    pub fn with_run_context(mut self, ctx: RunContext) -> Self {
        self.run_context = ctx;
        if self.persist_registry {
            let mut metadata = self.build_metadata(RunStatus::Starting);
            metadata.current_step = self.first_step_id();
            self.persist_metadata(&metadata);
        }
        self
    }

    /// Determine the first step id of the workflow, if any.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    fn first_step_id(&self) -> Option<String> {
        self.instance
            .workflow_type
            .steps
            .first()
            .map(|s| s.step_id.clone())
    }

    /// Build a `RunMetadata` from the current instance + run context.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    fn build_metadata(&self, status: RunStatus) -> RunMetadata {
        let mut metadata = RunMetadata::new(
            &self.instance.run_id,
            &self.instance.workflow_type.workflow_type_id,
            &self.instance.config.config_id,
        );
        metadata.status = status;
        metadata.process_pid = Some(std::process::id());
        metadata.log_path = self.run_context.log_path.clone();
        metadata.artifact_root = self.run_context.artifact_root.clone();
        metadata.workspace_path = self.run_context.workspace_path.clone();
        metadata.repository = self.run_context.repository.clone();
        metadata.issue_number = self.run_context.issue_number;
        metadata.pr_number = self.run_context.pr_number;
        metadata.head_sha = self.run_context.head_sha.clone();
        metadata
    }

    /// Persist the initial run record (status Starting) at construction time.
    ///
    /// Non-destructive when a row already exists: a reopened/in-flight run (e.g.
    /// reconstructed for operator continuation) already represents this run with
    /// its own status, `created_at`, current step, and history. Overwriting it
    /// with a fresh `Starting` record would reset `created_at`, clear history,
    /// and reset `current_step` to the first step, so we skip persistence when a
    /// row is present and only write the fresh `Starting` row on first
    /// construction.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    /// @plan:PLAN-20260623-LUTHER-CONTINUATION
    fn persist_initial_run(&mut self) {
        if !self.persist_registry {
            return;
        }
        if self.load_metadata().is_some() {
            return;
        }
        let mut metadata = self.build_metadata(RunStatus::Starting);
        metadata.current_step = self.first_step_id();
        self.persist_metadata(&metadata);
    }

    /// Best-effort persist of a run metadata record to the registry.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    fn persist_metadata(&self, metadata: &RunMetadata) {
        let conn = self.conn.borrow();
        let _ = persist_run_with_conn(&conn, metadata);
    }

    /// Load the current run metadata from the registry, if present.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    fn load_metadata(&self) -> Option<RunMetadata> {
        let conn = self.conn.borrow();
        crate::persistence::get_run_with_conn(&conn, &self.instance.run_id)
            .ok()
            .flatten()
    }

    /// Record a typed lifecycle event (best-effort).
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    fn record_event(
        &self,
        event_type: EventType,
        step_id: &str,
        outcome: &str,
        details: Option<&str>,
    ) {
        let conn = self.conn.borrow();
        let _ = append_typed_event_with_conn(
            &conn,
            &self.instance.run_id,
            step_id,
            outcome,
            event_type,
            details,
            chrono::Utc::now(),
        );
    }

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

        let mut current_step_id = self.instance.current_state.clone();

        loop {
            if self.interrupted.load(std::sync::atomic::Ordering::SeqCst) {
                return self.interrupt_at_step(&current_step_id);
            }

            self.prepare_step_start(&current_step_id);
            let outcome = self.execute_current_step(&current_step_id)?;
            self.persist_step_outcome(&current_step_id, &outcome)?;
            self.update_previous_step_metadata(&current_step_id, &outcome);

            if outcome == StepOutcome::Abandon {
                return Ok(self.abandon_at_step(&current_step_id));
            }

            match self.resolve_next_step(&current_step_id, &outcome)? {
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
        let conn = self.conn.borrow();
        if let Some(checkpoint) = load_checkpoint_with_conn(&conn, &self.instance.run_id)? {
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

        let run_outcome = RunOutcome::Interrupted {
            step_id: current_step_id.to_string(),
        };
        let _ = self.record_run_completion(&run_outcome, current_step_id);
        Ok(run_outcome)
    }

    fn prepare_step_start(&mut self, current_step_id: &str) {
        self.context.set_current_step_id(current_step_id);
        if self.persist_registry {
            if let Some(mut md) = self.load_metadata() {
                md.set_current_step(current_step_id);
                md.set_next_step_candidates(self.compute_next_step_candidates(current_step_id));
                self.persist_metadata(&md);
            }
            self.record_event(EventType::StepStart, current_step_id, "started", None);
        }
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

    fn persist_step_outcome(
        &self,
        current_step_id: &str,
        outcome: &StepOutcome,
    ) -> Result<(), EngineError> {
        let checkpoint = self.create_checkpoint(current_step_id, "completed");
        let conn = self.conn.borrow();
        save_checkpoint_with_conn(&conn, &checkpoint)?;
        let _ = append_typed_event_with_conn(
            &conn,
            &self.instance.run_id,
            current_step_id,
            &outcome.to_string(),
            EventType::StepOutcome,
            None,
            chrono::Utc::now(),
        );
        Ok(())
    }

    fn update_previous_step_metadata(&self, current_step_id: &str, outcome: &StepOutcome) {
        if self.persist_registry {
            if let Some(mut md) = self.load_metadata() {
                md.set_previous_step_and_outcome(current_step_id, outcome.to_string());
                md.set_next_step_candidates(self.compute_next_step_candidates(current_step_id));
                self.persist_metadata(&md);
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

    fn finish_without_transition(
        &self,
        current_step_id: &str,
        outcome: &StepOutcome,
    ) -> Result<RunOutcome, EngineError> {
        if *outcome == StepOutcome::Wait {
            return self.pause_for_external_wait(current_step_id);
        }

        let run_outcome = run_outcome_without_transition(current_step_id, outcome);
        let _ = self.record_run_completion(&run_outcome, current_step_id);
        Ok(run_outcome)
    }

    /// Persist a resumable `waiting` checkpoint at the current step and return
    /// a non-advancing `WaitingExternal` outcome. The resume point is the wait
    /// step itself, so a later resume re-enters it and refreshes external state.
    /// @plan:PLAN-20260623-LUTHER-CONTINUATION
    fn pause_for_external_wait(&self, step_id: &str) -> Result<RunOutcome, EngineError> {
        let checkpoint = self.create_checkpoint(step_id, CHECKPOINT_STATUS_WAITING);
        {
            let conn = self.conn.borrow();
            save_checkpoint_with_conn(&conn, &checkpoint)?;
        }
        let run_outcome = RunOutcome::WaitingExternal {
            step_id: step_id.to_string(),
            reason: "External condition still pending at watch limit".to_string(),
        };
        let _ = self.record_run_completion(&run_outcome, step_id);
        Ok(run_outcome)
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

    /// Export a normalized smoke trace of this run's recorded step/outcome
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

    /// Set the working directory for step execution context.
    /// @plan:PLAN-20260408-STEP-EXEC.P07
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
    /// @requirement:REQ-LF-PROF-003
    pub fn set_work_dir(&mut self, work_dir: std::path::PathBuf) {
        self.context.set_work_dir(work_dir);
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
            context: std::collections::HashMap::new(),
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

    /// Record run completion metadata to the persistence store.
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
    /// @requirement:REQ-LF-FAIL-005
    fn record_run_completion(
        &self,
        outcome: &RunOutcome,
        final_step_id: &str,
    ) -> Result<(), EngineError> {
        // Determine RunStatus based on outcome
        let status = match outcome {
            RunOutcome::Success => RunStatus::Completed,
            RunOutcome::Failure { .. } => RunStatus::Failed,
            RunOutcome::Abandoned { .. } => RunStatus::Abandoned,
            RunOutcome::Interrupted { .. } => RunStatus::Paused,
            // A recoverable external wait maps to a non-terminal status so the
            // run stays visible/active and can be resumed.
            // @plan:PLAN-20260623-LUTHER-CONTINUATION
            RunOutcome::WaitingExternal { .. } => RunStatus::WaitingExternal,
        };

        // Update the existing run record (created at start) rather than
        // creating a fresh one. Fall back to a new record if none exists
        // (e.g. the in-memory ::new() path that does not persist at start).
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P05
        let mut metadata = self
            .load_metadata()
            .unwrap_or_else(|| self.build_metadata(status.clone()));
        metadata.status = status.clone();
        metadata.set_current_step(final_step_id);

        {
            let conn = self.conn.borrow();
            persist_run_with_conn(&conn, &metadata).map_err(|e| {
                EngineError::PersistenceError(format!("Failed to record run completion: {}", e))
            })?;
        }

        // Emit a terminal-state event describing the final status.
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P05
        self.record_event(
            EventType::TerminalState,
            final_step_id,
            &status.to_string(),
            None,
        );

        Ok(())
    }
}

fn preview_for_log(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }

    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    &value[..boundary]
}

fn open_initialized_connection(db_path: &Path) -> Result<Connection, EngineError> {
    let conn = Connection::open(db_path)
        .map_err(|e| EngineError::PersistenceError(format!("Failed to open database: {}", e)))?;

    crate::persistence::checkpoint::init_checkpoint_table(&conn).map_err(|e| {
        EngineError::PersistenceError(format!("Failed to initialize checkpoint schema: {e}"))
    })?;
    crate::persistence::run_metadata::init_runs_table(&conn).map_err(|e| {
        EngineError::PersistenceError(format!("Failed to initialize runs schema: {e}"))
    })?;

    Ok(conn)
}

fn load_checkpoint_state(conn: &Connection, run_id: &str) -> (u32, HashMap<String, u32>) {
    if let Ok(Some(checkpoint)) = load_checkpoint_with_conn(conn, run_id) {
        (
            checkpoint.state_snapshot.retry_count,
            checkpoint.state_snapshot.edge_loop_counts.clone(),
        )
    } else {
        (0, HashMap::new())
    }
}

fn build_step_context(instance: &WorkflowInstance) -> Result<StepContext, EngineError> {
    let work_dir = std::env::temp_dir().join(&instance.run_id);
    let mut context = StepContext::new(work_dir, instance.run_id.clone());

    for (key, value) in &instance.config.variables {
        context.set(key, value);
    }

    if let Some(work_dir_str) = instance.config.variables.get("work_dir") {
        let path = std::path::PathBuf::from(work_dir_str);
        std::fs::create_dir_all(&path).map_err(|e| {
            EngineError::InvalidState(format!(
                "Failed to create work_dir '{}': {}",
                work_dir_str, e
            ))
        })?;
        context.set_work_dir(path);
    }

    seed_target_paths(&mut context, instance);

    Ok(context)
}

fn run_outcome_without_transition(step_id: &str, outcome: &StepOutcome) -> RunOutcome {
    match outcome {
        StepOutcome::Success => RunOutcome::Success,
        StepOutcome::Fatal => RunOutcome::Failure {
            step_id: step_id.to_string(),
            reason: "Fatal error occurred".to_string(),
        },
        StepOutcome::Fixable => RunOutcome::Failure {
            step_id: step_id.to_string(),
            reason: "Fixable error with no recovery transition".to_string(),
        },
        _ => RunOutcome::Failure {
            step_id: step_id.to_string(),
            reason: "Unexpected outcome".to_string(),
        },
    }
}

#[cfg(test)]
mod runner_tests;
