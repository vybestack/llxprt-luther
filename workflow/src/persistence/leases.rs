/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P02
/// Issue-lease persistence — durable claim state preventing duplicate daemon
/// work for the same issue across restarts and across multiple configs.
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Result as SqliteResult};

/// Lifecycle status of an issue lease.
///
/// Mirrors the shape of [`crate::persistence::run_metadata::RunStatus`] but is
/// scoped to claim/lease bookkeeping rather than run execution.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P02
/// @requirement:REQ-DAEMON-DISCOVERY-002
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseStatus {
    Pending,
    Claimed,
    Running,
    WaitingExternal,
    ReadyToResume,
    Completed,
    Failed,
    Abandoned,
    Stale,
}

/// Result of a conditional lease transition together with the durable state
/// observed when the transition did not apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConditionalLeaseStatusOutcome {
    Applied,
    Rejected {
        current_status: LeaseStatus,
        current_run_id: Option<String>,
    },
    Missing,
}

impl LeaseStatus {
    /// Canonical lowercase string representation used for database
    /// persistence and all SQL parameter binding.
    ///
    /// Centralising the mapping here (and having both [`std::fmt::Display`]
    /// and the reclaimable bind values derive from it) prevents the kind of
    /// silent drift where hardcoded literals diverge from the actual enum
    /// strings.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            LeaseStatus::Pending => "pending",
            LeaseStatus::Claimed => "claimed",
            LeaseStatus::Running => "running",
            LeaseStatus::WaitingExternal => "waiting_external",
            LeaseStatus::ReadyToResume => "ready_to_resume",
            LeaseStatus::Completed => "completed",
            LeaseStatus::Failed => "failed",
            LeaseStatus::Abandoned => "abandoned",
            LeaseStatus::Stale => "stale",
        }
    }
}

impl std::fmt::Display for LeaseStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl rusqlite::ToSql for LeaseStatus {
    fn to_sql(&self) -> SqliteResult<rusqlite::types::ToSqlOutput<'_>> {
        self.as_str().to_sql()
    }
}

impl std::str::FromStr for LeaseStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(LeaseStatus::Pending),
            "claimed" => Ok(LeaseStatus::Claimed),
            "running" => Ok(LeaseStatus::Running),
            "waiting_external" => Ok(LeaseStatus::WaitingExternal),
            "ready_to_resume" => Ok(LeaseStatus::ReadyToResume),
            "completed" => Ok(LeaseStatus::Completed),
            "failed" => Ok(LeaseStatus::Failed),
            "abandoned" => Ok(LeaseStatus::Abandoned),
            "stale" => Ok(LeaseStatus::Stale),
            _ => Err(format!("Unknown lease status: {s}")),
        }
    }
}

impl LeaseStatus {
    /// Terminal states whose lease no longer protects an issue from re-work and
    /// may therefore be superseded by a fresh claim.
    ///
    /// This is the single source of truth shared by [`blocks_duplicate_work`]
    /// (the discovery-side eligibility check) and [`try_claim`]'s conflict
    /// guard, so the "is this eligible?" and "can I actually re-claim it?"
    /// decisions can never disagree.
    ///
    /// [`blocks_duplicate_work`]: LeaseStatus::blocks_duplicate_work
    /// [`try_claim`]: crate::persistence::leases::try_claim
    pub const RECLAIMABLE: [LeaseStatus; 4] = [
        LeaseStatus::Completed,
        LeaseStatus::Failed,
        LeaseStatus::Abandoned,
        LeaseStatus::Stale,
    ];

    /// Whether this lease occupies a concurrency slot (Claimed or Running).
    #[must_use]
    pub fn is_active(self) -> bool {
        matches!(self, LeaseStatus::Claimed | LeaseStatus::Running)
    }

    /// Whether this lease retains duplicate-work protection for an issue.
    #[must_use]
    pub fn blocks_duplicate_work(self) -> bool {
        !Self::RECLAIMABLE.contains(&self)
    }
}

