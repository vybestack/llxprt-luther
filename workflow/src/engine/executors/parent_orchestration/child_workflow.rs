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
    crate::engine::workspace_ownership::verify_workspace_ownership(workspace, run_id)
        .map_or(Ok(()), Err)
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
    match mode {
        ChildRunMode::Launch => {
            launch_child_workflow(request, &workflow_type, &config, config_root, &db_path)
        }
        ChildRunMode::Resume => {
            resume_child_workflow(request, &workflow_type, &config, config_root, &db_path)
        }
    }
}

/// Launch a fresh child workflow: provision workspace ownership, insert the
/// starting run row with provenance, construct the runner, and run.
fn launch_child_workflow(
    request: &ChildWorkflowLaunchRequest,
    workflow_type: &crate::workflow::schema::WorkflowType,
    config: &WorkflowConfig,
    config_root: &Path,
    db_path: &Path,
) -> Result<ChildWorkflowRunResult, String> {
    // Launch provisions the durable workspace ownership marker; it is the
    // only path that may write it.
    if let Some(work_dir) = request.work_dir.as_deref() {
        crate::engine::workspace_ownership::provision_workspace_ownership(
            work_dir,
            &request.run_id,
        )
        .map_err(|err| format!("provision child workspace ownership: {err}"))?;
    }
    let launch_provenance =
        crate::persistence::LaunchProvenance::from_resolved(workflow_type, config, config_root)
            .map_err(|err| format!("record child launch provenance: {err}"))?;
    let mut run_context = child_run_context(config, request)?;
    run_context.launch_provenance = Some(launch_provenance);
    let instance = WorkflowInstance::create_with_run_id(
        workflow_type.clone(),
        config.clone(),
        &request.run_id,
    );
    // A fresh launch must fail closed if the initial Starting RunMetadata with
    // Some provenance cannot be atomically inserted (run_id collision or DB
    // error).
    let mut runner = EngineRunner::with_db_path_for_launch(
        instance,
        crate::engine::executor::ExecutorRegistry::with_defaults(),
        db_path,
        run_context,
    )
    .map_err(|err| err.to_string())?;
    let outcome = runner.run().map_err(|err| err.to_string())?;
    child_result_from_run_outcome(outcome, request, config, db_path)
}

/// Resume an existing child workflow: perform complete read-only validation
/// (identity, provenance, workspace marker, current-step, checkpoint,
/// authorization) BEFORE any durable mutation, then promote ownership, commit
/// the checkpoint, construct the runner, and run.
fn resume_child_workflow(
    request: &ChildWorkflowLaunchRequest,
    workflow_type: &crate::workflow::schema::WorkflowType,
    config: &WorkflowConfig,
    config_root: &Path,
    db_path: &Path,
) -> Result<ChildWorkflowRunResult, String> {
    // Issue 158 gap 1: perform the COMPLETE read-only validation BEFORE any
    // durable mutation. This returns the ephemeral WorkspaceAuthorization and
    // the selected checkpoint identity. On any failure (foreign owner, missing
    // evidence, malformed marker, provenance mismatch, missing current_step)
    // the child run aborts without touching markers, lease, or checkpoint.
    let prepared = child_resume_preparation::prepare_child_resume_readonly(
        db_path,
        request,
        workflow_type,
        config,
        config_root,
    )?;
    // Promote verified existing evidence only AFTER the read-only
    // authorization succeeded. Resume never creates a first claim.
    if let Some(work_dir) = request.work_dir.as_deref() {
        crate::engine::workspace_ownership::ensure_durable_workspace_ownership(
            work_dir,
            &request.run_id,
        )
        .map_err(|err| format!("verify child workspace ownership: {err}"))?;
    }
    // Commit the resume checkpoint using the identity selected during
    // read-only preparation. The commit re-validates the identity inside its
    // IMMEDIATE transaction.
    child_resume_preparation::commit_prepared_resume_checkpoint(
        db_path,
        request,
        prepared.checkpoint_identity(),
    )?;
    let run_context = child_run_context(config, request)?;
    let instance = WorkflowInstance::create_with_run_id(
        workflow_type.clone(),
        config.clone(),
        &request.run_id,
    );
    // Resume reuses the existing persisted row via the best-effort constructor.
    let mut runner = EngineRunner::with_db_path_and_context(
        instance,
        crate::engine::executor::ExecutorRegistry::with_defaults(),
        db_path,
        run_context,
    )
    .map_err(|err| err.to_string())?;
    // Inject the ephemeral WorkspaceAuthorization reconstructed from the
    // verified workspace descriptor BEFORE any resumed step executes.
    if let Some(authorization) = prepared.authorization() {
        runner.attach_workspace_authorization(authorization);
    }
    let outcome = runner.run().map_err(|err| err.to_string())?;
    child_result_from_run_outcome(outcome, request, config, db_path)
}

/// Verify the persisted launch provenance for a child resume against the
/// recomputed digests, refusing before any mutation on mismatch.
///
/// @plan:PLAN-20260722-ISSUE158-LAUNCH-PROVENANCE
pub(super) fn verify_child_resume_provenance(
    db_path: &Path,
    request: &ChildWorkflowLaunchRequest,
    workflow_type: &crate::workflow::schema::WorkflowType,
    config: &WorkflowConfig,
    config_root: &Path,
) -> Result<(), String> {
    let conn = rusqlite::Connection::open(db_path).map_err(|e| e.to_string())?;
    let metadata = crate::persistence::get_run_with_conn(&conn, &request.run_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("missing run metadata for child {}", request.run_id))?;
    let verification = crate::persistence::verify_provenance(
        &metadata.launch_provenance,
        workflow_type,
        config,
        config_root,
        crate::persistence::LegacyAllowed::Allowed,
    );
    match verification {
        crate::persistence::ProvenanceVerification::Match => Ok(()),
        crate::persistence::ProvenanceVerification::Legacy(warning) => {
            tracing::warn!("child run '{}': {warning}", request.run_id);
            Ok(())
        }
        crate::persistence::ProvenanceVerification::Mismatch(reason) => Err(format!(
            "child launch provenance mismatch for run '{}': {reason}",
            request.run_id
        )),
    }
}
