//! Immutable capsule store for `ExecutionCapsuleV1`. [C8]
//!
//! The capsule store persists an [`ExecutionCapsuleV1`] immutably: one row per
//! `run_id` (PRIMARY KEY). [`persist_capsule_v1`] refuses to overwrite an
//! existing capsule (no `ON CONFLICT DO UPDATE`) so a persisted capsule cannot
//! be mutated after launch. [C8]
//!
//! The table stores the envelope digest as **the** authority plus the component
//! digests (workflow/config) as metadata. [C8]
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08
//! @requirement:REQ-RP-002

use rusqlite::{params, Connection, Result as SqliteResult, TransactionBehavior};

use crate::engine::recovery::capsule::ExecutionCapsuleV1;
use crate::persistence::run_metadata::RunMetadata;
use crate::persistence::sqlite::InitialRunInsert;

/// Table name for the immutable execution capsule store. [C8]
pub const EXECUTION_CAPSULES_TABLE: &str = "execution_capsules";

/// Initialize the immutable capsule store table (idempotent). [C8]
///
/// DDL:
/// ```text
/// CREATE TABLE IF NOT EXISTS execution_capsules (
///   run_id TEXT PRIMARY KEY,
///   schema_version INTEGER NOT NULL,
///   canonicalization_version INTEGER NOT NULL,
///   domain_version INTEGER NOT NULL,
///   provenance_version INTEGER NOT NULL,
///   capsule_json TEXT NOT NULL,
///   envelope_digest TEXT NOT NULL,       -- THE authority [C8]
///   workflow_digest TEXT NOT NULL,       -- metadata [C8]
///   config_digest TEXT NOT NULL,         -- metadata [C8]
///   created_at TEXT NOT NULL
/// )
/// ```
///
/// The table is immutable by design: `run_id` is the PRIMARY KEY and
/// [`persist_capsule_v1`] uses a plain `INSERT` (no upsert) so an attempt to
/// re-write a capsule for an existing run is rejected. [C8]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08
/// @requirement:REQ-RP-002
pub fn init_capsules_table(conn: &Connection) -> SqliteResult<()> {
    conn.execute(
        &format!(
            "CREATE TABLE IF NOT EXISTS {EXECUTION_CAPSULES_TABLE} (
                run_id TEXT PRIMARY KEY,
                schema_version INTEGER NOT NULL,
                canonicalization_version INTEGER NOT NULL,
                domain_version INTEGER NOT NULL,
                provenance_version INTEGER NOT NULL,
                capsule_json TEXT NOT NULL,
                envelope_digest TEXT NOT NULL,
                workflow_digest TEXT NOT NULL,
                config_digest TEXT NOT NULL,
                created_at TEXT NOT NULL
            )"
        ),
        [],
    )?;
    Ok(())
}

/// Persist an `ExecutionCapsuleV1` immutably. [C8]
///
/// Uses a plain `INSERT` keyed by `run_id` (PRIMARY KEY). An attempt to
/// re-write a capsule for an existing run is rejected by the PRIMARY KEY
/// constraint — there is no `ON CONFLICT DO UPDATE`. [C8]
///
/// The capsule is stored as a canonical JSON blob (`capsule_json`) so the full
/// capsule round-trips through [`load_capsule_v1`]. The envelope/workflow/
/// config digests and version fields are stored as separate columns so they
/// can be validated against the JSON on load without trusting the blob. [C8]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08
/// @requirement:REQ-RP-002
pub fn persist_capsule_v1(conn: &Connection, capsule: &ExecutionCapsuleV1) -> SqliteResult<()> {
    crate::engine::recovery::capsule::verify_envelope_digest(capsule).map_err(|error| {
        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid capsule envelope: {error}"),
        )))
    })?;
    let capsule_json = serde_json::to_string(capsule)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
    let created_at = capsule.created_at.to_rfc3339();
    conn.execute(
        &format!(
            "INSERT INTO {EXECUTION_CAPSULES_TABLE} (
                run_id,
                schema_version,
                canonicalization_version,
                domain_version,
                provenance_version,
                capsule_json,
                envelope_digest,
                workflow_digest,
                config_digest,
                created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"
        ),
        params![
            capsule.run_id,
            capsule.schema_version,
            capsule.canonicalization_version,
            capsule.domain_version,
            capsule.provenance_version,
            capsule_json,
            capsule.envelope_digest,
            capsule.workflow_digest,
            capsule.config_digest,
            created_at,
        ],
    )?;
    Ok(())
}

