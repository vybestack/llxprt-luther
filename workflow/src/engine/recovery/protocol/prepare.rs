//! Prepare phase: read-only loading and authority capture. [B1/B6/C8]
//!
//! Loads and verifies the capsule, resolves the adapter/policy/strategy,
//! captures the authority snapshot, and anchors workspace ownership. Does not
//! mutate durable state.

use std::path::Path;

use rusqlite::Connection;

use super::super::adapters::adapter_for;
use super::super::capsule::{verify_envelope_digest, ExecutionCapsuleV1};
use super::super::policy::policy_for_step;
use crate::engine::workspace_ownership::{
    adjudicate_workspace_ownership, OwnershipVerdict, VerifiedWorkspace, WorkspaceAuthorization,
};
use crate::persistence::attempts::AttemptRow;
use crate::persistence::capsule_store::load_capsule_v1;
use crate::persistence::checkpoint::load_checkpoint_with_conn;
use crate::persistence::recovery_operations::{
    compute_intent_digest, compute_logical_request_key, compute_operation_id,
};
use crate::persistence::sqlite::get_run_with_conn;
use crate::persistence::wait_state::get_wait_state;

use super::{
    checkpoint_identity_of, map_adapter, map_persist, normalize_operator_verb, CheckpointIdentity,
    PreparedRecovery, PreparedRecoveryParts, RecoveryAuthority, RecoveryError, RecoveryRequest,
    RecoveryStrategy,
};

/// Run the prepare phase: load+verify the capsule, resolve the adapter, policy,
/// strategy, capture the authority snapshot, and anchor workspace ownership.
///
/// Read-only with respect to durable state. [B1/B6/C8]
pub(super) fn run(
    conn: &Connection,
    workspace: &Path,
    request: &RecoveryRequest,
) -> Result<PreparedRecovery, RecoveryError> {
    let capsule = load_capsule_v1(conn, &request.run_id).map_err(map_persist("load capsule"))?;
    verify_envelope_digest(&capsule).map_err(|e| RecoveryError::Capsule(e.to_string()))?;

    let step_def = resolve_step_def(&capsule, &request.step_id)?;
    let policy = policy_for_step(&step_def, &request.step_id);
    let strategy = super::super::policy::select_strategy(policy.clone());

    let source_attempt = latest_attempt_for_step(conn, &request.run_id, &request.step_id)?;

    let (verified_workspace, workspace_authorization) =
        anchor_workspace_ownership(workspace, &request.run_id, &strategy)?;

    let bindings = compute_bindings(request, &capsule, source_attempt.as_ref());

    let metadata = get_run_with_conn(conn, &request.run_id)
        .map_err(map_persist("get run"))?
        .ok_or_else(|| RecoveryError::Persistence(format!("run not found: {}", request.run_id)))?;

    let checkpoint_identity = load_checkpoint_identity(conn, &request.run_id)?;
    let wait_state =
        get_wait_state(conn, &request.run_id).map_err(map_persist("get wait state"))?;
    let lease = load_issue_lease(conn, &metadata);

    let authority = RecoveryAuthority::new(
        workspace_authorization,
        capsule,
        source_attempt,
        policy,
        strategy,
    );

    Ok(PreparedRecovery::new(PreparedRecoveryParts {
        authority,
        expected_epoch: request.expected_epoch,
        step_id: request.step_id.clone(),
        operator_verb: request.operator_verb,
        operation_id: bindings.operation_id,
        logical_request_key: bindings.logical_request_key,
        intent_digest: bindings.intent_digest,
        run_status: metadata.status.clone(),
        current_step: metadata.current_step.clone(),
        live_pid: metadata.process_pid,
        checkpoint_identity,
        wait_state,
        lease,
        verified_workspace,
        workspace_path: workspace.to_path_buf(),
    }))
}

