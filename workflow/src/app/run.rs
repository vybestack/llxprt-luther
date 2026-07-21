use super::runs::reconstruct_runner_with_config;
use super::status::install_interrupt_handlers;
use super::wait_state::persist_external_wait_state;
use luther_workflow::adapters::github::{run_preflight, GithubError, SystemGithubCommandRunner};
use luther_workflow::adapters::llxprt::{
    run_preflight as run_llxprt_preflight, LlxprtError, SystemLlxprtCommandRunner,
};
use luther_workflow::engine::executor::ExecutorRegistry;
use luther_workflow::engine::instance::WorkflowInstance;
use luther_workflow::engine::runner::{EngineRunner, RunOutcome};
use luther_workflow::persistence::{get_run_with_conn, init_database};
use luther_workflow::workflow::config_loader::{
    resolve_workflow, resolve_workflow_config, resolve_workflow_type,
    validate_artifact_dependencies, validate_workflow_tokens,
};
use luther_workflow::workflow::schema::{WorkflowConfig, WorkflowType};
use luther_workflow::workflow::target_profile::{
    apply_target_profile_overrides, target_profile_validation_required, validate_target_profile,
    TargetProfileOverrides,
};
use std::process;

/// Report dry-run semantic validation: unresolved interpolation tokens and
/// missing artifact producers. Returns `true` if any error was reported.
///
/// Output uses stable, greppable prefixes (`unresolved token:` /
/// `missing artifact producer:`) so callers and tests can assert on them.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P11
pub fn report_dry_run_validation(workflow_type: &WorkflowType, config: &WorkflowConfig) -> bool {
    let unresolved = validate_workflow_tokens(workflow_type, config);
    let missing = validate_artifact_dependencies(workflow_type);

    if !unresolved.is_empty() {
        println!("\nUnresolved interpolation tokens:");
        for token in &unresolved {
            println!(
                "  unresolved token: step '{}' {} references '{{{}}}'",
                token.step_id, token.parameter_path, token.token_name
            );
        }
    }

    if !missing.is_empty() {
        println!("\nMissing artifact producers:");
        for producer in &missing {
            println!(
                "  missing artifact producer: step '{}' consumes '{}' which no step produces",
                producer.consumer_step_id, producer.artifact_name
            );
        }
    }

    !unresolved.is_empty() || !missing.is_empty()
}

