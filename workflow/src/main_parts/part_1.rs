/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
/// Main entry point for the luther-workflow CLI.
use std::path::Path;
use std::process;

use tracing_subscriber::{fmt, EnvFilter};

use luther_workflow::adapters::github::{run_preflight, GithubError, SystemGithubCommandRunner};
use luther_workflow::adapters::github_issues::SystemGithubIssueQuery;
use luther_workflow::adapters::llxprt::{
    run_preflight as run_llxprt_preflight, LlxprtError, SystemLlxprtCommandRunner,
};
use luther_workflow::cli::{parse_args, Commands};
use luther_workflow::daemon::discovery::{discover, DiscoveryResult};
use luther_workflow::daemon::{
    is_daemon_alive, stop_daemon, DaemonState, DaemonStatus, DaemonStore, StopOutcome,
};
use luther_workflow::engine::executor::ExecutorRegistry;
use luther_workflow::engine::instance::WorkflowInstance;
use luther_workflow::engine::runner::{EngineRunner, RunOutcome};
use luther_workflow::monitor::heartbeat::read_all_heartbeats;
use luther_workflow::monitor::heartbeat::MonitorState;
use luther_workflow::monitor::snapshot::{
    render_snapshot, resolve_snapshot_count, separator_line, DaemonSummary, MonitorFilter,
    MonitorSnapshot, RunCounts, CLEAR_SCREEN,
};
use luther_workflow::persistence::init_database;
use luther_workflow::persistence::leases::{
    list_all_leases, list_leases_by_config, IssueLease, LeaseStatus,
};
use luther_workflow::persistence::{
    get_run_with_conn, get_wait_state, list_artifacts, list_wait_states, load_checkpoint_with_conn,
    load_events, load_recent_events, persist_run_with_conn, upsert_wait_state,
    write_wait_state_artifact, EventRecord, RunMetadata, RunStatus, SqliteStore, WaitKind,
    WaitStateRecord,
};
use luther_workflow::service::{Service, ServiceConfig};
use luther_workflow::workflow::config_loader::{
    load_daemon_scheduler_config, resolve_discovery_config, resolve_workflow,
    resolve_workflow_config, resolve_workflow_type, validate_artifact_dependencies,
    validate_workflow_tokens,
};
use luther_workflow::workflow::schema::{StepDef, WorkflowConfig, WorkflowType};
use serde_json::{Map, Value};
use luther_workflow::workflow::target_profile::{
    apply_target_profile_overrides, target_profile_validation_required, validate_target_profile,
    TargetProfileOverrides,
};

#[tokio::main]
async fn main() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(env_filter).with_target(false).init();

    let cli = parse_args();

    match cli.command {
        Commands::Run(args) => {
            handle_run_command(&args).await;
        }
        Commands::Status(args) => {
            handle_status_command(&args).await;
        }
        Commands::Service(args) => {
            handle_service_command(&args).await;
        }
        Commands::Daemon(args) => {
            handle_daemon_command(&args).await;
        }
        Commands::Runs(args) => {
            handle_runs_command(&args).await;
        }
        Commands::Monitor(args) => {
            handle_monitor_command(&args).await;
        }
    }
}

