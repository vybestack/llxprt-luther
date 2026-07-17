/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
/// SQLite persistence layer for run metadata.
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, Result as SqliteResult};

use super::run_metadata::{
    deserialize_pid_list, deserialize_string_list, migrate_runs_table, serialize_pid_list,
    serialize_string_list, RunMetadata, RunStatus,
};

/// SQLite database connection wrapper for run persistence.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub struct SqliteStore {
    conn: Connection,
    db_path: Option<PathBuf>,
}

impl SqliteStore {
    /// Open or create a SQLite database at the given path.
    /// Initializes the schema if needed.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    pub fn open(db_path: impl AsRef<Path>) -> SqliteResult<Self> {
        let db_path = db_path.as_ref().to_path_buf();
        let conn = Connection::open(&db_path)?;
        let store = Self {
            conn,
            db_path: Some(db_path),
        };
        store.init_schema()?;
        Ok(store)
    }

    /// Create an in-memory SQLite store (for testing).
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    pub fn open_in_memory() -> SqliteResult<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self {
            conn,
            db_path: None,
        };
        store.init_schema()?;
        Ok(store)
    }

    /// Initialize the database schema.
    /// Creates the `runs` table with columns for all identifiers.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    /// @requirement:REQ-EARS-PERSIST-001,REQ-EARS-SCALE-002
    fn init_schema(&self) -> SqliteResult<()> {
        init_runs_schema_serialized(&self.conn)?;
        super::leases::init_leases_table(&self.conn)
    }

    /// Persist a new run metadata record.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    /// @requirement:REQ-EARS-PERSIST-001
    pub fn persist_run(&self, metadata: &RunMetadata) -> SqliteResult<()> {
        persist_run_with_conn(&self.conn, metadata)
    }

    /// Get run metadata by run_id.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    pub fn get_run(&self, run_id: &str) -> SqliteResult<Option<RunMetadata>> {
        get_run_with_conn(&self.conn, run_id)
    }

    /// List all runs in the database.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    pub fn list_runs(&self) -> SqliteResult<Vec<RunMetadata>> {
        list_runs_with_conn(&self.conn)
    }

    /// List selected runs by id.
    /// @plan:issue-117
    pub fn list_runs_by_ids(&self, run_ids: &[&str]) -> SqliteResult<Vec<RunMetadata>> {
        list_runs_by_ids_with_conn(&self.conn, run_ids)
    }

    /// List runs filtered by status.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    pub fn list_runs_by_status(&self, status: &RunStatus) -> SqliteResult<Vec<RunMetadata>> {
        Ok(self
            .list_runs()?
            .into_iter()
            .filter(|r| &r.status == status)
            .collect())
    }

    /// Get all non-terminal (active) runs.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    pub fn get_active_runs(&self) -> SqliteResult<Vec<RunMetadata>> {
        Ok(self
            .list_runs()?
            .into_iter()
            .filter(|r| !r.status.is_terminal())
            .collect())
    }

    /// Get all runs for a given repository.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    pub fn get_runs_for_repository(&self, repository: &str) -> SqliteResult<Vec<RunMetadata>> {
        Ok(self
            .list_runs()?
            .into_iter()
            .filter(|r| r.repository.as_deref() == Some(repository))
            .collect())
    }

    /// Update the status of a run.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    pub fn update_status(&self, run_id: &str, status: RunStatus) -> SqliteResult<()> {
        let updated_at = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE runs SET status = ?1, updated_at = ?2 WHERE run_id = ?3",
            params![status.to_string(), updated_at, run_id],
        )?;
        Ok(())
    }

    /// Get the underlying database connection (for advanced use).
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    #[must_use]
    pub fn db_path(&self) -> Option<&Path> {
        self.db_path.as_deref()
    }

    /// Create a store from an existing connection (for use with borrowed connections).
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
    pub fn from_connection_ref(conn: &Connection) -> SqliteStoreRef<'_> {
        SqliteStoreRef { conn }
    }
}

