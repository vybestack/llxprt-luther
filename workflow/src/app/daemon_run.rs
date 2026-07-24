//! Daemon workflow launch/resume helpers, extracted from `run.rs` for
//! source-size decomposition. All items here are part of the `run` module's
//! public/private API surface; `run.rs` re-exports the externally-visible ones.
use super::*;

/// Production [`WorkflowLauncher`] that builds and executes the durable engine
/// runner for a claimed issue, applying `repo`/`issue` overrides to the config.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P06
pub struct DaemonWorkflowLauncher;

impl DaemonWorkflowLauncher {
    pub fn new(_config_id: String) -> Self {
        Self
    }
}

impl luther_workflow::daemon::launcher::WorkflowLauncher for DaemonWorkflowLauncher {
    fn launch(
        &self,
        request: &luther_workflow::daemon::launcher::LaunchRequest,
    ) -> Result<luther_workflow::daemon::launcher::WorkflowLaunchResult, String> {
        launch_daemon_workflow(&request.config_id, request)
    }

    fn resume(
        &self,
        request: &luther_workflow::daemon::launcher::LaunchRequest,
    ) -> Result<luther_workflow::daemon::launcher::WorkflowLaunchResult, String> {
        resume_daemon_workflow(request)
    }
}

pub fn launch_daemon_workflow(
    config_id: &str,
    request: &luther_workflow::daemon::launcher::LaunchRequest,
) -> Result<luther_workflow::daemon::launcher::WorkflowLaunchResult, String> {
    // Issue 158 finding 5: use the request's config root (flowed from the
    // supervisor's SchedulerTarget) rather than hardcoding "config".
    let config_root = request.config_root.clone();
    let mut config = resolve_workflow_config(config_id, &config_root)
        .map_err(|e| format!("resolve config '{config_id}': {e}"))?;
    let workflow_type_id = request
        .workflow_type_id
        .as_deref()
        .unwrap_or(&config.workflow_type_id);
    let workflow_type = resolve_workflow_type(workflow_type_id, &config_root)
        .map_err(|e| format!("resolve workflow type: {e}"))?;
    let overrides = TargetProfileOverrides {
        repo: Some(request.repo.clone()),
        issue: Some(request.issue_number.to_string()),
        work_dir: request.work_dir.clone(),
        artifact_dir: request.artifact_dir.clone(),
    };
    apply_target_profile_overrides(&mut config, &overrides)
        .map_err(|e| format!("apply overrides: {e}"))?;
    apply_daemon_claim_overrides(&mut config, request);
    let db_path = luther_workflow::runtime_paths::get_data_dir().join("checkpoints.db");
    let wait_config = config.clone();
    // @plan:PLAN-20260722-ISSUE158-LAUNCH-PROVENANCE
    // Record exact launch provenance from the resolved (post-override) workflow
    // type/config + config root before they are moved into the runner.
    let launch_provenance = luther_workflow::persistence::LaunchProvenance::from_resolved(
        &workflow_type,
        &config,
        &config_root,
    )
    .map_err(|error| format!("record launch provenance: {error}"))?;
    // Issue 158 finding 1: propagate the durable-runner construction error
    // instead of process::exit. The daemon launcher maps this to a launch
    // error so the lease finalizer can compensate.
    let mut runner = create_durable_runner_with_provenance(
        workflow_type,
        config,
        &request.run_id,
        &db_path,
        request.daemon_managed_claim,
        launch_provenance,
        &config_root,
    )
    .map_err(|error| format!("create durable runner: {error}"))?;
    ensure_daemon_run_dirs(request)?;
    run_daemon_runner(request, &wait_config, &db_path, &mut runner)
}
fn apply_daemon_claim_overrides(
    config: &mut luther_workflow::workflow::schema::WorkflowConfig,
    request: &luther_workflow::daemon::launcher::LaunchRequest,
) {
    for (key, value) in [
        ("daemon_managed_claim", request.daemon_managed_claim),
        ("claim_assignment_added", request.claim_assignment_added),
        ("claim_label_added", request.claim_label_added),
    ] {
        config.variables.insert(key.to_owned(), value.to_string());
    }
}

