use super::*;

/// Reject a run id that is not a safe single path component.
pub fn validate_run_id_path_component(run_id: &str) -> Result<(), String> {
    match validate_safe_run_id_bytes(run_id) {
        RunIdValidation::Valid => Ok(()),
        RunIdValidation::Empty | RunIdValidation::Unsafe => Err(format!(
            "child workflow run id '{}' must contain only alphanumeric, '-', or '_' characters",
            run_id
        )),
    }
}

/// Byte-level classification of a candidate run id.
enum RunIdValidation {
    Valid,
    Empty,
    Unsafe,
}

/// Classify a run id without closures so the complexity parser tracks each
/// function boundary correctly.
fn validate_safe_run_id_bytes(run_id: &str) -> RunIdValidation {
    if run_id.is_empty() {
        return RunIdValidation::Empty;
    }
    match run_id.bytes().all(is_safe_run_id_byte) {
        true => RunIdValidation::Valid,
        false => RunIdValidation::Unsafe,
    }
}

/// Predicate for a single safe run id byte.
fn is_safe_run_id_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

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

/// Build the error string for a missing child run metadata row.
pub(super) fn missing_run_metadata(run_id: &str) -> String {
    format!("missing child run metadata for child workflow run id {run_id}")
}

/// Build the error string for a missing persisted workspace_path.
pub(super) fn missing_workspace_path(run_id: &str) -> String {
    format!("missing workspace_path for child workflow resume {run_id}")
}

/// Require the request work_dir to be present and to match the persisted
/// workspace exactly. Returns the verified request workspace string.
pub(super) fn require_matching_workspace<'a>(
    request: &'a ChildWorkflowLaunchRequest,
    persisted_workspace: &str,
) -> Result<&'a str, String> {
    let request_workspace = request
        .work_dir
        .as_deref()
        .and_then(Path::to_str)
        .ok_or_else(|| {
            format!(
                "missing work_dir for child workflow resume {}",
                request.run_id
            )
        })?;
    if request_workspace != persisted_workspace {
        return Err(format!(
            "child resume workspace mismatch for {}: request work_dir '{}' does not match persisted workspace '{}'",
            request.run_id, request_workspace, persisted_workspace
        ));
    }
    Ok(request_workspace)
}

/// Validate every persisted identity-bearing request field against persisted
/// metadata. A mismatch on any field is rejected outright.
pub(super) fn validate_child_resume_identity(
    request: &ChildWorkflowLaunchRequest,
    metadata: &crate::persistence::RunMetadata,
    request_workspace: &str,
) -> Result<(), String> {
    if metadata.run_id != request.run_id {
        return Err(identity_mismatch(
            &request.run_id,
            "persisted run_id",
            &metadata.run_id,
        ));
    }
    if metadata.workflow_type_id != request.workflow_type_id {
        return Err(field_mismatch(
            &request.run_id,
            "workflow_type_id",
            &request.workflow_type_id,
            &metadata.workflow_type_id,
        ));
    }
    if metadata.config_id != request.config_id {
        return Err(field_mismatch(
            &request.run_id,
            "config_id",
            &request.config_id,
            &metadata.config_id,
        ));
    }
    if metadata.repository.as_deref() != Some(request.repo.as_str()) {
        return Err(format!(
            "child resume identity mismatch for {}: request repo '{}' does not match persisted repository {:?}",
            request.run_id, request.repo, metadata.repository
        ));
    }
    let persisted_issue = metadata
        .issue_number
        .and_then(|number| u64::try_from(number).ok());
    if persisted_issue != Some(request.issue_number) {
        return Err(format!(
            "child resume identity mismatch for {}: request issue_number {} does not match persisted issue {:?}",
            request.run_id, request.issue_number, persisted_issue
        ));
    }
    if request_workspace != metadata.workspace_path.as_deref().unwrap_or_default() {
        return Err(format!(
            "child resume workspace mismatch for {}: request work_dir '{}' does not match persisted workspace '{}'",
            request.run_id,
            request_workspace,
            metadata.workspace_path.as_deref().unwrap_or_default()
        ));
    }
    Ok(())
}