/// Reference-based SQLite store for use with borrowed connections.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P15
pub struct SqliteStoreRef<'a> {
    pub conn: &'a Connection,
}

impl<'a> SqliteStoreRef<'a> {
    /// Persist a new run metadata record.
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
    pub fn persist_run(&self, metadata: &RunMetadata) -> SqliteResult<()> {
        persist_run_with_conn(self.conn, metadata)
    }

    /// Get run metadata by run_id.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    pub fn get_run(&self, run_id: &str) -> SqliteResult<Option<RunMetadata>> {
        get_run_with_conn(self.conn, run_id)
    }

    /// List all runs in the database.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    pub fn list_runs(&self) -> SqliteResult<Vec<RunMetadata>> {
        list_runs_with_conn(self.conn)
    }

    /// List selected runs by id.
    /// @plan:issue-117
    pub fn list_runs_by_ids(&self, run_ids: &[&str]) -> SqliteResult<Vec<RunMetadata>> {
        list_runs_by_ids_with_conn(self.conn, run_ids)
    }
}

/// SQL column list shared by the runs SELECT statements (must match query order).
const RUN_SELECT_COLUMNS: &str =
    "run_id, workflow_type_id, config_id, status, created_at, updated_at, current_step, \
     previous_step, previous_outcome, next_step_candidates, log_path, artifact_root, \
     workspace_path, repository, issue_number, pr_number, head_sha, process_pid, child_pids, \
     failure_cleanup";

/// Initialize the runs schema on the given connection (table + migration).
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub fn init_runs_schema(conn: &Connection) -> SqliteResult<()> {
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

/// Initialize the runs schema while holding an immediate write transaction.
/// This serializes legacy-column inspection and DDL across concurrent openers.
pub fn init_runs_schema_serialized(conn: &Connection) -> SqliteResult<()> {
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    init_runs_schema(&tx)?;
    tx.commit()
}

/// Persist a run record (insert or update) using a borrowed connection.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub fn persist_run_with_conn(conn: &Connection, metadata: &RunMetadata) -> SqliteResult<()> {
    conn.execute(
        "INSERT INTO runs (run_id, workflow_type_id, config_id, status, created_at, updated_at, \
            current_step, previous_step, previous_outcome, next_step_candidates, log_path, \
            artifact_root, workspace_path, repository, issue_number, pr_number, head_sha, \
            process_pid, child_pids, failure_cleanup)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
         ON CONFLICT(run_id) DO UPDATE SET
            workflow_type_id = excluded.workflow_type_id,
            config_id = excluded.config_id,
            status = excluded.status,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            current_step = excluded.current_step,
            previous_step = excluded.previous_step,
            previous_outcome = excluded.previous_outcome,
            next_step_candidates = excluded.next_step_candidates,
            log_path = excluded.log_path,
            artifact_root = excluded.artifact_root,
            workspace_path = excluded.workspace_path,
            repository = excluded.repository,
            issue_number = excluded.issue_number,
            pr_number = excluded.pr_number,
            head_sha = excluded.head_sha,
            process_pid = excluded.process_pid,
            child_pids = excluded.child_pids,
            failure_cleanup = excluded.failure_cleanup",
        params![
            metadata.run_id,
            metadata.workflow_type_id,
            metadata.config_id,
            metadata.status.to_string(),
            metadata.created_at.to_rfc3339(),
            metadata.updated_at.map(|t| t.to_rfc3339()),
            metadata.current_step,
            metadata.previous_step,
            metadata.previous_outcome,
            serialize_string_list(&metadata.next_step_candidates),
            metadata.log_path,
            metadata.artifact_root,
            metadata.workspace_path,
            metadata.repository,
            metadata.issue_number,
            metadata.pr_number,
            metadata.head_sha,
            metadata.process_pid,
            serialize_pid_list(&metadata.child_pids),
            metadata
                .failure_cleanup
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?,
        ],
    )?;
    Ok(())
}