pub fn ensure_daemon_run_dirs(
    request: &luther_workflow::daemon::launcher::LaunchRequest,
) -> Result<(), String> {
    // Issue 158 finding 6: authorize/provision the workspace BEFORE creating
    // the artifact directory. The workspace ownership provisioning is the
    // first mutation that establishes the ownership anchor; the artifact
    // directory creation is a later mutation that must only occur after the
    // workspace is owned. Reversing this order would allow an artifact dir
    // to be created on an unowned workspace, violating the first-mutation
    // ordering invariant.
    ensure_daemon_workspace(request.work_dir.as_deref(), &request.run_id)?;
    ensure_daemon_run_dir("artifact", request.artifact_dir.as_deref())
}

fn ensure_daemon_workspace(work_dir: Option<&std::path::Path>, run_id: &str) -> Result<(), String> {
    let Some(work_dir) = work_dir else {
        return Ok(());
    };
    // Provision bootstrap ownership through the cohesive lifecycle API. The
    // graph promotes it after Git initialization, and resume can promote
    // verified existing evidence.
    luther_workflow::engine::workspace_ownership::provision_workspace_ownership(work_dir, run_id)
        .map_err(|e| format!("provision workspace ownership: {e}"))
}

/// Create the non-daemon workspace and provision bootstrap ownership.
///
/// Unlike the daemon path (which provisions ownership through the cohesive
/// lifecycle API after precheck), the non-daemon fresh-launch path must
/// establish the bootstrap ownership marker as part of workspace creation so
/// the workspace is owned before any graph step runs. Typed graph steps then
/// handle ownership verification and durable promotion after Git init.
///
/// The marker provisioning owns the atomic directory creation: it uses
/// `create_dir` (single-component) to detect first-claim vs pre-existing and
/// refuses to adopt an unowned pre-existing directory. Calling
/// `create_dir_all` here would defeat that atomic creation signal.
pub(super) fn ensure_non_daemon_workspace(
    config: &luther_workflow::workflow::schema::WorkflowConfig,
    run_id: &str,
) -> Result<(), String> {
    let workspace_path = config
        .variables
        .get("work_dir")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| luther_workflow::runtime_paths::get_run_dir(run_id));
    // Ensure the parent chain exists so the atomic single-component `create_dir`
    // inside marker provisioning can observe first-claim. Creating the parent
    // is safe: only the final component's creation is the first-claim signal.
    if let Some(parent) = workspace_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create workspace parent {}: {error}",
                parent.display()
            )
        })?;
    }
    luther_workflow::engine::workspace_ownership::provision_workspace_owner_marker(
        &workspace_path,
        run_id,
    )
    .map_err(|error| {
        format!(
            "failed to provision workspace ownership {}: {error}",
            workspace_path.display()
        )
    })
}

pub fn ensure_daemon_run_dir(kind: &str, path: Option<&std::path::Path>) -> Result<(), String> {
    let Some(path) = path else {
        return Ok(());
    };
    std::fs::create_dir_all(path)
        .map_err(|e| format!("failed to create {kind} dir {}: {e}", path.display()))
}

