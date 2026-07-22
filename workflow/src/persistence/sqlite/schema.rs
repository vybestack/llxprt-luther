//! Schema initialization and migration for the `runs` table.
//!
//! Extracted from the main sqlite module to keep it under the source-size
//! budget. Contains the `CREATE TABLE` DDL, the legacy-column migration
//! dispatch, and the serialized (immediate-transaction) initializer used to
//! avoid concurrent-migration races.
use rusqlite::{Connection, Result as SqliteResult};

use super::super::run_metadata::migrate_runs_table;

/// SQL column list shared by the runs SELECT statements (must match query order).
pub(super) const RUN_SELECT_COLUMNS: &str =
    "run_id, workflow_type_id, config_id, status, created_at, updated_at, current_step, \
     previous_step, previous_outcome, next_step_candidates, log_path, artifact_root, \
     workspace_path, repository, issue_number, pr_number, head_sha, process_pid, child_pids, \
     continuation_rearm_checkpoint_id, failure_cleanup, launch_provenance";

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
            continuation_rearm_checkpoint_id TEXT,
            failure_cleanup TEXT,
            launch_provenance TEXT
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
