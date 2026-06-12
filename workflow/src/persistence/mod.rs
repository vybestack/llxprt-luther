/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
/// Persistence module - durable storage for run metadata and state.
pub mod artifacts;
pub mod checkpoint;
pub mod run_metadata;
pub mod sqlite;
pub mod trace;

pub use artifacts::{
    default_artifacts_root, get_artifacts_dir, list_artifacts, read_artifact, write_artifact,
    ArtifactRecord,
};
pub use checkpoint::{
    append_event, append_event_with_conn, list_checkpoints, load_checkpoint,
    load_checkpoint_with_conn, load_events, save_checkpoint, save_checkpoint_with_conn, Checkpoint,
    EventRecord, PersistenceError, StateSnapshot,
};
pub use run_metadata::{run_metadata_from_ref, RunMetadata, RunStatus};
pub use sqlite::{SqliteStore, SqliteStoreRef};
pub use trace::{
    export_trace, load_trace, save_trace, SmokeTrace, TraceEvent, TraceOutcome, SCHEMA_VERSION,
};

use rusqlite::Connection;
use std::path::Path;

/// Initialize the checkpoint database at the given path.
/// Creates the database directory if needed and initializes the schema.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
pub fn init_database(db_path: &Path) -> Result<(), checkpoint::PersistenceError> {
    // Ensure parent directory exists
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).map_err(checkpoint::PersistenceError::Io)?;
    }

    // Open and initialize the database
    let conn = Connection::open(db_path).map_err(|e| {
        checkpoint::PersistenceError::Database(format!("Failed to open database: {}", e))
    })?;

    // Initialize checkpoint table
    checkpoint::init_checkpoint_table(&conn).map_err(|e| {
        checkpoint::PersistenceError::Database(format!("Failed to initialize schema: {}", e))
    })?;

    Ok(())
}
