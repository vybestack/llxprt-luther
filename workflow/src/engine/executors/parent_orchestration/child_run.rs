use super::*;

pub fn validated_child_id<'a>(value: &'a str, label: &str) -> Result<&'a str, String> {
    if !is_safe_child_id(value) {
        return Err(format!("unsafe child workflow {label} '{value}'"));
    }
    Ok(value)
}

fn is_safe_child_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

pub fn apply_child_overrides(
    config: &mut WorkflowConfig,
    request: &ChildWorkflowLaunchRequest,
) -> Result<(), String> {
    let overrides = TargetProfileOverrides {
        repo: Some(request.repo.clone()),
        issue: Some(request.issue_number.to_string()),
        work_dir: request.work_dir.clone(),
        artifact_dir: request.artifact_dir.clone(),
    };
    apply_target_profile_overrides(config, &overrides)
        .map_err(|err| format!("apply child target overrides: {err}"))
}

pub fn prepare_child_resume(
    db_path: &Path,
    request: &ChildWorkflowLaunchRequest,
) -> Result<(), String> {
    let conn = open_parent_orchestration_connection(db_path)?;
    let metadata = get_run_with_conn(&conn, &request.run_id)
        .map_err(|err| err.to_string())?
        .ok_or_else(|| {
            format!(
                "missing child run metadata for head_sha child workflow run id {}",
                request.run_id
            )
        })?;
    let step = metadata
        .current_step
        .as_deref()
        .filter(|step| !step.is_empty())
        .ok_or_else(|| format!("missing current_step for child resume {}", request.run_id))?;
    crate::engine::commit_continuation(
        &conn,
        &crate::engine::ContinuationRequest {
            run_id: request.run_id.clone(),
            kind: crate::engine::ContinuationKind::Resume,
            force: true,
        },
        step,
    )
    .map(|_| ())
    .map_err(|err| format!("commit child resume: {err}"))
}

pub fn child_run_context(
    config: &WorkflowConfig,
    request: &ChildWorkflowLaunchRequest,
) -> Result<RunContext, String> {
    let issue_number = i64::try_from(request.issue_number).map_err(|_| {
        format!(
            "child issue number {} exceeds supported range",
            request.issue_number
        )
    })?;
    Ok(RunContext {
        log_path: None,
        artifact_root: request
            .artifact_dir
            .as_ref()
            .map(|path| path.to_string_lossy().to_string())
            .or_else(|| config.variables.get("artifact_dir").cloned()),
        workspace_path: request
            .work_dir
            .as_ref()
            .map(|path| path.to_string_lossy().to_string())
            .or_else(|| config.variables.get("work_dir").cloned()),
        repository: Some(request.repo.clone()),
        issue_number: Some(issue_number),
        pr_number: None,
        head_sha: None,
    })
}

pub fn child_result_from_run_outcome(
    outcome: RunOutcome,
    request: &ChildWorkflowLaunchRequest,
    config: &WorkflowConfig,
    db_path: &Path,
) -> Result<ChildWorkflowRunResult, String> {
    match outcome {
        RunOutcome::Success => Ok(ChildWorkflowRunResult::CompletedSuccess),
        RunOutcome::WaitingExternal { step_id, reason } => {
            persist_child_external_wait_state(request, config, db_path, &step_id, &reason)
                .map_err(|err| err.to_string())?;
            Ok(ChildWorkflowRunResult::WaitingExternal)
        }
        RunOutcome::Interrupted { step_id } => {
            persist_child_interrupted_state(request, config, db_path, &step_id)
                .map_err(|err| err.to_string())?;
            Ok(ChildWorkflowRunResult::WaitingExternal)
        }
        RunOutcome::Failure { .. } | RunOutcome::Abandoned { .. } => {
            Ok(ChildWorkflowRunResult::CompletedFailure)
        }
    }
}
