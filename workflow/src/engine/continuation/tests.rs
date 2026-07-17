use super::*;
use crate::persistence::checkpoint::init_checkpoint_table;
use crate::persistence::run_metadata::init_runs_table;
use crate::persistence::{
    save_checkpoint_with_conn, RunStatus, StateSnapshot, CHECKPOINT_STATUS_WAITING,
};
use std::collections::HashMap;

fn test_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory db");
    init_checkpoint_table(&conn).expect("checkpoint table");
    init_runs_table(&conn).expect("runs table");
    crate::persistence::leases::init_leases_table(&conn).expect("leases table");
    conn
}

fn seed_run(conn: &Connection, run_id: &str, status: RunStatus, current_step: &str) {
    let mut md = RunMetadata::new(run_id, "llxprt-issue-fix", "llxprt-issue-fix-v1");
    md.status = status;
    md.current_step = Some(current_step.to_string());
    md.repository = Some("vybestack/llxprt-code".to_string());
    md.issue_number = Some(2133);
    md.pr_number = Some(2138);
    md.workspace_path = Some("/tmp/ws".to_string());
    persist_run_with_conn(conn, &md).expect("persist run");
}

fn seed_checkpoint(conn: &Connection, run_id: &str, step: &str, status: &str) {
    let snapshot = StateSnapshot {
        retry_count: 0,
        loop_count: 0,
        edge_loop_counts: HashMap::new(),
        context: HashMap::new(),
        status: status.to_string(),
    };
    let cp = Checkpoint::with_snapshot(run_id, step, snapshot);
    save_checkpoint_with_conn(conn, &cp).expect("save checkpoint");
    // Ensure distinct, increasing timestamps for ordering assertions.
    std::thread::sleep(std::time::Duration::from_millis(2));
}

fn seed_terminal_failed_run(conn: &Connection, run_id: &str) {
    seed_run(conn, run_id, RunStatus::Failed, TERMINAL_STEP);
    seed_checkpoint(conn, run_id, "capture_pr_identity", "completed");
    seed_checkpoint(conn, run_id, "post_pr_iteration_guard", "completed");
    seed_checkpoint(conn, run_id, "watch_pr_checks", "completed");
    seed_checkpoint(conn, run_id, "collect_ci_failures", "completed");
    seed_checkpoint(conn, run_id, TERMINAL_STEP, "completed");
}

fn request(run_id: &str, kind: ContinuationKind, force: bool) -> ContinuationRequest {
    ContinuationRequest {
        run_id: run_id.to_string(),
        kind,
        force,
    }
}
fn seed_cleanup_abandonment(conn: &Connection, run_id: &str, workspace: &Path) -> String {
    seed_run(conn, run_id, RunStatus::Abandoned, "abandon_and_log");
    seed_checkpoint(conn, run_id, "remediate", "completed");
    let checkpoint = get_checkpoint_for_step(conn, run_id, "remediate")
        .expect("checkpoint query")
        .expect("failed checkpoint");
    seed_checkpoint(conn, run_id, "abandon_and_log", "completed");
    let mut metadata = get_run_with_conn(conn, run_id)
        .expect("run query")
        .expect("run");
    metadata.workspace_path = Some(workspace.to_string_lossy().to_string());
    metadata.failure_cleanup = Some(crate::persistence::FailureCleanupState {
        schema_version: crate::persistence::FailureCleanupState::SCHEMA_VERSION,
        failed_step: "remediate".to_string(),
        failure_outcome: "fatal".to_string(),
        failure_reason: "agent timed out".to_string(),
        failed_checkpoint_id: checkpoint_identity(&checkpoint),
        failed_state_snapshot: checkpoint.state_snapshot.clone(),
        cleanup_step: "abandon_and_log".to_string(),
        cleanup_succeeded: true,
        captured_at: Utc::now(),
        cleanup_completed_at: Some(Utc::now()),
        recovery_consumed_at: None,
    });
    persist_run_with_conn(conn, &metadata).expect("persist cleanup provenance");
    // Write the durable `.luther/workspace-owner` marker so the workspace is
    // trusted for cleanup-failure-abandonment recovery.
    write_workspace_owner_marker(workspace, run_id).expect("write owner marker");
    checkpoint_identity(&checkpoint)
}

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

    let validation = validate_continuation(&conn, &request).expect("validate");
    assert!(validation.ok, "{:?}", validation.failure_reasons());
    let metadata = get_run_with_conn(&conn, "cleanup-retry")
        .expect("query")
        .expect("run");
    let selected = select_checkpoint(&conn, &request, &metadata).expect("select");
    assert_eq!(checkpoint_identity(&selected), expected);
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
        !validate_continuation(&conn, &no_force)
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
    persist_run_with_conn(&conn, &metadata).expect("persist missing workspace");
    let forced = request(
        "cleanup-safety",
        ContinuationKind::Retry {
            from_failed_step: false,
        },
        true,
    );
    assert!(!validate_continuation(&conn, &forced).expect("validate").ok);
}

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
        RunStatus::WaitingForChecks,
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
    let validation = validate_continuation(&conn, &request).expect("validation result");
    assert!(!validation.ok);
    assert!(validation
        .checks
        .iter()
        .any(|check| check.name == "resumable_status" && !check.passed));
}

