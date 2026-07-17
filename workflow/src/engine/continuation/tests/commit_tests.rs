//! Transactional commit tests for continuation.

use super::support::*;
use crate::engine::continuation::{
    checkpoint_identity, commit_continuation, select_checkpoint, ContinuationKind,
};
use crate::persistence::{
    get_checkpoint_for_step, get_run_with_conn, persist_run_with_conn, RunStatus,
    CHECKPOINT_STATUS_WAITING,
};

#[test]
fn cleanup_abandonment_allows_live_daemon_host_pid_when_lease_is_protected() {
    let conn = test_conn();
    let workspace = tempfile::tempdir().expect("workspace");
    seed_cleanup_abandonment(&conn, "cleanup-live", workspace.path());
    let mut metadata = get_run_with_conn(&conn, "cleanup-live")
        .expect("query")
        .expect("run");
    metadata.process_pid = Some(std::process::id());
    persist_run_with_conn(&conn, &metadata).expect("persist daemon PID");
    let forced = request(
        "cleanup-live",
        ContinuationKind::Retry {
            from_failed_step: false,
        },
        true,
    );
    let cp = select_checkpoint(&conn, &forced, &metadata).expect("select checkpoint");
    commit_continuation(&conn, &forced, &checkpoint_identity(&cp))
        .expect("protected terminal recovery");
}

#[test]
fn commit_continuation_reopens_run_and_rearms_checkpoint() {
    let conn = test_conn();
    seed_terminal_failed_run(&conn, "run-10");
    let req = request("run-10", ContinuationKind::Resume, false);
    let md = get_run_with_conn(&conn, "run-10").unwrap().unwrap();
    let cp = select_checkpoint(&conn, &req, &md).expect("select");
    let identity = checkpoint_identity(&cp);
    let md = commit_continuation(&conn, &req, &identity).expect("commit");
    assert_eq!(md.status, RunStatus::Running);
    // The re-stamped checkpoint becomes the newest and is ready_to_resume.
    let newest = crate::persistence::load_checkpoint_with_conn(&conn, "run-10")
        .unwrap()
        .unwrap();
    assert_eq!(newest.step_id, "collect_ci_failures");
    assert_eq!(
        newest.state_snapshot.status,
        crate::persistence::CHECKPOINT_STATUS_READY_TO_RESUME
    );
}

#[test]
fn cleanup_recovery_authorization_is_consumed_on_commit() {
    let conn = test_conn();
    let workspace = tempfile::tempdir().expect("workspace");
    seed_cleanup_abandonment(&conn, "run-cleanup-consumed", workspace.path());
    let request = request(
        "run-cleanup-consumed",
        ContinuationKind::Retry {
            from_failed_step: true,
        },
        true,
    );

    let md_before = get_run_with_conn(&conn, "run-cleanup-consumed")
        .unwrap()
        .unwrap();
    let cp = select_checkpoint(&conn, &request, &md_before).expect("select");
    let metadata = commit_continuation(&conn, &request, &checkpoint_identity(&cp)).expect("commit");
    assert_eq!(metadata.status, RunStatus::Running);
    assert!(metadata
        .failure_cleanup
        .as_ref()
        .is_some_and(|state| state.recovery_consumed_at.is_some()));

    let mut later = metadata;
    later.status = RunStatus::Abandoned;
    crate::persistence::persist_run_with_conn(&conn, &later).expect("persist later abandonment");
    let validation =
        crate::engine::continuation::validate_continuation(&conn, &request).expect("validation");
    assert!(!validation.ok);
    assert!(validation
        .checks
        .iter()
        .any(|check| check.name == "resumable_status" && !check.passed));
}

