use super::monitor::monitor_state_token;
use super::status::{
    install_interrupt_handlers, next_step_label, pid_liveness_label, run_metadata_to_json,
};
use luther_workflow::engine::executor::ExecutorRegistry;
use luther_workflow::engine::instance::WorkflowInstance;
use luther_workflow::engine::runner::{EngineRunner, RunOutcome};
use luther_workflow::monitor::heartbeat::{read_all_heartbeats, MonitorState};
use luther_workflow::persistence::{
    list_artifacts, load_events, RunMetadata, RunStatus, SqliteStore,
};
use luther_workflow::workflow::config_loader::{resolve_workflow_config, resolve_workflow_type};
use luther_workflow::workflow::schema::WorkflowConfig;
use luther_workflow::workflow::target_profile::{
    apply_target_profile_overrides, target_profile_validation_required, validate_target_profile,
};
use std::path::Path;
use std::process;

/// Open the persistent run registry store at the shared checkpoints.db.
///
/// Returns `Ok(None)` when the database file does not exist yet (treated as an
/// empty registry), `Ok(Some(store))` when opened, and `Err` when the file is
/// present but cannot be opened (surfaced distinctly from "no runs").
/// @plan:issue-51
pub fn open_runs_store() -> Result<Option<SqliteStore>, String> {
    let db_path = luther_workflow::runtime_paths::get_data_dir().join("checkpoints.db");
    if !db_path.exists() {
        return Ok(None);
    }
    SqliteStore::open(&db_path)
        .map(Some)
        .map_err(|e| format!("failed to open run registry at {}: {e}", db_path.display()))
}

/// Dispatch the `runs` command family (issue #51).
/// @plan:issue-51
pub async fn handle_runs_command(args: &luther_workflow::cli::RunsArgs) {
    use luther_workflow::cli::RunsCommand;
    match &args.command {
        RunsCommand::List(list_args) => handle_runs_list(list_args),
        RunsCommand::Show(show_args) => handle_runs_show(show_args),
        RunsCommand::Tail(tail_args) => handle_runs_tail(tail_args).await,
        RunsCommand::Ps(ps_args) => handle_runs_ps(ps_args).await,
        RunsCommand::Checkpoints(cp_args) => handle_runs_checkpoints(cp_args),
        RunsCommand::Resume(resume_args) => handle_runs_resume(resume_args),
        RunsCommand::Retry(retry_args) => handle_runs_retry(retry_args),
        RunsCommand::Rewind(rewind_args) => handle_runs_rewind(rewind_args),
    }
}

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
    }
}

/// Reconstruct a durable runner for an existing run from its persisted metadata.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
#[cfg(test)]
pub fn reconstruct_runner(
    md: &RunMetadata,
    run_id: &str,
    db_path: &std::path::Path,
    config_dir: &Option<std::path::PathBuf>,
) -> Result<EngineRunner, String> {
    let config_root = config_dir
        .as_deref()
        .unwrap_or(std::path::Path::new("config"));
    let config = resolve_workflow_config(&md.config_id, config_root)
        .map_err(|e| format!("resolve config '{}': {e}", md.config_id))?;
    reconstruct_runner_with_config(md, run_id, db_path, config_dir, config)
}

pub(super) fn reconstruct_runner_with_daemon_provenance(
    md: &RunMetadata,
    run_id: &str,
    db_path: &std::path::Path,
    config_dir: &Option<std::path::PathBuf>,
    config: WorkflowConfig,
    daemon_managed: bool,
) -> Result<EngineRunner, String> {
    reconstruct_runner_with_config_and_provenance(
        md,
        run_id,
        db_path,
        config_dir,
        config,
        daemon_managed,
    )
}

