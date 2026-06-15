/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
/// Main entry point for the luther-workflow CLI.
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
use luther_workflow::persistence::init_database;
use luther_workflow::persistence::leases::{
    list_all_leases, list_leases_by_config, list_leases_by_status, IssueLease, LeaseStatus,
};
use luther_workflow::service::{Service, ServiceConfig};
use luther_workflow::workflow::config_loader::{
    resolve_discovery_config, resolve_workflow, resolve_workflow_config, resolve_workflow_type,
    validate_artifact_dependencies, validate_workflow_tokens,
};
use luther_workflow::workflow::schema::{WorkflowConfig, WorkflowType};
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
// Pre-existing CLI orchestration flow; split in a dedicated refactor stage.
#[allow(clippy::too_many_lines)]
async fn handle_run_command(args: &luther_workflow::cli::RunArgs) {
    // 1. Determine config root directory (production or custom)
    let config_root = args
        .config_dir
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from("config"));

    let (workflow_type, mut config, run_ref) = if let Some(config_path) = &args.config {
        // Load from specified path
        let config_id = config_path.file_stem().map_or_else(
            || "default".to_string(),
            |s| s.to_string_lossy().to_string(),
        );

        let workflow_type_id = args
            .workflow_type
            .clone()
            .unwrap_or_else(|| "test-workflow".to_string());

        let workflow_type = match resolve_workflow_type(&workflow_type_id, &config_root) {
            Ok(wt) => wt,
            Err(e) => {
                eprintln!("Error: Failed to resolve workflow type '{workflow_type_id}': {e}");
                process::exit(1);
            }
        };

        let config = match resolve_workflow_config(&config_id, &config_root) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("Error: Failed to resolve config '{config_id}': {e}");
                process::exit(1);
            }
        };

        let run_ref = luther_workflow::workflow::schema::WorkflowRunRef::new(
            &workflow_type_id,
            &config_id,
            args.run_id
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        );
        (workflow_type, config, run_ref)
    } else {
        // Use default: test-workflow with test-config
        let workflow_type_id = args
            .workflow_type
            .clone()
            .unwrap_or_else(|| "test-workflow".to_string());
        let config_id = "test-config".to_string();
        let run_id = args
            .run_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        match resolve_workflow(&workflow_type_id, &config_id, &run_id, &config_root) {
            Ok((wt, cfg, rr)) => (wt, cfg, rr),
            Err(e) => {
                eprintln!("Error: Failed to resolve workflow: {e}");
                process::exit(1);
            }
        }
    };

    let overrides = TargetProfileOverrides {
        repo: args.repo.clone(),
        issue: args.issue.clone(),
        work_dir: args.work_dir.clone(),
        artifact_dir: args.artifact_dir.clone(),
    };
    if let Err(e) = apply_target_profile_overrides(&mut config, &overrides) {
        eprintln!("Error: {e}");
        process::exit(1);
    }
    if target_profile_validation_required(&workflow_type.workflow_type_id, &config, &overrides) {
        if let Err(e) = validate_target_profile(&config) {
            eprintln!("Error: {e}");
            process::exit(1);
        }
    }

    // 2. Create run_id (already done in run_ref)
    let run_id = run_ref.run_id;
    println!("Starting workflow run: {run_id}");
    println!("  Workflow type: {}", workflow_type.workflow_type_id);
    println!("  Config: {}", config.config_id);

    // 2b. GitHub `gh` readiness preflight — runs before any state (DB, work_dir,
    // artifacts) is created so a missing/unauthenticated/under-scoped `gh`
    // aborts cleanly with actionable diagnostics instead of corrupting state.
    // Skipped under --dry-run, --skip-preflight, or for workflows that never
    // shell out to `gh`.
    if !args.dry_run && !args.skip_preflight && workflow_requires_github(&workflow_type) {
        let repo = config
            .variables
            .get("target_repo")
            .cloned()
            .or_else(|| args.repo.clone());
        if let Some(repo) = repo {
            let runner = SystemGithubCommandRunner;
            match run_preflight(&runner, &repo, &["repo"]) {
                Ok(report) => {
                    println!(
                        "  GitHub preflight OK: repo {} (scopes: {})",
                        report.repo,
                        report.scopes.join(", ")
                    );
                }
                Err(e) => fail_preflight(&e),
            }
        }
    }

    // 2c. llxprt agent binary readiness preflight — mirrors the `gh` gate above
    // and runs before any state is created so a missing/incompatible llxprt
    // binary aborts cleanly with actionable diagnostics. Skipped under
    // --dry-run, --skip-preflight, or for workflows that never spawn llxprt.
    if !args.dry_run && !args.skip_preflight && workflow_requires_llxprt(&workflow_type) {
        let runner = SystemLlxprtCommandRunner;
        match run_llxprt_preflight(&runner, &workflow_type, &config.variables) {
            Ok(paths) => {
                println!("  llxprt preflight OK: validated {}", paths.join(", "));
            }
            Err(e) => fail_llxprt_preflight(&e),
        }
    }

    // 3. Initialize checkpoint database
    let db_path = luther_workflow::runtime_paths::get_data_dir().join("checkpoints.db");
    if let Err(e) = init_database(&db_path) {
        eprintln!("Warning: Failed to initialize checkpoint database: {e}");
    }

    if args.dry_run {
        println!("Dry run mode - workflow would execute the following steps:");
        for step in &workflow_type.steps {
            println!(
                "  - {} ({}): {:?}",
                step.step_id,
                step.step_type,
                step.description.as_deref().unwrap_or("No description")
            );
        }
        let had_errors = report_dry_run_validation(&workflow_type, &config);
        if had_errors {
            eprintln!("\nDry run found validation errors. No changes made.");
            process::exit(1);
        }
        println!("\nDry run complete. No changes made.");
        process::exit(0);
    }

    // 4. Create durable EngineRunner with default registry and persistent checkpoints.
    let mut runner = create_durable_runner(workflow_type, config, &run_id, &db_path);
    install_interrupt_handlers(runner.interrupt_handle());

    // 5. Execute workflow
    println!("Executing workflow...");
    match runner.run() {
        Ok(outcome) => {
            // 6. Report results
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
                    process::exit(130); // 128 + SIGINT (2)
                }
            }
        }
        Err(e) => {
            eprintln!("\nWorkflow execution error: {e}");
            process::exit(1);
        }
    }
}