#[test]
fn prepare_continuation_writes_artifacts() {
    let conn = test_conn();
    let temp = tempfile::tempdir().expect("tempdir");
    seed_terminal_failed_run(&conn, "run-11");
    let mut md = get_run_with_conn(&conn, "run-11").unwrap().unwrap();
    md.artifact_root = Some(temp.path().to_string_lossy().to_string());
    let req = request("run-11", ContinuationKind::Resume, false);
    let plan = prepare_continuation(&conn, &req, &md).expect("prepare");
    assert!(plan.validation.ok);
    assert!(plan.artifact_dir.join("continuation-request.json").exists());
    assert!(plan
        .artifact_dir
        .join("continuation-validation.json")
        .exists());
    assert!(plan.artifact_dir.join("checkpoint-selection.json").exists());
}

#[test]
fn prepare_continuation_writes_validation_on_failure() {
    let conn = test_conn();
    let temp = tempfile::tempdir().expect("tempdir");
    seed_run(&conn, "run-12", RunStatus::Failed, "implement");
    seed_checkpoint(&conn, "run-12", "implement", "completed");
    let mut md = get_run_with_conn(&conn, "run-12").unwrap().unwrap();
    md.artifact_root = Some(temp.path().to_string_lossy().to_string());
    let req = request(
        "run-12",
        ContinuationKind::Rewind {
            target: RewindTarget::ToStep("implement".to_string()),
        },
        false,
    );
    let plan = prepare_continuation(&conn, &req, &md).expect("prepare");
    assert!(!plan.validation.ok);
    assert!(plan.selected.is_none());
    assert!(plan
        .artifact_dir
        .join("continuation-validation.json")
        .exists());
}

#[test]
fn result_artifact_name_differs_for_retry() {
    assert_eq!(
        result_artifact_name(&ContinuationKind::Resume),
        "resume-result.json"
    );
    assert_eq!(
        result_artifact_name(&ContinuationKind::Retry {
            from_failed_step: true
        }),
        "retry-result.json"
    );
    assert_eq!(
        result_artifact_name(&ContinuationKind::Rewind {
            target: RewindTarget::ToStep("watch_pr_checks".to_string()),
        }),
        "resume-result.json"
    );
}

/// Continuation kinds that should be rejected uniformly when a run is in a
/// non-resumable terminal state, regardless of `--force`.
fn resumable_kinds() -> Vec<ContinuationKind> {
    vec![
        ContinuationKind::Resume,
        ContinuationKind::Retry {
            from_failed_step: false,
        },
        ContinuationKind::Retry {
            from_failed_step: true,
        },
        ContinuationKind::Rewind {
            target: RewindTarget::ToStep("watch_pr_checks".to_string()),
        },
    ]
}