pub fn reconstruct_runner_with_config(
    md: &RunMetadata,
    run_id: &str,
    db_path: &std::path::Path,
    config_dir: &Option<std::path::PathBuf>,
    config: WorkflowConfig,
) -> Result<EngineRunner, String> {
    reconstruct_runner_with_config_and_provenance(md, run_id, db_path, config_dir, config, false)
}

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
    // Re-apply the original run's effective runtime overrides so the resumed
    // interpolation context (target_repo, issue_number, work_dir, artifact_dir)
    // matches the original target/workspace/artifacts rather than static config
    // defaults. @plan:PLAN-20260623-LUTHER-CONTINUATION
    let overrides = luther_workflow::engine::continuation::continuation_overrides(md);
    apply_target_profile_overrides(&mut config, &overrides)
        .map_err(|e| format!("apply continuation overrides: {e}"))?;
    let workflow_type = resolve_workflow_type(&md.workflow_type_id, config_root)
        .map_err(|e| format!("resolve workflow type '{}': {e}", md.workflow_type_id))?;
    // Fail fast with diagnostics rather than resuming against an invalid profile,
    // but only when the workflow actually uses a target profile (mirrors the
    // initial-run gate). @plan:PLAN-20260623-LUTHER-CONTINUATION
    if target_profile_validation_required(&workflow_type.workflow_type_id, &config, &overrides) {
        validate_target_profile(&config)
            .map_err(|e| format!("invalid continuation profile: {e}"))?;
    }
    let mut run_context = run_context_from_metadata(md, run_id);
    run_context.daemon_managed = daemon_managed;
    let mut instance = WorkflowInstance::create_with_run_id(workflow_type, config, run_id);
    if let Some(step) = md.current_step.as_deref().filter(|step| !step.is_empty()) {
        if !instance
            .workflow_type
            .steps
            .iter()
            .any(|def| def.step_id == step)
        {
            return Err(format!(
                "run '{run_id}' current_step '{step}' is not present in workflow type '{}'",
                md.workflow_type_id
            ));
        }
        instance.transition_to(step);
    }
    let registry = ExecutorRegistry::with_defaults();
    EngineRunner::with_db_path_and_context(instance, registry, db_path, run_context)
        .map_err(|e| format!("create runner: {e}"))
}

/// Validate + plan a continuation, writing request/validation artifacts and
/// exiting non-zero with diagnostics when validation fails.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn plan_continuation_or_exit(
    store: &SqliteStore,
    md: &RunMetadata,
    request: &luther_workflow::engine::ContinuationRequest,
) -> luther_workflow::engine::continuation::ContinuationPlan {
    let plan = match luther_workflow::engine::prepare_continuation(store.conn(), request, md) {
        Ok(plan) => plan,
        Err(e) => {
            eprintln!("Error: continuation failed: {e}");
            process::exit(1);
        }
    };
    if !plan.validation.ok {
        eprintln!("Refusing to {}: unsafe continuation", request.kind.verb());
        for reason in plan.validation.failure_reasons() {
            eprintln!("  - {reason}");
        }
        eprintln!(
            "Validation artifact written under: {}",
            plan.artifact_dir.display()
        );
        process::exit(1);
    }
    plan
}

fn continuation_lease(
    store: &SqliteStore,
    metadata: &RunMetadata,
) -> Result<Option<luther_workflow::persistence::IssueLease>, rusqlite::Error> {
    let Some(repository) = metadata.repository.as_deref() else {
        return Ok(None);
    };
    let Some(issue_number) = metadata
        .issue_number
        .or(metadata.pr_number)
        .and_then(|number| u64::try_from(number).ok())
    else {
        return Ok(None);
    };
    luther_workflow::persistence::get_lease_for_issue(store.conn(), repository, issue_number)
}

