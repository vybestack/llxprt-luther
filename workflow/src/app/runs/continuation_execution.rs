//! Continuation execution helpers shared by operator recovery and daemon
//! resume paths.
//!
//! After the operator CLI migration to `RecoveryProtocolV1`, the operator
//! entrypoints no longer call `commit_and_execute` or the rewind
//! `commit_continuation` path. This module now retains only:
//!
//! - [`run_context_from_metadata`]: build a `RunContext` from persisted
//!   metadata (used by both operator recovery wiring and daemon resume).
//! - The `reconstruct_runner_with_*` family: reconstruct an `EngineRunner`
//!   from persisted metadata (used by daemon resume and tests).
//! - The lease finalization helpers ([`finalize_continuation_lease`] and its
//!   private supporting functions): used by operator recovery wiring and
//!   tests.
//! - [`persist_continuation_failure`], [`write_continuation_result`],
//!   [`continuation_outcome_exit_code`], and
//!   [`report_aggregated_maintenance_errors`]: shared post-run maintenance
//!   and output helpers used by operator recovery wiring and tests.

#[cfg(test)]
use luther_workflow::engine::executor::ExecutorRegistry;
#[cfg(test)]
use luther_workflow::engine::instance::WorkflowInstance;
#[cfg(test)]
use luther_workflow::engine::runner::EngineRunner;
use luther_workflow::engine::runner::RunOutcome;
use luther_workflow::persistence::{RunMetadata, SqliteStore};
#[cfg(test)]
use luther_workflow::workflow::config_loader::resolve_workflow_type;
#[cfg(test)]
use luther_workflow::workflow::schema::WorkflowConfig;
#[cfg(test)]
use luther_workflow::workflow::target_profile::{
    apply_target_profile_overrides, target_profile_validation_required, validate_target_profile,
};

use super::inspect::run_log_path;

/// Build a [`RunContext`] from an existing run record so a resumed runner keeps
/// the original issue/PR identity and paths instead of re-deriving them.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn run_context_from_metadata(
    md: &RunMetadata,
    run_id: &str,
) -> luther_workflow::engine::RunContext {
    luther_workflow::engine::RunContext {
        daemon_managed: false,
        log_path: md
            .log_path
            .clone()
            .or_else(|| Some(run_log_path(run_id).to_string_lossy().to_string())),
        artifact_root: md.artifact_root.clone(),
        workspace_path: md.workspace_path.clone(),
        repository: md.repository.clone(),
        issue_number: md.issue_number,
        pr_number: md.pr_number,
        head_sha: md.head_sha.clone(),
        workspace_authorization: None,
        // Resume paths do not set launch_provenance; the engine preserves the
        // existing persisted value because persist_initial_run is a no-op for
        // existing rows. @plan:PLAN-20260722-ISSUE158-LAUNCH-PROVENANCE
        launch_provenance: None,
    }
}

#[cfg(test)]
fn reconstruct_runner_with_config_and_provenance(
    md: &RunMetadata,
    run_id: &str,
    db_path: &std::path::Path,
    config_dir: &Option<std::path::PathBuf>,
    mut config: WorkflowConfig,
    daemon_managed: bool,
) -> Result<EngineRunner, String> {
    let config_root = config_dir
        .as_deref()
        .unwrap_or(std::path::Path::new("config"));
    let overrides = luther_workflow::engine::continuation::continuation_overrides(md);
    apply_target_profile_overrides(&mut config, &overrides)
        .map_err(|error| format!("apply continuation overrides: {error}"))?;
    let workflow_type = resolve_workflow_type(&md.workflow_type_id, config_root)
        .map_err(|error| format!("resolve workflow type '{}': {error}", md.workflow_type_id))?;
    if target_profile_validation_required(&workflow_type.workflow_type_id, &config, &overrides) {
        validate_target_profile(&config)
            .map_err(|error| format!("invalid continuation profile: {error}"))?;
    }
    let mut run_context = run_context_from_metadata(md, run_id);
    run_context.daemon_managed = daemon_managed;
    let mut instance = WorkflowInstance::create_with_run_id(workflow_type, config, run_id);
    if let Some(step) = md.current_step.as_deref().filter(|step| !step.is_empty()) {
        if !instance
            .workflow_type
            .steps
            .iter()
            .any(|definition| definition.step_id == step)
        {
            return Err(format!(
                "current_step '{step}' is not present in workflow type '{}'",
                instance.workflow_type.workflow_type_id
            ));
        }
        instance.transition_to(step);
    }
    EngineRunner::with_db_path_and_context(
        instance,
        ExecutorRegistry::with_defaults(),
        db_path,
        run_context,
    )
    .map_err(|error| format!("create runner: {error}"))
}

