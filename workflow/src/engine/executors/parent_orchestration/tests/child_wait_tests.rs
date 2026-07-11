//! Child wait-kind, wait-identity, and run-result classification tests.

use super::super::*;
use super::support::*;

#[test]
fn child_run_registry_status_overrides_zero_exit_waiting_child() {
    assert_eq!(
        classify_child_run_result(
            &ChildWorkflowRunResult::CompletedSuccess,
            Some(&RunStatus::WaitingExternal)
        ),
        ChildWorkflowRunResult::WaitingExternal
    );
}

#[test]
fn child_wait_kind_mapping_covers_known_steps() {
    for (step, wait_kind) in [
        ("watch_pr_checks", WaitKind::PrChecks),
        ("collect_coderabbit_feedback", WaitKind::CoderabbitReview),
        ("merge_pr", WaitKind::PrMerge),
        (
            "dependency_child_workflow",
            WaitKind::DependencyChildWorkflow,
        ),
        ("dependency_child_merge", WaitKind::DependencyChildMerge),
        ("github_rate_limit_backoff", WaitKind::RateLimitBackoff),
        ("other", WaitKind::HumanReview),
    ] {
        assert_eq!(child_wait_kind_for_step(step), wait_kind);
    }
}

#[test]
fn child_wait_identity_accepts_required_metadata() {
    let mut metadata = child_run_metadata(Some(17), Some("abc123"));

    let identity = child_wait_poll_identity(Some(&metadata), WaitKind::PrChecks).unwrap();

    assert_eq!(identity.pr_number, Some(17));
    assert_eq!(identity.head_sha.as_deref(), Some("abc123"));

    assert!(child_wait_poll_identity(Some(&metadata), WaitKind::DependencyChildWorkflow).is_ok());
    metadata.head_sha = None;
    assert!(child_wait_poll_identity(Some(&metadata), WaitKind::DependencyChildWorkflow).is_err());
}

#[test]
fn child_wait_identity_rejects_missing_pr_context() {
    let metadata = child_run_metadata(None, Some("abc123"));

    assert!(child_wait_poll_identity(Some(&metadata), WaitKind::CoderabbitReview).is_err());
    assert!(child_wait_poll_identity(None, WaitKind::HumanReview).is_err());
    assert!(child_wait_poll_identity(None, WaitKind::RateLimitBackoff).is_ok());
}

#[test]
fn child_wait_identity_rejects_missing_check_head_sha() {
    let metadata = child_run_metadata(Some(17), None);

    assert!(child_wait_poll_identity(Some(&metadata), WaitKind::PrChecks).is_err());
}

#[test]
fn interrupted_child_workflow_uses_step_wait_kind() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("checkpoints.db");
    crate::persistence::init_database(&db_path).unwrap();
    let conn = open_parent_orchestration_connection(&db_path).unwrap();
    let request = ChildWorkflowLaunchRequest {
        workflow_type_id: "llxprt-issue-fix-v1".to_string(),
        config_id: "llxprt-code".to_string(),
        run_id: "child-interrupted-run".to_string(),
        repo: "owner/repo".to_string(),
        issue_number: unique_child_issue_number(),
        work_dir: None,
        artifact_dir: None,
        config_root: PathBuf::from("config"),
    };
    let config = workflow_config(&request);
    crate::persistence::checkpoint::save_checkpoint_with_conn(
        &conn,
        &crate::persistence::Checkpoint::new(&request.run_id, "launch_or_resume_child_workflow"),
    )
    .unwrap();

    persist_child_interrupted_state(
        &request,
        &config,
        &db_path,
        "launch_or_resume_child_workflow",
    )
    .unwrap();

    let record = crate::persistence::get_wait_state(&conn, &request.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(record.wait_kind, WaitKind::DependencyChildWorkflow);
}

#[test]
fn classify_child_pr_wait_covers_all_states() {
    assert!(matches!(
        classify_child_pr_wait(None),
        ChildPrWait::MissingPr
    ));
    assert!(matches!(
        classify_child_pr_wait(Some(&merged_pr(1))),
        ChildPrWait::Merged
    ));
    let mut superseded = pr_with_checks(2, Some("passed"), Some("approved"));
    superseded.state = "SUPERSEDED".to_string();
    assert!(matches!(
        classify_child_pr_wait(Some(&superseded)),
        ChildPrWait::Superseded
    ));
    let mut closed = pr_with_checks(3, Some("passed"), Some("approved"));
    closed.state = "CLOSED".to_string();
    assert!(matches!(
        classify_child_pr_wait(Some(&closed)),
        ChildPrWait::ClosedUnmerged
    ));
    assert!(matches!(
        classify_child_pr_wait(Some(&ready_pr(4))),
        ChildPrWait::ReadyForHumanMerge
    ));
}

#[test]
fn classify_child_run_result_maps_run_status() {
    assert_eq!(
        classify_child_run_result(
            &ChildWorkflowRunResult::CompletedSuccess,
            Some(&RunStatus::Running)
        ),
        ChildWorkflowRunResult::WaitingExternal
    );
    assert_eq!(
        classify_child_run_result(
            &ChildWorkflowRunResult::WaitingExternal,
            Some(&RunStatus::Completed)
        ),
        ChildWorkflowRunResult::CompletedSuccess
    );
    assert_eq!(
        classify_child_run_result(
            &ChildWorkflowRunResult::CompletedSuccess,
            Some(&RunStatus::Failed)
        ),
        ChildWorkflowRunResult::CompletedFailure
    );
    assert_eq!(
        classify_child_run_result(
            &ChildWorkflowRunResult::CompletedFailure,
            Some(&RunStatus::Merged)
        ),
        ChildWorkflowRunResult::CompletedSuccess
    );
    // With no run status, the process result passes through unchanged.
    assert_eq!(
        classify_child_run_result(&ChildWorkflowRunResult::WaitingExternal, None),
        ChildWorkflowRunResult::WaitingExternal
    );
}

#[test]
fn classify_child_run_result_prefers_run_status_over_process_result() {
    use crate::persistence::RunStatus;
    assert!(matches!(
        classify_child_run_result(
            &ChildWorkflowRunResult::CompletedSuccess,
            Some(&RunStatus::Running)
        ),
        ChildWorkflowRunResult::WaitingExternal
    ));
    assert!(matches!(
        classify_child_run_result(
            &ChildWorkflowRunResult::WaitingExternal,
            Some(&RunStatus::Completed)
        ),
        ChildWorkflowRunResult::CompletedSuccess
    ));
    assert!(matches!(
        classify_child_run_result(
            &ChildWorkflowRunResult::WaitingExternal,
            Some(&RunStatus::Failed)
        ),
        ChildWorkflowRunResult::CompletedFailure
    ));
    // With no run status, the process result is passed through unchanged.
    assert!(matches!(
        classify_child_run_result(&ChildWorkflowRunResult::CompletedFailure, None),
        ChildWorkflowRunResult::CompletedFailure
    ));
}
