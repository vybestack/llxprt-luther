//! Idempotent recovery operations ledger.
//!
//! Each recovery is a durable operation recorded in [`RECOVERY_OPERATIONS_TABLE`].
//! The ledger has a stable [`operation_id`](compute_operation_id) (durable row
//! identity) that binds the run, step, capsule envelope digest, source attempt,
//! and normalized operator intent, and a separate normalized
//! [`logical_request_key`](compute_logical_request_key) (uniqueness/conflict
//! binding) so one operation exists per logical request. [C2/B3]
//!
//! A `Pending` operation carries a guarded owner/lease claim (`owner_pid` +
//! `lease_expires_at`) so exactly one process may execute or reconcile. [B3]
//! A durable `execution_attempt_id` is allocated at reserve and recorded here
//! before any effect. [B4]
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
//! @requirement:REQ-RP-004

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Result as SqliteResult};
use sha2::{Digest, Sha256};

/// Table name for the idempotent recovery operations ledger. [C2/B3]
pub const RECOVERY_OPERATIONS_TABLE: &str = "recovery_operations";

/// The durable status of a recovery operation. [C2]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-004
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationStatus {
    /// Reserved but not yet finalized; carries a guarded owner/lease claim. [B3]
    Pending,
    /// Finalized with a serialized prior outcome. [C2]
    Completed,
    /// Finalized as refused. [C2]
    Refused,
    /// Finalized as a conflict (e.g. duplicate with mismatched binding). [C2/B3]
    Conflict,
}

impl OperationStatus {
    /// The string stored in the `status` column.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Completed => "completed",
            Self::Refused => "refused",
            Self::Conflict => "conflict",
        }
    }

    /// Parse a persisted status string, returning `None` for unknown values.
    #[must_use]
    pub fn parse_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "completed" => Some(Self::Completed),
            "refused" => Some(Self::Refused),
            "conflict" => Some(Self::Conflict),
            _ => None,
        }
    }
}

/// The outcome of attempting to adopt an expired-lease `Pending` operation. [B3]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-004
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdoptOutcome {
    /// The operation's lease was expired and ownership was claimed by the
    /// caller. [B3]
    Adopted,
    /// The operation's lease was still live (owned by another process); the
    /// caller must not execute or reconcile. [B3]
    StillOwned,
}

/// A row in the durable recovery operations ledger.
///
/// Binds the exact operation identity (`operation_id`), normalized logical
/// request (`logical_request_key`), capsule, source attempt, guarded owner
/// claim, and a durable `execution_attempt_id`. [C2/B3/B4]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-004
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryOperation {
    /// Durable row identity (PRIMARY KEY). [B3]
    pub operation_id: String,
    /// The run this operation is for.
    pub run_id: String,
    /// The epoch at which this operation was reserved.
    pub epoch: u64,
    /// The step this operation targets.
    pub step_id: String,
    /// Exact capsule binding (envelope digest). [C3/C8]
    pub capsule_envelope_digest: String,
    /// Exact source attempt (nullable for a fresh run). [C3]
    pub source_attempt_id: Option<i64>,
    /// Normalized logical request key (UNIQUE). [B3]
    pub logical_request_key: String,
    /// Normalized operator-intent binding digest. [B3]
    pub intent_digest: String,
    /// The durable status.
    pub status: OperationStatus,
    /// Guarded owner PID for a `Pending` claim. [B3]
    pub owner_pid: Option<u32>,
    /// Guarded lease expiry for a `Pending` claim. [B3]
    pub lease_expires_at: Option<DateTime<Utc>>,
    /// Durable execution attempt allocated at reserve. [B4]
    pub execution_attempt_id: Option<i64>,
    /// JSON of a prior outcome, set at finalization. [C2]
    pub serialized_outcome: Option<String>,
}