/// Reconstruct a durable runner for an existing run from its persisted metadata
/// (test-only convenience that resolves config from persisted provenance or
/// the supplied config directory).
///
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
#[cfg(test)]
pub fn reconstruct_runner(
    md: &RunMetadata,
    run_id: &str,
    db_path: &std::path::Path,
    config_dir: &Option<std::path::PathBuf>,
) -> Result<EngineRunner, String> {
    let persisted_root = match md.launch_provenance.as_ref() {
        Some(provenance) => Some(
            luther_workflow::persistence::decode_config_root(&provenance.canonical_config_root)
                .map_err(|error| format!("decode persisted config root: {error}"))?,
        ),
        None => None,
    };
    let config_root = persisted_root
        .as_deref()
        .or(config_dir.as_deref())
        .unwrap_or(std::path::Path::new("config"));
    let config = luther_workflow::workflow::config_loader::resolve_workflow_config(
        &md.config_id,
        config_root,
    )
    .map_err(|e| format!("resolve config '{}': {e}", md.config_id))?;
    reconstruct_runner_with_config_and_provenance(md, run_id, db_path, config_dir, config, false)
}

fn continuation_lease(
    store: &SqliteStore,
    metadata: &RunMetadata,
) -> Result<Option<luther_workflow::persistence::IssueLease>, rusqlite::Error> {
    let Some(repository) = metadata.repository.as_deref() else {
        return Ok(None);
    };
    // Issue 158 slice 5: lease authority requires the immutable issue number,
    // never a PR number. A PR-only run has no issue lease to resolve.
    let Some(issue_number) = metadata.issue_lease_number() else {
        return Ok(None);
    };
    luther_workflow::persistence::get_lease_for_issue(store.conn(), repository, issue_number)
}

pub(super) fn finalize_continuation_lease(
    store: &SqliteStore,
    metadata: &RunMetadata,
    run_id: &str,
    outcome: &Result<RunOutcome, luther_workflow::engine::runner::EngineError>,
) -> Result<(), String> {
    let Some(lease) = resolve_continuation_lease(store, metadata, run_id)? else {
        // No lease to finalize for this run (and no missing-lease error).
        return Ok(());
    };
    // Ownership guard: only the run that owns the lease may finalize it.
    verify_lease_ownership(&lease, run_id)?;
    let status = lease_status_for_outcome(store, run_id, outcome)?;
    commit_or_verify_finalization(store, metadata, &lease, run_id, status)
}

/// Apply the conditional lease transition, or — when it does not apply — re-read
/// the fresh current lease and validate exact owner + status for idempotent
/// success. Any ownership or status drift is fail-closed with diagnostics
/// rather than silently accepted.
fn commit_or_verify_finalization(
    store: &SqliteStore,
    metadata: &RunMetadata,
    lease: &luther_workflow::persistence::IssueLease,
    run_id: &str,
    status: luther_workflow::persistence::LeaseStatus,
) -> Result<(), String> {
    let expected_statuses = expected_statuses_for(status);
    if apply_lease_transition(store, lease, status, &expected_statuses, run_id)? {
        return Ok(());
    }
    verify_idempotent_finalization(store, metadata, lease, run_id, status)
}

