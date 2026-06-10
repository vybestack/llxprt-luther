/// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
/// Checkpoint persistence - saves and restores workflow execution state.
use std::cell::RefCell;
use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use thiserror::Error;

/// Errors that can occur during checkpoint persistence.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
/// @requirement:REQ-EARS-PERSIST-004
#[derive(Error, Debug)]
pub enum PersistenceError {
    #[error("database error: {0}")]
    Database(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("checkpoint not found: {0}")]
    NotFound(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<rusqlite::Error> for PersistenceError {
    fn from(err: rusqlite::Error) -> Self {
        PersistenceError::Database(err.to_string())
    }
}

// Thread-local storage for default database connection (for backwards compatibility)
thread_local! {
    static DEFAULT_CONN: RefCell<Option<Connection>> = const { RefCell::new(None) };
}

/// Initialize the default connection (for backwards-compatible API).
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
fn ensure_default_conn() -> Result<(), PersistenceError> {
    DEFAULT_CONN.with(|c| {
        let mut conn_opt = c.borrow_mut();
        if conn_opt.is_none() {
            let conn = Connection::open_in_memory().map_err(|e| {
                PersistenceError::Database(format!("Failed to create in-memory DB: {}", e))
            })?;
            // Initialize schema
            conn.execute(
                "CREATE TABLE IF NOT EXISTS checkpoints (
                    run_id TEXT NOT NULL,
                    step_id TEXT NOT NULL,
                    retry_count INTEGER NOT NULL DEFAULT 0,
                    loop_count INTEGER NOT NULL DEFAULT 0,
                    context TEXT,
                    status TEXT NOT NULL DEFAULT 'running',
                    timestamp TEXT NOT NULL,
                    PRIMARY KEY (run_id, step_id)
                )",
                [],
            )
            .map_err(|e| PersistenceError::Database(e.to_string()))?;
            conn.execute(
                "CREATE TABLE IF NOT EXISTS events (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    run_id TEXT NOT NULL,
                    step_id TEXT NOT NULL,
                    outcome TEXT NOT NULL,
                    timestamp TEXT NOT NULL
                )",
                [],
            )
            .map_err(|e| PersistenceError::Database(e.to_string()))?;
            *conn_opt = Some(conn);
        }
        Ok(())
    })
}

/// A checkpoint capturing the state of a workflow run at a point in time.
/// Used for resumable execution and crash recovery.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
/// @requirement:REQ-EARS-ENG-002,REQ-EARS-ENG-004,REQ-EARS-PERSIST-002
#[derive(Debug, Clone)]
pub struct Checkpoint {
    /// The unique identifier of the run this checkpoint belongs to.
    pub run_id: String,
    /// The step_id that was active when this checkpoint was created.
    pub step_id: String,
    /// The current state of the workflow - includes loop counters,
    /// retry counts, and other execution context.
    pub state_snapshot: StateSnapshot,
    /// When this checkpoint was created.
    pub timestamp: DateTime<Utc>,
}

/// Snapshot of workflow execution state.
/// Contains data needed to resume execution from this point.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
/// @plan:PLAN-20260408-LLXPRT-FIRST.P12
/// @requirement:REQ-EARS-PERSIST-002,REQ-LF-LOOP-005
#[derive(Debug, Clone)]
pub struct StateSnapshot {
    /// Current retry count for the current step.
    pub retry_count: u32,
    /// Remediation loop counter.
    pub loop_count: u32,
    /// Per-edge loop counts keyed by "from:to" step pair.
    pub edge_loop_counts: HashMap<String, u32>,
    /// Additional context data (step-specific state).
    pub context: HashMap<String, serde_json::Value>,
    /// Status of the checkpoint (e.g., "completed", "interrupted").
    pub status: String,
}

impl Default for StateSnapshot {
    fn default() -> Self {
        Self {
            retry_count: 0,
            loop_count: 0,
            edge_loop_counts: HashMap::new(),
            context: HashMap::new(),
            status: "running".to_string(),
        }
    }
}