/// A persisted claim on a single GitHub issue.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P02
/// @requirement:REQ-DAEMON-DISCOVERY-002
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueLease {
    pub lease_id: String,
    pub issue_repo: String,
    pub issue_number: u64,
    pub config_id: String,
    pub run_id: Option<String>,
    pub status: LeaseStatus,
    pub claimed_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub heartbeat_at: DateTime<Utc>,
}

/// Initialize the `issue_leases` table.
///
/// The `UNIQUE(issue_repo, issue_number)` constraint is the core
/// duplicate-prevention primitive: only one lease can exist per issue.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P02
/// @requirement:REQ-DAEMON-DISCOVERY-002
pub fn init_leases_table(conn: &Connection) -> SqliteResult<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS issue_leases (
            lease_id TEXT PRIMARY KEY,
            issue_repo TEXT NOT NULL,
            issue_number INTEGER NOT NULL,
            config_id TEXT NOT NULL,
            run_id TEXT,
            status TEXT NOT NULL,
            claimed_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            heartbeat_at TEXT NOT NULL,
            UNIQUE(issue_repo, issue_number)
        )",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_issue_leases_config_status
            ON issue_leases (config_id, status)",
        [],
    )?;
    super::claim_metadata::init_claim_metadata_table(conn)?;
    Ok(())
}

/// Parse a single row into an [`IssueLease`].
fn row_to_lease(row: &rusqlite::Row<'_>) -> SqliteResult<IssueLease> {
    let status_str: String = row.get(5)?;
    let status = status_str
        .parse::<LeaseStatus>()
        .unwrap_or(LeaseStatus::Pending);
    Ok(IssueLease {
        lease_id: row.get(0)?,
        issue_repo: row.get(1)?,
        issue_number: row.get::<_, i64>(2)? as u64,
        config_id: row.get(3)?,
        run_id: row.get(4)?,
        status,
        claimed_at: parse_ts(&row.get::<_, String>(6)?),
        updated_at: parse_ts(&row.get::<_, String>(7)?),
        heartbeat_at: parse_ts(&row.get::<_, String>(8)?),
    })
}

/// Parse an RFC3339 timestamp, falling back to the epoch on malformed input.
fn parse_ts(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(|_| DateTime::<Utc>::UNIX_EPOCH)
}

/// Insert a new lease record.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P02
pub fn create_lease(conn: &Connection, lease: &IssueLease) -> SqliteResult<()> {
    let issue_number = issue_number_to_sql_i64(lease.issue_number)?;
    conn.execute(
        "INSERT INTO issue_leases
            (lease_id, issue_repo, issue_number, config_id, run_id, status,
             claimed_at, updated_at, heartbeat_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            lease.lease_id,
            lease.issue_repo,
            issue_number,
            lease.config_id,
            lease.run_id,
            lease.status.to_string(),
            lease.claimed_at.to_rfc3339(),
            lease.updated_at.to_rfc3339(),
            lease.heartbeat_at.to_rfc3339(),
        ],
    )?;
    Ok(())
}