/// Resolve the continuation lease for `metadata`, failing closed with a
/// diagnostic when the run has issue identity but no lease row is found, and
/// returning `Ok(None)` when the run has no issue identity to lease against.
fn resolve_continuation_lease(
    store: &SqliteStore,
    metadata: &RunMetadata,
    run_id: &str,
) -> Result<Option<luther_workflow::persistence::IssueLease>, String> {
    let Some(lease) = continuation_lease(store, metadata).map_err(|error| error.to_string())?
    else {
        return if metadata_has_issue_identity(metadata) {
            Err(format!("missing issue lease for continuation run {run_id}"))
        } else {
            Ok(None)
        };
    };
    Ok(Some(lease))
}

/// Whether `metadata` carries enough identity (repository + issue number) to
/// be expected to hold an issue lease.
///
/// Issue 158 slice 5: a PR number is **not** a lease anchor. A run that
/// recorded only a `pr_number` has no issue lease, so it is not expected to
/// hold one and a missing lease for it is not a hard error.
fn metadata_has_issue_identity(metadata: &RunMetadata) -> bool {
    metadata.repository.is_some() && metadata.issue_lease_number().is_some()
}

/// Fail closed unless the lease is owned by `run_id`.
fn verify_lease_ownership(
    lease: &luther_workflow::persistence::IssueLease,
    run_id: &str,
) -> Result<(), String> {
    if lease.run_id.as_deref() == Some(run_id) {
        Ok(())
    } else {
        Err(format!(
            "lease {} belongs to {:?}, not continuation run {}",
            lease.lease_id, lease.run_id, run_id
        ))
    }
}

/// Map the continuation outcome to the durable lease status.
///
/// `Abandoned` and `Failure` outcomes consult the persisted run metadata to
/// distinguish a cleanup-abandonment terminal from a plain one.
fn lease_status_for_outcome(
    store: &SqliteStore,
    run_id: &str,
    outcome: &Result<RunOutcome, luther_workflow::engine::runner::EngineError>,
) -> Result<luther_workflow::persistence::LeaseStatus, String> {
    use luther_workflow::persistence::LeaseStatus;
    let status = match outcome {
        Ok(RunOutcome::Success) => LeaseStatus::Completed,
        Ok(RunOutcome::WaitingExternal { .. }) => LeaseStatus::WaitingExternal,
        // An interrupted run is resumable, not failed. Mapping it to
        // ReadyToResume keeps the lease in a reclaimable state so a later
        // continuation can resume it, rather than forcing a full failure
        // recovery path. @plan:PLAN-20260623-LUTHER-CONTINUATION
        Ok(RunOutcome::Interrupted { .. }) => LeaseStatus::ReadyToResume,
        Ok(RunOutcome::Abandoned { .. }) => lease_status_for_abandoned(store, run_id)?,
        Ok(RunOutcome::Failure { .. }) => lease_status_for_failure(store, run_id)?,
        Err(_) => LeaseStatus::Failed,
    };
    Ok(status)
}

/// Distinguish a cleanup-after-failure abandonment (`CleanupAbandoned`) from a
/// plain abandonment (`Abandoned`) by inspecting durable run provenance.
fn lease_status_for_abandoned(
    store: &SqliteStore,
    run_id: &str,
) -> Result<luther_workflow::persistence::LeaseStatus, String> {
    use luther_workflow::persistence::LeaseStatus;
    let current = load_continued_run(store, run_id)?;
    Ok(if current.is_cleanup_failure_abandonment() {
        LeaseStatus::CleanupAbandoned
    } else {
        LeaseStatus::Abandoned
    })
}

/// A failure outcome may have triggered failure-cleanup. If the durable run
/// metadata records an incomplete cleanup-failure abandonment (cleanup not yet
/// succeeded), preserve the failed-run identity as `CleanupAbandoned` rather
/// than plain `Failed`. This prevents a duplicate relaunch from clobbering
/// pending recovery state. When cleanup has already succeeded (or there is no
/// `failure_cleanup` provenance), plain `Failed` is correct.
fn lease_status_for_failure(
    store: &SqliteStore,
    run_id: &str,
) -> Result<luther_workflow::persistence::LeaseStatus, String> {
    use luther_workflow::persistence::LeaseStatus;
    let current = luther_workflow::persistence::get_run_with_conn(store.conn(), run_id)
        .map_err(|err| format!("load continued run {run_id} after failure: {err}"))?
        .ok_or_else(|| format!("continued run {run_id} disappeared after failure"))?;
    let has_incomplete_cleanup = current
        .failure_cleanup
        .as_ref()
        .is_some_and(|state| !state.cleanup_succeeded);
    Ok(if has_incomplete_cleanup {
        LeaseStatus::CleanupAbandoned
    } else {
        LeaseStatus::Failed
    })
}

