//! Validation (safety check) tests for continuation requests.

use super::support::*;
use crate::engine::continuation::{validate_continuation, ContinuationKind, RewindTarget};
use crate::persistence::{
    get_run_with_conn, persist_run_with_conn, RunMetadata, RunStatus, CHECKPOINT_STATUS_WAITING,
};

#[test]
fn validation_fails_when_run_missing() {
    let conn = test_conn();
    let req = request("absent", ContinuationKind::Resume, false);
    let validation = validate_continuation(&conn, &req).expect("validate");
    assert!(!validation.ok);
    assert!(validation
        .failure_reasons()
        .iter()
        .any(|r| r.contains("run_exists")));
}

#[test]
fn validation_passes_for_terminal_failed_resume() {
    let conn = test_conn();
    seed_terminal_failed_run(&conn, "run-1");
    let req = request("run-1", ContinuationKind::Resume, false);
    let validation = validate_continuation(&conn, &req).expect("validate");
    assert!(validation.ok, "reasons: {:?}", validation.failure_reasons());
}

#[test]
fn validation_rejects_unsafe_step_without_force() {
    let conn = test_conn();
    seed_run(&conn, "run-2", RunStatus::Failed, "implement");
    seed_checkpoint(&conn, "run-2", "implement", "completed");
    let req = request(
        "run-2",
        ContinuationKind::Rewind {
            target: RewindTarget::ToStep("implement".to_string()),
        },
        false,
    );
    let validation = validate_continuation(&conn, &req).expect("validate");
    assert!(!validation.ok);
    assert!(validation
        .failure_reasons()
        .iter()
        .any(|r| r.contains("safe_step")));
}

#[test]
fn validation_rejects_unsafe_step_even_with_force() {
    let conn = test_conn();
    seed_run(&conn, "run-3", RunStatus::Failed, "implement");
    seed_checkpoint(&conn, "run-3", "implement", "completed");
    let req = request(
        "run-3",
        ContinuationKind::Rewind {
            target: RewindTarget::ToStep("implement".to_string()),
        },
        true,
    );
    let validation = validate_continuation(&conn, &req).expect("validate");
    assert!(!validation.ok);
    assert!(validation
        .failure_reasons()
        .iter()
        .any(|reason| reason.contains("safe_step")));
}

#[test]
fn validation_rejects_missing_rewind_step() {
    let conn = test_conn();
    seed_terminal_failed_run(&conn, "run-4");
    let req = request(
        "run-4",
        ContinuationKind::Rewind {
            target: RewindTarget::ToStep("does_not_exist".to_string()),
        },
        false,
    );
    let validation = validate_continuation(&conn, &req).expect("validate");
    assert!(!validation.ok);
    assert!(validation
        .failure_reasons()
        .iter()
        .any(|r| r.contains("checkpoint_exists")));
}

#[test]
fn validation_rejects_repo_only_identity() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    let conn = test_conn();
    let mut md = RunMetadata::new("anchorless", "wf", "cfg");
    md.status = RunStatus::Failed;
    md.current_step = Some("watch_pr_checks".to_string());
    md.repository = Some("vybestack/llxprt-code".to_string());
    // Neither issue_number nor pr_number recorded.
    persist_run_with_conn(&conn, &md).expect("persist run");
    seed_checkpoint(
        &conn,
        "anchorless",
        "watch_pr_checks",
        CHECKPOINT_STATUS_WAITING,
    );
    let req = request("anchorless", ContinuationKind::Resume, false);
    let validation = validate_continuation(&conn, &req).expect("validate");
    assert!(!validation.ok);
    assert!(validation
        .failure_reasons()
        .iter()
        .any(|r| r.contains("identity_recoverable")));
}

#[test]
fn validation_rejects_completed_run() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    assert_non_resumable_rejected(RunStatus::Completed);
}

#[test]
fn validation_rejects_merged_run() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    assert_non_resumable_rejected(RunStatus::Merged);
}

#[test]
fn validation_rejects_abandoned_run() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    assert_non_resumable_rejected(RunStatus::Abandoned);
}

#[test]
fn validation_rejects_cancelled_run() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    assert_non_resumable_rejected(RunStatus::Cancelled);
}

