use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Result as SqliteResult};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitKind {
    PrChecks,
    CoderabbitReview,
    HumanReview,
    PrMerge,
    RateLimitBackoff,
    DependencyChildMerge,
}

impl std::fmt::Display for WaitKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            WaitKind::PrChecks => "pr_checks",
            WaitKind::CoderabbitReview => "coderabbit_review",
            WaitKind::HumanReview => "human_review",
            WaitKind::PrMerge => "pr_merge",
            WaitKind::RateLimitBackoff => "rate_limit_backoff",
            WaitKind::DependencyChildMerge => "dependency_child_merge",
        };
        write!(f, "{s}")
    }
}

impl std::str::FromStr for WaitKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pr_checks" => Ok(WaitKind::PrChecks),
            "coderabbit_review" => Ok(WaitKind::CoderabbitReview),
            "human_review" => Ok(WaitKind::HumanReview),
            "pr_merge" => Ok(WaitKind::PrMerge),
            "rate_limit_backoff" => Ok(WaitKind::RateLimitBackoff),
            "dependency_child_merge" => Ok(WaitKind::DependencyChildMerge),
            _ => Err(format!("Unknown wait kind: {s}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaitStateRecord {
    pub run_id: String,
    pub lease_id: Option<String>,
    pub workflow_type: String,
    pub config_id: String,
    pub repository: String,
    pub issue_number: u64,
    pub pr_number: Option<u64>,
    pub head_sha: Option<String>,
    pub wait_kind: WaitKind,
    pub wait_condition: serde_json::Value,
    pub last_observed_state: serde_json::Value,
    pub next_poll_at: DateTime<Utc>,
    pub poll_interval_seconds: u64,
    pub max_wait_seconds: Option<u64>,
    pub resume_step: String,
    pub checkpoint_id: String,
    pub poll_count: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl WaitStateRecord {
    #[must_use]
    pub fn new(run_id: impl Into<String>, config_id: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            run_id: run_id.into(),
            lease_id: None,
            workflow_type: String::new(),
            config_id: config_id.into(),
            repository: String::new(),
            issue_number: 0,
            pr_number: None,
            head_sha: None,
            wait_kind: WaitKind::PrChecks,
            wait_condition: serde_json::Value::Null,
            last_observed_state: serde_json::Value::Null,
            next_poll_at: now,
            poll_interval_seconds: 300,
            max_wait_seconds: None,
            resume_step: String::new(),
            checkpoint_id: String::new(),
            poll_count: 0,
            created_at: now,
            updated_at: now,
        }
    }
}

pub fn init_wait_states_table(conn: &Connection) -> SqliteResult<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS wait_states (
            run_id TEXT PRIMARY KEY,
            lease_id TEXT,
            workflow_type TEXT NOT NULL,
            config_id TEXT NOT NULL,
            repository TEXT NOT NULL,
            issue_number INTEGER NOT NULL,
            pr_number INTEGER,
            head_sha TEXT,
            wait_kind TEXT NOT NULL,
            wait_condition_json TEXT NOT NULL,
            last_observed_state_json TEXT NOT NULL,
            next_poll_at TEXT NOT NULL,
            poll_interval_seconds INTEGER NOT NULL,
            max_wait_seconds INTEGER,
            resume_step TEXT NOT NULL,
            checkpoint_id TEXT NOT NULL,
            poll_count INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_wait_states_pollable
            ON wait_states (next_poll_at, config_id, repository)",
        [],
    )?;
    Ok(())
}

pub fn upsert_wait_state(conn: &Connection, record: &WaitStateRecord) -> SqliteResult<()> {
    conn.execute(
        "INSERT INTO wait_states
            (run_id, lease_id, workflow_type, config_id, repository, issue_number,
             pr_number, head_sha, wait_kind, wait_condition_json,
             last_observed_state_json, next_poll_at, poll_interval_seconds,
             max_wait_seconds, resume_step, checkpoint_id, poll_count, created_at,
             updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                 ?15, ?16, ?17, ?18, ?19)
         ON CONFLICT(run_id) DO UPDATE SET
             lease_id = excluded.lease_id,
             workflow_type = excluded.workflow_type,
             config_id = excluded.config_id,
             repository = excluded.repository,
             issue_number = excluded.issue_number,
             pr_number = excluded.pr_number,
             head_sha = excluded.head_sha,
             wait_kind = excluded.wait_kind,
             wait_condition_json = excluded.wait_condition_json,
             last_observed_state_json = excluded.last_observed_state_json,
             next_poll_at = excluded.next_poll_at,
             poll_interval_seconds = excluded.poll_interval_seconds,
             max_wait_seconds = excluded.max_wait_seconds,
             resume_step = excluded.resume_step,
             checkpoint_id = excluded.checkpoint_id,
             poll_count = excluded.poll_count,
             updated_at = excluded.updated_at",
        record_params(record)?,
    )?;
    Ok(())
}

