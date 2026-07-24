//! Operator CLI recovery wiring: bridges the `runs resume/retry/rewind`
//! commands to [`RecoveryProtocolV1`].
//!
//! This module owns the single production helper that replaces the legacy
//! `commit_and_execute` / rewind `commit_continuation` path. It uses the
//! existing continuation plan for exact selected step/safety/artifacts, then
//! dispatches through the capsule-backed recovery protocol via
//! [`RecoveryWiring::runner_executor`] and
//! [`RecoveryProtocolV1::recover_with_executor`].
//!
//! Salvage-only runs (no valid pre-execution V1 capsule) are routed through
//! [`classify_run`] / [`salvage_recover`] and refused with a salvage lineage
//! record. Lease maintenance and output reporting are preserved.
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14
//! @requirement:REQ-RP-001,REQ-RP-009

use std::path::{Path, PathBuf};

use luther_workflow::engine::recovery::protocol::OperatorVerb;
use luther_workflow::engine::recovery::{
    classify_run, salvage_recover, RecoveryOutcome, RecoveryProtocolV1, RecoveryRequest,
    RecoveryWiring, RefusalReason, RunClassification,
};
use luther_workflow::engine::runner::{EngineError, RunOutcome};
use luther_workflow::engine::{ContinuationKind, ContinuationRequest};
use luther_workflow::persistence::recovery_epoch::read_epoch;
use luther_workflow::persistence::{RunMetadata, RunStatus, SqliteStore};

use super::continuation_execution::{
    continuation_outcome_exit_code, finalize_continuation_lease, persist_continuation_failure,
    report_aggregated_maintenance_errors, run_context_from_metadata, write_continuation_result,
};

/// The typed outcome of an operator recovery run, carrying the run outcome
/// (for lease finalization and output) and whether maintenance failed.
pub(super) struct RecoveryRunResult {
    /// The run outcome inferred from durable state after recovery.
    pub outcome: Result<RunOutcome, EngineError>,
    /// Whether any post-run maintenance action failed.
    pub maintenance_failed: bool,
    /// The exit code for this outcome.
    pub exit_code: i32,
}

/// Map a [`ContinuationKind`] to the recovery protocol's [`OperatorVerb`].
///
/// Resume → [`OperatorVerb::Resume`], Retry → [`OperatorVerb::Retry`],
/// Rewind → [`OperatorVerb::Rewind`].
pub(super) fn map_operator_verb(kind: &ContinuationKind) -> OperatorVerb {
    match kind {
        ContinuationKind::Resume => OperatorVerb::Resume,
        ContinuationKind::Retry { .. } => OperatorVerb::Retry,
        ContinuationKind::Rewind { .. } => OperatorVerb::Rewind,
    }
}

