/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
/// Integration tests for the persistent run registry & event model (issue #50).
///
/// These tests verify run-record creation at start, step transitions, terminal
/// states, event ordering/typing, restart survival, PID staleness handling, and
/// metadata persistence.
use std::collections::HashMap;

use chrono::Utc;
use rusqlite::Connection;

use luther_workflow::engine::executor::{ExecutorRegistry, NoOpExecutor};
use luther_workflow::engine::instance::WorkflowInstance;
use luther_workflow::engine::runner::{EngineRunner, RunOutcome};
use luther_workflow::persistence::{
    append_typed_event_with_conn, count_events_by_type, init_database, is_pid_stale, load_events,
    load_events_by_type, load_latest_event, EventType, RunMetadata, RunStatus, SqliteStore,
};
use luther_workflow::workflow::schema::{
    GuardLimits, RepoConfig, RuntimeConfig, StepDef, TransitionDef, WorkflowConfig, WorkflowType,
};

fn step(id: &str) -> StepDef {
    StepDef {
        step_id: id.to_string(),
        step_type: "test".to_string(),
        description: None,
        produces: None,
        consumes: None,
        terminal: None,
        parameters: None,
    }
}

fn two_step_workflow() -> WorkflowType {
    WorkflowType {
        workflow_type_id: "registry-test-v1".to_string(),
        steps: vec![step("step_a"), step("step_b")],
        transitions: vec![TransitionDef {
            from: "step_a".to_string(),
            to: "step_b".to_string(),
            condition: None,
            max_iterations: None,
        }],
        guards: Default::default(),
    }
}

fn test_config() -> WorkflowConfig {
    WorkflowConfig {
        config_id: "registry-config".to_string(),
        workflow_type_id: "registry-test-v1".to_string(),
        runtime: RuntimeConfig {
            timeout_seconds: 3600,
            max_retries: 3,
            parallel_steps: None,
            log_level: None,
        },
        repo: RepoConfig {
            workspace_strategy: "temp".to_string(),
            branch_template: "test-{run_id}".to_string(),
            base_branch: Some("main".to_string()),
            workspace_root: None,
            project_subdir: None,
            artifact_path_base: None,
            diff_path_base: None,
            diff_path_normalization:
                luther_workflow::workflow::schema::DiffPathNormalization::RepoRelative,
        },
        guard_limits: GuardLimits {
            max_iterations: Some(3),
            max_file_changes: Some(50),
            max_tokens: Some(10000),
            max_cost: Some(10.0),
        },
        variables: HashMap::new(),
        discovery: None,
        parent_orchestration: Default::default(),
        command_manifest: None,
        target_profile: None,
    }
}

fn test_registry() -> ExecutorRegistry {
    let mut registry = ExecutorRegistry::new();
    registry.register("test", Box::new(NoOpExecutor));
    registry
}

/// Acceptance: each daemon-launched run has a persistent record created at
/// start, before completion (in-flight visibility).
#[test]
fn run_record_created_at_init() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("checkpoints.db");
    let instance = WorkflowInstance::create(two_step_workflow(), test_config());
    let run_id = instance.run_id.clone();
    let registry = test_registry();

    let _runner = EngineRunner::with_db_path(instance, registry, &db_path).expect("create runner");

    let store = SqliteStore::open(&db_path).expect("open store");
    let md = store.get_run(&run_id).expect("query").expect("run exists");
    assert!(
        matches!(md.status, RunStatus::Starting | RunStatus::Running),
        "in-flight run should be Starting/Running, got {}",
        md.status
    );
    assert_eq!(md.process_pid, Some(std::process::id()));
}

/// Acceptance: step transitions update current/previous step and next-step
/// candidates; terminal state writes Completed + TerminalState event.
#[test]
fn step_transitions_and_terminal_state() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("checkpoints.db");
    let instance = WorkflowInstance::create(two_step_workflow(), test_config());
    let run_id = instance.run_id.clone();
    let registry = test_registry();

    let mut runner =
        EngineRunner::with_db_path(instance, registry, &db_path).expect("create runner");
    let outcome = runner.run().expect("run");
    assert!(matches!(outcome, RunOutcome::Success));

    let store = SqliteStore::open(&db_path).expect("open");
    let md = store.get_run(&run_id).expect("query").expect("exists");
    assert_eq!(md.status, RunStatus::Completed);
    assert_eq!(md.previous_step.as_deref(), Some("step_b"));
    assert_eq!(md.previous_outcome.as_deref(), Some("success"));

    let conn = store.conn();
    let terminal = load_events_by_type(conn, &run_id, EventType::TerminalState).expect("events");
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0].outcome, "completed");
}