/// Values required to reserve a new pending recovery operation.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-004
#[derive(Debug, Clone)]
pub struct PendingOperationInsert {
    /// Durable row identity (PRIMARY KEY). [B3]
    pub operation_id: String,
    /// The run this operation is for.
    pub run_id: String,
    /// The epoch at which this operation was reserved.
    pub epoch: u64,
    /// The step this operation targets.
    pub step_id: String,
    /// Exact capsule binding (envelope digest). [C3/C8]
    pub capsule_envelope_digest: String,
    /// Exact source attempt (nullable for a fresh run). [C3]
    pub source_attempt_id: Option<i64>,
    /// Normalized logical request key (UNIQUE). [B3]
    pub logical_request_key: String,
    /// Normalized operator-intent binding digest. [B3]
    pub intent_digest: String,
    /// Guarded owner PID for a `Pending` claim. [B3]
    pub owner_pid: u32,
    /// Guarded lease expiry for a `Pending` claim. [B3]
    pub lease_expires_at: DateTime<Utc>,
    /// Durable execution attempt allocated at reserve. [B4]
    pub execution_attempt_id: i64,
}

/// Initialize the durable recovery operations ledger table (idempotent). [C2]
///
/// DDL (operations pseudocode lines 02–17):
/// ```text
/// CREATE TABLE IF NOT EXISTS recovery_operations (
///   operation_id TEXT PRIMARY KEY,             -- durable row identity [B3]
///   run_id TEXT NOT NULL,
///   epoch INTEGER NOT NULL,                    -- epoch at which this op was reserved
///   step_id TEXT NOT NULL,
///   capsule_envelope_digest TEXT NOT NULL,     -- exact capsule binding [C3/C8]
///   source_attempt_id INTEGER,                 -- exact source attempt (nullable for fresh)
///   logical_request_key TEXT NOT NULL UNIQUE,  -- one operation per logical request [B3]
///   intent_digest TEXT NOT NULL,               -- normalized operator intent binding
///   status TEXT NOT NULL DEFAULT 'pending',
///   owner_pid INTEGER,                         -- guarded owner claim for Pending [B3]
///   lease_expires_at TEXT,                     -- guarded lease claim for Pending [B3]
///   execution_attempt_id INTEGER,              -- allocated at reserve [B4]
///   serialized_outcome TEXT,                   -- JSON of prior outcome
///   created_at TEXT NOT NULL,
///   finalized_at TEXT
/// )
/// ```
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-004
pub fn init_operations_table(conn: &Connection) -> SqliteResult<()> {
    conn.execute(
        &format!(
            "CREATE TABLE IF NOT EXISTS {RECOVERY_OPERATIONS_TABLE} (
                operation_id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                epoch INTEGER NOT NULL,
                step_id TEXT NOT NULL,
                capsule_envelope_digest TEXT NOT NULL,
                source_attempt_id INTEGER,
                logical_request_key TEXT NOT NULL UNIQUE,
                intent_digest TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                owner_pid INTEGER,
                lease_expires_at TEXT,
                execution_attempt_id INTEGER,
                serialized_outcome TEXT,
                created_at TEXT NOT NULL,
                finalized_at TEXT
            )"
        ),
        [],
    )?;
    Ok(())
}

/// Compute the lowercase-hex SHA-256 digest of a byte slice.
fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// Append a length-prefixed (u64 big-endian) byte slice to the canonical buffer.
///
/// The length prefix prevents different field compositions from hashing to the
/// same digest (e.g. `"ab"+"c"` vs `"a"+"bc"`). Using a fixed-width u64 keeps
/// the canonical form stable across platforms and serializations.
fn push_len_prefixed(buf: &mut Vec<u8>, bytes: &[u8]) {
    let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(bytes);
}

