//! Checkpoint selection tests (resume/retry/rewind).

use super::support::*;
use crate::engine::continuation::{
    checkpoint_identity, select_checkpoint, select_rewind_checkpoint, ContinuationError,
    ContinuationKind, RewindTarget,
};
use crate::persistence::{get_checkpoint_for_step, get_run_with_conn, CHECKPOINT_STATUS_WAITING};

#[test]
fn forced_retry_selects_preserved_failed_checkpoint() {
    let conn = test_conn();
    let workspace = tempfile::tempdir().expect("workspace");
    let expected = seed_cleanup_abandonment(&conn, "cleanup-retry", workspace.path());
    let request = request(
        "cleanup-retry",
        ContinuationKind::Retry {
            from_failed_step: false,
        },
        true,
    );

    let validation =
        crate::engine::continuation::validate_continuation(&conn, &request).expect("validate");
    assert!(validation.ok, "{:?}", validation.failure_reasons());
    let metadata = get_run_with_conn(&conn, "cleanup-retry")
        .expect("query")
        .expect("run");
    let selected = select_checkpoint(&conn, &request, &metadata).expect("select");
    assert_eq!(checkpoint_identity(&selected), expected);
}

#[test]
fn incomplete_cleanup_selects_preserved_failed_checkpoint() {
    let conn = test_conn();
    let workspace = tempfile::tempdir().expect("workspace");
    let expected = seed_cleanup_abandonment(&conn, "cleanup-incomplete", workspace.path());
    let mut metadata = get_run_with_conn(&conn, "cleanup-incomplete")
        .expect("query")
        .expect("run");
    metadata.status = crate::persistence::RunStatus::Failed;
    let failure = metadata.failure_cleanup.as_mut().expect("failure cleanup");
    failure.cleanup_succeeded = false;
    failure.cleanup_completed_at = None;
    failure.recovery_consumed_at = None;
    crate::persistence::persist_run_with_conn(&conn, &metadata)
        .expect("persist incomplete cleanup");

    let request = request("cleanup-incomplete", ContinuationKind::Resume, false);
    let selected = select_checkpoint(&conn, &request, &metadata).expect("select failed checkpoint");
    assert_eq!(checkpoint_identity(&selected), expected);
    let validation = crate::engine::continuation::validate_continuation(&conn, &request)
        .expect("validate incomplete cleanup");
    assert!(validation.ok, "{:?}", validation.failure_reasons());
}

#[test]
fn retry_progress_to_later_wait_resumes_current_checkpoint() {
    let conn = test_conn();
    let workspace = tempfile::tempdir().expect("workspace");
    let artifact_root = workspace.path().join("artifacts");
    let original_failed_identity =
        seed_cleanup_abandonment(&conn, "cleanup-progress", workspace.path());
    let mut metadata = get_run_with_conn(&conn, "cleanup-progress")
        .expect("query")
        .expect("run");
    metadata.artifact_root = Some(artifact_root.to_string_lossy().into_owned());
    crate::persistence::persist_run_with_conn(&conn, &metadata).expect("persist artifact root");

    let retry = request(
        "cleanup-progress",
        ContinuationKind::Retry {
            from_failed_step: false,
        },
        true,
    );
    let retry_plan = crate::engine::continuation::prepare_continuation(&conn, &retry, &metadata)
        .expect("prepare retry");
    assert!(retry_plan.validation.ok);
    crate::engine::continuation::commit_continuation(
        &conn,
        &retry,
        &retry_plan.checkpoint_identity,
    )
    .expect("commit retry");

    seed_checkpoint(&conn, "cleanup-progress", "remediate", "completed");
    seed_checkpoint(
        &conn,
        "cleanup-progress",
        "implement",
        CHECKPOINT_STATUS_WAITING,
    );
    let mut progressed = get_run_with_conn(&conn, "cleanup-progress")
        .expect("query")
        .expect("run");
    progressed.status = crate::persistence::RunStatus::WaitingExternal;
    progressed.current_step = Some("implement".to_string());
    crate::persistence::persist_run_with_conn(&conn, &progressed).expect("persist progress");
    crate::persistence::update_lease_status(
        &conn,
        "lease-cleanup-progress",
        crate::persistence::LeaseStatus::WaitingExternal,
        Some("cleanup-progress"),
    )
    .expect("wait lease");

    let resume = request("cleanup-progress", ContinuationKind::Resume, false);
    let resume_plan =
        crate::engine::continuation::prepare_continuation(&conn, &resume, &progressed)
            .expect("prepare current wait");
    assert!(
        resume_plan.validation.ok,
        "{:?}",
        resume_plan.validation.failure_reasons()
    );
    let selected = resume_plan
        .selected
        .as_ref()
        .expect("selected current wait");
    assert_eq!(selected.step_id, "implement");
    assert_eq!(selected.state_snapshot.status, CHECKPOINT_STATUS_WAITING);
    assert_ne!(resume_plan.checkpoint_identity, original_failed_identity);
    let reopened = crate::engine::continuation::commit_continuation(
        &conn,
        &resume,
        &resume_plan.checkpoint_identity,
    )
    .expect("commit current wait");
    assert_eq!(reopened.issue_number, metadata.issue_number);
    assert_eq!(reopened.workspace_path, metadata.workspace_path);
    let failure = reopened
        .failure_cleanup
        .expect("preserved failure provenance");
    assert_eq!(failure.failed_step, "remediate");
    assert_eq!(failure.failure_reason, "agent timed out");
    assert!(failure.cleanup_succeeded);
    assert!(failure.recovery_consumed_at.is_some());

    let selection: serde_json::Value = serde_json::from_slice(
        &std::fs::read(resume_plan.artifact_dir.join("checkpoint-selection.json"))
            .expect("read selection artifact"),
    )
    .expect("parse selection artifact");
    assert_eq!(selection["step_id"], "implement");
    assert_eq!(selection["checkpoint_id"], resume_plan.checkpoint_identity);
}

