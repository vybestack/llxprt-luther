use super::run::DaemonWorkflowLauncher;
use super::runs::open_runs_store;
use super::service::daemon_config_id;
use super::status::install_interrupt_handlers;
use luther_workflow::adapters::github::SystemGithubCommandRunner;
use luther_workflow::adapters::github_issues::SystemGithubIssueQuery;
use luther_workflow::daemon::discovery::{discover, DiscoveryResult};
use luther_workflow::daemon::scheduler::{RunSummary, SchedulerTarget};
use luther_workflow::daemon::{
    is_daemon_alive, stop_daemon, DaemonState, DaemonStatus, DaemonStore, StopOutcome,
};
use luther_workflow::persistence::leases::{
    list_all_leases, list_leases_by_config, IssueLease, LeaseStatus,
};
use luther_workflow::persistence::{init_database, list_wait_states, RunMetadata, SqliteStore};
use luther_workflow::workflow::config_loader::{
    load_daemon_scheduler_config, resolve_discovery_config, resolve_workflow_config,
};
use std::process;

/// Dispatch the `daemon` command family.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
pub async fn handle_daemon_command(args: &luther_workflow::cli::DaemonArgs) {
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
pub fn resolve_discovery_for(
    config: &std::path::Path,
    config_dir: &Option<std::path::PathBuf>,
) -> luther_workflow::workflow::schema::DiscoveryConfig {
    let config_id = daemon_config_id(config);
    let (_, discovery) = resolve_config_and_discovery_for(&config_id, config_dir);
    discovery
}
pub fn resolve_config_and_discovery_for(
    config_id: &str,
    config_dir: &Option<std::path::PathBuf>,
) -> (
    luther_workflow::workflow::schema::WorkflowConfig,
    luther_workflow::workflow::schema::DiscoveryConfig,
) {
    let config_root = config_dir
        .as_deref()
        .unwrap_or(std::path::Path::new("config"));
    let cfg = match resolve_workflow_config(config_id, config_root) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("Error: Failed to resolve config '{config_id}': {e}");
            process::exit(1);
        }
    };
    let discovery = resolve_discovery_config(&cfg);
    (cfg, discovery)
}
/// Open the shared checkpoints database (creating schema if needed).
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P05
pub fn open_daemon_db(
) -> Result<rusqlite::Connection, luther_workflow::persistence::PersistenceError> {
    let db_path = luther_workflow::runtime_paths::get_data_dir().join("checkpoints.db");
    init_database(&db_path)?;
    rusqlite::Connection::open(&db_path)
        .map_err(luther_workflow::persistence::PersistenceError::from)
}
pub fn open_daemon_db_or_exit() -> rusqlite::Connection {
    match open_daemon_db() {
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
pub fn handle_daemon_discover_command(args: &luther_workflow::cli::DaemonDiscoverArgs) {
    let discovery = resolve_discovery_for(&args.config, &args.config_dir);
    let config_id = daemon_config_id(&args.config);
    let conn = open_daemon_db_or_exit();
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
pub fn print_discovery_json(result: &DiscoveryResult) {
    let eligible: Vec<serde_json::Value> = result
        .eligible
        .iter()
        .map(|routed| {
            serde_json::json!({
                "number": routed.issue.number,
                "title": routed.issue.title,
                "labels": routed.issue.labels,
                "workflow_type_id": routed.workflow_type_id,
                "config_id": routed.config_id,
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
pub fn print_discovery_text(result: &DiscoveryResult) {
    println!("Eligible issues ({}):", result.eligible.len());
    for routed in &result.eligible {
        println!(
            "  #{} {} [{}] workflow={} config={}",
            routed.issue.number,
            routed.issue.title,
            routed.issue.labels.join(", "),
            routed.workflow_type_id.as_deref().unwrap_or("default"),
            routed.config_id.as_deref().unwrap_or("default")
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
pub fn handle_daemon_queue_command(args: &luther_workflow::cli::DaemonQueueArgs) {
    let conn = open_daemon_db_or_exit();
    let leases = collect_queue_leases(&conn, args);
    if args.json {
        print_queue_json(&conn, &leases);
    } else {
        print_queue_text(&conn, &leases);
    }
}
/// Collect leases for the queue command honoring `--config` and `--status`.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P05
pub fn collect_queue_leases(
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
pub fn parse_status_or_exit(status: &str) -> LeaseStatus {
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
pub fn print_queue_json(conn: &rusqlite::Connection, leases: &[IssueLease]) {
    let waits = queue_wait_summaries(conn);
    let metadata = queue_run_metadata(conn, leases);
    let items: Vec<serde_json::Value> = leases
        .iter()
        .map(|l| {
            let wait = l.run_id.as_deref().and_then(|run_id| waits.get(run_id));
            let run_metadata = l.run_id.as_deref().and_then(|run_id| metadata.get(run_id));
            serde_json::json!({
                "issue_repo": l.issue_repo,
                "issue_number": l.issue_number,
                "config_id": l.config_id,
                "run_id": l.run_id,
                "status": l.status.to_string(),
                "active_slot_used": l.status.is_active(),
                "wait": wait,
                "workspace_path": run_metadata.as_ref().and_then(|md| md.workspace_path.clone()),
                "artifact_root": run_metadata.as_ref().and_then(|md| md.artifact_root.clone()),
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
pub fn print_queue_text(conn: &rusqlite::Connection, leases: &[IssueLease]) {
    if leases.is_empty() {
        println!("Queue is empty.");
        return;
    }
    let waits = queue_wait_summaries(conn);
    let metadata = queue_run_metadata(conn, leases);
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
            let run_metadata = lease
                .run_id
                .as_deref()
                .and_then(|run_id| metadata.get(run_id));
            let paths = run_metadata
                .map(|md| {
                    let workspace = md.workspace_path.as_deref().unwrap_or("(none)");
                    let artifact = md.artifact_root.as_deref().unwrap_or("(none)");
                    format!(" work={workspace} artifacts={artifact}")
                })
                .unwrap_or_default();
            println!(
                "  {}#{} config={} run={} active_slot_used={}{}{}",
                lease.issue_repo,
                lease.issue_number,
                lease.config_id,
                run,
                active_slot,
                wait,
                paths
            );
        }
    }
}
pub fn queue_wait_summaries(
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
/// Load persisted run metadata for queue leases in one batch.
/// @plan:issue-117
pub fn queue_run_metadata(
    conn: &rusqlite::Connection,
    leases: &[IssueLease],
) -> std::collections::HashMap<String, luther_workflow::persistence::RunMetadata> {
    let run_ids = leases
        .iter()
        .filter_map(|lease| lease.run_id.as_deref())
        .collect::<Vec<_>>();
    let runs = match luther_workflow::persistence::list_runs_by_ids_with_conn(conn, &run_ids) {
        Ok(runs) => runs,
        Err(e) => {
            eprintln!("Warning: failed to load queue run metadata: {e}");
            return std::collections::HashMap::new();
        }
    };
    runs.into_iter()
        .map(|run| (run.run_id.clone(), run))
        .collect()
}
pub fn format_wait_summary(wait: &serde_json::Value) -> String {
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
/// Handle `daemon stop` for a single config or `--all`.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
pub fn handle_daemon_stop(store: &DaemonStore, args: &luther_workflow::cli::DaemonStopArgs) {
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
pub fn report_stop_outcome(config_id: &str, outcome: StopOutcome) {
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
pub fn stop_all_daemons(store: &DaemonStore) {
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
pub fn handle_daemon_status(store: &DaemonStore, args: &luther_workflow::cli::DaemonStatusArgs) {
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
pub fn daemon_state_json(state: &DaemonState) -> serde_json::Value {
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
pub fn daemon_status_single(store: &DaemonStore, config_id: &str, json: bool) {
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
pub fn daemon_display_status(state: &DaemonState, alive: bool) -> String {
    if state.status == DaemonStatus::Running && !alive {
        "stale".to_string()
    } else {
        state.status.to_string()
    }
}
/// Render the aggregate status across all known daemons.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
pub fn daemon_status_all(store: &DaemonStore, json: bool) {
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
pub fn heartbeat_run_index(
    store: Option<&SqliteStore>,
    heartbeats: &std::collections::HashMap<String, luther_workflow::monitor::heartbeat::Heartbeat>,
) -> std::collections::BTreeMap<String, RunMetadata> {
    let Some(store) = store else {
        return std::collections::BTreeMap::new();
    };
    let run_ids = heartbeats
        .values()
        .filter_map(|hb| hb.run_id.as_deref())
        .collect::<Vec<_>>();
    match store.list_runs_by_ids(&run_ids) {
        Ok(runs) => runs
            .into_iter()
            .map(|metadata| (metadata.run_id.clone(), metadata))
            .collect(),
        Err(err) => {
            eprintln!("Warning: failed to read heartbeat runs for filtering: {err}");
            std::collections::BTreeMap::new()
        }
    }
}
/// Filter status heartbeats and run registry results by config id (issue #51).
///
/// The registry is opened once to resolve heartbeat -> config relationships; a
/// registry error short-circuits the run filtering but still scopes heartbeats.
/// @plan:issue-51
pub fn filter_status_by_config(
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
    let heartbeat_runs = heartbeat_run_index(store.as_ref(), &heartbeats);
    let filtered_hbs = if store.is_some() {
        heartbeats
            .into_iter()
            .filter(|(_, hb)| {
                hb.run_id
                    .as_deref()
                    .and_then(|run_id| heartbeat_runs.get(run_id))
                    .is_some_and(|metadata| metadata.config_id == config_id)
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
mod lock;
mod supervisor;
pub use lock::*;
pub use supervisor::*;

#[cfg(test)]
#[path = "mod_tests.rs"]
mod mod_tests;
