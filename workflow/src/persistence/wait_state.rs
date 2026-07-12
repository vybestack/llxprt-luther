use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Result as SqliteResult};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::persistence::checkpoint::{
    set_resume_point, PersistenceError as CheckpointPersistenceError,
};
use crate::persistence::leases::{update_lease_status_conditional, LeaseStatus};
use crate::persistence::run_metadata::RunStatus;
use crate::persistence::sqlite::{
    persist_run_status_conditional_outcome_in_transaction, ConditionalStatusOutcome,
};

/// Typed failure from validating or writing a wait-state record.
#[derive(Debug, Error)]
pub enum WaitStateWriteError {
    /// A wait-state without a suspension generation cannot participate in
    /// guarded polling and must never be persisted.
    #[error("wait state for run {run_id} has an empty suspension_id")]
    EmptySuspensionId { run_id: String },
    /// Underlying SQLite error from writing the wait-state.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// Domain error for the external-wait persistence lifecycle.
///
/// Replaces the earlier practice of overloading `rusqlite::Error` variants
/// (e.g. `ToSqlConversionFailure`, `QueryReturnedNoRows`) for domain-level
/// conditions that are not SQL parameter-conversion or row-count failures.
/// Each variant preserves the original error source so callers can downcast,
/// pattern-match, or log the underlying cause without information loss.
#[derive(Debug, Error)]
pub enum ExternalWaitError {
    /// The run metadata record backing the external-wait transition is
    /// missing — an integrity failure. The wait-state exists without a
    /// backing run.
    #[error("run metadata for run {0} is missing — integrity failure")]
    RunMissing(String),
    /// The run is already in a terminal state and must not be resurrected
    /// back to `WaitingExternal`. The current status is preserved in the
    /// error so callers can decide how to compensate.
    #[error(
        "run {run_id} is already terminal ({current}); refusing to resurrect to WaitingExternal"
    )]
    RunAlreadyTerminal { run_id: String, current: RunStatus },
    /// The conditional lease update matched zero rows because the lease
    /// advanced to an unexpected state (or was concurrently reclaimed by a
    /// new run). The transaction is rolled back by the caller.
    #[error("lease transition rejected for run {run_id}: the lease is no longer in the expected state or is owned by another run")]
    LeaseTransitionRejected { run_id: String },
    /// The canonical wait record is missing identity required by the atomic
    /// run/checkpoint/lease transition.
    #[error("external wait for run {run_id} has incomplete identity: {field}")]
    IdentityIncomplete { run_id: String, field: &'static str },
    /// Wait-state validation or persistence failure.
    #[error(transparent)]
    WaitState(#[from] WaitStateWriteError),
    /// Checkpoint-layer persistence error (serialization, I/O, or database
    /// failure from the checkpoint subsystem). The original error is
    /// preserved for source-chain inspection.
    #[error(transparent)]
    Checkpoint(#[from] CheckpointPersistenceError),
    /// Underlying SQLite error from the transaction.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitKind {
    PrChecks,
    CoderabbitReview,
    HumanReview,
    PrMerge,
    RateLimitBackoff,
    DependencyChildWorkflow,
    DependencyChildMerge,
}

impl std::fmt::Display for WaitKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            WaitKind::PrChecks => "pr_checks",
            WaitKind::CoderabbitReview => "coderabbit_review",
            WaitKind::HumanReview => "human_review",
            WaitKind::PrMerge => "pr_merge",
            WaitKind::RateLimitBackoff => "rate_limit_backoff",
            WaitKind::DependencyChildWorkflow => "dependency_child_workflow",
            WaitKind::DependencyChildMerge => "dependency_child_merge",
        };
        write!(f, "{s}")
    }
}

