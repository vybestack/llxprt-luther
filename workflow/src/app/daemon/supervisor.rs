use super::*;
use luther_workflow::daemon::scheduler::SchedulerError;

#[cfg(test)]
#[path = "supervisor_tests.rs"]
mod supervisor_tests;

/// Run a foreground daemon for the given config with clean Ctrl-C handling.
///
/// When the resolved `[discovery]` config is enabled, the daemon drives the
/// discovery/launch scheduler (still writing heartbeats); otherwise it keeps
/// the original heartbeat-only behavior. `once` performs a single pass.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P06
pub async fn handle_daemon_run(
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
            let config_root = config_dir
                .as_deref()
                .unwrap_or(std::path::Path::new("config"));
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

pub fn scheduler_targets(
    scheduler: &luther_workflow::workflow::schema::DaemonSchedulerConfig,
    config_dir: &Option<std::path::PathBuf>,
) -> Vec<SchedulerTarget> {
    let config_root = config_dir
        .as_deref()
        .unwrap_or(std::path::Path::new("config"));
    scheduler
        .targets
        .iter()
        .filter_map(|target| scheduler_target(target, scheduler, config_root))
        .collect()
}

pub fn scheduler_target(
    target: &luther_workflow::workflow::schema::DaemonTargetConfig,
    scheduler: &luther_workflow::workflow::schema::DaemonSchedulerConfig,
    config_root: &std::path::Path,
) -> Option<SchedulerTarget> {
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
    discovery.enabled.then(|| {
        SchedulerTarget::new(
            target.config_id.clone(),
            discovery,
            path_bases,
            parent_path_bases,
            config_root.to_path_buf(),
        )
    })
}

/// Build the single-config scheduler target used by `daemon run/start` without
/// a supervisor config. This keeps one-shot and long-running daemon discovery
/// on the same per-run path isolation path as the multi-target scheduler.
/// @plan:issue-117
pub fn discovery_scheduler_target(
    config_id: &str,
    discovery: &luther_workflow::workflow::schema::DiscoveryConfig,
    cfg: &luther_workflow::workflow::schema::WorkflowConfig,
    config_root: &std::path::Path,
) -> SchedulerTarget {
    SchedulerTarget::new(
        config_id.to_string(),
        discovery.clone(),
        daemon_path_bases_from_config(cfg),
        parent_path_bases_from_config(cfg, config_root),
        config_root.to_path_buf(),
    )
}

pub fn parent_path_bases_from_config(
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

pub fn parent_path_bases_entry(
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
pub fn daemon_path_bases_from_config(
    cfg: &luther_workflow::workflow::schema::WorkflowConfig,
) -> luther_workflow::daemon::launcher::DaemonPathBases {
    luther_workflow::daemon::launcher::DaemonPathBases {
        work_dir_base: cfg.variables.get("work_dir").map(std::path::PathBuf::from),
        artifact_dir_base: cfg
            .variables
            .get("artifact_dir")
            .map(std::path::PathBuf::from),
    }
}

pub const MAX_HEARTBEAT_WRITE_FAILURES: u32 = 3;
pub const HEARTBEAT_TICK_MILLIS: u64 = 200;
pub const HEARTBEAT_WRITE_TICKS: u32 = 150;
pub const SCHEDULER_FAILURE_BACKOFF_SECS: u64 = 5;
pub const SCHEDULER_FAILURE_BACKOFF_MAX_EXPONENT: u32 = 4;

pub fn write_daemon_heartbeat(
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

pub fn recover_stale_daemon_leases(
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

const MAX_SCHEDULER_DETAIL_LOGS_PER_KIND: usize = 10;

#[derive(Debug, PartialEq, Eq)]
struct SchedulerDiagnosticPlan {
    summary: String,
    preserved_details_to_log: usize,
    preserved_details_dropped: usize,
    skipped_details_to_log: usize,
    skipped_details_dropped: usize,
    artifact_warnings_to_log: usize,
    artifact_warnings_dropped: usize,
}

fn scheduler_diagnostic_plan(summary: &RunSummary) -> SchedulerDiagnosticPlan {
    let preserved_details_to_log = summary
        .lease_state_preserved_details
        .len()
        .min(MAX_SCHEDULER_DETAIL_LOGS_PER_KIND);
    let preserved_details_dropped = summary
        .lease_state_preserved_details_dropped
        .saturating_add(summary.lease_state_preserved_details.len() - preserved_details_to_log);
    let skipped_details_to_log = summary
        .skipped_poll_details
        .len()
        .min(MAX_SCHEDULER_DETAIL_LOGS_PER_KIND);
    let skipped_details_dropped = summary
        .skipped_poll_details_dropped
        .saturating_add(summary.skipped_poll_details.len() - skipped_details_to_log);
    let artifact_warnings_to_log = summary
        .artifact_warnings
        .len()
        .min(MAX_SCHEDULER_DETAIL_LOGS_PER_KIND);
    let artifact_warnings_dropped = summary
        .artifact_warnings_dropped
        .saturating_add(summary.artifact_warnings.len() - artifact_warnings_to_log);
    let summary_message = format!(
        concat!(
            "scheduler pass: {launched} launched, {resumed} resumed, ",
            "{suspended} suspended, {failed} failed, ",
            "{preserved} lease states preserved, ",
            "{preserved_dropped} preserved details dropped, {skipped} skipped, ",
            "{pollable} pollable waits, {applied} polls applied, ",
            "{polls_skipped} polls skipped, {skip_dropped} skip details dropped, ",
            "{warnings} artifact warnings, {warning_dropped} warning details dropped"
        ),
        launched = summary.launched,
        resumed = summary.resumed,
        suspended = summary.suspended,
        failed = summary.failed,
        preserved = summary.lease_states_preserved,
        preserved_dropped = preserved_details_dropped,
        skipped = summary.skipped,
        pollable = summary.pollable_waits,
        applied = summary.polls_applied,
        polls_skipped = summary.skipped_polls,
        skip_dropped = skipped_details_dropped,
        warnings = summary.artifact_warning_count(),
        warning_dropped = artifact_warnings_dropped,
    );
    SchedulerDiagnosticPlan {
        summary: summary_message,
        preserved_details_to_log,
        preserved_details_dropped,
        skipped_details_to_log,
        skipped_details_dropped,
        artifact_warnings_to_log,
        artifact_warnings_dropped,
    }
}

fn report_scheduler_summary(summary: &RunSummary) {
    let plan = scheduler_diagnostic_plan(summary);
    tracing::info!(
        launched = summary.launched,
        resumed = summary.resumed,
        suspended = summary.suspended,
        failed = summary.failed,
        lease_states_preserved = summary.lease_states_preserved,
        lease_state_preserved_details_dropped = plan.preserved_details_dropped,
        skipped = summary.skipped,
        pollable_waits = summary.pollable_waits,
        polls_applied = summary.polls_applied,
        polls_skipped = summary.skipped_polls,
        skipped_details_dropped = plan.skipped_details_dropped,
        artifact_warnings = summary.artifact_warning_count(),
        artifact_warnings_dropped = plan.artifact_warnings_dropped,
        message = %plan.summary,
        "scheduler pass completed"
    );
    for detail in summary
        .lease_state_preserved_details
        .iter()
        .take(plan.preserved_details_to_log)
    {
        tracing::warn!(
            run_id = %detail.run_id,
            current_status = ?detail.current_status,
            current_run_id = detail.current_run_id.as_deref().unwrap_or("none"),
            "scheduler preserved newer lease state"
        );
    }
    for detail in summary
        .skipped_poll_details
        .iter()
        .take(plan.skipped_details_to_log)
    {
        tracing::debug!(
            run_id = %detail.run_id,
            lease_id = detail.lease_id.as_deref().unwrap_or("none"),
            reason = ?detail.reason,
            lease_transition_reason = detail.lease_transition_reason.unwrap_or("none"),
            "scheduler poll skipped"
        );
    }
    for warning in summary
        .artifact_warnings
        .iter()
        .take(plan.artifact_warnings_to_log)
    {
        tracing::warn!(
            run_id = %warning.run_id,
            phase = ?warning.phase,
            error = %warning.error,
            "scheduler artifact persistence failed after commit"
        );
    }
}

pub fn run_supervisor_scheduler_pass(
    targets: &[SchedulerTarget],
    conn: &rusqlite::Connection,
) -> Result<RunSummary, SchedulerError> {
    let queries = targets
        .iter()
        .map(|_| SystemGithubIssueQuery::new(SystemGithubCommandRunner))
        .collect::<Vec<_>>();
    let query_refs = queries
        .iter()
        .map(|query| query as &dyn luther_workflow::adapters::github_issues::GithubIssueQuery)
        .collect::<Vec<_>>();
    let launcher = DaemonWorkflowLauncher::new("supervisor".to_string());
    luther_workflow::daemon::scheduler::run_multi_target_once(targets, &query_refs, conn, &launcher)
}

pub async fn run_supervisor_scheduler_pass_blocking(
    targets: &[SchedulerTarget],
) -> Result<RunSummary, String> {
    let targets = targets.to_vec();
    match tokio::task::spawn_blocking(move || {
        let conn = open_daemon_db().map_err(|e| format!("failed to open daemon database: {e}"))?;
        run_supervisor_scheduler_pass(&targets, &conn).map_err(|e| e.to_string())
    })
    .await
    {
        Ok(result) => result,
        Err(e) => Err(scheduler_join_error(e)),
    }
}

pub fn run_discovery_scheduler_pass(
    target: &SchedulerTarget,
    conn: &rusqlite::Connection,
) -> Result<RunSummary, SchedulerError> {
    let query = SystemGithubIssueQuery::new(SystemGithubCommandRunner);
    let launcher = DaemonWorkflowLauncher::new(target.config_id.clone());
    luther_workflow::daemon::scheduler::run_multi_target_once(
        std::slice::from_ref(target),
        &[&query as &dyn luther_workflow::adapters::github_issues::GithubIssueQuery],
        conn,
        &launcher,
    )
}

pub async fn run_discovery_scheduler_pass_blocking(
    target: &SchedulerTarget,
) -> Result<RunSummary, String> {
    let target = target.clone();
    match tokio::task::spawn_blocking(move || {
        let conn = open_daemon_db().map_err(|e| format!("failed to open daemon database: {e}"))?;
        run_discovery_scheduler_pass(&target, &conn).map_err(|e| e.to_string())
    })
    .await
    {
        Ok(result) => result,
        Err(e) => Err(scheduler_join_error(e)),
    }
}

pub fn scheduler_join_error(error: tokio::task::JoinError) -> String {
    if error.is_panic() {
        format!("scheduler blocking task panicked: {error}")
    } else if error.is_cancelled() {
        format!("scheduler blocking task was cancelled: {error}")
    } else {
        format!("scheduler blocking task failed: {error}")
    }
}

pub async fn backoff_after_scheduler_failure(
    consecutive_failures: &mut u32,
    shutdown: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    *consecutive_failures = consecutive_failures.saturating_add(1);
    let exponent = (*consecutive_failures - 1).min(SCHEDULER_FAILURE_BACKOFF_MAX_EXPONENT);
    let secs = SCHEDULER_FAILURE_BACKOFF_SECS.saturating_mul(1_u64 << exponent);
    sleep_secs_with_shutdown(secs, shutdown).await;
}

pub fn reset_scheduler_failures(consecutive_failures: &mut u32) {
    *consecutive_failures = 0;
}

pub async fn run_daemon_supervisor_loop(
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
            Ok(summary) => {
                reset_scheduler_failures(&mut scheduler_failures);
                if summary.launched > 0
                    || summary.resumed > 0
                    || summary.suspended > 0
                    || summary.failed > 0
                    || summary.lease_states_preserved > 0
                    || summary.skipped_polls > 0
                    || !summary.skipped_poll_details.is_empty()
                    || !summary.artifact_warnings.is_empty()
                {
                    report_scheduler_summary(&summary);
                }
            }
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
pub async fn run_daemon_discovery_loop(
    store: &DaemonStore,
    state: &mut DaemonState,
    shutdown: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    target: SchedulerTarget,
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
            Ok(summary) => {
                reset_scheduler_failures(&mut scheduler_failures);
                if summary.launched > 0
                    || summary.resumed > 0
                    || summary.suspended > 0
                    || summary.failed > 0
                    || summary.lease_states_preserved > 0
                    || summary.skipped_polls > 0
                    || !summary.skipped_poll_details.is_empty()
                    || !summary.artifact_warnings.is_empty()
                {
                    report_scheduler_summary(&summary);
                }
            }
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
pub async fn sleep_secs_with_shutdown(
    secs: u64,
    shutdown: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use std::sync::atomic::Ordering;
    let total_millis = secs.saturating_mul(1_000);
    let ticks = total_millis.div_ceil(HEARTBEAT_TICK_MILLIS);
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
pub async fn run_daemon_heartbeat_loop(
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
