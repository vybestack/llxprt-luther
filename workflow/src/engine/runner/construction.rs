//! Runner construction and initial run-record persistence.
//!
//! Extracted from the main runner module to keep `runner.rs` under the
//! source-size budget. These methods own the construction lifecycle:
//! creating a runner (in-memory, file-backed, or launch-specific) and
//! persisting the initial `Starting` run-record plus subsequent
//! best-effort metadata updates and typed lifecycle events.
//!
//! The public constructor API is unchanged; these are `impl EngineRunner`
//! blocks so callers see no difference.
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use rusqlite::Connection;

use crate::engine::executor::ExecutorRegistry;
use crate::engine::instance::WorkflowInstance;
use crate::persistence::{
    append_typed_event_with_conn, persist_run_with_conn, EventType, RunMetadata, RunStatus,
};

use super::support::{build_step_context, load_checkpoint_state, open_initialized_connection};
use super::{EngineError, EngineRunner, RunContext};

impl EngineRunner {
    /// Create a new engine runner for the given workflow instance.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    /// @plan:PLAN-20260408-STEP-EXEC.P06
    /// @requirement:REQ-EARS-ENG-001
    pub fn new(
        instance: WorkflowInstance,
        registry: ExecutorRegistry,
    ) -> Result<Self, EngineError> {
        Self::with_context(instance, registry, RunContext::default())
    }

    /// Create a new in-memory engine runner with an immutable [`RunContext`].
    ///
    /// The `RunContext.workspace_path` is the authoritative source for the
    /// immutable `StepContext::work_dir` (resolved before construction), so a
    /// shell step cannot redirect workspace-mutating cleanup verification by
    /// overwriting the mutable `work_dir` context variable. This constructor
    /// never creates directories: callers create the workspace explicitly when
    /// required. The workspace authority is frozen at construction time.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    /// @plan:PLAN-20260408-STEP-EXEC.P06
    /// @requirement:REQ-EARS-ENG-001
    pub fn with_context(
        instance: WorkflowInstance,
        registry: ExecutorRegistry,
        run_context: RunContext,
    ) -> Result<Self, EngineError> {
        let max_retries = instance.config.runtime.max_retries;
        let max_loops = instance.config.guard_limits.max_iterations.unwrap_or(10);
        let context = build_step_context(&instance, Some(&run_context))?;

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
            interrupted: Arc::new(AtomicBool::new(false)),
            registry,
            context,
            run_context,
            persist_registry: false,
            pending_failure_cleanup: None,
            terminal_ownership_failure: false,
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
        let context = build_step_context(&instance, Some(&run_context))?;

        let mut runner = Self {
            instance,
            retry_count,
            edge_loop_counts,
            max_retries,
            max_loops,
            conn: RefCell::new(conn),
            interrupted: Arc::new(AtomicBool::new(false)),
            registry,
            context,
            run_context,
            persist_registry: true,
            pending_failure_cleanup: None,
            terminal_ownership_failure: false,
        };

        // Persist an initial run record so in-flight runs are visible before
        // they complete. The run context is already attached above, so the
        // first durable `Starting` row includes path and GitHub metadata.
        // Best-effort: a persistence failure must not block execution.
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P05
        runner.persist_initial_run();

        Ok(runner)
    }