impl std::str::FromStr for WaitKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pr_checks" => Ok(WaitKind::PrChecks),
            "coderabbit_review" => Ok(WaitKind::CoderabbitReview),
            "human_review" => Ok(WaitKind::HumanReview),
            "pr_merge" => Ok(WaitKind::PrMerge),
            "rate_limit_backoff" => Ok(WaitKind::RateLimitBackoff),
            "dependency_child_workflow" => Ok(WaitKind::DependencyChildWorkflow),
            "dependency_child_merge" => Ok(WaitKind::DependencyChildMerge),
            _ => Err(format!("Unknown wait kind: {s}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaitStateRecord {
    pub run_id: String,
    #[serde(default = "new_suspension_id")]
    pub suspension_id: String,
    pub lease_id: Option<String>,
    pub workflow_type: String,
    pub config_id: String,
    pub repository: String,
    pub issue_number: u64,
    pub pr_number: Option<u64>,
    pub head_sha: Option<String>,
    pub wait_kind: WaitKind,
    pub wait_condition: serde_json::Value,
    pub last_observed_state: serde_json::Value,
    pub next_poll_at: DateTime<Utc>,
    pub poll_interval_seconds: u64,
    pub max_wait_seconds: Option<u64>,
    pub resume_step: String,
    pub checkpoint_id: String,
    pub poll_count: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn new_suspension_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

impl WaitStateRecord {
    #[must_use]
    pub fn new(run_id: impl Into<String>, config_id: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            run_id: run_id.into(),
            suspension_id: new_suspension_id(),
            lease_id: None,
            workflow_type: String::new(),
            config_id: config_id.into(),
            repository: String::new(),
            issue_number: 0,
            pr_number: None,
            head_sha: None,
            wait_kind: WaitKind::PrChecks,
            wait_condition: serde_json::Value::Null,
            last_observed_state: serde_json::Value::Null,
            next_poll_at: now,
            poll_interval_seconds: 300,
            max_wait_seconds: None,
            resume_step: String::new(),
            checkpoint_id: String::new(),
            poll_count: 0,
            created_at: now,
            updated_at: now,
        }
    }
}

pub fn init_wait_states_table(conn: &Connection) -> SqliteResult<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS wait_states (
            run_id TEXT PRIMARY KEY,
            suspension_id TEXT NOT NULL DEFAULT '',
            lease_id TEXT,
            workflow_type TEXT NOT NULL,
            config_id TEXT NOT NULL,
            repository TEXT NOT NULL,
            issue_number INTEGER NOT NULL,
            pr_number INTEGER,
            head_sha TEXT,
            wait_kind TEXT NOT NULL,
            wait_condition_json TEXT NOT NULL,
            last_observed_state_json TEXT NOT NULL,
            next_poll_at TEXT NOT NULL,
            poll_interval_seconds INTEGER NOT NULL,
            max_wait_seconds INTEGER,
            resume_step TEXT NOT NULL,
            checkpoint_id TEXT NOT NULL,
            poll_count INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )",
        [],
    )?;
    if !wait_states_has_suspension_id(conn)? {
        match conn.execute(
            "ALTER TABLE wait_states ADD COLUMN suspension_id TEXT NOT NULL DEFAULT ''",
            [],
        ) {
            Ok(_) => {}
            Err(error) if is_duplicate_suspension_id_column(&error) => {
                // Another initializer may have completed the same migration
                // while this connection waited on SQLite's schema lock. Only
                // accept that exact idempotent outcome after verifying schema.
                if !wait_states_has_suspension_id(conn)? {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
    }
    // Legacy rows receive a stable token once during migration. The default
    // keeps older database writers compatible, while initialized databases
    // never expose an empty generation to pollers.
    conn.execute(
        "UPDATE wait_states
         SET suspension_id = lower(hex(randomblob(16)))
         WHERE suspension_id = ''",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_wait_states_pollable
            ON wait_states (next_poll_at, config_id, repository)",
        [],
    )?;
    Ok(())
}

fn wait_states_has_suspension_id(conn: &Connection) -> SqliteResult<bool> {
    let mut stmt = conn.prepare("PRAGMA table_info(wait_states)")?;
    let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
    Ok(columns
        .collect::<SqliteResult<Vec<_>>>()?
        .iter()
        .any(|column| column == "suspension_id"))
}

fn is_duplicate_suspension_id_column(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(_, Some(message))
            if message.eq_ignore_ascii_case("duplicate column name: suspension_id")
    )
}

pub fn upsert_wait_state(
    conn: &Connection,
    record: &WaitStateRecord,
) -> Result<(), WaitStateWriteError> {
    if record.suspension_id.is_empty() {
        return Err(WaitStateWriteError::EmptySuspensionId {
            run_id: record.run_id.clone(),
        });
    }
    conn.execute(
        "INSERT INTO wait_states
            (run_id, suspension_id, lease_id, workflow_type, config_id, repository, issue_number,
             pr_number, head_sha, wait_kind, wait_condition_json,
             last_observed_state_json, next_poll_at, poll_interval_seconds,
             max_wait_seconds, resume_step, checkpoint_id, poll_count, created_at,
             updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                 ?15, ?16, ?17, ?18, ?19, ?20)
         ON CONFLICT(run_id) DO UPDATE SET
             suspension_id = excluded.suspension_id,
             lease_id = excluded.lease_id,
             workflow_type = excluded.workflow_type,
             config_id = excluded.config_id,
             repository = excluded.repository,
             issue_number = excluded.issue_number,
             pr_number = excluded.pr_number,
             head_sha = excluded.head_sha,
             wait_kind = excluded.wait_kind,
             wait_condition_json = excluded.wait_condition_json,
             last_observed_state_json = excluded.last_observed_state_json,
             next_poll_at = excluded.next_poll_at,
             poll_interval_seconds = excluded.poll_interval_seconds,
             max_wait_seconds = excluded.max_wait_seconds,
             resume_step = excluded.resume_step,
             checkpoint_id = excluded.checkpoint_id,
             poll_count = excluded.poll_count,
             updated_at = excluded.updated_at",
        record_params(record)?,
    )?;
    Ok(())
}

/// Atomically establish a complete external-wait state from one canonical record.
///
/// The record supplies the run, lease, resume, and suspension identities used
/// by every write. Required identities are validated before the transaction,
/// preventing divergent run/lease/checkpoint state.
pub fn persist_external_wait(
    conn: &Connection,
    record: &WaitStateRecord,
) -> Result<(), ExternalWaitError> {
    let lease_id = record
        .lease_id
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ExternalWaitError::IdentityIncomplete {
            run_id: record.run_id.clone(),
            field: "lease_id",
        })?;
    if record.run_id.is_empty() {
        return Err(ExternalWaitError::IdentityIncomplete {
            run_id: record.run_id.clone(),
            field: "run_id",
        });
    }
    if record.resume_step.is_empty() {
        return Err(ExternalWaitError::IdentityIncomplete {
            run_id: record.run_id.clone(),
            field: "resume_step",
        });
    }
    if record.suspension_id.is_empty() {
        return Err(ExternalWaitError::IdentityIncomplete {
            run_id: record.run_id.clone(),
            field: "suspension_id",
        });
    }

    let tx = conn.unchecked_transaction()?;
    set_resume_point(&tx, &record.run_id, &record.resume_step)?;
    mark_run_waiting_external(&tx, &record.run_id, &record.resume_step)?;
    upsert_wait_state(&tx, record)?;

    let applied = update_lease_status_conditional(
        &tx,
        lease_id,
        LeaseStatus::WaitingExternal,
        &[LeaseStatus::Running, LeaseStatus::WaitingExternal],
        None,
        Some(&record.run_id),
    )?;
    if !applied {
        return Err(ExternalWaitError::LeaseTransitionRejected {
            run_id: record.run_id.clone(),
        });
    }

    tx.commit()?;
    Ok(())
}

