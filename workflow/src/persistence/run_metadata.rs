/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
/// Run metadata persistence - structures for tracking workflow run state and identifiers.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::persistence::checkpoint::StateSnapshot;
use crate::workflow::schema::WorkflowRunRef;

/// Status of a workflow run.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunStatus {
    Initialized,
    Queued,
    Starting,
    Running,
    WaitingForChecks,
    WaitingExternal,
    ReadyToResume,
    Remediating,
    Blocked,
    Paused,
    Completed,
    Failed,
    Abandoned,
    Merged,
    Cancelled,
}

impl std::fmt::Display for RunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            RunStatus::Initialized => "initialized",
            RunStatus::Queued => "queued",
            RunStatus::Starting => "starting",
            RunStatus::Running => "running",
            RunStatus::WaitingForChecks => "waiting_for_checks",
            RunStatus::WaitingExternal => "waiting_external",
            RunStatus::ReadyToResume => "ready_to_resume",
            RunStatus::Remediating => "remediating",
            RunStatus::Blocked => "blocked",
            RunStatus::Paused => "paused",
            RunStatus::Completed => "completed",
            RunStatus::Failed => "failed",
            RunStatus::Abandoned => "abandoned",
            RunStatus::Merged => "merged",
            RunStatus::Cancelled => "cancelled",
        };
        write!(f, "{}", s)
    }
}

/// Error returned when persisted text is not a recognized [`RunStatus`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Unknown run status: {0}")]
pub struct RunStatusParseError(String);

impl std::str::FromStr for RunStatus {
    type Err = RunStatusParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "initialized" => Ok(RunStatus::Initialized),
            "queued" => Ok(RunStatus::Queued),
            "starting" => Ok(RunStatus::Starting),
            "running" => Ok(RunStatus::Running),
            "waiting_for_checks" => Ok(RunStatus::WaitingForChecks),
            "waiting_external" => Ok(RunStatus::WaitingExternal),
            "ready_to_resume" => Ok(RunStatus::ReadyToResume),
            "remediating" => Ok(RunStatus::Remediating),
            "blocked" => Ok(RunStatus::Blocked),
            "paused" => Ok(RunStatus::Paused),
            "completed" => Ok(RunStatus::Completed),
            "failed" => Ok(RunStatus::Failed),
            "abandoned" => Ok(RunStatus::Abandoned),
            "merged" => Ok(RunStatus::Merged),
            "cancelled" => Ok(RunStatus::Cancelled),
            _ => Err(RunStatusParseError(s.to_string())),
        }
    }
}

impl RunStatus {
    /// SQL string values for all terminal statuses, sourced from the same
    /// set as [`RunStatus::is_terminal`]. Used in conditional `UPDATE … WHERE
    /// status NOT IN (…)` clauses so the SQL guard and the Rust method can
    /// never disagree about which statuses are terminal.
    pub const TERMINAL_SQL: [&str; 5] = ["completed", "failed", "abandoned", "merged", "cancelled"];

    /// Returns true when the status represents a terminal run state.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            RunStatus::Completed
                | RunStatus::Failed
                | RunStatus::Abandoned
                | RunStatus::Merged
                | RunStatus::Cancelled
        )
    }

    /// Returns true when the run can be reopened/continued by an operator.
    ///
    /// Non-terminal waiting/paused/blocked runs are always resumable. A
    /// terminal `Failed` run is resumable only via explicit operator
    /// continuation (resume/retry/rewind); other terminal states
    /// (Completed/Merged/Abandoned/Cancelled) are not.
    /// @plan:PLAN-20260623-LUTHER-CONTINUATION
    pub fn is_resumable(&self) -> bool {
        matches!(
            self,
            RunStatus::WaitingForChecks
                | RunStatus::WaitingExternal
                | RunStatus::ReadyToResume
                | RunStatus::Paused
                | RunStatus::Blocked
                | RunStatus::Failed
        )
    }
}

