use super::runs::open_runs_store;
use luther_workflow::daemon::{is_daemon_alive, DaemonStore};
use luther_workflow::monitor::heartbeat::MonitorState;
use luther_workflow::monitor::snapshot::{
    render_snapshot, resolve_snapshot_count, separator_line, DaemonSummary, MonitorFilter,
    MonitorSnapshot, RunCounts, CLEAR_SCREEN,
};
use luther_workflow::persistence::{load_recent_events, EventRecord, RunMetadata, SqliteStore};

/// Handle the `monitor` command (issue #52).
///
/// Continuous, plain-CLI watch view. This is the thin I/O + loop + signal
/// shell; all modeling/filtering/rendering lives in the pure `monitor::snapshot`
/// module. Strictly read-only: it never stops daemons or cancels runs.
/// @plan:issue-52
pub async fn handle_monitor_command(args: &luther_workflow::cli::MonitorArgs) {
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
pub fn render_one_snapshot(filter: &MonitorFilter, tail: usize, clear: bool) {
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
pub fn collect_snapshot(filter: &MonitorFilter, tail: usize) -> MonitorSnapshot {
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
    let recent_events =
        collect_selected_events(runs_store.as_ref(), filtered.selected.as_ref(), tail);
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
pub fn collect_daemon_summaries(filter: &MonitorFilter, now: i64) -> Vec<DaemonSummary> {
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
pub fn collect_selected_events(
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
        eprintln!(
            "Warning: failed to load recent events for {}: {err}",
            md.run_id
        );
        Vec::new()
    })
}

/// Map a `MonitorState` to its stable lowercase token.
/// @plan:issue-51
pub fn monitor_state_token(state: &MonitorState) -> &'static str {
    match state {
        MonitorState::Starting => "starting",
        MonitorState::Running => "running",
        MonitorState::Degraded => "degraded",
        MonitorState::Stopping => "stopping",
        MonitorState::Stopped => "stopped",
        MonitorState::Error => "error",
    }
}