fn finalize_continuation_lease(
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

/// Whether `metadata` carries enough identity (repository + issue/PR number) to
/// be expected to hold an issue lease.
fn metadata_has_issue_identity(metadata: &RunMetadata) -> bool {
    metadata.repository.is_some() && metadata.issue_number.or(metadata.pr_number).is_some()
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

/// Commit a planned continuation (re-stamp resume point + reopen run) and
/// execute the reconstructed runner, writing the result artifact.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn commit_and_execute(
    store: &SqliteStore,
    md: &RunMetadata,
    request: &luther_workflow::engine::ContinuationRequest,
    plan: &luther_workflow::engine::continuation::ContinuationPlan,
    config_dir: &Option<std::path::PathBuf>,
) {
    let continuation_lease = continuation_lease(store, md).unwrap_or_else(|error| {
        eprintln!("Error: failed to inspect continuation lease: {error}");
        process::exit(1);
    });
    let continuation_had_lease = continuation_lease.is_some();
    let daemon_managed = md.issue_number.is_some()
        && continuation_lease
            .as_ref()
            .is_some_and(|lease| lease.run_id.as_deref() == Some(request.run_id.as_str()));
    let step = plan
        .selected
        .as_ref()
        .map(|c| c.step_id.clone())
        .unwrap_or_default();
    // Reconstruct the runner first: it applies and validates the continuation /
    // target-profile overrides (continuation_overrides, apply_target_profile_overrides,
    // resolve_workflow_type, and validate_target_profile when required). Running
    // this before commit_continuation ensures a profile/continuation failure
    // cannot mutate run state and leave a refused continuation reopened and stuck
    // in 'Running'. @plan:PLAN-20260623-LUTHER-CONTINUATION
    let db_path = store
        .db_path()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| luther_workflow::runtime_paths::get_data_dir().join("checkpoints.db"));
    let mut runner =
        reconstruct_runner_or_exit(md, &request.run_id, &db_path, config_dir, daemon_managed);
    commit_continuation_or_exit(store, request, &plan.checkpoint_identity);
    let mut maintenance_errors: Vec<String> = Vec::new();
    if let Err(error) = write_committed_checkpoint_artifacts(store, request, plan, &step) {
        maintenance_errors.push(error);
    }
    println!(
        "Reopened run '{}' at step '{step}' (continuation: {})",
        request.run_id,
        request.kind.verb()
    );
    install_interrupt_handlers(runner.interrupt_handle());
    let outcome = runner.run();
    // Post-run maintenance is performed independently so a failure in one
    // action cannot skip or suppress the others. Concretely, a persistence
    // failure while recording the failed-state must not leave the continuation
    // lease stuck in `Running` nor suppress the result artifact. Maintenance
    // errors are aggregated and reported after all actions are attempted.
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    if let Err(ref error) = outcome {
        if let Err(maintenance_error) = persist_continuation_failure(store, &request.run_id, error)
        {
            maintenance_errors.push(maintenance_error);
        }
    }
    write_continuation_result(&plan.artifact_dir, &request.kind, &step, &outcome);
    if continuation_had_lease {
        if let Err(error) = finalize_continuation_lease(store, md, &request.run_id, &outcome) {
            maintenance_errors.push(format!("failed to finalize continuation lease: {error}"));
        }
    }
    report_aggregated_maintenance_errors(&request.run_id, &maintenance_errors);
    report_continuation_outcome(
        &request.run_id,
        &step,
        outcome,
        !maintenance_errors.is_empty(),
    );
}

fn reconstruct_runner_or_exit(
    md: &RunMetadata,
    run_id: &str,
    db_path: &Path,
    config_dir: &Option<std::path::PathBuf>,
    daemon_managed: bool,
) -> EngineRunner {
    let config_root = config_dir.as_deref().unwrap_or(Path::new("config"));
    let reconstruction = resolve_workflow_config(&md.config_id, config_root)
        .map_err(|error| format!("resolve config '{}': {error}", md.config_id))
        .and_then(|config| {
            if daemon_managed {
                reconstruct_runner_with_daemon_provenance(
                    md, run_id, db_path, config_dir, config, true,
                )
            } else {
                reconstruct_runner_with_config(md, run_id, db_path, config_dir, config)
            }
        });
    match reconstruction {
        Ok(runner) => runner,
        Err(e) => {
            eprintln!("Error: {e}");
            process::exit(1);
        }
    }
}