impl Checkpoint {
    /// Create a new checkpoint for a run at the given step.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    /// @requirement:REQ-EARS-PERSIST-002
    pub fn new(run_id: impl Into<String>, step_id: impl Into<String>) -> Self {
        Self {
            run_id: run_id.into(),
            step_id: step_id.into(),
            state_snapshot: StateSnapshot::default(),
            timestamp: Utc::now(),
        }
    }

    /// Create a checkpoint with a specific state snapshot.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    pub fn with_snapshot(
        run_id: impl Into<String>,
        step_id: impl Into<String>,
        snapshot: StateSnapshot,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            step_id: step_id.into(),
            state_snapshot: snapshot,
            timestamp: Utc::now(),
        }
    }

    /// Mark this checkpoint as an interruption checkpoint.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    /// @requirement:REQ-EARS-ENG-004
    pub fn mark_interrupted(&mut self) {
        self.state_snapshot.status = "interrupted".to_string();
        self.timestamp = Utc::now();
    }

    /// Mark this checkpoint as completed for the step.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    pub fn mark_completed(&mut self) {
        self.state_snapshot.status = "completed".to_string();
        self.timestamp = Utc::now();
    }
}

/// Save a checkpoint to persistent storage using a specific connection.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
/// @plan:PLAN-20260408-LLXPRT-FIRST.P14
/// @requirement:REQ-EARS-ENG-002,REQ-EARS-PERSIST-002,REQ-EARS-PERSIST-004,REQ-LF-LOOP-005
pub fn save_checkpoint_with_conn(
    conn: &Connection,
    checkpoint: &Checkpoint,
) -> Result<(), PersistenceError> {
    // Create the checkpoints table if it doesn't exist
    conn.execute(
        "CREATE TABLE IF NOT EXISTS checkpoints (
            run_id TEXT NOT NULL,
            step_id TEXT NOT NULL,
            retry_count INTEGER NOT NULL DEFAULT 0,
            loop_count INTEGER NOT NULL DEFAULT 0,
            context TEXT,
            status TEXT NOT NULL DEFAULT 'running',
            timestamp TEXT NOT NULL,
            PRIMARY KEY (run_id, step_id)
        )",
        [],
    )?;

    // Build context with edge_loop_counts stored under reserved key
    let mut context_data = checkpoint.state_snapshot.context.clone();
    context_data.insert(
        "__edge_loop_counts".to_string(),
        serde_json::to_value(&checkpoint.state_snapshot.edge_loop_counts).map_err(|e| {
            PersistenceError::Serialization(format!("Failed to serialize edge_loop_counts: {}", e))
        })?,
    );

    // Serialize context to JSON
    let context_json = serde_json::to_string(&context_data).map_err(|e| {
        PersistenceError::Serialization(format!("Failed to serialize context: {}", e))
    })?;

    // Insert or replace the checkpoint
    conn.execute(
        "INSERT INTO checkpoints (run_id, step_id, retry_count, loop_count, context, status, timestamp)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(run_id, step_id) DO UPDATE SET
            retry_count = excluded.retry_count,
            loop_count = excluded.loop_count,
            context = excluded.context,
            status = excluded.status,
            timestamp = excluded.timestamp",
        params![
            checkpoint.run_id,
            checkpoint.step_id,
            checkpoint.state_snapshot.retry_count,
            checkpoint.state_snapshot.loop_count,
            context_json,
            checkpoint.state_snapshot.status,
            checkpoint.timestamp.to_rfc3339(),
        ],
    )?;

    Ok(())
}

