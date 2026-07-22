//! Durable state machine table for recoverable legacy ownership migration.
//!
//! The filesystem (marker publication) and the database (audit events,
//! provenance tag) cannot be updated atomically together. To make the
//! migration recoverable across crashes, this table records a durable intent
//! row **before** any filesystem mutation, and a durable completion row
//! **after** the marker is published and the audit/provenance are recorded.
//!
//! ## State machine
//!
//! ```text
//!   (absent) ──persist_intent──▶ pending ──publish+complete──▶ completed
//!                                   │
//!                                   └──crash──▶ reconcile: re-publish + complete
//! ```
//!
//! - [`MigrationStatus::Pending`]: intent persisted; marker not yet published
//!   or completion audit not yet recorded. A crash between `persist_intent`
//!   and `record_completion` leaves the row here.
//! - [`MigrationStatus::Completed`]: marker published, audit recorded, and
//!   synthetic provenance tagged. A retry/reconciliation observes this and
//!   produces no additional completion audit (exactly-once completion).
//!
//! ## Resume trust contract
//!
//! An ordinary resume trusts the migrated marker **only** when a durable
//! `completed` row exists for the run. A `pending` row with no completion
//! blocks the resume so a partially-published migration cannot be silently
//! trusted.
//!
//! @plan:PLAN-20260722-ISSUE158-LEGACY-OWNERSHIP-MIGRATION

use chrono::{DateTime, Utc};
use rusqlite::{Connection, Result as SqliteResult};

/// Table name for the durable migration state machine.
pub const LEGACY_MIGRATION_TABLE: &str = "legacy_ownership_migrations";

/// The durable status of a legacy ownership migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationStatus {
    /// Intent persisted; marker publication and/or completion not yet durable.
    Pending,
    /// Marker published, audit recorded, provenance tagged. Idempotent.
    Completed,
}

impl MigrationStatus {
    /// The string stored in the `status` column.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Completed => "completed",
        }
    }

    /// Parse a persisted status string, returning `None` for unknown values.
    #[must_use]
    pub fn parse_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "completed" => Some(Self::Completed),
            _ => None,
        }
    }
}

/// A row in the durable migration state table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationStateRow {
    /// The run id this migration is for.
    pub run_id: String,
    /// The persisted workspace path that was verified at intent time.
    pub workspace_path: String,
    /// When the intent was first persisted.
    pub intent_at: DateTime<Utc>,
    /// When the completion was recorded, if completed.
    pub completed_at: Option<DateTime<Utc>>,
    /// The durable status.
    pub status: MigrationStatus,
}

/// Initialize the durable migration state table (idempotent).
pub fn init_legacy_migration_table(conn: &Connection) -> SqliteResult<()> {
    conn.execute(
        &format!(
            "CREATE TABLE IF NOT EXISTS {LEGACY_MIGRATION_TABLE} (
                run_id TEXT PRIMARY KEY,
                workspace_path TEXT NOT NULL,
                intent_at TEXT NOT NULL,
                completed_at TEXT,
                status TEXT NOT NULL
            )"
        ),
        [],
    )?;
    Ok(())
}

/// Persist a durable intent row for a migration.
///
/// Uses `INSERT OR IGNORE` so a re-invocation (retry) does not overwrite an
/// existing intent: the original `intent_at` and `workspace_path` are
/// preserved. Returns whether a new row was inserted.
pub fn persist_migration_intent(
    conn: &Connection,
    run_id: &str,
    workspace_path: &str,
    now: DateTime<Utc>,
) -> SqliteResult<bool> {
    let inserted = conn.execute(
        &format!(
            "INSERT OR IGNORE INTO {LEGACY_MIGRATION_TABLE}
             (run_id, workspace_path, intent_at, completed_at, status)
             VALUES (?1, ?2, ?3, NULL, ?4)"
        ),
        rusqlite::params![
            run_id,
            workspace_path,
            now.to_rfc3339(),
            MigrationStatus::Pending.as_str()
        ],
    )?;
    Ok(inserted > 0)
}

