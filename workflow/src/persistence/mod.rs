/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
/// Persistence module - durable storage for run metadata and state.

pub mod artifacts;
pub mod checkpoint;
pub mod run_metadata;
pub mod sqlite;

pub use artifacts::{ArtifactRecord, default_artifacts_root, get_artifacts_dir, list_artifacts, read_artifact, write_artifact};
pub use checkpoint::{save_checkpoint, save_checkpoint_with_conn, load_checkpoint, load_checkpoint_with_conn, list_checkpoints, Checkpoint, PersistenceError, StateSnapshot, EventRecord, append_event, append_event_with_conn, load_events};
pub use run_metadata::{run_metadata_from_ref, RunMetadata, RunStatus};
pub use sqlite::SqliteStore;

use std::path::Path;
use rusqlite::Connection;

/// Initialize the checkpoint database at the given path.
/// Creates the database directory if needed and initializes the schema.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
pub fn init_database(db_path: &Path) -> Result<(), checkpoint::PersistenceError> {
    // Ensure parent directory exists
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            checkpoint::PersistenceError::Io(e)
        })?;
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
