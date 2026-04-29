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
    append_event_with_conn, load_checkpoint_with_conn, save_checkpoint_with_conn, Checkpoint,
    PersistenceError, RunMetadata, RunStatus, SqliteStoreRef, StateSnapshot,
};

/// Errors that can occur during workflow execution.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
/// @requirement:REQ-EARS-ENG-003,REQ-EARS-PERSIST-004
#[derive(Error, Debug)]
pub enum EngineError {
    #[error("step execution failed: {step_id} - {message}")]
    StepExecutionError { step_id: String, message: String },

    #[error("transition not found from {step_id} with outcome {outcome:?}")]
    TransitionNotFound { step_id: String, outcome: StepOutcome },

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
}

impl From<PersistenceError> for EngineError {
    fn from(err: PersistenceError) -> Self {
        EngineError::PersistenceError(err.to_string())
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
}

/// The workflow execution engine.
/// Manages the execution lifecycle of a workflow instance.
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
    max_retries: u32,
    /// Maximum remediation loops allowed.
    max_loops: u32,
    /// SQLite connection for persistence.
    conn: RefCell<Connection>,
    /// Flag indicating if an interrupt was received.
    interrupted: RefCell<bool>,
    /// Executor registry for dispatching step execution.
    registry: ExecutorRegistry,
    /// Step execution context for variable storage and interpolation.
    context: StepContext,
}

