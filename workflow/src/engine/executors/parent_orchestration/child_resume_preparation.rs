//! Consolidated read-only child resume preparation.
//!
//! Issue 158 gap 1: the child resume path previously promoted workspace
//! ownership (a durable mutation) and constructed the runner (DB mutation)
//! BEFORE validating current-step / checkpoint / continuation authorization.
//! This module performs the **complete read-only** resume validation in one
//! cohesive pass — persisted identity, artifact, provenance, workspace
//! marker, current-step, checkpoint existence, and continuation authorization
//! — returning a [`PreparedChildResume`] that carries the ephemeral
//! [`WorkspaceAuthorization`] and the selected checkpoint identity.
//!
//! The caller may only perform durable mutations (ownership promotion,
//! checkpoint commit, runner construction) AFTER this function returns `Ok`.
//! On any failure, no mutation has occurred.
//!
//! @plan:PLAN-20260722-ISSUE158-CHILD-RESUME-PREPARATION

use std::path::Path;

use crate::engine::workspace_ownership::WorkspaceAuthorization;
use crate::persistence::{get_run_with_conn, RunMetadata};
use crate::workflow::schema::WorkflowConfig;
use crate::workflow::schema::WorkflowType;

use super::child_run::{
    commit_resume_checkpoint_with_identity, missing_run_metadata, missing_workspace_path,
    require_current_step, require_matching_workspace, validate_child_resume_artifact,
    validate_child_resume_identity,
};
use super::lease::open_parent_orchestration_connection;
use super::{
    verify_child_resume_provenance, verify_existing_workspace_owner_marker,
    ChildWorkflowLaunchRequest,
};

/// The result of a complete read-only child resume preparation.
///
/// Carries the ephemeral [`WorkspaceAuthorization`] reconstructed from the
/// verified workspace descriptor and the selected checkpoint identity, so the
/// caller can perform the durable mutations (ownership promotion, checkpoint
/// commit, runner construction) using verified state without re-validating.
///
/// Constructed exclusively by [`prepare_child_resume_readonly`], which
/// performs only read-only operations. The type is opaque outside this module
/// so a caller cannot forge the prepared state.
pub(super) struct PreparedChildResume {
    authorization: Option<WorkspaceAuthorization>,
    checkpoint_identity: String,
}

impl PreparedChildResume {
    /// The ephemeral [`WorkspaceAuthorization`] reconstructed from the verified
    /// workspace descriptor, if the request had a work_dir. Inject this into
    /// the resumed runner's `RunContext` before any resumed step executes.
    #[must_use]
    pub(super) fn authorization(&self) -> Option<WorkspaceAuthorization> {
        self.authorization
    }

    /// The exact `step_id@rfc3339` identity of the selected resume checkpoint.
    /// The caller passes this to [`commit_resume_checkpoint_with_identity`]
    /// so the commit transaction can verify it has not been substituted.
    #[must_use]
    pub(super) fn checkpoint_identity(&self) -> &str {
        &self.checkpoint_identity
    }
}

