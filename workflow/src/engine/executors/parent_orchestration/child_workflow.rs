use super::*;

use crate::workflow::config_loader::{resolve_workflow_config, resolve_workflow_type};

pub fn child_launch_request(state: &OrchestrationState, child: u64) -> ChildWorkflowLaunchRequest {
    let stamp = Utc::now().timestamp_millis();
    child_request_with_run_id(
        state,
        child,
        format!("parent{}-child{}-{stamp}", state.parent_issue_number, child),
    )
}

pub fn child_resume_request(
    state: &OrchestrationState,
    child: u64,
    run_id: String,
) -> ChildWorkflowLaunchRequest {
    child_request_with_run_id(state, child, run_id)
}

pub fn child_request_with_run_id(
    state: &OrchestrationState,
    child: u64,
    run_id: String,
) -> ChildWorkflowLaunchRequest {
    let artifact_dir = state
        .artifact_dir
        .as_ref()
        .map(|base| child_artifact_dir(base, child, &run_id));
    // Derive an isolated workspace directory per child issue and run rather
    // than reusing the parent's `work_dir`. Each child workflow gets its own
    // persisted worktree so concurrent children and relaunches do not stomp
    // on a shared parent workspace, and the durable workspace-owner marker can
    // be bound to the child run id without cross-run conflicts.
    let work_dir = state
        .work_dir
        .as_ref()
        .map(|base| child_work_dir(base, child, &run_id));
    ChildWorkflowLaunchRequest {
        workflow_type_id: state.child_workflow_type_id.clone(),
        config_id: state.child_config_id.clone(),
        run_id,
        repo: state.repo.clone(),
        issue_number: child,
        work_dir,
        artifact_dir,
        config_root: state.config_root.clone(),
    }
}

pub fn child_artifact_dir(base: &Path, child: u64, run_id: &str) -> PathBuf {
    base.join(format!("issue-{child}")).join(run_id)
}

/// Derive an isolated persisted workspace directory for a child run.
///
/// Mirrors the per-child/per-run layout already used for artifact directories,
/// so each child issue and each relaunch of that child gets its own workspace
/// tree under the parent `work_dir` base rather than sharing it.
pub fn child_work_dir(base: &Path, child: u64, run_id: &str) -> PathBuf {
    base.join("children")
        .join(format!("issue-{child}"))
        .join(run_id)
}

pub fn resume_child_process(
    request: &ChildWorkflowLaunchRequest,
) -> Result<ChildWorkflowRunResult, String> {
    run_child_workflow(request, ChildRunMode::Resume)
}

pub fn launch_child_process(
    request: &ChildWorkflowLaunchRequest,
) -> Result<ChildWorkflowRunResult, String> {
    run_child_workflow(request, ChildRunMode::Launch)
}

pub enum ChildRunMode {
    Launch,
    Resume,
}

pub fn run_child_workflow(
    request: &ChildWorkflowLaunchRequest,
    mode: ChildRunMode,
) -> Result<ChildWorkflowRunResult, String> {
    let config_root = &request.config_root;
    let config_id = validated_child_id(&request.config_id, "config id")?;
    let workflow_type_id = validated_child_id(&request.workflow_type_id, "type id")?;
    let mut config = resolve_workflow_config(config_id, config_root)
        .map_err(|err| format!("resolve child config '{config_id}': {err}"))?;
    let workflow_type = resolve_workflow_type(workflow_type_id, config_root)
        .map_err(|err| format!("resolve child workflow type: {err}"))?;
    apply_child_overrides(&mut config, request)?;
    let db_path = crate::runtime_paths::get_data_dir().join("checkpoints.db");
    if let Some(work_dir) = request.work_dir.as_deref() {
        std::fs::create_dir_all(work_dir)
            .map_err(|err| format!("create child work_dir '{}': {err}", work_dir.display()))?;
        crate::engine::continuation::write_workspace_owner_marker(work_dir, &request.run_id)
            .map_err(|err| format!("write child workspace owner marker: {err}"))?;
    }
    let run_context = child_run_context(&config, request)?;
    let instance =
        WorkflowInstance::create_with_run_id(workflow_type, config.clone(), &request.run_id);
    let mut runner = EngineRunner::with_db_path_and_context(
        instance,
        crate::engine::executor::ExecutorRegistry::with_defaults(),
        &db_path,
        run_context,
    )
    .map_err(|err| err.to_string())?;
    if matches!(mode, ChildRunMode::Resume) {
        prepare_child_resume(&db_path, request)?;
    }
    let outcome = runner.run().map_err(|err| err.to_string())?;
    child_result_from_run_outcome(outcome, request, &config, &db_path)
}