/// Save a checkpoint to persistent storage (backwards-compatible version).
/// Uses a thread-local default connection.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
/// @requirement:REQ-EARS-ENG-002,REQ-EARS-PERSIST-002,REQ-EARS-PERSIST-004
pub fn save_checkpoint(_run_id: &str, checkpoint: &Checkpoint) -> Result<(), PersistenceError> {
    ensure_default_conn()?;
    DEFAULT_CONN.with(|c| {
        let conn_opt = c.borrow();
        if let Some(ref conn) = *conn_opt {
            save_checkpoint_with_conn(conn, checkpoint)
        } else {
            Err(PersistenceError::Database(
                "No database connection available".to_string(),
            ))
        }
    })
}

/// Load the checkpoint for a run using a specific connection.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
/// @plan:PLAN-20260408-LLXPRT-FIRST.P14
/// @requirement:REQ-EARS-ENG-004,REQ-EARS-PERSIST-002,REQ-LF-LOOP-005
pub fn load_checkpoint_with_conn(
    conn: &Connection,
    run_id: &str,
) -> Result<Option<Checkpoint>, PersistenceError> {
    let mut stmt = conn.prepare(
        "SELECT run_id, step_id, retry_count, loop_count, context, status, timestamp
         FROM checkpoints
         WHERE run_id = ?1
         ORDER BY timestamp DESC
         LIMIT 1",
    )?;

    let mut rows = stmt.query(params![run_id])?;

    if let Some(row) = rows.next()? {
        let context_json: String = row.get(4)?;
        let context: HashMap<String, serde_json::Value> = serde_json::from_str(&context_json)
            .map_err(|e| {
                PersistenceError::Serialization(format!("Failed to deserialize context: {}", e))
            })?;

        // Extract edge_loop_counts from context blob under reserved key
        let edge_loop_counts = context
            .get("__edge_loop_counts")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let timestamp_str: String = row.get(6)?;
        let timestamp = timestamp_str
            .parse()
            .map_err(|_| PersistenceError::Database("Invalid timestamp format".to_string()))?;

        Ok(Some(Checkpoint {
            run_id: row.get(0)?,
            step_id: row.get(1)?,
            state_snapshot: StateSnapshot {
                retry_count: row.get::<_, i64>(2)? as u32,
                loop_count: row.get::<_, i64>(3)? as u32,
                edge_loop_counts,
                context,
                status: row.get(5)?,
            },
            timestamp,
        }))
    } else {
        Ok(None)
    }
}

/// Load the checkpoint for a run (backwards-compatible version).
/// Uses a thread-local default connection.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
/// @requirement:REQ-EARS-ENG-004,REQ-EARS-PERSIST-002
pub fn load_checkpoint(_run_id: &str) -> Result<Option<Checkpoint>, PersistenceError> {
    ensure_default_conn()?;
    DEFAULT_CONN.with(|c| {
        let conn_opt = c.borrow();
        if let Some(ref conn) = *conn_opt {
            load_checkpoint_with_conn(conn, _run_id)
        } else {
            Err(PersistenceError::Database(
                "No database connection available".to_string(),
            ))
        }
    })
}

/// List all checkpoints for a run.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
/// @plan:PLAN-20260408-LLXPRT-FIRST.P14
/// @requirement:REQ-EARS-PERSIST-002,REQ-LF-LOOP-005
pub fn list_checkpoints(
    conn: &Connection,
    run_id: &str,
) -> Result<Vec<Checkpoint>, PersistenceError> {
    let mut stmt = conn.prepare(
        "SELECT run_id, step_id, retry_count, loop_count, context, status, timestamp
         FROM checkpoints
         WHERE run_id = ?1
         ORDER BY timestamp ASC",
    )?;

    let rows = stmt.query_map(params![run_id], |row| {
        let context_json: String = row.get(4)?;
        let context: HashMap<String, serde_json::Value> = serde_json::from_str(&context_json)
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    4,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("Failed to deserialize context: {}", e),
                    )),
                )
            })?;

        // Extract edge_loop_counts from context blob under reserved key
        let edge_loop_counts = context
            .get("__edge_loop_counts")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let timestamp_str: String = row.get(6)?;
        let timestamp = timestamp_str.parse().map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                6,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Invalid timestamp format",
                )),
            )
        })?;

        Ok(Checkpoint {
            run_id: row.get(0)?,
            step_id: row.get(1)?,
            state_snapshot: StateSnapshot {
                retry_count: row.get::<_, i64>(2)? as u32,
                loop_count: row.get::<_, i64>(3)? as u32,
                edge_loop_counts,
                context,
                status: row.get(5)?,
            },
            timestamp,
        })
    })?;

    let mut checkpoints = Vec::new();
    for checkpoint in rows {
        checkpoints.push(checkpoint?);
    }

    Ok(checkpoints)
}

