//! Integration tests for the per-config daemon lifecycle (issue #48).
//!
//! Each test uses an isolated `TempDir` root via `DaemonStore::at` so daemon
//! state persistence never touches the real data directory.
//!
//! @plan:PLAN-20260404-INITIAL-RUNTIME.P09
//! @requirement:REQ-EARS-SVC-001
use std::process::Command;

use luther_workflow::daemon::{
    is_daemon_alive, stop_daemon, DaemonState, DaemonStatus, DaemonStore, StopOutcome,
};
use luther_workflow::monitor::{acquire_singleton_lock, process::MonitorError};
use tempfile::TempDir;

/// Spawn a long-lived child process and return its handle.
fn spawn_sleeper() -> std::process::Child {
    Command::new("sleep")
        .arg("86400")
        .spawn()
        .expect("spawn sleeper")
}

#[test]
fn duplicate_daemon_prevention() {
    // @plan:PLAN-20260404-INITIAL-RUNTIME.P09
    let tmp = TempDir::new().expect("tempdir");
    let store = DaemonStore::at(tmp.path());
    let config_id = "config-a";

    let lock_path = store.lock_path(config_id).to_string_lossy().to_string();
    let _guard = acquire_singleton_lock(&lock_path).expect("first lock acquires");
    store
        .write(&DaemonState::new(config_id).with_status(DaemonStatus::Running))
        .expect("write state");

    // A second acquisition for the same config must fail.
    let second = acquire_singleton_lock(&lock_path);
    assert!(
        matches!(second, Err(MonitorError::LockHeld { .. })),
        "second daemon for same config should be rejected"
    );

    // The first daemon's state file remains intact.
    let state = store.read(config_id).expect("state present");
    assert_eq!(state.config_id, config_id);
    assert_eq!(state.status, DaemonStatus::Running);
}

#[test]
fn multi_config_records_are_independent() {
    // @plan:PLAN-20260404-INITIAL-RUNTIME.P09
    let tmp = TempDir::new().expect("tempdir");
    let store = DaemonStore::at(tmp.path());

    for id in ["alpha", "bravo", "charlie"] {
        store.write(&DaemonState::new(id)).expect("write state");
    }

    let all = store.read_all();
    let ids: Vec<&str> = all.iter().map(|s| s.config_id.as_str()).collect();
    assert_eq!(ids, vec!["alpha", "bravo", "charlie"]);

    // Updating bravo does not change alpha's file.
    let alpha_before = store.read("alpha").expect("alpha present");
    let mut bravo = store.read("bravo").expect("bravo present");
    bravo.set_status(DaemonStatus::Stopped);
    store.write(&bravo).expect("update bravo");

    let alpha_after = store.read("alpha").expect("alpha present");
    assert_eq!(alpha_before, alpha_after);
    assert_eq!(
        store.read("bravo").expect("bravo").status,
        DaemonStatus::Stopped
    );
}

#[test]
fn status_rendering_single_and_aggregate() {
    // @plan:PLAN-20260404-INITIAL-RUNTIME.P09
    let tmp = TempDir::new().expect("tempdir");
    let store = DaemonStore::at(tmp.path());

    let running = DaemonState::new("running-cfg")
        .with_pid(1234)
        .with_status(DaemonStatus::Running);
    let stopped = DaemonState::new("stopped-cfg")
        .with_pid(5678)
        .with_status(DaemonStatus::Stopped);
    store.write(&running).expect("write running");
    store.write(&stopped).expect("write stopped");

    // JSON round-trip preserves required fields.
    let json = serde_json::to_string(&running).expect("serialize");
    assert!(json.contains("running-cfg"));
    assert!(json.contains("1234"));

    // Aggregate lists both configs.
    let all = store.read_all();
    assert_eq!(all.len(), 2);

    // Non-existent config yields no state.
    assert!(store.read("absent").is_none());
}

#[test]
fn stop_behavior_terminates_and_retains_state() {
    // @plan:PLAN-20260404-INITIAL-RUNTIME.P09
    let tmp = TempDir::new().expect("tempdir");
    let store = DaemonStore::at(tmp.path());

    let mut child = spawn_sleeper();
    let pid = child.id();
    let config_id = "stop-cfg";
    store
        .write(
            &DaemonState::new(config_id)
                .with_pid(pid)
                .with_status(DaemonStatus::Running),
        )
        .expect("write state");

    assert!(is_daemon_alive(pid), "child should be alive before stop");
    let outcome = stop_daemon(&store, config_id);
    assert_eq!(outcome, StopOutcome::Stopped);

    // Reap the (now-terminated) child so it is not left as a zombie, which
    // `kill -0` would still report as alive.
    let status = child.wait().expect("reap child");
    assert!(!status.success(), "child should have been signalled");

    // State file is retained, never deleted on stop.
    assert!(
        store.read(config_id).is_some(),
        "state.json must survive stop"
    );
}

#[test]
fn stop_is_idempotent_for_dead_and_absent() {
    // @plan:PLAN-20260404-INITIAL-RUNTIME.P09
    let tmp = TempDir::new().expect("tempdir");
    let store = DaemonStore::at(tmp.path());

    // Absent config => NotFound (idempotent, no error).
    assert_eq!(stop_daemon(&store, "absent"), StopOutcome::NotFound);

    // Dead PID => AlreadyStopped.
    store
        .write(
            &DaemonState::new("dead-cfg")
                .with_pid(4_000_000_000)
                .with_status(DaemonStatus::Running),
        )
        .expect("write state");
    assert_eq!(stop_daemon(&store, "dead-cfg"), StopOutcome::AlreadyStopped);
}

#[test]
fn stop_all_terminates_multiple_daemons() {
    // @plan:PLAN-20260404-INITIAL-RUNTIME.P09
    let tmp = TempDir::new().expect("tempdir");
    let store = DaemonStore::at(tmp.path());

    let mut a = spawn_sleeper();
    let mut b = spawn_sleeper();
    store
        .write(
            &DaemonState::new("cfg-a")
                .with_pid(a.id())
                .with_status(DaemonStatus::Running),
        )
        .expect("write a");
    store
        .write(
            &DaemonState::new("cfg-b")
                .with_pid(b.id())
                .with_status(DaemonStatus::Running),
        )
        .expect("write b");

    for state in store.read_all() {
        assert_eq!(stop_daemon(&store, &state.config_id), StopOutcome::Stopped);
    }

    // Reap both children to clear zombies before asserting termination.
    let status_a = a.wait().expect("reap a");
    let status_b = b.wait().expect("reap b");
    assert!(!status_a.success(), "cfg-a child should be signalled");
    assert!(!status_b.success(), "cfg-b child should be signalled");
}