/// Resolve the persisted workspace path for recovery, falling back to the
/// current directory when the metadata has no recorded workspace.
///
/// The recovery protocol's workspace ownership adjudication tolerates
/// `NoEvidence` for non-`ContinueWorkspace` strategies.
fn resolve_workspace_path(md: &RunMetadata) -> PathBuf {
    md.workspace_path
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Infer the [`RunOutcome`] from durable run metadata after recovery execution.
///
/// The capsule-backed executor runs the normal transition loop, which persists
/// the final run status via `record_run_completion`. This re-reads the
/// metadata and maps the status to a [`RunOutcome`] for lease finalization.
fn infer_outcome_from_metadata(
    store: &SqliteStore,
    run_id: &str,
    step: &str,
) -> Result<RunOutcome, EngineError> {
    let md = luther_workflow::persistence::get_run_with_conn(store.conn(), run_id)
        .map_err(|e| EngineError::PersistenceError(format!("load post-recovery metadata: {e}")))?
        .ok_or_else(|| {
            EngineError::PersistenceError(format!("run '{run_id}' disappeared after recovery"))
        })?;
    Ok(status_to_outcome(&md.status, step))
}

/// Map a [`RunStatus`] to a [`RunOutcome`] for the given step.
///
/// Nonterminal statuses that are not already mapped to a specific outcome
/// (`WaitingExternal`, `Paused`, `ReadyToResume`) are treated as interrupted
/// (resumable), never as success. [`RunStatus::ReviewReady`] is nonterminal:
/// a merge-required run that reached `ReviewReady` is NOT `Completed` and must
/// not report success. [B12/C11]
fn status_to_outcome(status: &RunStatus, step: &str) -> RunOutcome {
    match status {
        RunStatus::Completed | RunStatus::Merged => RunOutcome::Success,
        RunStatus::WaitingExternal => RunOutcome::WaitingExternal {
            step_id: step.to_string(),
            reason: "external wait condition".to_string(),
        },
        RunStatus::Paused | RunStatus::ReadyToResume => RunOutcome::Interrupted {
            step_id: step.to_string(),
        },
        RunStatus::Abandoned => RunOutcome::Abandoned {
            step_id: step.to_string(),
            reason: "loop limit reached".to_string(),
        },
        RunStatus::Failed => RunOutcome::Failure {
            step_id: step.to_string(),
            reason: "step failed".to_string(),
        },
        // ReviewReady, Initialized, Queued, Starting, Running,
        // WaitingForChecks, Remediating, Blocked are all nonterminal. They
        // are NOT Completed and must never report success. Treat as
        // interrupted so the operator knows the run did not reach a
        // terminal success.
        _ => RunOutcome::Interrupted {
            step_id: step.to_string(),
        },
    }
}

/// The single production recovery helper for operator CLI runs.
///
/// This Result-returning core (no `process::exit`) replaces the legacy
/// `commit_and_execute` and rewind `commit_continuation` paths. It:
///
/// 1. Plans the continuation via the existing `prepare_continuation` (exact
///    selected step/safety/artifacts).
/// 2. Classifies the run: capsule-backed → recovery protocol; salvage-only →
///    salvage lineage record + refusal.
/// 3. Reads the current epoch and constructs a [`RecoveryRequest`] with the
///    mapped [`OperatorVerb`].
/// 4. Dispatches through [`RecoveryProtocolV1::recover_with_executor`] with a
///    [`RecoveryWiring::runner_executor`] backed by the persisted workspace
///    and [`run_context_from_metadata`].
/// 5. Preserves lease maintenance ([`finalize_continuation_lease`]) and output
///    reporting ([`write_continuation_result`], exit code).
pub(super) fn recover_operator_run(
    store: &SqliteStore,
    md: &RunMetadata,
    request: &ContinuationRequest,
) -> Result<RecoveryRunResult, String> {
    let plan = plan_continuation(store, md, request)?;
    let step = plan
        .selected
        .as_ref()
        .map(|c| c.step_id.clone())
        .unwrap_or_default();
    let db_path = resolve_db_path(store);
    let had_lease = resolve_had_lease(store, md, request);

    let classification = classify_run(store.conn(), &request.run_id)
        .map_err(|e| format!("classify run '{}': {e}", request.run_id))?;

    let recovery_outcome = dispatch_recovery(store, md, request, &plan, &db_path, &classification)?;

    let outcome = map_recovery_to_outcome(store, request, &step, &recovery_outcome);

    write_continuation_result(&plan.artifact_dir, &request.kind, &step, &outcome);

    let maintenance_errors =
        run_post_recovery_maintenance(store, md, request, &outcome, &recovery_outcome, had_lease);

    let exit_code = continuation_outcome_exit_code(&request.run_id, &step, &outcome);

    Ok(RecoveryRunResult {
        outcome,
        maintenance_failed: !maintenance_errors.is_empty(),
        exit_code,
    })
}

/// Plan a continuation without exiting on error (Result-returning core).
fn plan_continuation(
    store: &SqliteStore,
    md: &RunMetadata,
    request: &ContinuationRequest,
) -> Result<luther_workflow::engine::continuation::ContinuationPlan, String> {
    let plan = luther_workflow::engine::prepare_continuation(store.conn(), request, md)
        .map_err(|e| format!("continuation failed: {e}"))?;
    if !plan.validation.ok {
        let reasons = plan.validation.failure_reasons();
        let detail = if reasons.is_empty() {
            String::new()
        } else {
            format!("\n  - {}", reasons.join("\n  - "))
        };
        return Err(format!(
            "Refusing to {}: unsafe continuation{}\nValidation artifact written under: {}",
            request.kind.verb(),
            detail,
            plan.artifact_dir.display()
        ));
    }
    Ok(plan)
}

/// Resolve the database path from the store or the default data directory.
fn resolve_db_path(store: &SqliteStore) -> PathBuf {
    store
        .db_path()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| luther_workflow::runtime_paths::get_data_dir().join("checkpoints.db"))
}