/// Whether a complete, pollable external wait exists for `run_id`.
///
/// "Complete" means: the run status is `WaitingExternal`, a wait-states row
/// exists with a non-empty `lease_id` and `resume_step`, and that lease is in
/// `WaitingExternal` status. This is the single check the launcher uses to
/// decide whether to keep a lease waiting after an error.
///
pub fn has_pollable_external_wait(conn: &Connection, run_id: &str) -> SqliteResult<bool> {
    let exists = conn.query_row(
        "SELECT EXISTS (
             SELECT 1
             FROM runs
             JOIN wait_states ON wait_states.run_id = runs.run_id
             JOIN issue_leases
               ON issue_leases.lease_id = wait_states.lease_id
              AND issue_leases.run_id = wait_states.run_id
             WHERE runs.run_id = ?1
               AND runs.status = ?2
               AND wait_states.lease_id IS NOT NULL
               AND wait_states.lease_id <> ''
               AND wait_states.resume_step <> ''
               AND wait_states.suspension_id <> ''
               AND issue_leases.status = ?3
         )",
        params![
            run_id,
            RunStatus::WaitingExternal.to_string(),
            LeaseStatus::WaitingExternal.as_str(),
        ],
        |row| row.get(0),
    )?;
    Ok(exists)
}

pub fn get_wait_state(conn: &Connection, run_id: &str) -> SqliteResult<Option<WaitStateRecord>> {
    conn.query_row(
        &format!("{SELECT_COLUMNS} WHERE run_id = ?1"),
        params![run_id],
        row_to_wait_state,
    )
    .optional()
}