/// Validate the artifact directory against the persisted artifact root.
///
/// Exact typed `Option<PathBuf>` comparison (issue 158 finding 3): the
/// request's `artifact_dir` (`Option<PathBuf>`) must match the persisted
/// `artifact_root` reconstructed as `Option<PathBuf>` exactly, including the
/// `Some`/`None` direction both ways. This avoids a lossy `to_str()`/`str`
/// comparison that could accept a path with a non-UTF-8 byte sequence that
/// round-trips differently, and that only compared when both sides were
/// `Some` (silently passing a present request value against a missing
/// persisted root). A missing request `artifact_dir` is acceptable (it is
/// reconstructed from persisted identity downstream); a missing persisted
/// `artifact_root` with a present request value is a mismatch.
pub(super) fn validate_child_resume_artifact(
    request: &ChildWorkflowLaunchRequest,
    metadata: &crate::persistence::RunMetadata,
) -> Result<(), String> {
    let request_artifact = request.artifact_dir.as_deref();
    let persisted_artifact: Option<std::path::PathBuf> = metadata
        .artifact_root
        .as_ref()
        .map(std::path::PathBuf::from);
    if let Some(request_artifact_dir) = request_artifact {
        if persisted_artifact.as_deref() != Some(request_artifact_dir) {
            return Err(format!(
                "child resume artifact mismatch for {}: request artifact_dir '{}' does not match persisted artifact_root {:?}",
                request.run_id,
                request_artifact_dir.display(),
                persisted_artifact
            ));
        }
    }
    Ok(())
}

/// Build a generic identity-mismatch error for a non-field comparison.
fn identity_mismatch(run_id: &str, label: &str, persisted: &str) -> String {
    format!(
        "child resume identity mismatch for {}: {} {} does not match request",
        run_id, label, persisted
    )
}

/// Build a field-level identity-mismatch error comparing request vs persisted.
fn field_mismatch(run_id: &str, field: &str, request_val: &str, persisted_val: &str) -> String {
    format!(
        "child resume identity mismatch for {}: request {} '{}' does not match persisted '{}'",
        run_id, field, request_val, persisted_val
    )
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
        .map_err(|err| format!("apply child target overrides: {err}"))?;
    config
        .variables
        .insert("daemon_managed_claim".to_string(), "true".to_string());
    Ok(())
}

/// Require a non-empty persisted current_step for the resume.
pub(super) fn require_current_step(
    metadata: &crate::persistence::RunMetadata,
    run_id: &str,
) -> Result<(), String> {
    if metadata
        .current_step
        .as_deref()
        .unwrap_or_default()
        .is_empty()
    {
        return Err(format!("missing current_step for child resume {run_id}"));
    }
    Ok(())
}

/// Commit the resume continuation using a checkpoint identity selected
/// during read-only preparation. This is the mutation-only companion to
/// [`super::child_resume_preparation::prepare_child_resume_readonly`]: it
/// re-validates the bound identity inside the `IMMEDIATE` transaction so a
/// concurrent checkpoint replacement cannot sneak through as a stale or
/// substituted resume point.
pub(super) fn commit_resume_checkpoint_with_identity(
    conn: &rusqlite::Connection,
    request: &ChildWorkflowLaunchRequest,
    checkpoint_identity: &str,
) -> Result<(), String> {
    let resume_request = crate::engine::ContinuationRequest {
        run_id: request.run_id.clone(),
        kind: crate::engine::ContinuationKind::Resume,
        force: true,
        trusted_internal: true,
    };
    crate::engine::commit_continuation(conn, &resume_request, checkpoint_identity)
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
        daemon_managed: true,
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
        workspace_authorization: None,
        // Set by run_child_workflow for launch; resume leaves None and verifies
        // separately. @plan:PLAN-20260722-ISSUE158-LAUNCH-PROVENANCE
        launch_provenance: None,
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
        RunOutcome::Failure { step_id, reason } => {
            // Preserve the child failure diagnostics before collapsing to
            // CompletedFailure; the caller in lease.rs returns this result
            // directly without logging, so without this the root cause is lost.
            tracing::warn!(
                run_id = %request.run_id,
                step_id = %step_id,
                reason = %reason,
                "child workflow failed"
            );
            Ok(ChildWorkflowRunResult::CompletedFailure)
        }
        RunOutcome::Abandoned { step_id, reason } => {
            tracing::warn!(
                run_id = %request.run_id,
                step_id = %step_id,
                reason = %reason,
                "child workflow abandoned"
            );
            Ok(ChildWorkflowRunResult::CompletedFailure)
        }
    }
}