/// Compute the exact `operation_id` (durable row identity). [B3]
///
/// Binds the run, step, capsule envelope digest, source attempt, and normalized
/// operator intent so that different operator verbs (or different capsules/
/// source attempts) cannot alias to the same row. This is the PRIMARY KEY and
/// prevents different operator verbs from aliasing (operations pseudocode lines
/// 22–27).
///
/// The canonical form is a length-prefixed concatenation of every binding
/// field, so two different bindings cannot collide by string concatenation
/// ambiguity.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-004
#[must_use]
pub fn compute_operation_id(
    run_id: &str,
    step_id: &str,
    capsule_envelope_digest: &str,
    source_attempt_id: Option<i64>,
    normalized_intent: &str,
) -> String {
    let mut buf = Vec::new();
    push_len_prefixed(&mut buf, run_id.as_bytes());
    push_len_prefixed(&mut buf, step_id.as_bytes());
    push_len_prefixed(&mut buf, capsule_envelope_digest.as_bytes());
    let source_bytes = match source_attempt_id {
        Some(id) => id.to_be_bytes(),
        None => [0u8; 8],
    };
    push_len_prefixed(&mut buf, &source_bytes);
    push_len_prefixed(&mut buf, normalized_intent.as_bytes());
    sha256_hex(&buf)
}

/// Compute the normalized `logical_request_key` (uniqueness/conflict binding).
/// [B3]
///
/// Separate from [`compute_operation_id`]: binds the normalized operator intent
/// and logical target (run + source attempt), independent of capsule/step
/// details. A second request for that target with different exact bindings is a
/// conflict. The `UNIQUE` constraint on `logical_request_key` makes the check
/// race-safe (operations pseudocode lines 35–40).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-004
#[must_use]
pub fn compute_logical_request_key(
    run_id: &str,
    source_attempt_id: Option<i64>,
    normalized_intent: &str,
) -> String {
    let mut buf = Vec::new();
    push_len_prefixed(&mut buf, run_id.as_bytes());
    let source_bytes = match source_attempt_id {
        Some(id) => id.to_be_bytes(),
        None => [0u8; 8],
    };
    push_len_prefixed(&mut buf, &source_bytes);
    push_len_prefixed(&mut buf, normalized_intent.as_bytes());
    sha256_hex(&buf)
}

/// Compute the `intent_digest` for a normalized operator intent. [B3]
///
/// The `intent_digest` is the SHA-256 of the normalized intent string, stored
/// in the `intent_digest` column of `recovery_operations`. It binds the
/// operator's normalized intent so reserve can compare exact bindings.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-004
#[must_use]
pub fn compute_intent_digest(normalized_intent: &str) -> String {
    sha256_hex(normalized_intent.as_bytes())
}

/// Look up the single operation for a logical request. [C2/B3]
///
/// Exact `operation_id`, intent digest, capsule, step, and source bindings are
/// compared by the protocol's reserve phase against the prepared authority
/// (operations pseudocode line 46).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-004
pub fn lookup_logical_operation(
    tx: &Connection,
    logical_request_key: &str,
) -> SqliteResult<Option<RecoveryOperation>> {
    let op = tx
        .query_row(
            &format!(
                "SELECT operation_id, run_id, epoch, step_id, capsule_envelope_digest,
                        source_attempt_id, logical_request_key, intent_digest, status,
                        owner_pid, lease_expires_at, execution_attempt_id, serialized_outcome
                 FROM {RECOVERY_OPERATIONS_TABLE}
                 WHERE logical_request_key = ?1"
            ),
            params![logical_request_key],
            map_operation_row,
        )
        .optional()?;
    Ok(op)
}

/// Find an adoptable `Pending` operation for a logical request with an expired
/// or missing lease, so a reconciler can adopt it. [B3]
///
/// Operations pseudocode lines 50–55.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-004
pub fn find_adoptable_pending(
    tx: &Connection,
    logical_request_key: &str,
    now: DateTime<Utc>,
) -> SqliteResult<Option<RecoveryOperation>> {
    let now_rfc = now.to_rfc3339();
    let op = tx
        .query_row(
            &format!(
                "SELECT operation_id, run_id, epoch, step_id, capsule_envelope_digest,
                        source_attempt_id, logical_request_key, intent_digest, status,
                        owner_pid, lease_expires_at, execution_attempt_id, serialized_outcome
                 FROM {RECOVERY_OPERATIONS_TABLE}
                 WHERE logical_request_key = ?1
                   AND status = 'pending'
                   AND (lease_expires_at IS NULL OR lease_expires_at < ?2)
                 ORDER BY created_at ASC
                 LIMIT 1"
            ),
            params![logical_request_key, now_rfc],
            map_operation_row,
        )
        .optional()?;
    Ok(op)
}