pub fn get_wait_state(conn: &Connection, run_id: &str) -> SqliteResult<Option<WaitStateRecord>> {
    conn.query_row(
        &format!("{SELECT_COLUMNS} WHERE run_id = ?1"),
        params![run_id],
        row_to_wait_state,
    )
    .optional()
}

pub fn list_wait_states(conn: &Connection) -> SqliteResult<Vec<WaitStateRecord>> {
    let mut stmt = conn.prepare(&format!(
        "{SELECT_COLUMNS} ORDER BY next_poll_at, repository, issue_number"
    ))?;
    collect_wait_states(&mut stmt, [])
}

pub fn list_pollable_wait_states(
    conn: &Connection,
    now: DateTime<Utc>,
) -> SqliteResult<Vec<WaitStateRecord>> {
    let mut stmt = conn.prepare(&format!(
        "{SELECT_COLUMNS}
         WHERE next_poll_at <= ?1
           AND EXISTS (
               SELECT 1 FROM issue_leases
               WHERE issue_leases.lease_id = wait_states.lease_id
                 AND issue_leases.status IN (
                     -- Keep in sync with LeaseStatus::blocks_duplicate_work().
                     'waiting_external', 'ready_to_resume', 'claimed', 'running'
                 )
           )
         ORDER BY next_poll_at, repository, issue_number"
    ))?;
    collect_wait_states(&mut stmt, params![now.to_rfc3339()])
}

pub fn update_wait_state_after_poll(
    conn: &Connection,
    run_id: &str,
    last_observed_state: &serde_json::Value,
    next_poll_at: DateTime<Utc>,
) -> SqliteResult<bool> {
    let rows = conn.execute(
        "UPDATE wait_states
         SET last_observed_state_json = ?1,
             next_poll_at = ?2,
             poll_count = poll_count + 1,
             updated_at = ?3
         WHERE run_id = ?4",
        params![
            last_observed_state.to_string(),
            next_poll_at.to_rfc3339(),
            Utc::now().to_rfc3339(),
            run_id,
        ],
    )?;
    Ok(rows > 0)
}

pub fn delete_wait_state(conn: &Connection, run_id: &str) -> SqliteResult<bool> {
    let deleted = conn.execute("DELETE FROM wait_states WHERE run_id = ?1", params![run_id])?;
    Ok(deleted > 0)
}

const SELECT_COLUMNS: &str =
    "SELECT run_id, lease_id, workflow_type, config_id, repository, issue_number, \
     pr_number, head_sha, wait_kind, wait_condition_json, last_observed_state_json, \
     next_poll_at, poll_interval_seconds, max_wait_seconds, resume_step, \
     checkpoint_id, poll_count, created_at, updated_at FROM wait_states";

fn record_params(record: &WaitStateRecord) -> SqliteResult<[Box<dyn rusqlite::ToSql>; 19]> {
    Ok([
        Box::new(record.run_id.clone()),
        Box::new(record.lease_id.clone()),
        Box::new(record.workflow_type.clone()),
        Box::new(record.config_id.clone()),
        Box::new(record.repository.clone()),
        Box::new(to_sql_i64(record.issue_number)?),
        Box::new(record.pr_number.map(to_sql_i64).transpose()?),
        Box::new(record.head_sha.clone()),
        Box::new(record.wait_kind.to_string()),
        Box::new(record.wait_condition.to_string()),
        Box::new(record.last_observed_state.to_string()),
        Box::new(record.next_poll_at.to_rfc3339()),
        Box::new(to_sql_i64(record.poll_interval_seconds)?),
        Box::new(record.max_wait_seconds.map(to_sql_i64).transpose()?),
        Box::new(record.resume_step.clone()),
        Box::new(record.checkpoint_id.clone()),
        Box::new(to_sql_i64(record.poll_count)?),
        Box::new(record.created_at.to_rfc3339()),
        Box::new(record.updated_at.to_rfc3339()),
    ])
}

