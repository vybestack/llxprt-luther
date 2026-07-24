/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
/// Persistence module - durable storage for run metadata and state.
pub mod artifacts;
pub mod attempts;
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P06
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08B
pub mod capsule_store;
pub mod checkpoint;
pub(crate) mod claim_metadata;
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P03
pub mod effect_intents;
/// Exact launch provenance: durable canonical serialization + SHA-256 digest
/// of the resolved workflow type/config at launch time.
/// @plan:PLAN-20260722-ISSUE158-LAUNCH-PROVENANCE
pub mod launch_provenance;
pub mod leases;
/// Durable state machine table for recoverable legacy ownership migration.
/// @plan:PLAN-20260722-ISSUE158-LEGACY-OWNERSHIP-MIGRATION
pub mod legacy_migration_state;
pub mod recovery_epoch;
pub mod recovery_operations;
pub mod run_metadata;
pub mod sqlite;
pub mod trace;
pub mod wait_state;

pub use artifacts::{
    default_artifacts_root, get_artifacts_dir, list_artifacts, read_artifact, write_artifact,
    write_poll_result_artifact, write_resume_decision_artifact, write_wait_state_artifact,
    ArtifactRecord,
};
pub use capsule_store::{
    init_capsules_table, load_capsule_v1, persist_capsule_v1, persist_launch_atomically,
    LaunchPersistenceError, LaunchPersistenceOutcome, EXECUTION_CAPSULES_TABLE,
};
pub use checkpoint::{
    append_event, append_event_with_conn, append_typed_event_with_conn, count_events_by_type,
    get_checkpoint_for_step, is_resumable_checkpoint_status, list_checkpoints, load_checkpoint,
    load_checkpoint_before_step, load_checkpoint_with_conn, load_events, load_events_by_type,
    load_latest_event, load_recent_events, save_checkpoint, save_checkpoint_with_conn,
    set_resume_point, Checkpoint, EventRecord, EventType, PersistenceError, StateSnapshot,
    CHECKPOINT_STATUS_INTERRUPTED, CHECKPOINT_STATUS_READY_TO_RESUME, CHECKPOINT_STATUS_WAITING,
};
pub use launch_provenance::{
    compute_config_digest, compute_workflow_digest, decode_config_root, encode_config_root,
    verify_provenance, LaunchProvenance, LaunchProvenanceError, LegacyAllowed, MigrationSource,
    ProvenanceVerification, LAUNCH_PROVENANCE_SCHEMA_VERSION,
};
pub use leases::{
    count_active_leases, count_active_leases_for_config, count_active_leases_for_repository,
    create_lease, get_lease_for_issue, get_lease_for_run, init_leases_table, list_all_leases,
    list_leases_by_config, list_leases_by_status, list_ready_to_resume_leases, mark_stale_leases,
    mark_stale_ready_to_resume_leases, touch_owned_running_lease_heartbeat, try_claim,
    update_lease_status, update_lease_status_conditional,
    update_lease_status_conditional_exact_owner, IssueLease, LeaseStatus,
};
pub use legacy_migration_state::{
    migration_is_durable_completed, migration_is_pending, MigrationStateRow, MigrationStatus,
};
pub use run_metadata::{
    is_pid_stale, run_metadata_from_ref, FailureCleanupState, RunMetadata, RunStatus,
};
pub use sqlite::{
    get_run_with_conn, insert_initial_run_with_conn, list_runs_by_ids_with_conn,
    list_runs_with_conn, persist_run_status_conditional_outcome_with_conn, persist_run_with_conn,
    ConditionalStatusOutcome, InitialRunInsert, SqliteStore, SqliteStoreRef,
};
pub use trace::{
    export_trace, load_trace, save_trace, SmokeTrace, TraceEvent, TraceOutcome, SCHEMA_VERSION,
};
pub use wait_state::{
    delete_wait_state, delete_wait_state_for_suspension, get_wait_state,
    has_pollable_external_wait, init_wait_states_table, list_pollable_wait_states,
    list_wait_states, persist_external_wait, update_wait_state_after_poll, upsert_wait_state,
    ExternalWaitError, WaitKind, WaitStateRecord, WaitStateWriteError,
};

