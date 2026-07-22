//! Conditional (atomic, status-guarded) run-status updates.
//!
//! Extracted from the main sqlite module to keep it under the source-size
//! budget. These functions resolve the read-modify-write TOCTOU window at the
//! persistence boundary with a single atomic `UPDATE … WHERE status …` guard,
//! optionally classifying a zero-row result as terminal-vs-missing.
use rusqlite::{Connection, Result as SqliteResult};

use super::super::run_metadata::RunStatus;
use super::row_parsing::get_run_status_with_conn;

/// Typed outcome of a conditional status update, distinguishing the three
/// possible results atomically without a separate read-then-write.
///
/// This resolves the check-then-act TOCTOU window at the persistence
/// boundary: a single atomic `UPDATE … WHERE status NOT IN (terminals)`
/// either updates the row ([`Updated`](Self::Updated)), matches zero rows
/// because the run is already terminal
/// ([`AlreadyTerminal`](Self::AlreadyTerminal)), or matches zero rows because
/// the run does not exist ([`RunMissing`](Self::RunMissing)). The
/// missing-vs-terminal classification is determined by a single post-update
/// `SELECT` that runs *after* the atomic guard has already decided not to
/// write, so it cannot race with a concurrent terminal transition to produce a
/// false non-terminal classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConditionalStatusOutcome {
    /// The atomic conditional UPDATE matched and transitioned the row.
    Updated,
    /// The run exists but is already in a terminal status — the guard
    /// rejected the write. The current terminal status is preserved so
    /// callers can compensate or log it.
    AlreadyTerminal(RunStatus),
    /// The run metadata record does not exist at all — an integrity failure.
    RunMissing,
}

/// Crate-internal outcome for a run transition guarded by explicit source
/// statuses.
pub(crate) enum ExpectedRunStatusOutcome {
    /// The run matched an expected source status and was updated.
    Updated,
    /// The run exists, but its status no longer matches the caller's snapshot.
    StatusMismatch,
    /// The run metadata record does not exist.
    RunMissing,
}

/// Conditionally update a run's status and current_step using an atomic
/// `UPDATE … WHERE run_id = ? AND status NOT IN (terminal set)` guard.
///
/// Returns `Ok(true)` when the row was updated and `Ok(false)` when no row
/// matched, either because the run is already terminal or because `run_id` is
/// missing. Call [`persist_run_status_conditional_outcome_with_conn`] when the
/// caller needs that distinction. The guard prevents the classic
/// read-modify-write race where two transactions read the same state and the
/// later write overwrites the earlier one.
///
/// When `current_step` is `Some`, the column is updated to the new value; when
/// `None`, the existing `current_step` is preserved (the column is never nulled
/// out by a status-only transition). This mirrors the `run_id` preservation in
/// [`crate::persistence::leases::update_lease_status_conditional`] via a single
/// atomic `CASE WHEN … THEN … ELSE … END` expression. The SQL is a static
/// constant — the terminal-status count is fixed at compile time
/// ([`RunStatus::TERMINAL_SQL`] has 5 entries), so the placeholder list is
/// constant and the query plan is cached by `prepare_cached`.
///
/// The guard mirrors [`RunStatus::is_terminal`] via [`RunStatus::TERMINAL_SQL`]
/// so the SQL set and the Rust predicate can never disagree.
pub(crate) fn persist_run_status_conditional_with_conn(
    conn: &Connection,
    run_id: &str,
    status: &RunStatus,
    current_step: Option<&str>,
) -> SqliteResult<bool> {
    let status_str = status.to_string();
    let now = chrono::Utc::now().to_rfc3339();
    // TERMINAL_SQL is a fixed-length compile-time constant (5 entries), so the
    // SQL shape never changes — use a static query string and prepare it once
    // per connection via prepare_cached to avoid repeated parse/planning.
    let mut stmt = conn.prepare_cached(CONDITIONAL_STATUS_UPDATE_SQL)?;
    let mut params: Vec<&dyn rusqlite::ToSql> =
        Vec::with_capacity(4 + RunStatus::TERMINAL_SQL.len());
    params.push(&status_str);
    params.push(&current_step);
    params.push(&now);
    params.push(&run_id);
    params.extend(
        RunStatus::TERMINAL_SQL
            .iter()
            .map(|t| t as &dyn rusqlite::ToSql),
    );
    let updated = stmt.execute(params.as_slice())?;
    Ok(updated > 0)
}

/// Atomically classify a conditional status update as
/// [`ConditionalStatusOutcome::Updated`], [`AlreadyTerminal`](ConditionalStatusOutcome::AlreadyTerminal),
/// or [`RunMissing`](ConditionalStatusOutcome::RunMissing).
///
/// This public entry point owns and commits the transaction that covers both
/// the guarded update and any status lookup needed to classify a zero-row
/// result. Crate-internal callers that already own a transaction use the
/// visibility-limited `persist_run_status_conditional_outcome_in_transaction`
/// helper so these operations remain part of their larger atomic unit.
pub fn persist_run_status_conditional_outcome_with_conn(
    conn: &Connection,
    run_id: &str,
    status: &RunStatus,
    current_step: Option<&str>,
) -> SqliteResult<ConditionalStatusOutcome> {
    let tx = conn.unchecked_transaction()?;
    let outcome =
        persist_run_status_conditional_outcome_in_transaction(&tx, run_id, status, current_step)?;
    tx.commit()?;
    Ok(outcome)
}