pub fn list_wait_states(conn: &Connection) -> SqliteResult<Vec<WaitStateRecord>> {
    let mut stmt = conn.prepare(&format!(
        "{SELECT_COLUMNS} ORDER BY next_poll_at, repository, issue_number"
    ))?;
    collect_wait_states(&mut stmt, [])
}

/// List wait-state records whose external wait is due for polling.
///
/// A record is pollable only when **all** of the following hold:
///
/// - `next_poll_at` is at or before `now`.
/// - The associated issue-lease is in `waiting_external` status. This is the
///   single status the poller owns; `ready_to_resume` is handled by the resume
///   path and `claimed`/`running` are in-flight launches that have not yet
///   suspended.
/// - The lease's `run_id` matches the wait-state's `run_id`. Without this, a
///   reclaimed lease (whose run was superseded) could be polled by the wrong
///   run, corrupting another run's wait-state.
///
/// Restricting the source to `WaitingExternal` prevents the poller from
/// re-reading records whose decision was already applied (e.g. a
/// `ready_to_resume` record concurrently transitioned by the resume path) and
/// from interfering with runs that are still launching.
pub fn list_pollable_wait_states(
    conn: &Connection,
    now: DateTime<Utc>,
) -> SqliteResult<Vec<WaitStateRecord>> {
    let waiting_status = LeaseStatus::WaitingExternal.as_str();
    let mut stmt = conn.prepare(&format!(
        "{SELECT_COLUMNS}
         WHERE next_poll_at <= ?1
           AND suspension_id <> ''
           AND EXISTS (
               SELECT 1 FROM issue_leases
               WHERE issue_leases.lease_id = wait_states.lease_id
                 AND issue_leases.status = ?2
                 AND issue_leases.run_id = wait_states.run_id
           )
         ORDER BY next_poll_at, repository, issue_number"
    ))?;
    collect_wait_states(&mut stmt, params![now.to_rfc3339(), waiting_status])
}

/// Refresh a wait-state row after a poll, guarded by the immutable
/// suspension generation and an optimistic poll-count version.
///
/// `expected_suspension_id` identifies the suspension read by the poller and
/// prevents an ABA match when a resumed run creates a replacement row whose
/// poll count resets. `expected_poll_count` prevents two pollers for the same
/// suspension from overwriting each other. A mismatch returns `Ok(false)`.
pub fn update_wait_state_after_poll(
    conn: &Connection,
    run_id: &str,
    last_observed_state: &serde_json::Value,
    next_poll_at: DateTime<Utc>,
    expected_poll_count: u64,
    expected_suspension_id: &str,
) -> SqliteResult<bool> {
    let rows = conn.execute(
        "UPDATE wait_states
         SET last_observed_state_json = ?1,
             next_poll_at = ?2,
             poll_count = poll_count + 1,
             updated_at = ?3
         WHERE run_id = ?4 AND poll_count = ?5 AND suspension_id = ?6",
        params![
            last_observed_state.to_string(),
            next_poll_at.to_rfc3339(),
            Utc::now().to_rfc3339(),
            run_id,
            to_sql_i64(expected_poll_count)?,
            expected_suspension_id,
        ],
    )?;
    Ok(rows > 0)
}