/// Atomically claim an issue, returning the new lease when the claim is won.
///
/// Uses `INSERT ... ON CONFLICT(issue_repo, issue_number) DO UPDATE` guarded by
/// a status predicate: a fresh claim wins when either no lease exists yet, or
/// the existing lease is in a terminal [`LeaseStatus::RECLAIMABLE`] state
/// (Completed/Failed/Abandoned/Stale). A lease that is still active
/// (Claimed/Running) or otherwise blocking is left untouched and the caller
/// gets `Ok(None)`. This keeps the concurrency guarantee — two live daemons (or
/// restarts) can never both launch the same issue — while allowing a finished
/// or abandoned issue to be picked up again on a later pass, matching
/// [`LeaseStatus::blocks_duplicate_work`].
///
/// The `RETURNING lease_id` clause distinguishes "this claim won the row" from
/// "the conflicting row was left in place": SQLite only emits a returned row
/// for the INSERT or for a DO UPDATE whose `WHERE` matched, so a suppressed
/// upsert yields no row and maps to `Ok(None)`.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P02
/// @requirement:REQ-DAEMON-DISCOVERY-002,REQ-DAEMON-DISCOVERY-005
pub fn try_claim(
    conn: &Connection,
    repo: &str,
    issue_number: u64,
    config_id: &str,
) -> SqliteResult<Option<IssueLease>> {
    let now = Utc::now();
    let lease = IssueLease {
        lease_id: format!(
            "lease-{repo}-{issue_number}-{}",
            now.timestamp_nanos_opt().unwrap_or(0)
        ),
        issue_repo: repo.to_string(),
        issue_number,
        config_id: config_id.to_string(),
        run_id: None,
        status: LeaseStatus::Claimed,
        claimed_at: now,
        updated_at: now,
        heartbeat_at: now,
    };
    let status_str = lease.status.as_str();
    let claimed_at = lease.claimed_at.to_rfc3339();
    let updated_at = lease.updated_at.to_rfc3339();
    let heartbeat_at = lease.heartbeat_at.to_rfc3339();
    // Bind canonical borrowed status strings directly; no status String or
    // intermediate reclaimable collection is allocated on the claim path.
    let issue_number_i64 = issue_number_to_sql_i64(lease.issue_number)?;
    let mut claim_params: Vec<&dyn rusqlite::ToSql> =
        Vec::with_capacity(9 + LeaseStatus::RECLAIMABLE.len());
    claim_params.push(&lease.lease_id);
    claim_params.push(&lease.issue_repo);
    claim_params.push(&issue_number_i64);
    claim_params.push(&lease.config_id);
    claim_params.push(&lease.run_id);
    claim_params.push(&status_str);
    claim_params.push(&claimed_at);
    claim_params.push(&updated_at);
    claim_params.push(&heartbeat_at);
    let insert_placeholders = sql_placeholders(claim_params.len());
    let reclaimable = sql_placeholders(LeaseStatus::RECLAIMABLE.len());
    for reclaimable_status in &LeaseStatus::RECLAIMABLE {
        claim_params.push(reclaimable_status);
    }
    let claimed_lease_id: Option<String> = conn
        .query_row(
            &format!(
                "INSERT INTO issue_leases
                    (lease_id, issue_repo, issue_number, config_id, run_id, status,
                     claimed_at, updated_at, heartbeat_at)
                 VALUES ({insert_placeholders})
                 ON CONFLICT(issue_repo, issue_number) DO UPDATE SET
                    lease_id = excluded.lease_id,
                    config_id = excluded.config_id,
                    run_id = excluded.run_id,
                    status = excluded.status,
                    claimed_at = excluded.claimed_at,
                    updated_at = excluded.updated_at,
                    heartbeat_at = excluded.heartbeat_at
                 WHERE issue_leases.status IN ({reclaimable})
                 RETURNING lease_id"
            ),
            claim_params.as_slice(),
            |row| row.get(0),
        )
        .optional()?;

    match claimed_lease_id {
        Some(lease_id) if lease_id == lease.lease_id => Ok(Some(lease)),
        _ => Ok(None),
    }
}

#[derive(Debug, thiserror::Error)]
#[error("issue number {issue_number} overflows i64 for SQLite binding")]
struct IssueNumberConversionError {
    issue_number: u64,
    #[source]
    source: std::num::TryFromIntError,
}

fn issue_number_to_sql_i64(issue_number: u64) -> SqliteResult<i64> {
    i64::try_from(issue_number).map_err(|source| {
        rusqlite::Error::ToSqlConversionFailure(Box::new(IssueNumberConversionError {
            issue_number,
            source,
        }))
    })
}

const _: () = assert!(
    !LeaseStatus::RECLAIMABLE.is_empty(),
    "LeaseStatus::RECLAIMABLE must not be empty"
);

