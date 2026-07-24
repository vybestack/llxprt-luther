//! Append-only recovery attempt rows with complete `StateSnapshot`.
//!
//! Each recovery attempt is an **immutable, append-only** row carrying the
//! complete [`StateSnapshot`], immutable IDs, step status, capsule binding,
//! and snapshot/checkpoint digests. No row is ever updated except the guarded
//! outcome-append that completes a row inserted at reserve. History is
//! preserved. [C3]
//!
//! A durable `execution_attempt_id` is allocated at reserve (before any
//! effect) and recorded via [`record_attempt_start`]. The outcome snapshot is
//! appended at finalize via [`append_attempt_outcome`]. If the process crashes
//! between execute and finalize, the durable runner-result record is
//! recoverable via [`load_unfinalized_for_operation`] so a reconciler can
//! detect that execution completed without a finalized outcome. [B4]
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
//! @requirement:REQ-RP-003

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Result as SqliteResult};
use sha2::{Digest, Sha256};

use crate::persistence::checkpoint::StateSnapshot;

/// Table name for the append-only recovery attempt rows. [C3/B4]
pub const RECOVERY_ATTEMPTS_TABLE: &str = "recovery_attempts";

/// A row in the append-only recovery attempts table.
///
/// Carries the complete [`StateSnapshot`], immutable IDs, capsule binding, and
/// snapshot/checkpoint digests. [C3/B4]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-003
#[derive(Debug, Clone, PartialEq)]
pub struct AttemptRow {
    /// Monotonic append-only primary key (`AUTOINCREMENT`).
    pub attempt_id: i64,
    /// The run this attempt is for.
    pub run_id: String,
    /// The epoch at which this attempt was reserved.
    pub epoch: u64,
    /// Parent attempt this recovers from (`None` for a fresh run). [C3]
    pub source_attempt_id: Option<i64>,
    /// Bound to the recovery operation. [B4]
    pub operation_id: String,
    /// The step this attempt targets.
    pub step_id: String,
    /// `'started'|'resumed'|'interrupted'|'completed'|'failed'`. [B4]
    pub step_status: String,
    /// Capsule schema version (exact capsule binding). [C3/C8]
    pub capsule_schema_version: u32,
    /// Exact capsule binding (envelope digest). [C3/C8]
    pub capsule_envelope_digest: String,
    /// Complete workflow state at this attempt. [C3]
    pub state_snapshot: StateSnapshot,
    /// The raw canonical JSON bytes persisted for `state_snapshot`. [C3]
    ///
    /// Kept so that [`verify_snapshot_digest`] can recompute the SHA-256 over
    /// the exact stored bytes rather than re-serializing the deserialized
    /// `StateSnapshot` (whose `HashMap` iteration order may differ).
    pub state_snapshot_json: Vec<u8>,
    /// SHA-256 of the canonical `state_snapshot` JSON. [C3]
    pub snapshot_digest: String,
    /// Digest of a referenced checkpoint (`None` if none). [C3]
    pub checkpoint_digest: Option<String>,
    /// Durable runner result (recoverable after a crash). [B4]
    pub runner_result_json: Option<serde_json::Value>,
    /// Attempt-start timestamp. [B4]
    pub started_at: DateTime<Utc>,
    /// `None` until the outcome is appended. [B4]
    pub finalized_at: Option<DateTime<Utc>>,
}