/// Durable provenance for a failed work step followed by successful cleanup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureCleanupState {
    pub schema_version: u32,
    pub failed_step: String,
    pub failure_outcome: String,
    pub failure_reason: String,
    pub failed_checkpoint_id: String,
    #[serde(default)]
    pub failed_state_snapshot: StateSnapshot,
    pub cleanup_step: String,
    pub cleanup_succeeded: bool,
    pub captured_at: DateTime<Utc>,
    pub cleanup_completed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub recovery_consumed_at: Option<DateTime<Utc>>,
}

impl FailureCleanupState {
    pub const SCHEMA_VERSION: u32 = 1;

    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.schema_version == Self::SCHEMA_VERSION
            && !self.failed_step.is_empty()
            && !self.failure_outcome.is_empty()
            && !self.failure_reason.is_empty()
            && !self.failed_checkpoint_id.is_empty()
            && !self.cleanup_step.is_empty()
            && self.cleanup_succeeded
            && self.cleanup_completed_at.is_some()
    }

    #[must_use]
    pub fn recovery_is_available(&self) -> bool {
        self.is_complete() && self.recovery_consumed_at.is_none()
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
    /// The previous step that ran (optional).
    pub previous_step: Option<String>,
    /// The outcome of the previous step (optional).
    pub previous_outcome: Option<String>,
    /// Candidate next steps when determinable (JSON array TEXT).
    pub next_step_candidates: Vec<String>,
    /// Path to the run log file (optional).
    pub log_path: Option<String>,
    /// Root directory for run artifacts (optional).
    pub artifact_root: Option<String>,
    /// Workspace path for the run (optional).
    pub workspace_path: Option<String>,
    /// GitHub repository reference (optional).
    pub repository: Option<String>,
    /// GitHub issue number (optional).
    pub issue_number: Option<i64>,
    /// GitHub PR number (optional).
    pub pr_number: Option<i64>,
    /// Head SHA of the PR/branch (optional).
    pub head_sha: Option<String>,
    /// PID of the workflow process (optional).
    pub process_pid: Option<u32>,
    /// PIDs of child/agent processes (JSON array TEXT).
    pub child_pids: Vec<u32>,
    /// Original failed-work provenance when a failure-cleanup terminal ran.
    pub failure_cleanup: Option<FailureCleanupState>,
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
            previous_step: None,
            previous_outcome: None,
            next_step_candidates: Vec::new(),
            log_path: None,
            artifact_root: None,
            workspace_path: None,
            repository: None,
            issue_number: None,
            pr_number: None,
            head_sha: None,
            process_pid: None,
            child_pids: Vec::new(),
            failure_cleanup: None,
        }
    }

    /// True only for an explicitly evidenced cleanup-after-failure terminal.
    #[must_use]
    pub fn is_cleanup_failure_abandonment(&self) -> bool {
        self.status == RunStatus::Abandoned
            && self
                .failure_cleanup
                .as_ref()
                .is_some_and(FailureCleanupState::is_complete)
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

    /// Reopen a (possibly terminal) run for operator continuation.
    ///
    /// Flips the status back to `Running` and refreshes `updated_at` and the
    /// owning process PID so monitor/`runs show` reflect the reopen. Prior
    /// history (events, previous step/outcome) is intentionally preserved.
    /// @plan:PLAN-20260623-LUTHER-CONTINUATION
    pub fn reopen(&mut self) {
        self.status = RunStatus::Running;
        self.process_pid = Some(std::process::id());
        self.updated_at = Some(Utc::now());
    }

    /// Update the current step.
    pub fn set_current_step(&mut self, step: impl Into<String>) {
        self.current_step = Some(step.into());
        self.updated_at = Some(Utc::now());
    }

    /// Record the previous step and its outcome.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    pub fn set_previous_step_and_outcome(
        &mut self,
        step: impl Into<String>,
        outcome: impl Into<String>,
    ) {
        self.previous_step = Some(step.into());
        self.previous_outcome = Some(outcome.into());
        self.updated_at = Some(Utc::now());
    }

    /// Record the candidate next steps.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    pub fn set_next_step_candidates(&mut self, candidates: Vec<String>) {
        self.next_step_candidates = candidates;
        self.updated_at = Some(Utc::now());
    }

    /// Add a child/agent process PID.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    pub fn add_child_pid(&mut self, pid: u32) {
        if !self.child_pids.contains(&pid) {
            self.child_pids.push(pid);
        }
        self.updated_at = Some(Utc::now());
    }

    /// Clear all recorded child PIDs.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    pub fn clear_child_pids(&mut self) {
        self.child_pids.clear();
        self.updated_at = Some(Utc::now());
    }

    /// Whether the workflow process PID is stale (no longer alive).
    /// Returns false when no PID is recorded.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    pub fn is_process_stale(&self) -> bool {
        match self.process_pid {
            Some(pid) => is_pid_stale(pid),
            None => false,
        }
    }

    /// Returns the list of child PIDs that are stale (no longer alive).
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    pub fn are_child_pids_stale(&self) -> Vec<u32> {
        self.child_pids
            .iter()
            .copied()
            .filter(|pid| is_pid_stale(*pid))
            .collect()
    }
}