#[test]
fn validation_accepts_resumable_statuses() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    for status in [
        RunStatus::Failed,
        RunStatus::WaitingForChecks,
        RunStatus::WaitingExternal,
        RunStatus::ReadyToResume,
        RunStatus::Paused,
        RunStatus::Blocked,
    ] {
        let conn = test_conn();
        seed_run(&conn, "ok", status.clone(), "watch_pr_checks");
        seed_checkpoint(&conn, "ok", "watch_pr_checks", CHECKPOINT_STATUS_WAITING);
        let req = request("ok", ContinuationKind::Resume, false);
        let validation = validate_continuation(&conn, &req).expect("validate");
        assert!(
            validation.ok,
            "status {status:?} should be resumable; reasons: {:?}",
            validation.failure_reasons()
        );
    }
}

#[cfg(unix)]
#[test]
fn validation_accepts_stale_running_resume_point() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    let conn = test_conn();
    seed_run(
        &conn,
        "stale-running",
        RunStatus::Running,
        "watch_pr_checks",
    );
    let mut md = get_run_with_conn(&conn, "stale-running")
        .expect("load run")
        .expect("run exists");
    let mut stale_process = short_lived_process().expect("spawn short-lived process");
    let stale_pid = stale_process.id();
    stale_process.wait().expect("wait for short-lived process");
    md.process_pid = Some(stale_pid);
    persist_run_with_conn(&conn, &md).expect("persist stale pid");
    seed_checkpoint(
        &conn,
        "stale-running",
        "watch_pr_checks",
        CHECKPOINT_STATUS_WAITING,
    );
    let req = request("stale-running", ContinuationKind::Resume, false);
    let validation = validate_continuation(&conn, &req).expect("validate");
    assert!(
        validation.ok,
        "stale running run should be resumable; reasons: {:?}",
        validation.failure_reasons()
    );
}

#[test]
fn validation_accepts_unrecorded_pid_running_resume_point() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    let conn = test_conn();
    seed_run(
        &conn,
        "unrecorded-running",
        RunStatus::Running,
        "watch_pr_checks",
    );
    seed_checkpoint(
        &conn,
        "unrecorded-running",
        "watch_pr_checks",
        CHECKPOINT_STATUS_WAITING,
    );
    let req = request("unrecorded-running", ContinuationKind::Resume, false);
    let validation = validate_continuation(&conn, &req).expect("validate");
    assert!(
        validation.ok,
        "running run with no recorded PID should be resumable; reasons: {:?}",
        validation.failure_reasons()
    );
}

#[test]
fn validation_rejects_live_running_resume_point_even_with_force() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    let conn = test_conn();
    seed_run(
        &conn,
        "force-live-running",
        RunStatus::Running,
        "watch_pr_checks",
    );
    let mut md = get_run_with_conn(&conn, "force-live-running")
        .expect("load run")
        .expect("run exists");
    md.process_pid = Some(std::process::id());
    persist_run_with_conn(&conn, &md).expect("persist live pid");
    seed_checkpoint(
        &conn,
        "force-live-running",
        "watch_pr_checks",
        CHECKPOINT_STATUS_WAITING,
    );
    let req = request("force-live-running", ContinuationKind::Resume, true);
    let validation = validate_continuation(&conn, &req).expect("validate");
    assert!(
        !validation.ok,
        "--force must not bypass live process ownership"
    );
}

#[test]
fn validation_rejects_live_running_resume_point() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    let conn = test_conn();
    seed_run(&conn, "live-running", RunStatus::Running, "watch_pr_checks");
    let mut md = get_run_with_conn(&conn, "live-running")
        .expect("load run")
        .expect("run exists");
    md.process_pid = Some(std::process::id());
    persist_run_with_conn(&conn, &md).expect("persist live pid");
    seed_checkpoint(
        &conn,
        "live-running",
        "watch_pr_checks",
        CHECKPOINT_STATUS_WAITING,
    );
    let req = request("live-running", ContinuationKind::Resume, false);
    let validation = validate_continuation(&conn, &req).expect("validate");
    assert!(!validation.ok);
    assert!(validation
        .failure_reasons()
        .iter()
        .any(|r| r.contains("resumable_status")));
}