/// Initialize the durable append-only attempts table (idempotent). [C3/B4]
///
/// DDL (attempts pseudocode lines 02–17):
/// ```text
/// CREATE TABLE IF NOT EXISTS recovery_attempts (
///   attempt_id INTEGER PRIMARY KEY AUTOINCREMENT,
///   run_id TEXT NOT NULL,
///   epoch INTEGER NOT NULL,
///   source_attempt_id INTEGER,               -- parent attempt [C3]
///   operation_id TEXT NOT NULL,              -- bound to recovery operation [B4]
///   step_id TEXT NOT NULL,
///   step_status TEXT NOT NULL,               -- 'started'|...|'failed' [B4]
///   capsule_schema_version INTEGER NOT NULL,  -- [C3/C8]
///   capsule_envelope_digest TEXT NOT NULL,    -- exact capsule binding [C3/C8]
///   state_snapshot_json TEXT NOT NULL,        -- complete StateSnapshot [C3]
///   snapshot_digest TEXT NOT NULL,            -- SHA-256 of canonical state JSON [C3]
///   checkpoint_digest TEXT,                   -- nullable [C3]
///   runner_result_json TEXT,                  -- durable runner result [B4]
///   started_at TEXT NOT NULL,                 -- attempt-start timestamp [B4]
///   finalized_at TEXT                         -- NULL until outcome appended [B4]
/// )
/// ```
///
/// The table is append-only by design: `AUTOINCREMENT` PK guarantees strictly
/// increasing attempt ids and no row reuse.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-003
pub fn init_attempts_table(conn: &Connection) -> SqliteResult<()> {
    conn.execute(
        &format!(
            "CREATE TABLE IF NOT EXISTS {RECOVERY_ATTEMPTS_TABLE} (
                attempt_id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL,
                epoch INTEGER NOT NULL,
                source_attempt_id INTEGER,
                operation_id TEXT NOT NULL,
                step_id TEXT NOT NULL,
                step_status TEXT NOT NULL,
                capsule_schema_version INTEGER NOT NULL,
                capsule_envelope_digest TEXT NOT NULL,
                state_snapshot_json TEXT NOT NULL,
                snapshot_digest TEXT NOT NULL,
                checkpoint_digest TEXT,
                runner_result_json TEXT,
                started_at TEXT NOT NULL,
                finalized_at TEXT
            )"
        ),
        [],
    )?;
    Ok(())
}

/// Values required to reserve an execution attempt.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-003
#[derive(Debug, Clone)]
pub struct AttemptStart<'a> {
    /// The run this attempt is for.
    pub run_id: &'a str,
    /// The epoch at which this attempt was reserved.
    pub epoch: u64,
    /// Parent attempt this recovers from (`None` for a fresh run). [C3]
    pub source_attempt_id: Option<i64>,
    /// Bound to the recovery operation. [B4]
    pub operation_id: &'a str,
    /// The step this attempt targets.
    pub step_id: &'a str,
    /// Capsule schema version (exact capsule binding). [C3/C8]
    pub capsule_schema_version: u32,
    /// Exact capsule binding (envelope digest). [C3/C8]
    pub capsule_envelope_digest: &'a str,
    /// Complete workflow state at this attempt. [C3]
    pub state_snapshot: &'a StateSnapshot,
}

/// Compute the lowercase-hex SHA-256 digest of a byte slice.
fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// Canonicalize a [`StateSnapshot`] into deterministic JSON bytes.
///
/// Converting through `serde_json::Value` places object members in
/// `serde_json::Map`'s deterministic key order, including the snapshot's
/// `HashMap` fields. The persisted bytes are therefore stable across processes.
fn canonical_snapshot_bytes(state_snapshot: &StateSnapshot) -> SqliteResult<Vec<u8>> {
    let value = serde_json::to_value(state_snapshot).map_err(|e| {
        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("failed to canonicalize StateSnapshot: {e}"),
        )))
    })?;
    serde_json::to_vec(&value).map_err(|e| {
        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("failed to serialize StateSnapshot: {e}"),
        )))
    })
}

/// Record the attempt **start** at reserve, before any effect. [B4]
///
/// Allocates the durable `execution_attempt_id` and writes a `'started'` row
/// with `finalized_at = NULL`. This row proves the attempt was reserved even
/// if the process crashes before finalize (attempts pseudocode lines 23–42).
///
/// The snapshot is canonicalized to deterministic JSON and its SHA-256 digest
/// is stored alongside it so integrity can be verified on read. [C3]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-003
pub fn record_attempt_start(tx: &Connection, attempt: &AttemptStart<'_>) -> SqliteResult<i64> {
    let snapshot_json = canonical_snapshot_bytes(attempt.state_snapshot)?;
    let snapshot_digest = sha256_hex(&snapshot_json);
    let snapshot_str = String::from_utf8(snapshot_json).map_err(|e| {
        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("StateSnapshot JSON is not utf8: {e}"),
        )))
    })?;
    let now = Utc::now().to_rfc3339();
    let epoch_i64 = i64::try_from(attempt.epoch).unwrap_or(i64::MAX);

    let attempt_id: i64 = tx.query_row(
        &format!(
            "INSERT INTO {RECOVERY_ATTEMPTS_TABLE}
               (run_id, epoch, source_attempt_id, operation_id, step_id, step_status,
                capsule_schema_version, capsule_envelope_digest,
                state_snapshot_json, snapshot_digest, checkpoint_digest,
                runner_result_json, started_at, finalized_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, NULL, ?11, NULL)
             RETURNING attempt_id"
        ),
        params![
            attempt.run_id,
            epoch_i64,
            attempt.source_attempt_id,
            attempt.operation_id,
            attempt.step_id,
            "started",
            i64::from(attempt.capsule_schema_version),
            attempt.capsule_envelope_digest,
            snapshot_str,
            snapshot_digest,
            now,
        ],
        |row| row.get(0),
    )?;
    Ok(attempt_id)
}

