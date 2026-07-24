//! Distinct durable per-run recovery epoch with CAS claim.
//!
//! The recovery epoch is a **single durable row per run** holding the current
//! epoch counter. It is NOT derived from `MAX(generation)` over attempt rows:
//! a dedicated row guarantees that epoch advancement is an explicit, guarded
//! mutation rather than an emergent side-effect of appending attempts. No
//! synthetic attempt rows are ever appended to bump the epoch. [C1]
//!
//! The epoch advances via a single compare-and-swap (CAS) inside the recovery
//! protocol's short `IMMEDIATE` reserve transaction. The CAS uses
//! `INSERT ... ON CONFLICT(run_id) DO UPDATE SET ... WHERE epoch = ?` with an
//! affected-row check so a concurrent advance is detected and reported as
//! [`CasOutcome::Stale`]. This is the **only** CAS in the recovery protocol.
//! [B2]
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
//! @requirement:REQ-RP-004

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Result as SqliteResult};

/// Table name for the durable recovery epoch.
///
/// One row per run, keyed by `run_id`. [C1]
pub const RECOVERY_EPOCH_TABLE: &str = "recovery_epoch";

/// The outcome of a compare-and-swap epoch advancement.
///
/// [`CasOutcome::Advanced`] reports the successful transition from `from` to
/// `to` (the expected epoch plus one). [`CasOutcome::Stale`] reports that a
/// concurrent claim already advanced the epoch away from `expected`, carrying
/// the `persisted` value so the caller can refresh and retry. [C1/B2]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-004
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CasOutcome {
    /// The epoch was advanced from `from` to `to` (`from + 1`).
    Advanced {
        /// The epoch value before the advancement.
        from: u64,
        /// The epoch value after the advancement (`from + 1`).
        to: u64,
    },
    /// A concurrent claim advanced the epoch from the caller's `expected`
    /// value. `persisted` is the current durable epoch.
    Stale {
        /// The current persisted epoch (advanced by a concurrent claim).
        persisted: u64,
        /// The epoch value the caller expected.
        expected: u64,
    },
}

/// Initialize the durable recovery epoch table (idempotent).
///
/// Creates a single-row-per-run table holding the current epoch counter. The
/// row is created lazily by the CAS upsert, so no seeding is required here.
/// [C1]
///
/// DDL (epoch pseudocode lines 02–06):
/// ```text
/// CREATE TABLE IF NOT EXISTS recovery_epoch (
///   run_id TEXT PRIMARY KEY,
///   epoch INTEGER NOT NULL DEFAULT 0,
///   updated_at TEXT NOT NULL
/// )
/// ```
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-004
pub fn init_epoch_table(conn: &Connection) -> SqliteResult<()> {
    conn.execute(
        &format!(
            "CREATE TABLE IF NOT EXISTS {RECOVERY_EPOCH_TABLE} (
                run_id TEXT PRIMARY KEY,
                epoch INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL
            )"
        ),
        [],
    )?;
    Ok(())
}

/// Read the current epoch for a run (read-only).
///
/// Reads a **dedicated row** in the epoch table, not `MAX(generation)` from
/// attempt rows. Returns `0` for a new run with no row yet (epoch pseudocode
/// line 10). [C1]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-004
pub fn read_epoch(conn: &Connection, run_id: &str) -> SqliteResult<u64> {
    let epoch: Option<i64> = conn
        .query_row(
            &format!("SELECT COALESCE(epoch, 0) FROM {RECOVERY_EPOCH_TABLE} WHERE run_id = ?1"),
            params![run_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(u64::try_from(epoch.unwrap_or(0)).unwrap_or(0))
}

/// Compare-and-swap epoch advancement.
///
/// Advances the epoch only if `expected_epoch` matches the persisted value,
/// using a guarded `INSERT ... ON CONFLICT(run_id) DO UPDATE SET ...
/// WHERE epoch = ?` with an affected-row check (epoch pseudocode lines 22–35).
/// On zero affected rows a concurrent claim advanced the epoch and
/// [`CasOutcome::Stale`] is returned with the persisted value. This is the
/// **only** CAS in the recovery protocol and runs inside the caller's short
/// `IMMEDIATE` reserve transaction. [C1/B2]
///
/// On the very first insert for a run (no existing row), the INSERT path
/// writes `expected_epoch + 1` and the CAS succeeds, advancing epoch `0 → 1`
/// for the first reservation. The `WHERE` clause on the `ON CONFLICT` branch
/// guards the existing-row path so a concurrent advance is detected. [C1/B2]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-004
pub fn cas_advance_epoch(
    tx: &Connection,
    run_id: &str,
    expected_epoch: u64,
) -> SqliteResult<CasOutcome> {
    let next_epoch = expected_epoch.checked_add(1).unwrap_or(expected_epoch);
    let now = Utc::now().to_rfc3339();
    let expected_i64 = i64::try_from(expected_epoch).unwrap_or(i64::MAX);
    let next_i64 = i64::try_from(next_epoch).unwrap_or(i64::MAX);

    let affected = tx.execute(
        &format!(
            "INSERT INTO {RECOVERY_EPOCH_TABLE} (run_id, epoch, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(run_id) DO UPDATE SET
               epoch = {RECOVERY_EPOCH_TABLE}.epoch + 1,
               updated_at = excluded.updated_at
             WHERE {RECOVERY_EPOCH_TABLE}.epoch = ?4"
        ),
        params![run_id, next_i64, now, expected_i64],
    )?;

    if affected == 0 {
        let persisted = read_epoch(tx, run_id)?;
        return Ok(CasOutcome::Stale {
            persisted,
            expected: expected_epoch,
        });
    }
    Ok(CasOutcome::Advanced {
        from: expected_epoch,
        to: next_epoch,
    })
}