pub fn resume_daemon_workflow(
    request: &luther_workflow::daemon::launcher::LaunchRequest,
) -> Result<luther_workflow::daemon::launcher::WorkflowLaunchResult, String> {
    // P12 call site: capsule-driven resume wiring (designated P14 implementation).
    // Once V1Adapter::build_instance is implemented (P14), this surface should
    // load the persisted capsule, verify the envelope digest, and dispatch
    // through adapter_for → Box<dyn CapsuleAdapter> instead of the ad-hoc
    // type/config resolution below. @plan:PLAN-20260723-SELFHOST-RELIABILITY.P12
    let db_path = luther_workflow::runtime_paths::get_data_dir().join("checkpoints.db");
    let conn = rusqlite::Connection::open(&db_path).map_err(|e| e.to_string())?;
    let metadata = get_run_with_conn(&conn, &request.run_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("missing run metadata for {}", request.run_id))?;
    // Validate ALL identity-bearing request fields against persisted metadata
    // BEFORE any ownership promotion or mutation. The request carries repo,
    // issue_number, and run_id that must match the persisted run identity;
    // reconstructing the workspace path from persisted identity (rather than
    // trusting the request) prevents a hostile request from redirecting
    // ownership verification into a foreign workspace. Only after this typed
    // validation passes may ownership be promoted.
    validate_resume_identity_against_metadata(request, &metadata)?;
    // Use the persisted canonical config root carried by the prepared resume
    // request (decoded from the launch provenance in `prepare_resume_lease`).
    // For fresh daemon launches the request carries `"config"`, matching the
    // launch path. @plan:PLAN-20260722-ISSUE158-LAUNCH-PROVENANCE
    let config_root = request.config_root.clone();
    let (workflow_type, mut wait_config, provenance_config, persisted_overrides) =
        resolve_daemon_resume_workflow(&metadata, &config_root, request)?;
    // Reconstruct the workspace path from the persisted identity (not the
    // request), so ownership verification can never be redirected by a
    // mismatched request path. The request's work_dir (if any) must match
    // exactly.
    let workspace_path = resolve_persisted_resume_workspace(&metadata, request)?;
    // Issue 158 slice 6: perform complete read-only persisted identity +
    // ownership + authorization BEFORE any durable mutation (ownership
    // promotion, lease CAS). The PreparedResume reconstructs the ephemeral
    // WorkspaceAuthorization from the same verified workspace descriptor so a
    // resumed shell step retains descriptor-anchored authorization. On any
    // failure (foreign owner, missing evidence, malformed marker) this leaves
    // lease/markers/run unchanged.
    let prepared = authorize_daemon_resume(&conn, request, workspace_path)?;
    verify_daemon_resume_provenance(
        &metadata.launch_provenance,
        &workflow_type,
        &provenance_config,
        &config_root,
    )?;
    authorize_daemon_resume_continuation(&conn, &request.run_id, &metadata)?;
    execute_daemon_resume(
        &DaemonResumeContext {
            conn: &conn,
            request,
            metadata: &metadata,
            db_path: &db_path,
            workspace_path,
        },
        &mut wait_config,
        &persisted_overrides,
        config_root,
        &prepared,
    )
}

/// Reconstruct the authoritative persisted workspace path for a daemon resume,
/// failing when the metadata carries no workspace or the request's work_dir
/// does not match it exactly.
fn resolve_persisted_resume_workspace<'a>(
    metadata: &'a luther_workflow::persistence::RunMetadata,
    request: &luther_workflow::daemon::launcher::LaunchRequest,
) -> Result<&'a std::path::Path, String> {
    let persisted_workspace = metadata.workspace_path.as_deref().ok_or_else(|| {
        format!(
            "missing workspace_path for resume of run {}",
            request.run_id
        )
    })?;
    resolve_resume_workspace_path(persisted_workspace, request)
}

/// Perform the read-only resume authorization: reject a pending legacy
/// ownership migration, then reconstruct the ephemeral
/// `WorkspaceAuthorization` from the verified workspace descriptor. On any
/// failure (foreign owner, missing evidence, malformed marker) this leaves
/// lease/markers/run unchanged.
fn authorize_daemon_resume(
    conn: &rusqlite::Connection,
    request: &luther_workflow::daemon::launcher::LaunchRequest,
    workspace_path: &std::path::Path,
) -> Result<luther_workflow::engine::continuation::PreparedResume, String> {
    // Issue 158: reject resume while a legacy ownership migration is durably
    // pending. A pending migration row signals an incomplete migration that may
    // have published the marker without recording the completion audit; the
    // resume trust contract requires a durable `completed` migration before the
    // migrated marker is trusted.
    if luther_workflow::persistence::migration_is_pending(conn, &request.run_id) {
        return Err(format!(
            "resume refused for run '{}': a legacy ownership migration is pending \
             (intent recorded but not completed)",
            request.run_id
        ));
    }
    luther_workflow::engine::continuation::prepare_resume_authorization(
        Some(workspace_path.to_str().unwrap_or("")),
        &request.run_id,
    )
    .map_err(|error| format!("resume authorization: {error}"))
}

