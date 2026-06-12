/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
/// Main entry point for the luther-workflow CLI.
use std::process;

use tracing_subscriber::{fmt, EnvFilter};

use luther_workflow::adapters::github::{run_preflight, GithubError, SystemGithubCommandRunner};
use luther_workflow::adapters::llxprt::{
    run_preflight as run_llxprt_preflight, LlxprtError, SystemLlxprtCommandRunner,
};
use luther_workflow::cli::{parse_args, Commands};
use luther_workflow::engine::executor::ExecutorRegistry;
use luther_workflow::engine::instance::WorkflowInstance;
use luther_workflow::engine::runner::{EngineRunner, RunOutcome};
use luther_workflow::monitor::heartbeat::read_all_heartbeats;
use luther_workflow::monitor::heartbeat::MonitorState;
use luther_workflow::persistence::init_database;
use luther_workflow::service::{Service, ServiceConfig};
use luther_workflow::workflow::config_loader::{
    resolve_workflow, resolve_workflow_config, resolve_workflow_type,
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
    let instance = WorkflowInstance::create_with_run_id(workflow_type, config, run_id);
    let registry = ExecutorRegistry::with_defaults();
    match EngineRunner::with_db_path(instance, registry, db_path) {
        Ok(runner) => runner,
        Err(e) => {
            eprintln!("Error: Failed to create durable engine runner: {e}");
            process::exit(1);
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

    // 2. Display monitor state
    if args.json {
        // JSON output
        let status = serde_json::json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "heartbeats": heartbeats,
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
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
fn build_service_spec(
    binary_override: Option<std::path::PathBuf>,
) -> luther_workflow::service::ServiceSpec {
    let binary = binary_override
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("luther-workflow"));
    let working_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    luther_workflow::service::build_install_spec(binary, working_dir)
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
    let spec = build_service_spec(args.binary.clone());
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
    let spec = build_service_spec(None);
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
    let spec = build_service_spec(None);
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