/// Handle the status command.
fn create_durable_runner(
    workflow_type: luther_workflow::workflow::schema::WorkflowType,
    config: luther_workflow::workflow::schema::WorkflowConfig,
    run_id: &str,
    db_path: &std::path::Path,
) -> EngineRunner {
    let run_context = build_run_context(&config, run_id);
    let instance = WorkflowInstance::create_with_run_id(workflow_type, config, run_id);
    let registry = ExecutorRegistry::with_defaults();
    match EngineRunner::with_db_path(instance, registry, db_path) {
        Ok(runner) => runner.with_run_context(run_context),
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
    let workspace_path = vars.get("work_dir").cloned().or_else(|| {
        Some(
            luther_workflow::runtime_paths::get_run_dir(run_id)
                .to_string_lossy()
                .to_string(),
        )
    });
    let log_path = Some(
        luther_workflow::runtime_paths::get_log_dir()
            .join(format!("{run_id}.log"))
            .to_string_lossy()
            .to_string(),
    );
    let artifact_root = vars.get("artifact_dir").cloned().or_else(|| {
        Some(
            luther_workflow::runtime_paths::get_artifacts_root()
                .to_string_lossy()
                .to_string(),
        )
    });
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
struct DaemonWorkflowLauncher {
    config_id: String,
}

impl DaemonWorkflowLauncher {
    fn new(config_id: String) -> Self {
        Self { config_id }
    }
}

impl luther_workflow::daemon::launcher::WorkflowLauncher for DaemonWorkflowLauncher {
    fn launch(
        &self,
        request: &luther_workflow::daemon::launcher::LaunchRequest,
    ) -> Result<bool, String> {
        let config_root = std::path::PathBuf::from("config");
        let mut config = resolve_workflow_config(&self.config_id, &config_root)
            .map_err(|e| format!("resolve config '{}': {e}", self.config_id))?;
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
        let mut runner = create_durable_runner(workflow_type, config, &request.run_id, &db_path);
        match runner.run() {
            Ok(RunOutcome::Success) => Ok(true),
            Ok(_) => Ok(false),
            Err(e) => Err(format!("run error: {e}")),
        }
    }
}

fn install_interrupt_handlers(interrupted: std::sync::Arc<std::sync::atomic::AtomicBool>) {
    let sigint_flag = interrupted.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            sigint_flag.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    });

    #[cfg(unix)]
    {
        let sigterm_flag = interrupted;
        tokio::spawn(async move {
            if let Ok(mut stream) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                stream.recv().await;
                sigterm_flag.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        });
    }
}