/// Borrowed bundle of the durable resume identity used by the execution
/// phase. Groups the read-only handles so the execution helper stays under
/// the argument-count limit.
struct DaemonResumeContext<'a> {
    conn: &'a rusqlite::Connection,
    request: &'a luther_workflow::daemon::launcher::LaunchRequest,
    metadata: &'a luther_workflow::persistence::RunMetadata,
    db_path: &'a std::path::Path,
    workspace_path: &'a std::path::Path,
}

#[derive(Debug, serde::Deserialize)]
struct DurableDaemonRunnerResult {
    outcome: String,
    #[serde(default)]
    step_id: String,
    #[serde(default)]
    reason: String,
}
/// Execute an authorized daemon resume exclusively through RecoveryProtocolV1.
fn execute_daemon_resume(
    ctx: &DaemonResumeContext<'_>,
    wait_config: &mut WorkflowConfig,
    persisted_overrides: &TargetProfileOverrides,
    _config_root: std::path::PathBuf,
    prepared: &luther_workflow::engine::continuation::PreparedResume,
) -> Result<luther_workflow::daemon::launcher::WorkflowLaunchResult, String> {
    luther_workflow::engine::workspace_ownership::ensure_durable_workspace_ownership(
        ctx.workspace_path,
        &ctx.request.run_id,
    )
    .map_err(|error| format!("verify and promote workspace ownership: {error}"))?;
    apply_target_profile_overrides(wait_config, persisted_overrides)
        .map_err(|e| format!("apply resume overrides: {e}"))?;

    let step_id = select_daemon_resume_step(ctx)?;
    let expected_epoch =
        luther_workflow::persistence::recovery_epoch::read_epoch(ctx.conn, &ctx.request.run_id)
            .map_err(|error| format!("read recovery epoch: {error}"))?;
    let mut run_context =
        crate::app::runs::run_context_from_metadata(ctx.metadata, &ctx.request.run_id);
    run_context.daemon_managed = ctx.request.daemon_managed_claim;
    run_context.workspace_authorization = Some(prepared.authorization());
    let executor = luther_workflow::engine::recovery::RecoveryWiring
        .runner_executor(ctx.db_path.to_path_buf(), run_context);
    let recovery_request = luther_workflow::engine::recovery::RecoveryRequest {
        run_id: ctx.request.run_id.clone(),
        step_id,
        expected_epoch,
        operator_verb: luther_workflow::engine::recovery::OperatorVerb::Resume,
    };
    let outcome = luther_workflow::engine::recovery::RecoveryProtocolV1
        .recover_with_executor(ctx.conn, ctx.workspace_path, &recovery_request, &executor)
        .map_err(|error| format!("daemon recovery protocol failed: {error}"))?;
    map_daemon_recovery_outcome(ctx, wait_config, outcome)
}

fn select_daemon_resume_step(ctx: &DaemonResumeContext<'_>) -> Result<String, String> {
    let request = luther_workflow::engine::ContinuationRequest {
        run_id: ctx.request.run_id.clone(),
        kind: luther_workflow::engine::ContinuationKind::Resume,
        force: true,
    };
    luther_workflow::engine::continuation::select_checkpoint(ctx.conn, &request, ctx.metadata)
        .map(|checkpoint| checkpoint.step_id)
        .map_err(|error| format!("select daemon recovery step: {error}"))
}