/// Determine whether a PID is stale (the process is no longer alive).
/// Portable across Linux and macOS via `kill(pid, 0)`.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub fn is_pid_stale(pid: u32) -> bool {
    !is_pid_alive(pid)
}

/// Portable liveness check for a PID using `kill(pid, 0)` on unix.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
#[cfg(unix)]
#[allow(unsafe_code)]
fn is_pid_alive(pid: u32) -> bool {
    // SAFETY: kill with signal 0 performs error checking without sending a signal.
    let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if ret == 0 {
        return true;
    }
    // EPERM means the process exists but we lack permission to signal it.
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Fallback liveness check on non-unix platforms (assume alive).
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
#[cfg(not(unix))]
fn is_pid_alive(_pid: u32) -> bool {
    true
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

/// Serialize a list of strings to a JSON array string.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub fn serialize_string_list(list: &[String]) -> String {
    serde_json::to_string(list).unwrap_or_else(|_| "[]".to_string())
}

/// Deserialize a JSON array string into a list of strings.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub fn deserialize_string_list(raw: Option<String>) -> Vec<String> {
    raw.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Serialize a list of PIDs to a JSON array string.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub fn serialize_pid_list(list: &[u32]) -> String {
    serde_json::to_string(list).unwrap_or_else(|_| "[]".to_string())
}

/// Deserialize a JSON array string into a list of PIDs.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub fn deserialize_pid_list(raw: Option<String>) -> Vec<u32> {
    raw.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
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
            current_step TEXT,
            previous_step TEXT,
            previous_outcome TEXT,
            next_step_candidates TEXT,
            log_path TEXT,
            artifact_root TEXT,
            workspace_path TEXT,
            repository TEXT,
            issue_number INTEGER,
            pr_number INTEGER,
            head_sha TEXT,
            process_pid INTEGER,
            child_pids TEXT,
            failure_cleanup TEXT
        )",
        [],
    )?;
    migrate_runs_table(conn)?;
    Ok(())
}