fn commit_continuation_or_exit(
    store: &SqliteStore,
    request: &luther_workflow::engine::ContinuationRequest,
    checkpoint_identity: &str,
) {
    if let Err(e) =
        luther_workflow::engine::commit_continuation(store.conn(), request, checkpoint_identity)
    {
        eprintln!("Error: failed to reopen run '{}': {e}", request.run_id);
        process::exit(1);
    }
}
fn write_committed_checkpoint_artifacts(
    store: &SqliteStore,
    request: &luther_workflow::engine::ContinuationRequest,
    plan: &luther_workflow::engine::continuation::ContinuationPlan,
    step_id: &str,
) -> Result<(), String> {
    let current = luther_workflow::persistence::get_checkpoint_for_step(
        store.conn(),
        &request.run_id,
        step_id,
    )
    .map_err(|error| {
        format!(
            "failed to load committed continuation checkpoint for run '{}': {error}",
            request.run_id
        )
    })?
    .ok_or_else(|| {
        format!(
            "committed continuation checkpoint for run '{}' step '{}' was not found",
            request.run_id, step_id
        )
    })?;
    let artifact = luther_workflow::engine::continuation::committed_selection_artifact(
        &plan.checkpoint_identity,
        &current,
    );
    let name = "checkpoint-commit.json";
    luther_workflow::engine::continuation::write_json_artifact(&plan.artifact_dir, name, &artifact)
        .map(|_| ())
        .map_err(|error| {
            format!(
                "failed to write committed continuation artifact '{}': {error}",
                plan.artifact_dir.join(name).display()
            )
        })
}

/// Persist the failed-state metadata for a continuation run that errored.
///
/// Returns `Err(diagnostic)` when the run metadata cannot be loaded, is
/// missing, or cannot be persisted back. Returning an error (rather than
/// exiting) lets the caller attempt the remaining maintenance actions —
/// result artifact writing and lease finalization — so a persistence failure
/// cannot leave the continuation lease stuck in `Running` or suppress the
/// result artifact. @plan:PLAN-20260623-LUTHER-CONTINUATION
fn persist_continuation_failure(
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
fn report_aggregated_maintenance_errors(run_id: &str, errors: &[String]) {
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

/// Print a human summary of a continuation run and exit with its code.
///
/// When `maintenance_failed` is true, the exit code is escalated to non-zero
/// so that a post-run maintenance failure (e.g. lease finalization) is never
/// silently masked by a successful outcome code. The outcome summary is still
/// printed so outcome reporting remains independent of maintenance status.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn report_continuation_outcome(
    run_id: &str,
    step: &str,
    outcome: Result<RunOutcome, luther_workflow::engine::runner::EngineError>,
    maintenance_failed: bool,
) {
    let code = continuation_outcome_exit_code(run_id, step, &outcome);
    process::exit(if maintenance_failed && code == 0 {
        1
    } else {
        code
    });
}

/// Print the human summary for a continuation outcome and return its exit code
/// without exiting. Split from [`report_continuation_outcome`] so callers can
/// escalate the exit code after aggregating maintenance failures.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
fn continuation_outcome_exit_code(
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

/// `runs checkpoints RUN_ID` — list every per-step checkpoint for a run.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn handle_runs_checkpoints(args: &luther_workflow::cli::RunsCheckpointsArgs) {
    let store = require_runs_store(&args.run_id);
    let checkpoints =
        match luther_workflow::persistence::list_checkpoints(store.conn(), &args.run_id) {
            Ok(cps) => cps,
            Err(e) => {
                eprintln!(
                    "Error: failed to list checkpoints for '{}': {e}",
                    args.run_id
                );
                process::exit(1);
            }
        };
    if args.json {
        print_checkpoints_json(&args.run_id, &checkpoints);
    } else {
        print_checkpoints_human(&args.run_id, &checkpoints);
    }
}

/// Render checkpoints as JSON.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn print_checkpoints_json(
    run_id: &str,
    checkpoints: &[luther_workflow::persistence::Checkpoint],
) {
    let rows: Vec<serde_json::Value> = checkpoints
        .iter()
        .map(|c| {
            serde_json::json!({
                "step_id": c.step_id,
                "checkpoint_id": luther_workflow::engine::continuation::checkpoint_identity(c),
                "status": c.state_snapshot.status,
                "timestamp": c.timestamp.to_rfc3339(),
                "loop_count": c.state_snapshot.loop_count,
                "retry_count": c.state_snapshot.retry_count,
                "context_keys": c.state_snapshot.context.len(),
            })
        })
        .collect();
    let doc = serde_json::json!({ "run_id": run_id, "checkpoints": rows });
    println!(
        "{}",
        serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".to_string())
    );
}