fn map_daemon_recovery_outcome(
    ctx: &DaemonResumeContext<'_>,
    wait_config: &WorkflowConfig,
    outcome: luther_workflow::engine::recovery::RecoveryOutcome,
) -> Result<luther_workflow::daemon::launcher::WorkflowLaunchResult, String> {
    let attempt_id = match outcome {
        luther_workflow::engine::recovery::RecoveryOutcome::Recovered { attempt_id, .. }
        | luther_workflow::engine::recovery::RecoveryOutcome::AlreadyApplied {
            attempt_id, ..
        } => attempt_id,
        other => return Err(format!("daemon recovery refused: {other:?}")),
    };
    let attempt = luther_workflow::persistence::attempts::load_attempt(ctx.conn, attempt_id)
        .map_err(|error| format!("load recovery attempt: {error}"))?;
    let runner_result: DurableDaemonRunnerResult = serde_json::from_value(
        attempt
            .runner_result_json
            .ok_or_else(|| "recovery attempt has no runner result".to_string())?,
    )
    .map_err(|error| format!("decode recovery runner result: {error}"))?;
    match runner_result.outcome.as_str() {
        "success" => Ok(luther_workflow::daemon::launcher::WorkflowLaunchResult::CompletedSuccess),
        "waiting_external" => {
            persist_external_wait_state(
                ctx.request,
                wait_config,
                ctx.db_path,
                &runner_result.step_id,
                &runner_result.reason,
            )
            .map_err(|error| format!("persist wait state: {error}"))?;
            Ok(luther_workflow::daemon::launcher::WorkflowLaunchResult::SuspendedExternalWait)
        }
        "abandoned" => classify_daemon_terminal(ctx),
        "failure" | "interrupted" => {
            Ok(luther_workflow::daemon::launcher::WorkflowLaunchResult::CompletedFailure)
        }
        other => Err(format!("unknown recovery runner outcome: {other}")),
    }
}

fn classify_daemon_terminal(
    ctx: &DaemonResumeContext<'_>,
) -> Result<luther_workflow::daemon::launcher::WorkflowLaunchResult, String> {
    let metadata = get_run_with_conn(ctx.conn, &ctx.request.run_id)
        .map_err(|error| format!("load run after recovery: {error}"))?
        .ok_or_else(|| format!("missing run after recovery: {}", ctx.request.run_id))?;
    if metadata.is_ownership_denied_terminal() {
        Ok(luther_workflow::daemon::launcher::WorkflowLaunchResult::OwnershipDenied)
    } else if metadata.is_cleanup_failure_abandonment() {
        Ok(luther_workflow::daemon::launcher::WorkflowLaunchResult::CleanupAbandoned)
    } else {
        Ok(luther_workflow::daemon::launcher::WorkflowLaunchResult::CompletedFailure)
    }
}

/// Resolve the workflow type and config for a daemon resume, applying the
/// daemon claim overrides and the persisted target-profile overrides. Returns
/// the resolved workflow type, the resolved (and overridden) config, a clone
/// of the config captured for provenance verification, and the persisted
/// target-profile overrides for later re-application after promotion.
fn resolve_daemon_resume_workflow(
    metadata: &luther_workflow::persistence::RunMetadata,
    config_root: &std::path::Path,
    request: &luther_workflow::daemon::launcher::LaunchRequest,
) -> Result<
    (
        luther_workflow::workflow::schema::WorkflowType,
        WorkflowConfig,
        WorkflowConfig,
        TargetProfileOverrides,
    ),
    String,
> {
    let workflow_type = resolve_workflow_type(&metadata.workflow_type_id, config_root)
        .map_err(|e| format!("resolve workflow type '{}': {e}", metadata.workflow_type_id))?;
    let mut wait_config = resolve_workflow_config(&metadata.config_id, config_root)
        .map_err(|e| format!("resolve config '{}': {e}", metadata.config_id))?;
    apply_daemon_claim_overrides(&mut wait_config, request);
    let persisted_overrides =
        luther_workflow::engine::continuation::continuation_overrides(metadata);
    apply_target_profile_overrides(&mut wait_config, &persisted_overrides)
        .map_err(|e| format!("apply resume provenance overrides: {e}"))?;
    let provenance_config = wait_config.clone();
    Ok((
        workflow_type,
        wait_config,
        provenance_config,
        persisted_overrides,
    ))
}