#[test]
fn cleanup_abandonment_requires_force_and_existing_workspace() {
    let conn = test_conn();
    let workspace = tempfile::tempdir().expect("workspace");
    seed_cleanup_abandonment(&conn, "cleanup-safety", workspace.path());
    let no_force = request(
        "cleanup-safety",
        ContinuationKind::Retry {
            from_failed_step: false,
        },
        false,
    );

    assert!(
        !crate::engine::continuation::validate_continuation(&conn, &no_force)
            .expect("validate")
            .ok
    );

    let mut metadata = get_run_with_conn(&conn, "cleanup-safety")
        .expect("query")
        .expect("run");
    metadata.workspace_path = Some(
        workspace
            .path()
            .join("missing")
            .to_string_lossy()
            .to_string(),
    );
    crate::persistence::persist_run_with_conn(&conn, &metadata).expect("persist missing workspace");
    let forced = request(
        "cleanup-safety",
        ContinuationKind::Retry {
            from_failed_step: false,
        },
        true,
    );
    assert!(
        !crate::engine::continuation::validate_continuation(&conn, &forced)
            .expect("validate")
            .ok
    );
}

#[test]
fn resume_selects_checkpoint_before_terminal_step() {
    let conn = test_conn();
    seed_terminal_failed_run(&conn, "run-5");
    let md = get_run_with_conn(&conn, "run-5").unwrap().unwrap();
    let req = request("run-5", ContinuationKind::Resume, false);
    let cp = select_checkpoint(&conn, &req, &md).expect("select");
    assert_eq!(cp.step_id, "collect_ci_failures");
}

#[test]
fn resume_prefers_waiting_checkpoint_when_present() {
    let conn = test_conn();
    seed_run(
        &conn,
        "run-6",
        crate::persistence::RunStatus::WaitingForChecks,
        "watch_pr_checks",
    );
    seed_checkpoint(&conn, "run-6", "capture_pr_identity", "completed");
    seed_checkpoint(&conn, "run-6", "watch_pr_checks", CHECKPOINT_STATUS_WAITING);
    let md = get_run_with_conn(&conn, "run-6").unwrap().unwrap();
    let req = request("run-6", ContinuationKind::Resume, false);
    let cp = select_checkpoint(&conn, &req, &md).expect("select");
    assert_eq!(cp.step_id, "watch_pr_checks");
    assert_eq!(cp.state_snapshot.status, CHECKPOINT_STATUS_WAITING);
}

#[test]
fn retry_from_failed_step_selects_watch_pr_checks() {
    let conn = test_conn();
    seed_terminal_failed_run(&conn, "run-7");
    let md = get_run_with_conn(&conn, "run-7").unwrap().unwrap();
    let req = request(
        "run-7",
        ContinuationKind::Retry {
            from_failed_step: true,
        },
        false,
    );
    let cp = select_checkpoint(&conn, &req, &md).expect("select");
    assert_eq!(cp.step_id, "watch_pr_checks");
}

#[test]
fn rewind_to_checkpoint_validates_timestamp() {
    let conn = test_conn();
    seed_terminal_failed_run(&conn, "run-8");
    let md = get_run_with_conn(&conn, "run-8").unwrap().unwrap();
    let guard = get_checkpoint_for_step(&conn, "run-8", "post_pr_iteration_guard")
        .unwrap()
        .unwrap();
    let identity = checkpoint_identity(&guard);
    let req = request(
        "run-8",
        ContinuationKind::Rewind {
            target: RewindTarget::ToCheckpoint(identity),
        },
        false,
    );
    let cp = select_checkpoint(&conn, &req, &md).expect("select");
    assert_eq!(cp.step_id, "post_pr_iteration_guard");
}

#[test]
fn rewind_to_checkpoint_rejects_timestamp_mismatch() {
    let conn = test_conn();
    seed_terminal_failed_run(&conn, "run-9");
    let bogus = "watch_pr_checks@2000-01-01T00:00:00+00:00".to_string();
    let err = select_rewind_checkpoint(&conn, "run-9", &RewindTarget::ToCheckpoint(bogus))
        .expect_err("mismatch must error");
    assert!(matches!(err, ContinuationError::InvalidTarget(_)));
}

#[test]
fn rewind_to_checkpoint_rejects_missing_separator() {
    let conn = test_conn();
    seed_terminal_failed_run(&conn, "run-9a");
    let malformed = "watch_pr_checks".to_string();
    let err = select_rewind_checkpoint(&conn, "run-9a", &RewindTarget::ToCheckpoint(malformed))
        .expect_err("missing @ must error");
    assert!(matches!(err, ContinuationError::InvalidTarget(_)));
}

#[test]
fn rewind_to_checkpoint_rejects_invalid_timestamp() {
    let conn = test_conn();
    seed_terminal_failed_run(&conn, "run-9b");
    let malformed = "watch_pr_checks@not-a-timestamp".to_string();
    let err = select_rewind_checkpoint(&conn, "run-9b", &RewindTarget::ToCheckpoint(malformed))
        .expect_err("invalid timestamp must error");
    assert!(matches!(err, ContinuationError::InvalidTarget(_)));
}
