//! Pure, I/O-free snapshot model + filtering + rendering for `luther monitor`.
//!
//! Issue #52 introduces a continuous, plain-CLI (non-TUI) `monitor` command.
//! To keep the logic deterministically unit-testable, all data modeling,
//! filtering, iteration-count resolution and rendering live here as pure
//! functions over already-collected inputs. The thin collection + loop +
//! signal shell lives in `main.rs` and is covered by E2E binary tests.
//!
//! @plan:issue-52
use std::fmt::Write as _;

use crate::daemon::DaemonState;
use crate::persistence::{EventRecord, RunMetadata, RunStatus};

/// ANSI escape that clears the screen and homes the cursor.
/// Used by the loop shell when repainting an interactive terminal.
/// @plan:issue-52
pub const CLEAR_SCREEN: &str = "\x1b[2J\x1b[H";

/// One daemon's summarized health for the monitor header.
/// @plan:issue-52
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonSummary {
    pub config_id: String,
    pub status: String,
    pub pid: u32,
    pub uptime_secs: i64,
    pub alive: bool,
}

impl DaemonSummary {
    /// Build a summary from a persisted [`DaemonState`] and a liveness flag.
    /// @plan:issue-52
    pub fn from_state(state: &DaemonState, alive: bool, now: i64) -> Self {
        Self {
            config_id: state.config_id.clone(),
            status: state.status.to_string(),
            pid: state.pid,
            uptime_secs: state.uptime_secs(now),
            alive,
        }
    }
}

/// Aggregate run counts grouped by lifecycle phase.
/// @plan:issue-52
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunCounts {
    pub active: usize,
    pub queued: usize,
    pub completed: usize,
    pub failed: usize,
}

impl RunCounts {
    /// Derive counts from a slice of run metadata.
    ///
    /// - active: running/starting/remediating/waiting_for_checks
    /// - queued: queued/initialized
    /// - completed: completed/merged
    /// - failed: failed/abandoned/cancelled
    ///
    /// Blocked/Paused are intentionally not counted in any bucket as they are
    /// neither making progress nor terminal.
    /// @plan:issue-52
    pub fn from_runs(runs: &[RunMetadata]) -> Self {
        let mut counts = RunCounts::default();
        for md in runs {
            match md.status {
                RunStatus::Running
                | RunStatus::Starting
                | RunStatus::Remediating
                | RunStatus::WaitingForChecks => counts.active += 1,
                RunStatus::Queued | RunStatus::Initialized => counts.queued += 1,
                RunStatus::Completed | RunStatus::Merged => counts.completed += 1,
                RunStatus::Failed | RunStatus::Abandoned | RunStatus::Cancelled => {
                    counts.failed += 1
                }
                RunStatus::Blocked | RunStatus::Paused => {}
            }
        }
        counts
    }
}

/// A complete point-in-time snapshot of daemon + run state.
/// @plan:issue-52
#[derive(Debug, Clone)]
pub struct MonitorSnapshot {
    pub generated_at: String,
    pub daemons: Vec<DaemonSummary>,
    pub counts: RunCounts,
    pub runs: Vec<RunMetadata>,
    pub selected: Option<RunMetadata>,
    pub recent_events: Vec<EventRecord>,
}

/// Conjunctive (AND) filter for narrowing the monitored runs.
/// @plan:issue-52
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MonitorFilter {
    pub config: Option<String>,
    pub run: Option<String>,
    pub issue: Option<i64>,
}

/// Result of applying a [`MonitorFilter`] to a set of runs.
/// @plan:issue-52
#[derive(Debug, Clone)]
pub struct FilteredRuns {
    pub runs: Vec<RunMetadata>,
    pub selected: Option<RunMetadata>,
}

impl MonitorFilter {
    /// Whether this filter has no active constraints.
    /// @plan:issue-52
    pub fn is_empty(&self) -> bool {
        self.config.is_none() && self.run.is_none() && self.issue.is_none()
    }

    /// Apply the filter conjunctively to `runs`.
    ///
    /// `--config` matches `config_id`, `--issue` matches `issue_number`, and
    /// `--run` matches `run_id` (and also marks the selected run). When no
    /// `--run` is supplied the selected run defaults to the first match so the
    /// detail block always has a focus when runs exist.
    /// @plan:issue-52
    pub fn apply(&self, runs: &[RunMetadata]) -> FilteredRuns {
        let filtered: Vec<RunMetadata> =
            runs.iter().filter(|md| self.matches(md)).cloned().collect();
        let selected = self.select(&filtered);
        FilteredRuns {
            runs: filtered,
            selected,
        }
    }