/// Acceptance: event history is queryable in order; StepStart precedes
/// StepOutcome per step.
#[test]
fn event_ordering_and_typing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("checkpoints.db");
    let instance = WorkflowInstance::create(two_step_workflow(), test_config());
    let run_id = instance.run_id.clone();
    let registry = test_registry();

    let mut runner =
        EngineRunner::with_db_path(instance, registry, &db_path).expect("create runner");
    runner.run().expect("run");

    let store = SqliteStore::open(&db_path).expect("open");
    let conn = store.conn();
    let events = load_events(conn, &run_id).expect("events");
    assert!(!events.is_empty());

    // First event for the run should be a StepStart.
    assert_eq!(events[0].event_type, EventType::StepStart.to_string());

    let starts = count_events_by_type(conn, &run_id, EventType::StepStart).expect("count");
    let outcomes = count_events_by_type(conn, &run_id, EventType::StepOutcome).expect("count");
    assert_eq!(starts, 2);
    assert_eq!(outcomes, 2);

    let latest = load_latest_event(conn, &run_id)
        .expect("latest")
        .expect("some");
    assert_eq!(latest.event_type, EventType::TerminalState.to_string());
}

/// Acceptance: run state survives CLI process restart (reopen DB file).
#[test]
fn run_survives_restart() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("checkpoints.db");
    init_database(&db_path).expect("init db");

    let mut md = RunMetadata::new("restart-run", "wf", "cfg");
    md.status = RunStatus::Running;
    md.current_step = Some("step_a".to_string());
    md.repository = Some("octo/repo".to_string());
    md.issue_number = Some(42);
    md.pr_number = Some(7);
    md.head_sha = Some("deadbeef".to_string());
    md.log_path = Some("/var/log/run.log".to_string());
    md.set_next_step_candidates(vec!["step_b".to_string()]);

    {
        let store = SqliteStore::open(&db_path).expect("open");
        store.persist_run(&md).expect("persist");
        let conn = store.conn();
        append_typed_event_with_conn(
            conn,
            "restart-run",
            "step_a",
            "started",
            EventType::StepStart,
            None,
            Utc::now(),
        )
        .expect("event");
    }

    // Reopen the DB file as a fresh connection (simulates CLI restart).
    let store = SqliteStore::open(&db_path).expect("reopen");
    let loaded = store
        .get_run("restart-run")
        .expect("query")
        .expect("exists");
    assert_eq!(loaded.status, RunStatus::Running);
    assert_eq!(loaded.issue_number, Some(42));
    assert_eq!(loaded.pr_number, Some(7));
    assert_eq!(loaded.head_sha.as_deref(), Some("deadbeef"));
    assert_eq!(loaded.next_step_candidates, vec!["step_b".to_string()]);

    let events = load_events(store.conn(), "restart-run").expect("events");
    assert_eq!(events.len(), 1);
}

/// Acceptance: full metadata round-trips through persist_run -> get_run.
#[test]
fn metadata_round_trip_all_fields() {
    let store = SqliteStore::open_in_memory().expect("store");
    let mut md = RunMetadata::new("full-run", "wf", "cfg");
    md.status = RunStatus::WaitingForChecks;
    md.current_step = Some("checks".to_string());
    md.set_previous_step_and_outcome("build", "success");
    md.set_next_step_candidates(vec!["merge".to_string(), "remediate".to_string()]);
    md.log_path = Some("/log".to_string());
    md.artifact_root = Some("/artifacts".to_string());
    md.workspace_path = Some("/ws".to_string());
    md.repository = Some("octo/repo".to_string());
    md.issue_number = Some(1234);
    md.pr_number = Some(5678);
    md.head_sha = Some("abc123".to_string());
    md.process_pid = Some(std::process::id());
    md.add_child_pid(111);
    md.add_child_pid(222);

    store.persist_run(&md).expect("persist");
    let loaded = store.get_run("full-run").expect("query").expect("exists");

    assert_eq!(loaded.status, RunStatus::WaitingForChecks);
    assert_eq!(loaded.previous_step.as_deref(), Some("build"));
    assert_eq!(loaded.previous_outcome.as_deref(), Some("success"));
    assert_eq!(
        loaded.next_step_candidates,
        vec!["merge".to_string(), "remediate".to_string()]
    );
    assert_eq!(loaded.artifact_root.as_deref(), Some("/artifacts"));
    assert_eq!(loaded.workspace_path.as_deref(), Some("/ws"));
    assert_eq!(loaded.issue_number, Some(1234));
    assert_eq!(loaded.pr_number, Some(5678));
    assert_eq!(loaded.process_pid, Some(std::process::id()));
    assert_eq!(loaded.child_pids, vec![111u32, 222u32]);
}