/// Append the immutable outcome snapshot at finalize. [B4]
///
/// This is the **only** non-append mutation on the attempts table: it completes
/// a row already inserted at reserve. It is guarded by `finalized_at IS NULL`
/// (attempts pseudocode lines 48–65).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-003
pub fn append_attempt_outcome(
    tx: &Connection,
    attempt_id: i64,
    step_status: &str,
    state_snapshot: &StateSnapshot,
    runner_result: Option<&serde_json::Value>,
    checkpoint_digest: Option<&str>,
) -> SqliteResult<()> {
    let snapshot_json = canonical_snapshot_bytes(state_snapshot)?;
    let snapshot_digest = sha256_hex(&snapshot_json);
    let snapshot_str = String::from_utf8(snapshot_json).map_err(|e| {
        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("StateSnapshot JSON is not utf8: {e}"),
        )))
    })?;
    let runner_json = runner_result.map(|v| v.to_string());
    let now = Utc::now().to_rfc3339();
    let affected = tx.execute(
        &format!(
            "UPDATE {RECOVERY_ATTEMPTS_TABLE}
             SET step_status = ?2,
                 state_snapshot_json = ?3,
                 snapshot_digest = ?4,
                 runner_result_json = ?5,
                 checkpoint_digest = ?6,
                 finalized_at = ?7
             WHERE attempt_id = ?1 AND finalized_at IS NULL"
        ),
        params![
            attempt_id,
            step_status,
            snapshot_str,
            snapshot_digest,
            runner_json,
            checkpoint_digest,
            now,
        ],
    )?;
    if affected != 1 {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some(format!(
                "append_attempt_outcome guard failed for attempt {attempt_id} (affected {affected})"
            )),
        ));
    }
    Ok(())
}

/// Load the latest attempt for a run+step (by monotonic `attempt_id`).
///
/// Attempts pseudocode lines 67–71.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-003
pub fn latest_for_step(
    conn: &Connection,
    run_id: &str,
    step_id: &str,
) -> SqliteResult<Option<AttemptRow>> {
    let row = conn
        .query_row(
            &format!(
                "SELECT attempt_id, run_id, epoch, source_attempt_id, operation_id, step_id,
                        step_status, capsule_schema_version, capsule_envelope_digest,
                        state_snapshot_json, snapshot_digest, checkpoint_digest,
                        runner_result_json, started_at, finalized_at
                 FROM {RECOVERY_ATTEMPTS_TABLE}
                 WHERE run_id = ?1 AND step_id = ?2 AND finalized_at IS NOT NULL
                 ORDER BY attempt_id DESC
                 LIMIT 1"
            ),
            params![run_id, step_id],
            map_attempt_row,
        )
        .optional()?;
    Ok(row)
}

/// Load a specific attempt by id.
///
/// Attempts pseudocode lines 74–76.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-003
pub fn load_attempt(conn: &Connection, attempt_id: i64) -> SqliteResult<AttemptRow> {
    conn.query_row(
        &format!(
            "SELECT attempt_id, run_id, epoch, source_attempt_id, operation_id, step_id,
                    step_status, capsule_schema_version, capsule_envelope_digest,
                    state_snapshot_json, snapshot_digest, checkpoint_digest,
                    runner_result_json, started_at, finalized_at
             FROM {RECOVERY_ATTEMPTS_TABLE}
             WHERE attempt_id = ?1"
        ),
        params![attempt_id],
        map_attempt_row,
    )
}