    /// Whether a single run satisfies all active constraints.
    fn matches(&self, md: &RunMetadata) -> bool {
        if let Some(config) = &self.config {
            if &md.config_id != config {
                return false;
            }
        }
        if let Some(run) = &self.run {
            if &md.run_id != run {
                return false;
            }
        }
        if let Some(issue) = self.issue {
            if md.issue_number != Some(issue) {
                return false;
            }
        }
        true
    }

    /// Choose the focused run for the detail block.
    fn select(&self, filtered: &[RunMetadata]) -> Option<RunMetadata> {
        if let Some(run) = &self.run {
            return filtered.iter().find(|md| &md.run_id == run).cloned();
        }
        filtered.first().cloned()
    }
}

/// Resolve how many snapshots to render before exiting.
///
/// `--once` is sugar for `--times 1`. `None` means run continuously until
/// interrupted.
/// @plan:issue-52
pub fn resolve_snapshot_count(once: bool, times: Option<u32>) -> Option<u32> {
    if once {
        Some(1)
    } else {
        times
    }
}

/// Truncate a field for fixed-width table rendering (mirrors `runs list`).
/// @plan:issue-52
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

/// Build the per-snapshot separator line for append (`--no-clear`) mode.
/// @plan:issue-52
pub fn separator_line(generated_at: &str) -> String {
    format!("===== monitor snapshot @ {generated_at} =====")
}

/// Render a snapshot as plain text into `out`.
///
/// Sections, in order: (1) daemon summary; (2) counts line; (3) run table;
/// (4) selected/current run detail; (5) up to `tail` recent log lines.
/// @plan:issue-52
pub fn render_snapshot(
    snapshot: &MonitorSnapshot,
    tail: usize,
    out: &mut String,
) -> std::fmt::Result {
    render_daemons(&snapshot.daemons, out)?;
    render_counts(&snapshot.counts, out)?;
    render_run_table(&snapshot.runs, out)?;
    render_selected(snapshot.selected.as_ref(), out)?;
    render_events(&snapshot.recent_events, tail, out)?;
    Ok(())
}

/// Render the daemon summary section.
fn render_daemons(daemons: &[DaemonSummary], out: &mut String) -> std::fmt::Result {
    writeln!(out, "Daemons:")?;
    if daemons.is_empty() {
        writeln!(out, "  (no daemons running)")?;
        return Ok(());
    }
    for d in daemons {
        let liveness = if d.alive { "alive" } else { "stale" };
        writeln!(
            out,
            "  {} status={} pid={} uptime={}s ({})",
            d.config_id, d.status, d.pid, d.uptime_secs, liveness
        )?;
    }
    Ok(())
}

/// Render the aggregate counts line.
fn render_counts(counts: &RunCounts, out: &mut String) -> std::fmt::Result {
    writeln!(
        out,
        "Runs: active={} queued={} completed={} failed={}",
        counts.active, counts.queued, counts.completed, counts.failed
    )
}

/// Render the fixed-width run table (mirrors the `runs list` columns).
fn render_run_table(runs: &[RunMetadata], out: &mut String) -> std::fmt::Result {
    if runs.is_empty() {
        writeln!(out, "No runs found.")?;
        return Ok(());
    }
    writeln!(
        out,
        "{:<20} {:<28} {:<7} {:<7} {:<11} {:<16} {:<25}",
        "CONFIG", "RUN ID", "ISSUE", "PR", "STATE", "STEP", "UPDATED"
    )?;
    for md in runs {
        let updated = md.updated_at.unwrap_or(md.created_at).to_rfc3339();
        writeln!(
            out,
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
        )?;
    }
    Ok(())
}