/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
async fn handle_status_command(args: &luther_workflow::cli::StatusArgs) {
    // 1. Read all heartbeat files from data dir
    let heartbeats = match read_all_heartbeats().await {
        Ok(hbs) => hbs,
        Err(e) => {
            eprintln!("Error reading heartbeats: {e}");
            std::collections::HashMap::new()
        }
    };

    // 1b. Read the persistent run registry so in-flight and historical runs are
    // visible without parsing the whole log.
    // @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    let runs = read_run_registry(args.run_id.as_deref());

    // 2. Display monitor state
    if args.json {
        // JSON output
        let runs_json: Vec<_> = runs.iter().map(run_metadata_to_json).collect();
        let status = serde_json::json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "heartbeats": heartbeats,
            "runs": runs_json,
        });
        println!("{}", serde_json::to_string_pretty(&status).unwrap());
    } else {
        // Human-readable output
        println!("Luther Workflow Monitor Status");
        println!("==============================");
        println!("Timestamp: {}", chrono::Utc::now().to_rfc3339());
        println!();

        if heartbeats.is_empty() {
            println!("No active runs found.");
            println!("  Status: No heartbeats detected");
        } else {
            println!("Active/Recent Runs:");
            for (run_id, hb) in &heartbeats {
                let state_str = match hb.state {
                    MonitorState::Starting => "starting",
                    MonitorState::Running => "running",
                    MonitorState::Degraded => "degraded",
                    MonitorState::Stopping => "stopping",
                    MonitorState::Stopped => "stopped",
                    MonitorState::Error => "error",
                };
                println!("  Run ID: {run_id}");
                println!("    State: {state_str}");
                println!("    Instance: {}", hb.instance_id);
                println!("    Uptime: {} seconds", hb.uptime_secs);
                println!(
                    "    Last heartbeat: {}",
                    chrono::DateTime::from_timestamp(hb.timestamp, 0)
                        .map_or_else(|| "unknown".to_string(), |dt| dt.to_rfc3339())
                );
                if hb.active_workers > 0 {
                    println!("    Active workers: {}", hb.active_workers);
                }
                println!();
            }
        }

        // Show current run if specified
        if let Some(run_id) = &args.run_id {
            if let Some(hb) = heartbeats.get(run_id) {
                println!("Details for run '{run_id}':");
                println!("  State: {:?}", hb.state);
                println!("  Active workers: {}", hb.active_workers);
            } else {
                println!("No heartbeat found for run '{run_id}'");
            }
        }

        // Persistent run registry section.
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P05
        print_run_registry(&runs);
    }
}

/// Read run records from the persistent registry (checkpoints.db).
/// When `run_id` is provided, returns just that run (if found).
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
fn read_run_registry(run_id: Option<&str>) -> Vec<luther_workflow::persistence::RunMetadata> {
    let db_path = luther_workflow::runtime_paths::get_data_dir().join("checkpoints.db");
    if !db_path.exists() {
        return Vec::new();
    }
    let store = match luther_workflow::persistence::SqliteStore::open(&db_path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    match run_id {
        Some(id) => store
            .get_run(id)
            .ok()
            .flatten()
            .map(|r| vec![r])
            .unwrap_or_default(),
        None => store.list_runs().unwrap_or_default(),
    }
}

/// Render a single run's PID liveness as a human-readable string.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
fn pid_liveness_label(md: &luther_workflow::persistence::RunMetadata) -> String {
    match md.process_pid {
        Some(pid) => {
            let state = if md.is_process_stale() {
                "stale"
            } else {
                "alive"
            };
            format!("{pid} ({state})")
        }
        None => "unknown".to_string(),
    }
}

/// Describe the next-step candidates for status output.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
fn next_step_label(md: &luther_workflow::persistence::RunMetadata) -> String {
    if md.next_step_candidates.is_empty() {
        if md.status.is_terminal() {
            "none (run is terminal)".to_string()
        } else {
            "unknown until current step completes".to_string()
        }
    } else {
        md.next_step_candidates.join(", ")
    }
}

/// Convert a run record into a JSON object for `--json` status output.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
fn run_metadata_to_json(md: &luther_workflow::persistence::RunMetadata) -> serde_json::Value {
    serde_json::json!({
        "run_id": md.run_id,
        "status": md.status.to_string(),
        "current_step": md.current_step,
        "previous_step": md.previous_step,
        "previous_outcome": md.previous_outcome,
        "next_step_candidates": md.next_step_candidates,
        "log_path": md.log_path,
        "artifact_root": md.artifact_root,
        "workspace_path": md.workspace_path,
        "repository": md.repository,
        "issue_number": md.issue_number,
        "pr_number": md.pr_number,
        "head_sha": md.head_sha,
        "process_pid": md.process_pid,
        "process_stale": md.is_process_stale(),
        "child_pids": md.child_pids,
        "stale_child_pids": md.are_child_pids_stale(),
    })
}

/// Print the persistent run registry section for human-readable status.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
fn print_run_registry(runs: &[luther_workflow::persistence::RunMetadata]) {
    println!();
    println!("Persistent Run Registry:");
    if runs.is_empty() {
        println!("  No runs recorded.");
        return;
    }
    for md in runs {
        println!("  Run ID: {}", md.run_id);
        println!("    Status: {}", md.status);
        println!(
            "    Current step: {}",
            md.current_step.as_deref().unwrap_or("(none)")
        );
        println!(
            "    Previous: {} -> {}",
            md.previous_step.as_deref().unwrap_or("(none)"),
            md.previous_outcome.as_deref().unwrap_or("(none)")
        );
        println!("    Next step: {}", next_step_label(md));
        println!("    Log: {}", md.log_path.as_deref().unwrap_or("(none)"));
        println!(
            "    Artifacts: {}",
            md.artifact_root.as_deref().unwrap_or("(none)")
        );
        println!(
            "    Workspace: {}",
            md.workspace_path.as_deref().unwrap_or("(none)")
        );
        println!(
            "    Repo: {}  Issue: {}  PR: {}",
            md.repository.as_deref().unwrap_or("(none)"),
            md.issue_number
                .map_or_else(|| "(none)".to_string(), |n| n.to_string()),
            md.pr_number
                .map_or_else(|| "(none)".to_string(), |n| n.to_string())
        );
        println!(
            "    Head SHA: {}",
            md.head_sha.as_deref().unwrap_or("(none)")
        );
        println!("    Process PID: {}", pid_liveness_label(md));
        println!();
    }
}

/// Handle the service command by dispatching to the requested subcommand.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
async fn handle_service_command(args: &luther_workflow::cli::ServiceArgs) {
    use luther_workflow::cli::ServiceCommand;

    match &args.command {
        ServiceCommand::Run(run_args) => handle_service_run(run_args).await,
        ServiceCommand::Install(install_args) => handle_service_install(install_args),
        ServiceCommand::Start => handle_service_lifecycle(ServiceLifecycle::Start),
        ServiceCommand::Stop => handle_service_lifecycle(ServiceLifecycle::Stop),
        ServiceCommand::Uninstall => handle_service_lifecycle(ServiceLifecycle::Uninstall),
        ServiceCommand::Status(status_args) => handle_service_status(status_args),
    }
}

/// Build the install spec for the current executable and working directory.
///
/// When `config_override` is provided it is appended to the supervised
/// process's argument list as `--config <path>` so the persisted service
/// definition launches `service run --config <path>`, honoring the
/// `service install --config` flag.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
fn build_service_spec(
    binary_override: Option<std::path::PathBuf>,
    config_override: Option<std::path::PathBuf>,
) -> luther_workflow::service::ServiceSpec {
    let binary = binary_override
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("luther-workflow"));
    let working_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let mut spec = luther_workflow::service::build_install_spec(binary, working_dir);
    if let Some(config_path) = config_override {
        spec = spec
            .with_arg("--config")
            .with_arg(config_path.to_string_lossy().to_string());
    }
    spec
}

