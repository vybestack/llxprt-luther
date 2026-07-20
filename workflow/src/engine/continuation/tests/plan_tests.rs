//! Tests for continuation overrides, artifacts, and plan preparation.

use std::path::PathBuf;

use super::support::*;
use crate::engine::continuation::{
    continuation_overrides, prepare_continuation, result_artifact_name, ContinuationKind,
    RewindTarget,
};
use crate::persistence::{get_run_with_conn, RunMetadata, RunStatus};

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