#[test]
fn commit_rejects_live_running_resume_point_without_force() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    let conn = test_conn();
    seed_run(
        &conn,
        "commit-live-running",
        RunStatus::Running,
        "watch_pr_checks",
    );
    let mut md = get_run_with_conn(&conn, "commit-live-running")
        .expect("load run")
        .expect("run exists");
    md.process_pid = Some(std::process::id());
    persist_run_with_conn(&conn, &md).expect("persist live pid");
    seed_checkpoint(
        &conn,
        "commit-live-running",
        "watch_pr_checks",
        CHECKPOINT_STATUS_WAITING,
    );
    let identity = checkpoint_identity(
        &get_checkpoint_for_step(&conn, "commit-live-running", "watch_pr_checks")
            .unwrap()
            .unwrap(),
    );
    let req = request("commit-live-running", ContinuationKind::Resume, false);
    let err = commit_continuation(&conn, &req, &identity)
        .expect_err("live running claim must be rejected");
    assert!(err
        .to_string()
        .contains("already running with live workflow PID"));
}

#[test]
fn commit_rejects_live_running_resume_point_even_with_force() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    let conn = test_conn();
    seed_run(
        &conn,
        "commit-force-live-running",
        RunStatus::Running,
        "watch_pr_checks",
    );
    let mut md = get_run_with_conn(&conn, "commit-force-live-running")
        .expect("load run")
        .expect("run exists");
    md.process_pid = Some(std::process::id());
    persist_run_with_conn(&conn, &md).expect("persist live pid");
    seed_checkpoint(
        &conn,
        "commit-force-live-running",
        "watch_pr_checks",
        CHECKPOINT_STATUS_WAITING,
    );
    let identity = checkpoint_identity(
        &get_checkpoint_for_step(&conn, "commit-force-live-running", "watch_pr_checks")
            .unwrap()
            .unwrap(),
    );
    let req = request("commit-force-live-running", ContinuationKind::Resume, true);
    let error = commit_continuation(&conn, &req, &identity)
        .expect_err("force must not override live process ownership");

    assert!(error
        .to_string()
        .contains("already running with live workflow PID"));
}

#[test]
fn commit_accepts_unrecorded_pid_running_resume_point() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    let conn = test_conn();
    seed_run(
        &conn,
        "commit-unrecorded-running",
        RunStatus::Running,
        "watch_pr_checks",
    );
    seed_checkpoint(
        &conn,
        "commit-unrecorded-running",
        "watch_pr_checks",
        CHECKPOINT_STATUS_WAITING,
    );
    let identity = checkpoint_identity(
        &get_checkpoint_for_step(&conn, "commit-unrecorded-running", "watch_pr_checks")
            .unwrap()
            .unwrap(),
    );
    let req = request("commit-unrecorded-running", ContinuationKind::Resume, false);
    let metadata = commit_continuation(&conn, &req, &identity)
        .expect("unrecorded running claim should reopen");
    assert_eq!(metadata.status, RunStatus::Running);
    assert_eq!(metadata.process_pid, Some(std::process::id()));
}

#[cfg(unix)]
#[test]
fn commit_accepts_stale_running_resume_point() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    let conn = test_conn();
    seed_run(
        &conn,
        "commit-stale-running",
        RunStatus::Running,
        "watch_pr_checks",
    );
    let mut md = get_run_with_conn(&conn, "commit-stale-running")
        .expect("load run")
        .expect("run exists");
    let mut stale_process = short_lived_process().expect("spawn short-lived process");
    let stale_pid = stale_process.id();
    stale_process.wait().expect("wait for short-lived process");
    md.process_pid = Some(stale_pid);
    persist_run_with_conn(&conn, &md).expect("persist stale pid");
    seed_checkpoint(
        &conn,
        "commit-stale-running",
        "watch_pr_checks",
        CHECKPOINT_STATUS_WAITING,
    );
    let req = request("commit-stale-running", ContinuationKind::Resume, false);
    let identity = checkpoint_identity(
        &get_checkpoint_for_step(&conn, "commit-stale-running", "watch_pr_checks")
            .unwrap()
            .unwrap(),
    );
    let metadata =
        commit_continuation(&conn, &req, &identity).expect("stale running claim should reopen");
    assert_eq!(metadata.status, RunStatus::Running);
    assert_eq!(metadata.process_pid, Some(std::process::id()));
}