/// Run the foreground service process supervised by launchd/systemd.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
async fn handle_service_run(args: &luther_workflow::cli::ServiceRunArgs) {
    let config = ServiceConfig {
        foreground: args.foreground,
        ipc_socket_path: args.socket_path.as_ref().map_or_else(
            || "/tmp/luther.sock".to_string(),
            |p| p.to_string_lossy().to_string(),
        ),
        log_level: "info".to_string(),
    };

    let mode = if config.foreground {
        "foreground"
    } else {
        "supervised"
    };
    println!("Starting service ({mode} mode)...");

    match Service::start(config).await {
        Ok(service) => {
            let instance_id = service
                .get_status()
                .await
                .map(|s| s.instance_id)
                .unwrap_or_default();
            println!("Service started successfully. Instance ID: {instance_id}");
            println!("Press Ctrl+C to stop...");
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
        }
        Err(e) => {
            eprintln!("Failed to start service: {e}");
            process::exit(1);
        }
    }
}

/// Install the platform service (launchd plist / systemd unit).
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
fn handle_service_install(args: &luther_workflow::cli::ServiceInstallArgs) {
    let spec = build_service_spec(args.binary.clone(), args.config.clone());
    match luther_workflow::service::install_service(&spec) {
        Ok(path) => {
            println!("Service installed at: {}", path.display());
            println!("Start it with `luther-workflow service start`.");
        }
        Err(e) => report_service_error(&e),
    }
}

/// Lifecycle operations that share the same dispatch shape.
enum ServiceLifecycle {
    Start,
    Stop,
    Uninstall,
}

/// Start/stop/uninstall the platform service.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
fn handle_service_lifecycle(action: ServiceLifecycle) {
    let spec = build_service_spec(None, None);
    let (result, success) = match action {
        ServiceLifecycle::Start => (
            luther_workflow::service::start_service(&spec),
            "Service started.",
        ),
        ServiceLifecycle::Stop => (
            luther_workflow::service::stop_service(&spec),
            "Service stopped.",
        ),
        ServiceLifecycle::Uninstall => (
            luther_workflow::service::uninstall_service(&spec),
            "Service uninstalled.",
        ),
    };
    match result {
        Ok(()) => println!("{success}"),
        Err(e) => report_service_error(&e),
    }
}