/// Render the selected/current run detail block.
fn render_selected(selected: Option<&RunMetadata>, out: &mut String) -> std::fmt::Result {
    let Some(md) = selected else {
        return Ok(());
    };
    writeln!(out, "Selected run: {}", md.run_id)?;
    writeln!(out, "  Status: {}", md.status)?;
    writeln!(
        out,
        "  Current step: {}",
        md.current_step.as_deref().unwrap_or("(none)")
    )?;
    writeln!(
        out,
        "  Previous: {} -> {}",
        md.previous_step.as_deref().unwrap_or("(none)"),
        md.previous_outcome.as_deref().unwrap_or("(none)")
    )?;
    writeln!(out, "  Next: {}", next_step_label(md))?;
    writeln!(out, "  PID: {}", pid_liveness_label(md))?;
    Ok(())
}

/// Render up to `tail` recent log/event lines for the selected run.
fn render_events(events: &[EventRecord], tail: usize, out: &mut String) -> std::fmt::Result {
    if tail == 0 {
        return Ok(());
    }
    writeln!(out, "Recent events:")?;
    let start = events.len().saturating_sub(tail);
    let shown = &events[start..];
    if shown.is_empty() {
        writeln!(out, "  (no events)")?;
        return Ok(());
    }
    for e in shown {
        writeln!(
            out,
            "  [{}] {} -> {} ({})",
            e.timestamp.to_rfc3339(),
            e.step_id,
            e.outcome,
            e.event_type
        )?;
    }
    Ok(())
}

