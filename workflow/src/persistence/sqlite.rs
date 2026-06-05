/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
/// SQLite persistence layer for run metadata.
use std::path::Path;

use rusqlite::{params, Connection, Result as SqliteResult};

use crate::persistence::run_metadata::{RunMetadata, RunStatus};

/// SQLite database connection wrapper for run persistence.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub struct SqliteStore {
    conn: Connection,
}

impl SqliteStore {
    /// Open or create a SQLite database at the given path.
    /// Initializes the schema if needed.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    pub fn open(db_path: impl AsRef<Path>) -> SqliteResult<Self> {
        let conn = Connection::open(db_path)?;
        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    /// Create an in-memory SQLite store (for testing).
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    pub fn open_in_memory() -> SqliteResult<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    /// Initialize the database schema.
    /// Creates the `runs` table with columns for all identifiers.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    /// @requirement:REQ-EARS-PERSIST-001,REQ-EARS-SCALE-002
    fn init_schema(&self) -> SqliteResult<()> {
        self.conn.execute(
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

    /// Persist a new run metadata record.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    /// @requirement:REQ-EARS-PERSIST-001
    pub fn persist_run(&self, metadata: &RunMetadata) -> SqliteResult<()> {
        self.conn.execute(
            "INSERT INTO runs (run_id, workflow_type_id, config_id, status, created_at, updated_at, current_step)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(run_id) DO UPDATE SET
                workflow_type_id = excluded.workflow_type_id,
                config_id = excluded.config_id,
                status = excluded.status,
                created_at = excluded.created_at,
                updated_at = excluded.updated_at,
                current_step = excluded.current_step",
            params![
                metadata.run_id,
                metadata.workflow_type_id,
                metadata.config_id,
                metadata.status.to_string(),
                metadata.created_at.to_rfc3339(),
                metadata.updated_at.map(|t| t.to_rfc3339()),
                metadata.current_step,
            ],
        )?;
        Ok(())
    }

    /// Get run metadata by run_id.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    pub fn get_run(&self, run_id: &str) -> SqliteResult<Option<RunMetadata>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, workflow_type_id, config_id, status, created_at, updated_at, current_step
             FROM runs WHERE run_id = ?1"
        )?;

        let mut rows = stmt.query(params![run_id])?;

        if let Some(row) = rows.next()? {
            let status_str: String = row.get(3)?;
            let status = status_str.parse().map_err(|e: String| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
                )
            })?;

            Ok(Some(RunMetadata {
                run_id: row.get(0)?,
                workflow_type_id: row.get(1)?,
                config_id: row.get(2)?,
                status,
                created_at: row.get::<_, String>(4)?.parse().map_err(|_| {
                    rusqlite::Error::FromSqlConversionFailure(
                        4,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "Invalid datetime",
                        )),
                    )
                })?,
                updated_at: row
                    .get::<_, Option<String>>(5)?
                    .map(|s| s.parse().ok())
                    .flatten(),
                current_step: row.get(6)?,
            }))
        } else {
            Ok(None)
        }
    }

    /// List all runs in the database.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    pub fn list_runs(&self) -> SqliteResult<Vec<RunMetadata>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, workflow_type_id, config_id, status, created_at, updated_at, current_step
             FROM runs ORDER BY created_at DESC"
        )?;

        let rows = stmt.query_map([], |row| {
            let status_str: String = row.get(3)?;
            let status = status_str.parse().map_err(|e: String| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
                )
            })?;

            Ok(RunMetadata {
                run_id: row.get(0)?,
                workflow_type_id: row.get(1)?,
                config_id: row.get(2)?,
                status,
                created_at: row.get::<_, String>(4)?.parse().map_err(|_| {
                    rusqlite::Error::FromSqlConversionFailure(
                        4,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "Invalid datetime",
                        )),
                    )
                })?,
                updated_at: row
                    .get::<_, Option<String>>(5)?
                    .map(|s| s.parse().ok())
                    .flatten(),
                current_step: row.get(6)?,
            })
        })?;

        rows.collect()
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
        self.conn.execute(
            "INSERT INTO runs (run_id, workflow_type_id, config_id, status, created_at, updated_at, current_step)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(run_id) DO UPDATE SET
                workflow_type_id = excluded.workflow_type_id,
                config_id = excluded.config_id,
                status = excluded.status,
                created_at = excluded.created_at,
                updated_at = excluded.updated_at,
                current_step = excluded.current_step",
            params![
                metadata.run_id,
                metadata.workflow_type_id,
                metadata.config_id,
                metadata.status.to_string(),
                metadata.created_at.to_rfc3339(),
                metadata.updated_at.map(|t| t.to_rfc3339()),
                metadata.current_step,
            ],
        )?;
        Ok(())
    }
}