/// Show the platform service status, optionally as JSON.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
fn handle_service_status(args: &luther_workflow::cli::ServiceStatusArgs) {
    let spec = build_service_spec(None, None);
    match luther_workflow::service::get_status(&spec) {
        Ok(status) => {
            if args.json {
                let payload = serde_json::json!({
                    "status": "ok",
                    "detail": status,
                });
                println!("{payload}");
            } else {
                println!("Service status:");
                println!("{status}");
            }
        }
        Err(e) => {
            if args.json {
                report_service_error_json(&e);
            } else {
                report_service_error(&e);
            }
            process::exit(1);
        }
    }
}

/// Print a structured, human-readable error block for service failures.
///
/// Surfaces platform, operation, OS-level message, log location, and
/// remediation steps (REQ-EARS-SVC-004), then exits non-zero.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
fn report_service_error(err: &luther_workflow::service::ServiceManagerError) {
    eprintln!("Service operation failed.");
    eprintln!("  Platform: {}", err.platform());
    if let Some(op) = err.operation() {
        eprintln!("  Operation: {op}");
    }
    eprintln!("  Error: {err}");
    if let Some(path) = err.log_path() {
        eprintln!("  Log location: {}", path.display());
    }
    eprintln!("  Remediation steps:");
    for step in err.get_remediation_steps() {
        eprintln!("    - {step}");
    }
    process::exit(1);
}

/// Emit the same service-error fields as a JSON object for `--json` consumers.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
fn report_service_error_json(err: &luther_workflow::service::ServiceManagerError) {
    let operation = err.operation().map(|op| op.to_string());
    let log_path = err.log_path().map(|p| p.display().to_string());
    let payload = serde_json::json!({
        "status": "error",
        "platform": err.platform(),
        "operation": operation,
        "error": err.to_string(),
        "log_path": log_path,
        "remediation": err.get_remediation_steps(),
    });
    println!("{payload}");
}

/// Derive the config id (file stem) from a `--config` path, mirroring
/// `handle_run_command`.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
fn daemon_config_id(config: &std::path::Path) -> String {
    config.file_stem().map_or_else(
        || "default".to_string(),
        |s| s.to_string_lossy().to_string(),
    )
}

/// Dispatch the `daemon` command family.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
async fn handle_daemon_command(args: &luther_workflow::cli::DaemonArgs) {
    use luther_workflow::cli::DaemonCommand;

    let store = DaemonStore::production();
    match &args.command {
        DaemonCommand::Start(start) => {
            println!("Starting daemon in foreground (Ctrl-C to stop)...");
            handle_daemon_run(&store, &start.config, start.force, &start.config_dir, false).await;
        }
        DaemonCommand::Run(run) => {
            handle_daemon_run(&store, &run.config, run.force, &run.config_dir, run.once).await;
        }
        DaemonCommand::Stop(stop) => handle_daemon_stop(&store, stop),
        DaemonCommand::Status(status) => handle_daemon_status(&store, status),
        DaemonCommand::Discover(discover_args) => handle_daemon_discover_command(discover_args),
        DaemonCommand::Queue(queue_args) => handle_daemon_queue_command(queue_args),
    }
}

/// Resolve the discovery config for a `--config` path under `--config-dir`.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P05
fn resolve_discovery_for(
    config: &std::path::Path,
    config_dir: &Option<std::path::PathBuf>,
) -> luther_workflow::workflow::schema::DiscoveryConfig {
    let config_root = config_dir
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from("config"));
    let config_id = daemon_config_id(config);
    let cfg = match resolve_workflow_config(&config_id, &config_root) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("Error: Failed to resolve config '{config_id}': {e}");
            process::exit(1);
        }
    };
    resolve_discovery_config(&cfg)
}

/// Open the shared checkpoints database (creating schema if needed).
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P05
fn open_daemon_db() -> rusqlite::Connection {
    let db_path = luther_workflow::runtime_paths::get_data_dir().join("checkpoints.db");
    if let Err(e) = init_database(&db_path) {
        eprintln!("Error: Failed to initialize database: {e}");
        process::exit(1);
    }
    match rusqlite::Connection::open(&db_path) {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("Error: Failed to open database: {e}");
            process::exit(1);
        }
    }
}

/// Handle `daemon discover`: dry-run issue discovery for a config.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P05
/// @requirement:REQ-DAEMON-DISCOVERY-004
fn handle_daemon_discover_command(args: &luther_workflow::cli::DaemonDiscoverArgs) {
    let discovery = resolve_discovery_for(&args.config, &args.config_dir);
    let config_id = daemon_config_id(&args.config);
    let conn = open_daemon_db();
    let active =
        luther_workflow::persistence::leases::count_active_leases_for_config(&conn, &config_id)
            .unwrap_or(0);
    let query = SystemGithubIssueQuery::new(SystemGithubCommandRunner);
    let result = match discover(&discovery, &query, &conn, active) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: discovery failed: {e}");
            process::exit(1);
        }
    };
    if args.json {
        print_discovery_json(&result);
    } else {
        print_discovery_text(&result);
    }
}