impl EngineRunner {
    /// Create a new engine runner for the given workflow instance.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    /// @plan:PLAN-20260408-STEP-EXEC.P06
    /// @requirement:REQ-EARS-ENG-001
    pub fn new(instance: WorkflowInstance, registry: ExecutorRegistry) -> Result<Self, EngineError> {
        let max_retries = instance.config.runtime.max_retries;
        let max_loops = instance.config.guard_limits.max_iterations.unwrap_or(10);

        // Create an in-memory SQLite connection for persistence
        let conn = Connection::open_in_memory()
            .map_err(|e| EngineError::PersistenceError(
                format!("Failed to create in-memory database: {e}")
            ))?;

        // Initialize checkpoint schema
        crate::persistence::checkpoint::init_checkpoint_table(&conn)
            .map_err(|e| EngineError::PersistenceError(
                format!("Failed to initialize checkpoint schema: {e}")
            ))?;

        // Create working directory path: tempdir/run_id
        let work_dir = std::env::temp_dir().join(&instance.run_id);

        // Initialize StepContext with work_dir and run_id
        let mut context = StepContext::new(work_dir, instance.run_id.clone());

        // Load config variables into context
        /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
        /// @requirement:REQ-LF-PROF-003
        for (key, value) in &instance.config.variables {
            context.set(key, value);
        }

        // If work_dir is specified in config variables, create it and set it
        /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
        /// @requirement:REQ-LF-WS-001
        if let Some(work_dir_str) = instance.config.variables.get("work_dir") {
            let path = std::path::PathBuf::from(work_dir_str);
            std::fs::create_dir_all(&path)
                .map_err(|e| EngineError::InvalidState(
                    format!("Failed to create work_dir '{work_dir_str}': {e}")
                ))?;
            context.set_work_dir(path);
        }

        Ok(Self {
            instance,
            retry_count: 0,
            edge_loop_counts: HashMap::new(),
            max_retries,
            max_loops,
            conn: RefCell::new(conn),
            interrupted: RefCell::new(false),
            registry,
            context,
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
        let max_retries = instance.config.runtime.max_retries;
        let max_loops = instance.config.guard_limits.max_iterations.unwrap_or(10);

        let conn = Connection::open(db_path).map_err(|e| {
            EngineError::PersistenceError(format!("Failed to open database: {}", e))
        })?;

        // Initialize checkpoint schema
        crate::persistence::checkpoint::init_checkpoint_table(&conn)
            .map_err(|e| EngineError::PersistenceError(
                format!("Failed to initialize checkpoint schema: {e}")
            ))?;

        // Initialize runs table schema for metadata
        crate::persistence::run_metadata::init_runs_table(&conn)
            .map_err(|e| EngineError::PersistenceError(
                format!("Failed to initialize runs schema: {e}")
            ))?;

        // Try to load existing checkpoint for resume
        let (retry_count, edge_loop_counts) =
            if let Ok(Some(checkpoint)) = load_checkpoint_with_conn(&conn, &instance.run_id) {
                (
                    checkpoint.state_snapshot.retry_count,
                    checkpoint.state_snapshot.edge_loop_counts.clone(),
                )
            } else {
                (0, HashMap::new())
            };

        // Create working directory path: tempdir/run_id
        let work_dir = std::env::temp_dir().join(&instance.run_id);

        // Initialize StepContext with work_dir and run_id
        let mut context = StepContext::new(work_dir, instance.run_id.clone());

        // Load config variables into context
        /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
        /// @requirement:REQ-LF-PROF-003
        for (key, value) in &instance.config.variables {
            context.set(key, value);
        }

        // If work_dir is specified in config variables, create it and set it
        /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
        /// @requirement:REQ-LF-WS-001
        if let Some(work_dir_str) = instance.config.variables.get("work_dir") {
            let path = std::path::PathBuf::from(work_dir_str);
            std::fs::create_dir_all(&path)
                .map_err(|e| EngineError::InvalidState(
                    format!("Failed to create work_dir '{}': {}", work_dir_str, e)
                ))?;
            context.set_work_dir(path);
        }

        Ok(Self {
            instance,
            retry_count,
            edge_loop_counts,
            max_retries,
            max_loops,
            conn: RefCell::new(conn),
            interrupted: RefCell::new(false),
            registry,
            context,
        })
    }

    /// Execute the workflow instance.
    /// Runs through steps, handling transitions and outcomes.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P14
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
    /// @requirement:REQ-EARS-ENG-002,REQ-EARS-ENG-003,REQ-EARS-ROUTE-001,REQ-LF-LOOP-001,REQ-LF-LOOP-002,REQ-LF-LOOP-003,REQ-LF-LOOP-004,REQ-LF-FAIL-001,REQ-LF-FAIL-005
    pub fn run(&mut self) -> Result<RunOutcome, EngineError> {
        // Check if we should resume from a checkpoint
        let conn = self.conn.borrow();
        if let Some(checkpoint) = load_checkpoint_with_conn(&conn, &self.instance.run_id)? {
            // Resume from checkpoint
            self.instance.transition_to(&checkpoint.step_id);
            self.retry_count = checkpoint.state_snapshot.retry_count;
            self.edge_loop_counts = checkpoint.state_snapshot.edge_loop_counts.clone();
        }
        drop(conn);

        let mut current_step_id = self.instance.current_state.clone();

        loop {
            // Check for interrupt
            if *self.interrupted.borrow() {
                let checkpoint = self.create_checkpoint(&current_step_id, "interrupted");
                let conn = self.conn.borrow();
                save_checkpoint_with_conn(&conn, &checkpoint)?;
                let run_outcome = RunOutcome::Interrupted {
                    step_id: current_step_id.clone(),
                };
                let _ = self.record_run_completion(&run_outcome, &current_step_id);
                return Ok(run_outcome);
            }

            // Set current step on context for namespaced storage
            self.context.set_current_step_id(&current_step_id);

            eprintln!("[engine] Executing step: {}", current_step_id);

            // Execute the current step
            let outcome = self.execute_step(&current_step_id)?;

            eprintln!("[engine] Step '{}' outcome: {}", current_step_id, outcome);
            if outcome != StepOutcome::Success {
                if let Some(stderr) = self.context.get("stderr") {
                    if !stderr.is_empty() {
                        eprintln!("[engine] stderr: {}", &stderr[..stderr.len().min(500)]);
                    }
                }
                if let Some(stdout) = self.context.get("stdout") {
                    if !stdout.is_empty() {
                        eprintln!("[engine] stdout: {}", &stdout[..stdout.len().min(500)]);
                    }
                }
            }

            // Persist checkpoint and event
            let checkpoint = self.create_checkpoint(&current_step_id, "completed");
            let conn = self.conn.borrow();
            save_checkpoint_with_conn(&conn, &checkpoint)?;
            append_event_with_conn(
                &conn,
                &self.instance.run_id,
                &current_step_id,
                &outcome.to_string(),
                chrono::Utc::now(),
            )?;
            drop(conn);

            // Check for Abandon outcome (early return - terminal)
            /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
            /// @requirement:REQ-LF-FAIL-001
            if outcome == StepOutcome::Abandon {
                let run_outcome = RunOutcome::Abandoned {
                    step_id: current_step_id.clone(),
                    reason: "Loop limit exceeded".to_string(),
                };
                let _ = self.record_run_completion(&run_outcome, &current_step_id);
                return Ok(run_outcome);
            }

            // Resolve the next step based on outcome
            /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
            /// @requirement:REQ-LF-FAIL-001
            let next_step = self.resolve_next_step(&current_step_id, &outcome)?;

            match next_step {
                Some(next_step_id) => {
                    // Compute edge key and find transition definition for per-edge limit
                    let edge_key = format!("{}:{}", current_step_id, next_step_id);
                    let transition_def = self.find_transition(&current_step_id, &outcome);
                    let edge_limit = transition_def
                        .and_then(|t| t.max_iterations)
                        .unwrap_or(self.max_loops);

                    // Check if this is a loop back (next step is earlier in the workflow)
                    if self.is_loop_back(&current_step_id, &next_step_id) {
                        let current_count = self.edge_loop_counts.get(&edge_key).copied().unwrap_or(0);
                        if current_count >= edge_limit {
                            // Per-edge loop limit exceeded
                            let run_outcome = RunOutcome::Abandoned {
                                step_id: current_step_id.clone(),
                                reason: format!(
                                    "Per-edge loop limit ({}) exceeded on edge {}",
                                    edge_limit, edge_key
                                ),
                            };
                            let _ = self.record_run_completion(&run_outcome, &current_step_id);
                            return Ok(run_outcome);
                        }
                        self.edge_loop_counts.insert(edge_key, current_count + 1);
                    }
                    current_step_id = next_step_id;
                    self.instance.transition_to(&current_step_id);
                }
                None => {
                    // No transition found - determine outcome based on step outcome
                    /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
                    /// @requirement:REQ-LF-FAIL-001
                    let run_outcome = match outcome {
                        StepOutcome::Success => RunOutcome::Success,
                        StepOutcome::Fatal => RunOutcome::Failure {
                            step_id: current_step_id.clone(),
                            reason: "Fatal error occurred".to_string(),
                        },
                        StepOutcome::Fixable => RunOutcome::Failure {
                            step_id: current_step_id.clone(),
                            reason: "Fixable error with no recovery transition".to_string(),
                        },
                        _ => RunOutcome::Failure {
                            step_id: current_step_id.clone(),
                            reason: "Unexpected outcome".to_string(),
                        },
                    };
                    let _ = self.record_run_completion(&run_outcome, &current_step_id);
                    return Ok(run_outcome);
                }
            }
        }
    }

    /// Find the transition definition matching the given from step and outcome.
    /// Returns Option<&TransitionDef> to access max_iterations for per-edge loop limits.
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P14
    /// @requirement:REQ-LF-LOOP-001,REQ-LF-LOOP-004
    fn find_transition(
        &self,
        from: &str,
        outcome: &StepOutcome,
    ) -> Option<&crate::workflow::schema::TransitionDef> {
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
        let params = step_def.parameters.as_ref().map_or(
            &serde_json::Value::Null,
            |p| p,
        );

        // Dispatch to the registry for execution
        self.registry.dispatch(step_type, &mut self.context, params)
    }

    /// Handle an interrupt signal and prepare for clean shutdown.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    /// @requirement:REQ-EARS-ENG-004
    pub fn handle_interrupt(&mut self) -> Result<RunOutcome, EngineError> {
        let current_step_id = self.instance.current_state.clone();

        // Mark as interrupted
        *self.interrupted.borrow_mut() = true;

        // Persist interrupt checkpoint
        let checkpoint = self.create_checkpoint(&current_step_id, "interrupted");
        let conn = self.conn.borrow();
        save_checkpoint_with_conn(&conn, &checkpoint)?;
        drop(conn);

        Ok(RunOutcome::Interrupted {
            step_id: current_step_id,
        })
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
    fn record_run_completion(&self, outcome: &RunOutcome, final_step_id: &str) -> Result<(), EngineError> {
        // Get issue_number from context if available
        let _issue_number = self.context.get("issue_number").map(|s| s.to_string());

        // Determine RunStatus based on outcome
        let status = match outcome {
            RunOutcome::Success => RunStatus::Completed,
            RunOutcome::Failure { .. } => RunStatus::Failed,
            RunOutcome::Abandoned { .. } => RunStatus::Abandoned,
            RunOutcome::Interrupted { .. } => RunStatus::Paused,
        };

        // Create run metadata
        let mut metadata = RunMetadata::new(
            &self.instance.run_id,
            &self.instance.workflow_type.workflow_type_id,
            &self.instance.config.config_id,
        );
        metadata.status = status;
        metadata.set_current_step(final_step_id);

        // Persist to the runner's connection
        let conn = self.conn.borrow();
        let store = SqliteStoreRef { conn: &conn };
        store.persist_run(&metadata).map_err(|e| {
            EngineError::PersistenceError(format!("Failed to record run completion: {}", e))
        })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_error_display_formats_correctly() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P08
        let err = EngineError::StepExecutionError {
            step_id: "test_step".to_string(),
            message: "something failed".to_string(),
        };
        assert!(err.to_string().contains("test_step"));
    }

    #[test]
    fn run_outcome_variants_exist() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P08
        let _success = RunOutcome::Success;
        let _failure = RunOutcome::Failure {
            step_id: "s1".to_string(),
            reason: "test".to_string(),
        };
        let _abandoned = RunOutcome::Abandoned {
            step_id: "s2".to_string(),
            reason: "loop".to_string(),
        };
        let _interrupted = RunOutcome::Interrupted {
            step_id: "s3".to_string(),
        };
    }

    #[test]
    fn engine_runner_can_be_created() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P08
        // @plan:PLAN-20260408-STEP-EXEC.P06
        use crate::workflow::schema::{GuardLimits, RepoConfig, RuntimeConfig, WorkflowConfig, WorkflowType};

        let workflow_type = WorkflowType {
            workflow_type_id: "test".to_string(),
            steps: vec![],
            transitions: vec![],
            guards: Default::default(),
        };

        let config = WorkflowConfig {
            config_id: "test-config".to_string(),
            workflow_type_id: "test".to_string(),
            runtime: RuntimeConfig {
                timeout_seconds: 3600,
                max_retries: 3,
                parallel_steps: None,
                log_level: None,
            },
            repo: RepoConfig {
                workspace_strategy: "temp".to_string(),
                branch_template: "test-{run_id}".to_string(),
                base_branch: Some("main".to_string()),
                workspace_root: None,
            },
            guard_limits: GuardLimits {
                max_iterations: Some(3),
                max_file_changes: Some(50),
                max_tokens: Some(10000),
                max_cost: Some(10.0),
            },
            variables: std::collections::HashMap::new(),
        };

        let instance = WorkflowInstance::create(workflow_type, config);
        let registry = crate::engine::executor::ExecutorRegistry::with_defaults();
        let runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");
        assert!(!runner.run_id().is_empty());
    }
}