/// Verify the persisted launch provenance against the resolved workflow type
/// and config. Legacy rows are admitted with a warning; a mismatch fails the
/// resume before any durable mutation.
fn verify_daemon_resume_provenance(
    launch_provenance: &Option<luther_workflow::persistence::LaunchProvenance>,
    workflow_type: &luther_workflow::workflow::schema::WorkflowType,
    provenance_config: &WorkflowConfig,
    config_root: &std::path::Path,
) -> Result<(), String> {
    match luther_workflow::persistence::verify_provenance(
        launch_provenance,
        workflow_type,
        provenance_config,
        config_root,
        luther_workflow::persistence::LegacyAllowed::Allowed,
    ) {
        luther_workflow::persistence::ProvenanceVerification::Match => Ok(()),
        luther_workflow::persistence::ProvenanceVerification::Legacy(warning) => {
            eprintln!("Warning: {warning}");
            Ok(())
        }
        luther_workflow::persistence::ProvenanceVerification::Mismatch(reason) => {
            Err(format!("resume launch provenance mismatch: {reason}"))
        }
    }
}

/// Authorize a daemon resume continuation: the persisted run must have a
/// non-empty `current_step` and pass read-only continuation validation. Both
/// checks are performed before any durable mutation.
fn authorize_daemon_resume_continuation(
    conn: &rusqlite::Connection,
    run_id: &str,
    metadata: &luther_workflow::persistence::RunMetadata,
) -> Result<(), String> {
    if metadata
        .current_step
        .as_deref()
        .unwrap_or_default()
        .is_empty()
    {
        return Err(format!("missing current_step for resume of run {run_id}"));
    }
    let continuation_request = luther_workflow::engine::ContinuationRequest {
        run_id: run_id.to_string(),
        kind: luther_workflow::engine::ContinuationKind::Resume,
        force: true,
    };
    let validation =
        luther_workflow::engine::continuation::validate_continuation(conn, &continuation_request)
            .map_err(|error| format!("validate resume continuation: {error}"))?;
    if !validation.ok {
        return Err(format!(
            "resume continuation authorization failed: {}",
            validation.failure_reasons().join("; ")
        ));
    }
    Ok(())
}

/// Validate all request identity fields against persisted run metadata before
/// ownership promotion or any other resume mutation.
pub(super) fn validate_resume_identity_against_metadata(
    request: &luther_workflow::daemon::launcher::LaunchRequest,
    metadata: &luther_workflow::persistence::RunMetadata,
) -> Result<(), String> {
    validate_resume_run_id(request, metadata)?;
    validate_resume_config_id(request, metadata)?;
    validate_resume_workflow_type(request, metadata)?;
    validate_resume_repo(request, metadata)?;
    validate_resume_issue(request, metadata)?;
    validate_resume_artifact(request, metadata)?;
    Ok(())
}

/// Validate the request run_id against the persisted run_id.
fn validate_resume_run_id(
    request: &luther_workflow::daemon::launcher::LaunchRequest,
    metadata: &luther_workflow::persistence::RunMetadata,
) -> Result<(), String> {
    if request.run_id != metadata.run_id {
        return Err(format!(
            "resume identity mismatch: request run_id '{}' does not match persisted run_id '{}'",
            request.run_id, metadata.run_id
        ));
    }
    Ok(())
}

/// Validate the request config_id against the persisted config_id.
fn validate_resume_config_id(
    request: &luther_workflow::daemon::launcher::LaunchRequest,
    metadata: &luther_workflow::persistence::RunMetadata,
) -> Result<(), String> {
    if request.config_id != metadata.config_id {
        return Err(format!(
            "resume identity mismatch: request config_id '{}' does not match persisted config_id '{}'",
            request.config_id, metadata.config_id
        ));
    }
    Ok(())
}