/// Print discovery results as JSON.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P05
fn print_discovery_json(result: &DiscoveryResult) {
    let eligible: Vec<serde_json::Value> = result
        .eligible
        .iter()
        .map(|i| {
            serde_json::json!({
                "number": i.number,
                "title": i.title,
                "labels": i.labels,
            })
        })
        .collect();
    let skipped: Vec<serde_json::Value> = result
        .skipped
        .iter()
        .map(|(i, reason)| {
            serde_json::json!({
                "number": i.number,
                "title": i.title,
                "reason": reason.code(),
                "detail": reason.to_string(),
            })
        })
        .collect();
    let payload = serde_json::json!({ "eligible": eligible, "skipped": skipped });
    println!("{}", serde_json::to_string_pretty(&payload).unwrap());
}

/// Print discovery results in human-readable form.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P05
fn print_discovery_text(result: &DiscoveryResult) {
    println!("Eligible issues ({}):", result.eligible.len());
    for issue in &result.eligible {
        println!(
            "  #{} {} [{}]",
            issue.number,
            issue.title,
            issue.labels.join(", ")
        );
    }
    println!("Skipped issues ({}):", result.skipped.len());
    for (issue, reason) in &result.skipped {
        println!("  #{} {} — {}", issue.number, issue.title, reason);
    }
}

/// Handle `daemon queue`: list issue leases grouped by status.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P05
/// @requirement:REQ-DAEMON-DISCOVERY-002
fn handle_daemon_queue_command(args: &luther_workflow::cli::DaemonQueueArgs) {
    let conn = open_daemon_db();
    let leases = collect_queue_leases(&conn, args);
    if args.json {
        print_queue_json(&leases);
    } else {
        print_queue_text(&leases);
    }
}

/// Collect leases for the queue command honoring `--config` and `--status`.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P05
fn collect_queue_leases(
    conn: &rusqlite::Connection,
    args: &luther_workflow::cli::DaemonQueueArgs,
) -> Vec<IssueLease> {
    let base = if let Some(config) = &args.config {
        let config_id = daemon_config_id(config);
        list_leases_by_config(conn, &config_id).unwrap_or_default()
    } else if let Some(status) = &args.status {
        match status.parse::<LeaseStatus>() {
            Ok(s) => return list_leases_by_status(conn, s).unwrap_or_default(),
            Err(e) => {
                eprintln!("Error: invalid --status: {e}");
                process::exit(1);
            }
        }
    } else {
        list_all_leases(conn).unwrap_or_default()
    };
    if let Some(status) = &args.status {
        match status.parse::<LeaseStatus>() {
            Ok(s) => base.into_iter().filter(|l| l.status == s).collect(),
            Err(e) => {
                eprintln!("Error: invalid --status: {e}");
                process::exit(1);
            }
        }
    } else {
        base
    }
}

/// Print the lease queue as JSON.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P05
fn print_queue_json(leases: &[IssueLease]) {
    let items: Vec<serde_json::Value> = leases
        .iter()
        .map(|l| {
            serde_json::json!({
                "issue_repo": l.issue_repo,
                "issue_number": l.issue_number,
                "config_id": l.config_id,
                "run_id": l.run_id,
                "status": l.status.to_string(),
            })
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&items).unwrap());
}

/// Print the lease queue grouped by status.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P05
fn print_queue_text(leases: &[IssueLease]) {
    if leases.is_empty() {
        println!("Queue is empty.");
        return;
    }
    for status in [
        LeaseStatus::Pending,
        LeaseStatus::Claimed,
        LeaseStatus::Running,
        LeaseStatus::Completed,
        LeaseStatus::Failed,
        LeaseStatus::Abandoned,
        LeaseStatus::Stale,
    ] {
        let group: Vec<&IssueLease> = leases.iter().filter(|l| l.status == status).collect();
        if group.is_empty() {
            continue;
        }
        println!("{} ({}):", status, group.len());
        for lease in group {
            let run = lease.run_id.as_deref().unwrap_or("-");
            println!(
                "  {}#{} config={} run={}",
                lease.issue_repo, lease.issue_number, lease.config_id, run
            );
        }
    }
}