use rusqlite::Connection;
use std::path::Path;

/// Initialize the checkpoint database at the given path.
/// Creates the database directory if needed and initializes the schema.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P03
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

    // An immediate transaction serializes schema inspection and DDL across
    // concurrently starting daemons. The busy timeout above lets later
    // initializers wait for the migration owner rather than fail on contention.
    // This connection was opened above and has not been shared, so no transaction
    // can already be active. `new_unchecked` is a safe shared-reference API;
    // SQLite returns an error if a future refactor attempts a nested BEGIN.
    let tx = rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| {
            checkpoint::PersistenceError::Database(format!(
                "Failed to begin database initialization transaction: {e}"
            ))
        })?;

    checkpoint::init_checkpoint_table(&tx).map_err(|e| {
        checkpoint::PersistenceError::Database(format!("Failed to initialize schema: {e}"))
    })?;
    sqlite::init_runs_schema(&tx).map_err(|e| {
        checkpoint::PersistenceError::Database(format!("Failed to initialize runs schema: {e}"))
    })?;

    // Initialize issue-lease table (daemon discovery/claiming).
    // @plan:PLAN-20260415-DAEMON-DISCOVERY.P02
    leases::init_leases_table(&tx).map_err(|e| {
        checkpoint::PersistenceError::Database(format!("Failed to initialize leases schema: {e}"))
    })?;
    claim_metadata::init_claim_metadata_table(&tx).map_err(|e| {
        checkpoint::PersistenceError::Database(format!(
            "Failed to initialize claim metadata schema: {e}"
        ))
    })?;

    wait_state::init_wait_states_table(&tx).map_err(|e| {
        checkpoint::PersistenceError::Database(format!(
            "Failed to initialize wait-state schema: {e}"
        ))
    })?;

    // Initialize durable legacy-ownership-migration state table (issue 158).
    // @plan:PLAN-20260722-ISSUE158-LEGACY-OWNERSHIP-MIGRATION
    legacy_migration_state::init_legacy_migration_table(&tx).map_err(|e| {
        checkpoint::PersistenceError::Database(format!(
            "Failed to initialize legacy migration state schema: {e}"
        ))
    })?;

    // Initialize durable recovery epoch, operations ledger, append-only
    // attempts, and effect-intents tables (self-hosting reliability, M1).
    // @plan:PLAN-20260723-SELFHOST-RELIABILITY.P03
    // @requirement:REQ-RP-003,REQ-RP-004,REQ-RP-008
    recovery_epoch::init_epoch_table(&tx).map_err(|e| {
        checkpoint::PersistenceError::Database(format!(
            "Failed to initialize recovery epoch schema: {e}"
        ))
    })?;
    recovery_operations::init_operations_table(&tx).map_err(|e| {
        checkpoint::PersistenceError::Database(format!(
            "Failed to initialize recovery operations schema: {e}"
        ))
    })?;
    attempts::init_attempts_table(&tx).map_err(|e| {
        checkpoint::PersistenceError::Database(format!(
            "Failed to initialize recovery attempts schema: {e}"
        ))
    })?;
    effect_intents::init_effect_intents_table(&tx).map_err(|e| {
        checkpoint::PersistenceError::Database(format!(
            "Failed to initialize effect intents schema: {e}"
        ))
    })?;

    // Initialize the immutable execution capsule store (self-hosting
    // reliability, M2). [C8]
    // @plan:PLAN-20260723-SELFHOST-RELIABILITY.P06
    // @requirement:REQ-RP-002
    capsule_store::init_capsules_table(&tx).map_err(|e| {
        checkpoint::PersistenceError::Database(format!(
            "Failed to initialize execution capsules schema: {e}"
        ))
    })?;

    // Initialize the immutable salvage lineage table (self-hosting
    // reliability, P15). [C9/B10]
    // @plan:PLAN-20260723-SELFHOST-RELIABILITY.P15
    // @requirement:REQ-RP-007
    crate::engine::recovery::salvage::init_salvage_lineage_table(&tx).map_err(|e| {
        checkpoint::PersistenceError::Database(format!(
            "Failed to initialize salvage lineage schema: {e}"
        ))
    })?;

    // Initialize the immutable merge artifacts table (self-hosting
    // reliability, P17). [B12]
    // @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
    // @requirement:REQ-RP-010
    crate::engine::recovery::typed_merge::init_merge_artifacts_table(&tx).map_err(|e| {
        checkpoint::PersistenceError::Database(format!(
            "Failed to initialize merge artifacts schema: {e}"
        ))
    })?;

    tx.commit().map_err(|e| {
        checkpoint::PersistenceError::Database(format!(
            "Failed to commit database initialization transaction: {e}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn concurrent_initializers_serialize_legacy_wait_state_migration() {
        let temp = tempfile::tempdir().expect("create database directory");
        let db_path = temp.path().join("state.db");
        let seed = Connection::open(&db_path).expect("open legacy database");
        seed.execute_batch(
            "CREATE TABLE wait_states (
                run_id TEXT PRIMARY KEY, lease_id TEXT, workflow_type TEXT NOT NULL,
                config_id TEXT NOT NULL, repository TEXT NOT NULL, issue_number INTEGER NOT NULL,
                pr_number INTEGER, head_sha TEXT, wait_kind TEXT NOT NULL,
                wait_condition_json TEXT NOT NULL, last_observed_state_json TEXT NOT NULL,
                next_poll_at TEXT NOT NULL, poll_interval_seconds INTEGER NOT NULL,
                max_wait_seconds INTEGER, resume_step TEXT NOT NULL, checkpoint_id TEXT NOT NULL,
                poll_count INTEGER NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL
             );
             INSERT INTO wait_states VALUES (
                'legacy-run', 'legacy-lease', 'wf', 'cfg', 'o/r', 1, NULL, NULL,
                'pr_checks', 'null', 'null', '2026-01-01T00:00:00Z', 30, NULL,
                'watch', 'checkpoint', 0, '2026-01-01T00:00:00Z',
                '2026-01-01T00:00:00Z'
             );",
        )
        .expect("seed legacy wait_states schema");
        drop(seed);

        let initializer_count = 8;
        let barrier = Arc::new(Barrier::new(initializer_count));
        let mut threads = Vec::with_capacity(initializer_count);
        for _ in 0..initializer_count {
            let db_path = db_path.clone();
            let barrier = Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                init_database(&db_path)
            }));
        }
        for thread in threads {
            thread
                .join()
                .expect("initializer thread must not panic")
                .expect("concurrent initialization must succeed");
        }

        let conn = Connection::open(&db_path).expect("reopen migrated database");
        let (suspension_id, lease_id, wait_kind, resume_step, checkpoint_id): (
            String,
            String,
            String,
            String,
            String,
        ) = conn
            .query_row(
                "SELECT suspension_id, lease_id, wait_kind, resume_step, checkpoint_id
                 FROM wait_states WHERE run_id = 'legacy-run'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("read migrated legacy wait state");
        assert_eq!(lease_id, "legacy-lease");
        assert_eq!(wait_kind, "pr_checks");
        assert_eq!(resume_step, "watch");
        assert_eq!(checkpoint_id, "checkpoint");
        assert!(!suspension_id.is_empty());
        let parsed_suspension_id =
            uuid::Uuid::parse_str(&suspension_id).expect("migration must generate a valid UUID");
        assert_eq!(parsed_suspension_id.simple().to_string(), suspension_id);
    }
}