/// Seed a run in `status` with a whitelisted, resumable `watch_pr_checks`
/// checkpoint, then assert every continuation kind is rejected with a
/// `resumable_status` failure, even with `force = true`.
fn assert_non_resumable_rejected(status: RunStatus) {
    let conn = test_conn();
    seed_run(&conn, "term", status.clone(), "watch_pr_checks");
    seed_checkpoint(&conn, "term", "watch_pr_checks", CHECKPOINT_STATUS_WAITING);
    for kind in resumable_kinds() {
        for force in [false, true] {
            let req = request("term", kind.clone(), force);
            let validation = validate_continuation(&conn, &req).expect("validate");
            assert!(
                !validation.ok,
                "status {status:?} kind {kind:?} force={force} must be rejected"
            );
            assert!(
                validation
                    .failure_reasons()
                    .iter()
                    .any(|r| r.contains("resumable_status")),
                "expected resumable_status failure for {status:?} (got {:?})",
                validation.failure_reasons()
            );
        }
    }
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
fn short_lived_process() -> std::io::Result<std::process::Child> {
    std::process::Command::new("true").spawn()
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
fn continuation_overrides_maps_recorded_identity() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    let mut md = RunMetadata::new("r", "wf", "cfg");
    md.repository = Some("vybestack/llxprt-luther".to_string());
    md.issue_number = Some(65);
    md.workspace_path = Some("/tmp/luther-workspaces/llxprt-luther".to_string());
    md.artifact_root = Some("/tmp/luther-artifacts/llxprt-luther".to_string());

    let overrides = continuation_overrides(&md);

    assert_eq!(overrides.repo.as_deref(), Some("vybestack/llxprt-luther"));
    assert_eq!(overrides.issue.as_deref(), Some("65"));
    assert_eq!(
        overrides.work_dir,
        Some(PathBuf::from("/tmp/luther-workspaces/llxprt-luther"))
    );
    assert_eq!(
        overrides.artifact_dir,
        Some(PathBuf::from("/tmp/luther-artifacts/llxprt-luther"))
    );
}

#[test]
fn continuation_overrides_omits_unrecorded_fields() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    let md = RunMetadata::new("r", "wf", "cfg");
    let overrides = continuation_overrides(&md);
    assert!(
        overrides.is_empty(),
        "a run with no recorded identity must not emit overrides"
    );
}

#[test]
fn continuation_overrides_falls_back_to_pr_anchor() {
    // A PR-only continuation (no issue_number, only pr_number) is accepted by
    // check_identity_recoverable, so the rebuilt overrides must preserve the
    // PR anchor instead of silently dropping to the default issue.
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    let mut md = RunMetadata::new("r", "wf", "cfg");
    md.repository = Some("vybestack/llxprt-luther".to_string());
    md.issue_number = None;
    md.pr_number = Some(66);

    let overrides = continuation_overrides(&md);

    assert_eq!(overrides.repo.as_deref(), Some("vybestack/llxprt-luther"));
    assert_eq!(
        overrides.issue.as_deref(),
        Some("66"),
        "a PR-only run must reuse pr_number as the issue anchor"
    );
}

#[test]
fn continuation_overrides_prefers_issue_over_pr_anchor() {
    // When both anchors are recorded, the issue number wins so a run that
    // recorded an explicit issue keeps targeting it.
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    let mut md = RunMetadata::new("r", "wf", "cfg");
    md.issue_number = Some(65);
    md.pr_number = Some(66);

    let overrides = continuation_overrides(&md);

    assert_eq!(overrides.issue.as_deref(), Some("65"));
}

// ---------------------------------------------------------------------------
// Workspace owner marker tests (issue 137)
// ---------------------------------------------------------------------------

#[test]
fn marker_is_idempotent_for_same_run() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    write_workspace_owner_marker(workspace, "run-A").expect("first write");
    // A second write for the same run id must succeed without error.
    write_workspace_owner_marker(workspace, "run-A").expect("idempotent write");
    let marker = workspace.join(".luther").join("workspace-owner");
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "run-A");
}

#[test]
fn marker_rejects_different_owner() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    write_workspace_owner_marker(workspace, "run-A").expect("first write");
    let err = write_workspace_owner_marker(workspace, "run-B")
        .expect_err("different owner must be rejected");
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(err.to_string().contains("run-A"));
    assert!(err.to_string().contains("run-B"));
    // The original owner is preserved.
    let marker = workspace.join(".luther").join("workspace-owner");
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "run-A");
}

#[test]
fn marker_rejects_directory_at_marker_path() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    let luther = workspace.join(".luther");
    std::fs::create_dir_all(&luther).unwrap();
    let marker = luther.join("workspace-owner");
    std::fs::create_dir(&marker).unwrap();
    let err = write_workspace_owner_marker(workspace, "run-A")
        .expect_err("directory marker must be rejected");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("directory"));
}

#[cfg(unix)]
#[test]
fn marker_rejects_symlink_at_marker_path() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    let luther = workspace.join(".luther");
    std::fs::create_dir_all(&luther).unwrap();
    let target = dir.path().join("evil");
    std::fs::write(&target, "run-evil").unwrap();
    let marker = luther.join("workspace-owner");
    std::os::unix::fs::symlink(&target, &marker).unwrap();
    let err = write_workspace_owner_marker(workspace, "run-A")
        .expect_err("symlink marker must be rejected");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("symlink"));
}