/// Acquire the per-config singleton lock, honoring `--force` recovery.
///
/// Returns the held guard on success, or `None` after printing a clear error
/// when another live daemon owns the lock.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
fn acquire_daemon_lock(
    store: &DaemonStore,
    config_id: &str,
    force: bool,
) -> Option<luther_workflow::monitor::SingletonGuard> {
    use luther_workflow::monitor::{acquire_singleton_lock, process::MonitorError};

    let lock_path = store.lock_path(config_id).to_string_lossy().to_string();
    match acquire_singleton_lock(&lock_path) {
        Ok(guard) => Some(guard),
        Err(MonitorError::LockHeld { pid }) => {
            if force {
                let _ = std::fs::remove_file(&lock_path);
                match acquire_singleton_lock(&lock_path) {
                    Ok(guard) => Some(guard),
                    Err(e) => {
                        eprintln!("Error: failed to replace daemon lock for '{config_id}': {e}");
                        None
                    }
                }
            } else {
                eprintln!(
                    "Error: daemon already running (config={config_id}, pid={pid}). \
                     Use --force to replace it."
                );
                None
            }
        }
        Err(e) => {
            eprintln!("Error: failed to acquire daemon lock for '{config_id}': {e}");
            None
        }
    }
}

/// Run a foreground daemon for the given config with clean Ctrl-C handling.
///
/// When the resolved `[discovery]` config is enabled, the daemon drives the
/// discovery/launch scheduler (still writing heartbeats); otherwise it keeps
/// the original heartbeat-only behavior. `once` performs a single pass.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P06
async fn handle_daemon_run(
    store: &DaemonStore,
    config: &std::path::Path,
    force: bool,
    config_dir: &Option<std::path::PathBuf>,
    once: bool,
) {
    let config_id = daemon_config_id(config);

    let _guard = match acquire_daemon_lock(store, &config_id, force) {
        Some(guard) => guard,
        None => process::exit(1),
    };

    let mut state = DaemonState::new(&config_id);
    if let Err(e) = store.write(&state) {
        eprintln!("Error: failed to persist daemon state: {e}");
        process::exit(1);
    }
    state.set_status(DaemonStatus::Running);
    let _ = store.write(&state);

    println!("Daemon running (config={config_id}, pid={}).", state.pid);

    let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    install_interrupt_handlers(shutdown.clone());

    let discovery = resolve_discovery_for(config, config_dir);
    if discovery.enabled {
        run_daemon_discovery_loop(store, &mut state, &shutdown, &discovery, &config_id, once).await;
    } else {
        run_daemon_heartbeat_loop(store, &mut state, &shutdown).await;
    }

    state.set_status(DaemonStatus::Stopping);
    let _ = store.write(&state);
    state.set_status(DaemonStatus::Stopped);
    let _ = store.write(&state);
    println!("Daemon stopped (config={config_id}).");
}

/// Drive the discovery/launch scheduler, writing heartbeats between passes.
///
/// Runs in a blocking task because the scheduler and SQLite access are
/// synchronous; the heartbeat is refreshed on the async side between passes.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P06
async fn run_daemon_discovery_loop(
    store: &DaemonStore,
    state: &mut DaemonState,
    shutdown: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    discovery: &luther_workflow::workflow::schema::DiscoveryConfig,
    config_id: &str,
    once: bool,
) {
    use std::sync::atomic::Ordering;

    let conn = open_daemon_db();
    let query = SystemGithubIssueQuery::new(SystemGithubCommandRunner);
    let launcher = DaemonWorkflowLauncher::new(config_id.to_string());
    let stale_timeout = discovery
        .poll_interval_secs
        .unwrap_or(300)
        .saturating_mul(4);

    if let Ok(recovered) =
        luther_workflow::persistence::leases::mark_stale_leases(&conn, stale_timeout)
    {
        if recovered > 0 {
            println!("recovered {recovered} stale lease(s) on startup");
        }
    }

    let poll = discovery.poll_interval_secs.unwrap_or(300);
    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        match luther_workflow::daemon::scheduler::run_once(
            discovery, &query, &conn, &launcher, config_id,
        ) {
            Ok(summary) if summary.launched > 0 || summary.failed > 0 => {
                println!(
                    "scheduler pass: {} launched, {} failed, {} skipped",
                    summary.launched, summary.failed, summary.skipped
                );
            }
            Ok(_) => {}
            Err(e) => eprintln!("scheduler error: {e}"),
        }
        state.touch_heartbeat();
        let _ = store.write(state);
        if once {
            break;
        }
        sleep_secs_with_shutdown(poll, shutdown).await;
    }
}

/// Async sleep up to `secs` that wakes early when shutdown is requested.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P06
async fn sleep_secs_with_shutdown(
    secs: u64,
    shutdown: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use std::sync::atomic::Ordering;
    let ticks = secs.saturating_mul(5);
    for _ in 0..ticks {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
    }
}