/// Idempotently add new columns to a pre-existing `runs` table.
/// Existing columns are discovered before DDL so real migration failures are
/// propagated rather than mistaken for harmless duplicate-column errors.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub fn migrate_runs_table(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    let existing = {
        let mut statement = conn.prepare("PRAGMA table_info(runs)")?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<std::collections::HashSet<_>>>()?;
        columns
    };
    let columns = [
        "previous_step TEXT",
        "previous_outcome TEXT",
        "next_step_candidates TEXT",
        "log_path TEXT",
        "artifact_root TEXT",
        "workspace_path TEXT",
        "repository TEXT",
        "issue_number INTEGER",
        "pr_number INTEGER",
        "head_sha TEXT",
        "process_pid INTEGER",
        "child_pids TEXT",
        "failure_cleanup TEXT",
    ];
    for column in columns {
        let name = column.split_whitespace().next().unwrap_or_default();
        if !existing.contains(name) {
            conn.execute(&format!("ALTER TABLE runs ADD COLUMN {column}"), [])?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn all_statuses_round_trip() {
        let statuses = [
            RunStatus::Initialized,
            RunStatus::Queued,
            RunStatus::Starting,
            RunStatus::Running,
            RunStatus::WaitingForChecks,
            RunStatus::WaitingExternal,
            RunStatus::ReadyToResume,
            RunStatus::Remediating,
            RunStatus::Blocked,
            RunStatus::Paused,
            RunStatus::Completed,
            RunStatus::Failed,
            RunStatus::Abandoned,
            RunStatus::Merged,
            RunStatus::Cancelled,
        ];
        for status in statuses {
            let s = status.to_string();
            let parsed = RunStatus::from_str(&s).expect("should parse");
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn terminal_classification() {
        assert!(RunStatus::Completed.is_terminal());
        assert!(RunStatus::Failed.is_terminal());
        assert!(RunStatus::Abandoned.is_terminal());
        assert!(RunStatus::Merged.is_terminal());
        assert!(RunStatus::Cancelled.is_terminal());
        assert!(!RunStatus::Running.is_terminal());
        assert!(!RunStatus::Starting.is_terminal());
        assert!(!RunStatus::WaitingForChecks.is_terminal());
        assert!(!RunStatus::WaitingExternal.is_terminal());
        assert!(!RunStatus::ReadyToResume.is_terminal());
    }

    #[test]
    fn resumable_classification() {
        // @plan:PLAN-20260623-LUTHER-CONTINUATION
        assert!(RunStatus::WaitingForChecks.is_resumable());
        assert!(RunStatus::WaitingExternal.is_resumable());
        assert!(RunStatus::ReadyToResume.is_resumable());
        assert!(RunStatus::Paused.is_resumable());
        assert!(RunStatus::Blocked.is_resumable());
        // Terminal Failed is resumable only via explicit continuation.
        assert!(RunStatus::Failed.is_resumable());
        // Other terminal states are not resumable.
        assert!(!RunStatus::Completed.is_resumable());
        assert!(!RunStatus::Merged.is_resumable());
        assert!(!RunStatus::Abandoned.is_resumable());
        assert!(!RunStatus::Cancelled.is_resumable());
    }

    #[test]
    fn reopen_flips_failed_run_to_running() {
        // @plan:PLAN-20260623-LUTHER-CONTINUATION
        let mut md = RunMetadata::new("r", "wf", "cfg");
        md.mark_failed();
        md.set_previous_step_and_outcome("watch_pr_checks", "wait");
        assert_eq!(md.status, RunStatus::Failed);

        md.reopen();
        assert_eq!(md.status, RunStatus::Running);
        assert_eq!(md.process_pid, Some(std::process::id()));
        // History preserved.
        assert_eq!(md.previous_step.as_deref(), Some("watch_pr_checks"));
        assert_eq!(md.previous_outcome.as_deref(), Some("wait"));
    }

    #[test]
    fn pid_staleness_for_current_process() {
        let pid = std::process::id();
        assert!(!is_pid_stale(pid), "current process must not be stale");
    }

    #[test]
    fn pid_staleness_for_dead_process() {
        // PID 0 is not a normal user process; treat as stale on unix.
        // Use a very large unlikely PID instead for portability.
        let dead_pid = 4_000_000_000u32;
        assert!(is_pid_stale(dead_pid), "unlikely PID should be stale");
    }

    #[test]
    fn child_pid_helpers() {
        let mut md = RunMetadata::new("r", "wf", "cfg");
        md.add_child_pid(std::process::id());
        md.add_child_pid(4_000_000_000);
        md.add_child_pid(std::process::id()); // duplicate ignored
        assert_eq!(md.child_pids.len(), 2);
        let stale = md.are_child_pids_stale();
        assert_eq!(stale, vec![4_000_000_000u32]);
        md.clear_child_pids();
        assert!(md.child_pids.is_empty());
    }

    #[test]
    fn string_list_round_trip() {
        let list = vec!["a".to_string(), "b".to_string()];
        let raw = serialize_string_list(&list);
        let back = deserialize_string_list(Some(raw));
        assert_eq!(back, list);
        assert!(deserialize_string_list(None).is_empty());
    }

    #[test]
    fn pid_list_round_trip() {
        let list = vec![1u32, 2u32, 3u32];
        let raw = serialize_pid_list(&list);
        let back = deserialize_pid_list(Some(raw));
        assert_eq!(back, list);
        assert!(deserialize_pid_list(None).is_empty());
    }
}
