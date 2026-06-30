/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
fn wait_kind_for_step(step_id: &str) -> WaitKind {
    match step_id {
        "watch_pr_checks" => WaitKind::PrChecks,
        "collect_coderabbit_feedback" => WaitKind::CoderabbitReview,
        "merge_pr" | "wait_for_merge" => WaitKind::PrMerge,
        "dependency_child_merge" | "wait_for_child_merge" => WaitKind::DependencyChildMerge,
        "rate_limit_backoff" | "github_rate_limit_backoff" => WaitKind::RateLimitBackoff,
        other => {
            eprintln!("Warning: unmapped wait step '{other}' defaulting to human_review");
            WaitKind::HumanReview
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

async fn handle_status_command(args: &luther_workflow::cli::StatusArgs) {
    let heartbeats = read_status_heartbeats().await;
    let runs_result = read_run_registry(args.run_id.as_deref());
    let (heartbeats, runs_result) = match args.config.as_deref() {
        Some(config_id) => filter_status_by_config(heartbeats, runs_result, config_id),
        None => (heartbeats, runs_result),
    };

    if args.json {
        print_status_json(&heartbeats, &runs_result);
    } else {
        print_status_human(args, &heartbeats, &runs_result);
    }
}

async fn read_status_heartbeats(
) -> std::collections::HashMap<String, luther_workflow::monitor::heartbeat::Heartbeat> {
    match read_all_heartbeats().await {
        Ok(hbs) => hbs,
        Err(e) => {
            eprintln!("Error reading heartbeats: {e}");
            std::collections::HashMap::new()
        }
    }
}

fn print_status_json(
    heartbeats: &std::collections::HashMap<
        String,
        luther_workflow::monitor::heartbeat::Heartbeat,
    >,
    runs_result: &Result<Vec<luther_workflow::persistence::RunMetadata>, String>,
) {
    let (runs_json, registry_error): (Vec<_>, Option<String>) = match runs_result {
        Ok(runs) => (runs.iter().map(run_metadata_to_json).collect(), None),
        Err(e) => (Vec::new(), Some(e.clone())),
    };
    let status = serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "heartbeats": heartbeats,
        "runs": runs_json,
        "registry_error": registry_error,
    });
    match serde_json::to_string_pretty(&status) {
        Ok(json) => println!("{json}"),
        Err(e) => eprintln!("Error: failed to serialize status JSON: {e}"),
    }
}

fn print_status_human(
    args: &luther_workflow::cli::StatusArgs,
    heartbeats: &std::collections::HashMap<
        String,
        luther_workflow::monitor::heartbeat::Heartbeat,
    >,
    runs_result: &Result<Vec<luther_workflow::persistence::RunMetadata>, String>,
) {
    println!("Luther Workflow Monitor Status");
    println!("==============================");
    println!("Timestamp: {}", chrono::Utc::now().to_rfc3339());
    println!();
    print_heartbeat_status(heartbeats);
    print_requested_heartbeat_details(args.run_id.as_deref(), heartbeats);
    match runs_result {
        Ok(runs) => print_run_registry(runs, args.run_id.as_deref()),
        Err(e) => print_run_registry_error(e),
    }
}

fn print_heartbeat_status(
    heartbeats: &std::collections::HashMap<
        String,
        luther_workflow::monitor::heartbeat::Heartbeat,
    >,
) {
    if heartbeats.is_empty() {
        println!("No active runs found.");
        println!("  Status: No heartbeats detected");
        return;
    }
    println!("Active/Recent Runs:");
    for (run_id, hb) in heartbeats {
        println!("  Run ID: {run_id}");
        println!("    State: {}", monitor_state_label(hb.state));
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

fn monitor_state_label(state: MonitorState) -> &'static str {
    match state {
        MonitorState::Starting => "starting",
        MonitorState::Running => "running",
        MonitorState::Degraded => "degraded",
        MonitorState::Stopping => "stopping",
        MonitorState::Stopped => "stopped",
        MonitorState::Error => "error",
    }
}

fn print_requested_heartbeat_details(
    run_id: Option<&str>,
    heartbeats: &std::collections::HashMap<
        String,
        luther_workflow::monitor::heartbeat::Heartbeat,
    >,
) {
    if let Some(run_id) = run_id {
        if let Some(hb) = heartbeats.get(run_id) {
            println!("Details for run '{run_id}':");
            println!("  State: {:?}", hb.state);
            println!("  Active workers: {}", hb.active_workers);
        } else {
            println!("No heartbeat found for run '{run_id}'");
        }
    }
}

fn print_run_registry_error(error: &str) {
    eprintln!("Error: run registry unavailable: {error}");
    println!();
    println!("Persistent Run Registry:");
    println!("  Status: registry unavailable ({error})");
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
            || {
                luther_workflow::runtime_paths::get_data_dir()
                    .join("luther.sock")
                    .to_string_lossy()
                    .to_string()
            },
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
        Ok(mut service) => {
            let instance_id = service
                .get_status()
                .await
                .map(|s| s.instance_id)
                .unwrap_or_default();
            println!("Service started successfully. Instance ID: {instance_id}");
            println!("Press Ctrl+C to stop...");
            loop {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {
                        println!("Shutting down service...");
                        let _ = service.stop().await;
                        break;
                    }
                    _ = tokio::time::sleep(tokio::time::Duration::from_secs(1)) => {}
                }
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
    match serde_json::to_string_pretty(&payload) {
        Ok(json) => println!("{json}"),
        Err(e) => eprintln!("Error: failed to serialize discovery JSON: {e}"),
    }
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
        print_queue_json(&conn, &leases);
    } else {
        print_queue_text(&conn, &leases);
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
    } else {
        list_all_leases(conn).unwrap_or_default()
    };
    if let Some(status) = &args.status {
        let status = parse_status_or_exit(status);
        base.into_iter().filter(|l| l.status == status).collect()
    } else {
        base
    }
}

fn parse_status_or_exit(status: &str) -> LeaseStatus {
    match status.parse::<LeaseStatus>() {
        Ok(status) => status,
        Err(e) => {
            eprintln!("Error: invalid --status: {e}");
            process::exit(1);
        }
    }
}

/// Print the lease queue as JSON.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P05
fn print_queue_json(conn: &rusqlite::Connection, leases: &[IssueLease]) {
    let waits = queue_wait_summaries(conn);
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
    match serde_json::to_string_pretty(&items) {
        Ok(json) => println!("{json}"),
        Err(e) => eprintln!("Error: failed to serialize queue JSON: {e}"),
    }
}

/// Print the lease queue grouped by status.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P05
fn print_queue_text(conn: &rusqlite::Connection, leases: &[IssueLease]) {
    if leases.is_empty() {
        println!("Queue is empty.");
        return;
    }
    let waits = queue_wait_summaries(conn);
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

fn queue_wait_summaries(
    conn: &rusqlite::Connection,
) -> std::collections::HashMap<String, serde_json::Value> {
    let waits = match list_wait_states(conn) {
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