/// Append an event record for a step completion using a specific connection.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
/// @requirement:REQ-EARS-PERSIST-002
pub fn append_event_with_conn(
    conn: &Connection,
    run_id: &str,
    step_id: &str,
    outcome: &str,
    timestamp: DateTime<Utc>,
) -> Result<(), PersistenceError> {
    // Create the events table if it doesn't exist
    conn.execute(
        "CREATE TABLE IF NOT EXISTS events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id TEXT NOT NULL,
            step_id TEXT NOT NULL,
            outcome TEXT NOT NULL,
            timestamp TEXT NOT NULL
        )",
        [],
    )?;

    // Insert the event
    conn.execute(
        "INSERT INTO events (run_id, step_id, outcome, timestamp)
         VALUES (?1, ?2, ?3, ?4)",
        params![run_id, step_id, outcome, timestamp.to_rfc3339()],
    )?;

    Ok(())
}

/// Append an event record for a step completion (backwards-compatible version).
/// Uses a thread-local default connection.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
/// @requirement:REQ-EARS-PERSIST-002
pub fn append_event(
    _run_id: &str,
    step_id: &str,
    outcome: &crate::engine::transition::StepOutcome,
    timestamp: DateTime<Utc>,
) -> Result<(), PersistenceError> {
    ensure_default_conn()?;
    DEFAULT_CONN.with(|c| {
        let conn_opt = c.borrow();
        if let Some(ref conn) = *conn_opt {
            append_event_with_conn(conn, _run_id, step_id, &outcome.to_string(), timestamp)
        } else {
            Err(PersistenceError::Database(
                "No database connection available".to_string(),
            ))
        }
    })
}

/// Load events for a run.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
/// @requirement:REQ-EARS-PERSIST-002
pub fn load_events(conn: &Connection, run_id: &str) -> Result<Vec<EventRecord>, PersistenceError> {
    let mut stmt = conn.prepare(
        "SELECT run_id, step_id, outcome, timestamp
         FROM events
         WHERE run_id = ?1
         ORDER BY timestamp ASC",
    )?;

    let rows = stmt.query_map(params![run_id], |row| {
        let timestamp_str: String = row.get(3)?;
        let timestamp = timestamp_str.parse().map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Invalid timestamp format",
                )),
            )
        })?;

        Ok(EventRecord {
            run_id: row.get(0)?,
            step_id: row.get(1)?,
            outcome: row.get(2)?,
            timestamp,
        })
    })?;

    let mut events = Vec::new();
    for event in rows {
        events.push(event?);
    }

    Ok(events)
}

/// Event record for step completion.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
#[derive(Debug, Clone)]
pub struct EventRecord {
    pub run_id: String,
    pub step_id: String,
    pub outcome: String,
    pub timestamp: DateTime<Utc>,
}

