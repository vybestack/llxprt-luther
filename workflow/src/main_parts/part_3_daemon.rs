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
                if !luther_workflow::daemon::terminate_pid(pid) {
                    eprintln!(
                        "Error: failed to confirm daemon pid {pid} exited before replacing lock for '{config_id}'"
                    );
                    return None;
                }
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

    let (cfg, discovery) = resolve_config_and_discovery_for(&config_id, config_dir);
    let daemon_failure = if discovery.enabled {
        if let Some(path) = scheduler_config {
            run_daemon_supervisor_loop(store, &mut state, &shutdown, path, config_dir, once).await
        } else {
            let config_root = config_dir.as_deref().unwrap_or(std::path::Path::new("config"));
            let target = discovery_scheduler_target(&config_id, &discovery, &cfg, config_root);
            run_daemon_discovery_loop(store, &mut state, &shutdown, target, once).await
        }
    } else {
        run_daemon_heartbeat_loop(store, &mut state, &shutdown).await
    };

    if let Some(error) = daemon_failure {
        eprintln!("Error: daemon exiting after failure: {error}");
        process::exit(1);
    }

    state.set_status(DaemonStatus::Stopping);
    if let Err(e) = store.write(&state) {
        tracing::warn!(config_id, error = %e, "failed to persist daemon stopping state");
    }
    state.set_status(DaemonStatus::Stopped);
    if let Err(e) = store.write(&state) {
        tracing::warn!(config_id, error = %e, "failed to persist daemon stopped state");
    }
    println!("Daemon stopped (config={config_id}).");
}

fn scheduler_targets(
    scheduler: &luther_workflow::workflow::schema::DaemonSchedulerConfig,
    config_dir: &Option<std::path::PathBuf>,
) -> Vec<luther_workflow::daemon::scheduler::SchedulerTarget> {
    let config_root = config_dir.as_deref().unwrap_or(std::path::Path::new("config"));
    scheduler
        .targets
        .iter()
        .filter_map(|target| scheduler_target(target, scheduler, config_root))
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
    cfg: &luther_workflow::workflow::schema::WorkflowConfig,
    config_root: &std::path::Path,
) -> luther_workflow::daemon::scheduler::SchedulerTarget {
    luther_workflow::daemon::scheduler::SchedulerTarget {
        config_id: config_id.to_string(),
        discovery: discovery.clone(),
        path_bases: daemon_path_bases_from_config(cfg),
        parent_path_bases: parent_path_bases_from_config(cfg, config_root),
    }
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
    let parent_cfg = match resolve_workflow_config(parent_config_id, config_root) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("Warning: failed to resolve parent config '{parent_config_id}': {e}");
            return None;
        }
    };
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

const MAX_HEARTBEAT_WRITE_FAILURES: u32 = 3;
const HEARTBEAT_TICK_MILLIS: u64 = 200;
const HEARTBEAT_WRITE_TICKS: u32 = 150;
const SCHEDULER_FAILURE_BACKOFF_SECS: u64 = 5;
const SCHEDULER_FAILURE_BACKOFF_MAX_EXPONENT: u32 = 4;