/// Render checkpoints as a human-readable table.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn print_checkpoints_human(
    run_id: &str,
    checkpoints: &[luther_workflow::persistence::Checkpoint],
) {
    if checkpoints.is_empty() {
        println!("No checkpoints recorded for run '{run_id}'.");
        return;
    }
    println!("Checkpoints for run '{run_id}':");
    println!(
        "  {:<26} {:<16} {:<6} {:<6} TIMESTAMP",
        "STEP", "STATUS", "LOOP", "RETRY"
    );
    for c in checkpoints {
        println!(
            "  {:<26} {:<16} {:<6} {:<6} {}",
            c.step_id,
            c.state_snapshot.status,
            c.state_snapshot.loop_count,
            c.state_snapshot.retry_count,
            c.timestamp.to_rfc3339(),
        );
    }
}

/// `runs resume RUN_ID` — resume from the latest resumable checkpoint.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn handle_runs_resume(args: &luther_workflow::cli::RunsResumeArgs) {
    let store = require_runs_store(&args.run_id);
    let md = load_run_or_exit(&store, &args.run_id);
    let request = luther_workflow::engine::ContinuationRequest {
        run_id: args.run_id.clone(),
        kind: luther_workflow::engine::ContinuationKind::Resume,
        force: args.force,
        trusted_internal: false,
    };
    let plan = plan_continuation_or_exit(&store, &md, &request);
    commit_and_execute(&store, &md, &request, &plan, &args.config_dir);
}

/// `runs retry RUN_ID [--from-failed-step]` — retry an external-wait step.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn handle_runs_retry(args: &luther_workflow::cli::RunsRetryArgs) {
    let store = require_runs_store(&args.run_id);
    let md = load_run_or_exit(&store, &args.run_id);
    let request = luther_workflow::engine::ContinuationRequest {
        run_id: args.run_id.clone(),
        kind: luther_workflow::engine::ContinuationKind::Retry {
            from_failed_step: args.from_failed_step,
        },
        force: args.force,
        trusted_internal: false,
    };
    let plan = plan_continuation_or_exit(&store, &md, &request);
    commit_and_execute(&store, &md, &request, &plan, &args.config_dir);
}

/// `runs rewind RUN_ID (--to-step S | --to-checkpoint ID)` — set the resume
/// point to an earlier checkpoint without immediately re-executing.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn handle_runs_rewind(args: &luther_workflow::cli::RunsRewindArgs) {
    let store = require_runs_store(&args.run_id);
    let md = load_run_or_exit(&store, &args.run_id);
    let target = if let Some(step) = &args.to_step {
        luther_workflow::engine::RewindTarget::ToStep(step.clone())
    } else if let Some(id) = &args.to_checkpoint {
        luther_workflow::engine::RewindTarget::ToCheckpoint(id.clone())
    } else {
        eprintln!("Error: rewind requires --to-step or --to-checkpoint");
        process::exit(1);
    };
    let request = luther_workflow::engine::ContinuationRequest {
        run_id: args.run_id.clone(),
        kind: luther_workflow::engine::ContinuationKind::Rewind { target },
        force: args.force,
        trusted_internal: false,
    };
    let plan = plan_continuation_or_exit(&store, &md, &request);
    let step = plan
        .selected
        .as_ref()
        .map(|c| c.step_id.clone())
        .unwrap_or_default();
    if let Err(e) = luther_workflow::engine::commit_continuation(
        store.conn(),
        &request,
        &plan.checkpoint_identity,
    ) {
        eprintln!("Error: failed to set resume point: {e}");
        process::exit(1);
    }
    if let Err(error) = write_committed_checkpoint_artifacts(&store, &request, &plan, &step) {
        eprintln!("Error: {error}");
        process::exit(1);
    }
    println!(
        "Rewound run '{}' to step '{step}'. Resume with: luther-workflow runs resume {}",
        args.run_id, args.run_id
    );
}

mod inspect;
pub use inspect::*;

#[cfg(test)]
#[path = "runs_tests.rs"]
mod runs_tests;