/// Report dry-run semantic validation: unresolved interpolation tokens and
/// missing artifact producers. Returns `true` if any error was reported.
///
/// Output uses stable, greppable prefixes (`unresolved token:` /
/// `missing artifact producer:`) so callers and tests can assert on them.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P11
fn report_dry_run_validation(workflow_type: &WorkflowType, config: &WorkflowConfig) -> bool {
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
fn workflow_requires_github(workflow_type: &WorkflowType) -> bool {
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
fn fail_preflight(err: &GithubError) -> ! {
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
fn workflow_requires_llxprt(workflow_type: &WorkflowType) -> bool {
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
fn fail_llxprt_preflight(err: &LlxprtError) -> ! {
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
async fn handle_run_command(args: &luther_workflow::cli::RunArgs) {
    let config_root = run_config_root(args);
    let (workflow_type, mut config, run_ref) = resolve_run_inputs(args, &config_root);
    apply_run_target_overrides(args, &workflow_type, &mut config);

    let run_id = run_ref.run_id;
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

    let mut runner = create_durable_runner(workflow_type, config, &run_id, &db_path);
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

fn run_config_root(args: &luther_workflow::cli::RunArgs) -> std::path::PathBuf {
    args.config_dir
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from("config"))
}

fn resolve_run_inputs(
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

fn resolve_explicit_run_config(
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

fn resolve_default_run_config(
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

fn apply_run_target_overrides(
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

fn run_start_preflights(
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

fn run_github_preflight_if_needed(
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

fn run_llxprt_preflight_if_needed(workflow_type: &WorkflowType, config: &WorkflowConfig) {
    if workflow_requires_llxprt(workflow_type) {
        let runner = SystemLlxprtCommandRunner;
        match run_llxprt_preflight(&runner, workflow_type, &config.variables) {
            Ok(paths) => println!("  llxprt preflight OK: validated {}", paths.join(", ")),
            Err(e) => fail_llxprt_preflight(&e),
        }
    }
}

fn finish_dry_run(workflow_type: &WorkflowType, config: &WorkflowConfig) -> ! {
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

fn exit_run_outcome(outcome: RunOutcome, run_id: &str) -> ! {
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

fn create_durable_runner(
    workflow_type: luther_workflow::workflow::schema::WorkflowType,
    config: luther_workflow::workflow::schema::WorkflowConfig,
    run_id: &str,
    db_path: &std::path::Path,
) -> EngineRunner {
    let run_context = build_run_context(&config, run_id);
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
fn build_run_context(
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
struct DaemonWorkflowLauncher;

impl DaemonWorkflowLauncher {
    fn new(_config_id: String) -> Self {
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

fn launch_daemon_workflow(
    config_id: &str,
    request: &luther_workflow::daemon::launcher::LaunchRequest,
) -> Result<luther_workflow::daemon::launcher::WorkflowLaunchResult, String> {
    let config_root = std::path::PathBuf::from("config");
    let mut config = resolve_workflow_config(config_id, &config_root)
        .map_err(|e| format!("resolve config '{config_id}': {e}"))?;
    let workflow_type = resolve_workflow_type(&config.workflow_type_id, &config_root)
        .map_err(|e| format!("resolve workflow type: {e}"))?;
    let overrides = TargetProfileOverrides {
        repo: Some(request.repo.clone()),
        issue: Some(request.issue_number.to_string()),
        work_dir: None,
        artifact_dir: None,
    };
    apply_target_profile_overrides(&mut config, &overrides)
        .map_err(|e| format!("apply overrides: {e}"))?;
    let db_path = luther_workflow::runtime_paths::get_data_dir().join("checkpoints.db");
    let wait_config = config.clone();
    let mut runner = create_durable_runner(workflow_type, config, &request.run_id, &db_path);
    run_daemon_runner(request, &wait_config, &db_path, &mut runner)
}

fn resume_daemon_workflow(
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
    let wait_config = resolve_workflow_config(&metadata.config_id, &config_root)
        .map_err(|e| format!("resolve config '{}': {e}", metadata.config_id))?;
    let config_dir = Some(config_root);
    let step = metadata
        .current_step
        .as_deref()
        .filter(|step| !step.is_empty())
        .ok_or_else(|| format!("missing current_step for resume of run {}", request.run_id))?;
    luther_workflow::engine::commit_continuation(
        &conn,
        &luther_workflow::engine::ContinuationRequest {
            run_id: request.run_id.clone(),
            kind: luther_workflow::engine::ContinuationKind::Resume,
            force: true,
        },
        step,
    )
    .map_err(|e| format!("commit resume: {e}"))?;
    let mut runner = reconstruct_runner(&metadata, &request.run_id, &db_path, &config_dir)?;
    run_daemon_runner(request, &wait_config, &db_path, &mut runner)
}

fn run_daemon_runner(
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
        Ok(_) => Ok(luther_workflow::daemon::launcher::WorkflowLaunchResult::CompletedFailure),
        Err(e) => Err(format!("run error: {e}")),
    }
}

fn persist_external_wait_state(
    request: &luther_workflow::daemon::launcher::LaunchRequest,
    config: &WorkflowConfig,
    db_path: &std::path::Path,
    step_id: &str,
    reason: &str,
) -> Result<(), String> {
    let conn = rusqlite::Connection::open(db_path).map_err(|e| e.to_string())?;
    let checkpoint = load_checkpoint_with_conn(&conn, &request.run_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("missing waiting checkpoint for {}", request.run_id))?;
    let mut metadata = get_run_with_conn(&conn, &request.run_id).map_err(|e| e.to_string())?;
    let wait_kind = wait_kind_for_step(step_id);
    let identity = wait_poll_identity(request, config, metadata.as_ref(), wait_kind)?;
    if let Some(md) = metadata.as_mut() {
        persist_run_poll_identity(&conn, md, &identity)?;
    }
    let previous = get_wait_state(&conn, &request.run_id).map_err(|e| e.to_string())?;
    let mut record =
        previous.unwrap_or_else(|| WaitStateRecord::new(&request.run_id, &config.config_id));
    record.lease_id = lookup_lease_id(&conn, request)?;
    record.workflow_type = config.workflow_type_id.clone();
    record.config_id = config.config_id.clone();
    record.repository = request.repo.clone();
    record.issue_number = request.issue_number;
    record.pr_number = identity.pr_number;
    record.head_sha = identity.head_sha;
    record.wait_kind = wait_kind;
    let step_params = resolved_wait_step_parameters(config, step_id)?;
    record.wait_condition = wait_condition_payload(
        step_id,
        reason,
        request,
        wait_kind,
        &step_params,
    )?;
    record.last_observed_state = serde_json::json!({
        "classification": "suspended",
        "step_id": step_id,
        "reason": reason
    });
    let poll_interval = config
        .discovery
        .as_ref()
        .and_then(|d| d.poll_interval_secs)
        .unwrap_or(300);
    record.poll_interval_seconds = poll_interval;
    record.next_poll_at = chrono::Utc::now() + chrono::Duration::seconds(poll_interval as i64);
    record.resume_step = checkpoint.step_id.clone();
    record.checkpoint_id = luther_workflow::engine::continuation::checkpoint_identity(&checkpoint);
    upsert_wait_state(&conn, &record).map_err(|e| e.to_string())?;
    if let Err(e) = write_wait_state_artifact(&request.run_id, &record) {
        eprintln!(
            "Warning: failed to write wait-state artifact for run {}: {e}",
            request.run_id
        );
    }
    Ok(())
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct WaitPollIdentity {
    pr_number: Option<u64>,
    head_sha: Option<String>,
}

fn persist_run_poll_identity(
    conn: &rusqlite::Connection,
    metadata: &mut RunMetadata,
    identity: &WaitPollIdentity,
) -> Result<(), String> {
    let mut changed = false;
    if let Some(pr_number) = identity.pr_number {
        let pr_number = i64::try_from(pr_number).map_err(|e| e.to_string())?;
        if metadata.pr_number != Some(pr_number) {
            metadata.pr_number = Some(pr_number);
            changed = true;
        }
    }
    if let Some(head_sha) = identity.head_sha.as_ref() {
        if metadata.head_sha.as_deref() != Some(head_sha.as_str()) {
            metadata.head_sha = Some(head_sha.clone());
            changed = true;
        }
    }
    if changed {
        persist_run_with_conn(conn, metadata).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn wait_poll_identity(
    request: &luther_workflow::daemon::launcher::LaunchRequest,
    config: &WorkflowConfig,
    metadata: Option<&RunMetadata>,
    wait_kind: WaitKind,
) -> Result<WaitPollIdentity, String> {
    let artifact_root = wait_artifact_root(config, metadata)?;
    let artifact_identity = artifact_root
        .as_deref()
        .map(|root| read_pr_identity_artifact(root, &request.run_id))
        .transpose()?
        .flatten();
    let artifact_pr_number = artifact_identity
        .as_ref()
        .and_then(|value| value.get("pr_number").and_then(serde_json::Value::as_u64));
    let artifact_head_sha = artifact_identity
        .as_ref()
        .and_then(|value| string_field(value, "head_sha"));
    let identity = WaitPollIdentity {
        pr_number: artifact_pr_number.or_else(|| metadata_pr_number(metadata)),
        head_sha: artifact_head_sha.or_else(|| metadata.and_then(|md| md.head_sha.clone())),
    };
    validate_wait_poll_identity(wait_kind, &identity)?;
    Ok(identity)
}

fn validate_wait_poll_identity(
    wait_kind: WaitKind,
    identity: &WaitPollIdentity,
) -> Result<(), String> {
    match wait_kind {
        WaitKind::PrChecks => {
            if identity.pr_number.is_none() || identity.head_sha.is_none() {
                return Err("missing PR number or head SHA for PR checks wait state".to_string());
            }
        }
        WaitKind::CoderabbitReview
        | WaitKind::HumanReview
        | WaitKind::PrMerge
        | WaitKind::DependencyChildMerge => {
            if identity.pr_number.is_none() {
                return Err(format!("missing PR number for {wait_kind} wait state"));
            }
        }
        WaitKind::RateLimitBackoff => {}
    }
    Ok(())
}

fn metadata_pr_number(metadata: Option<&RunMetadata>) -> Option<u64> {
    metadata
        .and_then(|md| md.pr_number)
        .and_then(|number| u64::try_from(number).ok())
}

fn wait_artifact_root(
    config: &WorkflowConfig,
    metadata: Option<&RunMetadata>,
) -> Result<Option<std::path::PathBuf>, String> {
    let Some(raw) = metadata
        .and_then(|md| md.artifact_root.clone())
        .or_else(|| config.variables.get("artifact_dir").cloned())
    else {
        return Ok(None);
    };
    let path = std::path::PathBuf::from(interpolate_config_variables(&raw, config)?);
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    Ok(Some(path))
}

fn interpolate_config_variables(raw: &str, config: &WorkflowConfig) -> Result<String, String> {
    let mut value = raw.to_string();
    for _ in 0..config.variables.len().max(1) {
        let previous = value.clone();
        for (key, replacement) in &config.variables {
            value = value.replace(&format!("{{{key}}}"), replacement);
        }
        if value == previous {
            return Ok(value);
        }
    }
    if has_unresolved_config_token(&value) {
        Err(format!(
            "unresolved variable interpolation in artifact path: {value}"
        ))
    } else {
        Ok(value)
    }
}

fn has_unresolved_config_token(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'{' {
            index += 1;
            continue;
        }
        let close = match value[index + 1..].find('}') {
            Some(close) => close,
            None => return true,
        };
        let token = &value[index + 1..index + 1 + close];
        if is_config_token_name(token) {
            return true;
        }
        index += close + 2;
    }
    false
}

fn is_config_token_name(token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    for byte in token.bytes() {
        if !is_config_token_byte(byte) {
            return false;
        }
    }
    true
}

const CONFIG_TOKEN_UNDERSCORE: u8 = 95;
const CONFIG_TOKEN_DOT: u8 = 46;

fn is_config_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == CONFIG_TOKEN_UNDERSCORE || byte == CONFIG_TOKEN_DOT
}

fn read_pr_identity_artifact(
    artifact_root: &std::path::Path,
    run_id: &str,
) -> Result<Option<serde_json::Value>, String> {
    let current_root = artifact_root
        .join("pr-followup")
        .join("current")
        .join(run_id);
    if !current_root.exists() {
        return Ok(None);
    }
    let mut matches = Vec::new();
    collect_pr_identity_artifacts(&current_root, run_id, &mut matches)?;
    matches.sort_by(|left, right| left.0.cmp(&right.0));
    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(matches.remove(0).1)),
        _ => Err(format!(
            "multiple PR identity artifacts found for run {run_id}; cannot choose poll identity"
        )),
    }
}

fn collect_pr_identity_artifacts(
    dir: &std::path::Path,
    run_id: &str,
    matches: &mut Vec<(std::path::PathBuf, serde_json::Value)>,
) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            collect_pr_identity_artifacts(&path, run_id, matches)?;
        } else if path.file_name().and_then(|name| name.to_str()) == Some("pr.json") {
            let value = read_json_path(&path)?;
            if value.get("run_id").and_then(serde_json::Value::as_str) == Some(run_id)
                && value
                    .get("pr_number")
                    .and_then(serde_json::Value::as_u64)
                    .is_some()
                && value
                    .get("head_sha")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|head| !head.is_empty())
            {
                matches.push((path, value));
            }
        }
    }
    Ok(())
}

fn read_json_path(path: &std::path::Path) -> Result<serde_json::Value, String> {
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&content).map_err(|e| e.to_string())
}

fn string_field(value: &serde_json::Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
}

fn lookup_lease_id(
    conn: &rusqlite::Connection,
    request: &luther_workflow::daemon::launcher::LaunchRequest,
) -> Result<Option<String>, String> {
    luther_workflow::persistence::leases::get_lease_for_issue(
        conn,
        &request.repo,
        request.issue_number,
    )
    .map(|lease| lease.map(|lease| lease.lease_id))
    .map_err(|e| e.to_string())
}
fn resolved_wait_step_parameters(config: &WorkflowConfig, step_id: &str) -> Result<Value, String> {
    let config_root = std::path::PathBuf::from("config");
    let workflow_type = resolve_workflow_type(&config.workflow_type_id, &config_root)
        .map_err(|e| format!("resolve workflow type for wait state: {e}"))?;
    let step = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == step_id)
        .ok_or_else(|| format!("missing wait step {step_id}"))?;
    resolve_step_parameters(config, step)
}

fn resolve_step_parameters(config: &WorkflowConfig, step: &StepDef) -> Result<Value, String> {
    match step.parameters.clone().unwrap_or(Value::Null) {
        Value::Object(map) => Ok(Value::Object(resolve_parameter_map(config, map)?)),
        Value::Null => Ok(Value::Null),
        other => Ok(resolve_parameter_value(config, other)?),
    }
}

fn resolve_parameter_map(
    config: &WorkflowConfig,
    map: Map<String, Value>,
) -> Result<Map<String, Value>, String> {
    let mut resolved = Map::new();
    for (key, value) in map {
        resolved.insert(key, resolve_parameter_value(config, value)?);
    }
    Ok(resolved)
}

fn resolve_parameter_value(config: &WorkflowConfig, value: Value) -> Result<Value, String> {
    match value {
        Value::String(raw) => Ok(Value::String(interpolate_config_variables(&raw, config)?)),
        Value::Array(items) => items
            .into_iter()
            .map(|item| resolve_parameter_value(config, item))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(map) => resolve_parameter_map(config, map).map(Value::Object),
        other => Ok(other),
    }
}

fn wait_condition_payload(
    step_id: &str,
    reason: &str,
    request: &luther_workflow::daemon::launcher::LaunchRequest,
    wait_kind: WaitKind,
    step_params: &Value,
) -> Result<Value, String> {
    let mut payload = serde_json::json!({
        "step_id": step_id,
        "reason": reason,
        "repository": request.repo,
        "issue_number": request.issue_number,
    });
    match wait_kind {
        WaitKind::PrChecks => add_required_pr_check_wait_parameters(&mut payload, step_params)?,
        _ => add_optional_wait_parameters(&mut payload, step_params),
    }
    Ok(payload)
}

fn add_required_pr_check_wait_parameters(
    payload: &mut Value,
    step_params: &Value,
) -> Result<(), String> {
    set_required_wait_parameter(payload, step_params, "artifact_root")?;
    set_optional_wait_parameter(payload, step_params, "check_policy");
    set_optional_wait_parameter(payload, step_params, "pr_check_policy");
    set_required_wait_parameter(payload, step_params, "head_ref")?;
    set_required_wait_parameter(payload, step_params, "base_ref")?;
    set_required_wait_parameter(payload, step_params, "base_sha")?;
    Ok(())
}

fn add_optional_wait_parameters(payload: &mut Value, step_params: &Value) {
    for key in [
        "artifact_root",
        "check_policy",
        "pr_check_policy",
        "head_ref",
        "base_ref",
        "base_sha",
    ] {
        set_optional_wait_parameter(payload, step_params, key);
    }
}

fn set_required_wait_parameter(
    payload: &mut Value,
    step_params: &Value,
    key: &str,
) -> Result<(), String> {
    let value = step_params
        .get(key)
        .filter(|value| !value.is_null())
        .cloned()
        .ok_or_else(|| format!("missing resolved PR check wait parameter {key}"))?;
    if value.as_str().is_some_and(has_unresolved_config_token) {
        return Err(format!("unresolved PR check wait parameter {key}: {value}"));
    }
    payload[key] = value;
    Ok(())
}

fn set_optional_wait_parameter(payload: &mut Value, step_params: &Value, key: &str) {
    payload[key] = step_params.get(key).cloned().unwrap_or(Value::Null);
}