/// Typed outcome of a conditional status update, distinguishing the three
/// possible results atomically without a separate read-then-write.
///
/// This resolves the check-then-act TOCTOU window at the persistence
/// boundary: a single atomic `UPDATE … WHERE status NOT IN (terminals)`
/// either updates the row ([`Updated`](Self::Updated)), matches zero rows
/// because the run is already terminal
/// ([`AlreadyTerminal`](Self::AlreadyTerminal)), or matches zero rows because
/// the run does not exist ([`RunMissing`](Self::RunMissing)). The
/// missing-vs-terminal classification is determined by a single post-update
/// `SELECT` that runs *after* the atomic guard has already decided not to
/// write, so it cannot race with a concurrent terminal transition to produce a
/// false non-terminal classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConditionalStatusOutcome {
    /// The atomic conditional UPDATE matched and transitioned the row.
    Updated,
    /// The run exists but is already in a terminal status — the guard
    /// rejected the write. The current terminal status is preserved so
    /// callers can compensate or log it.
    AlreadyTerminal(RunStatus),
    /// The run metadata record does not exist at all — an integrity failure.
    RunMissing,
}

/// Conditionally update a run's status and current_step using an atomic
/// `UPDATE … WHERE run_id = ? AND status NOT IN (terminal set)` guard.
///
/// Returns `Ok(true)` when the row was updated and `Ok(false)` when no row
/// matched, either because the run is already terminal or because `run_id` is
/// missing. Call [`persist_run_status_conditional_outcome_with_conn`] when the
/// caller needs that distinction. The guard prevents the classic
/// read-modify-write race where two transactions read the same state and the
/// later write overwrites the earlier one.
///
/// When `current_step` is `Some`, the column is updated to the new value; when
/// `None`, the existing `current_step` is preserved (the column is never nulled
/// out by a status-only transition). This mirrors the `run_id` preservation in
/// [`crate::persistence::leases::update_lease_status_conditional`] via a single
/// atomic `CASE WHEN … THEN … ELSE … END` expression. The SQL is a static
/// constant — the terminal-status count is fixed at compile time
/// ([`RunStatus::TERMINAL_SQL`] has 5 entries), so the placeholder list is
/// constant and the query plan is cached by `prepare_cached`.
///
/// The guard mirrors [`RunStatus::is_terminal`] via [`RunStatus::TERMINAL_SQL`]
/// so the SQL set and the Rust predicate can never disagree.
pub(crate) fn persist_run_status_conditional_with_conn(
    conn: &Connection,
    run_id: &str,
    status: &RunStatus,
    current_step: Option<&str>,
) -> SqliteResult<bool> {
    let status_str = status.to_string();
    let now = chrono::Utc::now().to_rfc3339();
    // TERMINAL_SQL is a fixed-length compile-time constant (5 entries), so the
    // SQL shape never changes — use a static query string and prepare it once
    // per connection via prepare_cached to avoid repeated parse/planning.
    let mut stmt = conn.prepare_cached(CONDITIONAL_STATUS_UPDATE_SQL)?;
    let mut params: Vec<&dyn rusqlite::ToSql> =
        Vec::with_capacity(4 + RunStatus::TERMINAL_SQL.len());
    params.push(&status_str);
    params.push(&current_step);
    params.push(&now);
    params.push(&run_id);
    params.extend(
        RunStatus::TERMINAL_SQL
            .iter()
            .map(|t| t as &dyn rusqlite::ToSql),
    );
    let updated = stmt.execute(params.as_slice())?;
    Ok(updated > 0)
}