pub fn delete_wait_state(conn: &Connection, run_id: &str) -> SqliteResult<bool> {
    let deleted = conn.execute("DELETE FROM wait_states WHERE run_id = ?1", params![run_id])?;
    Ok(deleted > 0)
}

pub fn delete_wait_state_for_suspension(
    conn: &Connection,
    run_id: &str,
    suspension_id: &str,
) -> SqliteResult<bool> {
    let deleted = conn.execute(
        "DELETE FROM wait_states WHERE run_id = ?1 AND suspension_id = ?2",
        params![run_id, suspension_id],
    )?;
    Ok(deleted > 0)
}

const SELECT_COLUMNS: &str =
    "SELECT run_id, suspension_id, lease_id, workflow_type, config_id, repository, issue_number, \
     pr_number, head_sha, wait_kind, wait_condition_json, last_observed_state_json, \
     next_poll_at, poll_interval_seconds, max_wait_seconds, resume_step, \
     checkpoint_id, poll_count, created_at, updated_at FROM wait_states";

fn record_params(record: &WaitStateRecord) -> SqliteResult<[Box<dyn rusqlite::ToSql>; 20]> {
    Ok([
        Box::new(record.run_id.clone()),
        Box::new(record.suspension_id.clone()),
        Box::new(record.lease_id.clone()),
        Box::new(record.workflow_type.clone()),
        Box::new(record.config_id.clone()),
        Box::new(record.repository.clone()),
        Box::new(to_sql_i64(record.issue_number)?),
        Box::new(record.pr_number.map(to_sql_i64).transpose()?),
        Box::new(record.head_sha.clone()),
        Box::new(record.wait_kind.to_string()),
        Box::new(record.wait_condition.to_string()),
        Box::new(record.last_observed_state.to_string()),
        Box::new(record.next_poll_at.to_rfc3339()),
        Box::new(to_sql_i64(record.poll_interval_seconds)?),
        Box::new(record.max_wait_seconds.map(to_sql_i64).transpose()?),
        Box::new(record.resume_step.clone()),
        Box::new(record.checkpoint_id.clone()),
        Box::new(to_sql_i64(record.poll_count)?),
        Box::new(record.created_at.to_rfc3339()),
        Box::new(record.updated_at.to_rfc3339()),
    ])
}

fn to_sql_i64(value: u64) -> SqliteResult<i64> {
    i64::try_from(value).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
}

fn collect_wait_states<P>(
    stmt: &mut rusqlite::Statement<'_>,
    params: P,
) -> SqliteResult<Vec<WaitStateRecord>>
where
    P: rusqlite::Params,
{
    let rows = stmt.query_map(params, row_to_wait_state)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn row_to_wait_state(row: &rusqlite::Row<'_>) -> SqliteResult<WaitStateRecord> {
    Ok(WaitStateRecord {
        run_id: row.get(0)?,
        suspension_id: row.get(1)?,
        lease_id: row.get(2)?,
        workflow_type: row.get(3)?,
        config_id: row.get(4)?,
        repository: row.get(5)?,
        issue_number: row_u64(row, 6)?,
        pr_number: optional_row_u64(row, 7)?,
        head_sha: row.get(8)?,
        wait_kind: row_wait_kind(row, 9)?,
        wait_condition: row_json(row, 10)?,
        last_observed_state: row_json(row, 11)?,
        next_poll_at: row_ts(row, 12)?,
        poll_interval_seconds: row_u64(row, 13)?,
        max_wait_seconds: optional_row_u64(row, 14)?,
        resume_step: row.get(15)?,
        checkpoint_id: row.get(16)?,
        poll_count: row_u64(row, 17)?,
        created_at: row_ts(row, 18)?,
        updated_at: row_ts(row, 19)?,
    })
}

fn row_u64(row: &rusqlite::Row<'_>, col: usize) -> SqliteResult<u64> {
    nonnegative_i64_to_u64(row.get(col)?, col)
}

fn optional_row_u64(row: &rusqlite::Row<'_>, col: usize) -> SqliteResult<Option<u64>> {
    optional_nonnegative_i64_to_u64(row.get(col)?, col)
}

fn row_wait_kind(row: &rusqlite::Row<'_>, col: usize) -> SqliteResult<WaitKind> {
    parse_wait_kind(&row.get::<_, String>(col)?, col)
}

fn row_json(row: &rusqlite::Row<'_>, col: usize) -> SqliteResult<serde_json::Value> {
    parse_json(&row.get::<_, String>(col)?, col)
}

fn row_ts(row: &rusqlite::Row<'_>, col: usize) -> SqliteResult<DateTime<Utc>> {
    parse_ts(&row.get::<_, String>(col)?, col)
}

fn parse_wait_kind(s: &str, col: usize) -> SqliteResult<WaitKind> {
    s.parse::<WaitKind>().map_err(|e| {
        conversion_error(
            col,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(e)),
        )
    })
}