/// Load an `ExecutionCapsuleV1` by run id. [C8]
///
/// Deserializes the canonical JSON blob and validates the loaded column
/// metadata (version fields, digests) against the capsule JSON. If the row
/// metadata does not match the capsule JSON, the row is treated as corrupted
/// and an error is returned (fail-closed). [C8]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08
/// @requirement:REQ-RP-002
pub fn load_capsule_v1(conn: &Connection, run_id: &str) -> SqliteResult<ExecutionCapsuleV1> {
    let mut stmt = conn.prepare(&format!(
        "SELECT
            schema_version,
            canonicalization_version,
            domain_version,
            provenance_version,
            capsule_json,
            envelope_digest,
            workflow_digest,
            config_digest
         FROM {EXECUTION_CAPSULES_TABLE} WHERE run_id = ?1"
    ))?;
    let row = stmt.query_row(params![run_id], |row| {
        Ok(LoadedCapsuleRow {
            schema_version: row.get::<_, i64>(0)?,
            canonicalization_version: row.get::<_, i64>(1)?,
            domain_version: row.get::<_, i64>(2)?,
            provenance_version: row.get::<_, i64>(3)?,
            capsule_json: row.get::<_, String>(4)?,
            envelope_digest: row.get::<_, String>(5)?,
            workflow_digest: row.get::<_, String>(6)?,
            config_digest: row.get::<_, String>(7)?,
        })
    })?;
    let capsule: ExecutionCapsuleV1 = serde_json::from_str(&row.capsule_json).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(err))
    })?;
    validate_loaded_capsule(run_id, &capsule, &row)?;
    crate::engine::recovery::capsule::verify_envelope_digest(&capsule).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid capsule envelope: {error}"),
            )),
        )
    })?;
    Ok(capsule)
}

/// A loaded capsule row's column metadata, validated against the JSON.
struct LoadedCapsuleRow {
    schema_version: i64,
    canonicalization_version: i64,
    domain_version: i64,
    provenance_version: i64,
    envelope_digest: String,
    workflow_digest: String,
    config_digest: String,
    capsule_json: String,
}

/// Validate the loaded column metadata against the deserialized capsule JSON.
/// Fail-closed if any column disagrees with the JSON. [C8]
fn validate_loaded_capsule(
    requested_run_id: &str,
    capsule: &ExecutionCapsuleV1,
    row: &LoadedCapsuleRow,
) -> SqliteResult<()> {
    if capsule.run_id != requested_run_id
        || capsule.schema_version != row.schema_version as u32
        || capsule.canonicalization_version != row.canonicalization_version as u32
        || capsule.domain_version != row.domain_version as u32
        || capsule.provenance_version != row.provenance_version as u32
        || capsule.envelope_digest != row.envelope_digest
        || capsule.workflow_digest != row.workflow_digest
        || capsule.config_digest != row.config_digest
    {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "capsule JSON metadata does not match row columns",
            )),
        ));
    }
    Ok(())
}

// ===========================================================================
// Atomic fresh-launch persistence (M2 closure)
// ===========================================================================

/// Typed outcome of a successful atomic launch persistence operation.
///
/// Only the success path produces an `Ok` value — the sole variant is
/// [`LaunchPersistenceOutcome::Persisted`]. Collision and error cases are
/// returned as [`LaunchPersistenceError`], so there are no impossible outcome
/// variants. [P08B]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08B
/// @requirement:REQ-RP-002
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchPersistenceOutcome {
    /// Both the initial `Starting` `RunMetadata` and the immutable
    /// `ExecutionCapsuleV1` were atomically persisted in one transaction.
    Persisted,
}

/// Typed errors produced by the atomic launch persistence API.
///
/// Carries the specific failure so the launch surface can fail closed with a
/// precise diagnostic rather than a generic DB error string.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08B
/// @requirement:REQ-RP-002
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LaunchPersistenceError {
    /// A run row already exists for the given `run_id`.
    #[error("launch collision: a run record already exists for run_id '{0}'; refusing to overwrite the existing launch record")]
    RunCollision(String),
    /// A capsule already exists for the given `run_id`.
    #[error("launch collision: an execution capsule already exists for run_id '{0}'; refusing to overwrite the immutable capsule")]
    CapsuleCollision(String),
    /// A generic SQLite error occurred during the atomic insert. The
    /// transaction was rolled back; neither row is durable.
    #[error("atomic launch persistence failed for run_id '{run_id}': {message}")]
    Database {
        /// The run_id that was being persisted.
        run_id: String,
        /// The underlying SQLite error message.
        message: String,
    },
}