/// Validate the request workflow_type_id (when provided) against persisted.
fn validate_resume_workflow_type(
    request: &luther_workflow::daemon::launcher::LaunchRequest,
    metadata: &luther_workflow::persistence::RunMetadata,
) -> Result<(), String> {
    if let Some(request_workflow_type_id) = request.workflow_type_id.as_deref() {
        if request_workflow_type_id != metadata.workflow_type_id {
            return Err(format!(
                "resume identity mismatch: request workflow_type_id '{}' does not match persisted workflow_type_id '{}'",
                request_workflow_type_id, metadata.workflow_type_id
            ));
        }
    }
    Ok(())
}

/// Validate the request repo against the persisted repository.
fn validate_resume_repo(
    request: &luther_workflow::daemon::launcher::LaunchRequest,
    metadata: &luther_workflow::persistence::RunMetadata,
) -> Result<(), String> {
    if Some(request.repo.as_str()) != metadata.repository.as_deref() {
        return Err(format!(
            "resume identity mismatch: request repo '{}' does not match persisted repository {:?}",
            request.repo, metadata.repository
        ));
    }
    Ok(())
}

/// Validate the request issue_number against the persisted issue number.
///
/// Issue 158 slice 5: a daemon resume is always issue-bound (the daemon claims
/// issues via the lease table), so the request `issue_number` must match the
/// persisted `issue_number` exactly. A PR number is not a valid lease
/// authority anchor for daemon resume validation, so `issue_lease_number`
/// (issue-only) is used rather than `issue_number.or(pr_number)`.
fn validate_resume_issue(
    request: &luther_workflow::daemon::launcher::LaunchRequest,
    metadata: &luther_workflow::persistence::RunMetadata,
) -> Result<(), String> {
    let persisted_issue = metadata.issue_lease_number();
    if Some(request.issue_number) != persisted_issue {
        return Err(format!(
            "resume identity mismatch: request issue_number {} does not match persisted issue {:?}",
            request.issue_number, persisted_issue
        ));
    }
    Ok(())
}

/// Validate the request artifact_dir against the persisted artifact_root.
///
/// Exact typed Option<PathBuf> comparison: the request's artifact_dir
/// (Option<PathBuf>) must match the persisted artifact_root reconstructed as
/// Option<PathBuf> exactly. This avoids a lossy to_str()/str comparison that
/// could accept a path with a non-UTF-8 byte sequence that round-trips
/// differently. A missing request artifact_dir is acceptable (it is
/// reconstructed from persisted identity downstream); a missing persisted
/// artifact_root with a present request value is a mismatch.
fn validate_resume_artifact(
    request: &luther_workflow::daemon::launcher::LaunchRequest,
    metadata: &luther_workflow::persistence::RunMetadata,
) -> Result<(), String> {
    let request_artifact: Option<std::path::PathBuf> = request.artifact_dir.clone();
    let persisted_artifact: Option<std::path::PathBuf> = metadata
        .artifact_root
        .as_ref()
        .map(std::path::PathBuf::from);
    if let Some(request_artifact_dir) = request_artifact.as_deref() {
        if persisted_artifact.as_deref() != Some(request_artifact_dir) {
            return Err(format!(
                "resume identity mismatch: request artifact_dir '{}' does not match persisted artifact_root {:?}",
                request_artifact_dir.display(),
                persisted_artifact
            ));
        }
    }
    Ok(())
}