fn parse_json(s: &str, col: usize) -> SqliteResult<serde_json::Value> {
    serde_json::from_str(s)
        .map_err(|e| conversion_error(col, rusqlite::types::Type::Text, Box::new(e)))
}

fn parse_ts(s: &str, col: usize) -> SqliteResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| conversion_error(col, rusqlite::types::Type::Text, Box::new(e)))
}

fn nonnegative_i64_to_u64(value: i64, col: usize) -> SqliteResult<u64> {
    u64::try_from(value)
        .map_err(|e| conversion_error(col, rusqlite::types::Type::Integer, Box::new(e)))
}

fn optional_nonnegative_i64_to_u64(value: Option<i64>, col: usize) -> SqliteResult<Option<u64>> {
    value.map(|n| nonnegative_i64_to_u64(n, col)).transpose()
}

fn conversion_error(
    col: usize,
    col_type: rusqlite::types::Type,
    error: Box<dyn std::error::Error + Send + Sync>,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(col, col_type, error)
}

/// Mark a run's status as `WaitingExternal` with the given current step,
/// using the shared atomic conditional update
/// [`persist_run_status_conditional_outcome_in_transaction`] to eliminate the
/// check-then-act TOCTOU window.
///
/// Returns [`ExternalWaitError::RunMissing`] when the run metadata record is
/// absent (an integrity failure) and [`ExternalWaitError::RunAlreadyTerminal`]
/// when the run is `Completed`, `Failed`, `Abandoned`, `Merged`, or
/// `Cancelled` — a concurrent path already classified this run, and we must
/// not resurrect it back to `WaitingExternal` within the same transaction.
///
/// By delegating to the shared typed-outcome function, the terminal-status
/// placeholder SQL and its bound parameters are constructed compile-time-safely
/// via [`RunStatus::TERMINAL_SQL`] in exactly one place
/// ([`persist_run_status_conditional_with_conn`]), keeping the SQL shape and
/// parameter count synchronized without a parallel hardcoded construction.
fn mark_run_waiting_external(
    conn: &rusqlite::Transaction<'_>,
    run_id: &str,
    step_id: &str,
) -> Result<(), ExternalWaitError> {
    match persist_run_status_conditional_outcome_in_transaction(
        conn,
        run_id,
        &RunStatus::WaitingExternal,
        Some(step_id),
    )? {
        ConditionalStatusOutcome::Updated => Ok(()),
        ConditionalStatusOutcome::RunMissing => {
            Err(ExternalWaitError::RunMissing(run_id.to_string()))
        }
        ConditionalStatusOutcome::AlreadyTerminal(current) => {
            Err(ExternalWaitError::RunAlreadyTerminal {
                run_id: run_id.to_string(),
                current,
            })
        }
    }
}

#[cfg(test)]
#[path = "wait_state_tests.rs"]
mod tests;