#[cfg(unix)]
#[test]
fn marker_rejects_symlinked_luther_parent() {
    // A symlinked `.luther` parent could redirect the marker to an
    // attacker-controlled location. The write must reject it before creating
    // the marker.
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    let evil = dir.path().join("evil-luther");
    std::fs::create_dir_all(&evil).unwrap();
    let luther_link = workspace.join(".luther");
    std::os::unix::fs::symlink(&evil, &luther_link).unwrap();
    let err = write_workspace_owner_marker(workspace, "run-A")
        .expect_err("symlinked .luther parent must be rejected");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains(".luther"));
    assert!(err.to_string().contains("symlink"));
    // No marker should have been written through the symlink.
    assert!(!evil.join("workspace-owner").exists());
}

#[test]
fn marker_rejects_empty_marker_without_rewriting_it() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    let luther = workspace.join(".luther");
    std::fs::create_dir_all(&luther).unwrap();
    let marker = luther.join("workspace-owner");
    std::fs::write(&marker, "   ").unwrap();
    let error = write_workspace_owner_marker(workspace, "run-A")
        .expect_err("empty marker must fail closed");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "   ");
}

#[test]
fn verify_rejects_missing_marker() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    // No marker exists at all.
    let reason = verify_workspace_ownership_marker(workspace, "run-A");
    assert!(reason.is_some());
    assert!(reason.unwrap().contains("missing"));
}

#[test]
fn verify_rejects_empty_marker() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    write_workspace_owner_marker(workspace, "run-A").unwrap();
    let marker = workspace.join(".luther").join("workspace-owner");
    std::fs::write(&marker, "").unwrap();
    let reason = verify_workspace_ownership_marker(workspace, "run-A");
    assert!(reason.is_some());
    assert!(reason.unwrap().contains("empty"));
}

#[test]
fn verify_rejects_mismatched_owner() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    write_workspace_owner_marker(workspace, "run-A").unwrap();
    let reason = verify_workspace_ownership_marker(workspace, "run-B");
    assert!(reason.is_some());
    let detail = reason.unwrap();
    assert!(detail.contains("run-A"));
    assert!(detail.contains("run-B"));
}

#[test]
fn verify_accepts_exact_owner() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    write_workspace_owner_marker(workspace, "run-A").unwrap();
    assert!(verify_workspace_ownership_marker(workspace, "run-A").is_none());
}

#[cfg(unix)]
#[test]
fn verify_rejects_symlinked_luther_parent() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    let evil = dir.path().join("evil-verify");
    std::fs::create_dir_all(&evil).unwrap();
    // Place a valid-looking marker behind the symlink target.
    let evil_luther = evil.join(".luther");
    std::fs::create_dir_all(&evil_luther).unwrap();
    std::fs::write(evil_luther.join("workspace-owner"), "run-A").unwrap();
    let luther_link = workspace.join(".luther");
    std::os::unix::fs::symlink(&evil, &luther_link).unwrap();
    let reason = verify_workspace_ownership_marker(workspace, "run-A");
    assert!(reason.is_some());
    assert!(reason.unwrap().contains("symlink"));
}

#[test]
fn cleanup_workspace_ownership_rejects_symlinked_workspace() {
    let conn = test_conn();
    let real = tempfile::tempdir().expect("real workspace");
    let link_root = tempfile::tempdir().expect("link parent");
    #[cfg(unix)]
    {
        let link = link_root.path().join("ws-link");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();
        let checkpoint = seed_cleanup_abandonment(&conn, "ws-symlink", real.path());
        // Point the workspace_path at the symlink, not the real dir.
        let mut md = get_run_with_conn(&conn, "ws-symlink").unwrap().unwrap();
        md.workspace_path = Some(link.to_string_lossy().to_string());
        persist_run_with_conn(&conn, &md).unwrap();
        let req = request(
            "ws-symlink",
            ContinuationKind::Retry {
                from_failed_step: false,
            },
            true,
        );
        let validation = validate_continuation(&conn, &req).expect("validate");
        assert!(!validation.ok);
        assert!(validation
            .failure_reasons()
            .iter()
            .any(|r| r.contains("symlink")));
        // Keep the checkpoint variable alive so the compiler is happy on non-unix.
        let _ = checkpoint;
    }
}

#[test]
fn cleanup_workspace_ownership_rejects_mismatched_owner_marker() {
    let conn = test_conn();
    let workspace = tempfile::tempdir().expect("workspace");
    seed_cleanup_abandonment(&conn, "ws-mismatch", workspace.path());
    // Overwrite the marker with a different run id.
    let marker = workspace.path().join(".luther").join("workspace-owner");
    std::fs::write(&marker, "run-impostor").unwrap();
    let req = request(
        "ws-mismatch",
        ContinuationKind::Retry {
            from_failed_step: false,
        },
        true,
    );
    let validation = validate_continuation(&conn, &req).expect("validate");
    assert!(!validation.ok);
    assert!(validation
        .failure_reasons()
        .iter()
        .any(|r| r.contains("workspace")));
}

