//! Validation (safety check) tests for continuation requests.

use super::support::*;
use crate::engine::continuation::{
    checkpoint_identity, validate_continuation, ContinuationKind, RewindTarget,
};
use crate::persistence::{
    get_checkpoint_for_step, get_run_with_conn, persist_run_with_conn, RunMetadata, RunStatus,
    CHECKPOINT_STATUS_READY_TO_RESUME, CHECKPOINT_STATUS_WAITING,
};

fn seed_committed_checkpoint(
    conn: &rusqlite::Connection,
    run_id: &str,
    status: RunStatus,
) -> RunMetadata {
    seed_run(conn, run_id, status, "implement");
    seed_checkpoint(conn, run_id, "implement", CHECKPOINT_STATUS_READY_TO_RESUME);
    let checkpoint = get_checkpoint_for_step(conn, run_id, "implement")
        .expect("query committed checkpoint")
        .expect("committed checkpoint");
    let mut metadata = get_run_with_conn(conn, run_id)
        .expect("load run")
        .expect("run exists");
    metadata.continuation_rearm_checkpoint_id = Some(checkpoint_identity(&checkpoint));
    persist_run_with_conn(conn, &metadata).expect("persist committed checkpoint provenance");
    metadata
}

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
fn committed_checkpoint_grant_rejects_non_resume_kinds_and_mismatched_state() {
    let conn = test_conn();
    let mut metadata = seed_committed_checkpoint(&conn, "committed-negative", RunStatus::Failed);

    for kind in [
        ContinuationKind::Retry {
            from_failed_step: false,
        },
        ContinuationKind::Rewind {
            target: RewindTarget::ToStep("implement".to_string()),
        },
    ] {
        let validation = validate_continuation(&conn, &request("committed-negative", kind, true))
            .expect("validate non-resume kind");
        assert!(!validation.ok);
        assert!(validation
            .failure_reasons()
            .iter()
            .any(|reason| reason.contains("safe_step")));
    }

    metadata.current_step = Some("remediate".to_string());
    persist_run_with_conn(&conn, &metadata).expect("persist mismatched step");
    let validation = validate_continuation(
        &conn,
        &request("committed-negative", ContinuationKind::Resume, false),
    )
    .expect("validate mismatched step");
    assert!(!validation.ok);

    metadata.current_step = Some("implement".to_string());
    persist_run_with_conn(&conn, &metadata).expect("restore current step");
    seed_checkpoint(&conn, "committed-negative", "implement", "completed");
    let validation = validate_continuation(
        &conn,
        &request("committed-negative", ContinuationKind::Resume, false),
    )
    .expect("validate completed checkpoint");
    assert!(!validation.ok);
    assert!(validation
        .failure_reasons()
        .iter()
        .any(|reason| reason.contains("safe_step")));
}

#[test]
fn validation_rejects_live_running_committed_checkpoint() {
    let conn = test_conn();
    let mut metadata = seed_committed_checkpoint(&conn, "live-committed", RunStatus::Running);
    metadata.process_pid = Some(std::process::id());
    persist_run_with_conn(&conn, &metadata).expect("persist live pid");
    let validation = validate_continuation(
        &conn,
        &request("live-committed", ContinuationKind::Resume, true),
    )
    .expect("validate live committed checkpoint");
    assert!(!validation.ok);
    assert!(validation
        .failure_reasons()
        .iter()
        .any(|reason| reason.contains("resumable_status")));
}

#[test]
fn ready_checkpoint_without_continuation_provenance_is_not_authorized() {
    let conn = test_conn();
    seed_run(
        &conn,
        "external-wait-failure",
        RunStatus::Failed,
        "implement",
    );
    seed_checkpoint(
        &conn,
        "external-wait-failure",
        "implement",
        CHECKPOINT_STATUS_READY_TO_RESUME,
    );
    let validation = validate_continuation(
        &conn,
        &request("external-wait-failure", ContinuationKind::Resume, false),
    )
    .expect("validate unproven ready checkpoint");
    assert!(!validation.ok);
    assert!(validation
        .failure_reasons()
        .iter()
        .any(|reason| reason.contains("safe_step")));
}

