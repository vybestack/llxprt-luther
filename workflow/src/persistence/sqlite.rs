/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
/// SQLite persistence layer for run metadata.
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, Result as SqliteResult};

use crate::persistence::run_metadata::{
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
        init_runs_schema(&self.conn)
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
}

/// SQL column list shared by the runs SELECT statements (must match query order).
const RUN_SELECT_COLUMNS: &str =
    "run_id, workflow_type_id, config_id, status, created_at, updated_at, current_step, \
     previous_step, previous_outcome, next_step_candidates, log_path, artifact_root, \
     workspace_path, repository, issue_number, pr_number, head_sha, process_pid, child_pids";

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
            child_pids TEXT
        )",
        [],
    )?;
    migrate_runs_table(conn);
    Ok(())
}

/// Persist a run record (insert or update) using a borrowed connection.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub fn persist_run_with_conn(conn: &Connection, metadata: &RunMetadata) -> SqliteResult<()> {
    conn.execute(
        "INSERT INTO runs (run_id, workflow_type_id, config_id, status, created_at, updated_at, \
            current_step, previous_step, previous_outcome, next_step_candidates, log_path, \
            artifact_root, workspace_path, repository, issue_number, pr_number, head_sha, \
            process_pid, child_pids)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
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
            child_pids = excluded.child_pids",
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
        ],
    )?;
    Ok(())
}

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

/// Map a SQLite row into a `RunMetadata`.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
fn map_run_row(row: &rusqlite::Row<'_>) -> SqliteResult<RunMetadata> {
    let status_str: String = row.get(3)?;
    let status: RunStatus = status_str.parse().map_err(|e: String| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
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
    })
}
