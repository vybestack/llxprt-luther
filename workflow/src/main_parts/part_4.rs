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
    let runs_store = match open_runs_store() {
        Ok(store) => store,
        Err(e) => {
            eprintln!("Warning: run registry unavailable: {e}");
            None
        }
    };
    let all_runs = match runs_store.as_ref() {
        Some(store) => match store.list_runs() {
            Ok(runs) => runs,
            Err(e) => {
                eprintln!("Warning: run registry unavailable: failed to list runs: {e}");
                Vec::new()
            }
        },
        None => Vec::new(),
    };
    let filtered = filter.apply(&all_runs);
    let counts = RunCounts::from_runs(&filtered.runs);
    let recent_events = collect_selected_events(runs_store.as_ref(), filtered.selected.as_ref(), tail);
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
fn collect_selected_events(
    store: Option<&SqliteStore>,
    selected: Option<&RunMetadata>,
    tail: usize,
) -> Vec<EventRecord> {
    if tail == 0 {
        return Vec::new();
    }
    let Some(md) = selected else {
        return Vec::new();
    };
    let Some(store) = store else {
        return Vec::new();
    };
    load_recent_events(store.conn(), &md.run_id, tail).unwrap_or_else(|err| {
        eprintln!("Warning: failed to load recent events for {}: {err}", md.run_id);
        Vec::new()
    })
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
    let tail_path = log_path.clone();
    let tail_count = args.lines;
    let lines = match tokio::task::spawn_blocking(move || tail_lines(&tail_path, tail_count)).await {
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
    let all_runs = match store.as_ref() {
        Some(store) => store
            .list_runs()
            .map_err(|e| format!("failed to list run metadata: {e}"))?,
        None => Vec::new(),
    };
    let run_index = all_runs
        .iter()
        .map(|md| (md.run_id.as_str(), md))
        .collect::<std::collections::BTreeMap<_, _>>();
    let now = chrono::Utc::now().timestamp();
    let mut rows = Vec::new();
    for hb in heartbeats.values() {
        let md = hb
            .run_id
            .as_deref()
            .and_then(|rid| run_index.get(rid).copied());
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
mod part_4_tests;