/// Insert a new `Pending` operation with a guarded owner/lease claim. [B3/B4]
///
/// Called inside the reserve transaction **after** the epoch CAS and **after**
/// allocating `execution_attempt_id`. The `owner_pid` + `lease_expires_at`
/// form a guarded claim so exactly one process may execute or reconcile
/// (operations pseudocode lines 61–78).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-004
pub fn insert_pending(tx: &Connection, operation: &PendingOperationInsert) -> SqliteResult<()> {
    let now = Utc::now().to_rfc3339();
    let lease_rfc = operation.lease_expires_at.to_rfc3339();
    let epoch_i64 = i64::try_from(operation.epoch).unwrap_or(i64::MAX);
    tx.execute(
        &format!(
            "INSERT INTO {RECOVERY_OPERATIONS_TABLE}
               (operation_id, run_id, epoch, step_id, capsule_envelope_digest,
                source_attempt_id, logical_request_key, intent_digest, status,
                owner_pid, lease_expires_at, execution_attempt_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"
        ),
        params![
            operation.operation_id,
            operation.run_id,
            epoch_i64,
            operation.step_id,
            operation.capsule_envelope_digest,
            operation.source_attempt_id,
            operation.logical_request_key,
            operation.intent_digest,
            OperationStatus::Pending.as_str(),
            i64::from(operation.owner_pid),
            lease_rfc,
            operation.execution_attempt_id,
            now,
        ],
    )?;
    Ok(())
}

/// Attempt to adopt an existing `Pending` operation whose lease has expired. [B3]
///
/// Guarded: only transitions if the lease is still expired inside this
/// transaction (operations pseudocode lines 83–96).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-004
pub fn try_adopt_pending(
    tx: &Connection,
    operation_id: &str,
    new_owner_pid: u32,
    new_lease_expires_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> SqliteResult<AdoptOutcome> {
    let now_rfc = now.to_rfc3339();
    let lease_rfc = new_lease_expires_at.to_rfc3339();
    let affected = tx.execute(
        &format!(
            "UPDATE {RECOVERY_OPERATIONS_TABLE}
             SET owner_pid = ?2, lease_expires_at = ?3
             WHERE operation_id = ?1
               AND status = 'pending'
               AND (lease_expires_at IS NULL OR lease_expires_at < ?4)"
        ),
        params![operation_id, i64::from(new_owner_pid), lease_rfc, now_rfc,],
    )?;
    if affected == 1 {
        Ok(AdoptOutcome::Adopted)
    } else {
        Ok(AdoptOutcome::StillOwned)
    }
}

/// Finalize an operation as `Completed` with a serialized outcome. [C2]
///
/// Guarded: only transitions from `Pending` and returns the attempt id encoded
/// in the serialized outcome (operations pseudocode lines 101–112).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-004
pub fn finalize_completed(
    tx: &Connection,
    operation_id: &str,
    serialized_outcome: &str,
) -> SqliteResult<i64> {
    let now = Utc::now().to_rfc3339();
    let affected = tx.execute(
        &format!(
            "UPDATE {RECOVERY_OPERATIONS_TABLE}
             SET status = ?2, serialized_outcome = ?3, finalized_at = ?4
             WHERE operation_id = ?1 AND status = ?5"
        ),
        params![
            operation_id,
            OperationStatus::Completed.as_str(),
            serialized_outcome,
            now,
            OperationStatus::Pending.as_str(),
        ],
    )?;
    if affected != 1 {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some(format!(
                "finalize_completed guard failed for {operation_id} (affected {affected})"
            )),
        ));
    }
    let attempt_id = attempt_id_from_outcome(serialized_outcome)?;
    Ok(attempt_id)
}

/// Finalize an operation as `Refused`. [C2]
///
/// Guarded: only transitions from `Pending` (operations pseudocode lines
/// 115–121).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-004
pub fn finalize_refused(tx: &Connection, operation_id: &str, reason: &str) -> SqliteResult<()> {
    finalize_guarded(tx, operation_id, OperationStatus::Refused, reason)
}