/// Render anonymous SQL placeholders for an ordered parameter group.
///
/// Callers derive each group length from the same values they append to the
/// bind list, so adding or removing values cannot shift a manually numbered
/// placeholder range.
fn sql_placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Fetch the lease (if any) covering a specific issue.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P02
pub fn get_lease_for_issue(
    conn: &Connection,
    repo: &str,
    issue_number: u64,
) -> SqliteResult<Option<IssueLease>> {
    let issue_number = issue_number_to_sql_i64(issue_number)?;
    conn.query_row(
        "SELECT lease_id, issue_repo, issue_number, config_id, run_id, status,
                claimed_at, updated_at, heartbeat_at
         FROM issue_leases WHERE issue_repo = ?1 AND issue_number = ?2",
        params![repo, issue_number],
        row_to_lease,
    )
    .optional()
}

/// Fetch all leases covering `issue_numbers` within `repo` in a single query.
///
/// Returns a map keyed by issue number so callers can look up each child's
/// lease without issuing one database round trip per child (avoids the N-query
/// pattern for parents with many children). Issues without a lease are simply
/// absent from the map.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P02
pub fn get_leases_for_issues(
    conn: &Connection,
    repo: &str,
    issue_numbers: &[u64],
) -> SqliteResult<std::collections::BTreeMap<u64, IssueLease>> {
    let mut leases = std::collections::BTreeMap::new();
    if issue_numbers.is_empty() {
        return Ok(leases);
    }
    let placeholders = sql_placeholders(issue_numbers.len());
    let sql = format!(
        "{SELECT_COLUMNS} WHERE issue_repo = ? AND issue_number IN ({placeholders}) \
         ORDER BY issue_number"
    );
    let mut stmt = conn.prepare(&sql)?;
    let numbers: Vec<i64> = issue_numbers
        .iter()
        .copied()
        .map(issue_number_to_sql_i64)
        .collect::<SqliteResult<_>>()?;
    let mut args: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(issue_numbers.len() + 1);
    args.push(&repo);
    for number in &numbers {
        args.push(number);
    }
    for lease in collect_leases(&mut stmt, &args)? {
        leases.insert(lease.issue_number, lease);
    }
    Ok(leases)
}

/// Update a lease's status (and optionally associate a run id).
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P02
pub fn update_lease_status(
    conn: &Connection,
    lease_id: &str,
    status: LeaseStatus,
    run_id: Option<&str>,
) -> SqliteResult<()> {
    let now = Utc::now().to_rfc3339();
    let changed = match run_id {
        Some(rid) => conn.execute(
            "UPDATE issue_leases SET status = ?1, run_id = ?2, updated_at = ?3
             WHERE lease_id = ?4",
            params![status.to_string(), rid, now, lease_id],
        )?,
        None => conn.execute(
            "UPDATE issue_leases SET status = ?1, updated_at = ?2 WHERE lease_id = ?3",
            params![status.to_string(), now, lease_id],
        )?,
    };
    let _ = changed;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
#[error("new run id must not be empty")]
pub(super) struct InvalidRunIdError;

/// Conditionally update a lease's status only when its current status is in
/// `expected_statuses` **and** (when `expected_run_id` is `Some`) the lease's
/// `run_id` already matches the expected owner, returning whether the
/// transition was applied.
///
/// This prevents a stale writer (e.g. a launcher returning from a long engine
/// call) from overwriting a newer terminal or ready transition made by the
/// poller while it was running. The `expected_run_id` guard additionally
/// prevents a concurrently reclaimed lease (whose `run_id` was superseded by a
/// new run) from being mutated by the old run's stale decision. When
/// `new_run_id` is a non-empty `Some`, the column is updated to the new value;
/// an empty value is rejected before executing SQL. When `None`, the existing
/// `run_id` is preserved (the column is never nulled out by a conditional
/// transition).
///
/// The `expected_statuses` list is bound as parameterised placeholders so the
/// status values are never interpolated into the SQL string.
///
pub fn update_lease_status_conditional(
    conn: &Connection,
    lease_id: &str,
    status: LeaseStatus,
    expected_statuses: &[LeaseStatus],
    new_run_id: Option<&str>,
    expected_run_id: Option<&str>,
) -> SqliteResult<bool> {
    if new_run_id.is_some_and(str::is_empty) {
        return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
            InvalidRunIdError,
        )));
    }
    if expected_statuses.is_empty() {
        return Ok(false);
    }
    let now = Utc::now().to_rfc3339();
    let status_str = status.as_str();
    let mut params: Vec<&dyn rusqlite::ToSql> =
        Vec::with_capacity(4 + expected_statuses.len() + usize::from(expected_run_id.is_some()));
    params.push(&status_str);
    params.push(&new_run_id);
    params.push(&now);
    params.push(&lease_id);
    let expected_placeholders = sql_placeholders(expected_statuses.len());
    for expected in expected_statuses {
        params.push(expected);
    }
    let ownership_guard = if let Some(run_id) = &expected_run_id {
        params.push(run_id);
        " AND run_id = ?"
    } else {
        ""
    };
    let sql = format!(
        "UPDATE issue_leases
         SET status = ?,
             run_id = COALESCE(?, run_id),
             updated_at = ?
         WHERE lease_id = ? AND status IN ({expected_placeholders}){ownership_guard}"
    );
    let changed = conn.execute(&sql, params.as_slice())?;
    Ok(changed > 0)
}