/// Resolve whether the run had a lease owned by this run to finalize.
fn resolve_had_lease(store: &SqliteStore, md: &RunMetadata, request: &ContinuationRequest) -> bool {
    let Some(repository) = md.repository.as_deref() else {
        return false;
    };
    let Some(issue_number) = md.issue_lease_number() else {
        return false;
    };
    luther_workflow::persistence::get_lease_for_issue(store.conn(), repository, issue_number)
        .map(|lease| {
            lease
                .as_ref()
                .is_some_and(|l| l.run_id.as_deref() == Some(request.run_id.as_str()))
        })
        .unwrap_or(false)
}

/// Dispatch recovery based on the run classification.
///
/// Capsule-backed runs go through the recovery protocol; salvage-only runs
/// get a salvage lineage record and a refusal outcome.
fn dispatch_recovery(
    store: &SqliteStore,
    md: &RunMetadata,
    request: &ContinuationRequest,
    plan: &luther_workflow::engine::continuation::ContinuationPlan,
    db_path: &Path,
    classification: &RunClassification,
) -> Result<RecoveryOutcome, String> {
    match classification {
        RunClassification::CapsuleBacked { .. } => {
            dispatch_capsule_recovery(store, md, request, plan, db_path)
        }
        RunClassification::SalvageOnly { run_id } => salvage_recover(store.conn(), run_id)
            .map_err(|e| format!("salvage recover '{run_id}': {e}")),
    }
}

/// Dispatch capsule-backed recovery through the recovery protocol.
fn dispatch_capsule_recovery(
    store: &SqliteStore,
    md: &RunMetadata,
    request: &ContinuationRequest,
    plan: &luther_workflow::engine::continuation::ContinuationPlan,
    db_path: &Path,
) -> Result<RecoveryOutcome, String> {
    let step = plan
        .selected
        .as_ref()
        .map(|c| c.step_id.clone())
        .unwrap_or_else(|| md.current_step.clone().unwrap_or_default());

    let operator_verb = map_operator_verb(&request.kind);

    let expected_epoch = read_epoch(store.conn(), &request.run_id)
        .map_err(|e| format!("read epoch for '{}': {e}", request.run_id))?;

    let workspace = resolve_workspace_path(md);
    let run_context = run_context_from_metadata(md, &request.run_id);

    let recovery_request = RecoveryRequest {
        run_id: request.run_id.clone(),
        step_id: step,
        expected_epoch,
        operator_verb,
    };

    let executor = RecoveryWiring.runner_executor(db_path.to_path_buf(), run_context);
    RecoveryProtocolV1
        .recover_with_executor(store.conn(), &workspace, &recovery_request, &executor)
        .map_err(|e| format!("recovery protocol failed for '{}': {e}", request.run_id))
}

/// Map a [`RecoveryOutcome`] to a [`RunOutcome`] for lease finalization and
/// output.
///
/// For outcomes that reached execution (`Recovered`, `AlreadyApplied`), the
/// actual step outcome is inferred from durable run metadata. For refused,
/// stale-epoch, or conflict outcomes, a synthetic error is produced.
fn map_recovery_to_outcome(
    store: &SqliteStore,
    request: &ContinuationRequest,
    step: &str,
    recovery_outcome: &RecoveryOutcome,
) -> Result<RunOutcome, EngineError> {
    match recovery_outcome {
        RecoveryOutcome::Recovered { .. } | RecoveryOutcome::AlreadyApplied { .. } => {
            infer_outcome_from_metadata(store, &request.run_id, step)
        }
        RecoveryOutcome::Refused { reason } => {
            Err(refusal_to_engine_error(&request.run_id, reason))
        }
        RecoveryOutcome::StaleEpoch {
            persisted,
            expected,
        } => Err(EngineError::PersistenceError(format!(
            "recovery stale epoch for run '{}' (persisted: {persisted}, expected: {expected})",
            request.run_id
        ))),
        RecoveryOutcome::Conflict { detail } => Err(EngineError::PersistenceError(format!(
            "recovery conflict for run '{}': {detail}",
            request.run_id
        ))),
    }
}

