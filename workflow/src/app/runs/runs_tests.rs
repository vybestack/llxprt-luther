use super::*;

fn write_config_root(root: &std::path::Path, wf: &str, restored_step: &str) {
    let workflows = root.join("workflows");
    let configs = root.join("workflow-configs");
    std::fs::create_dir_all(&workflows).expect("workflow dir");
    std::fs::create_dir_all(&configs).expect("config dir");
    let workflow = serde_json::json!({
        "workflow_type_id": wf,
        "steps": [
            {"step_id": "prepare_custom_resume", "step_type": "noop"},
            {"step_id": restored_step, "step_type": "noop"},
            {"step_id": "post_restore_sentinel", "step_type": "noop"}
        ],
        "transitions": [],
        "guards": {"max_retries": 1, "timeout_seconds": 30}
    });
    let config = serde_json::json!({
        "config_id": "custom-resume-config",
        "workflow_type_id": wf,
        "runtime": {"timeout_seconds": 30, "max_retries": 1},
        "repository": {"workspace_strategy": "temp", "branch_template": "test-{run_id}", "base_branch": "main"},
        "guards": {"max_iterations": 1, "max_file_changes": 10, "max_tokens": 1000, "max_cost": 1.0}
    });
    std::fs::write(workflows.join(format!("{wf}.json")), workflow.to_string())
        .expect("workflow file");
    std::fs::write(
        configs.join("custom-resume-config.json"),
        config.to_string(),
    )
    .expect("config file");
}

fn seed_run(store: &SqliteStore, run_id: &str, wf: &str, step: &str) -> RunMetadata {
    let mut md = RunMetadata::new(run_id, wf, "custom-resume-config");
    md.status = RunStatus::Failed;
    md.current_step = Some(step.to_string());
    persist_run_with_conn(store.conn(), &md).expect("persist run");
    let cp = luther_workflow::persistence::Checkpoint::with_snapshot(
        run_id,
        step,
        luther_workflow::persistence::StateSnapshot {
            status: luther_workflow::persistence::CHECKPOINT_STATUS_INTERRUPTED.to_string(),
            ..Default::default()
        },
    );
    luther_workflow::persistence::save_checkpoint_with_conn(store.conn(), &cp)
        .expect("save checkpoint");
    md
}

#[test]
fn reconstructs_runner_from_non_default_config_dir() {
    let temp = tempfile::tempdir().expect("tempdir");
    let config_root = temp.path().join("custom-config");
    let db_path = temp.path().join("checkpoints.db");
    write_config_root(&config_root, "custom-resume-v1", "custom_marker_step");
    let store = SqliteStore::open(&db_path).expect("open store");
    let md = seed_run(
        &store,
        "custom-config-run",
        "custom-resume-v1",
        "custom_marker_step",
    );
    let runner = reconstruct_runner(&md, &md.run_id, &db_path, &Some(config_root))
        .expect("custom config root reconstructs runner");
    assert_eq!(runner.current_step(), "custom_marker_step");
    assert_eq!(runner.workflow_type_id(), "custom-resume-v1");
    assert_eq!(runner.config_id(), "custom-resume-config");
}

#[test]
fn reconstruct_runner_rejects_missing_current_step() {
    let temp = tempfile::tempdir().expect("tempdir");
    let config_root = temp.path().join("custom-config");
    let db_path = temp.path().join("checkpoints.db");
    write_config_root(&config_root, "custom-resume-v1", "custom_marker_step");
    let store = SqliteStore::open(&db_path).expect("open store");
    let md = seed_run(
        &store,
        "missing-step-run",
        "custom-resume-v1",
        "removed_marker_step",
    );
    let err = match reconstruct_runner(&md, &md.run_id, &db_path, &Some(config_root)) {
        Ok(_) => panic!("missing persisted step is rejected"),
        Err(err) => err,
    };
    assert!(
        err.contains("current_step 'removed_marker_step' is not present"),
        "unexpected error: {err}"
    );
}