/// Record a durable completion for a migration.
///
/// Updates the row to `completed` with a `completed_at` timestamp. This is
/// the exactly-once completion point: after this succeeds, the migration is
/// durable and a retry will not produce another completion audit.
pub fn record_migration_completion(
    conn: &Connection,
    run_id: &str,
    now: DateTime<Utc>,
) -> SqliteResult<()> {
    conn.execute(
        &format!(
            "UPDATE {LEGACY_MIGRATION_TABLE}
             SET status = ?1, completed_at = ?2
             WHERE run_id = ?3"
        ),
        rusqlite::params![
            MigrationStatus::Completed.as_str(),
            now.to_rfc3339(),
            run_id
        ],
    )?;
    Ok(())
}

/// Outcome of a guarded `pending → completed` transition.
///
/// This is the exactly-once completion guard. The transition succeeds only
/// when the row exists and its current status is `pending`; otherwise it
/// reports the reason the transition was not applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardedCompletionOutcome {
    /// The row was in `pending` status and was atomically transitioned to
    /// `completed`.
    Transitioned,
    /// The row was already `completed`. The caller treats this as an
    /// idempotent no-op (no additional completion audit).
    AlreadyCompleted,
    /// No row exists for `run_id`. This is an integrity failure because
    /// [`persist_migration_intent`] must run before completion.
    Missing,
}

/// Atomically transition a migration row from `pending` to `completed` using
/// a guarded conditional `UPDATE`.
///
/// The guard (`WHERE run_id = ? AND status = 'pending'`) ensures that, under
/// concurrent connections, exactly one writer observes the `pending` row and
/// flips it to `completed`; every other concurrent writer sees zero affected
/// rows and the row is already `completed` (or missing). This is the
/// exactly-once completion point across concurrent connections.
///
/// Designed to run **inside** a caller-owned transaction so the guarded
/// completion, the completion audit event, and the provenance tag commit
/// atomically as a single unit. The caller is responsible for beginning and
/// committing/rolling back the transaction.
pub fn guarded_complete_migration_in_transaction(
    conn: &Connection,
    run_id: &str,
    now: DateTime<Utc>,
) -> SqliteResult<GuardedCompletionOutcome> {
    let updated = conn.execute(
        &format!(
            "UPDATE {LEGACY_MIGRATION_TABLE}
             SET status = ?1, completed_at = ?2
             WHERE run_id = ?3 AND status = ?4"
        ),
        rusqlite::params![
            MigrationStatus::Completed.as_str(),
            now.to_rfc3339(),
            run_id,
            MigrationStatus::Pending.as_str(),
        ],
    )?;
    if updated > 0 {
        return Ok(GuardedCompletionOutcome::Transitioned);
    }
    match load_migration_state(conn, run_id)? {
        Some(MigrationStateRow {
            status: MigrationStatus::Completed,
            ..
        }) => Ok(GuardedCompletionOutcome::AlreadyCompleted),
        _ => Ok(GuardedCompletionOutcome::Missing),
    }
}

