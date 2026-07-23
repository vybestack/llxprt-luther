use super::*;

/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn require_runs_store(run_id: &str) -> SqliteStore {
    match open_runs_store() {
        Ok(Some(store)) => store,
        Ok(None) => {
            eprintln!("Error: run '{run_id}' not found (no run registry)");
            process::exit(1);
        }
        Err(e) => {
            eprintln!("Error: failed to open run registry while loading run '{run_id}': {e}");
            process::exit(1);
        }
    }
}

/// Load a run record from the store, exiting cleanly when absent.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn load_run_or_exit(store: &SqliteStore, run_id: &str) -> RunMetadata {
    match store.get_run(run_id) {
        Ok(Some(md)) => md,
        Ok(None) => {
            eprintln!("Error: run '{run_id}' not found");
            process::exit(1);
        }
        Err(e) => {
            eprintln!("Error: failed to read run '{run_id}' from run registry: {e}");
            process::exit(1);
        }
    }
}

/// Load all runs from the registry, applying config/state filters (issue #51).
/// @plan:issue-51
pub fn load_filtered_runs(
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

/// Handle `runs list` (issue #51).
/// @plan:issue-51
pub fn handle_runs_list(args: &luther_workflow::cli::RunsListArgs) {
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
pub fn print_runs_table(runs: &[RunMetadata]) {
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
pub fn truncate_field(value: &str, width: usize) -> String {
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
pub fn handle_runs_show(args: &luther_workflow::cli::RunsShowArgs) {
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
pub fn run_log_path(run_id: &str) -> std::path::PathBuf {
    luther_workflow::runtime_paths::get_log_dir().join(format!("{run_id}.log"))
}

/// Resolve the effective log path for a run, preferring the persisted
/// `RunMetadata.log_path` and falling back to the conventional path.
/// @plan:issue-51
pub fn effective_log_path(md: &RunMetadata, run_id: &str) -> std::path::PathBuf {
    md.log_path
        .as_deref()
        .map_or_else(|| run_log_path(run_id), std::path::PathBuf::from)
}

/// Render `runs show` as JSON (issue #51).
/// @plan:issue-51
pub fn print_runs_show_json(
    md: &RunMetadata,
    events: &[luther_workflow::persistence::EventRecord],
    artifacts: &[luther_workflow::persistence::ArtifactRecord],
    log_path: &std::path::Path,
    log_exists: bool,
) {
    let mut value = run_metadata_to_json(md);
    let Some(obj) = value.as_object_mut() else {
        eprintln!("Error: internal run metadata json is not an object");
        process::exit(1);
    };
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
        "llxprt_diagnostics".to_string(),
        serde_json::json!(super::diagnostics::project(
            events,
            md.artifact_root.as_deref()
        )),
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
pub fn print_runs_show_info(md: &RunMetadata) {
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
    if let Some(failure) = &md.failure_cleanup {
        println!();
        println!("Work Failure:");
        println!("  Failed step: {}", failure.failed_step);
        println!("  Reason: {}", failure.failure_reason);
        println!("  Cleanup step: {}", failure.cleanup_step);
        println!("  Cleanup succeeded: {}", failure.cleanup_succeeded);
    }
}

/// Render the Paths + Processes sections of `runs show` (issue #51).
/// @plan:issue-51
pub fn print_runs_show_paths_and_procs(
    md: &RunMetadata,
    log_path: &std::path::Path,
    log_exists: bool,
) {
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
pub fn print_runs_show_events(
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
pub fn print_runs_show_human(
    md: &RunMetadata,
    events: &[luther_workflow::persistence::EventRecord],
    artifacts: &[luther_workflow::persistence::ArtifactRecord],
    log_path: &std::path::Path,
    log_exists: bool,
) {
    print_runs_show_info(md);
    print_runs_show_paths_and_procs(md, log_path, log_exists);
    print_runs_show_scope_control(md);
    super::diagnostics::print_human(events, md.artifact_root.as_deref());
    print_runs_show_events(events, artifacts);
}

/// Render the scope-control section of `runs show` (issue #142).
fn print_runs_show_scope_control(md: &RunMetadata) {
    println!();
    let status = luther_workflow::engine::executors::scope_control::project_scope_status(
        md.artifact_root.as_deref(),
        &md.run_id,
    );
    let human = luther_workflow::engine::executors::scope_control::scope_status_to_human(&status);
    for line in human.lines() {
        println!("{line}");
    }
}

/// Resolve the run id for `runs tail` from args or active heartbeats (issue #51).
/// @plan:issue-51
pub async fn resolve_tail_run_id(
    args: &luther_workflow::cli::RunsTailArgs,
) -> Result<String, String> {
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
pub fn tail_lines(path: &std::path::Path, n: usize) -> std::io::Result<Vec<String>> {
    use std::collections::VecDeque;
    use std::io::BufRead;

    if n == 0 {
        return Ok(Vec::new());
    }
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    let mut tail: VecDeque<String> = VecDeque::with_capacity(n);
    for chunk in reader.split(b'\n') {
        let chunk = chunk?;
        if tail.len() == n {
            tail.pop_front();
        }
        tail.push_back(String::from_utf8_lossy(&chunk).into_owned());
    }
    Ok(tail.into_iter().collect())
}

/// Handle `runs tail` (issue #51).
/// @plan:issue-51
pub async fn handle_runs_tail(args: &luther_workflow::cli::RunsTailArgs) {
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
            Ok(None) => run_log_path(&run_id),
            Err(err) => {
                eprintln!("Warning: failed to read run '{run_id}' for log path: {err}");
                run_log_path(&run_id)
            }
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
    let tail_path = log_path.clone();
    let tail_count = args.lines;
    let lines = match tokio::task::spawn_blocking(move || tail_lines(&tail_path, tail_count)).await
    {
        Ok(Ok(lines)) => lines,
        Ok(Err(e)) => {
            eprintln!("Error: failed to read log file {}: {e}", log_path.display());
            process::exit(1);
        }
        Err(e) => {
            eprintln!("Error: tail task failed: {e}");
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
pub fn instance_pid(instance_id: &str) -> Option<u32> {
    instance_id
        .strip_prefix("monitor-")
        .and_then(|s| s.parse::<u32>().ok())
}

/// A single row of `runs ps` output describing a process's liveness.
/// @plan:issue-51
pub struct PsRow {
    pub instance_id: String,
    pub run_id: Option<String>,
    pub config_id: Option<String>,
    pub state: String,
    pub active_workers: u32,
    pub uptime_secs: i64,
    pub pid: Option<u32>,
    pub is_alive: bool,
    pub is_stale: bool,
    pub child_pids: Vec<u32>,
    pub stale_child_pids: Vec<u32>,
}

pub fn load_heartbeat_runs<'a>(
    store: Option<&SqliteStore>,
    run_ids: impl Iterator<Item = &'a str>,
) -> std::collections::BTreeMap<String, RunMetadata> {
    let Some(store) = store else {
        return std::collections::BTreeMap::new();
    };
    let run_ids = run_ids.collect::<Vec<_>>();
    match store.list_runs_by_ids(&run_ids) {
        Ok(runs) => runs
            .into_iter()
            .map(|metadata| (metadata.run_id.clone(), metadata))
            .collect(),
        Err(err) => {
            eprintln!("Warning: failed to load heartbeat runs for process view: {err}");
            std::collections::BTreeMap::new()
        }
    }
}

/// Build the `runs ps` rows from heartbeats and the run registry (issue #51).
/// @plan:issue-51
pub async fn build_ps_rows(config: Option<&str>) -> Result<Vec<PsRow>, String> {
    let heartbeats = read_all_heartbeats()
        .await
        .map_err(|e| format!("failed to read heartbeats: {e}"))?;
    let store = open_runs_store()?;
    let run_index = load_heartbeat_runs(
        store.as_ref(),
        heartbeats.values().filter_map(|hb| hb.run_id.as_deref()),
    );
    let now = chrono::Utc::now().timestamp();
    let mut rows = Vec::new();
    for hb in heartbeats.values() {
        let md = hb.run_id.as_deref().and_then(|rid| run_index.get(rid));
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
                .map(RunMetadata::are_child_pids_stale)
                .unwrap_or_default(),
        });
    }
    Ok(rows)
}

/// Convert a `runs ps` row to its stable JSON object (issue #51).
/// @plan:issue-51
pub fn ps_row_to_json(row: &PsRow) -> serde_json::Value {
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
pub async fn handle_runs_ps(args: &luther_workflow::cli::RunsPsArgs) {
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
pub fn format_child_pids(child_pids: &[u32], stale_child_pids: &[u32]) -> String {
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

    fn ps_row() -> PsRow {
        PsRow {
            instance_id: "monitor-4242".to_string(),
            run_id: Some("run-1".to_string()),
            config_id: Some("cfg".to_string()),
            state: "running".to_string(),
            active_workers: 2,
            uptime_secs: 90,
            pid: Some(4242),
            is_alive: true,
            is_stale: false,
            child_pids: vec![10, 11],
            stale_child_pids: vec![11],
        }
    }

    #[test]
    fn truncate_field_short_value_unchanged() {
        assert_eq!(truncate_field("abc", 10), "abc");
    }

    #[test]
    fn truncate_field_long_value_ellipsized() {
        let out = truncate_field("abcdefghij", 5);
        assert_eq!(out.chars().count(), 5);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn truncate_field_width_one() {
        assert_eq!(truncate_field("abcdef", 1), "a");
    }

    #[test]
    fn instance_pid_parses_monitor_prefix() {
        assert_eq!(instance_pid("monitor-4242"), Some(4242));
        assert_eq!(instance_pid("monitor-notnum"), None);
        assert_eq!(instance_pid("other-1"), None);
    }

    #[test]
    fn run_log_path_ends_with_run_id_log() {
        let path = run_log_path("run-xyz");
        assert!(path.to_string_lossy().ends_with("run-xyz.log"));
    }

    #[test]
    fn effective_log_path_prefers_metadata() {
        let mut md = RunMetadata::new("run-1", "wf", "cfg");
        md.log_path = Some("/custom/path.log".to_string());
        let path = effective_log_path(&md, "run-1");
        assert_eq!(path.to_string_lossy(), "/custom/path.log");
    }

    #[test]
    fn effective_log_path_falls_back_to_conventional() {
        let md = RunMetadata::new("run-1", "wf", "cfg");
        let path = effective_log_path(&md, "run-1");
        assert!(path.to_string_lossy().ends_with("run-1.log"));
    }

    #[test]
    fn format_child_pids_empty_is_dash() {
        assert_eq!(format_child_pids(&[], &[]), "-");
    }

    #[test]
    fn format_child_pids_marks_stale() {
        let out = format_child_pids(&[10, 11], &[11]);
        assert_eq!(out, "10, 11 (stale)");
    }

    #[test]
    fn ps_row_to_json_serializes_all_fields() {
        let row = ps_row();
        let value = ps_row_to_json(&row);
        assert_eq!(value.get("instance_id").unwrap(), "monitor-4242");
        assert_eq!(value.get("run_id").unwrap(), "run-1");
        assert_eq!(value.get("active_workers").unwrap(), 2);
        assert_eq!(value.get("pid").unwrap(), 4242);
        assert_eq!(value.get("is_alive").unwrap(), true);
        assert_eq!(
            value.get("child_pids").unwrap(),
            &serde_json::json!([10, 11])
        );
        assert_eq!(
            value.get("stale_child_pids").unwrap(),
            &serde_json::json!([11])
        );
    }

    #[test]
    fn tail_lines_zero_returns_empty() {
        let path = std::path::Path::new("/nonexistent/does-not-matter");
        let out = tail_lines(path, 0).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn tail_lines_reads_last_n_lines() {
        let dir = std::env::temp_dir();
        let file = dir.join(format!("inspect-tail-{}.log", std::process::id()));
        std::fs::write(&file, "l1\nl2\nl3\nl4\nl5\n").unwrap();
        let out = tail_lines(&file, 2).unwrap();
        assert_eq!(out, vec!["l4".to_string(), "l5".to_string()]);
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn load_heartbeat_runs_none_store_is_empty() {
        let empty = load_heartbeat_runs(None, std::iter::empty());
        assert!(empty.is_empty());
    }

    fn sample_metadata() -> RunMetadata {
        let mut md = RunMetadata::new("run-print", "wf-type", "cfg-print");
        md.repository = Some("owner/repo".to_string());
        md.issue_number = Some(125);
        md.pr_number = Some(126);
        md.head_sha = Some("abc123".to_string());
        md.current_step = Some("do_work".to_string());
        md.previous_step = Some("prep".to_string());
        md.previous_outcome = Some("success".to_string());
        md.workspace_path = Some("/tmp/ws".to_string());
        md.artifact_root = Some("/tmp/artifacts".to_string());
        md
    }

    fn sample_event() -> luther_workflow::persistence::EventRecord {
        luther_workflow::persistence::EventRecord {
            run_id: "run-print".to_string(),
            step_id: "do_work".to_string(),
            outcome: "success".to_string(),
            event_type: "step_completed".to_string(),
            details: Some("all good".to_string()),
            timestamp: chrono::Utc::now(),
        }
    }

    fn sample_artifact() -> luther_workflow::persistence::ArtifactRecord {
        let mut a = luther_workflow::persistence::ArtifactRecord::new(
            "run-print",
            "/tmp/artifacts/out.json",
            "output",
            "do_work",
        );
        a.size_bytes = Some(2048);
        a
    }

    #[test]
    fn print_runs_table_empty_and_populated_do_not_panic() {
        print_runs_table(&[]);
        let md = sample_metadata();
        print_runs_table(std::slice::from_ref(&md));
    }

    #[test]
    fn print_runs_show_info_renders_all_sections() {
        let md = sample_metadata();
        print_runs_show_info(&md);
    }

    #[test]
    fn print_runs_show_info_handles_missing_optional_fields() {
        let md = RunMetadata::new("run-bare", "wf", "cfg");
        print_runs_show_info(&md);
    }

    #[test]
    fn print_runs_show_paths_and_procs_without_children() {
        let md = sample_metadata();
        let log = std::path::Path::new("/tmp/artifacts/run-print.log");
        print_runs_show_paths_and_procs(&md, log, false);
    }

    #[test]
    fn print_runs_show_paths_and_procs_with_children() {
        let mut md = sample_metadata();
        md.child_pids = vec![4242, 4243];
        let log = std::path::Path::new("/tmp/artifacts/run-print.log");
        print_runs_show_paths_and_procs(&md, log, true);
    }

    #[test]
    fn print_runs_show_events_empty_and_populated() {
        print_runs_show_events(&[], &[]);
        let events = vec![sample_event()];
        let artifacts = vec![sample_artifact()];
        print_runs_show_events(&events, &artifacts);
    }

    #[test]
    fn print_runs_show_events_truncates_to_last_fifteen() {
        let events: Vec<_> = (0..20).map(|_| sample_event()).collect();
        print_runs_show_events(&events, &[]);
    }

    #[test]
    fn print_runs_show_human_renders_full_report() {
        let md = sample_metadata();
        let events = vec![sample_event()];
        let artifacts = vec![sample_artifact()];
        let log = std::path::Path::new("/tmp/artifacts/run-print.log");
        print_runs_show_human(&md, &events, &artifacts, log, true);
    }

    #[test]
    fn print_runs_show_json_serializes_run_and_children() {
        let md = sample_metadata();
        let events = vec![sample_event()];
        let artifacts = vec![sample_artifact()];
        let log = std::path::Path::new("/tmp/artifacts/run-print.log");
        // Exercises the JSON rendering path end-to-end.
        print_runs_show_json(&md, &events, &artifacts, log, true);
    }

    #[test]
    fn effective_log_path_uses_conventional_when_metadata_absent() {
        let md = sample_metadata();
        let path = effective_log_path(&md, "run-print");
        assert!(path.to_string_lossy().ends_with("run-print.log"));
    }

    #[test]
    fn ps_row_to_json_reflects_dead_process_flags() {
        let mut row = ps_row();
        row.is_alive = false;
        row.is_stale = true;
        row.pid = None;
        row.run_id = None;
        row.config_id = None;
        let value = ps_row_to_json(&row);
        assert_eq!(value.get("is_alive").unwrap(), false);
        assert_eq!(value.get("is_stale").unwrap(), true);
        assert!(value.get("pid").unwrap().is_null());
    }

    #[test]
    fn format_child_pids_all_live() {
        assert_eq!(format_child_pids(&[1, 2, 3], &[]), "1, 2, 3");
    }
}