/// Atomically classify a conditional status update as
/// [`ConditionalStatusOutcome::Updated`], [`AlreadyTerminal`](ConditionalStatusOutcome::AlreadyTerminal),
/// or [`RunMissing`](ConditionalStatusOutcome::RunMissing).
///
/// This public entry point owns and commits the transaction that covers both
/// the guarded update and any status lookup needed to classify a zero-row
/// result. Crate-internal callers that already own a transaction use the
/// visibility-limited `persist_run_status_conditional_outcome_in_transaction`
/// helper so these operations remain part of their larger atomic unit.
pub fn persist_run_status_conditional_outcome_with_conn(
    conn: &Connection,
    run_id: &str,
    status: &RunStatus,
    current_step: Option<&str>,
) -> SqliteResult<ConditionalStatusOutcome> {
    let tx = conn.unchecked_transaction()?;
    let outcome =
        persist_run_status_conditional_outcome_in_transaction(&tx, run_id, status, current_step)?;
    tx.commit()?;
    Ok(outcome)
}

/// Crate-internal implementation for callers that already own a transaction.
///
/// Unlike [`persist_run_status_conditional_outcome_with_conn`], this helper
/// neither starts nor commits a transaction. The caller controls the complete
/// atomic unit. The update and targeted `SELECT status` execute in that same
/// transaction. In SQLite, the attempted `UPDATE` starts the write transaction
/// and holds its writer lock through this read, even when no row matched, so a
/// concurrent writer cannot change or delete the row before classification.
/// The guarded update can match zero rows only when the run is missing or its
/// status is terminal; the follow-up read classifies those two outcomes.
pub(crate) fn persist_run_status_conditional_outcome_in_transaction(
    conn: &rusqlite::Transaction<'_>,
    run_id: &str,
    status: &RunStatus,
    current_step: Option<&str>,
) -> SqliteResult<ConditionalStatusOutcome> {
    if persist_run_status_conditional_with_conn(conn, run_id, status, current_step)? {
        return Ok(ConditionalStatusOutcome::Updated);
    }

    match get_run_status_with_conn(conn, run_id)? {
        None => Ok(ConditionalStatusOutcome::RunMissing),
        Some(current) => Ok(ConditionalStatusOutcome::AlreadyTerminal(current)),
    }
}

/// Crate-internal outcome for a run transition guarded by explicit source
/// statuses.
pub(crate) enum ExpectedRunStatusOutcome {
    /// The run matched an expected source status and was updated.
    Updated,
    /// The run exists, but its status no longer matches the caller's snapshot.
    StatusMismatch,
    /// The run metadata record does not exist.
    RunMissing,
}

/// Update a run only when its current status is one of `expected_statuses`.
///
/// This is intentionally crate-private: poller transitions use it to align the
/// run guard with their lease guard in one transaction without broadening the
/// public persistence API. Every current poller call uses exactly one expected
/// status (`WaitingExternal`), so that hot shape uses static cached SQL. The
/// dynamic fallback remains for tests and future multi-status callers and is
/// cached by SQL shape through rusqlite's bounded statement cache.
pub(crate) fn persist_run_status_from_expected_in_transaction(
    conn: &rusqlite::Transaction<'_>,
    run_id: &str,
    status: &RunStatus,
    current_step: Option<&str>,
    expected_statuses: &[RunStatus],
) -> SqliteResult<ExpectedRunStatusOutcome> {
    if expected_statuses.is_empty() {
        return classify_expected_status_rejection(conn, run_id);
    }
    let dynamic_sql;
    let sql = if expected_statuses.len() == 1 {
        EXPECTED_STATUS_UPDATE_ONE_SQL
    } else {
        let placeholders = (0..expected_statuses.len())
            .map(|offset| format!("?{}", offset + 5))
            .collect::<Vec<_>>()
            .join(", ");
        dynamic_sql = format!(
            "UPDATE runs SET status = ?1,
                 current_step = CASE WHEN ?2 IS NULL THEN current_step ELSE ?2 END,
                 updated_at = ?3
             WHERE run_id = ?4 AND status IN ({placeholders})"
        );
        &dynamic_sql
    };
    let status = status.to_string();
    let expected_statuses: Vec<String> =
        expected_statuses.iter().map(ToString::to_string).collect();
    let now = chrono::Utc::now().to_rfc3339();
    let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(4 + expected_statuses.len());
    params.extend([
        &status as &dyn rusqlite::ToSql,
        &current_step,
        &now,
        &run_id,
    ]);
    params.extend(
        expected_statuses
            .iter()
            .map(|value| value as &dyn rusqlite::ToSql),
    );
    let mut stmt = conn.prepare_cached(sql)?;
    if stmt.execute(params.as_slice())? > 0 {
        return Ok(ExpectedRunStatusOutcome::Updated);
    }
    classify_expected_status_rejection(conn, run_id)
}

