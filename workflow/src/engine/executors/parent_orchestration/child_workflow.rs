use super::*;

use crate::workflow::config_loader::{resolve_workflow_config, resolve_workflow_type};

/// Verify (without writing) that a child workspace's durable ownership marker
/// already exists and belongs to the resume's `run_id`.
///
/// A resume re-enters a workspace that a prior launch provisioned and therefore
/// must never (re)write the marker. Instead it verifies the marker is present
/// and owned by the resuming run id, failing closed (returning an error) when
/// the marker is missing, empty, malformed, or owned by a different run. This
/// prevents a resume from silently claiming a workspace that was never
/// provisioned for it or that a concurrent run has since claimed.
pub(super) fn verify_existing_workspace_owner_marker(
    workspace: &Path,
    run_id: &str,
) -> Result<(), String> {
    crate::engine::continuation::verify_workspace_ownership_marker(workspace, run_id)
        .map_or(Ok(()), Err)
}

/// Provision a child workspace's durable ownership marker.
///
/// Only the launch path may write the marker: it is the provisioning moment
/// that establishes durable ownership for the new run id. This writes the
/// `.luther/workspace-owner` marker atomically (create-new on first provision,
/// idempotent on a same-owner match) and rejects a workspace already owned by a
/// different run.
pub(super) fn write_child_workspace_owner_marker(
    workspace: &Path,
    run_id: &str,
) -> Result<(), String> {
    std::fs::create_dir_all(workspace)
        .map_err(|err| format!("create child work_dir '{}': {err}", workspace.display()))?;
    crate::engine::continuation::write_workspace_owner_marker(workspace, run_id)
        .map_err(|err| format!("write child workspace owner marker: {err}"))
}

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

/// Reject a run id that is not a safe single path component.
///
/// A run id is interpolated directly into child artifact and work directories
/// (`base/issue-{child}/{run_id}` and `base/children/issue-{child}/{run_id}`),
/// so it must be a safe single path component: non-empty, no path separators
/// (`/`, ``), no parent-directory traversal (`..`), no NUL byte, and only
/// otherwise-permissible identifier characters. This prevents a hostile or
/// malformed run id from escaping the per-child directory subtree into an
/// arbitrary filesystem location.
pub fn validate_run_id_path_component(run_id: &str) -> Result<(), String> {
    if run_id.is_empty() {
        return Err("child workflow run id must not be empty".to_string());
    }
    if run_id.contains('/') || run_id.contains('\\') {
        return Err(format!(
            "child workflow run id '{run_id}' must not contain path separators"
        ));
    }
    if run_id == "." || run_id == ".." || run_id.contains('\0') {
        return Err(format!(
            "child workflow run id '{run_id}' must be a safe single path component"
        ));
    }
    if !run_id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(format!(
            "child workflow run id '{run_id}' must contain only alphanumeric, '-', or '_' characters"
        ));
    }
    Ok(())
}

pub fn run_child_workflow(
    request: &ChildWorkflowLaunchRequest,
    mode: ChildRunMode,
) -> Result<ChildWorkflowRunResult, String> {
    validate_run_id_path_component(&request.run_id)?;
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
        match mode {
            // Launch provisions the durable workspace ownership marker; it is
            // the only path that may write it.
            ChildRunMode::Launch => {
                write_child_workspace_owner_marker(work_dir, &request.run_id)?;
            }
            // Resume must verify the marker already exists and belongs to the
            // resuming run id, never (re)writing it. A missing or foreign-owned
            // marker means the resume cannot safely proceed.
            ChildRunMode::Resume => {
                verify_existing_workspace_owner_marker(work_dir, &request.run_id)?;
            }
        }
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