/// Refresh the heartbeat until the shutdown flag is set.
///
/// Uses a short tick so Ctrl-C is responsive while only writing the heartbeat
/// roughly every 30 seconds.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
async fn run_daemon_heartbeat_loop(
    store: &DaemonStore,
    state: &mut DaemonState,
    shutdown: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use std::sync::atomic::Ordering;

    let mut ticks: u32 = 0;
    while !shutdown.load(Ordering::SeqCst) {
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        ticks += 1;
        if ticks >= 150 {
            ticks = 0;
            state.touch_heartbeat();
            let _ = store.write(state);
        }
    }
}

/// Handle `daemon stop` for a single config or `--all`.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
fn handle_daemon_stop(store: &DaemonStore, args: &luther_workflow::cli::DaemonStopArgs) {
    if args.all {
        stop_all_daemons(store);
        return;
    }
    let Some(config) = &args.config else {
        eprintln!("Error: daemon stop requires --config <PATH> or --all.");
        process::exit(1);
    };
    let config_id = daemon_config_id(config);
    report_stop_outcome(&config_id, stop_daemon(store, &config_id));
}

/// Print a human-readable summary for a single stop outcome.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
fn report_stop_outcome(config_id: &str, outcome: StopOutcome) {
    match outcome {
        StopOutcome::Stopped => println!("Stopped daemon (config={config_id})."),
        StopOutcome::AlreadyStopped => {
            println!("Daemon already stopped (config={config_id}).");
        }
        StopOutcome::NotFound => println!("No daemon found (config={config_id})."),
    }
}

/// Stop every known daemon instance, continuing past individual failures.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
fn stop_all_daemons(store: &DaemonStore) {
    let states = store.read_all();
    if states.is_empty() {
        println!("No daemons found.");
        return;
    }
    let mut stopped = 0u32;
    let mut already = 0u32;
    for state in &states {
        match stop_daemon(store, &state.config_id) {
            StopOutcome::Stopped => stopped += 1,
            StopOutcome::AlreadyStopped | StopOutcome::NotFound => already += 1,
        }
    }
    println!("{stopped} stopped, {already} already stopped.");
}

/// Handle `daemon status` for a single config or the aggregate view.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
fn handle_daemon_status(store: &DaemonStore, args: &luther_workflow::cli::DaemonStatusArgs) {
    match &args.config {
        Some(config) => {
            let config_id = daemon_config_id(config);
            daemon_status_single(store, &config_id, args.json);
        }
        None => daemon_status_all(store, args.json),
    }
}

/// Build a JSON value describing one daemon state, including liveness.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
fn daemon_state_json(state: &DaemonState) -> serde_json::Value {
    let now = chrono::Utc::now().timestamp();
    serde_json::json!({
        "config_id": state.config_id,
        "pid": state.pid,
        "status": state.status.to_string(),
        "start_timestamp": state.start_timestamp,
        "heartbeat_timestamp": state.heartbeat_timestamp,
        "uptime_secs": state.uptime_secs(now),
        "alive": is_daemon_alive(state.pid),
    })
}

/// Render detailed status for a single config.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
fn daemon_status_single(store: &DaemonStore, config_id: &str, json: bool) {
    let Some(state) = store.read(config_id) else {
        if json {
            println!(
                "{}",
                serde_json::json!({ "config_id": config_id, "found": false })
            );
        } else {
            println!("No daemon found (config={config_id}).");
        }
        return;
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&daemon_state_json(&state)).unwrap_or_default()
        );
        return;
    }
    let now = chrono::Utc::now().timestamp();
    let alive = is_daemon_alive(state.pid);
    println!("Daemon status (config={config_id})");
    println!("  PID: {}", state.pid);
    println!("  Status: {}", daemon_display_status(&state, alive));
    println!("  Uptime: {}s", state.uptime_secs(now));
    println!("  Last heartbeat: {}", state.heartbeat_timestamp);
}

/// Compute the displayed status token, marking running-but-dead as `stale`.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
fn daemon_display_status(state: &DaemonState, alive: bool) -> String {
    if state.status == DaemonStatus::Running && !alive {
        "stale".to_string()
    } else {
        state.status.to_string()
    }
}

/// Render the aggregate status across all known daemons.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
fn daemon_status_all(store: &DaemonStore, json: bool) {
    let states = store.read_all();
    if json {
        let array: Vec<serde_json::Value> = states.iter().map(daemon_state_json).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!(array)).unwrap_or_default()
        );
        return;
    }
    if states.is_empty() {
        println!("No daemons found.");
        return;
    }
    println!(
        "{:<24} {:>8}  {:<10} {:>10}",
        "CONFIG", "PID", "STATUS", "UPTIME"
    );
    let now = chrono::Utc::now().timestamp();
    for state in &states {
        let alive = is_daemon_alive(state.pid);
        println!(
            "{:<24} {:>8}  {:<10} {:>9}s",
            state.config_id,
            state.pid,
            daemon_display_status(state, alive),
            state.uptime_secs(now)
        );
    }
}