// `Transaction` dereferences to `Connection`, so callers inside an existing
// transaction use this helper without starting or obscuring a new boundary.
fn classify_expected_status_rejection(
    conn: &Connection,
    run_id: &str,
) -> SqliteResult<ExpectedRunStatusOutcome> {
    Ok(match get_run_status_with_conn(conn, run_id)? {
        Some(_) => ExpectedRunStatusOutcome::StatusMismatch,
        None => ExpectedRunStatusOutcome::RunMissing,
    })
}

/// Read only the status needed to classify a rejected conditional update.
fn get_run_status_with_conn(conn: &Connection, run_id: &str) -> SqliteResult<Option<RunStatus>> {
    let mut stmt = conn.prepare_cached("SELECT status FROM runs WHERE run_id = ?1")?;
    let mut rows = stmt.query(params![run_id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let status = row.get::<_, String>(0)?.parse().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(Some(status))
}

/// Hot one-expected-status shape used by all current poller transitions.
const EXPECTED_STATUS_UPDATE_ONE_SQL: &str = concat!(
    "UPDATE runs SET status = ?1, ",
    "current_step = CASE WHEN ?2 IS NULL THEN current_step ELSE ?2 END, ",
    "updated_at = ?3 ",
    "WHERE run_id = ?4 AND status IN (?5)"
);

/// Static SQL for [`persist_run_status_conditional_with_conn`].
///
/// [`RunStatus::TERMINAL_SQL`] has 5 entries, so the placeholder list is
/// constant and the query plan is cached by `prepare_cached` across calls
/// without per-invocation `format!` allocation.
const CONDITIONAL_STATUS_UPDATE_SQL: &str = concat!(
    "UPDATE runs SET status = ?1, ",
    "current_step = CASE WHEN ?2 IS NULL THEN current_step ELSE ?2 END, ",
    "updated_at = ?3 ",
    "WHERE run_id = ?4 AND status NOT IN (?5, ?6, ?7, ?8, ?9)"
);

// Compile-time assertion: the SQL above has exactly 5 terminal-status
// placeholders (?5..?9). This must match `RunStatus::TERMINAL_SQL.len()`
// so that the bind-parameter count never drifts from the placeholder count.
// If a terminal status is added or removed, this assertion fails at compile
// time and the SQL constant must be updated in lockstep.
const _: () = assert!(
    RunStatus::TERMINAL_SQL.len() == 5,
    "CONDITIONAL_STATUS_UPDATE_SQL has 5 terminal placeholders; update the SQL when TERMINAL_SQL changes"
);

/// Get a run record by id using a borrowed connection.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub fn get_run_with_conn(conn: &Connection, run_id: &str) -> SqliteResult<Option<RunMetadata>> {
    let sql = format!("SELECT {} FROM runs WHERE run_id = ?1", RUN_SELECT_COLUMNS);
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params![run_id])?;
    if let Some(row) = rows.next()? {
        Ok(Some(map_run_row(row)?))
    } else {
        Ok(None)
    }
}

/// List all run records using a borrowed connection.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub fn list_runs_with_conn(conn: &Connection) -> SqliteResult<Vec<RunMetadata>> {
    let sql = format!(
        "SELECT {} FROM runs ORDER BY created_at DESC",
        RUN_SELECT_COLUMNS
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], map_run_row)?;
    rows.collect()
}