/// Load an attempt by `operation_id` that was started but never finalized. [B4]
///
/// Recoverable after an execute-before-finalize crash (attempts pseudocode
/// lines 80–84).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-003
pub fn load_unfinalized_for_operation(
    conn: &Connection,
    operation_id: &str,
) -> SqliteResult<Option<AttemptRow>> {
    let row = conn
        .query_row(
            &format!(
                "SELECT attempt_id, run_id, epoch, source_attempt_id, operation_id, step_id,
                        step_status, capsule_schema_version, capsule_envelope_digest,
                        state_snapshot_json, snapshot_digest, checkpoint_digest,
                        runner_result_json, started_at, finalized_at
                 FROM {RECOVERY_ATTEMPTS_TABLE}
                 WHERE operation_id = ?1 AND finalized_at IS NULL
                 ORDER BY attempt_id DESC
                 LIMIT 1"
            ),
            params![operation_id],
            map_attempt_row,
        )
        .optional()?;
    Ok(row)
}

/// Verify the snapshot digest of a loaded attempt row.
///
/// Recomputes the SHA-256 over the **exact stored JSON bytes** and compares
/// it to the stored digest (attempts pseudocode lines 87–93). Using the stored
/// bytes rather than re-serializing the deserialized `StateSnapshot` avoids
/// `HashMap` iteration-order divergence between insert and verify. [C3]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-003
pub fn verify_snapshot_digest(row: &AttemptRow) -> SqliteResult<()> {
    let recomputed = sha256_hex(&row.state_snapshot_json);
    if recomputed != row.snapshot_digest {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some(format!(
                "snapshot_digest mismatch for attempt {} (stored {}, recomputed {})",
                row.attempt_id, row.snapshot_digest, recomputed
            )),
        ));
    }
    Ok(())
}

/// Parse a `recovery_attempts` row into an [`AttemptRow`].
fn map_attempt_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AttemptRow> {
    let snapshot_json: String = row.get(9)?;
    let state_snapshot: StateSnapshot = serde_json::from_str(&snapshot_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            9,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("failed to deserialize StateSnapshot: {e}"),
            )),
        )
    })?;
    let runner_json_str: Option<String> = row.get(12)?;
    let runner_result_json = runner_json_str
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok());
    let started_at_str: String = row.get(13)?;
    let started_at = parse_rfc3339_utc(13, &started_at_str)?;
    let finalized_at_str: Option<String> = row.get(14)?;
    let finalized_at = finalized_at_str
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));
    let epoch_i64: i64 = row.get(2)?;
    let epoch = u64::try_from(epoch_i64).unwrap_or(0);
    let capsule_schema_version_i64: i64 = row.get(7)?;
    let capsule_schema_version = u32::try_from(capsule_schema_version_i64).unwrap_or(0);
    Ok(AttemptRow {
        attempt_id: row.get(0)?,
        run_id: row.get(1)?,
        epoch,
        source_attempt_id: row.get(3)?,
        operation_id: row.get(4)?,
        step_id: row.get(5)?,
        step_status: row.get(6)?,
        capsule_schema_version,
        capsule_envelope_digest: row.get(8)?,
        state_snapshot,
        state_snapshot_json: snapshot_json.into_bytes(),
        snapshot_digest: row.get(10)?,
        checkpoint_digest: row.get(11)?,
        runner_result_json,
        started_at,
        finalized_at,
    })
}

/// Parse an RFC 3339 timestamp into a UTC `DateTime`, mapping failure to a
/// SQL conversion error with the originating column index.
fn parse_rfc3339_utc(col: usize, value: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                col,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid RFC 3339 timestamp '{value}': {e}"),
                )),
            )
        })
}

/// Count durable recovery attempts for one run.
pub fn count_attempts_for_run(conn: &Connection, run_id: &str) -> SqliteResult<i64> {
    conn.query_row(
        &format!("SELECT COUNT(*) FROM {RECOVERY_ATTEMPTS_TABLE} WHERE run_id = ?1"),
        params![run_id],
        |row| row.get(0),
    )
}
