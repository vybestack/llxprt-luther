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
    Completed,
    Failed,
    Abandoned,
    Stale,
}

impl std::fmt::Display for LeaseStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            LeaseStatus::Pending => "pending",
            LeaseStatus::Claimed => "claimed",
            LeaseStatus::Running => "running",
            LeaseStatus::Completed => "completed",
            LeaseStatus::Failed => "failed",
            LeaseStatus::Abandoned => "abandoned",
            LeaseStatus::Stale => "stale",
        };
        write!(f, "{s}")
    }
}

impl std::str::FromStr for LeaseStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(LeaseStatus::Pending),
            "claimed" => Ok(LeaseStatus::Claimed),
            "running" => Ok(LeaseStatus::Running),
            "completed" => Ok(LeaseStatus::Completed),
            "failed" => Ok(LeaseStatus::Failed),
            "abandoned" => Ok(LeaseStatus::Abandoned),
            "stale" => Ok(LeaseStatus::Stale),
            _ => Err(format!("Unknown lease status: {s}")),
        }
    }
}

impl LeaseStatus {
    /// Whether this lease occupies a concurrency slot (Claimed or Running).
    #[must_use]
    pub fn is_active(self) -> bool {
        matches!(self, LeaseStatus::Claimed | LeaseStatus::Running)
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
    conn.execute(
        "INSERT INTO issue_leases
            (lease_id, issue_repo, issue_number, config_id, run_id, status,
             claimed_at, updated_at, heartbeat_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            lease.lease_id,
            lease.issue_repo,
            lease.issue_number as i64,
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
/// Active leases (`claimed`/`running`) block duplicate work. Terminal leases are
/// reusable so a failed or completed daemon attempt does not permanently hide an
/// otherwise eligible issue from later scheduler passes.
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
    let inserted = conn.execute(
        "INSERT INTO issue_leases
            (lease_id, issue_repo, issue_number, config_id, run_id, status,
             claimed_at, updated_at, heartbeat_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(issue_repo, issue_number) DO NOTHING",
        params![
            lease.lease_id,
            lease.issue_repo,
            lease.issue_number as i64,
            lease.config_id,
            lease.run_id,
            lease.status.to_string(),
            lease.claimed_at.to_rfc3339(),
            lease.updated_at.to_rfc3339(),
            lease.heartbeat_at.to_rfc3339(),
        ],
    )?;
    if inserted == 1 {
        return Ok(Some(lease));
    }

    let reclaimed = conn.execute(
        "UPDATE issue_leases
            SET lease_id = ?1,
                config_id = ?2,
                run_id = NULL,
                status = ?3,
                claimed_at = ?4,
                updated_at = ?5,
                heartbeat_at = ?6
          WHERE issue_repo = ?7
            AND issue_number = ?8
            AND status NOT IN ('claimed', 'running')",
        params![
            lease.lease_id,
            lease.config_id,
            lease.status.to_string(),
            lease.claimed_at.to_rfc3339(),
            lease.updated_at.to_rfc3339(),
            lease.heartbeat_at.to_rfc3339(),
            lease.issue_repo,
            lease.issue_number as i64,
        ],
    )?;

    if reclaimed == 1 {
        Ok(Some(lease))
    } else {
        Ok(None)
    }
}

/// Fetch the lease (if any) covering a specific issue.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P02
pub fn get_lease_for_issue(
    conn: &Connection,
    repo: &str,
    issue_number: u64,
) -> SqliteResult<Option<IssueLease>> {
    conn.query_row(
        "SELECT lease_id, issue_repo, issue_number, config_id, run_id, status,
                claimed_at, updated_at, heartbeat_at
         FROM issue_leases WHERE issue_repo = ?1 AND issue_number = ?2",
        params![repo, issue_number as i64],
        row_to_lease,
    )
    .optional()
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
    match run_id {
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
    Ok(())
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

/// Mark Claimed/Running leases whose heartbeat is older than `timeout_secs` as
/// Stale, returning the number of leases recovered. Run on daemon startup so a
/// crashed previous instance does not permanently block an issue.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P02
/// @requirement:REQ-DAEMON-DISCOVERY-007
pub fn mark_stale_leases(conn: &Connection, timeout_secs: u64) -> SqliteResult<usize> {
    let cutoff = (Utc::now() - chrono::Duration::seconds(timeout_secs as i64)).to_rfc3339();
    let now = Utc::now().to_rfc3339();
    let updated = conn.execute(
        "UPDATE issue_leases SET status = 'stale', updated_at = ?1
         WHERE status IN ('claimed', 'running') AND heartbeat_at < ?2",
        params![now, cutoff],
    )?;
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        init_leases_table(&c).unwrap();
        c
    }

    #[test]
    fn status_display_fromstr_round_trip() {
        for status in [
            LeaseStatus::Pending,
            LeaseStatus::Claimed,
            LeaseStatus::Running,
            LeaseStatus::Completed,
            LeaseStatus::Failed,
            LeaseStatus::Abandoned,
            LeaseStatus::Stale,
        ] {
            let s = status.to_string();
            assert_eq!(s.parse::<LeaseStatus>().unwrap(), status);
        }
    }

    #[test]
    fn create_then_get_round_trip() {
        let c = conn();
        let claimed = try_claim(&c, "o/r", 7, "cfg").unwrap().unwrap();
        let fetched = get_lease_for_issue(&c, "o/r", 7).unwrap().unwrap();
        assert_eq!(fetched.issue_number, 7);
        assert_eq!(fetched.config_id, "cfg");
        assert_eq!(fetched.lease_id, claimed.lease_id);
        assert_eq!(fetched.status, LeaseStatus::Claimed);
    }

    #[test]
    fn try_claim_second_attempt_loses() {
        let c = conn();
        let first = try_claim(&c, "o/r", 1, "cfg-a").unwrap();
        let second = try_claim(&c, "o/r", 1, "cfg-b").unwrap();
        assert!(first.is_some());
        assert!(second.is_none(), "duplicate claim must be rejected");
    }

    #[test]
    fn try_claim_reclaims_terminal_lease() {
        let c = conn();
        let first = try_claim(&c, "o/r", 8, "cfg-a").unwrap().unwrap();
        update_lease_status(&c, &first.lease_id, LeaseStatus::Failed, Some("run-old")).unwrap();

        let second = try_claim(&c, "o/r", 8, "cfg-b").unwrap().unwrap();
        assert_ne!(second.lease_id, first.lease_id);

        let fetched = get_lease_for_issue(&c, "o/r", 8).unwrap().unwrap();
        assert_eq!(fetched.lease_id, second.lease_id);
        assert_eq!(fetched.config_id, "cfg-b");
        assert_eq!(fetched.status, LeaseStatus::Claimed);
        assert!(fetched.run_id.is_none());
    }

    #[test]
    fn update_status_transitions() {
        let c = conn();
        let lease = try_claim(&c, "o/r", 2, "cfg").unwrap().unwrap();
        update_lease_status(&c, &lease.lease_id, LeaseStatus::Running, Some("run-9")).unwrap();
        let fetched = get_lease_for_issue(&c, "o/r", 2).unwrap().unwrap();
        assert_eq!(fetched.status, LeaseStatus::Running);
        assert_eq!(fetched.run_id.as_deref(), Some("run-9"));
    }

    #[test]
    fn count_active_only_counts_claimed_and_running() {
        let c = conn();
        let l1 = try_claim(&c, "o/r", 10, "cfg").unwrap().unwrap();
        let l2 = try_claim(&c, "o/r", 11, "cfg").unwrap().unwrap();
        let l3 = try_claim(&c, "o/r", 12, "cfg").unwrap().unwrap();
        update_lease_status(&c, &l2.lease_id, LeaseStatus::Running, None).unwrap();
        update_lease_status(&c, &l3.lease_id, LeaseStatus::Completed, None).unwrap();
        // l1 Claimed + l2 Running = 2 active; l3 Completed excluded.
        assert_eq!(count_active_leases_for_config(&c, "cfg").unwrap(), 2);
        let _ = l1;
    }

    #[test]
    fn list_by_status_filters() {
        let c = conn();
        let l1 = try_claim(&c, "o/r", 20, "cfg").unwrap().unwrap();
        let _l2 = try_claim(&c, "o/r", 21, "cfg").unwrap().unwrap();
        update_lease_status(&c, &l1.lease_id, LeaseStatus::Completed, None).unwrap();
        let completed = list_leases_by_status(&c, LeaseStatus::Completed).unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].issue_number, 20);
        let claimed = list_leases_by_status(&c, LeaseStatus::Claimed).unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].issue_number, 21);
    }

    #[test]
    fn mark_stale_flips_overdue_only() {
        let c = conn();
        // Fresh claim — should not go stale.
        let fresh = try_claim(&c, "o/r", 30, "cfg").unwrap().unwrap();
        // Insert an overdue lease directly with an old heartbeat.
        let old = (Utc::now() - chrono::Duration::seconds(10_000)).to_rfc3339();
        c.execute(
            "INSERT INTO issue_leases
                (lease_id, issue_repo, issue_number, config_id, run_id, status,
                 claimed_at, updated_at, heartbeat_at)
             VALUES ('stale-1','o/r',31,'cfg',NULL,'running',?1,?1,?1)",
            params![old],
        )
        .unwrap();
        let recovered = mark_stale_leases(&c, 300).unwrap();
        assert_eq!(recovered, 1);
        let fresh_now = get_lease_for_issue(&c, "o/r", 30).unwrap().unwrap();
        assert_eq!(fresh_now.status, LeaseStatus::Claimed);
        let stale_now = get_lease_for_issue(&c, "o/r", 31).unwrap().unwrap();
        assert_eq!(stale_now.status, LeaseStatus::Stale);
        let _ = fresh;
    }

    #[test]
    fn list_by_config_and_all() {
        let c = conn();
        try_claim(&c, "o/r", 40, "cfg-a").unwrap();
        try_claim(&c, "o/r", 41, "cfg-b").unwrap();
        assert_eq!(list_leases_by_config(&c, "cfg-a").unwrap().len(), 1);
        assert_eq!(list_all_leases(&c).unwrap().len(), 2);
    }

    #[test]
    fn touch_heartbeat_updates_timestamp() {
        let c = conn();
        let lease = try_claim(&c, "o/r", 50, "cfg").unwrap().unwrap();
        let before = get_lease_for_issue(&c, "o/r", 50).unwrap().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        touch_lease_heartbeat(&c, &lease.lease_id).unwrap();
        let after = get_lease_for_issue(&c, "o/r", 50).unwrap().unwrap();
        assert!(after.heartbeat_at >= before.heartbeat_at);
    }

    #[test]
    fn create_lease_explicit_record() {
        let c = conn();
        let now = Utc::now();
        let lease = IssueLease {
            lease_id: "explicit-1".to_string(),
            issue_repo: "o/r".to_string(),
            issue_number: 60,
            config_id: "cfg".to_string(),
            run_id: Some("run-1".to_string()),
            status: LeaseStatus::Pending,
            claimed_at: now,
            updated_at: now,
            heartbeat_at: now,
        };
        create_lease(&c, &lease).unwrap();
        let fetched = get_lease_for_issue(&c, "o/r", 60).unwrap().unwrap();
        assert_eq!(fetched.lease_id, "explicit-1");
        assert_eq!(fetched.status, LeaseStatus::Pending);
    }
}