/// Load the durable run metadata for a continued run, failing closed when the
/// record is missing.
fn load_continued_run(store: &SqliteStore, run_id: &str) -> Result<RunMetadata, String> {
    luther_workflow::persistence::get_run_with_conn(store.conn(), run_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("missing continued run metadata for {run_id}"))
}

/// Build the set of acceptable pre-transition lease statuses for a conditional
/// update toward `status`.
///
/// `Running` is always acceptable. When the runner has already atomically
/// protected the lease as `CleanupAbandoned` (via
/// `protect_failure_cleanup_lease`), including `CleanupAbandoned` makes the
/// update idempotent, mirroring the runner's own transition guard. An
/// interrupted run may already have the lease in `ReadyToResume` from a prior
/// continuation commit, so that state is accepted too.
fn expected_statuses_for(
    status: luther_workflow::persistence::LeaseStatus,
) -> Vec<luther_workflow::persistence::LeaseStatus> {
    use luther_workflow::persistence::LeaseStatus;
    let mut expected = vec![LeaseStatus::Running];
    if status == LeaseStatus::CleanupAbandoned {
        expected.push(LeaseStatus::CleanupAbandoned);
    }
    if status == LeaseStatus::ReadyToResume {
        expected.push(LeaseStatus::ReadyToResume);
    }
    expected
}

/// Apply a guarded conditional lease transition. Returns `Ok(true)` when the
/// row was updated and `Ok(false)` when the precondition did not hold.
fn apply_lease_transition(
    store: &SqliteStore,
    lease: &luther_workflow::persistence::IssueLease,
    status: luther_workflow::persistence::LeaseStatus,
    expected_statuses: &[luther_workflow::persistence::LeaseStatus],
    run_id: &str,
) -> Result<bool, String> {
    luther_workflow::persistence::update_lease_status_conditional(
        store.conn(),
        &lease.lease_id,
        status,
        expected_statuses,
        None,
        Some(run_id),
    )
    .map_err(|error| error.to_string())
}

/// Re-read the fresh current lease after a rejected conditional update and
/// validate exact owner + status for idempotent success. Any ownership or
/// status drift is fail-closed with diagnostics rather than silently accepted.
fn verify_idempotent_finalization(
    store: &SqliteStore,
    metadata: &RunMetadata,
    lease: &luther_workflow::persistence::IssueLease,
    run_id: &str,
    status: luther_workflow::persistence::LeaseStatus,
) -> Result<(), String> {
    let current = continuation_lease(store, metadata)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("lease {} vanished during finalization", lease.lease_id))?;
    if lease_already_finalized(&current, lease, run_id, status) {
        return Ok(());
    }
    Err(format!(
        "lease {} was not finalized for continuation run {} \
         (current status: {}, owner: {:?}, expected status: {})",
        lease.lease_id, run_id, current.status, current.run_id, status
    ))
}

/// Whether the freshly re-read `current` lease already matches the original
/// lease identity, owner, and target status — i.e. the finalization is
/// idempotent and already in effect.
fn lease_already_finalized(
    current: &luther_workflow::persistence::IssueLease,
    lease: &luther_workflow::persistence::IssueLease,
    run_id: &str,
    status: luther_workflow::persistence::LeaseStatus,
) -> bool {
    current.lease_id == lease.lease_id
        && current.run_id.as_deref() == Some(run_id)
        && current.status == status
}