/// Derive a stable checkpoint identity (`step@timestamp`) for the most-recently
/// selected checkpoint, if any. [B1]
///
/// Uses [`load_checkpoint_with_conn`] (the newest-first resume loader) so the
/// identity matches exactly what a resume would reselect. The stable identity
/// is `step_id@rfc3339_timestamp`, compared for exact equality at reserve.
fn load_checkpoint_identity(
    conn: &Connection,
    run_id: &str,
) -> Result<Option<CheckpointIdentity>, RecoveryError> {
    let Some(checkpoint) = load_checkpoint_with_conn(conn, run_id)
        .map_err(|e| RecoveryError::Persistence(format!("load checkpoint for identity: {e}")))?
    else {
        return Ok(None);
    };
    Ok(Some(checkpoint_identity_of(&checkpoint)))
}

/// Load the matching issue lease for a run when its metadata carries a
/// repository and issue number. [B1]
///
/// Returns `None` when the run has no issue anchor (e.g. a PR-only run or a
/// non-daemon CLI run) — such a run has no durable lease authority to capture.
fn load_issue_lease(
    conn: &Connection,
    metadata: &crate::persistence::RunMetadata,
) -> Option<crate::persistence::leases::IssueLease> {
    let repo = metadata.repository.as_deref()?;
    let issue_number = metadata.issue_lease_number()?;
    crate::persistence::leases::get_lease_for_run(conn, &metadata.run_id)
        .or_else(|_| crate::persistence::leases::get_lease_for_issue(conn, repo, issue_number))
        .ok()
        .flatten()
}

/// Computed normalized binding keys for operation idempotency. [B3]
struct RecoveryBindings {
    operation_id: String,
    logical_request_key: String,
    intent_digest: String,
}

/// Compute exact normalized binding keys. [B3]
fn compute_bindings(
    request: &RecoveryRequest,
    capsule: &ExecutionCapsuleV1,
    source_attempt: Option<&AttemptRow>,
) -> RecoveryBindings {
    let normalized_intent = normalize_operator_verb(request.operator_verb);
    let source_attempt_id = source_attempt.map(|a| a.attempt_id);
    let operation_id = compute_operation_id(
        &request.run_id,
        &request.step_id,
        &capsule.envelope_digest,
        source_attempt_id,
        normalized_intent,
    );
    let logical_request_key =
        compute_logical_request_key(&request.run_id, source_attempt_id, normalized_intent);
    let intent_digest = compute_intent_digest(normalized_intent);
    RecoveryBindings {
        operation_id,
        logical_request_key,
        intent_digest,
    }
}

/// Resolve the step definition from the capsule via the adapter. [C8]
fn resolve_step_def(
    capsule: &ExecutionCapsuleV1,
    step_id: &str,
) -> Result<crate::workflow::schema::StepDef, RecoveryError> {
    let adapter = adapter_for(capsule).map_err(map_adapter())?;
    adapter
        .step_def_for(capsule, step_id)
        .map_err(map_adapter())
}

/// Anchor workspace ownership for the strategy. [C4/B6]
///
/// `ContinueWorkspace` requires an `Owned` verdict (exact descriptor match).
/// Other strategies tolerate `NoEvidence` (no workspace is needed for re-enter
/// or reconcile) but still fail closed on `Rejected`. [B6]
fn anchor_workspace_ownership(
    workspace: &Path,
    run_id: &str,
    strategy: &RecoveryStrategy,
) -> Result<(Option<VerifiedWorkspace>, Option<WorkspaceAuthorization>), RecoveryError> {
    match adjudicate_workspace_ownership(workspace, run_id) {
        OwnershipVerdict::Owned(verified) => {
            let auth = verified.authorization();
            Ok((Some(verified), Some(auth)))
        }
        OwnershipVerdict::NoEvidence => match strategy {
            RecoveryStrategy::ContinueWorkspace => Err(RecoveryError::Verification(
                "workspace ownership evidence is missing for ContinueWorkspace".to_string(),
            )),
            _ => Ok((None, None)),
        },
        OwnershipVerdict::Rejected(reason) => Err(RecoveryError::Verification(reason)),
    }
}

/// Load the latest source attempt for a step, if any. [C3]
fn latest_attempt_for_step(
    conn: &Connection,
    run_id: &str,
    step_id: &str,
) -> Result<Option<AttemptRow>, RecoveryError> {
    crate::persistence::attempts::latest_for_step(conn, run_id, step_id)
        .map_err(map_persist("latest_for_step"))
}