fn to_sql_i64(value: u64) -> SqliteResult<i64> {
    i64::try_from(value).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
}

fn collect_wait_states<P>(
    stmt: &mut rusqlite::Statement<'_>,
    params: P,
) -> SqliteResult<Vec<WaitStateRecord>>
where
    P: rusqlite::Params,
{
    let rows = stmt.query_map(params, row_to_wait_state)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn row_to_wait_state(row: &rusqlite::Row<'_>) -> SqliteResult<WaitStateRecord> {
    Ok(WaitStateRecord {
        run_id: row.get(0)?,
        lease_id: row.get(1)?,
        workflow_type: row.get(2)?,
        config_id: row.get(3)?,
        repository: row.get(4)?,
        issue_number: nonnegative_i64_to_u64(row.get(5)?, 5)?,
        pr_number: optional_nonnegative_i64_to_u64(row.get(6)?, 6)?,
        head_sha: row.get(7)?,
        wait_kind: parse_wait_kind(&row.get::<_, String>(8)?, 8)?,
        wait_condition: parse_json(&row.get::<_, String>(9)?, 9)?,
        last_observed_state: parse_json(&row.get::<_, String>(10)?, 10)?,
        next_poll_at: parse_ts(&row.get::<_, String>(11)?, 11)?,
        poll_interval_seconds: nonnegative_i64_to_u64(row.get(12)?, 12)?,
        max_wait_seconds: optional_nonnegative_i64_to_u64(row.get(13)?, 13)?,
        resume_step: row.get(14)?,
        checkpoint_id: row.get(15)?,
        poll_count: nonnegative_i64_to_u64(row.get(16)?, 16)?,
        created_at: parse_ts(&row.get::<_, String>(17)?, 17)?,
        updated_at: parse_ts(&row.get::<_, String>(18)?, 18)?,
    })
}

fn parse_wait_kind(s: &str, col: usize) -> SqliteResult<WaitKind> {
    s.parse::<WaitKind>().map_err(|e| {
        conversion_error(
            col,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(e)),
        )
    })
}

fn parse_json(s: &str, col: usize) -> SqliteResult<serde_json::Value> {
    serde_json::from_str(s)
        .map_err(|e| conversion_error(col, rusqlite::types::Type::Text, Box::new(e)))
}

fn parse_ts(s: &str, col: usize) -> SqliteResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| conversion_error(col, rusqlite::types::Type::Text, Box::new(e)))
}

fn nonnegative_i64_to_u64(value: i64, col: usize) -> SqliteResult<u64> {
    u64::try_from(value)
        .map_err(|e| conversion_error(col, rusqlite::types::Type::Integer, Box::new(e)))
}

fn optional_nonnegative_i64_to_u64(value: Option<i64>, col: usize) -> SqliteResult<Option<u64>> {
    value.map(|n| nonnegative_i64_to_u64(n, col)).transpose()
}