/// Atomically persist the initial `Starting` `RunMetadata` and the immutable
/// `ExecutionCapsuleV1` in **one** SQLite `IMMEDIATE` transaction.
///
/// This is the sole launch persistence API: the capsule must exist before any
/// workflow execution/effects, and a duplicate run id or capsule causes full
/// rollback. A capsule insert failure leaves no run; a run insert failure
/// leaves no capsule. There is no historical/backfill path and no separate
/// persistence calls.
///
/// The transaction uses `IMMEDIATE` behavior so the writer lock is acquired
/// before any insert, serializing concurrent fresh launches for the same
/// `run_id`. The `INSERT OR FAIL` for the run row and the plain `INSERT` for
/// the capsule both execute inside this single transaction. If either fails,
/// the transaction is rolled back and neither row is durable.
///
/// # Errors
///
/// - [`LaunchPersistenceError::RunCollision`] when a run row already exists.
/// - [`LaunchPersistenceError::CapsuleCollision`] when a capsule already
///   exists (including the case where the run insert succeeded but the capsule
///   PRIMARY KEY constraint fires — the run insert is rolled back).
/// - [`LaunchPersistenceError::Database`] for any other SQLite error.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08B
/// @requirement:REQ-RP-002
pub fn persist_launch_atomically(
    conn: &Connection,
    metadata: &RunMetadata,
    capsule: &ExecutionCapsuleV1,
) -> Result<LaunchPersistenceOutcome, LaunchPersistenceError> {
    let run_id = metadata.run_id.clone();
    // BEGIN IMMEDIATE: acquire the writer lock before any insert so concurrent
    // fresh launches for the same run_id serialize. The connection is owned by
    // the caller and has not been shared, so no transaction can already be
    // active. `new_unchecked` is safe here for the same reason as
    // `init_database`: SQLite returns an error if a future refactor attempts a
    // nested BEGIN.
    let tx = match rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate) {
        Ok(tx) => tx,
        Err(error) => {
            return Err(LaunchPersistenceError::Database {
                run_id,
                message: format!("failed to begin launch transaction: {error}"),
            });
        }
    };

    // 1. INSERT OR FAIL the initial Starting RunMetadata. A collision (PRIMARY
    //    KEY constraint) is classified as RunCollision; any other error is
    //    rolled back and surfaced as Database.
    match crate::persistence::sqlite::insert_initial_run_with_conn(&tx, metadata) {
        Ok(InitialRunInsert::Inserted) => {}
        Ok(InitialRunInsert::Collision) => {
            // No need to explicitly rollback: dropping the transaction rolls it
            // back. Return the collision directly.
            return Err(LaunchPersistenceError::RunCollision(run_id));
        }
        Err(error) => {
            return Err(LaunchPersistenceError::Database {
                run_id,
                message: format!("initial run insert failed: {error}"),
            });
        }
    }

    // 2. Persist the immutable capsule. A PRIMARY KEY constraint violation
    //    means a capsule already exists for this run_id; the run insert above
    //    is rolled back by dropping the transaction. An envelope-digest
    //    verification failure is also rolled back.
    if let Err(error) = persist_capsule_v1(&tx, capsule) {
        if let Some(rusqlite::ffi::ErrorCode::ConstraintViolation) = error.sqlite_error_code() {
            return Err(LaunchPersistenceError::CapsuleCollision(run_id));
        }
        return Err(LaunchPersistenceError::Database {
            run_id,
            message: format!("capsule insert failed: {error}"),
        });
    }

    // 3. COMMIT. If commit fails, SQLite rolls back automatically.
    match tx.commit() {
        Ok(()) => Ok(LaunchPersistenceOutcome::Persisted),
        Err(error) => Err(LaunchPersistenceError::Database {
            run_id,
            message: format!("launch transaction commit failed: {error}"),
        }),
    }
}
