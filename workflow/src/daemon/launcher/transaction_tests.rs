use std::sync::{Arc, Barrier};

use super::*;
use crate::persistence::checkpoint::{save_checkpoint_with_conn, Checkpoint};
use crate::persistence::leases::{get_lease_for_issue, init_leases_table, try_claim};
use crate::persistence::wait_state::{init_wait_states_table, WaitStateRecord};
use crate::persistence::{persist_external_wait, persist_run_with_conn, RunMetadata};

fn seed_complete_wait(
    db_path: &std::path::Path,
    issue_number: u64,
    run_id: &str,
) -> (rusqlite::Connection, String) {
    let conn = rusqlite::Connection::open(db_path).expect("open db");
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .expect("set timeout");
    init_leases_table(&conn).expect("init leases");
    crate::persistence::sqlite::init_runs_schema(&conn).expect("init runs");
    init_wait_states_table(&conn).expect("init waits");
    let lease = try_claim(&conn, "o/r", issue_number, "cfg")
        .expect("claim")
        .expect("lease");
    update_lease_status(&conn, &lease.lease_id, LeaseStatus::Running, Some(run_id))
        .expect("set running");
    persist_run_with_conn(&conn, &RunMetadata::new(run_id, "wf", "cfg")).expect("persist run");
    save_checkpoint_with_conn(&conn, &Checkpoint::new(run_id, "watch_pr_checks"))
        .expect("checkpoint");
    let mut wait = WaitStateRecord::new(run_id, "cfg");
    wait.lease_id = Some(lease.lease_id.clone());
    wait.repository = "o/r".to_string();
    wait.issue_number = issue_number;
    wait.resume_step = "watch_pr_checks".to_string();
    persist_external_wait(&conn, &wait).expect("persist wait");
    (conn, lease.lease_id)
}

#[test]
fn complete_wait_survives_launch_error_compensation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (conn, lease_id) = seed_complete_wait(&temp.path().join("wait.db"), 40, "run-wait");
    let outcome = finish_lease_after_result(
        &conn,
        &lease_id,
        "run-wait",
        Err("downstream error".to_string()),
    )
    .expect("compensation");
    assert_eq!(
        outcome,
        LaunchOutcome::WaitingExternal {
            run_id: "run-wait".to_string(),
        }
    );
    assert_eq!(
        get_lease_for_issue(&conn, "o/r", 40)
            .expect("read lease")
            .expect("lease")
            .status,
        LeaseStatus::WaitingExternal
    );
}

#[test]
fn concurrent_ready_transition_is_not_overwritten_by_compensation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("race.db");
    let (conn, lease_id) = seed_complete_wait(&db_path, 41, "run-race");
    let barrier = Arc::new(Barrier::new(2));
    let thread_barrier = Arc::clone(&barrier);
    let thread_lease = lease_id.clone();
    let handle = std::thread::spawn(move || {
        let poller = rusqlite::Connection::open(db_path).expect("open poller");
        poller
            .busy_timeout(std::time::Duration::from_secs(5))
            .expect("set timeout");
        thread_barrier.wait();
        let _ = update_lease_status(
            &poller,
            &thread_lease,
            LeaseStatus::ReadyToResume,
            Some("run-race"),
        );
    });
    barrier.wait();
    let outcome = finish_lease_after_result(
        &conn,
        &lease_id,
        "run-race",
        Err("concurrent error".to_string()),
    )
    .expect("compensation");
    handle.join().expect("poller");
    let durable = get_lease_for_issue(&conn, "o/r", 41)
        .expect("read lease")
        .expect("lease");
    assert_eq!(durable.status, LeaseStatus::ReadyToResume);
    assert!(matches!(
        outcome,
        LaunchOutcome::WaitingExternal { .. } | LaunchOutcome::LeaseStatePreserved { .. }
    ));
}