fn conversion_error(
    col: usize,
    col_type: rusqlite::types::Type,
    error: Box<dyn std::error::Error + Send + Sync>,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(col, col_type, error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use serde_json::json;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::persistence::leases::init_leases_table(&c).unwrap();
        init_wait_states_table(&c).unwrap();
        c
    }

    fn record(run_id: &str, next_poll_at: DateTime<Utc>) -> WaitStateRecord {
        let mut record = WaitStateRecord::new(run_id, "cfg");
        record.lease_id = Some(run_id.to_string());
        record.workflow_type = "issue-fix".to_string();
        record.repository = "o/r".to_string();
        record.issue_number = 62;
        record.pr_number = Some(7);
        record.wait_condition = json!({ "checks": "pending" });
        record.next_poll_at = next_poll_at;
        record.resume_step = "collect_ci_failures".to_string();
        record.checkpoint_id = "cp-1".to_string();
        record
    }

    fn insert_lease(c: &Connection, run_id: &str, status: &str) {
        let issue_number = run_id
            .bytes()
            .fold(0_i64, |acc, byte| acc + i64::from(byte));
        c.execute(
            "INSERT INTO issue_leases
                (lease_id, issue_repo, issue_number, config_id, run_id, status,
                 claimed_at, updated_at, heartbeat_at)
             VALUES (?1,'o/r',?2,'cfg',?1,?3,?4,?4,?4)",
            params![run_id, issue_number, status, Utc::now().to_rfc3339()],
        )
        .unwrap();
    }

    #[test]
    fn wait_kind_roundtrips() {
        for kind in [
            WaitKind::PrChecks,
            WaitKind::CoderabbitReview,
            WaitKind::HumanReview,
            WaitKind::PrMerge,
            WaitKind::RateLimitBackoff,
            WaitKind::DependencyChildMerge,
        ] {
            assert_eq!(kind.to_string().parse::<WaitKind>().unwrap(), kind);
        }
    }

    #[test]
    fn upsert_get_and_delete_roundtrip() {
        let c = conn();
        let now = Utc::now();
        upsert_wait_state(&c, &record("run-1", now)).unwrap();
        let fetched = get_wait_state(&c, "run-1").unwrap().unwrap();
        assert_eq!(fetched.repository, "o/r");
        assert_eq!(fetched.wait_kind, WaitKind::PrChecks);
        assert!(delete_wait_state(&c, "run-1").unwrap());
        assert!(get_wait_state(&c, "run-1").unwrap().is_none());
    }

    #[test]
    fn list_pollable_orders_due_records_only() {
        let c = conn();
        let now = Utc::now();
        insert_lease(&c, "run-later", "waiting_external");
        insert_lease(&c, "run-now", "waiting_external");
        insert_lease(&c, "run-earlier", "waiting_external");
        upsert_wait_state(&c, &record("run-later", now + Duration::minutes(5))).unwrap();
        upsert_wait_state(&c, &record("run-now", now)).unwrap();
        upsert_wait_state(&c, &record("run-earlier", now - Duration::minutes(5))).unwrap();
        let due = list_pollable_wait_states(&c, now).unwrap();
        let run_ids: Vec<&str> = due.iter().map(|r| r.run_id.as_str()).collect();
        assert_eq!(run_ids, vec!["run-earlier", "run-now"]);
    }

    #[test]
    fn list_pollable_excludes_waits_without_protective_lease() {
        let c = conn();
        let now = Utc::now();
        insert_lease(&c, "run-active", "waiting_external");
        insert_lease(&c, "run-done", "completed");
        upsert_wait_state(&c, &record("run-active", now)).unwrap();
        upsert_wait_state(&c, &record("run-done", now)).unwrap();
        upsert_wait_state(&c, &record("run-orphan", now)).unwrap();

        let due = list_pollable_wait_states(&c, now).unwrap();

        let run_ids: Vec<&str> = due.iter().map(|r| r.run_id.as_str()).collect();
        assert_eq!(run_ids, vec!["run-active"]);
    }

    #[test]
    fn list_pollable_includes_ready_to_resume_and_active_protective_leases() {
        let c = conn();
        let now = Utc::now();
        insert_lease(&c, "run-ready", "ready_to_resume");
        insert_lease(&c, "run-running", "running");
        upsert_wait_state(&c, &record("run-ready", now)).unwrap();
        upsert_wait_state(&c, &record("run-running", now)).unwrap();

        let due = list_pollable_wait_states(&c, now).unwrap();

        let run_ids: Vec<&str> = due.iter().map(|r| r.run_id.as_str()).collect();
        assert_eq!(run_ids, vec!["run-ready", "run-running"]);
    }

    #[test]
    fn update_after_poll_records_backoff_and_count() {
        let c = conn();
        let next = Utc::now() + Duration::minutes(10);
        upsert_wait_state(&c, &record("run-1", Utc::now())).unwrap();
        update_wait_state_after_poll(&c, "run-1", &json!({ "state": "pending" }), next).unwrap();
        let fetched = get_wait_state(&c, "run-1").unwrap().unwrap();
        assert_eq!(fetched.poll_count, 1);
        assert_eq!(fetched.last_observed_state, json!({ "state": "pending" }));
        assert_eq!(fetched.next_poll_at, next);
    }
}