/// Determine whether the selected workflow actually depends on the GitHub CLI.
///
/// Returns `true` when any step is a registered `github_*` step type, or any
/// shell step's `command` parameter contains a `gh ` token. Pure
/// `shell`/`noop` workflows that never call `gh` return `false` so offline runs
/// are unaffected by the preflight gate.
pub fn workflow_requires_github(workflow_type: &WorkflowType) -> bool {
    workflow_type.steps.iter().any(|step| {
        if step.step_type.starts_with("github_") {
            return true;
        }
        step.parameters
            .as_ref()
            .and_then(|params| params.get("command"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|command| command.contains("gh "))
    })
}

/// Print actionable diagnostics for a failed GitHub preflight using a stable,
/// greppable prefix, then exit the process without creating any state.
pub fn fail_preflight(err: &GithubError) -> ! {
    eprintln!("gh preflight failed: {err}");
    let diagnostics = err.get_diagnostics();
    let mut keys: Vec<&String> = diagnostics.keys().collect();
    keys.sort();
    for key in keys {
        if let Some(value) = diagnostics.get(key) {
            eprintln!("  {key}: {value}");
        }
    }
    process::exit(1);
}

/// Returns `true` when any step is a `llxprt` step that actually spawns the
/// binary (i.e. is not a pure `static_content` / `static_stdout` step). Pure
/// static workflows never invoke `llxprt`, so the preflight gate is skipped.
pub fn workflow_requires_llxprt(workflow_type: &WorkflowType) -> bool {
    workflow_type.steps.iter().any(|step| {
        if step.step_type != "llxprt" {
            return false;
        }
        step.parameters.as_ref().is_none_or(|params| {
            let has_static = params
                .get("static_content")
                .and_then(serde_json::Value::as_str)
                .is_some()
                || params
                    .get("static_stdout")
                    .and_then(serde_json::Value::as_str)
                    .is_some();
            !has_static
        })
    })
}

/// Print actionable diagnostics for a failed llxprt preflight using a stable,
/// greppable prefix, then exit the process without creating any state.
pub fn fail_llxprt_preflight(err: &LlxprtError) -> ! {
    eprintln!("llxprt preflight failed: {err}");
    let diagnostics = err.get_diagnostics();
    let mut keys: Vec<&String> = diagnostics.keys().collect();
    keys.sort();
    for key in keys {
        if let Some(value) = diagnostics.get(key) {
            eprintln!("  {key}: {value}");
        }
    }
    process::exit(1);
}

/// Handle the run command.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
/// @plan:PLAN-20260408-LLXPRT-FIRST.P20
pub async fn handle_run_command(args: &luther_workflow::cli::RunArgs) {
    let config_root = run_config_root(args);
    let (workflow_type, mut config, run_ref) = resolve_run_inputs(args, &config_root);
    apply_run_target_overrides(args, &workflow_type, &mut config);

    let run_id = run_ref.run_id;
    validate_cli_run_id(&run_id);
    println!("Starting workflow run: {run_id}");
    println!("  Workflow type: {}", workflow_type.workflow_type_id);
    println!("  Config: {}", config.config_id);

    run_start_preflights(args, &workflow_type, &config);
    let db_path = luther_workflow::runtime_paths::get_data_dir().join("checkpoints.db");
    if let Err(e) = init_database(&db_path) {
        eprintln!("Warning: Failed to initialize checkpoint database: {e}");
    }
    if args.dry_run {
        finish_dry_run(&workflow_type, &config);
    }

    let mut runner = create_durable_runner(workflow_type, config, &run_id, &db_path, false);
    install_interrupt_handlers(runner.interrupt_handle());
    println!("Executing workflow...");
    match runner.run() {
        Ok(outcome) => exit_run_outcome(outcome, &run_id),
        Err(e) => {
            eprintln!("\nWorkflow execution error: {e}");
            process::exit(1);
        }
    }
}

fn validate_cli_run_id(run_id: &str) {
    if run_id.is_empty()
        || !run_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        eprintln!("Error: run id must contain only ASCII letters, digits, '-' or '_'");
        process::exit(1);
    }
}

pub fn run_config_root(args: &luther_workflow::cli::RunArgs) -> std::path::PathBuf {
    args.config_dir
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from("config"))
}

pub fn resolve_run_inputs(
    args: &luther_workflow::cli::RunArgs,
    config_root: &std::path::Path,
) -> (
    WorkflowType,
    WorkflowConfig,
    luther_workflow::workflow::schema::WorkflowRunRef,
) {
    match &args.config {
        Some(config_path) => resolve_explicit_run_config(args, config_root, config_path),
        None => resolve_default_run_config(args, config_root),
    }
}

pub fn resolve_explicit_run_config(
    args: &luther_workflow::cli::RunArgs,
    config_root: &std::path::Path,
    config_path: &std::path::Path,
) -> (
    WorkflowType,
    WorkflowConfig,
    luther_workflow::workflow::schema::WorkflowRunRef,
) {
    let config_id = config_path.file_stem().map_or_else(
        || "default".to_string(),
        |s| s.to_string_lossy().to_string(),
    );
    let workflow_type_id = args
        .workflow_type
        .clone()
        .unwrap_or_else(|| "test-workflow".to_string());
    let workflow_type = resolve_workflow_type(&workflow_type_id, config_root).unwrap_or_else(|e| {
        eprintln!("Error: Failed to resolve workflow type '{workflow_type_id}': {e}");
        process::exit(1);
    });
    let config = resolve_workflow_config(&config_id, config_root).unwrap_or_else(|e| {
        eprintln!("Error: Failed to resolve config '{config_id}': {e}");
        process::exit(1);
    });
    let run_ref = luther_workflow::workflow::schema::WorkflowRunRef::new(
        &workflow_type_id,
        &config_id,
        args.run_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
    );
    (workflow_type, config, run_ref)
}