#[test]
fn malformed_continuation_provenance_is_not_authorized() {
    let conn = test_conn();
    let mut metadata = seed_committed_checkpoint(&conn, "malformed-provenance", RunStatus::Failed);
    metadata.continuation_rearm_checkpoint_id = Some("not-a-checkpoint-identity".to_string());
    persist_run_with_conn(&conn, &metadata).expect("persist malformed provenance");
    let validation = validate_continuation(
        &conn,
        &request("malformed-provenance", ContinuationKind::Resume, true),
    )
    .expect("validate malformed provenance");
    assert!(!validation.ok);
    assert!(validation
        .failure_reasons()
        .iter()
        .any(|reason| reason.contains("safe_step")));
}

#[test]
fn validation_rejects_failed_committed_checkpoint_with_live_owners() {
    for child_owner in [false, true] {
        let conn = test_conn();
        let run_id = if child_owner {
            "failed-live-child"
        } else {
            "failed-live-workflow"
        };
        let mut metadata = seed_committed_checkpoint(&conn, run_id, RunStatus::Failed);
        if child_owner {
            metadata.child_pids = vec![std::process::id()];
        } else {
            metadata.process_pid = Some(std::process::id());
        }
        persist_run_with_conn(&conn, &metadata).expect("persist live owner");
        let validation =
            validate_continuation(&conn, &request(run_id, ContinuationKind::Resume, true))
                .expect("validate live owner");
        assert!(!validation.ok);
        assert!(validation
            .failure_reasons()
            .iter()
            .any(|reason| reason.contains("safe_step")));
    }
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

// ---------------------------------------------------------------------------
// Ownership-denied terminal: distinct non-resumable state that must never be
// selected for cleanup continuation (issue 158).
// ---------------------------------------------------------------------------

#[test]
fn ownership_denied_terminal_rejects_resume_even_with_force() {
    let conn = test_conn();
    seed_ownership_denied_terminal(&conn, "ownership-denied");
    for force in [false, true] {
        let req = request("ownership-denied", ContinuationKind::Resume, force);
        let validation = validate_continuation(&conn, &req).expect("validate");
        assert!(
            !validation.ok,
            "ownership-denied terminal must reject resume (force={force})"
        );
        assert!(
            validation
                .failure_reasons()
                .iter()
                .any(|r| r.contains("ownership denial")),
            "expected ownership-denied rejection (force={force}), got {:?}",
            validation.failure_reasons()
        );
    }
}

#[test]
fn ownership_denied_terminal_rejects_retry_even_with_force() {
    let conn = test_conn();
    seed_ownership_denied_terminal(&conn, "ownership-denied-retry");
    for force in [false, true] {
        let req = request(
            "ownership-denied-retry",
            ContinuationKind::Retry {
                from_failed_step: false,
            },
            force,
        );
        let validation = validate_continuation(&conn, &req).expect("validate");
        assert!(
            !validation.ok,
            "ownership-denied terminal must reject retry (force={force})"
        );
        assert!(
            validation
                .failure_reasons()
                .iter()
                .any(|r| r.contains("ownership denial")),
            "expected ownership-denied rejection (force={force}), got {:?}",
            validation.failure_reasons()
        );
    }
}

#[test]
fn ownership_denied_terminal_stays_distinct_from_cleanup_abandonment() {
    // An ownership-denied terminal is a distinct state from cleanup-failure-
    // abandonment. It must not be treated as recoverable cleanup abandonment
    // even though it carries failure_cleanup state with cleanup_succeeded =
    // false. The `is_cleanup_failure_abandonment` check must return false for
    // an ownership-denied terminal (status is Failed, not Abandoned), and the
    // `is_ownership_denied_terminal` check must return true.
    let conn = test_conn();
    seed_ownership_denied_terminal(&conn, "ownership-distinct");
    let metadata = get_run_with_conn(&conn, "ownership-distinct")
        .expect("query")
        .expect("run");
    assert!(
        metadata.is_ownership_denied_terminal(),
        "ownership-denied terminal must be detected"
    );
    assert!(
        !metadata.is_cleanup_failure_abandonment(),
        "ownership-denied terminal must not be classified as cleanup abandonment"
    );
}