/// Acceptance: PID staleness is handled portably (works on macOS + Linux).
#[test]
fn pid_staleness_handling() {
    assert!(!is_pid_stale(std::process::id()));
    assert!(is_pid_stale(4_000_000_000));

    let mut md = RunMetadata::new("pid-run", "wf", "cfg");
    md.process_pid = Some(std::process::id());
    assert!(!md.is_process_stale());
    md.add_child_pid(std::process::id());
    md.add_child_pid(4_000_000_000);
    assert_eq!(md.are_child_pids_stale(), vec![4_000_000_000u32]);
}

/// Acceptance: query helpers filter by status/activity/repository.
#[test]
fn registry_query_helpers() {
    let store = SqliteStore::open_in_memory().expect("store");

    let mut active = RunMetadata::new("active", "wf", "cfg");
    active.status = RunStatus::Running;
    active.repository = Some("octo/repo".to_string());
    store.persist_run(&active).expect("persist active");

    let mut done = RunMetadata::new("done", "wf", "cfg");
    done.status = RunStatus::Completed;
    done.repository = Some("octo/other".to_string());
    store.persist_run(&done).expect("persist done");

    let actives = store.get_active_runs().expect("active");
    assert_eq!(actives.len(), 1);
    assert_eq!(actives[0].run_id, "active");

    let running = store
        .list_runs_by_status(&RunStatus::Running)
        .expect("by status");
    assert_eq!(running.len(), 1);

    let for_repo = store.get_runs_for_repository("octo/repo").expect("by repo");
    assert_eq!(for_repo.len(), 1);
    assert_eq!(for_repo[0].run_id, "active");
}

/// Acceptance: schema migration adds new columns to an old-style DB.
#[test]
fn schema_migration_adds_columns() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("old.db");

    // Create an old-style runs/events schema lacking the new columns.
    {
        let conn = Connection::open(&db_path).expect("open");
        conn.execute(
            "CREATE TABLE runs (
                run_id TEXT PRIMARY KEY,
                workflow_type_id TEXT NOT NULL,
                config_id TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT,
                current_step TEXT
            )",
            [],
        )
        .expect("create old runs");
        conn.execute(
            "INSERT INTO runs (run_id, workflow_type_id, config_id, status, created_at)
             VALUES ('old-run', 'wf', 'cfg', 'running', '2026-01-01T00:00:00+00:00')",
            [],
        )
        .expect("insert old run");
        conn.execute(
            "CREATE TABLE events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL,
                step_id TEXT NOT NULL,
                outcome TEXT NOT NULL,
                timestamp TEXT NOT NULL
            )",
            [],
        )
        .expect("create old events");
    }

    // Daemon initialization must migrate the run schema before any store or
    // engine runner opens the database.
    init_database(&db_path).expect("init/migrate");
    let daemon_conn = Connection::open(&db_path).expect("open daemon connection");
    let daemon_md = luther_workflow::persistence::get_run_with_conn(&daemon_conn, "old-run")
        .expect("daemon query")
        .expect("daemon run exists");
    assert_eq!(daemon_md.status, RunStatus::Running);
    assert!(daemon_md.continuation_rearm_checkpoint_id.is_none());
    drop(daemon_conn);

    let store = SqliteStore::open(&db_path).expect("open store");
    let md = store.get_run("old-run").expect("query").expect("exists");
    assert_eq!(md.status, RunStatus::Running);
    assert!(md.next_step_candidates.is_empty());
    assert!(md.child_pids.is_empty());

    // New typed event column is usable after migration.
    let conn = store.conn();
    append_typed_event_with_conn(
        conn,
        "old-run",
        "step_a",
        "started",
        EventType::StepStart,
        Some("detail"),
        Utc::now(),
    )
    .expect("typed event after migration");
    let events = load_events(conn, "old-run").expect("events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, EventType::StepStart.to_string());
    assert_eq!(events[0].details.as_deref(), Some("detail"));
}
