use super::*;
use luther_workflow::daemon::discovery::{RoutedIssue, SkipReason};
use luther_workflow::daemon::DaemonStatus;
use luther_workflow::monitor::heartbeat::Heartbeat;

fn issue(
    number: u64,
    title: &str,
    labels: &[&str],
) -> luther_workflow::adapters::github_issues::GithubIssue {
    luther_workflow::adapters::github_issues::GithubIssue {
        number,
        title: title.to_string(),
        state: "open".to_string(),
        labels: labels.iter().map(|l| l.to_string()).collect(),
        assignee: None,
        milestone: None,
        body: None,
    }
}

fn routed(number: u64) -> RoutedIssue {
    RoutedIssue {
        issue: issue(number, "routed", &["ready"]),
        workflow_type_id: Some("wf-v1".to_string()),
        config_id: Some("cfg-a".to_string()),
    }
}

fn daemon_state(config_id: &str, status: DaemonStatus) -> DaemonState {
    let mut state = DaemonState::new(config_id).with_status(status);
    state.start_timestamp = chrono::Utc::now().timestamp() - 30;
    state
}

#[test]
fn print_discovery_json_and_text_do_not_panic() {
    let result = DiscoveryResult {
        eligible: vec![routed(1), routed(2)],
        skipped: vec![
            (issue(3, "skip", &[]), SkipReason::HasOpenPr),
            (
                issue(4, "skip2", &["blocked"]),
                SkipReason::MissingRequiredLabel("ready".to_string()),
            ),
        ],
    };
    print_discovery_json(&result);
    print_discovery_text(&result);
}

#[test]
fn print_discovery_text_handles_empty_result() {
    let result = DiscoveryResult {
        eligible: vec![],
        skipped: vec![],
    };
    print_discovery_text(&result);
    print_discovery_json(&result);
}

#[test]
fn format_wait_summary_reads_fields_with_defaults() {
    let wait = serde_json::json!({
        "wait_kind": "pr_checks",
        "next_poll_at": "2026-07-10T00:00:00Z",
        "poll_count": 4,
        "resume_step": "watch_pr_checks",
    });
    let summary = format_wait_summary(&wait);
    assert!(summary.contains("kind=pr_checks"));
    assert!(summary.contains("poll_count=4"));
    assert!(summary.contains("resume_step=watch_pr_checks"));

    // Missing fields fall back to placeholders.
    let empty = format_wait_summary(&serde_json::json!({}));
    assert!(empty.contains("kind=-"));
    assert!(empty.contains("poll_count=0"));
}

#[test]
fn report_stop_outcome_covers_all_variants() {
    report_stop_outcome("cfg", StopOutcome::Stopped);
    report_stop_outcome("cfg", StopOutcome::AlreadyStopped);
    report_stop_outcome("cfg", StopOutcome::NotFound);
}

#[test]
fn daemon_display_status_marks_running_dead_as_stale() {
    let running = daemon_state("cfg", DaemonStatus::Running);
    assert_eq!(daemon_display_status(&running, true), "running");
    assert_eq!(daemon_display_status(&running, false), "stale");

    let stopped = daemon_state("cfg", DaemonStatus::Stopped);
    // A non-running status is displayed verbatim regardless of liveness.
    assert_eq!(
        daemon_display_status(&stopped, false),
        DaemonStatus::Stopped.to_string()
    );
}

#[test]
fn daemon_state_json_includes_expected_fields() {
    let state = daemon_state("cfg-json", DaemonStatus::Running);
    let json = daemon_state_json(&state);
    assert_eq!(json.get("config_id").unwrap(), "cfg-json");
    assert_eq!(json.get("pid").unwrap(), state.pid);
    assert!(json.get("uptime_secs").unwrap().as_i64().unwrap() >= 0);
    assert!(json.get("alive").unwrap().is_boolean());
}

#[test]
fn daemon_status_single_and_all_render_from_store() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = DaemonStore::at(temp.path());
    // Missing config renders "not found" in both formats.
    daemon_status_single(&store, "absent", false);
    daemon_status_single(&store, "absent", true);
    // Empty aggregate view.
    daemon_status_all(&store, false);
    daemon_status_all(&store, true);

    // Persist a state and render populated views.
    let state = daemon_state("cfg-live", DaemonStatus::Running);
    store.write(&state).expect("write daemon state");
    daemon_status_single(&store, "cfg-live", false);
    daemon_status_single(&store, "cfg-live", true);
    daemon_status_all(&store, false);
    daemon_status_all(&store, true);
}

#[test]
fn stop_all_daemons_handles_empty_store() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = DaemonStore::at(temp.path());
    stop_all_daemons(&store);
}

#[test]
fn heartbeat_run_index_empty_without_store() {
    let heartbeats = std::collections::HashMap::new();
    let index = heartbeat_run_index(None, &heartbeats);
    assert!(index.is_empty());
}

#[test]
fn filter_status_by_config_scopes_runs_by_config_id() {
    let mut hb = Heartbeat::new("instance-1");
    hb.run_id = Some("run-1".to_string());
    let mut heartbeats = std::collections::HashMap::new();
    heartbeats.insert("instance-1".to_string(), hb);

    let mut keep = RunMetadata::new("run-1", "wf", "cfg-keep");
    keep.config_id = "cfg-keep".to_string();
    let mut drop = RunMetadata::new("run-2", "wf", "cfg-other");
    drop.config_id = "cfg-other".to_string();
    let runs_result = Ok(vec![keep, drop]);

    let (_hbs, filtered_runs) = filter_status_by_config(heartbeats, runs_result, "cfg-keep");
    let filtered = filtered_runs.expect("runs filtered");
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].config_id, "cfg-keep");
}

#[test]
fn collect_queue_leases_and_metadata_on_empty_db() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("checkpoints.db");
    luther_workflow::persistence::init_database(&db_path).expect("init db");
    let store = SqliteStore::open(&db_path).expect("open store");
    let conn = store.conn();
    let leases: Vec<IssueLease> = Vec::new();
    let metadata = queue_run_metadata(conn, &leases);
    assert!(metadata.is_empty());
    let waits = queue_wait_summaries(conn);
    assert!(waits.is_empty());
    // Rendering an empty queue.
    print_queue_text(conn, &leases);
    print_queue_json(conn, &leases);
}
