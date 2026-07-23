/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
/// SQLite persistence layer for run metadata.
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, Result as SqliteResult};

use super::run_metadata::{RunMetadata, RunStatus};

mod conditional_status;
mod row_parsing;
mod schema;

pub(crate) use conditional_status::{
    persist_run_status_conditional_outcome_in_transaction,
    persist_run_status_from_expected_in_transaction, ExpectedRunStatusOutcome,
};
pub use conditional_status::{
    persist_run_status_conditional_outcome_with_conn, ConditionalStatusOutcome,
};
pub use row_parsing::{get_run_with_conn, list_runs_by_ids_with_conn, list_runs_with_conn};
pub use schema::{init_runs_schema, init_runs_schema_serialized};

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
        super::leases::init_leases_table(&self.conn)?;
        // Durable legacy-ownership-migration state table (issue 158).
        // @plan:PLAN-20260722-ISSUE158-LEGACY-OWNERSHIP-MIGRATION
        super::legacy_migration_state::init_legacy_migration_table(&self.conn)
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

/// Persist a run record (insert or update) using a borrowed connection.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub fn persist_run_with_conn(conn: &Connection, metadata: &RunMetadata) -> SqliteResult<()> {
    conn.execute(
        "INSERT INTO runs (run_id, workflow_type_id, config_id, status, created_at, updated_at, \
            current_step, previous_step, previous_outcome, next_step_candidates, log_path, \
            artifact_root, workspace_path, repository, issue_number, pr_number, head_sha, \
            process_pid, child_pids, continuation_rearm_checkpoint_id, failure_cleanup, \
            launch_provenance)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, \
            ?18, ?19, ?20, ?21, ?22)
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
            continuation_rearm_checkpoint_id = excluded.continuation_rearm_checkpoint_id,
            failure_cleanup = excluded.failure_cleanup,
            launch_provenance = excluded.launch_provenance",
        rusqlite::params_from_iter(row_parsing::bind_run_metadata_params(metadata)?),
    )?;
    Ok(())
}

/// Outcome of an atomic initial-run insert.
///
/// [`InitialRunInsert::Inserted`] is the success path. The variants let the
/// caller distinguish a collision (a row already exists for `run_id`) from a
/// generic DB error so the launch surface can fail closed with a precise
/// diagnostic rather than silently overwriting the existing row.
///
/// @plan:PLAN-20260722-ISSUE158-LAUNCH-PERSISTENCE
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitialRunInsert {
    /// The initial `Starting` row was atomically inserted.
    Inserted,
    /// A row already exists for `run_id`. The launch must fail closed; the
    /// existing row is left untouched.
    Collision,
}

/// Atomically insert the **initial** `Starting` run metadata row, failing
/// closed on a `run_id` collision rather than overwriting the existing row.
///
/// Unlike [`persist_run_with_conn`] (which upserts), this uses `INSERT OR FAIL`
/// so a concurrent or stale writer that already persisted a row for the same
/// `run_id` surfaces as [`InitialRunInsert::Collision`] and the existing row is
/// preserved. A new launch that cannot atomically claim its `run_id` in the
/// registry must fail closed: proceeding would silently overwrite a prior run's
/// `created_at`, history, and provenance, violating the launch-persistence
/// invariant that the initial `Starting` row is the authoritative launch
/// record.
///
/// @plan:PLAN-20260722-ISSUE158-LAUNCH-PERSISTENCE
pub fn insert_initial_run_with_conn(
    conn: &Connection,
    metadata: &RunMetadata,
) -> SqliteResult<InitialRunInsert> {
    conn.execute(
        "INSERT OR FAIL INTO runs (run_id, workflow_type_id, config_id, status, created_at, \
            updated_at, current_step, previous_step, previous_outcome, next_step_candidates, \
            log_path, artifact_root, workspace_path, repository, issue_number, pr_number, \
            head_sha, process_pid, child_pids, continuation_rearm_checkpoint_id, \
            failure_cleanup, launch_provenance)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, \
            ?18, ?19, ?20, ?21, ?22)",
        rusqlite::params_from_iter(row_parsing::bind_run_metadata_params(metadata)?),
    )
    .map(|_| InitialRunInsert::Inserted)
    .or_else(classify_collision)
}

/// Map a primary-key constraint violation to [`InitialRunInsert::Collision`],
/// propagating all other errors. Used by [`insert_initial_run_with_conn`] to
/// distinguish a run_id collision from a generic DB error.
fn classify_collision(error: rusqlite::Error) -> SqliteResult<InitialRunInsert> {
    if let Some(rusqlite::ffi::ErrorCode::ConstraintViolation) = error.sqlite_error_code() {
        Ok(InitialRunInsert::Collision)
    } else {
        Err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    use super::conditional_status::persist_run_status_conditional_with_conn;

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
                row_parsing::get_run_status_with_conn(&c, "run-invalid-status")
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
