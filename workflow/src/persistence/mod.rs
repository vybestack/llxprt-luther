/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
/// Persistence module - durable storage for run metadata and state.
pub mod artifacts;
pub mod checkpoint;
pub mod leases;
pub mod run_metadata;
pub mod sqlite;
pub mod trace;
pub mod wait_state;

pub use artifacts::{
    default_artifacts_root, get_artifacts_dir, list_artifacts, read_artifact, write_artifact,
    write_poll_result_artifact, write_resume_decision_artifact, write_wait_state_artifact,
    ArtifactRecord,
};
pub use checkpoint::{
    append_event, append_event_with_conn, append_typed_event_with_conn, count_events_by_type,
    get_checkpoint_for_step, is_resumable_checkpoint_status, list_checkpoints, load_checkpoint,
    load_checkpoint_before_step, load_checkpoint_with_conn, load_events, load_events_by_type,
    load_latest_event, load_recent_events, save_checkpoint, save_checkpoint_with_conn,
    set_resume_point, Checkpoint, EventRecord, EventType, PersistenceError, StateSnapshot,
    CHECKPOINT_STATUS_INTERRUPTED, CHECKPOINT_STATUS_READY_TO_RESUME, CHECKPOINT_STATUS_WAITING,
};
pub use leases::{
    count_active_leases, count_active_leases_for_config, count_active_leases_for_repository,
    create_lease, get_lease_for_issue, init_leases_table, list_all_leases, list_leases_by_config,
    list_leases_by_status, list_ready_to_resume_leases, mark_stale_leases,
    mark_stale_ready_to_resume_leases, touch_lease_heartbeat, try_claim, update_lease_status,
    IssueLease, LeaseStatus,
};
pub use run_metadata::{is_pid_stale, run_metadata_from_ref, RunMetadata, RunStatus};
pub use sqlite::{
    get_run_with_conn, list_runs_by_ids_with_conn, list_runs_with_conn, persist_run_with_conn,
    SqliteStore, SqliteStoreRef,
};
pub use trace::{
    export_trace, load_trace, save_trace, SmokeTrace, TraceEvent, TraceOutcome, SCHEMA_VERSION,
};
pub use wait_state::{
    delete_wait_state, get_wait_state, init_wait_states_table, list_pollable_wait_states,
    list_wait_states, update_wait_state_after_poll, upsert_wait_state, WaitKind, WaitStateRecord,
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
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| {
            checkpoint::PersistenceError::Database(format!(
                "Failed to set database busy timeout: {e}"
            ))
        })?;

    // Initialize checkpoint table
    checkpoint::init_checkpoint_table(&conn).map_err(|e| {
        checkpoint::PersistenceError::Database(format!("Failed to initialize schema: {}", e))
    })?;

    // Initialize issue-lease table (daemon discovery/claiming).
    // @plan:PLAN-20260415-DAEMON-DISCOVERY.P02
    leases::init_leases_table(&conn).map_err(|e| {
        checkpoint::PersistenceError::Database(format!("Failed to initialize leases schema: {}", e))
    })?;

    wait_state::init_wait_states_table(&conn).map_err(|e| {
        checkpoint::PersistenceError::Database(format!(
            "Failed to initialize wait-state schema: {}",
            e
        ))
    })?;

    Ok(())
}