    /// Create a new engine runner for a **fresh launch** with a custom database
    /// path and run context, failing closed if the initial `Starting`
    /// `RunMetadata` row cannot be atomically inserted.
    ///
    /// Unlike [`with_db_path_and_context`](Self::with_db_path_and_context)
    /// (which best-effort upserts and is shared with resume), this constructor
    /// uses an atomic `INSERT OR FAIL` so a `run_id` collision or DB error
    /// surfaces immediately as an [`EngineError`] rather than silently
    /// overwriting an existing row. When [`RunContext::launch_provenance`] is
    /// `Some` (the normal case for new records), the provenance is persisted in
    /// the same atomic insert, so the durable `Starting` row is the
    /// authoritative launch record.
    ///
    /// @plan:PLAN-20260722-ISSUE158-LAUNCH-PERSISTENCE
    pub fn with_db_path_for_launch(
        instance: WorkflowInstance,
        registry: ExecutorRegistry,
        db_path: impl AsRef<Path>,
        run_context: RunContext,
    ) -> Result<Self, EngineError> {
        let max_retries = instance.config.runtime.max_retries;
        let max_loops = instance.config.guard_limits.max_iterations.unwrap_or(10);
        let conn = open_initialized_connection(db_path.as_ref())?;
        let (retry_count, edge_loop_counts) = load_checkpoint_state(&conn, &instance.run_id);
        let context = build_step_context(&instance, Some(&run_context))?;

        let mut runner = Self {
            instance,
            retry_count,
            edge_loop_counts,
            max_retries,
            max_loops,
            conn: RefCell::new(conn),
            interrupted: Arc::new(AtomicBool::new(false)),
            registry,
            context,
            run_context,
            persist_registry: true,
            pending_failure_cleanup: None,
            terminal_ownership_failure: false,
        };

        // Atomically insert the initial Starting row. Fail closed on collision
        // or DB error rather than overwriting a prior run's record.
        // @plan:PLAN-20260722-ISSUE158-LAUNCH-PERSISTENCE
        runner.persist_initial_run_for_launch()?;

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

    /// Attach an ephemeral [`WorkspaceAuthorization`] reconstructed by a resume
    /// surface from a freshly-verified workspace descriptor.
    ///
    /// **Issue 158 slice 6:** resume surfaces (daemon, child, CLI) call this
    /// AFTER reconstructing the authorization via
    /// [`prepare_resume_authorization`](crate::engine::continuation::prepare_resume_authorization)
    /// and BEFORE any resumed step executes. The authorization propagates into
    /// the internal [`StepContext`] via `build_step_context`, which reads it
    /// from `RunContext.workspace_authorization`. It is **never persisted**:
    /// `build_metadata` does not copy it into `RunMetadata`.
    ///
    /// This method sets the authorization on both the `RunContext` and the
    /// already-constructed `StepContext` so a runner constructed before the
    /// authorization is known (the common resume pattern) receives it without
    /// rebuilding.
    pub fn attach_workspace_authorization(
        &mut self,
        authorization: crate::engine::workspace_ownership::WorkspaceAuthorization,
    ) {
        self.run_context.workspace_authorization = Some(authorization);
        self.context.set_workspace_authorization(authorization);
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
    pub(super) fn build_metadata(&self, status: RunStatus) -> RunMetadata {
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
        metadata.launch_provenance = self.run_context.launch_provenance.clone();
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

    /// Atomically insert the initial `Starting` run record for a fresh launch,
    /// failing closed on collision or DB error.
    ///
    /// Unlike [`persist_initial_run`](Self::persist_initial_run) (best-effort
    /// upsert shared with resume), this uses
    /// [`insert_initial_run_with_conn`](crate::persistence::insert_initial_run_with_conn)
    /// so a `run_id` collision surfaces immediately and the existing row is
    /// preserved. When `RunContext.launch_provenance` is `Some` the provenance
    /// travels in the same atomic insert.
    ///
    /// @plan:PLAN-20260722-ISSUE158-LAUNCH-PERSISTENCE
    fn persist_initial_run_for_launch(&mut self) -> Result<(), EngineError> {
        if !self.persist_registry {
            return Ok(());
        }
        let mut metadata = self.build_metadata(RunStatus::Starting);
        metadata.current_step = self.first_step_id();
        let conn = self.conn.borrow();
        let outcome = crate::persistence::insert_initial_run_with_conn(&conn, &metadata)
            .map_err(|err| EngineError::PersistenceError(err.to_string()))?;
        match outcome {
            crate::persistence::InitialRunInsert::Inserted => Ok(()),
            crate::persistence::InitialRunInsert::Collision => {
                Err(EngineError::PersistenceError(format!(
                    "launch collision: a run record already exists for run_id '{}'; refusing to \
                     overwrite the existing launch record",
                    self.instance.run_id
                )))
            }
        }
    }

    /// Best-effort persist of a run metadata record to the registry.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    pub(super) fn persist_metadata(&self, metadata: &RunMetadata) {
        let conn = self.conn.borrow();
        let _ = persist_run_with_conn(&conn, metadata);
    }

    /// Load the current run metadata from the registry, if present.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    pub(super) fn load_metadata(&self) -> Option<RunMetadata> {
        let conn = self.conn.borrow();
        crate::persistence::get_run_with_conn(&conn, &self.instance.run_id)
            .ok()
            .flatten()
    }

    /// Record a typed lifecycle event (best-effort).
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    pub(super) fn record_event(
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
}
