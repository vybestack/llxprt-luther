/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
/// Run metadata persistence - structures for tracking workflow run state and identifiers.

use chrono::{DateTime, Utc};

use crate::workflow::schema::WorkflowRunRef;

/// Status of a workflow run.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunStatus {
    Initialized,
    Running,
    Paused,
    Completed,
    Failed,
    Abandoned,
}

impl std::fmt::Display for RunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunStatus::Initialized => write!(f, "initialized"),
            RunStatus::Running => write!(f, "running"),
            RunStatus::Paused => write!(f, "paused"),
            RunStatus::Completed => write!(f, "completed"),
            RunStatus::Failed => write!(f, "failed"),
            RunStatus::Abandoned => write!(f, "abandoned"),
        }
    }
}

impl std::str::FromStr for RunStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "initialized" => Ok(RunStatus::Initialized),
            "running" => Ok(RunStatus::Running),
            "paused" => Ok(RunStatus::Paused),
            "completed" => Ok(RunStatus::Completed),
            "failed" => Ok(RunStatus::Failed),
            "abandoned" => Ok(RunStatus::Abandoned),
            _ => Err(format!("Unknown run status: {}", s)),
        }
    }
}

/// Metadata for a workflow run persisted to storage.
/// Contains all identifiers needed to reconstruct the run context.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
/// @requirement:REQ-EARS-PERSIST-001,REQ-EARS-SCALE-002
#[derive(Debug, Clone)]
pub struct RunMetadata {
    /// Unique identifier for this run (UUID v4).
    pub run_id: String,
    /// The workflow type identifier used for this run.
    pub workflow_type_id: String,
    /// The config identifier used for this run.
    pub config_id: String,
    /// Current status of the run.
    pub status: RunStatus,
    /// When the run was created.
    pub created_at: DateTime<Utc>,
    /// When the run was last updated (optional, set on status changes).
    pub updated_at: Option<DateTime<Utc>>,
    /// Current step/state of the workflow (optional).
    pub current_step: Option<String>,
}

impl RunMetadata {
    /// Create new run metadata for a new workflow run.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    /// @requirement:REQ-EARS-PERSIST-001
    pub fn new(
        run_id: impl Into<String>,
        workflow_type_id: impl Into<String>,
        config_id: impl Into<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            run_id: run_id.into(),
            workflow_type_id: workflow_type_id.into(),
            config_id: config_id.into(),
            status: RunStatus::Initialized,
            created_at: now,
            updated_at: None,
            current_step: None,
        }
    }

    /// Mark the run as started (Running status).
    pub fn mark_started(&mut self) {
        self.status = RunStatus::Running;
        self.updated_at = Some(Utc::now());
    }

    /// Mark the run as completed.
    pub fn mark_completed(&mut self) {
        self.status = RunStatus::Completed;
        self.updated_at = Some(Utc::now());
    }

    /// Mark the run as failed.
    pub fn mark_failed(&mut self) {
        self.status = RunStatus::Failed;
        self.updated_at = Some(Utc::now());
    }

    /// Update the current step.
    pub fn set_current_step(&mut self, step: impl Into<String>) {
        self.current_step = Some(step.into());
        self.updated_at = Some(Utc::now());
    }
}

/// Create run metadata from a WorkflowRunRef.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub fn run_metadata_from_ref(run_ref: &WorkflowRunRef) -> RunMetadata {
    RunMetadata::new(
        &run_ref.run_id,
        &run_ref.workflow_type_id,
        &run_ref.config_id,
    )
}

/// Initialize the runs table for run metadata storage.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P16
/// @requirement:REQ-LF-FAIL-005
pub fn init_runs_table(conn: &rusqlite::Connection) -> Result<(), rusqlite::Error> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS runs (
            run_id TEXT PRIMARY KEY,
            workflow_type_id TEXT NOT NULL,
            config_id TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT,
            current_step TEXT
        )",
        [],
    )?;
    Ok(())
}