/// Map a [`RefusalReason`] to an [`EngineError`] with a diagnostic message.
fn refusal_to_engine_error(run_id: &str, reason: &RefusalReason) -> EngineError {
    let detail = match reason {
        RefusalReason::NonRecoverable => "step is not recoverable".to_string(),
        RefusalReason::VerificationFailed(msg) => format!("verification failed: {msg}"),
        RefusalReason::NotAuthorized => "not authorized".to_string(),
        RefusalReason::SalvageOnly => "salvage-only run: no valid capsule".to_string(),
        RefusalReason::ConflictingOperation => {
            "conflicting recovery operation in progress".to_string()
        }
    };
    EngineError::PersistenceError(format!("recovery refused for run '{run_id}': {detail}"))
}

/// Run post-recovery maintenance: persist failure state and finalize the lease.
///
/// Each action is attempted independently so a failure in one cannot skip or
/// suppress the other. Returns the aggregated maintenance errors.
///
/// `persist_continuation_failure` (which marks the run as `Failed`) is invoked
/// ONLY for actual execution failures — i.e. when the recovery reached
/// execution (`Recovered`/`AlreadyApplied`) and then errored. Protocol-level
/// outcomes (`Refused`, `StaleEpoch`, `Conflict`) are NOT execution failures:
/// the run's durable state was not advanced, so marking it `Failed` would
/// corrupt the resumable state the operator needs for a retry.
fn run_post_recovery_maintenance(
    store: &SqliteStore,
    md: &RunMetadata,
    request: &ContinuationRequest,
    outcome: &Result<RunOutcome, EngineError>,
    recovery_outcome: &RecoveryOutcome,
    had_lease: bool,
) -> Vec<String> {
    let mut errors = Vec::new();
    let reached_execution = matches!(
        recovery_outcome,
        RecoveryOutcome::Recovered { .. } | RecoveryOutcome::AlreadyApplied { .. }
    );
    if reached_execution {
        if let Err(error) = outcome {
            if let Err(maintenance_error) =
                persist_continuation_failure(store, &request.run_id, error)
            {
                errors.push(maintenance_error);
            }
        }
    }
    if had_lease {
        if let Err(error) = finalize_continuation_lease(store, md, &request.run_id, outcome) {
            errors.push(format!("failed to finalize continuation lease: {error}"));
        }
    }
    report_aggregated_maintenance_errors(&request.run_id, &errors);
    errors
}

/// Report the outcome to stdout/stderr and return the final exit code,
/// escalating to non-zero when maintenance failed.
pub(super) fn report_recovery_outcome(run_id: &str, step: &str, result: &RecoveryRunResult) -> i32 {
    report_outcome_message(run_id, step, &result.outcome);
    if result.maintenance_failed && result.exit_code == 0 {
        1
    } else {
        result.exit_code
    }
}

/// Print the human-readable summary for a recovery outcome.
fn report_outcome_message(run_id: &str, step: &str, outcome: &Result<RunOutcome, EngineError>) {
    match outcome {
        Ok(RunOutcome::Success) => {
            println!("Run '{run_id}' completed after recovery.");
        }
        Ok(RunOutcome::WaitingExternal { step_id, reason }) => {
            println!("Run '{run_id}' is waiting at '{step_id}': {reason}");
            println!("Resume with: luther-workflow runs resume {run_id}");
        }
        Ok(RunOutcome::Interrupted { step_id }) => {
            println!("Run '{run_id}' interrupted at '{step_id}' (can be resumed).");
        }
        Ok(RunOutcome::Abandoned { step_id, reason }) => {
            eprintln!("Run '{run_id}' abandoned at '{step_id}': {reason}");
        }
        Ok(RunOutcome::Failure { step_id, reason }) => {
            eprintln!("Run '{run_id}' failed at '{step_id}': {reason}");
        }
        Err(e) => {
            eprintln!("Run '{run_id}' recovery from '{step}' errored: {e}");
        }
    }
}