/// Conditionally update a lease and atomically classify a rejected transition.
///
/// An immediate transaction acquires SQLite writer exclusion before a rejected
/// state is read, so another writer cannot supersede the state between the
/// conditional update and its classification.
///
/// `conn` must not already have an active transaction. Rusqlite's
/// `new_unchecked` is a safe Rust API that checks this at runtime and returns a
/// SQLite error for a nested transaction; "unchecked" only means the
/// `&mut Connection` compile-time exclusion is unavailable through this API.
pub fn update_lease_status_conditional_outcome(
    conn: &Connection,
    lease_id: &str,
    status: LeaseStatus,
    expected_statuses: &[LeaseStatus],
    new_run_id: Option<&str>,
    expected_run_id: Option<&str>,
) -> SqliteResult<ConditionalLeaseStatusOutcome> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let applied = update_lease_status_conditional(
        &tx,
        lease_id,
        status,
        expected_statuses,
        new_run_id,
        expected_run_id,
    )?;
    let outcome = if applied {
        ConditionalLeaseStatusOutcome::Applied
    } else {
        let current = tx
            .query_row(
                "SELECT status, run_id FROM issue_leases WHERE lease_id = ?1",
                params![lease_id],
                |row| {
                    let status_string = row.get::<_, String>(0)?;
                    let status = status_string.parse::<LeaseStatus>().map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
                        )
                    })?;
                    Ok((status, row.get::<_, Option<String>>(1)?))
                },
            )
            .optional()?;
        match current {
            Some((current_status, current_run_id)) => ConditionalLeaseStatusOutcome::Rejected {
                current_status,
                current_run_id,
            },
            None => ConditionalLeaseStatusOutcome::Missing,
        }
    };
    tx.commit()?;
    Ok(outcome)
}

/// Refresh a lease's heartbeat timestamp to keep it from going stale.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P02
pub fn touch_lease_heartbeat(conn: &Connection, lease_id: &str) -> SqliteResult<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE issue_leases SET heartbeat_at = ?1, updated_at = ?1 WHERE lease_id = ?2",
        params![now, lease_id],
    )?;
    Ok(())
}

/// Collect rows produced by a prepared statement into a `Vec<IssueLease>`.
fn collect_leases(
    stmt: &mut rusqlite::Statement<'_>,
    args: &[&dyn rusqlite::ToSql],
) -> SqliteResult<Vec<IssueLease>> {
    let rows = stmt.query_map(args, row_to_lease)?;
    let mut out = Vec::new();
    for lease in rows {
        out.push(lease?);
    }
    Ok(out)
}

const SELECT_COLUMNS: &str =
    "SELECT lease_id, issue_repo, issue_number, config_id, run_id, status, \
     claimed_at, updated_at, heartbeat_at FROM issue_leases";