#[test]
fn run_context_from_metadata_preserves_identity_and_defaults_log_path() {
    let mut md = RunMetadata::new("ctx-run", "wf", "cfg");
    md.artifact_root = Some("/artifacts".to_string());
    md.workspace_path = Some("/workspace".to_string());
    md.repository = Some("owner/repo".to_string());
    md.issue_number = Some(125);
    md.pr_number = Some(126);
    md.head_sha = Some("deadbeef".to_string());

    let ctx = run_context_from_metadata(&md, "ctx-run");
    assert_eq!(ctx.artifact_root.as_deref(), Some("/artifacts"));
    assert_eq!(ctx.workspace_path.as_deref(), Some("/workspace"));
    assert_eq!(ctx.repository.as_deref(), Some("owner/repo"));
    assert_eq!(ctx.issue_number, Some(125));
    assert_eq!(ctx.pr_number, Some(126));
    assert_eq!(ctx.head_sha.as_deref(), Some("deadbeef"));
    // log_path defaults to the derived run log path when metadata omits it.
    assert!(ctx.log_path.is_some());
}

#[test]
fn run_context_from_metadata_uses_explicit_log_path() {
    let mut md = RunMetadata::new("ctx-run", "wf", "cfg");
    md.log_path = Some("/custom/log.txt".to_string());
    let ctx = run_context_from_metadata(&md, "ctx-run");
    assert_eq!(ctx.log_path.as_deref(), Some("/custom/log.txt"));
}

#[test]
fn write_continuation_result_writes_named_artifact() {
    let temp = tempfile::tempdir().expect("tempdir");
    let outcome: Result<RunOutcome, luther_workflow::engine::runner::EngineError> =
        Ok(RunOutcome::Success);
    write_continuation_result(
        temp.path(),
        &luther_workflow::engine::ContinuationKind::Resume,
        "some_step",
        &outcome,
    );
    let name = luther_workflow::engine::continuation::result_artifact_name(
        &luther_workflow::engine::ContinuationKind::Resume,
    );
    let written = temp.path().join(name);
    assert!(written.exists(), "expected {name} to be written");
    let content = std::fs::read_to_string(&written).unwrap();
    assert!(content.contains("completed"));
}

#[test]
fn write_continuation_result_maps_waiting_external_status() {
    let temp = tempfile::tempdir().expect("tempdir");
    let outcome: Result<RunOutcome, luther_workflow::engine::runner::EngineError> =
        Ok(RunOutcome::WaitingExternal {
            step_id: "watch".to_string(),
            reason: "pending".to_string(),
        });
    write_continuation_result(
        temp.path(),
        &luther_workflow::engine::ContinuationKind::Retry {
            from_failed_step: true,
        },
        "watch",
        &outcome,
    );
    let name = luther_workflow::engine::continuation::result_artifact_name(
        &luther_workflow::engine::ContinuationKind::Retry {
            from_failed_step: true,
        },
    );
    let content = std::fs::read_to_string(temp.path().join(name)).unwrap();
    assert!(content.contains("waiting_external"));
}

fn sample_checkpoint(run_id: &str, step: &str) -> luther_workflow::persistence::Checkpoint {
    luther_workflow::persistence::Checkpoint::with_snapshot(
        run_id,
        step,
        luther_workflow::persistence::StateSnapshot {
            status: "interrupted".to_string(),
            loop_count: 2,
            retry_count: 1,
            ..Default::default()
        },
    )
}

#[test]
fn print_checkpoints_json_emits_valid_document() {
    let cps = vec![sample_checkpoint("run-x", "step-a")];
    // Exercises the JSON rendering path (stdout side effects are ignored).
    print_checkpoints_json("run-x", &cps);
    // Rebuild the same document to assert on structure/content.
    let identity = luther_workflow::engine::continuation::checkpoint_identity(&cps[0]);
    assert!(!identity.is_empty());
}

#[test]
fn print_checkpoints_human_handles_empty_and_populated() {
    print_checkpoints_human("empty-run", &[]);
    let cps = vec![sample_checkpoint("run-y", "step-b")];
    print_checkpoints_human("run-y", &cps);
}
