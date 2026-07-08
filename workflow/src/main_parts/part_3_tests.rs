use super::*;

fn write_config_root(root: &std::path::Path, wf: &str, step: &str) {
    let workflows = root.join("workflows");
    let configs = root.join("workflow-configs");
    std::fs::create_dir_all(&workflows).expect("workflow dir");
    std::fs::create_dir_all(&configs).expect("config dir");
    let workflow = serde_json::json!({
        "workflow_type_id": wf,
        "steps": [{"step_id": step, "step_type": "noop"}],
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
    std::fs::write(configs.join("custom-resume-config.json"), config.to_string())
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
    let md = seed_run(&store, "custom-config-run", "custom-resume-v1", "custom_marker_step");
    let runner = reconstruct_runner(&md, &md.run_id, &db_path, &Some(config_root))
        .expect("custom config root reconstructs runner");
    assert_eq!(runner.current_step(), "custom_marker_step");
}

