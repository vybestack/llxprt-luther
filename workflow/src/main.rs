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
use luther_workflow::monitor::snapshot::{
    render_snapshot, resolve_snapshot_count, separator_line, DaemonSummary, MonitorFilter,
    MonitorSnapshot, RunCounts, CLEAR_SCREEN,
};
use luther_workflow::persistence::init_database;
use luther_workflow::persistence::leases::{
    list_all_leases, list_leases_by_config, list_leases_by_status, IssueLease, LeaseStatus,
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
                RunOutcome::WaitingExternal { step_id, reason } => {
                    println!("\nWorkflow paused at step '{step_id}' awaiting external state");
                    println!("Reason: {reason}");
                    println!("Run ID: {run_id}");
                    println!("Resume with: luther-workflow runs resume {run_id}");
                    process::exit(0);
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
    _config_id: String,
}

impl DaemonWorkflowLauncher {
    fn new(config_id: String) -> Self {
        Self {
            _config_id: config_id,
        }
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
    let config_root = std::path::PathBuf::from("config");
    let wait_config = resolve_workflow_config(&metadata.config_id, &config_root)
        .map_err(|e| format!("resolve config '{}': {e}", metadata.config_id))?;
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
    let mut runner = reconstruct_runner(&metadata, &request.run_id, &db_path)?;
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
    record.wait_condition = serde_json::json!({
        "step_id": step_id,
        "reason": reason,
        "repository": request.repo,
        "issue_number": request.issue_number
    });
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
    let path = std::path::PathBuf::from(interpolate_config_variables(&raw, config));
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    Ok(Some(path))
}

fn interpolate_config_variables(raw: &str, config: &WorkflowConfig) -> String {
    let mut value = raw.to_string();
    for (key, replacement) in &config.variables {
        value = value.replace(&format!("{{{key}}}"), replacement);
    }
    value
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

fn wait_kind_for_step(step_id: &str) -> WaitKind {
    match step_id {
        "watch_pr_checks" => WaitKind::PrChecks,
        "collect_coderabbit_feedback" => WaitKind::CoderabbitReview,
        "merge_pr" | "wait_for_merge" => WaitKind::PrMerge,
        _ => WaitKind::HumanReview,
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
    // visible without parsing the whole log. Registry open/query failures are
    // surfaced distinctly from a legitimately empty registry rather than being
    // silently swallowed into an empty list.
    // @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    let runs_result = read_run_registry(args.run_id.as_deref());

    // 1c. Optional --config filter (issue #51): keep only heartbeats and runs
    // whose config id matches. Daemon-level aggregation across configs already
    // lives in `daemon status`; this filter scopes the workflow-run view.
    let (heartbeats, runs_result) = match args.config.as_deref() {
        Some(config_id) => filter_status_by_config(heartbeats, runs_result, config_id),
        None => (heartbeats, runs_result),
    };

    // 2. Display monitor state
    if args.json {
        // JSON output
        let (runs_json, registry_error): (Vec<_>, Option<String>) = match &runs_result {
            Ok(runs) => (runs.iter().map(run_metadata_to_json).collect(), None),
            Err(e) => (Vec::new(), Some(e.clone())),
        };
        let status = serde_json::json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "heartbeats": heartbeats,
            "runs": runs_json,
            "registry_error": registry_error,
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
        match &runs_result {
            Ok(runs) => print_run_registry(runs, args.run_id.as_deref()),
            Err(e) => {
                eprintln!("Error: run registry unavailable: {e}");
                println!();
                println!("Persistent Run Registry:");
                println!("  Status: registry unavailable ({e})");
            }
        }
    }
}

/// Read run records from the persistent registry (checkpoints.db).
///
/// When `run_id` is provided, returns just that run (if found). A missing
/// database file is treated as a legitimately empty registry (`Ok(vec![])`),
/// but failures to open the store or query it are propagated as `Err` so the
/// caller can distinguish "no runs recorded" from "registry unavailable or
/// corrupt" instead of silently collapsing both into an empty list.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
fn read_run_registry(
    run_id: Option<&str>,
) -> Result<Vec<luther_workflow::persistence::RunMetadata>, String> {
    let db_path = luther_workflow::runtime_paths::get_data_dir().join("checkpoints.db");
    if !db_path.exists() {
        return Ok(Vec::new());
    }
    let store = luther_workflow::persistence::SqliteStore::open(&db_path)
        .map_err(|e| format!("failed to open run registry at {}: {e}", db_path.display()))?;
    match run_id {
        Some(id) => store
            .get_run(id)
            .map(|maybe| maybe.map(|r| vec![r]).unwrap_or_default())
            .map_err(|e| format!("failed to read run '{id}' from registry: {e}")),
        None => store
            .list_runs()
            .map_err(|e| format!("failed to list runs from registry: {e}")),
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
        "config_id": md.config_id,
        "workflow_type_id": md.workflow_type_id,
        "status": md.status.to_string(),
        "created_at": md.created_at.to_rfc3339(),
        "updated_at": md.updated_at.unwrap_or(md.created_at).to_rfc3339(),
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
fn print_run_registry(
    runs: &[luther_workflow::persistence::RunMetadata],
    queried_run_id: Option<&str>,
) {
    println!();
    println!("Persistent Run Registry:");
    if runs.is_empty() {
        // Echo the queried run id so a `--run-id` miss is actionable (issue #53).
        match queried_run_id {
            Some(id) => println!("  No run found with id '{id}'."),
            None => println!("  No runs recorded."),
        }
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
            handle_daemon_run(
                &store,
                &start.config,
                start.force,
                &start.config_dir,
                false,
                &None,
            )
            .await;
        }
        DaemonCommand::Run(run) => {
            handle_daemon_run(
                &store,
                &run.config,
                run.force,
                &run.config_dir,
                run.once,
                &run.scheduler_config,
            )
            .await;
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
    let waits = queue_wait_summaries();
    let items: Vec<serde_json::Value> = leases
        .iter()
        .map(|l| {
            let wait = l.run_id.as_deref().and_then(|run_id| waits.get(run_id));
            serde_json::json!({
                "issue_repo": l.issue_repo,
                "issue_number": l.issue_number,
                "config_id": l.config_id,
                "run_id": l.run_id,
                "status": l.status.to_string(),
                "active_slot_used": l.status.is_active(),
                "wait": wait,
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
    let waits = queue_wait_summaries();
    for status in [
        LeaseStatus::Pending,
        LeaseStatus::Claimed,
        LeaseStatus::Running,
        LeaseStatus::WaitingExternal,
        LeaseStatus::ReadyToResume,
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
            let active_slot = if lease.status.is_active() {
                "yes"
            } else {
                "no"
            };
            let wait = lease
                .run_id
                .as_deref()
                .and_then(|run_id| waits.get(run_id))
                .map(|w| format!(" wait={}", format_wait_summary(w)))
                .unwrap_or_default();
            println!(
                "  {}#{} config={} run={} active_slot_used={}{}",
                lease.issue_repo, lease.issue_number, lease.config_id, run, active_slot, wait
            );
        }
    }
}

fn queue_wait_summaries() -> std::collections::HashMap<String, serde_json::Value> {
    let conn = open_daemon_db();
    let waits = match list_wait_states(&conn) {
        Ok(waits) => waits,
        Err(e) => {
            eprintln!("Warning: failed to load wait states: {e}");
            return std::collections::HashMap::new();
        }
    };
    waits
        .into_iter()
        .map(|wait| {
            (
                wait.run_id.clone(),
                serde_json::json!({
                    "wait_kind": wait.wait_kind.to_string(),
                    "next_poll_at": wait.next_poll_at.to_rfc3339(),
                    "poll_count": wait.poll_count,
                    "last_observed_state": wait.last_observed_state,
                    "resume_step": wait.resume_step,
                    "active_slot_used": false,
                }),
            )
        })
        .collect()
}

fn format_wait_summary(wait: &serde_json::Value) -> String {
    format!(
        "kind={} next_poll_at={} poll_count={} resume_step={}",
        wait.get("wait_kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("-"),
        wait.get("next_poll_at")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("-"),
        wait.get("poll_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default(),
        wait.get("resume_step")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("-"),
    )
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
    scheduler_config: &Option<std::path::PathBuf>,
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
        if let Some(path) = scheduler_config {
            run_daemon_supervisor_loop(store, &mut state, &shutdown, path, config_dir, once).await;
        } else {
            run_daemon_discovery_loop(store, &mut state, &shutdown, &discovery, &config_id, once)
                .await;
        }
    } else {
        run_daemon_heartbeat_loop(store, &mut state, &shutdown).await;
    }

    state.set_status(DaemonStatus::Stopping);
    let _ = store.write(&state);
    state.set_status(DaemonStatus::Stopped);
    let _ = store.write(&state);
    println!("Daemon stopped (config={config_id}).");
}

fn scheduler_targets(
    scheduler: &luther_workflow::workflow::schema::DaemonSchedulerConfig,
    config_dir: &Option<std::path::PathBuf>,
) -> Vec<luther_workflow::daemon::scheduler::SchedulerTarget> {
    let config_root = config_dir
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from("config"));
    scheduler
        .targets
        .iter()
        .filter_map(|target| scheduler_target(target, scheduler, &config_root))
        .collect()
}

fn scheduler_target(
    target: &luther_workflow::workflow::schema::DaemonTargetConfig,
    scheduler: &luther_workflow::workflow::schema::DaemonSchedulerConfig,
    config_root: &std::path::Path,
) -> Option<luther_workflow::daemon::scheduler::SchedulerTarget> {
    let cfg = match resolve_workflow_config(&target.config_id, config_root) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!(
                "Error: Failed to resolve config '{}': {e}",
                target.config_id
            );
            return None;
        }
    };
    let mut discovery = resolve_discovery_config(&cfg);
    discovery.max_concurrent_active_runs = discovery
        .max_concurrent_active_runs
        .or(scheduler.max_concurrent_active_runs);
    discovery.max_concurrent_runs_per_config = discovery
        .max_concurrent_runs_per_config
        .or(scheduler.max_concurrent_runs_per_config);
    discovery.max_concurrent_runs_per_repository = discovery
        .max_concurrent_runs_per_repository
        .or(scheduler.max_concurrent_runs_per_repository);
    if discovery.poll_interval_secs.is_none() {
        discovery.poll_interval_secs = scheduler.poll_interval_seconds;
    }
    discovery
        .enabled
        .then(|| luther_workflow::daemon::scheduler::SchedulerTarget {
            config_id: target.config_id.clone(),
            discovery,
        })
}
fn recover_stale_daemon_leases(conn: &rusqlite::Connection, stale_timeout: u64) {
    let recovered = luther_workflow::persistence::leases::mark_stale_leases(conn, stale_timeout);
    let ready_recovered = luther_workflow::persistence::leases::mark_stale_ready_to_resume_leases(
        conn,
        stale_timeout,
    );
    match (recovered, ready_recovered) {
        (Ok(recovered), Ok(ready_recovered)) => {
            if recovered > 0 || ready_recovered > 0 {
                println!(
                    "recovered {recovered} active stale lease(s) and {ready_recovered} ready-to-resume stale lease(s) on startup"
                );
            }
        }
        (Err(e), _) => eprintln!("Warning: active stale lease recovery failed: {e}"),
        (_, Err(e)) => eprintln!("Warning: ready-to-resume stale lease recovery failed: {e}"),
    }
}

async fn run_daemon_supervisor_loop(
    store: &DaemonStore,
    state: &mut DaemonState,
    shutdown: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    scheduler_path: &std::path::Path,
    config_dir: &Option<std::path::PathBuf>,
    once: bool,
) {
    use std::sync::atomic::Ordering;

    let scheduler = match load_daemon_scheduler_config(scheduler_path) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Error: Failed to load daemon scheduler config: {e}");
            return;
        }
    };
    let targets = scheduler_targets(&scheduler, config_dir);
    let conn = open_daemon_db();
    let queries = targets
        .iter()
        .map(|_| SystemGithubIssueQuery::new(SystemGithubCommandRunner))
        .collect::<Vec<_>>();
    let query_refs = queries
        .iter()
        .map(|query| query as &dyn luther_workflow::adapters::github_issues::GithubIssueQuery)
        .collect::<Vec<_>>();
    let stale_timeout = scheduler
        .poll_interval_seconds
        .unwrap_or(300)
        .saturating_mul(4);
    recover_stale_daemon_leases(&conn, stale_timeout);
    let launcher = DaemonWorkflowLauncher::new("supervisor".to_string());
    let poll = scheduler.poll_interval_seconds.unwrap_or(300);
    while !shutdown.load(Ordering::SeqCst) {
        match luther_workflow::daemon::scheduler::run_multi_target_once(
            &targets,
            &query_refs,
            &conn,
            &launcher,
        ) {
            Ok(summary)
                if summary.launched > 0
                    || summary.resumed > 0
                    || summary.suspended > 0
                    || summary.failed > 0 =>
            {
                println!(
                    "scheduler pass: {} launched, {} resumed, {} suspended, {} failed, {} skipped",
                    summary.launched,
                    summary.resumed,
                    summary.suspended,
                    summary.failed,
                    summary.skipped
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

    recover_stale_daemon_leases(&conn, stale_timeout);

    let poll = discovery.poll_interval_secs.unwrap_or(300);
    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        match luther_workflow::daemon::scheduler::run_once(
            discovery, &query, &conn, &launcher, config_id,
        ) {
            Ok(summary)
                if summary.launched > 0
                    || summary.resumed > 0
                    || summary.suspended > 0
                    || summary.failed > 0 =>
            {
                println!(
                    "scheduler pass: {} launched, {} resumed, {} suspended, {} failed, {} skipped",
                    summary.launched,
                    summary.resumed,
                    summary.suspended,
                    summary.failed,
                    summary.skipped
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

/// Resolve the config id for a heartbeat by looking up its run in the registry.
///
/// Returns `None` when the heartbeat has no run id or the run is not recorded.
/// @plan:issue-51
fn heartbeat_config_id(store: Option<&SqliteStore>, hb_run_id: Option<&str>) -> Option<String> {
    let store = store?;
    let run_id = hb_run_id?;
    store.get_run(run_id).ok().flatten().map(|md| md.config_id)
}

/// Filter status heartbeats and run registry results by config id (issue #51).
///
/// The registry is opened once to resolve heartbeat -> config relationships; a
/// registry error short-circuits the run filtering but still scopes heartbeats.
/// @plan:issue-51
fn filter_status_by_config(
    heartbeats: std::collections::HashMap<String, luther_workflow::monitor::heartbeat::Heartbeat>,
    runs_result: Result<Vec<RunMetadata>, String>,
    config_id: &str,
) -> (
    std::collections::HashMap<String, luther_workflow::monitor::heartbeat::Heartbeat>,
    Result<Vec<RunMetadata>, String>,
) {
    let store = open_runs_store().ok().flatten();
    let filtered_hbs = heartbeats
        .into_iter()
        .filter(|(_, hb)| {
            heartbeat_config_id(store.as_ref(), hb.run_id.as_deref()).as_deref() == Some(config_id)
        })
        .collect();
    let filtered_runs = runs_result.map(|runs| {
        runs.into_iter()
            .filter(|md| md.config_id == config_id)
            .collect()
    });
    (filtered_hbs, filtered_runs)
}

/// Open the persistent run registry store at the shared checkpoints.db.
///
/// Returns `Ok(None)` when the database file does not exist yet (treated as an
/// empty registry), `Ok(Some(store))` when opened, and `Err` when the file is
/// present but cannot be opened (surfaced distinctly from "no runs").
/// @plan:issue-51
fn open_runs_store() -> Result<Option<SqliteStore>, String> {
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
async fn handle_runs_command(args: &luther_workflow::cli::RunsArgs) {
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

/// Open the run registry, exiting with a clear error when it is absent.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
fn require_runs_store(run_id: &str) -> SqliteStore {
    match open_runs_store() {
        Ok(Some(store)) => store,
        Ok(None) => {
            eprintln!("Error: run '{run_id}' not found (no run registry)");
            process::exit(1);
        }
        Err(e) => {
            eprintln!("Error: {e}");
            process::exit(1);
        }
    }
}

/// Build a [`RunContext`] from an existing run record so a resumed runner keeps
/// the original issue/PR identity and paths instead of re-deriving them.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
fn run_context_from_metadata(
    md: &RunMetadata,
    run_id: &str,
) -> luther_workflow::engine::RunContext {
    luther_workflow::engine::RunContext {
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
fn reconstruct_runner(
    md: &RunMetadata,
    run_id: &str,
    db_path: &std::path::Path,
) -> Result<EngineRunner, String> {
    let config_root = std::path::PathBuf::from("config");
    let mut config = resolve_workflow_config(&md.config_id, &config_root)
        .map_err(|e| format!("resolve config '{}': {e}", md.config_id))?;
    // Re-apply the original run's effective runtime overrides so the resumed
    // interpolation context (target_repo, issue_number, work_dir, artifact_dir)
    // matches the original target/workspace/artifacts rather than static config
    // defaults. @plan:PLAN-20260623-LUTHER-CONTINUATION
    let overrides = luther_workflow::engine::continuation::continuation_overrides(md);
    apply_target_profile_overrides(&mut config, &overrides)
        .map_err(|e| format!("apply continuation overrides: {e}"))?;
    let workflow_type = resolve_workflow_type(&md.workflow_type_id, &config_root)
        .map_err(|e| format!("resolve workflow type '{}': {e}", md.workflow_type_id))?;
    // Fail fast with diagnostics rather than resuming against an invalid profile,
    // but only when the workflow actually uses a target profile (mirrors the
    // initial-run gate). @plan:PLAN-20260623-LUTHER-CONTINUATION
    if target_profile_validation_required(&workflow_type.workflow_type_id, &config, &overrides) {
        validate_target_profile(&config)
            .map_err(|e| format!("invalid continuation profile: {e}"))?;
    }
    let run_context = run_context_from_metadata(md, run_id);
    let instance = WorkflowInstance::create_with_run_id(workflow_type, config, run_id);
    let registry = ExecutorRegistry::with_defaults();
    EngineRunner::with_db_path_and_context(instance, registry, db_path, run_context)
        .map_err(|e| format!("create runner: {e}"))
}

/// Validate + plan a continuation, writing request/validation artifacts and
/// exiting non-zero with diagnostics when validation fails.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
fn plan_continuation_or_exit(
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

/// Commit a planned continuation (re-stamp resume point + reopen run) and
/// execute the reconstructed runner, writing the result artifact.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
fn commit_and_execute(
    store: &SqliteStore,
    md: &RunMetadata,
    request: &luther_workflow::engine::ContinuationRequest,
    plan: &luther_workflow::engine::continuation::ContinuationPlan,
) {
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
    let db_path = luther_workflow::runtime_paths::get_data_dir().join("checkpoints.db");
    let mut runner = match reconstruct_runner(md, &request.run_id, &db_path) {
        Ok(runner) => runner,
        Err(e) => {
            eprintln!("Error: {e}");
            process::exit(1);
        }
    };
    if let Err(e) = luther_workflow::engine::commit_continuation(store.conn(), request, &step) {
        eprintln!("Error: failed to reopen run '{}': {e}", request.run_id);
        process::exit(1);
    }
    println!(
        "Reopened run '{}' at step '{step}' (continuation: {})",
        request.run_id,
        request.kind.verb()
    );
    install_interrupt_handlers(runner.interrupt_handle());
    let outcome = runner.run();
    write_continuation_result(&plan.artifact_dir, &request.kind, &step, &outcome);
    report_continuation_outcome(&request.run_id, &step, outcome);
}

/// Write the `resume-result.json` / `retry-result.json` artifact.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
fn write_continuation_result(
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
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
fn report_continuation_outcome(
    run_id: &str,
    step: &str,
    outcome: Result<RunOutcome, luther_workflow::engine::runner::EngineError>,
) {
    match outcome {
        Ok(RunOutcome::Success) => {
            println!("Run '{run_id}' completed after continuation.");
            process::exit(0);
        }
        Ok(RunOutcome::WaitingExternal { step_id, reason }) => {
            println!("Run '{run_id}' is waiting at '{step_id}': {reason}");
            println!("Resume with: luther-workflow runs resume {run_id}");
            process::exit(0);
        }
        Ok(RunOutcome::Interrupted { step_id }) => {
            println!("Run '{run_id}' interrupted at '{step_id}' (can be resumed).");
            process::exit(130);
        }
        Ok(RunOutcome::Abandoned { step_id, reason }) => {
            eprintln!("Run '{run_id}' abandoned at '{step_id}': {reason}");
            process::exit(1);
        }
        Ok(RunOutcome::Failure { step_id, reason }) => {
            eprintln!("Run '{run_id}' failed at '{step_id}': {reason}");
            process::exit(1);
        }
        Err(e) => {
            eprintln!("Run '{run_id}' continuation from '{step}' errored: {e}");
            process::exit(1);
        }
    }
}

/// `runs checkpoints RUN_ID` — list every per-step checkpoint for a run.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
fn handle_runs_checkpoints(args: &luther_workflow::cli::RunsCheckpointsArgs) {
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
fn print_checkpoints_json(run_id: &str, checkpoints: &[luther_workflow::persistence::Checkpoint]) {
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
fn print_checkpoints_human(run_id: &str, checkpoints: &[luther_workflow::persistence::Checkpoint]) {
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
fn handle_runs_resume(args: &luther_workflow::cli::RunsResumeArgs) {
    let store = require_runs_store(&args.run_id);
    let md = load_run_or_exit(&store, &args.run_id);
    let request = luther_workflow::engine::ContinuationRequest {
        run_id: args.run_id.clone(),
        kind: luther_workflow::engine::ContinuationKind::Resume,
        force: args.force,
    };
    let plan = plan_continuation_or_exit(&store, &md, &request);
    commit_and_execute(&store, &md, &request, &plan);
}

/// `runs retry RUN_ID [--from-failed-step]` — retry an external-wait step.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
fn handle_runs_retry(args: &luther_workflow::cli::RunsRetryArgs) {
    let store = require_runs_store(&args.run_id);
    let md = load_run_or_exit(&store, &args.run_id);
    let request = luther_workflow::engine::ContinuationRequest {
        run_id: args.run_id.clone(),
        kind: luther_workflow::engine::ContinuationKind::Retry {
            from_failed_step: args.from_failed_step,
        },
        force: args.force,
    };
    let plan = plan_continuation_or_exit(&store, &md, &request);
    commit_and_execute(&store, &md, &request, &plan);
}

/// `runs rewind RUN_ID (--to-step S | --to-checkpoint ID)` — set the resume
/// point to an earlier checkpoint without immediately re-executing.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
fn handle_runs_rewind(args: &luther_workflow::cli::RunsRewindArgs) {
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
    };
    let plan = plan_continuation_or_exit(&store, &md, &request);
    let step = plan
        .selected
        .as_ref()
        .map(|c| c.step_id.clone())
        .unwrap_or_default();
    if let Err(e) = luther_workflow::engine::commit_continuation(store.conn(), &request, &step) {
        eprintln!("Error: failed to set resume point: {e}");
        process::exit(1);
    }
    println!(
        "Rewound run '{}' to step '{step}'. Resume with: luther-workflow runs resume {}",
        args.run_id, args.run_id
    );
}

/// Load a run record from the store, exiting cleanly when absent.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
fn load_run_or_exit(store: &SqliteStore, run_id: &str) -> RunMetadata {
    match store.get_run(run_id) {
        Ok(Some(md)) => md,
        Ok(None) => {
            eprintln!("Error: run '{run_id}' not found");
            process::exit(1);
        }
        Err(e) => {
            eprintln!("Error: failed to read run '{run_id}': {e}");
            process::exit(1);
        }
    }
}

/// Load all runs from the registry, applying config/state filters (issue #51).
/// @plan:issue-51
fn load_filtered_runs(
    config: Option<&str>,
    state: Option<&str>,
) -> Result<Vec<RunMetadata>, String> {
    let Some(store) = open_runs_store()? else {
        return Ok(Vec::new());
    };
    let mut runs = store
        .list_runs()
        .map_err(|e| format!("failed to list runs from registry: {e}"))?;
    if let Some(config_id) = config {
        runs.retain(|md| md.config_id == config_id);
    }
    if let Some(state_str) = state {
        let wanted: RunStatus = state_str
            .parse()
            .map_err(|e| format!("invalid --state '{state_str}': {e}"))?;
        runs.retain(|md| md.status == wanted);
    }
    Ok(runs)
}

/// Handle the `monitor` command (issue #52).
///
/// Continuous, plain-CLI watch view. This is the thin I/O + loop + signal
/// shell; all modeling/filtering/rendering lives in the pure `monitor::snapshot`
/// module. Strictly read-only: it never stops daemons or cancels runs.
/// @plan:issue-52
async fn handle_monitor_command(args: &luther_workflow::cli::MonitorArgs) {
    use std::io::IsTerminal;

    let count = resolve_snapshot_count(args.once, args.times);
    let filter = MonitorFilter {
        config: args.config.clone(),
        run: args.run.clone(),
        issue: args.issue,
    };
    let clear = !args.no_clear && std::io::stdout().is_terminal();
    let mut remaining = count;
    let mut first = true;

    loop {
        // Stop before rendering (and before sleeping) once the requested count
        // is exhausted. This guarantees `--times 0` emits zero snapshots and
        // that we never sleep after the final snapshot.
        if let Some(left) = remaining.as_ref() {
            if *left == 0 {
                return;
            }
        }

        if !first {
            let tick = tokio::time::sleep(tokio::time::Duration::from_secs(args.interval));
            tokio::select! {
                _ = tick => {}
                _ = tokio::signal::ctrl_c() => {
                    eprintln!("Monitor stopped");
                    return;
                }
            }
        }
        first = false;

        render_one_snapshot(&filter, args.tail, clear);

        if let Some(left) = remaining.as_mut() {
            *left = left.saturating_sub(1);
        }
    }
}

/// Collect, render and print exactly one monitor snapshot.
/// @plan:issue-52
fn render_one_snapshot(filter: &MonitorFilter, tail: usize, clear: bool) {
    let snapshot = collect_snapshot(filter, tail);
    let mut body = String::new();
    if render_snapshot(&snapshot, tail, &mut body).is_err() {
        eprintln!("Error rendering monitor snapshot");
        return;
    }
    if clear {
        print!("{CLEAR_SCREEN}");
    } else {
        println!("{}", separator_line(&snapshot.generated_at));
    }
    print!("{body}");
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

/// Collect a single snapshot from global state (thin I/O shell).
/// @plan:issue-52
fn collect_snapshot(filter: &MonitorFilter, tail: usize) -> MonitorSnapshot {
    let now = chrono::Utc::now();
    let daemons = collect_daemon_summaries(filter, now.timestamp());
    let all_runs = match open_runs_store() {
        Ok(Some(store)) => match store.list_runs() {
            Ok(runs) => runs,
            Err(e) => {
                eprintln!("Warning: run registry unavailable: failed to list runs: {e}");
                Vec::new()
            }
        },
        Ok(None) => Vec::new(),
        Err(e) => {
            eprintln!("Warning: run registry unavailable: {e}");
            Vec::new()
        }
    };
    let filtered = filter.apply(&all_runs);
    let counts = RunCounts::from_runs(&filtered.runs);
    let recent_events = collect_selected_events(filtered.selected.as_ref(), tail);
    MonitorSnapshot {
        generated_at: now.to_rfc3339(),
        daemons,
        counts,
        runs: filtered.runs,
        selected: filtered.selected,
        recent_events,
    }
}

/// Collect daemon summaries, honoring the `--config` filter.
/// @plan:issue-52
fn collect_daemon_summaries(filter: &MonitorFilter, now: i64) -> Vec<DaemonSummary> {
    DaemonStore::production()
        .read_all()
        .iter()
        .filter(|state| {
            filter
                .config
                .as_ref()
                .is_none_or(|cfg| &state.config_id == cfg)
        })
        .map(|state| {
            let alive = is_daemon_alive(state.pid);
            DaemonSummary::from_state(state, alive, now)
        })
        .collect()
}

/// Load recent events for the selected run (empty when none / tail == 0).
/// @plan:issue-52
fn collect_selected_events(selected: Option<&RunMetadata>, tail: usize) -> Vec<EventRecord> {
    if tail == 0 {
        return Vec::new();
    }
    let Some(md) = selected else {
        return Vec::new();
    };
    let Ok(Some(store)) = open_runs_store() else {
        return Vec::new();
    };
    load_recent_events(store.conn(), &md.run_id, tail).unwrap_or_default()
}

/// Handle `runs list` (issue #51).
/// @plan:issue-51
fn handle_runs_list(args: &luther_workflow::cli::RunsListArgs) {
    let runs = match load_filtered_runs(args.config.as_deref(), args.state.as_deref()) {
        Ok(runs) => runs,
        Err(e) => {
            eprintln!("Error: {e}");
            process::exit(1);
        }
    };
    if args.json {
        let runs_json: Vec<_> = runs.iter().map(run_metadata_to_json).collect();
        let value = serde_json::json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "runs": runs_json,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
        return;
    }
    print_runs_table(&runs);
}

/// Render the human-readable `runs list` table (issue #51).
/// @plan:issue-51
fn print_runs_table(runs: &[RunMetadata]) {
    if runs.is_empty() {
        println!("No runs found.");
        return;
    }
    println!(
        "{:<20} {:<28} {:<7} {:<7} {:<11} {:<16} {:<25}",
        "CONFIG", "RUN ID", "ISSUE", "PR", "STATE", "STEP", "UPDATED"
    );
    for md in runs {
        let updated = md.updated_at.unwrap_or(md.created_at).to_rfc3339();
        println!(
            "{:<20} {:<28} {:<7} {:<7} {:<11} {:<16} {:<25}",
            truncate_field(&md.config_id, 20),
            truncate_field(&md.run_id, 28),
            md.issue_number
                .map_or_else(|| "-".to_string(), |n| n.to_string()),
            md.pr_number
                .map_or_else(|| "-".to_string(), |n| n.to_string()),
            md.status.to_string(),
            truncate_field(md.current_step.as_deref().unwrap_or("-"), 16),
            updated,
        );
    }
}

/// Truncate a field for fixed-width table rendering.
/// @plan:issue-51
fn truncate_field(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        value.to_string()
    } else if width <= 1 {
        value.chars().take(width).collect()
    } else {
        let prefix: String = value.chars().take(width - 1).collect();
        format!("{prefix}…")
    }
}

/// Handle `runs show RUN_ID` (issue #51).
/// @plan:issue-51
fn handle_runs_show(args: &luther_workflow::cli::RunsShowArgs) {
    let store = match open_runs_store() {
        Ok(Some(store)) => store,
        Ok(None) => {
            eprintln!("Error: run '{}' not found (no run registry)", args.run_id);
            process::exit(1);
        }
        Err(e) => {
            eprintln!("Error: {e}");
            process::exit(1);
        }
    };
    let md = match store.get_run(&args.run_id) {
        Ok(Some(md)) => md,
        Ok(None) => {
            eprintln!("Error: run '{}' not found", args.run_id);
            process::exit(1);
        }
        Err(e) => {
            eprintln!("Error: failed to read run '{}': {e}", args.run_id);
            process::exit(1);
        }
    };
    let events = load_events(store.conn(), &args.run_id).unwrap_or_default();
    let artifacts = list_artifacts(&args.run_id).unwrap_or_default();
    let log_path = effective_log_path(&md, &args.run_id);
    let log_exists = log_path.exists();
    if args.json {
        print_runs_show_json(&md, &events, &artifacts, &log_path, log_exists);
    } else {
        print_runs_show_human(&md, &events, &artifacts, &log_path, log_exists);
    }
}

/// Compute the conventional log path for a run.
/// @plan:issue-51
fn run_log_path(run_id: &str) -> std::path::PathBuf {
    luther_workflow::runtime_paths::get_log_dir().join(format!("{run_id}.log"))
}

/// Resolve the effective log path for a run, preferring the persisted
/// `RunMetadata.log_path` and falling back to the conventional path.
/// @plan:issue-51
fn effective_log_path(md: &RunMetadata, run_id: &str) -> std::path::PathBuf {
    md.log_path
        .as_deref()
        .map_or_else(|| run_log_path(run_id), std::path::PathBuf::from)
}

/// Render `runs show` as JSON (issue #51).
/// @plan:issue-51
fn print_runs_show_json(
    md: &RunMetadata,
    events: &[luther_workflow::persistence::EventRecord],
    artifacts: &[luther_workflow::persistence::ArtifactRecord],
    log_path: &std::path::Path,
    log_exists: bool,
) {
    let mut value = run_metadata_to_json(md);
    let obj = value
        .as_object_mut()
        .expect("run metadata json is an object");
    obj.insert(
        "events".to_string(),
        serde_json::json!(events
            .iter()
            .map(|e| serde_json::json!({
                "step_id": e.step_id,
                "outcome": e.outcome,
                "event_type": e.event_type,
                "details": e.details,
                "timestamp": e.timestamp.to_rfc3339(),
            }))
            .collect::<Vec<_>>()),
    );
    obj.insert(
        "artifacts".to_string(),
        serde_json::json!(artifacts
            .iter()
            .map(|a| serde_json::json!({
                "artifact_path": a.artifact_path.display().to_string(),
                "size_bytes": a.size_bytes,
            }))
            .collect::<Vec<_>>()),
    );
    obj.insert(
        "log_path".to_string(),
        serde_json::json!(log_path.display().to_string()),
    );
    obj.insert("log_exists".to_string(), serde_json::json!(log_exists));
    println!(
        "{}",
        serde_json::to_string_pretty(&value).unwrap_or_default()
    );
}

/// Render the Run Info + Current State sections of `runs show` (issue #51).
/// @plan:issue-51
fn print_runs_show_info(md: &RunMetadata) {
    println!("Run {}", md.run_id);
    println!("================================");
    println!("Run Info:");
    println!("  Config: {}", md.config_id);
    println!("  Workflow type: {}", md.workflow_type_id);
    println!(
        "  Repository: {}",
        md.repository.as_deref().unwrap_or("(none)")
    );
    println!(
        "  Issue: {}  PR: {}",
        md.issue_number
            .map_or_else(|| "(none)".to_string(), |n| n.to_string()),
        md.pr_number
            .map_or_else(|| "(none)".to_string(), |n| n.to_string())
    );
    println!("  Head SHA: {}", md.head_sha.as_deref().unwrap_or("(none)"));
    println!("  Status: {}", md.status);
    println!();
    println!("Current State:");
    println!(
        "  Current step: {}",
        md.current_step.as_deref().unwrap_or("(none)")
    );
    println!(
        "  Previous: {} -> {}",
        md.previous_step.as_deref().unwrap_or("(none)"),
        md.previous_outcome.as_deref().unwrap_or("(none)")
    );
    println!("  Next step: {}", next_step_label(md));
}

/// Render the Paths + Processes sections of `runs show` (issue #51).
/// @plan:issue-51
fn print_runs_show_paths_and_procs(md: &RunMetadata, log_path: &std::path::Path, log_exists: bool) {
    println!();
    println!("Paths:");
    println!(
        "  Workspace: {}",
        md.workspace_path.as_deref().unwrap_or("(none)")
    );
    println!(
        "  Log: {} ({})",
        log_path.display(),
        if log_exists { "exists" } else { "missing" }
    );
    println!(
        "  Artifact root: {}",
        md.artifact_root.as_deref().unwrap_or("(none)")
    );
    println!();
    println!("Processes:");
    println!("  Workflow PID: {}", pid_liveness_label(md));
    if md.child_pids.is_empty() {
        println!("  Child PIDs: (none)");
    } else {
        let stale = md.are_child_pids_stale();
        println!(
            "  Child PIDs: {} (stale: {})",
            md.child_pids
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            if stale.is_empty() {
                "none".to_string()
            } else {
                stale
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        );
    }
}

/// Render the Recent Events + Artifacts sections of `runs show` (issue #51).
/// @plan:issue-51
fn print_runs_show_events(
    events: &[luther_workflow::persistence::EventRecord],
    artifacts: &[luther_workflow::persistence::ArtifactRecord],
) {
    println!();
    println!("Recent Events:");
    if events.is_empty() {
        println!("  (none)");
    } else {
        let start = events.len().saturating_sub(15);
        for e in &events[start..] {
            println!(
                "  [{}] {} -> {} ({})",
                e.timestamp.to_rfc3339(),
                e.step_id,
                e.outcome,
                e.event_type
            );
        }
    }
    println!();
    println!("Artifacts:");
    if artifacts.is_empty() {
        println!("  (none)");
    } else {
        for a in artifacts {
            let size = a
                .size_bytes
                .map_or_else(|| "?".to_string(), |s| s.to_string());
            println!("  {} ({} bytes)", a.artifact_path.display(), size);
        }
    }
}

/// Render `runs show` in human-readable form (issue #51).
/// @plan:issue-51
fn print_runs_show_human(
    md: &RunMetadata,
    events: &[luther_workflow::persistence::EventRecord],
    artifacts: &[luther_workflow::persistence::ArtifactRecord],
    log_path: &std::path::Path,
    log_exists: bool,
) {
    print_runs_show_info(md);
    print_runs_show_paths_and_procs(md, log_path, log_exists);
    print_runs_show_events(events, artifacts);
}

/// Resolve the run id for `runs tail` from args or active heartbeats (issue #51).
/// @plan:issue-51
async fn resolve_tail_run_id(args: &luther_workflow::cli::RunsTailArgs) -> Result<String, String> {
    if let Some(run_id) = &args.run_id {
        return Ok(run_id.clone());
    }
    if !args.current {
        return Err("provide a RUN_ID or use --current".to_string());
    }
    let heartbeats = read_all_heartbeats()
        .await
        .map_err(|e| format!("failed to read heartbeats: {e}"))?;
    let active: Vec<String> = heartbeats
        .values()
        .filter(|hb| {
            matches!(
                hb.state,
                MonitorState::Running | MonitorState::Starting | MonitorState::Degraded
            )
        })
        .filter_map(|hb| hb.run_id.clone())
        .collect();
    match active.len() {
        0 => Err("no active run found for --current".to_string()),
        1 => Ok(active[0].clone()),
        _ => Err("multiple active runs found; specify an explicit RUN_ID".to_string()),
    }
}

/// Read the last `n` lines of a file using a bounded buffer.
/// @plan:issue-51
fn tail_lines(path: &std::path::Path, n: usize) -> std::io::Result<Vec<String>> {
    use std::collections::VecDeque;
    use std::io::BufRead;

    if n == 0 {
        return Ok(Vec::new());
    }
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    let mut tail: VecDeque<String> = VecDeque::with_capacity(n);
    for line in reader.lines() {
        let line = line?;
        if tail.len() == n {
            tail.pop_front();
        }
        tail.push_back(line);
    }
    Ok(tail.into_iter().collect())
}

/// Handle `runs tail` (issue #51).
/// @plan:issue-51
async fn handle_runs_tail(args: &luther_workflow::cli::RunsTailArgs) {
    let run_id = match resolve_tail_run_id(args).await {
        Ok(id) => id,
        Err(e) => {
            eprintln!("Error: {e}");
            process::exit(1);
        }
    };
    let log_path = match open_runs_store() {
        Ok(Some(store)) => match store.get_run(&run_id) {
            Ok(Some(md)) => effective_log_path(&md, &run_id),
            _ => run_log_path(&run_id),
        },
        _ => run_log_path(&run_id),
    };
    if !log_path.exists() {
        let artifacts = list_artifacts(&run_id).unwrap_or_default();
        if args.json {
            let value = serde_json::json!({
                "run_id": run_id,
                "log_path": log_path.display().to_string(),
                "log_exists": false,
                "lines": [],
                "artifacts": artifacts
                    .iter()
                    .map(|a| a.artifact_path.display().to_string())
                    .collect::<Vec<_>>(),
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&value).unwrap_or_default()
            );
        } else {
            println!("No log file at {}", log_path.display());
            if !artifacts.is_empty() {
                println!("Artifacts that may contain logs:");
                for a in &artifacts {
                    println!("  {}", a.artifact_path.display());
                }
            }
        }
        return;
    }
    let lines = match tail_lines(&log_path, args.lines) {
        Ok(lines) => lines,
        Err(e) => {
            eprintln!("Error: failed to read log file {}: {e}", log_path.display());
            process::exit(1);
        }
    };
    if args.json {
        let value = serde_json::json!({
            "run_id": run_id,
            "log_path": log_path.display().to_string(),
            "log_exists": true,
            "lines": lines,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
    } else {
        for line in &lines {
            println!("{line}");
        }
    }
}

/// Parse a monitor instance id (`monitor-<pid>`) into its PID component.
/// @plan:issue-51
fn instance_pid(instance_id: &str) -> Option<u32> {
    instance_id
        .strip_prefix("monitor-")
        .and_then(|s| s.parse::<u32>().ok())
}

/// A single row of `runs ps` output describing a process's liveness.
/// @plan:issue-51
struct PsRow {
    instance_id: String,
    run_id: Option<String>,
    config_id: Option<String>,
    state: String,
    active_workers: u32,
    uptime_secs: i64,
    pid: Option<u32>,
    is_alive: bool,
    is_stale: bool,
    child_pids: Vec<u32>,
    stale_child_pids: Vec<u32>,
}

/// Build the `runs ps` rows from heartbeats and the run registry (issue #51).
/// @plan:issue-51
async fn build_ps_rows(config: Option<&str>) -> Result<Vec<PsRow>, String> {
    let heartbeats = read_all_heartbeats()
        .await
        .map_err(|e| format!("failed to read heartbeats: {e}"))?;
    let store = open_runs_store()?;
    let now = chrono::Utc::now().timestamp();
    let mut rows = Vec::new();
    for hb in heartbeats.values() {
        let md = hb
            .run_id
            .as_deref()
            .and_then(|rid| store.as_ref().and_then(|s| s.get_run(rid).ok().flatten()));
        let config_id = md.as_ref().map(|m| m.config_id.clone());
        if let Some(want) = config {
            if config_id.as_deref() != Some(want) {
                continue;
            }
        }
        let pid = instance_pid(&hb.instance_id);
        let is_alive = pid.is_some_and(luther_workflow::monitor::process::is_process_alive);
        let is_stale = !is_alive || (now - hb.timestamp) > 60;
        rows.push(PsRow {
            instance_id: hb.instance_id.clone(),
            run_id: hb.run_id.clone(),
            config_id,
            state: monitor_state_token(&hb.state).to_string(),
            active_workers: hb.active_workers,
            uptime_secs: hb.uptime_secs,
            pid,
            is_alive,
            is_stale,
            child_pids: md
                .as_ref()
                .map(|m| m.child_pids.clone())
                .unwrap_or_default(),
            stale_child_pids: md
                .as_ref()
                .map(RunMetadata::are_child_pids_stale)
                .unwrap_or_default(),
        });
    }
    Ok(rows)
}

/// Map a `MonitorState` to its stable lowercase token.
/// @plan:issue-51
fn monitor_state_token(state: &MonitorState) -> &'static str {
    match state {
        MonitorState::Starting => "starting",
        MonitorState::Running => "running",
        MonitorState::Degraded => "degraded",
        MonitorState::Stopping => "stopping",
        MonitorState::Stopped => "stopped",
        MonitorState::Error => "error",
    }
}

/// Convert a `runs ps` row to its stable JSON object (issue #51).
/// @plan:issue-51
fn ps_row_to_json(row: &PsRow) -> serde_json::Value {
    serde_json::json!({
        "instance_id": row.instance_id,
        "run_id": row.run_id,
        "config_id": row.config_id,
        "state": row.state,
        "active_workers": row.active_workers,
        "uptime_secs": row.uptime_secs,
        "pid": row.pid,
        "is_alive": row.is_alive,
        "is_stale": row.is_stale,
        "child_pids": row.child_pids,
        "stale_child_pids": row.stale_child_pids,
    })
}

/// Handle `runs ps` (issue #51).
/// @plan:issue-51
async fn handle_runs_ps(args: &luther_workflow::cli::RunsPsArgs) {
    let rows = match build_ps_rows(args.config.as_deref()).await {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("Error: {e}");
            process::exit(1);
        }
    };
    if args.json {
        let array: Vec<_> = rows.iter().map(ps_row_to_json).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!(array)).unwrap_or_default()
        );
        return;
    }
    if rows.is_empty() {
        println!("No processes found.");
        return;
    }
    println!(
        "{:<18} {:<24} {:<10} {:>7} {:>9} {:>8} {:<5} {:<20}",
        "INSTANCE", "RUN ID", "STATE", "WORKERS", "UPTIME", "PID", "STALE", "CHILD PIDS"
    );
    for row in &rows {
        println!(
            "{:<18} {:<24} {:<10} {:>7} {:>8}s {:>8} {:<5} {:<20}",
            truncate_field(&row.instance_id, 18),
            truncate_field(row.run_id.as_deref().unwrap_or("-"), 24),
            row.state,
            row.active_workers,
            row.uptime_secs,
            row.pid.map_or_else(|| "-".to_string(), |p| p.to_string()),
            if row.is_stale { "yes" } else { "no" },
            format_child_pids(&row.child_pids, &row.stale_child_pids),
        );
    }
}

/// Render child PIDs for the `runs ps` table, marking stale entries.
/// @plan:issue-51
fn format_child_pids(child_pids: &[u32], stale_child_pids: &[u32]) -> String {
    if child_pids.is_empty() {
        return "-".to_string();
    }
    child_pids
        .iter()
        .map(|pid| {
            if stale_child_pids.contains(pid) {
                format!("{pid} (stale)")
            } else {
                pid.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn workflow_config(artifact_dir: &std::path::Path) -> WorkflowConfig {
        WorkflowConfig {
            config_id: "cfg".to_string(),
            workflow_type_id: "wf".to_string(),
            runtime: luther_workflow::workflow::schema::RuntimeConfig {
                timeout_seconds: 1,
                max_retries: 0,
                parallel_steps: None,
                log_level: None,
            },
            repo: luther_workflow::workflow::schema::RepoConfig {
                workspace_strategy: "reuse".to_string(),
                branch_template: "issue{issue_number}".to_string(),
                base_branch: Some("main".to_string()),
                workspace_root: None,
            },
            guard_limits: luther_workflow::workflow::schema::GuardLimits {
                max_iterations: None,
                max_file_changes: None,
                max_tokens: None,
                max_cost: None,
            },
            variables: HashMap::from([(
                "artifact_dir".to_string(),
                artifact_dir.to_string_lossy().to_string(),
            )]),
            discovery: None,
        }
    }

    #[test]
    fn wait_poll_identity_reads_captured_pr_artifact_when_metadata_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let pr_dir = tmp
            .path()
            .join("pr-followup")
            .join("current")
            .join("run-identity")
            .join("owner")
            .join("repo")
            .join("62");
        std::fs::create_dir_all(&pr_dir).unwrap();
        std::fs::write(
            pr_dir.join("pr.json"),
            serde_json::to_vec(&serde_json::json!({
                "run_id": "run-identity",
                "pr_number": 62,
                "head_sha": "abcdef123456",
                "repository_owner": "owner",
                "repository_name": "repo"
            }))
            .unwrap(),
        )
        .unwrap();
        let request = luther_workflow::daemon::launcher::LaunchRequest {
            config_id: "cfg".to_string(),
            run_id: "run-identity".to_string(),
            repo: "owner/repo".to_string(),
            issue_number: 62,
        };
        let identity = wait_poll_identity(
            &request,
            &workflow_config(tmp.path()),
            None,
            WaitKind::PrChecks,
        )
        .unwrap();

        assert_eq!(identity.pr_number, Some(62));
        assert_eq!(identity.head_sha.as_deref(), Some("abcdef123456"));
    }

    #[test]
    fn wait_poll_identity_rejects_missing_pr_check_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let request = luther_workflow::daemon::launcher::LaunchRequest {
            config_id: "cfg".to_string(),
            run_id: "run-missing".to_string(),
            repo: "owner/repo".to_string(),
            issue_number: 62,
        };

        let err = wait_poll_identity(
            &request,
            &workflow_config(tmp.path()),
            None,
            WaitKind::PrChecks,
        )
        .unwrap_err();

        assert!(err.contains("missing PR number or head SHA"));
    }

    #[test]
    fn persist_run_poll_identity_updates_stale_or_empty_metadata() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        luther_workflow::persistence::sqlite::init_runs_schema(&conn).unwrap();
        let mut metadata = RunMetadata::new("run-identity", "wf", "cfg");
        metadata.pr_number = Some(1);
        metadata.head_sha = Some("old".to_string());
        persist_run_with_conn(&conn, &metadata).unwrap();
        let identity = WaitPollIdentity {
            pr_number: Some(62),
            head_sha: Some("new".to_string()),
        };

        persist_run_poll_identity(&conn, &mut metadata, &identity).unwrap();

        let loaded = get_run_with_conn(&conn, "run-identity").unwrap().unwrap();
        assert_eq!(loaded.pr_number, Some(62));
        assert_eq!(loaded.head_sha.as_deref(), Some("new"));
    }

    #[test]
    fn wait_poll_identity_prefers_captured_pr_artifact_over_stale_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let pr_dir = tmp
            .path()
            .join("pr-followup")
            .join("current")
            .join("run-stale")
            .join("owner")
            .join("repo")
            .join("62");
        std::fs::create_dir_all(&pr_dir).unwrap();
        std::fs::write(
            pr_dir.join("pr.json"),
            serde_json::to_vec(&serde_json::json!({
                "run_id": "run-stale",
                "pr_number": 62,
                "head_sha": "fresh-head",
                "repository_owner": "owner",
                "repository_name": "repo"
            }))
            .unwrap(),
        )
        .unwrap();
        let request = luther_workflow::daemon::launcher::LaunchRequest {
            config_id: "cfg".to_string(),
            run_id: "run-stale".to_string(),
            repo: "owner/repo".to_string(),
            issue_number: 62,
        };
        let mut metadata = RunMetadata::new("run-stale", "wf", "cfg");
        metadata.pr_number = Some(1);
        metadata.head_sha = Some("stale-head".to_string());

        let identity = wait_poll_identity(
            &request,
            &workflow_config(tmp.path()),
            Some(&metadata),
            WaitKind::PrChecks,
        )
        .unwrap();

        assert_eq!(identity.pr_number, Some(62));
        assert_eq!(identity.head_sha.as_deref(), Some("fresh-head"));
    }
}
