//! Integration tests for daemon queue listing (issue #49).
//!
//! Seeds the `issue_leases` table across statuses in a temp database and
//! asserts grouping/listing and `--status` filtering at the persistence-query
//! level used by the `daemon queue` command.
//!
//! @plan:PLAN-20260415-DAEMON-DISCOVERY.P05
//! @requirement:REQ-DAEMON-DISCOVERY-002
use luther_workflow::persistence::leases::{
    create_lease, init_leases_table, list_all_leases, list_leases_by_config, list_leases_by_status,
    IssueLease, LeaseStatus,
};
use rusqlite::Connection;

fn lease(num: u64, config_id: &str, status: LeaseStatus) -> IssueLease {
    let now = chrono::Utc::now();
    IssueLease {
        lease_id: format!("lease-{config_id}-{num}"),
        issue_repo: "owner/repo".to_string(),
        issue_number: num,
        config_id: config_id.to_string(),
        run_id: None,
        status,
        claimed_at: now,
        updated_at: now,
        heartbeat_at: now,
    }
}

fn seeded_db() -> Connection {
    let conn = Connection::open_in_memory().expect("open db");
    init_leases_table(&conn).expect("init");
    create_lease(&conn, &lease(1, "cfg-a", LeaseStatus::Pending)).unwrap();
    create_lease(&conn, &lease(2, "cfg-a", LeaseStatus::Running)).unwrap();
    create_lease(&conn, &lease(3, "cfg-b", LeaseStatus::Completed)).unwrap();
    create_lease(&conn, &lease(4, "cfg-b", LeaseStatus::Failed)).unwrap();
    conn
}

#[test]
fn list_all_returns_every_lease() {
    let conn = seeded_db();
    let all = list_all_leases(&conn).expect("list all");
    assert_eq!(all.len(), 4);
}

#[test]
fn list_by_config_filters_to_one_config() {
    let conn = seeded_db();
    let a = list_leases_by_config(&conn, "cfg-a").expect("by config");
    assert_eq!(a.len(), 2);
    assert!(a.iter().all(|l| l.config_id == "cfg-a"));
}

#[test]
fn list_by_status_filters_to_one_status() {
    let conn = seeded_db();
    let running = list_leases_by_status(&conn, LeaseStatus::Running).expect("by status");
    assert_eq!(running.len(), 1);
    assert_eq!(running[0].issue_number, 2);

    let failed = list_leases_by_status(&conn, LeaseStatus::Failed).expect("by status");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].issue_number, 4);
}