/// List all leases ordered by issue number.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P02
pub fn list_all_leases(conn: &Connection) -> SqliteResult<Vec<IssueLease>> {
    let sql = format!("{SELECT_COLUMNS} ORDER BY issue_repo, issue_number");
    let mut stmt = conn.prepare(&sql)?;
    collect_leases(&mut stmt, &[])
}

/// List leases belonging to a specific config.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P02
pub fn list_leases_by_config(conn: &Connection, config_id: &str) -> SqliteResult<Vec<IssueLease>> {
    let sql = format!("{SELECT_COLUMNS} WHERE config_id = ?1 ORDER BY issue_repo, issue_number");
    let mut stmt = conn.prepare(&sql)?;
    collect_leases(&mut stmt, &[&config_id])
}

/// List leases with a specific status.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P02
pub fn list_leases_by_status(
    conn: &Connection,
    status: LeaseStatus,
) -> SqliteResult<Vec<IssueLease>> {
    let sql = format!("{SELECT_COLUMNS} WHERE status = ?1 ORDER BY issue_repo, issue_number");
    let mut stmt = conn.prepare(&sql)?;
    collect_leases(&mut stmt, &[&status.to_string()])
}

/// List ready-to-resume leases for a config, oldest first.
pub fn list_ready_to_resume_leases(
    conn: &Connection,
    config_id: &str,
) -> SqliteResult<Vec<IssueLease>> {
    let sql = format!(
        "{SELECT_COLUMNS} WHERE config_id = ?1 AND status = 'ready_to_resume' ORDER BY updated_at, issue_repo, issue_number"
    );
    let mut stmt = conn.prepare(&sql)?;
    collect_leases(&mut stmt, &[&config_id])
}

/// Count all active (Claimed + Running) leases — the global concurrency gate.
pub fn count_active_leases(conn: &Connection) -> SqliteResult<usize> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM issue_leases WHERE status IN ('claimed', 'running')",
        [],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

/// Count active (Claimed + Running) leases for a config — the concurrency gate.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P02
/// @requirement:REQ-DAEMON-DISCOVERY-006
pub fn count_active_leases_for_config(conn: &Connection, config_id: &str) -> SqliteResult<usize> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM issue_leases
         WHERE config_id = ?1 AND status IN ('claimed', 'running')",
        params![config_id],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

/// Count active (Claimed + Running) leases for a repository.
pub fn count_active_leases_for_repository(conn: &Connection, repo: &str) -> SqliteResult<usize> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM issue_leases
         WHERE issue_repo = ?1 AND status IN ('claimed', 'running')",
        params![repo],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

/// Mark Claimed/Running leases whose heartbeat is older than `timeout_secs` as
/// Stale, returning the number of leases recovered. Run on daemon startup so a
/// crashed previous instance does not permanently block an issue.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P02
/// @requirement:REQ-DAEMON-DISCOVERY-007
pub fn mark_stale_leases(conn: &Connection, timeout_secs: u64) -> SqliteResult<usize> {
    let now_ts = Utc::now();
    let cutoff = (now_ts - chrono::Duration::seconds(timeout_secs as i64)).to_rfc3339();
    let now = now_ts.to_rfc3339();
    let updated = conn.execute(
        "UPDATE issue_leases SET status = 'stale', updated_at = ?1
         WHERE status IN ('claimed', 'running') AND heartbeat_at < ?2",
        params![now, cutoff],
    )?;
    Ok(updated)
}

/// Mark overdue ready-to-resume leases stale while leaving deliberate external waits intact.
pub fn mark_stale_ready_to_resume_leases(
    conn: &Connection,
    timeout_secs: u64,
) -> SqliteResult<usize> {
    let now_ts = Utc::now();
    let cutoff = (now_ts - chrono::Duration::seconds(timeout_secs as i64)).to_rfc3339();
    let now = now_ts.to_rfc3339();
    let updated = conn.execute(
        "UPDATE issue_leases SET status = 'stale', updated_at = ?1
         WHERE status = 'ready_to_resume' AND updated_at < ?2",
        params![now, cutoff],
    )?;
    Ok(updated)
}

#[cfg(test)]
#[path = "leases_tests.rs"]
mod tests;