/// Perform the complete read-only child resume validation, returning a
/// [`PreparedChildResume`] with the ephemeral authorization and selected
/// checkpoint identity.
///
/// This function performs **only read-only** operations:
/// - Verifies launch provenance against recomputed digests.
/// - Validates persisted identity (run_id, workflow_type_id, config_id, repo,
///   issue_number, workspace_path, artifact_root) against the request.
/// - Verifies the existing workspace owner marker is present and owned by the
///   resuming run id (fail-closed on missing/foreign/malformed).
/// - Requires a non-empty persisted current_step.
/// - Selects the resume checkpoint (read-only selection).
/// - Reconstructs the ephemeral [`WorkspaceAuthorization`] from the verified
///   workspace descriptor.
///
/// It **never** performs ownership promotion, checkpoint commit, lease CAS,
/// or run mutation. On any failure it returns `Err` and leaves all durable
/// state unchanged. The caller must perform mutations only after this returns
/// `Ok`.
///
/// @plan:PLAN-20260722-ISSUE158-CHILD-RESUME-PREPARATION
pub(super) fn prepare_child_resume_readonly(
    db_path: &Path,
    request: &ChildWorkflowLaunchRequest,
    workflow_type: &WorkflowType,
    config: &WorkflowConfig,
    config_root: &Path,
) -> Result<PreparedChildResume, String> {
    verify_child_resume_provenance(db_path, request, workflow_type, config, config_root)?;
    let conn = open_parent_orchestration_connection(db_path)?;
    let metadata = load_and_validate_resume_metadata(&conn, request)?;
    // Issue 158: reject child resume while a legacy ownership migration is
    // durably pending. A pending migration row signals an incomplete migration
    // that may have published the marker without recording the completion
    // audit; the resume trust contract requires a durable `completed`
    // migration before the migrated marker is trusted.
    if crate::persistence::migration_is_pending(&conn, &request.run_id) {
        return Err(format!(
            "child resume refused for run '{}': a legacy ownership migration is pending \
             (intent recorded but not completed)",
            request.run_id
        ));
    }
    let workspace = metadata.workspace_path.as_deref().unwrap_or_default();
    verify_existing_workspace_owner_marker(Path::new(workspace), &request.run_id)?;
    require_current_step(&metadata, &request.run_id)?;
    let checkpoint_identity = select_resume_checkpoint_identity(&conn, request)?;
    let authorization = prepare_ephemeral_authorization(request)?;
    Ok(PreparedChildResume {
        authorization,
        checkpoint_identity,
    })
}

/// Load persisted run metadata and validate every identity-bearing field
/// against the request. Read-only; fails closed on any mismatch.
fn load_and_validate_resume_metadata(
    conn: &rusqlite::Connection,
    request: &ChildWorkflowLaunchRequest,
) -> Result<RunMetadata, String> {
    let metadata = get_run_with_conn(conn, &request.run_id)
        .map_err(|err| format!("get child run metadata: {err}"))?
        .ok_or_else(|| missing_run_metadata(&request.run_id))?;
    let persisted_workspace = metadata
        .workspace_path
        .as_deref()
        .ok_or_else(|| missing_workspace_path(&request.run_id))?;
    let request_workspace = require_matching_workspace(request, persisted_workspace)?;
    validate_child_resume_identity(request, &metadata, request_workspace)?;
    validate_child_resume_artifact(request, &metadata)?;
    Ok(metadata)
}

/// Select the resume checkpoint (read-only) and return its identity string.
fn select_resume_checkpoint_identity(
    conn: &rusqlite::Connection,
    request: &ChildWorkflowLaunchRequest,
) -> Result<String, String> {
    let resume_request = crate::engine::ContinuationRequest {
        run_id: request.run_id.clone(),
        kind: crate::engine::ContinuationKind::Resume,
        force: true,
        trusted_internal: true,
    };
    let metadata = get_run_with_conn(conn, &request.run_id)
        .map_err(|err| format!("get child run metadata for checkpoint: {err}"))?
        .ok_or_else(|| missing_run_metadata(&request.run_id))?;
    let checkpoint =
        crate::engine::continuation::select_checkpoint(conn, &resume_request, &metadata)
            .map_err(|err| format!("select child resume checkpoint: {err}"))?;
    Ok(crate::engine::continuation::checkpoint_identity(
        &checkpoint,
    ))
}

/// Reconstruct the ephemeral [`WorkspaceAuthorization`] from the verified
/// workspace descriptor. Read-only and fail-closed.
fn prepare_ephemeral_authorization(
    request: &ChildWorkflowLaunchRequest,
) -> Result<Option<WorkspaceAuthorization>, String> {
    match request.work_dir.as_deref() {
        Some(work_dir) => {
            let prepared = crate::engine::continuation::prepare_resume_authorization(
                work_dir.to_str(),
                &request.run_id,
            )
            .map_err(|err| format!("child resume authorization: {err}"))?;
            Ok(Some(prepared.authorization()))
        }
        None => Ok(None),
    }
}

/// Commit the resume checkpoint using the identity selected during read-only
/// preparation. This is the only mutation performed after preparation.
pub(super) fn commit_prepared_resume_checkpoint(
    db_path: &Path,
    request: &ChildWorkflowLaunchRequest,
    checkpoint_identity: &str,
) -> Result<(), String> {
    let conn = open_parent_orchestration_connection(db_path)?;
    commit_resume_checkpoint_with_identity(&conn, request, checkpoint_identity)
}