/// Crate-internal implementation for callers that already own a transaction.
///
/// Unlike [`persist_run_status_conditional_outcome_with_conn`], this helper
/// neither starts nor commits a transaction. The caller controls the complete
/// atomic unit. The update and targeted `SELECT status` execute in that same
/// transaction. In SQLite, the attempted `UPDATE` starts the write transaction
/// and holds its writer lock through this read, even when no row matched, so a
/// concurrent writer cannot change or delete the row before classification.
/// The guarded update can match zero rows only when the run is missing or its
/// status is terminal; the follow-up read classifies those two outcomes.
pub(crate) fn persist_run_status_conditional_outcome_in_transaction(
    conn: &rusqlite::Transaction<'_>,
    run_id: &str,
    status: &RunStatus,
    current_step: Option<&str>,
) -> SqliteResult<ConditionalStatusOutcome> {
    if persist_run_status_conditional_with_conn(conn, run_id, status, current_step)? {
        return Ok(ConditionalStatusOutcome::Updated);
    }

    match get_run_status_with_conn(conn, run_id)? {
        None => Ok(ConditionalStatusOutcome::RunMissing),
        Some(current) => Ok(ConditionalStatusOutcome::AlreadyTerminal(current)),
    }
}

/// Update a run only when its current status is one of `expected_statuses`.
///
/// This is intentionally crate-private: poller transitions use it to align the
/// run guard with their lease guard in one transaction without broadening the
/// public persistence API. Every current poller call uses exactly one expected
/// status (`WaitingExternal`), so that hot shape uses static cached SQL. The
/// dynamic fallback remains for tests and future multi-status callers and is
/// cached by SQL shape through rusqlite's bounded statement cache.
pub(crate) fn persist_run_status_from_expected_in_transaction(
    conn: &rusqlite::Transaction<'_>,
    run_id: &str,
    status: &RunStatus,
    current_step: Option<&str>,
    expected_statuses: &[RunStatus],
) -> SqliteResult<ExpectedRunStatusOutcome> {
    if expected_statuses.is_empty() {
        return classify_expected_status_rejection(conn, run_id);
    }
    let dynamic_sql;
    let sql = if expected_statuses.len() == 1 {
        EXPECTED_STATUS_UPDATE_ONE_SQL
    } else {
        let placeholders = (0..expected_statuses.len())
            .map(|offset| format!("?{}", offset + 5))
            .collect::<Vec<_>>()
            .join(", ");
        dynamic_sql = format!(
            "UPDATE runs SET status = ?1,
                 current_step = CASE WHEN ?2 IS NULL THEN current_step ELSE ?2 END,
                 updated_at = ?3
             WHERE run_id = ?4 AND status IN ({placeholders})"
        );
        &dynamic_sql
    };
    let status = status.to_string();
    let expected_statuses: Vec<String> =
        expected_statuses.iter().map(ToString::to_string).collect();
    let now = chrono::Utc::now().to_rfc3339();
    let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(4 + expected_statuses.len());
    params.extend([
        &status as &dyn rusqlite::ToSql,
        &current_step,
        &now,
        &run_id,
    ]);
    params.extend(
        expected_statuses
            .iter()
            .map(|value| value as &dyn rusqlite::ToSql),
    );
    let mut stmt = conn.prepare_cached(sql)?;
    if stmt.execute(params.as_slice())? > 0 {
        return Ok(ExpectedRunStatusOutcome::Updated);
    }
    classify_expected_status_rejection(conn, run_id)
}

// `Transaction` dereferences to `Connection`, so callers inside an existing
// transaction use this helper without starting or obscuring a new boundary.
fn classify_expected_status_rejection(
    conn: &Connection,
    run_id: &str,
) -> SqliteResult<ExpectedRunStatusOutcome> {
    Ok(match get_run_status_with_conn(conn, run_id)? {
        Some(_) => ExpectedRunStatusOutcome::StatusMismatch,
        None => ExpectedRunStatusOutcome::RunMissing,
    })
}

/// Hot one-expected-status shape used by all current poller transitions.
const EXPECTED_STATUS_UPDATE_ONE_SQL: &str = concat!(
    "UPDATE runs SET status = ?1, ",
    "current_step = CASE WHEN ?2 IS NULL THEN current_step ELSE ?2 END, ",
    "updated_at = ?3 ",
    "WHERE run_id = ?4 AND status IN (?5)"
);

/// Static SQL for [`persist_run_status_conditional_with_conn`].
///
/// [`RunStatus::TERMINAL_SQL`] has 5 entries, so the placeholder list is
/// constant and the query plan is cached by `prepare_cached` across calls
/// without per-invocation `format!` allocation.
const CONDITIONAL_STATUS_UPDATE_SQL: &str = concat!(
    "UPDATE runs SET status = ?1, ",
    "current_step = CASE WHEN ?2 IS NULL THEN current_step ELSE ?2 END, ",
    "updated_at = ?3 ",
    "WHERE run_id = ?4 AND status NOT IN (?5, ?6, ?7, ?8, ?9)"
);

// Compile-time assertion: the SQL above has exactly 5 terminal-status
// placeholders (?5..?9). This must match `RunStatus::TERMINAL_SQL.len()`
// so that the bind-parameter count never drifts from the placeholder count.
// If a terminal status is added or removed, this assertion fails at compile
// time and the SQL constant must be updated in lockstep.
const _: () = assert!(
    RunStatus::TERMINAL_SQL.len() == 5,
    "CONDITIONAL_STATUS_UPDATE_SQL has 5 terminal placeholders; update the SQL when TERMINAL_SQL changes"
);