pub fn resolve_default_run_config(
    args: &luther_workflow::cli::RunArgs,
    config_root: &std::path::Path,
) -> (
    WorkflowType,
    WorkflowConfig,
    luther_workflow::workflow::schema::WorkflowRunRef,
) {
    let workflow_type_id = args
        .workflow_type
        .clone()
        .unwrap_or_else(|| "test-workflow".to_string());
    let config_id = "test-config".to_string();
    let run_id = args
        .run_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    resolve_workflow(&workflow_type_id, &config_id, &run_id, config_root).unwrap_or_else(|e| {
        eprintln!("Error: Failed to resolve workflow: {e}");
        process::exit(1);
    })
}

pub fn apply_run_target_overrides(
    args: &luther_workflow::cli::RunArgs,
    workflow_type: &WorkflowType,
    config: &mut WorkflowConfig,
) {
    let overrides = TargetProfileOverrides {
        repo: args.repo.clone(),
        issue: args.issue.clone(),
        work_dir: args.work_dir.clone(),
        artifact_dir: args.artifact_dir.clone(),
    };
    if let Err(e) = apply_target_profile_overrides(config, &overrides) {
        eprintln!("Error: {e}");
        process::exit(1);
    }
    if target_profile_validation_required(&workflow_type.workflow_type_id, config, &overrides) {
        if let Err(e) = validate_target_profile(config) {
            eprintln!("Error: {e}");
            process::exit(1);
        }
    }
}

pub fn run_start_preflights(
    args: &luther_workflow::cli::RunArgs,
    workflow_type: &WorkflowType,
    config: &WorkflowConfig,
) {
    if args.dry_run || args.skip_preflight {
        return;
    }
    run_github_preflight_if_needed(args, workflow_type, config);
    run_llxprt_preflight_if_needed(workflow_type, config);
}

pub fn run_github_preflight_if_needed(
    args: &luther_workflow::cli::RunArgs,
    workflow_type: &WorkflowType,
    config: &WorkflowConfig,
) {
    if !workflow_requires_github(workflow_type) {
        return;
    }
    let repo = config
        .variables
        .get("target_repo")
        .cloned()
        .or_else(|| args.repo.clone());
    let Some(repo) = repo else {
        return;
    };
    let runner = SystemGithubCommandRunner;
    match run_preflight(&runner, &repo, &["repo"]) {
        Ok(report) => println!(
            "  GitHub preflight OK: repo {} (scopes: {})",
            report.repo,
            report.scopes.join(", ")
        ),
        Err(e) => fail_preflight(&e),
    }
}

pub fn run_llxprt_preflight_if_needed(workflow_type: &WorkflowType, config: &WorkflowConfig) {
    if workflow_requires_llxprt(workflow_type) {
        let runner = SystemLlxprtCommandRunner;
        match run_llxprt_preflight(&runner, workflow_type, &config.variables) {
            Ok(paths) => println!("  llxprt preflight OK: validated {}", paths.join(", ")),
            Err(e) => fail_llxprt_preflight(&e),
        }
    }
}

pub fn finish_dry_run(workflow_type: &WorkflowType, config: &WorkflowConfig) -> ! {
    println!("Dry run mode - workflow would execute the following steps:");
    for step in &workflow_type.steps {
        println!(
            "  - {} ({}): {:?}",
            step.step_id,
            step.step_type,
            step.description.as_deref().unwrap_or("No description")
        );
    }
    if report_dry_run_validation(workflow_type, config) {
        eprintln!("\nDry run found validation errors. No changes made.");
        process::exit(1);
    }
    println!("\nDry run complete. No changes made.");
    process::exit(0);
}