/// Initialize the checkpoint and events tables in the given connection.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
pub fn init_checkpoint_table(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS checkpoints (
            run_id TEXT NOT NULL,
            step_id TEXT NOT NULL,
            retry_count INTEGER NOT NULL DEFAULT 0,
            loop_count INTEGER NOT NULL DEFAULT 0,
            context TEXT,
            status TEXT NOT NULL DEFAULT 'running',
            timestamp TEXT NOT NULL,
            PRIMARY KEY (run_id, step_id)
        )",
        [],
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id TEXT NOT NULL,
            step_id TEXT NOT NULL,
            outcome TEXT NOT NULL,
            timestamp TEXT NOT NULL
        )",
        [],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_can_be_created() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P08
        let cp = Checkpoint::new("run-123", "step-1");
        assert_eq!(cp.run_id, "run-123");
        assert_eq!(cp.step_id, "step-1");
        assert_eq!(cp.state_snapshot.status, "running");
    }

    #[test]
    fn checkpoint_with_snapshot() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P08
        let snapshot = StateSnapshot {
            retry_count: 2,
            loop_count: 1,
            edge_loop_counts: HashMap::new(),
            context: HashMap::new(),
            status: "running".to_string(),
        };
        let cp = Checkpoint::with_snapshot("run-456", "step-2", snapshot);
        assert_eq!(cp.run_id, "run-456");
        assert_eq!(cp.step_id, "step-2");
        assert_eq!(cp.state_snapshot.retry_count, 2);
        assert_eq!(cp.state_snapshot.loop_count, 1);
    }

    #[test]
    fn checkpoint_mark_interrupted() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P08
        let mut cp = Checkpoint::new("run-789", "step-3");
        cp.mark_interrupted();
        assert_eq!(cp.state_snapshot.status, "interrupted");
    }

    #[test]
    fn persistence_error_variants_exist() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P08
        let _db = PersistenceError::Database("test".to_string());
        let _ser = PersistenceError::Serialization("test".to_string());
        let _nf = PersistenceError::NotFound("test".to_string());
    }

    #[test]
    fn save_and_load_checkpoint() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P08
        let conn = Connection::open_in_memory().expect("Failed to open in-memory database");

        let checkpoint = Checkpoint::new("run-123", "step-a");
        save_checkpoint_with_conn(&conn, &checkpoint).expect("Failed to save checkpoint");

        let loaded =
            load_checkpoint_with_conn(&conn, "run-123").expect("Failed to load checkpoint");
        assert!(loaded.is_some(), "Checkpoint should be found");
        let loaded_cp = loaded.unwrap();
        assert_eq!(loaded_cp.run_id, "run-123");
        assert_eq!(loaded_cp.step_id, "step-a");
    }

    #[test]
    fn checkpoint_preserves_counters() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P08
        let conn = Connection::open_in_memory().expect("Failed to open in-memory database");

        let snapshot = StateSnapshot {
            retry_count: 3,
            loop_count: 2,
            edge_loop_counts: HashMap::new(),
            context: HashMap::new(),
            status: "interrupted".to_string(),
        };
        let checkpoint = Checkpoint::with_snapshot("run-456", "step-b", snapshot);
        save_checkpoint_with_conn(&conn, &checkpoint).expect("Failed to save checkpoint");

        let loaded =
            load_checkpoint_with_conn(&conn, "run-456").expect("Failed to load checkpoint");
        assert!(loaded.is_some());
        let loaded_cp = loaded.unwrap();
        assert_eq!(loaded_cp.state_snapshot.retry_count, 3);
        assert_eq!(loaded_cp.state_snapshot.loop_count, 2);
        assert_eq!(loaded_cp.state_snapshot.status, "interrupted");
    }

    #[test]
    fn save_and_load_events() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P08
        let conn = Connection::open_in_memory().expect("Failed to open in-memory database");

        let timestamp = Utc::now();
        append_event_with_conn(&conn, "run-123", "step-a", "success", timestamp)
            .expect("Failed to append event");
        append_event_with_conn(&conn, "run-123", "step-b", "success", timestamp)
            .expect("Failed to append event");

        let events = load_events(&conn, "run-123").expect("Failed to load events");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].step_id, "step-a");
        assert_eq!(events[0].outcome, "success");
        assert_eq!(events[1].step_id, "step-b");
    }
}