const RUN_ID_QUERY_CHUNK_SIZE: usize = 500;

/// List selected run records using a borrowed connection.
/// @plan:issue-117
pub fn list_runs_by_ids_with_conn(
    conn: &Connection,
    run_ids: &[&str],
) -> SqliteResult<Vec<RunMetadata>> {
    let run_ids = unique_run_ids(run_ids);
    if run_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut runs = Vec::with_capacity(run_ids.len());
    for chunk in run_ids.chunks(RUN_ID_QUERY_CHUNK_SIZE) {
        runs.extend(list_runs_by_id_chunk(conn, chunk)?);
    }
    runs.sort_by_key(|run| std::cmp::Reverse(run.created_at));
    Ok(runs)
}

fn list_runs_by_id_chunk(conn: &Connection, run_ids: &[&str]) -> SqliteResult<Vec<RunMetadata>> {
    let placeholders = std::iter::repeat_n("?", run_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT {} FROM runs WHERE run_id IN ({}) ORDER BY created_at DESC",
        RUN_SELECT_COLUMNS, placeholders
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(run_ids), map_run_row)?;
    rows.collect()
}

fn unique_run_ids<'a>(run_ids: &'a [&'a str]) -> Vec<&'a str> {
    let mut seen = std::collections::HashSet::new();
    run_ids
        .iter()
        .copied()
        .filter(|run_id| seen.insert(*run_id))
        .collect()
}

/// Map a SQLite row into a `RunMetadata`.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
fn map_run_row(row: &rusqlite::Row<'_>) -> SqliteResult<RunMetadata> {
    let status_str: String = row.get(3)?;
    let status: RunStatus = status_str.parse().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(error))
    })?;

    let created_at = row.get::<_, String>(4)?.parse().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid datetime",
            )),
        )
    })?;

    Ok(RunMetadata {
        run_id: row.get(0)?,
        workflow_type_id: row.get(1)?,
        config_id: row.get(2)?,
        status,
        created_at,
        updated_at: row
            .get::<_, Option<String>>(5)?
            .and_then(|s| s.parse().ok()),
        current_step: row.get(6)?,
        previous_step: row.get(7)?,
        previous_outcome: row.get(8)?,
        next_step_candidates: deserialize_string_list(row.get::<_, Option<String>>(9)?),
        log_path: row.get(10)?,
        artifact_root: row.get(11)?,
        workspace_path: row.get(12)?,
        repository: row.get(13)?,
        issue_number: row.get(14)?,
        pr_number: row.get(15)?,
        head_sha: row.get(16)?,
        process_pid: row.get::<_, Option<i64>>(17)?.map(|p| p as u32),
        child_pids: deserialize_pid_list(row.get::<_, Option<String>>(18)?),
        failure_cleanup: row
            .get::<_, Option<String>>(19)?
            .map(|raw| serde_json::from_str(&raw))
            .transpose()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    19,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn test_conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        init_runs_schema(&c).unwrap();
        c
    }

    fn seed_run(conn: &Connection, run_id: &str, status: RunStatus, step: Option<&str>) {
        let mut metadata = RunMetadata::new(run_id, "wf", "cfg");
        metadata.status = status;
        if let Some(s) = step {
            metadata.set_current_step(s);
        }
        persist_run_with_conn(conn, &metadata).unwrap();
    }

    #[test]
    fn run_status_readers_preserve_typed_parse_error_source() {
        let c = test_conn();
        seed_run(&c, "run-invalid-status", RunStatus::Running, None);
        c.execute(
            "UPDATE runs SET status = 'not-a-run-status' WHERE run_id = ?1",
            params!["run-invalid-status"],
        )
        .unwrap();

        for (error, expected_column) in [
            (
                get_run_status_with_conn(&c, "run-invalid-status")
                    .expect_err("status-only reader must reject corrupt status"),
                0,
            ),
            (
                get_run_with_conn(&c, "run-invalid-status")
                    .expect_err("full-row reader must reject corrupt status"),
                3,
            ),
        ] {
            match error {
                rusqlite::Error::FromSqlConversionFailure(
                    column,
                    rusqlite::types::Type::Text,
                    source,
                ) => {
                    assert_eq!(column, expected_column);
                    assert!(
                        source
                            .downcast_ref::<crate::persistence::run_metadata::RunStatusParseError>()
                            .is_some(),
                        "run-status parse failures must remain directly downcastable"
                    );
                }
                other => panic!("expected FromSqlConversionFailure, got {other}"),
            }
        }
    }

    #[test]
    fn conditional_status_none_preserves_existing_current_step() {
        // OCR 3565748071: current_step = None must preserve the existing
        // value, not null it out — analogous to lease run_id preservation.
        let c = test_conn();
        seed_run(&c, "run-keep", RunStatus::Running, Some("step-alpha"));

        let applied = persist_run_status_conditional_with_conn(
            &c,
            "run-keep",
            &RunStatus::WaitingExternal,
            None,
        )
        .unwrap();

        assert!(applied, "non-terminal run must be updated");
        let run = get_run_with_conn(&c, "run-keep").unwrap().unwrap();
        assert_eq!(run.status, RunStatus::WaitingExternal);
        assert_eq!(
            run.current_step.as_deref(),
            Some("step-alpha"),
            "None current_step must preserve the existing value, not null it"
        );
    }

    #[test]
    fn conditional_status_some_updates_current_step() {
        // OCR 3565748071: current_step = Some must write the new value.
        let c = test_conn();
        seed_run(&c, "run-set", RunStatus::Running, Some("step-old"));

        let applied = persist_run_status_conditional_with_conn(
            &c,
            "run-set",
            &RunStatus::WaitingExternal,
            Some("step-new"),
        )
        .unwrap();

        assert!(applied, "non-terminal run must be updated");
        let run = get_run_with_conn(&c, "run-set").unwrap().unwrap();
        assert_eq!(run.status, RunStatus::WaitingExternal);
        assert_eq!(
            run.current_step.as_deref(),
            Some("step-new"),
            "Some current_step must update to the new value"
        );
    }

    #[test]
    fn conditional_status_none_preserves_null_current_step() {
        // When the run has no current_step and None is passed, the column
        // must remain NULL (no spurious value injected).
        let c = test_conn();
        seed_run(&c, "run-null", RunStatus::Running, None);

        let applied = persist_run_status_conditional_with_conn(
            &c,
            "run-null",
            &RunStatus::WaitingExternal,
            None,
        )
        .unwrap();

        assert!(applied);
        let run = get_run_with_conn(&c, "run-null").unwrap().unwrap();
        assert_eq!(run.status, RunStatus::WaitingExternal);
        assert!(
            run.current_step.is_none(),
            "NULL current_step must remain NULL when None is passed"
        );
    }

    #[test]
    fn conditional_status_some_sets_current_step_from_null() {
        // When the run has no current_step and Some is passed, the column
        // must be set to the provided value.
        let c = test_conn();
        seed_run(&c, "run-from-null", RunStatus::Running, None);

        let applied = persist_run_status_conditional_with_conn(
            &c,
            "run-from-null",
            &RunStatus::WaitingExternal,
            Some("step-fresh"),
        )
        .unwrap();

        assert!(applied);
        let run = get_run_with_conn(&c, "run-from-null").unwrap().unwrap();
        assert_eq!(run.current_step.as_deref(), Some("step-fresh"));
    }

    #[test]
    fn conditional_status_terminal_guard_rejects_and_preserves_step() {
        // OCR 3565748071: the terminal guard must reject the update AND
        // must not alter current_step on a terminal run.
        for terminal in [
            RunStatus::Completed,
            RunStatus::Failed,
            RunStatus::Abandoned,
            RunStatus::Merged,
            RunStatus::Cancelled,
        ] {
            let c = test_conn();
            let run_id = format!("run-term-{}", terminal);
            seed_run(&c, &run_id, terminal.clone(), Some("final-step"));

            // Try both None and Some — both must be rejected.
            let applied_none = persist_run_status_conditional_with_conn(
                &c,
                &run_id,
                &RunStatus::WaitingExternal,
                None,
            )
            .unwrap();
            assert!(
                !applied_none,
                "terminal {terminal} must reject update (None step)"
            );

            let applied_some = persist_run_status_conditional_with_conn(
                &c,
                &run_id,
                &RunStatus::WaitingExternal,
                Some("sneaky-step"),
            )
            .unwrap();
            assert!(
                !applied_some,
                "terminal {terminal} must reject update (Some step)"
            );

            let run = get_run_with_conn(&c, &run_id).unwrap().unwrap();
            assert_eq!(
                run.status, terminal,
                "terminal status must not be resurrected"
            );
            assert_eq!(
                run.current_step.as_deref(),
                Some("final-step"),
                "current_step must be unchanged after rejected update"
            );
        }
    }

    #[test]
    fn conditional_outcome_wrapper_classifies_terminal_and_missing_runs() {
        let c = test_conn();
        seed_run(&c, "run-terminal", RunStatus::Completed, Some("final-step"));

        let terminal = persist_run_status_conditional_outcome_with_conn(
            &c,
            "run-terminal",
            &RunStatus::WaitingExternal,
            Some("stale-step"),
        )
        .unwrap();
        assert_eq!(
            terminal,
            ConditionalStatusOutcome::AlreadyTerminal(RunStatus::Completed)
        );

        let missing = persist_run_status_conditional_outcome_with_conn(
            &c,
            "run-missing",
            &RunStatus::WaitingExternal,
            None,
        )
        .unwrap();
        assert_eq!(missing, ConditionalStatusOutcome::RunMissing);
    }

    #[test]
    fn conditional_outcome_in_transaction_does_not_start_nested_transaction() {
        let c = test_conn();
        seed_run(&c, "run-nested", RunStatus::Running, Some("step-old"));
        let tx = c.unchecked_transaction().unwrap();

        let outcome = persist_run_status_conditional_outcome_in_transaction(
            &tx,
            "run-nested",
            &RunStatus::WaitingExternal,
            Some("step-new"),
        )
        .unwrap();
        assert_eq!(outcome, ConditionalStatusOutcome::Updated);
        tx.commit().unwrap();

        let run = get_run_with_conn(&c, "run-nested").unwrap().unwrap();
        assert_eq!(run.status, RunStatus::WaitingExternal);
        assert_eq!(run.current_step.as_deref(), Some("step-new"));
    }

    #[test]
    fn conditional_status_updates_updated_at_timestamp() {
        // Deterministic timestamp test: seed the run with a fixed past
        // updated_at (well before "now") so the conditional UPDATE must
        // always write a strictly newer timestamp. No sleep needed.
        let c = test_conn();
        seed_run(&c, "run-ts", RunStatus::Running, Some("step-a"));
        // Pin the row's updated_at to a deterministic past timestamp.
        let past = chrono::DateTime::parse_from_rfc3339("2020-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        c.execute(
            "UPDATE runs SET updated_at = ?1 WHERE run_id = ?2",
            rusqlite::params![past.to_rfc3339(), "run-ts"],
        )
        .unwrap();
        let before = get_run_with_conn(&c, "run-ts").unwrap().unwrap();
        assert_eq!(before.updated_at, Some(past));

        persist_run_status_conditional_with_conn(&c, "run-ts", &RunStatus::WaitingExternal, None)
            .unwrap();

        let after = get_run_with_conn(&c, "run-ts").unwrap().unwrap();
        assert!(
            after.updated_at > before.updated_at,
            "updated_at must advance from the pinned past value to the current time"
        );
    }
}