// Resolve the persisted resume workspace before any ownership mutation. The
// persisted workspace path is the authoritative source of truth: an explicit
// request path must match it exactly, and an omitted request work_dir
// reconstructs from the persisted identity (it does not bypass comparison,
// because the persisted path — not the request — is what ownership
// verification runs against).
pub(super) fn resolve_resume_workspace_path<'a>(
    persisted_workspace: &'a str,
    request: &luther_workflow::daemon::launcher::LaunchRequest,
) -> Result<&'a std::path::Path, String> {
    let persisted = std::path::Path::new(persisted_workspace);
    if let Some(requested) = request.work_dir.as_deref() {
        if requested != persisted {
            return Err(format!(
                "resume workspace mismatch: request work_dir '{}' does not match persisted workspace_path '{}'",
                requested.display(),
                persisted.display()
            ));
        }
    }
    Ok(persisted)
}

pub fn run_daemon_runner(
    request: &luther_workflow::daemon::launcher::LaunchRequest,
    wait_config: &WorkflowConfig,
    db_path: &std::path::Path,
    runner: &mut EngineRunner,
) -> Result<luther_workflow::daemon::launcher::WorkflowLaunchResult, String> {
    match runner.run() {
        Ok(RunOutcome::Success) => {
            Ok(luther_workflow::daemon::launcher::WorkflowLaunchResult::CompletedSuccess)
        }
        Ok(RunOutcome::WaitingExternal { step_id, reason }) => {
            persist_external_wait_state(request, wait_config, db_path, &step_id, &reason)
                .map_err(|e| format!("persist wait state: {e}"))?;
            Ok(luther_workflow::daemon::launcher::WorkflowLaunchResult::SuspendedExternalWait)
        }
        Ok(RunOutcome::Abandoned { .. }) => {
            let conn = rusqlite::Connection::open(db_path)
                .map_err(|error| format!("open run registry after abandonment: {error}"))?;
            let metadata = get_run_with_conn(&conn, &request.run_id)
                .map_err(|error| format!("load run after abandonment: {error}"))?
                .ok_or_else(|| {
                    format!("missing run metadata after abandonment: {}", request.run_id)
                })?;
            // An ownership-denied terminal is a distinct non-resumable state
            // that must never be selected for cleanup continuation. Check it
            // BEFORE the cleanup-abandonment check so an ownership-denied run
            // is not misclassified as CleanupAbandoned (which could route it
            // to cleanup continuation in a future daemon cycle).
            if metadata.is_ownership_denied_terminal() {
                return Ok(
                    luther_workflow::daemon::launcher::WorkflowLaunchResult::OwnershipDenied,
                );
            }
            if metadata.is_cleanup_failure_abandonment() {
                Ok(luther_workflow::daemon::launcher::WorkflowLaunchResult::CleanupAbandoned)
            } else {
                Ok(luther_workflow::daemon::launcher::WorkflowLaunchResult::CompletedFailure)
            }
        }
        Ok(RunOutcome::Failure { .. }) => {
            let conn = rusqlite::Connection::open(db_path)
                .map_err(|error| format!("open run registry after failure: {error}"))?;
            let metadata = get_run_with_conn(&conn, &request.run_id)
                .map_err(|error| format!("load run after failure: {error}"))?
                .ok_or_else(|| format!("missing run metadata after failure: {}", request.run_id))?;
            // An ownership-denied terminal is a distinct non-resumable state
            // that must never be selected for cleanup continuation. Check it
            // BEFORE the cleanup-succeeded check so an ownership-denied run is
            // not misclassified as CleanupAbandoned.
            if metadata.is_ownership_denied_terminal() {
                return Ok(
                    luther_workflow::daemon::launcher::WorkflowLaunchResult::OwnershipDenied,
                );
            }
            if metadata
                .failure_cleanup
                .as_ref()
                .is_some_and(|failure| !failure.cleanup_succeeded)
            {
                Ok(luther_workflow::daemon::launcher::WorkflowLaunchResult::CleanupAbandoned)
            } else {
                Ok(luther_workflow::daemon::launcher::WorkflowLaunchResult::CompletedFailure)
            }
        }
        Ok(RunOutcome::Interrupted { .. }) => {
            Ok(luther_workflow::daemon::launcher::WorkflowLaunchResult::CompletedFailure)
        }
        Err(e) => Err(format!("run error: {e}")),
    }
}