pub fn exit_run_outcome(outcome: RunOutcome, run_id: &str) -> ! {
    match outcome {
        RunOutcome::Success => {
            println!("\nWorkflow completed successfully!");
            println!("Run ID: {run_id}");
            process::exit(0);
        }
        RunOutcome::Failure { step_id, reason } => {
            eprintln!("\nWorkflow failed at step '{step_id}'");
            eprintln!("Reason: {reason}");
            process::exit(1);
        }
        RunOutcome::Abandoned { step_id, reason } => {
            eprintln!("\nWorkflow abandoned at step '{step_id}'");
            eprintln!("Reason: {reason}");
            process::exit(1);
        }
        RunOutcome::Interrupted { step_id } => {
            println!("\nWorkflow interrupted at step '{step_id}'");
            println!("Run ID: {run_id} (can be resumed)");
            process::exit(130);
        }
        RunOutcome::WaitingExternal { step_id, reason } => {
            println!("\nWorkflow paused at step '{step_id}' awaiting external state");
            println!("Reason: {reason}");
            println!("Run ID: {run_id}");
            println!("Resume with: luther-workflow runs resume {run_id}");
            process::exit(0);
        }
    }
}

pub fn create_durable_runner(
    workflow_type: luther_workflow::workflow::schema::WorkflowType,
    config: luther_workflow::workflow::schema::WorkflowConfig,
    run_id: &str,
    db_path: &std::path::Path,
    daemon_managed: bool,
) -> EngineRunner {
    let mut run_context = build_run_context(&config, run_id);
    run_context.daemon_managed = daemon_managed;
    let instance = WorkflowInstance::create_with_run_id(workflow_type, config, run_id);
    let registry = ExecutorRegistry::with_defaults();
    // Attach the run context up front so the initial persisted `Starting` row
    // includes path and GitHub metadata, instead of chaining
    // `with_run_context` after the initial record has already been written.
    match EngineRunner::with_db_path_and_context(instance, registry, db_path, run_context) {
        Ok(runner) => runner,
        Err(e) => {
            eprintln!("Error: Failed to create durable engine runner: {e}");
            process::exit(1);
        }
    }
}

/// Build a [`RunContext`] from a workflow config and run id, populating run
/// paths (log/artifact/workspace) and GitHub references when available.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub fn build_run_context(
    config: &luther_workflow::workflow::schema::WorkflowConfig,
    run_id: &str,
) -> luther_workflow::engine::RunContext {
    let vars = &config.variables;
    let repository = vars.get("target_repo").cloned();
    let issue_number = vars
        .get("primary_issue_number")
        .or_else(|| vars.get("issue_number"))
        .and_then(|s| s.parse::<i64>().ok());
    let workspace_path = Some(vars.get("work_dir").cloned().unwrap_or_else(|| {
        luther_workflow::runtime_paths::get_run_dir(run_id)
            .to_string_lossy()
            .to_string()
    }));
    let log_path = Some(
        luther_workflow::runtime_paths::get_log_dir()
            .join(format!("{run_id}.log"))
            .to_string_lossy()
            .to_string(),
    );
    let artifact_root = Some(vars.get("artifact_dir").cloned().unwrap_or_else(|| {
        luther_workflow::runtime_paths::get_artifacts_root()
            .to_string_lossy()
            .to_string()
    }));
    luther_workflow::engine::RunContext {
        daemon_managed: false,
        log_path,
        artifact_root,
        workspace_path,
        repository,
        issue_number,
        pr_number: None,
        head_sha: None,
    }
}
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
    let config_root = std::path::PathBuf::from("config");
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
    ensure_daemon_run_dirs(request)?;
    let db_path = luther_workflow::runtime_paths::get_data_dir().join("checkpoints.db");
    let wait_config = config.clone();
    let mut runner = create_durable_runner(
        workflow_type,
        config,
        &request.run_id,
        &db_path,
        request.daemon_managed_claim,
    );
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
    ensure_daemon_run_dir("artifact", request.artifact_dir.as_deref())?;
    ensure_daemon_workspace(request.work_dir.as_deref(), &request.run_id)
}

