//! Safety checks for continuation validation.
//!
//! Extracted from the parent continuation module to keep the validation check
//! primitives in a single cohesive unit. Each check returns a [`SafetyCheck`]
//! capturing whether it passed and a human-readable detail.
//!
//! @plan:PLAN-20260623-LUTHER-CONTINUATION

use std::path::Path;

use rusqlite::Connection;

use crate::persistence::{Checkpoint, RunMetadata, RunStatus};

use super::{
    authorization::checkpoint_is_authorized, is_safe_rerun_step, ContinuationError,
    ContinuationKind, ContinuationRequest, SafetyCheck,
};
pub(super) fn pass(name: &str, detail: impl Into<String>) -> SafetyCheck {
    SafetyCheck {
        name: name.to_string(),
        passed: true,
        detail: detail.into(),
    }
}

pub(super) fn fail(name: &str, detail: impl Into<String>) -> SafetyCheck {
    SafetyCheck {
        name: name.to_string(),
        passed: false,
        detail: detail.into(),
    }
}

pub(super) fn check_run_exists(metadata: &Option<RunMetadata>, run_id: &str) -> SafetyCheck {
    match metadata {
        Some(_) => pass("run_exists", format!("run {run_id} found in registry")),
        None => fail("run_exists", format!("run {run_id} not found in registry")),
    }
}

pub(super) fn check_workflow_resolvable(metadata: &RunMetadata) -> SafetyCheck {
    if metadata.workflow_type_id.is_empty() || metadata.config_id.is_empty() {
        fail(
            "workflow_resolvable",
            "run record is missing workflow_type_id or config_id",
        )
    } else {
        pass(
            "workflow_resolvable",
            format!(
                "workflow_type={} config={}",
                metadata.workflow_type_id, metadata.config_id
            ),
        )
    }
}

/// Refuse to reopen terminal non-failed runs (Completed/Merged/Abandoned/
/// Cancelled). `Failed` is the single intentional terminal exception, encoded in
/// `RunStatus::is_resumable`. A `Running` run is accepted when its recorded
/// workflow PID is stale or unrecorded, or when `--force` is specified. Force
/// overrides even a live Running claim, so operators must only use it after
/// confirming the recorded process is unrelated; all other terminal refusals
/// remain non-bypassable.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub(super) fn check_resumable_status(
    metadata: &RunMetadata,
    request: &ContinuationRequest,
) -> SafetyCheck {
    let cleanup_recovery = metadata.is_cleanup_failure_abandonment()
        && metadata
            .failure_cleanup
            .as_ref()
            .is_some_and(crate::persistence::FailureCleanupState::recovery_is_available)
        && request.force
        && matches!(
            request.kind,
            ContinuationKind::Retry { .. } | ContinuationKind::Rewind { .. }
        );
    if metadata.status.is_resumable() {
        pass(
            "resumable_status",
            format!("run status {} is resumable", metadata.status),
        )
    } else if cleanup_recovery {
        pass(
            "resumable_status",
            "forced retry/rewind of an evidenced cleanup abandonment",
        )
    } else if running_claim_is_available(metadata) {
        pass(
            "resumable_status",
            format!(
                "run status {} is resumable because recorded workflow PID is stale or unrecorded",
                metadata.status
            ),
        )
    } else {
        fail(
            "resumable_status",
            format!(
                "run status {} is not resumable; terminal states other than failed cannot be continued",
                metadata.status
            ),
        )
    }
}

/// A continuation must always have a repository plus an issue or PR anchor before
/// executor dispatch; a repo-only or anchor-less row cannot safely target work.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub(super) fn check_identity_recoverable(metadata: &RunMetadata) -> SafetyCheck {
    let has_repo = metadata.repository.is_some();
    let has_anchor = metadata.issue_number.is_some() || metadata.pr_number.is_some();
    if has_repo && has_anchor {
        pass(
            "identity_recoverable",
            format!(
                "repository={:?} issue={:?} pr={:?}",
                metadata.repository, metadata.issue_number, metadata.pr_number
            ),
        )
    } else {
        fail(
            "identity_recoverable",
            format!(
                "continuation requires a repository plus an issue or PR anchor; got repository={:?} issue={:?} pr={:?}",
                metadata.repository, metadata.issue_number, metadata.pr_number
            ),
        )
    }
}

pub(super) fn check_workspace(metadata: &RunMetadata) -> SafetyCheck {
    match &metadata.workspace_path {
        Some(path) if metadata.is_cleanup_failure_abandonment() => {
            check_cleanup_workspace_ownership(Path::new(path), metadata)
        }
        Some(path) if Path::new(path).is_dir() => {
            pass("workspace", format!("workspace_path={path}"))
        }
        Some(path) => pass("workspace", format!("workspace_path={path}")),
        None if metadata.is_cleanup_failure_abandonment() => fail(
            "workspace",
            "cleanup-abandonment recovery requires an explicit preserved workspace",
        ),
        None => pass("workspace", "workspace path reconstructable from run id"),
    }
}

