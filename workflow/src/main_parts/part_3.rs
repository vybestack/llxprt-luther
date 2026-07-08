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
                luther_workflow::daemon::terminate_pid(pid);
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
    if let Err(e) = store.write(&state) {
        eprintln!("Error: failed to persist daemon running state: {e}");
        process::exit(1);
    }

    println!("Daemon running (config={config_id}, pid={}).", state.pid);

    let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    install_interrupt_handlers(shutdown.clone());

    let discovery = resolve_discovery_for(config, config_dir);
    if discovery.enabled {
        if let Some(path) = scheduler_config {
            run_daemon_supervisor_loop(store, &mut state, &shutdown, path, config_dir, once).await;
        } else {
            run_daemon_discovery_loop(
                store,
                &mut state,
                &shutdown,
                &discovery,
                &config_id,
                config_dir,
                once,
            )
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
    let path_bases = daemon_path_bases_from_config(&cfg);
    let parent_path_bases = parent_path_bases_from_config(&cfg, config_root);
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
            path_bases,
            parent_path_bases,
        })
}

/// Build the single-config scheduler target used by `daemon run/start` without
/// a supervisor config. This keeps one-shot and long-running daemon discovery
/// on the same per-run path isolation path as the multi-target scheduler.
/// @plan:issue-117
fn discovery_scheduler_target(
    config_id: &str,
    discovery: &luther_workflow::workflow::schema::DiscoveryConfig,
    config_root: &std::path::Path,
) -> Option<luther_workflow::daemon::scheduler::SchedulerTarget> {
    let cfg = match resolve_workflow_config(config_id, config_root) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("Error: Failed to resolve config '{config_id}': {e}");
            return None;
        }
    };
    Some(luther_workflow::daemon::scheduler::SchedulerTarget {
        config_id: config_id.to_string(),
        discovery: discovery.clone(),
        path_bases: daemon_path_bases_from_config(&cfg),
        parent_path_bases: parent_path_bases_from_config(&cfg, config_root),
    })
}

fn parent_path_bases_from_config(
    cfg: &luther_workflow::workflow::schema::WorkflowConfig,
    config_root: &std::path::Path,
) -> std::collections::BTreeMap<String, luther_workflow::daemon::launcher::DaemonPathBases> {
    cfg.discovery
        .as_ref()
        .and_then(|d| d.parent_config_id.as_deref())
        .and_then(|parent_config_id| parent_path_bases_entry(parent_config_id, config_root))
        .into_iter()
        .collect()
}

fn parent_path_bases_entry(
    parent_config_id: &str,
    config_root: &std::path::Path,
) -> Option<(String, luther_workflow::daemon::launcher::DaemonPathBases)> {
    let parent_cfg = resolve_workflow_config(parent_config_id, config_root).ok()?;
    Some((
        parent_config_id.to_string(),
        daemon_path_bases_from_config(&parent_cfg),
    ))
}

/// Extract structured daemon base roots (`work_dir`/`artifact_dir`) from the
/// fully resolved config variables. These are treated as base roots; per-run
/// `issue-N/run-id` suffixes are constructed by the launcher.
/// @plan:issue-117
fn daemon_path_bases_from_config(
    cfg: &luther_workflow::workflow::schema::WorkflowConfig,
) -> luther_workflow::daemon::launcher::DaemonPathBases {
    luther_workflow::daemon::launcher::DaemonPathBases {
        work_dir_base: cfg
            .variables
            .get("work_dir")
            .map(std::path::PathBuf::from),
        artifact_dir_base: cfg
            .variables
            .get("artifact_dir")
            .map(std::path::PathBuf::from),
    }
}
fn recover_stale_daemon_leases(
    conn: &rusqlite::Connection,
    stale_timeout: u64,
) -> Result<(), rusqlite::Error> {
    let recovered = luther_workflow::persistence::leases::mark_stale_leases(conn, stale_timeout)?;
    let ready_recovered = luther_workflow::persistence::leases::mark_stale_ready_to_resume_leases(
        conn,
        stale_timeout,
    )?;
    if recovered > 0 || ready_recovered > 0 {
        println!(
            "recovered {recovered} active stale lease(s) and {ready_recovered} ready-to-resume stale lease(s) on startup"
        );
    }
    Ok(())
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
    if let Err(e) = recover_stale_daemon_leases(&conn, stale_timeout) {
        eprintln!("Error: stale lease recovery failed: {e}");
        return;
    }
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
    config_dir: &Option<std::path::PathBuf>,
    once: bool,
) {
    use std::sync::atomic::Ordering;

    let config_root = config_dir
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from("config"));
    let Some(target) = discovery_scheduler_target(config_id, discovery, &config_root) else {
        return;
    };
    let conn = open_daemon_db();
    let query = SystemGithubIssueQuery::new(SystemGithubCommandRunner);
    let launcher = DaemonWorkflowLauncher::new(config_id.to_string());
    let stale_timeout = discovery
        .poll_interval_secs
        .unwrap_or(300)
        .saturating_mul(4);

    if let Err(e) = recover_stale_daemon_leases(&conn, stale_timeout) {
        eprintln!("Error: stale lease recovery failed: {e}");
        return;
    }

    let poll = discovery.poll_interval_secs.unwrap_or(300);
    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        match luther_workflow::daemon::scheduler::run_multi_target_once(
            std::slice::from_ref(&target),
            &[&query as &dyn luther_workflow::adapters::github_issues::GithubIssueQuery],
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
    let store = match open_runs_store() {
        Ok(store) => store,
        Err(err) => {
            eprintln!("Warning: could not open run registry for heartbeat filtering: {err}");
            None
        }
    };
    let filtered_hbs = if store.is_some() {
        heartbeats
            .into_iter()
            .filter(|(_, hb)| {
                heartbeat_config_id(store.as_ref(), hb.run_id.as_deref()).as_deref()
                    == Some(config_id)
            })
            .collect()
    } else {
        heartbeats
    };
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
    config_dir: &Option<std::path::PathBuf>,
) -> Result<EngineRunner, String> {
    let config_root = config_dir.as_deref().unwrap_or(std::path::Path::new("config"));
    let mut config = resolve_workflow_config(&md.config_id, config_root)
        .map_err(|e| format!("resolve config '{}': {e}", md.config_id))?;
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
    config_dir: &Option<std::path::PathBuf>,
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
    let db_path = store
        .db_path()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| luther_workflow::runtime_paths::get_data_dir().join("checkpoints.db"));
    let mut runner = match reconstruct_runner(md, &request.run_id, &db_path, config_dir) {
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
    if let Err(ref e) = outcome {
        let mut restored = md.clone();
        restored.updated_at = Some(chrono::Utc::now());
        if let Err(persist_err) = persist_run_with_conn(store.conn(), &restored) {
            eprintln!(
                "Warning: failed to restore run '{}' after continuation error {e}: {persist_err}",
                request.run_id
            );
        } else {
            eprintln!(
                "Run '{}' restored to status '{}' after continuation error: {e}",
                request.run_id, restored.status
            );
        }
    }
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
    commit_and_execute(&store, &md, &request, &plan, &args.config_dir);
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
    commit_and_execute(&store, &md, &request, &plan, &args.config_dir);
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

#[cfg(test)]
mod part_3_tests;