fn write_daemon_heartbeat(
    store: &DaemonStore,
    state: &DaemonState,
    consecutive_failures: &mut u32,
) -> Option<String> {
    match store.write(state) {
        Ok(()) => {
            *consecutive_failures = 0;
            None
        }
        Err(e) => {
            *consecutive_failures = consecutive_failures.saturating_add(1);
            tracing::warn!(
                config_id = %state.config_id,
                consecutive_failures = *consecutive_failures,
                error = %e,
                "failed to persist daemon heartbeat"
            );
            (*consecutive_failures >= MAX_HEARTBEAT_WRITE_FAILURES).then(|| {
                format!(
                    "heartbeat persistence failed {} consecutive times for config '{}': {e}",
                    *consecutive_failures, state.config_id
                )
            })
        }
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

fn run_supervisor_scheduler_pass(
    targets: &[luther_workflow::daemon::scheduler::SchedulerTarget],
    conn: &rusqlite::Connection,
) -> Result<luther_workflow::daemon::scheduler::RunSummary, luther_workflow::persistence::PersistenceError> {
    let queries = targets
        .iter()
        .map(|_| SystemGithubIssueQuery::new(SystemGithubCommandRunner))
        .collect::<Vec<_>>();
    let query_refs = queries
        .iter()
        .map(|query| query as &dyn luther_workflow::adapters::github_issues::GithubIssueQuery)
        .collect::<Vec<_>>();
    let launcher = DaemonWorkflowLauncher::new("supervisor".to_string());
    luther_workflow::daemon::scheduler::run_multi_target_once(
        targets,
        &query_refs,
        conn,
        &launcher,
    )
    .map_err(luther_workflow::persistence::PersistenceError::from)
}

async fn run_supervisor_scheduler_pass_blocking(
    targets: &[luther_workflow::daemon::scheduler::SchedulerTarget],
) -> Result<luther_workflow::daemon::scheduler::RunSummary, String> {
    let targets = targets.to_vec();
    match tokio::task::spawn_blocking(move || {
        let conn = open_daemon_db().map_err(|e| format!("failed to open daemon database: {e}"))?;
        run_supervisor_scheduler_pass(&targets, &conn).map_err(|e| e.to_string())
    })
    .await
    {
        Ok(result) => result,
        Err(e) => Err(format!("scheduler blocking task failed: {e}")),
    }
}

fn run_discovery_scheduler_pass(
    target: &luther_workflow::daemon::scheduler::SchedulerTarget,
    conn: &rusqlite::Connection,
) -> Result<luther_workflow::daemon::scheduler::RunSummary, luther_workflow::persistence::PersistenceError> {
    let query = SystemGithubIssueQuery::new(SystemGithubCommandRunner);
    let launcher = DaemonWorkflowLauncher::new(target.config_id.clone());
    luther_workflow::daemon::scheduler::run_multi_target_once(
        std::slice::from_ref(target),
        &[&query as &dyn luther_workflow::adapters::github_issues::GithubIssueQuery],
        conn,
        &launcher,
    )
    .map_err(luther_workflow::persistence::PersistenceError::from)
}

async fn run_discovery_scheduler_pass_blocking(
    target: &luther_workflow::daemon::scheduler::SchedulerTarget,
) -> Result<luther_workflow::daemon::scheduler::RunSummary, String> {
    let target = target.clone();
    match tokio::task::spawn_blocking(move || {
        let conn = open_daemon_db().map_err(|e| format!("failed to open daemon database: {e}"))?;
        run_discovery_scheduler_pass(&target, &conn).map_err(|e| e.to_string())
    })
    .await
    {
        Ok(result) => result,
        Err(e) => Err(format!("scheduler blocking task failed: {e}")),
    }
}

async fn backoff_after_scheduler_failure(
    consecutive_failures: &mut u32,
    shutdown: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    *consecutive_failures = consecutive_failures.saturating_add(1);
    let exponent = (*consecutive_failures - 1).min(SCHEDULER_FAILURE_BACKOFF_MAX_EXPONENT);
    let secs = SCHEDULER_FAILURE_BACKOFF_SECS.saturating_mul(1_u64 << exponent);
    sleep_secs_with_shutdown(secs, shutdown).await;
}

fn reset_scheduler_failures(consecutive_failures: &mut u32) {
    *consecutive_failures = 0;
}

async fn run_daemon_supervisor_loop(
    store: &DaemonStore,
    state: &mut DaemonState,
    shutdown: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    scheduler_path: &std::path::Path,
    config_dir: &Option<std::path::PathBuf>,
    once: bool,
) -> Option<String> {
    use std::sync::atomic::Ordering;

    let scheduler = match load_daemon_scheduler_config(scheduler_path) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Error: Failed to load daemon scheduler config: {e}");
            return Some(format!("failed to load daemon scheduler config: {e}"));
        }
    };
    let targets = scheduler_targets(&scheduler, config_dir);
    if targets.is_empty() {
        eprintln!("Error: daemon scheduler config resolved no enabled targets");
        return Some("daemon scheduler config resolved no enabled targets".to_string());
    }
    let stale_timeout = scheduler
        .poll_interval_seconds
        .unwrap_or(300)
        .saturating_mul(4);
    let recovery_conn = match open_daemon_db() {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("Error: failed to open daemon database: {e}");
            return Some(format!("failed to open daemon database: {e}"));
        }
    };
    if let Err(e) = recover_stale_daemon_leases(&recovery_conn, stale_timeout) {
        eprintln!("Error: stale lease recovery failed: {e}");
        return Some(format!("stale lease recovery failed: {e}"));
    }
    drop(recovery_conn);
    let poll = scheduler.poll_interval_seconds.unwrap_or(300);
    let mut heartbeat_failures = 0;
    let mut scheduler_failures = 0;
    while !shutdown.load(Ordering::SeqCst) {
        match run_supervisor_scheduler_pass_blocking(&targets).await {
            Ok(summary)
                if summary.launched > 0
                    || summary.resumed > 0
                    || summary.suspended > 0
                    || summary.failed > 0 =>
            {
                reset_scheduler_failures(&mut scheduler_failures);
                println!(
                    "scheduler pass: {} launched, {} resumed, {} suspended, {} failed, {} skipped",
                    summary.launched,
                    summary.resumed,
                    summary.suspended,
                    summary.failed,
                    summary.skipped
                );
            }
            Ok(_) => reset_scheduler_failures(&mut scheduler_failures),
            Err(e) => {
                eprintln!("scheduler error: {e}");
                backoff_after_scheduler_failure(&mut scheduler_failures, shutdown).await;
            }
        }
        state.touch_heartbeat();
        if let Some(error) = write_daemon_heartbeat(store, state, &mut heartbeat_failures) {
            return Some(error);
        }
        if once {
            break;
        }
        sleep_secs_with_shutdown(poll, shutdown).await;
    }
    None
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
    target: luther_workflow::daemon::scheduler::SchedulerTarget,
    once: bool,
) -> Option<String> {
    use std::sync::atomic::Ordering;

    let stale_timeout = target
        .discovery
        .poll_interval_secs
        .unwrap_or(300)
        .saturating_mul(4);

    let recovery_conn = match open_daemon_db() {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("Error: failed to open daemon database: {e}");
            return Some(format!("failed to open daemon database: {e}"));
        }
    };
    if let Err(e) = recover_stale_daemon_leases(&recovery_conn, stale_timeout) {
        eprintln!("Error: stale lease recovery failed: {e}");
        return Some(format!("stale lease recovery failed: {e}"));
    }
    drop(recovery_conn);

    let poll = target.discovery.poll_interval_secs.unwrap_or(300);
    let mut heartbeat_failures = 0;
    let mut scheduler_failures = 0;
    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        match run_discovery_scheduler_pass_blocking(&target).await {
            Ok(summary)
                if summary.launched > 0
                    || summary.resumed > 0
                    || summary.suspended > 0
                    || summary.failed > 0 =>
            {
                reset_scheduler_failures(&mut scheduler_failures);
                println!(
                    "scheduler pass: {} launched, {} resumed, {} suspended, {} failed, {} skipped",
                    summary.launched,
                    summary.resumed,
                    summary.suspended,
                    summary.failed,
                    summary.skipped
                );
            }
            Ok(_) => reset_scheduler_failures(&mut scheduler_failures),
            Err(e) => {
                eprintln!("scheduler error: {e}");
                backoff_after_scheduler_failure(&mut scheduler_failures, shutdown).await;
            }
        }
        state.touch_heartbeat();
        if let Some(error) = write_daemon_heartbeat(store, state, &mut heartbeat_failures) {
            return Some(error);
        }
        if once {
            break;
        }
        sleep_secs_with_shutdown(poll, shutdown).await;
    }
    None
}

/// Async sleep up to `secs` that wakes early when shutdown is requested.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P06
async fn sleep_secs_with_shutdown(
    secs: u64,
    shutdown: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use std::sync::atomic::Ordering;
    let ticks = secs.saturating_mul(1_000 / HEARTBEAT_TICK_MILLIS);
    for _ in 0..ticks {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(HEARTBEAT_TICK_MILLIS)).await;
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
) -> Option<String> {
    use std::sync::atomic::Ordering;

    let mut ticks: u32 = 0;
    let mut heartbeat_failures = 0;
    while !shutdown.load(Ordering::SeqCst) {
        tokio::time::sleep(tokio::time::Duration::from_millis(HEARTBEAT_TICK_MILLIS)).await;
        ticks += 1;
        if ticks >= HEARTBEAT_WRITE_TICKS {
            ticks = 0;
            state.touch_heartbeat();
            if let Some(error) = write_daemon_heartbeat(store, state, &mut heartbeat_failures) {
                return Some(error);
            }
        }
    }
    None
}
