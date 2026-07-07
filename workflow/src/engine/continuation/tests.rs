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
fn validation_allows_unsafe_step_with_force() {
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
    assert!(validation.ok, "reasons: {:?}", validation.failure_reasons());
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
    let md = commit_continuation(&conn, &req, "collect_ci_failures").expect("commit");
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
fn validation_allows_live_running_resume_point_with_force() {
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
            validation.ok,
            "--force should allow an operator to recover a running record whose PID may have been recycled; reasons: {:?}",
            validation.failure_reasons()
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
    let req = request("commit-live-running", ContinuationKind::Resume, false);
    let err = commit_continuation(&conn, &req, "watch_pr_checks")
        .expect_err("live running claim must be rejected");
    assert!(err
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
    let req = request("commit-unrecorded-running", ContinuationKind::Resume, false);
    let metadata = commit_continuation(&conn, &req, "watch_pr_checks")
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
    let metadata = commit_continuation(&conn, &req, "watch_pr_checks")
        .expect("stale running claim should reopen");
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