/// Strengthened workspace ownership validation for cleanup-failure-abandonment
/// recovery. Rejects symlinks, requires the workspace to be a real directory,
/// and verifies durable workspace ownership via the `.luther/workspace-owner`
/// regular-file marker bound to the run_id, preventing wrong-owner
/// substitutions.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub(super) fn check_cleanup_workspace_ownership(
    path: &Path,
    metadata: &RunMetadata,
) -> SafetyCheck {
    // Reject symlinks: a symlinked workspace could redirect cleanup recovery
    // into an attacker-controlled directory. Use symlink_metadata so the link
    // itself is inspected rather than its target.
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return fail(
                "workspace",
                format!(
                    "cleanup workspace must not be a symlink: {path_display}",
                    path_display = path.display()
                ),
            );
        }
        Ok(_) => {}
        Err(_) => {
            return fail(
                "workspace",
                format!(
                    "preserved workspace is missing or not a directory: {path_display}",
                    path_display = path.display()
                ),
            );
        }
    }
    if !path.is_dir() {
        return fail(
            "workspace",
            format!(
                "preserved workspace is not a directory: {path_display}",
                path_display = path.display()
            ),
        );
    }
    // Verify durable workspace ownership via the `.luther/workspace-owner`
    // regular-file marker, which records the run_id that created the
    // workspace. This prevents a wrong-owner substitution where a different
    // run's workspace is pointed at by the recorded workspace_path. The
    // marker is required: a missing, empty, non-regular (directory/symlink),
    // or mismatched marker all fail closed.
    if let Some(reason) =
        super::workspace_marker::verify_workspace_ownership_marker(path, &metadata.run_id)
    {
        return fail("workspace", reason);
    }
    pass(
        "workspace",
        format!(
            "workspace_path={path_display} (ownership verified)",
            path_display = path.display()
        ),
    )
}

pub(super) fn check_checkpoint_exists(
    selection: &Result<Checkpoint, ContinuationError>,
) -> SafetyCheck {
    match selection {
        Ok(cp) => pass(
            "checkpoint_exists",
            format!("selected checkpoint at step {}", cp.step_id),
        ),
        Err(err) => fail("checkpoint_exists", err.to_string()),
    }
}

pub(super) fn check_safe_step(step_id: &str, force: bool, authorized: bool) -> SafetyCheck {
    if is_safe_rerun_step(step_id) || authorized {
        pass(
            "safe_step",
            format!("step {step_id} is explicitly authorized for this rerun"),
        )
    } else {
        let force_note = if force {
            "; --force cannot bypass rerun safety"
        } else {
            ""
        };
        fail(
            "safe_step",
            format!("step {step_id} is not safe to re-run{force_note}"),
        )
    }
}

pub(super) fn running_claim_is_available(metadata: &RunMetadata) -> bool {
    metadata.status == RunStatus::Running
        && (metadata.is_process_stale() || metadata.process_pid.is_none())
}

pub(super) fn reopen_status_is_allowed(
    metadata: &RunMetadata,
    request: &ContinuationRequest,
) -> bool {
    metadata.status.is_resumable()
        || metadata.status == RunStatus::Running
        || (metadata.is_cleanup_failure_abandonment()
            && metadata
                .failure_cleanup
                .as_ref()
                .is_some_and(crate::persistence::FailureCleanupState::recovery_is_available)
            && request.force
            && matches!(
                request.kind,
                ContinuationKind::Retry { .. } | ContinuationKind::Rewind { .. }
            ))
}

/// Validate a continuation request against the issue's checkpoint-safety list.
/// Returns per-check diagnostics; the caller refuses when `!ok`.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn validate_continuation(
    conn: &Connection,
    request: &ContinuationRequest,
) -> Result<super::ContinuationValidation, crate::persistence::PersistenceError> {
    let mut checks = Vec::new();
    let metadata = crate::persistence::get_run_with_conn(conn, &request.run_id)?;
    checks.push(check_run_exists(&metadata, &request.run_id));
    let Some(metadata) = metadata else {
        return Ok(super::ContinuationValidation::from_checks(checks));
    };
    checks.push(check_workflow_resolvable(&metadata));
    checks.push(check_resumable_status(&metadata, request));
    checks.push(check_identity_recoverable(&metadata));
    checks.push(check_workspace(&metadata));
    let selection = super::select_checkpoint(conn, request, &metadata);
    checks.push(check_checkpoint_exists(&selection));
    if let Ok(cp) = &selection {
        let authorized = checkpoint_is_authorized(conn, &metadata, request, cp);
        checks.push(check_safe_step(&cp.step_id, request.force, authorized));
    }
    Ok(super::ContinuationValidation::from_checks(checks))
}