/// Describe next-step candidates (pure mirror of the `status` helper).
fn next_step_label(md: &RunMetadata) -> String {
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

/// Render a run's PID liveness (pure mirror of the `status` helper).
fn pid_liveness_label(md: &RunMetadata) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn run_with_status(run_id: &str, status: RunStatus) -> RunMetadata {
        let mut md = RunMetadata::new(run_id, "wf", "cfg");
        md.status = status;
        md
    }

    #[test]
    fn resolve_count_once_is_one() {
        assert_eq!(resolve_snapshot_count(true, None), Some(1));
        // once wins even if times provided.
        assert_eq!(resolve_snapshot_count(true, Some(5)), Some(1));
    }

    #[test]
    fn resolve_count_times_passthrough() {
        assert_eq!(resolve_snapshot_count(false, Some(5)), Some(5));
    }

    #[test]
    fn resolve_count_default_infinite() {
        assert_eq!(resolve_snapshot_count(false, None), None);
    }

    #[test]
    fn run_counts_classifies_states() {
        let runs = vec![
            run_with_status("a", RunStatus::Running),
            run_with_status("b", RunStatus::Starting),
            run_with_status("c", RunStatus::Queued),
            run_with_status("d", RunStatus::Initialized),
            run_with_status("e", RunStatus::Completed),
            run_with_status("f", RunStatus::Merged),
            run_with_status("g", RunStatus::Failed),
            run_with_status("h", RunStatus::Abandoned),
            run_with_status("i", RunStatus::Cancelled),
            run_with_status("j", RunStatus::Blocked),
        ];
        let counts = RunCounts::from_runs(&runs);
        assert_eq!(counts.active, 2);
        assert_eq!(counts.queued, 2);
        assert_eq!(counts.completed, 2);
        assert_eq!(counts.failed, 3);
    }

    #[test]
    fn filter_by_config_is_conjunctive() {
        let mut a = run_with_status("run-a", RunStatus::Running);
        a.config_id = "alpha".to_string();
        a.issue_number = Some(10);
        let mut b = run_with_status("run-b", RunStatus::Running);
        b.config_id = "beta".to_string();
        b.issue_number = Some(20);
        let runs = vec![a, b];

        let filter = MonitorFilter {
            config: Some("alpha".to_string()),
            ..Default::default()
        };
        let out = filter.apply(&runs);
        assert_eq!(out.runs.len(), 1);
        assert_eq!(out.runs[0].run_id, "run-a");
        assert_eq!(out.selected.as_ref().unwrap().run_id, "run-a");
    }

    #[test]
    fn filter_by_issue_uses_issue_number() {
        let mut a = run_with_status("run-a", RunStatus::Running);
        a.issue_number = Some(1801);
        let mut b = run_with_status("run-b", RunStatus::Running);
        b.issue_number = Some(99);
        let runs = vec![a, b];

        let filter = MonitorFilter {
            issue: Some(1801),
            ..Default::default()
        };
        let out = filter.apply(&runs);
        assert_eq!(out.runs.len(), 1);
        assert_eq!(out.runs[0].run_id, "run-a");
    }

    #[test]
    fn filter_by_run_marks_selected() {
        let runs = vec![
            run_with_status("run-a", RunStatus::Running),
            run_with_status("run-b", RunStatus::Running),
        ];
        let filter = MonitorFilter {
            run: Some("run-b".to_string()),
            ..Default::default()
        };
        let out = filter.apply(&runs);
        assert_eq!(out.runs.len(), 1);
        assert_eq!(out.selected.as_ref().unwrap().run_id, "run-b");
    }

    #[test]
    fn filter_combines_constraints() {
        let mut a = run_with_status("run-a", RunStatus::Running);
        a.config_id = "alpha".to_string();
        a.issue_number = Some(7);
        let mut b = run_with_status("run-b", RunStatus::Running);
        b.config_id = "alpha".to_string();
        b.issue_number = Some(8);
        let runs = vec![a, b];

        let filter = MonitorFilter {
            config: Some("alpha".to_string()),
            issue: Some(8),
            ..Default::default()
        };
        let out = filter.apply(&runs);
        assert_eq!(out.runs.len(), 1);
        assert_eq!(out.runs[0].run_id, "run-b");
    }

    fn sample_event(step: &str) -> EventRecord {
        EventRecord {
            run_id: "run-a".to_string(),
            step_id: step.to_string(),
            outcome: "success".to_string(),
            event_type: "step_completed".to_string(),
            details: None,
            timestamp: Utc::now(),
        }
    }

    fn sample_snapshot() -> MonitorSnapshot {
        let mut selected = run_with_status("run-a", RunStatus::Running);
        selected.current_step = Some("implement".to_string());
        let daemon = DaemonSummary {
            config_id: "llxprt-code".to_string(),
            status: "running".to_string(),
            pid: 1234,
            uptime_secs: 42,
            alive: true,
        };
        MonitorSnapshot {
            generated_at: "2026-06-15T00:00:00Z".to_string(),
            daemons: vec![daemon],
            counts: RunCounts {
                active: 1,
                ..Default::default()
            },
            runs: vec![selected.clone()],
            selected: Some(selected),
            recent_events: vec![sample_event("s1"), sample_event("s2"), sample_event("s3")],
        }
    }

    #[test]
    fn render_includes_all_sections() {
        let snapshot = sample_snapshot();
        let mut out = String::new();
        render_snapshot(&snapshot, 10, &mut out).unwrap();
        assert!(out.contains("Daemons:"));
        assert!(out.contains("llxprt-code"));
        assert!(out.contains("active=1"));
        assert!(out.contains("run-a"));
        assert!(out.contains("Selected run: run-a"));
        assert!(out.contains("implement"));
        assert!(out.contains("Recent events:"));
        assert!(out.contains("s1"));
    }

    #[test]
    fn render_tail_limits_event_lines() {
        let snapshot = sample_snapshot();
        let mut out = String::new();
        render_snapshot(&snapshot, 2, &mut out).unwrap();
        let event_lines = out.lines().filter(|l| l.contains(" -> success ")).count();
        assert_eq!(event_lines, 2);
    }

    #[test]
    fn render_tail_zero_hides_events() {
        let snapshot = sample_snapshot();
        let mut out = String::new();
        render_snapshot(&snapshot, 0, &mut out).unwrap();
        assert!(!out.contains("Recent events:"));
    }

    #[test]
    fn render_no_runs_is_graceful() {
        let snapshot = MonitorSnapshot {
            generated_at: "2026-06-15T00:00:00Z".to_string(),
            daemons: Vec::new(),
            counts: RunCounts::default(),
            runs: Vec::new(),
            selected: None,
            recent_events: Vec::new(),
        };
        let mut out = String::new();
        render_snapshot(&snapshot, 10, &mut out).unwrap();
        assert!(out.contains("(no daemons running)"));
        assert!(out.contains("No runs found."));
    }

    #[test]
    fn separator_line_contains_timestamp() {
        let line = separator_line("2026-06-15T00:00:00Z");
        assert!(line.contains("2026-06-15T00:00:00Z"));
        assert!(line.contains("monitor snapshot"));
    }

    #[test]
    fn truncate_field_adds_ellipsis() {
        assert_eq!(truncate_field("short", 10), "short");
        assert_eq!(truncate_field("abcdefghij", 5), "abcd…");
    }
}
