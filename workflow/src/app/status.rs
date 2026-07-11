use super::daemon::filter_status_by_config;
use luther_workflow::monitor::heartbeat::{read_all_heartbeats, MonitorState};
use luther_workflow::persistence::WaitKind;

/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
pub fn wait_kind_for_step(step_id: &str) -> WaitKind {
    match step_id {
        "watch_pr_checks" => WaitKind::PrChecks,
        "collect_coderabbit_feedback" => WaitKind::CoderabbitReview,
        "merge_pr" | "wait_for_merge" => WaitKind::PrMerge,
        "launch_or_resume_child_workflow" | "dependency_child_workflow" => {
            WaitKind::DependencyChildWorkflow
        }
        "dependency_child_merge" | "wait_for_child_merge" => WaitKind::DependencyChildMerge,
        "rate_limit_backoff" | "github_rate_limit_backoff" => WaitKind::RateLimitBackoff,
        other => {
            eprintln!("Warning: unmapped wait step '{other}' defaulting to human_review");
            WaitKind::HumanReview
        }
    }
}

pub fn install_interrupt_handlers(interrupted: std::sync::Arc<std::sync::atomic::AtomicBool>) {
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

pub async fn handle_status_command(args: &luther_workflow::cli::StatusArgs) {
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

pub async fn read_status_heartbeats(
) -> std::collections::HashMap<String, luther_workflow::monitor::heartbeat::Heartbeat> {
    match read_all_heartbeats().await {
        Ok(hbs) => hbs,
        Err(e) => {
            eprintln!("Error reading heartbeats: {e}");
            std::collections::HashMap::new()
        }
    }
}

pub fn print_status_json(
    heartbeats: &std::collections::HashMap<String, luther_workflow::monitor::heartbeat::Heartbeat>,
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

pub fn print_status_human(
    args: &luther_workflow::cli::StatusArgs,
    heartbeats: &std::collections::HashMap<String, luther_workflow::monitor::heartbeat::Heartbeat>,
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

pub fn print_heartbeat_status(
    heartbeats: &std::collections::HashMap<String, luther_workflow::monitor::heartbeat::Heartbeat>,
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

pub fn monitor_state_label(state: MonitorState) -> &'static str {
    match state {
        MonitorState::Starting => "starting",
        MonitorState::Running => "running",
        MonitorState::Degraded => "degraded",
        MonitorState::Stopping => "stopping",
        MonitorState::Stopped => "stopped",
        MonitorState::Error => "error",
    }
}

pub fn print_requested_heartbeat_details(
    run_id: Option<&str>,
    heartbeats: &std::collections::HashMap<String, luther_workflow::monitor::heartbeat::Heartbeat>,
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

pub fn print_run_registry_error(error: &str) {
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
pub fn read_run_registry(
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
pub fn pid_liveness_label(md: &luther_workflow::persistence::RunMetadata) -> String {
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
pub fn next_step_label(md: &luther_workflow::persistence::RunMetadata) -> String {
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
pub fn run_metadata_to_json(md: &luther_workflow::persistence::RunMetadata) -> serde_json::Value {
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
pub fn print_run_registry(
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

#[cfg(test)]
#[path = "status_tests.rs"]
mod status_tests;