/// Finalize an operation as `Conflict`. [C2]
///
/// Guarded: only transitions from `Pending` (operations pseudocode lines
/// 124–130).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-004
pub fn finalize_conflict(tx: &Connection, operation_id: &str, detail: &str) -> SqliteResult<()> {
    finalize_guarded(tx, operation_id, OperationStatus::Conflict, detail)
}

/// Shared guarded finalize for the `Refused`/`Conflict` terminal transitions.
///
/// Both transitions are guarded by `WHERE status = 'pending'` and require
/// exactly one affected row. The `detail` is stored in `serialized_outcome`
/// for audit. [C2]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-004
fn finalize_guarded(
    tx: &Connection,
    operation_id: &str,
    terminal: OperationStatus,
    detail: &str,
) -> SqliteResult<()> {
    let now = Utc::now().to_rfc3339();
    let affected = tx.execute(
        &format!(
            "UPDATE {RECOVERY_OPERATIONS_TABLE}
             SET status = ?2, serialized_outcome = ?3, finalized_at = ?4
             WHERE operation_id = ?1 AND status = ?5"
        ),
        params![
            operation_id,
            terminal.as_str(),
            detail,
            now,
            OperationStatus::Pending.as_str(),
        ],
    )?;
    if affected != 1 {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some(format!(
                "{} guard failed for {operation_id} (affected {affected})",
                terminal.as_str()
            )),
        ));
    }
    Ok(())
}

/// Extract the `attempt_id` from a serialized completed outcome.
///
/// The serialized outcome is a JSON object with an integer `attempt_id` field
/// (set by the protocol finalize phase). Returns a database error if the JSON
/// is missing the field or is not a valid object.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-004
fn attempt_id_from_outcome(serialized_outcome: &str) -> SqliteResult<i64> {
    let value: serde_json::Value = serde_json::from_str(serialized_outcome).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("serialized_outcome is not valid JSON: {e}"),
            )),
        )
    })?;
    let attempt_id = value
        .get("attempt_id")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "serialized_outcome missing integer attempt_id",
                )),
            )
        })?;
    Ok(attempt_id)
}

/// Map a `recovery_operations` row into a [`RecoveryOperation`].
fn map_operation_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RecoveryOperation> {
    let status_str: String = row.get(8)?;
    let status = OperationStatus::parse_str(&status_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            8,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown operation status '{status_str}'"),
            )),
        )
    })?;
    let owner_pid_i64: Option<i64> = row.get(9)?;
    let owner_pid = owner_pid_i64.map(|p| u32::try_from(p).unwrap_or(0));
    let lease_expires_at_str: Option<String> = row.get(10)?;
    let lease_expires_at = lease_expires_at_str
        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
        .map(|dt| dt.with_timezone(&Utc));
    let epoch_i64: i64 = row.get(2)?;
    let epoch = u64::try_from(epoch_i64).unwrap_or(0);
    Ok(RecoveryOperation {
        operation_id: row.get(0)?,
        run_id: row.get(1)?,
        epoch,
        step_id: row.get(3)?,
        capsule_envelope_digest: row.get(4)?,
        source_attempt_id: row.get(5)?,
        logical_request_key: row.get(6)?,
        intent_digest: row.get(7)?,
        status,
        owner_pid,
        lease_expires_at,
        execution_attempt_id: row.get(11)?,
        serialized_outcome: row.get(12)?,
    })
}

/// Count durable recovery operations for one run.
///
/// This read-only inspection API keeps qualification and diagnostics from
/// embedding persistence SQL outside this module.
pub fn count_operations_for_run(conn: &Connection, run_id: &str) -> SqliteResult<i64> {
    conn.query_row(
        &format!("SELECT COUNT(*) FROM {RECOVERY_OPERATIONS_TABLE} WHERE run_id = ?1"),
        params![run_id],
        |row| row.get(0),
    )
}