fn ensure_daemon_workspace(work_dir: Option<&std::path::Path>, run_id: &str) -> Result<(), String> {
    let Some(work_dir) = work_dir else {
        return Ok(());
    };
    luther_workflow::engine::continuation::provision_workspace_owner_marker(work_dir, run_id)
        .map_err(|e| format!("provision workspace owner marker: {e}"))
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
    let db_path = luther_workflow::runtime_paths::get_data_dir().join("checkpoints.db");
    let conn = rusqlite::Connection::open(&db_path).map_err(|e| e.to_string())?;
    let metadata = get_run_with_conn(&conn, &request.run_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("missing run metadata for {}", request.run_id))?;
    // Daemon-launched resumes use the same hardcoded "config" root as launch_daemon_workflow;
    // CLI `runs resume --config-dir` covers temporary per-run config roots.
    let config_root = std::path::PathBuf::from("config");
    let mut wait_config = resolve_workflow_config(&metadata.config_id, &config_root)
        .map_err(|e| format!("resolve config '{}': {e}", metadata.config_id))?;
    apply_daemon_claim_overrides(&mut wait_config, request);
    let workspace = metadata.workspace_path.as_deref().ok_or_else(|| {
        format!(
            "missing workspace_path for resume of run {}",
            request.run_id
        )
    })?;
    luther_workflow::engine::continuation::verify_workspace_ownership_marker(
        std::path::Path::new(workspace),
        &request.run_id,
    )
    .map_or(Ok(()), Err)?;
    let config_dir = Some(config_root);
    if metadata
        .current_step
        .as_deref()
        .unwrap_or_default()
        .is_empty()
    {
        return Err(format!(
            "missing current_step for resume of run {}",
            request.run_id
        ));
    }
    let overrides = TargetProfileOverrides {
        repo: Some(request.repo.clone()),
        issue: Some(request.issue_number.to_string()),
        work_dir: request.work_dir.clone(),
        artifact_dir: request.artifact_dir.clone(),
    };
    apply_target_profile_overrides(&mut wait_config, &overrides)
        .map_err(|e| format!("apply resume overrides: {e}"))?;
    // Provision workspace dirs and ownership marker on resume too, so the
    // durable ownership anchor stays current for the resumed run id even when
    // the original launch did not write it (or the workspace moved).
    ensure_daemon_run_dirs(request)?;
    let mut runner = reconstruct_runner_with_config(
        &metadata,
        &request.run_id,
        &db_path,
        &config_dir,
        wait_config.clone(),
    )?;
    // Construct the resume request once and derive the checkpoint identity via
    // request-bound `select_checkpoint` rather than a first-by-step lookup. The
    // same request is then passed to `commit_continuation`, whose internal
    // transaction re-selects and verifies this identity, so the bound identity
    // and the in-transaction selection cannot diverge. `select_checkpoint`
    // honors failure-cleanup provenance and terminal-step handling rather than
    // blindly matching the first checkpoint for `current_step`.
    let resume_request = luther_workflow::engine::ContinuationRequest {
        run_id: request.run_id.clone(),
        kind: luther_workflow::engine::ContinuationKind::Resume,
        force: true,
        trusted_internal: true,
    };
    let checkpoint =
        luther_workflow::engine::continuation::select_checkpoint(&conn, &resume_request, &metadata)
            .map_err(|e| format!("select resume checkpoint: {e}"))?;
    let checkpoint_identity =
        luther_workflow::engine::continuation::checkpoint_identity(&checkpoint);
    luther_workflow::engine::commit_continuation(&conn, &resume_request, &checkpoint_identity)
        .map_err(|e| format!("commit resume: {e}"))?;
    run_daemon_runner(request, &wait_config, &db_path, &mut runner)
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

#[cfg(test)]
#[path = "run_tests.rs"]
mod run_tests;