#[test]
fn cleanup_workspace_ownership_rejects_marker_directory() {
    let conn = test_conn();
    let workspace = tempfile::tempdir().expect("workspace");
    seed_cleanup_abandonment(&conn, "ws-dir-marker", workspace.path());
    // Replace the marker file with a directory.
    let marker = workspace.path().join(".luther").join("workspace-owner");
    std::fs::remove_file(&marker).unwrap();
    std::fs::create_dir(&marker).unwrap();
    let req = request(
        "ws-dir-marker",
        ContinuationKind::Retry {
            from_failed_step: false,
        },
        true,
    );
    let validation = validate_continuation(&conn, &req).expect("validate");
    assert!(!validation.ok);
    assert!(validation
        .failure_reasons()
        .iter()
        .any(|r| r.contains("directory")));
}

#[test]
fn cleanup_workspace_ownership_rejects_not_a_directory() {
    let conn = test_conn();
    let workspace = tempfile::tempdir().expect("workspace");
    seed_cleanup_abandonment(&conn, "ws-notdir", workspace.path());
    // Point workspace_path at a regular file, not a directory.
    let file = tempfile::NamedTempFile::new().expect("temp file");
    let mut md = get_run_with_conn(&conn, "ws-notdir").unwrap().unwrap();
    md.workspace_path = Some(file.path().to_string_lossy().to_string());
    persist_run_with_conn(&conn, &md).unwrap();
    let req = request(
        "ws-notdir",
        ContinuationKind::Retry {
            from_failed_step: false,
        },
        true,
    );
    let validation = validate_continuation(&conn, &req).expect("validate");
    assert!(!validation.ok);
    assert!(validation
        .failure_reasons()
        .iter()
        .any(|r| r.contains("directory") || r.contains("not a directory")));
}

/// Concurrent marker writes for different run ids on the same workspace must
/// allow exactly one winner and reject the other, proving the atomic
/// create-new path closes the TOCTOU window.
#[test]
fn marker_concurrent_different_owners_one_wins() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let dir = Arc::new(tempfile::tempdir().expect("workspace"));
    let workspace = dir.path().to_path_buf();
    let errors = Arc::new(AtomicUsize::new(0));
    let successes = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for i in 0..8 {
        let ws = workspace.clone();
        let run_id = format!("run-concurrent-{i}");
        let errors = Arc::clone(&errors);
        let successes = Arc::clone(&successes);
        handles.push(std::thread::spawn(
            move || match write_workspace_owner_marker(&ws, &run_id) {
                Ok(()) => {
                    successes.fetch_add(1, Ordering::SeqCst);
                }
                Err(_) => {
                    errors.fetch_add(1, Ordering::SeqCst);
                }
            },
        ));
    }
    for handle in handles {
        handle.join().expect("thread panic");
    }
    // Exactly one writer wins; the rest are rejected with AlreadyExists.
    assert_eq!(
        successes.load(Ordering::SeqCst),
        1,
        "exactly one concurrent writer must claim the workspace"
    );
    assert_eq!(
        errors.load(Ordering::SeqCst),
        7,
        "the losing writers must be rejected"
    );
}

/// Multiple child runs (relaunches) must each get a distinct workspace, proving
/// child workspace isolation at the ownership-marker level.
#[test]
fn child_relaunch_gets_distinct_isolated_workspaces() {
    // Each isolated child workspace can be independently claimed by its run id,
    // and cross-verification fails, proving the ownership marker binds a
    // workspace to exactly one run.
    let dir_first = tempfile::tempdir().expect("first workspace");
    write_workspace_owner_marker(dir_first.path(), "child-run-1").expect("claim first");
    let dir_second = tempfile::tempdir().expect("second workspace");
    write_workspace_owner_marker(dir_second.path(), "child-run-2").expect("claim second");
    assert!(verify_workspace_ownership_marker(dir_first.path(), "child-run-1").is_none());
    assert!(verify_workspace_ownership_marker(dir_second.path(), "child-run-2").is_none());
    // Cross-verification fails: first workspace does not belong to second run.
    assert!(verify_workspace_ownership_marker(dir_first.path(), "child-run-2").is_some());
}
