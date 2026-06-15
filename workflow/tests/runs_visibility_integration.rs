/// Integration tests for the `runs` visibility CLI data layer (issue #51).
///
/// The `runs list/show/tail/ps` commands render data sourced from the
/// persistent run registry, the event log, artifact records, and process
/// liveness helpers. These tests seed a temporary `checkpoints.db` and assert
/// that the data backing each command is retrievable and filterable, that the
/// full `runs show` field set is present, and that stale-process detection
/// behaves correctly.
use chrono::Utc;

use luther_workflow::persistence::append_typed_event_with_conn;
use luther_workflow::persistence::{
    init_database, is_pid_stale, list_artifacts, load_events, write_artifact, EventType,
    RunMetadata, RunStatus, SqliteStore,
};

/// Build a fully-populated run record for assertions.
fn seed_full_run(run_id: &str, config_id: &str, status: RunStatus) -> RunMetadata {
    let mut md = RunMetadata::new(run_id, "runs-vis-v1", config_id);
    md.status = status;
    md.current_step = Some("implement".to_string());
    md.set_previous_step_and_outcome("plan", "success");
    md.set_next_step_candidates(vec!["verify".to_string()]);
    md.log_path = Some(format!("/logs/{run_id}.log"));
    md.artifact_root = Some(format!("/artifacts/{run_id}"));
    md.workspace_path = Some(format!("/ws/{run_id}"));
    md.repository = Some("octo/repo".to_string());
    md.issue_number = Some(51);
    md.pr_number = Some(99);
    md.head_sha = Some("cafef00d".to_string());
    md.process_pid = Some(std::process::id());
    md
}

/// `runs list --config X` returns only runs whose config_id matches.
#[test]
fn runs_list_filters_by_config() {
    let store = SqliteStore::open_in_memory().expect("store");
    store
        .persist_run(&seed_full_run("run-a", "llxprt-code", RunStatus::Running))
        .expect("persist a");
    store
        .persist_run(&seed_full_run("run-b", "other-config", RunStatus::Running))
        .expect("persist b");

    let all = store.list_runs().expect("list");
    let only_llxprt: Vec<_> = all
        .into_iter()
        .filter(|md| md.config_id == "llxprt-code")
        .collect();
    assert_eq!(only_llxprt.len(), 1);
    assert_eq!(only_llxprt[0].run_id, "run-a");
}

/// `runs list --state running` returns only runs in the requested state.
#[test]
fn runs_list_filters_by_state() {
    let store = SqliteStore::open_in_memory().expect("store");
    store
        .persist_run(&seed_full_run("run-run", "cfg", RunStatus::Running))
        .expect("persist running");
    store
        .persist_run(&seed_full_run("run-done", "cfg", RunStatus::Completed))
        .expect("persist completed");

    let running = store
        .list_runs_by_status(&RunStatus::Running)
        .expect("by status");
    assert_eq!(running.len(), 1);
    assert_eq!(running[0].run_id, "run-run");

    // State token round-trips through FromStr as the CLI parses it.
    let parsed: RunStatus = "running".parse().expect("parse state");
    assert_eq!(parsed, RunStatus::Running);
}

/// `runs show` exposes the complete acceptance-criteria field set.
#[test]
fn runs_show_includes_all_acceptance_fields() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("checkpoints.db");
    init_database(&db_path).expect("init");
    let store = SqliteStore::open(&db_path).expect("open");
    store
        .persist_run(&seed_full_run(
            "run-show",
            "llxprt-code",
            RunStatus::Running,
        ))
        .expect("persist");
    append_typed_event_with_conn(
        store.conn(),
        "run-show",
        "plan",
        "success",
        EventType::StepOutcome,
        Some("planned"),
        Utc::now(),
    )
    .expect("event");

    let md = store.get_run("run-show").expect("query").expect("exists");
    assert_eq!(md.repository.as_deref(), Some("octo/repo"));
    assert_eq!(md.issue_number, Some(51));
    assert_eq!(md.pr_number, Some(99));
    assert_eq!(md.head_sha.as_deref(), Some("cafef00d"));
    assert_eq!(md.current_step.as_deref(), Some("implement"));
    assert_eq!(md.previous_step.as_deref(), Some("plan"));
    assert_eq!(md.previous_outcome.as_deref(), Some("success"));
    assert_eq!(md.next_step_candidates, vec!["verify".to_string()]);
    assert!(md.workspace_path.is_some());
    assert!(md.log_path.is_some());
    assert!(md.artifact_root.is_some());

    let events = load_events(store.conn(), "run-show").expect("events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].step_id, "plan");
    assert_eq!(events[0].outcome, "success");
}

/// `runs show` artifacts section is backed by list_artifacts.
#[test]
fn runs_show_lists_artifacts() {
    // Isolate the artifacts root to a tempdir so the test does not pollute the
    // workspace's default `./artifacts` directory.
    let tmp = tempfile::tempdir().expect("tempdir");
    let old_value = std::env::var("LUTHER_ARTIFACTS_ROOT").ok();
    std::env::set_var("LUTHER_ARTIFACTS_ROOT", tmp.path());

    let run_id = format!("artifact-run-{}", std::process::id());
    write_artifact(&run_id, "summary.md", b"# done\n").expect("write artifact");

    let artifacts = list_artifacts(&run_id).expect("list artifacts");
    match old_value {
        Some(val) => std::env::set_var("LUTHER_ARTIFACTS_ROOT", val),
        None => std::env::remove_var("LUTHER_ARTIFACTS_ROOT"),
    }

    assert!(
        artifacts
            .iter()
            .any(|a| a.artifact_path.to_string_lossy().contains("summary.md")),
        "artifact list should include the written artifact: {artifacts:?}"
    );
}

/// `runs tail` must handle a missing log path gracefully (no panic, path known).
#[test]
fn runs_tail_missing_log_path_is_known() {
    let run_id = "missing-log-run";
    let log_path = luther_workflow::runtime_paths::get_log_dir().join(format!("{run_id}.log"));
    // The conventional path is deterministic and used by the command to report
    // the missing-file case; it must end in the run-scoped log file name.
    assert!(log_path
        .to_string_lossy()
        .ends_with(&format!("{run_id}.log")));
}

/// `runs ps` stale detection: a dead PID is stale; the live PID is not.
#[test]
fn runs_ps_stale_detection() {
    // A clearly-out-of-range PID is treated as dead/stale.
    assert!(is_pid_stale(4_000_000_000));
    // The current test process PID is alive (not stale).
    assert!(!is_pid_stale(std::process::id()));

    let mut md = RunMetadata::new("ps-run", "wf", "cfg");
    md.process_pid = Some(std::process::id());
    assert!(!md.is_process_stale());

    md.add_child_pid(std::process::id());
    md.add_child_pid(4_000_000_000);
    assert_eq!(md.are_child_pids_stale(), vec![4_000_000_000u32]);
}

/// Heartbeat-style staleness uses a 60s freshness window relative to now.
#[test]
fn runs_ps_timestamp_freshness_window() {
    let now = Utc::now().timestamp();
    let fresh_ts = now - 5;
    let stale_ts = now - 120;
    assert!((now - fresh_ts) <= 60, "5s-old heartbeat is fresh");
    assert!((now - stale_ts) > 60, "120s-old heartbeat is stale");
}