/// Load the durable migration state for a run, if any.
pub fn load_migration_state(
    conn: &Connection,
    run_id: &str,
) -> SqliteResult<Option<MigrationStateRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT run_id, workspace_path, intent_at, completed_at, status
         FROM {LEGACY_MIGRATION_TABLE} WHERE run_id = ?1"
    ))?;
    let row = stmt.query_row(rusqlite::params![run_id], |row| {
        let status_str: String = row.get(4)?;
        let status = MigrationStatus::parse_str(&status_str).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("unknown migration status '{status_str}'"),
                )),
            )
        })?;
        let intent_at_str: String = row.get(2)?;
        let intent_at = DateTime::parse_from_rfc3339(&intent_at_str)
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
                )
            })?
            .with_timezone(&Utc);
        let completed_at_str: Option<String> = row.get(3)?;
        let completed_at = completed_at_str
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&Utc));
        Ok(MigrationStateRow {
            run_id: row.get(0)?,
            workspace_path: row.get(1)?,
            intent_at,
            completed_at,
            status,
        })
    });
    match row {
        Ok(state) => Ok(Some(state)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Whether a durable completed migration exists for the given run.
///
/// This is the trust anchor for ordinary resume: the resume trusts the
/// migrated marker only when this returns `true`. A `pending` row does not
/// satisfy this contract.
#[must_use]
pub fn migration_is_durable_completed(conn: &Connection, run_id: &str) -> bool {
    matches!(
        load_migration_state(conn, run_id),
        Ok(Some(MigrationStateRow {
            status: MigrationStatus::Completed,
            ..
        }))
    )
}

/// Whether a durable pending (incomplete) migration exists for the given run.
///
/// A `pending` row signals that a legacy ownership migration was started but
/// did not reach its durable completion point. Resume authorization must
/// reject a run while this returns `true`: a partially-completed migration
/// may have published the marker but not recorded the completion audit, so the
/// resume trust contract (completed migration ⇒ durable trust) is violated.
/// The operator must re-run `migrate-legacy-ownership` to complete the
/// transition before resuming.
#[must_use]
pub fn migration_is_pending(conn: &Connection, run_id: &str) -> bool {
    matches!(
        load_migration_state(conn, run_id),
        Ok(Some(MigrationStateRow {
            status: MigrationStatus::Pending,
            ..
        }))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        init_legacy_migration_table(&conn).expect("init table");
        conn
    }

    #[test]
    fn persist_intent_inserts_new_row() {
        let conn = test_conn();
        let now = Utc::now();
        let inserted = persist_migration_intent(&conn, "run-1", "/ws", now).expect("persist");
        assert!(inserted);
        let state = load_migration_state(&conn, "run-1")
            .expect("load")
            .expect("row exists");
        assert_eq!(state.status, MigrationStatus::Pending);
        assert_eq!(state.workspace_path, "/ws");
        assert!(state.completed_at.is_none());
    }

    #[test]
    fn persist_intent_is_idempotent_on_retry() {
        let conn = test_conn();
        let now = Utc::now();
        persist_migration_intent(&conn, "run-1", "/ws", now).expect("first");
        let inserted = persist_migration_intent(&conn, "run-1", "/ws", now).expect("retry");
        assert!(!inserted, "retry must not overwrite existing intent");
    }

    #[test]
    fn record_completion_sets_status_and_timestamp() {
        let conn = test_conn();
        let now = Utc::now();
        persist_migration_intent(&conn, "run-1", "/ws", now).expect("persist");
        let completed_at = Utc::now();
        record_migration_completion(&conn, "run-1", completed_at).expect("complete");
        let state = load_migration_state(&conn, "run-1")
            .expect("load")
            .expect("row exists");
        assert_eq!(state.status, MigrationStatus::Completed);
        assert!(state.completed_at.is_some());
    }

    #[test]
    fn durable_completed_returns_true_only_after_completion() {
        let conn = test_conn();
        let now = Utc::now();
        persist_migration_intent(&conn, "run-1", "/ws", now).expect("persist");
        assert!(!migration_is_durable_completed(&conn, "run-1"));
        record_migration_completion(&conn, "run-1", Utc::now()).expect("complete");
        assert!(migration_is_durable_completed(&conn, "run-1"));
    }

    #[test]
    fn durable_completed_returns_false_for_missing_run() {
        let conn = test_conn();
        assert!(!migration_is_durable_completed(&conn, "nope"));
    }

    #[test]
    fn init_table_is_idempotent() {
        let conn = Connection::open_in_memory().expect("open");
        init_legacy_migration_table(&conn).expect("first");
        init_legacy_migration_table(&conn).expect("second");
    }

    #[test]
    fn guarded_completion_transitions_pending_to_completed() {
        let conn = test_conn();
        let now = Utc::now();
        persist_migration_intent(&conn, "run-1", "/ws", now).expect("persist intent");
        let outcome =
            guarded_complete_migration_in_transaction(&conn, "run-1", Utc::now()).expect("guard");
        assert_eq!(outcome, GuardedCompletionOutcome::Transitioned);
        let state = load_migration_state(&conn, "run-1")
            .expect("load")
            .expect("row exists");
        assert_eq!(state.status, MigrationStatus::Completed);
        assert!(state.completed_at.is_some());
    }

    #[test]
    fn guarded_completion_already_completed_is_idempotent() {
        let conn = test_conn();
        let now = Utc::now();
        persist_migration_intent(&conn, "run-1", "/ws", now).expect("persist intent");
        guarded_complete_migration_in_transaction(&conn, "run-1", Utc::now()).expect("first");
        let outcome =
            guarded_complete_migration_in_transaction(&conn, "run-1", Utc::now()).expect("retry");
        assert_eq!(outcome, GuardedCompletionOutcome::AlreadyCompleted);
    }

    #[test]
    fn guarded_completion_missing_row_reports_missing() {
        let conn = test_conn();
        let outcome =
            guarded_complete_migration_in_transaction(&conn, "nope", Utc::now()).expect("guard");
        assert_eq!(outcome, GuardedCompletionOutcome::Missing);
    }
}