/// Persist the failed-state metadata for a continuation run that errored.
///
/// Returns `Err(diagnostic)` when the run metadata cannot be loaded, is
/// missing, or cannot be persisted back. Returning an error (rather than
/// exiting) lets the caller attempt the remaining maintenance actions —
/// result artifact writing and lease finalization — so a persistence failure
/// cannot leave the continuation lease stuck in `Running` or suppress the
/// result artifact. @plan:PLAN-20260623-LUTHER-CONTINUATION
pub(super) fn persist_continuation_failure(
    store: &SqliteStore,
    run_id: &str,
    error: &impl std::fmt::Display,
) -> Result<(), String> {
    eprintln!(
        "Run '{}' stopped after continuation error without rolling back durable progress: {error}",
        run_id
    );
    let mut current = luther_workflow::persistence::get_run_with_conn(store.conn(), run_id)
        .map_err(|persist_error| {
            format!("failed to load continuation failure state for '{run_id}': {persist_error}")
        })?
        .ok_or_else(|| {
            format!("missing run metadata while persisting continuation failure for '{run_id}'")
        })?;
    current.mark_failed();
    luther_workflow::persistence::persist_run_with_conn(store.conn(), &current).map_err(
        |persist_error| {
            format!("failed to persist continuation failure for '{run_id}': {persist_error}")
        },
    )
}

/// Report aggregated post-run maintenance errors (failed-state persistence,
/// lease finalization) to stderr. Each error is printed distinctly so the
/// operator can diagnose every failure even when multiple actions failed.
/// This never exits: the continuation outcome is reported afterwards so the
/// process exit code reflects the run result rather than the first
/// maintenance failure. @plan:PLAN-20260623-LUTHER-CONTINUATION
pub(super) fn report_aggregated_maintenance_errors(run_id: &str, errors: &[String]) {
    if errors.is_empty() {
        return;
    }
    eprintln!(
        "Error: {count} post-run maintenance failure(s) for continuation run '{run_id}':",
        count = errors.len()
    );
    for error in errors {
        eprintln!("  - {error}");
    }
}

/// Write the `resume-result.json` / `retry-result.json` artifact.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn write_continuation_result(
    artifact_dir: &std::path::Path,
    kind: &luther_workflow::engine::ContinuationKind,
    step: &str,
    outcome: &Result<RunOutcome, luther_workflow::engine::runner::EngineError>,
) {
    let status_label = match outcome {
        Ok(RunOutcome::Success) => "completed",
        Ok(RunOutcome::WaitingExternal { .. }) => "waiting_external",
        Ok(RunOutcome::Interrupted { .. }) => "interrupted",
        Ok(RunOutcome::Abandoned { .. }) => "abandoned",
        Ok(RunOutcome::Failure { .. }) => "failed",
        Err(_) => "error",
    };
    let value =
        luther_workflow::engine::continuation::result_artifact(kind, status_label, step, None);
    let name = luther_workflow::engine::continuation::result_artifact_name(kind);
    let _ = luther_workflow::engine::continuation::write_json_artifact(artifact_dir, name, &value);
}

/// Print the human summary for a continuation outcome and return its exit code
/// without exiting. Used by the operator recovery wiring to derive the exit
/// code after aggregating maintenance failures.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub(super) fn continuation_outcome_exit_code(
    run_id: &str,
    step: &str,
    outcome: &Result<RunOutcome, luther_workflow::engine::runner::EngineError>,
) -> i32 {
    match outcome {
        Ok(RunOutcome::Success) => {
            println!("Run '{run_id}' completed after continuation.");
            0
        }
        Ok(RunOutcome::WaitingExternal { step_id, reason }) => {
            println!("Run '{run_id}' is waiting at '{step_id}': {reason}");
            println!("Resume with: luther-workflow runs resume {run_id}");
            0
        }
        Ok(RunOutcome::Interrupted { step_id }) => {
            println!("Run '{run_id}' interrupted at '{step_id}' (can be resumed).");
            130
        }
        Ok(RunOutcome::Abandoned { step_id, reason }) => {
            eprintln!("Run '{run_id}' abandoned at '{step_id}': {reason}");
            1
        }
        Ok(RunOutcome::Failure { step_id, reason }) => {
            eprintln!("Run '{run_id}' failed at '{step_id}': {reason}");
            1
        }
        Err(e) => {
            eprintln!("Run '{run_id}' continuation from '{step}' errored: {e}");
            1
        }
    }
}
