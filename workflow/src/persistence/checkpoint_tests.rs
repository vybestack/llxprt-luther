//! Checkpoint persistence tests.
//!
//! Split from `checkpoint.rs`, which was over the 1000-line hard limit.
//! Behaviour is unchanged; this is a move.

use super::*;

#[test]
fn checkpoint_can_be_created() {
    // @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    let cp = Checkpoint::new("run-123", "step-1");
    assert_eq!(cp.run_id, "run-123");
    assert_eq!(cp.step_id, "step-1");
    assert_eq!(cp.state_snapshot.status, "running");
}

#[test]
fn checkpoint_with_snapshot() {
    // @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    let snapshot = StateSnapshot {
        retry_count: 2,
        loop_count: 1,
        edge_loop_counts: HashMap::new(),
        context: HashMap::new(),
        status: "running".to_string(),
    };
    let cp = Checkpoint::with_snapshot("run-456", "step-2", snapshot);
    assert_eq!(cp.run_id, "run-456");
    assert_eq!(cp.step_id, "step-2");
    assert_eq!(cp.state_snapshot.retry_count, 2);
    assert_eq!(cp.state_snapshot.loop_count, 1);
}

#[test]
fn checkpoint_mark_interrupted() {
    // @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    let mut cp = Checkpoint::new("run-789", "step-3");
    cp.mark_interrupted();
    assert_eq!(cp.state_snapshot.status, "interrupted");
}

#[test]
fn persistence_error_variants_exist() {
    // @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    let _db = PersistenceError::Database("test".to_string());
    let _ser = PersistenceError::Serialization("test".to_string());
    let _nf = PersistenceError::NotFound("test".to_string());
}

#[test]
fn save_and_load_checkpoint() {
    // @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    let conn = Connection::open_in_memory().expect("Failed to open in-memory database");

    let checkpoint = Checkpoint::new("run-123", "step-a");
    save_checkpoint_with_conn(&conn, &checkpoint).expect("Failed to save checkpoint");

    let loaded = load_checkpoint_with_conn(&conn, "run-123").expect("Failed to load checkpoint");
    assert!(loaded.is_some(), "Checkpoint should be found");
    let loaded_cp = loaded.unwrap();
    assert_eq!(loaded_cp.run_id, "run-123");
    assert_eq!(loaded_cp.step_id, "step-a");
}

#[test]
fn checkpoint_preserves_counters() {
    // @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    let conn = Connection::open_in_memory().expect("Failed to open in-memory database");

    let snapshot = StateSnapshot {
        retry_count: 3,
        loop_count: 2,
        edge_loop_counts: HashMap::new(),
        context: HashMap::new(),
        status: "interrupted".to_string(),
    };
    let checkpoint = Checkpoint::with_snapshot("run-456", "step-b", snapshot);
    save_checkpoint_with_conn(&conn, &checkpoint).expect("Failed to save checkpoint");

    let loaded = load_checkpoint_with_conn(&conn, "run-456").expect("Failed to load checkpoint");
    assert!(loaded.is_some());
    let loaded_cp = loaded.unwrap();
    assert_eq!(loaded_cp.state_snapshot.retry_count, 3);
    assert_eq!(loaded_cp.state_snapshot.loop_count, 2);
    assert_eq!(loaded_cp.state_snapshot.status, "interrupted");
}

#[test]
fn save_and_load_events() {
    // @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    let conn = Connection::open_in_memory().expect("Failed to open in-memory database");

    let timestamp = Utc::now();
    append_event_with_conn(&conn, "run-123", "step-a", "success", timestamp)
        .expect("Failed to append event");
    append_event_with_conn(&conn, "run-123", "step-b", "success", timestamp)
        .expect("Failed to append event");

    let events = load_events(&conn, "run-123").expect("Failed to load events");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].step_id, "step-a");
    assert_eq!(events[0].outcome, "success");
    assert_eq!(events[1].step_id, "step-b");
}

#[test]
fn load_recent_events_bounds_and_orders_chronologically() {
    // @plan:issue-52
    let conn = Connection::open_in_memory().expect("Failed to open in-memory database");

    let timestamp = Utc::now();
    for step in ["step-a", "step-b", "step-c", "step-d"] {
        append_event_with_conn(&conn, "run-123", step, "success", timestamp)
            .expect("Failed to append event");
    }

    // Tail of 2 returns the two most recent events in chronological order.
    let recent = load_recent_events(&conn, "run-123", 2).expect("Failed to load recent events");
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].step_id, "step-c");
    assert_eq!(recent[1].step_id, "step-d");

    // A limit larger than the number of stored events returns all of them.
    let all = load_recent_events(&conn, "run-123", 10).expect("Failed to load recent events");
    assert_eq!(all.len(), 4);
    assert_eq!(all[0].step_id, "step-a");
    assert_eq!(all[3].step_id, "step-d");

    // A zero limit yields an empty result without touching the database.
    let none = load_recent_events(&conn, "run-123", 0).expect("Failed to load recent events");
    assert!(none.is_empty());
}

/// Persist a checkpoint with an explicit step and status for resume tests.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
fn seed_checkpoint(conn: &Connection, run_id: &str, step_id: &str, status: &str) {
    let snapshot = StateSnapshot {
        status: status.to_string(),
        ..Default::default()
    };
    let checkpoint = Checkpoint::with_snapshot(run_id, step_id, snapshot);
    save_checkpoint_with_conn(conn, &checkpoint).expect("seed checkpoint");
    // Ensure later checkpoints sort after earlier ones despite fast clocks.
    std::thread::sleep(std::time::Duration::from_millis(2));
}

#[test]
fn resumable_status_classification() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    assert!(is_resumable_checkpoint_status(CHECKPOINT_STATUS_WAITING));
    assert!(is_resumable_checkpoint_status(
        CHECKPOINT_STATUS_INTERRUPTED
    ));
    assert!(is_resumable_checkpoint_status(
        CHECKPOINT_STATUS_READY_TO_RESUME
    ));
    assert!(!is_resumable_checkpoint_status("completed"));
}

#[test]
fn get_checkpoint_for_step_finds_specific_step() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    let conn = Connection::open_in_memory().expect("open db");
    seed_checkpoint(&conn, "run-x", "step-a", "completed");
    seed_checkpoint(&conn, "run-x", "step-b", "waiting");

    let found = get_checkpoint_for_step(&conn, "run-x", "step-b")
        .expect("query")
        .expect("checkpoint present");
    assert_eq!(found.step_id, "step-b");
    assert_eq!(found.state_snapshot.status, "waiting");

    let missing = get_checkpoint_for_step(&conn, "run-x", "nope").expect("query");
    assert!(missing.is_none());
}

#[test]
fn load_checkpoint_before_step_returns_prior() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    let conn = Connection::open_in_memory().expect("open db");
    seed_checkpoint(&conn, "run-y", "good_pre_watch", "completed");
    seed_checkpoint(&conn, "run-y", "watch_pr_checks", "completed");
    seed_checkpoint(&conn, "run-y", "post_pr_failure_terminal", "completed");

    let before = load_checkpoint_before_step(&conn, "run-y", "post_pr_failure_terminal")
        .expect("query")
        .expect("prior checkpoint");
    assert_eq!(before.step_id, "watch_pr_checks");

    // No checkpoint precedes the first step.
    let none = load_checkpoint_before_step(&conn, "run-y", "good_pre_watch").expect("query");
    assert!(none.is_none());
}

#[test]
fn set_resume_point_rearms_selected_checkpoint() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    let conn = Connection::open_in_memory().expect("open db");
    seed_checkpoint(&conn, "run-z", "watch_pr_checks", "completed");
    seed_checkpoint(&conn, "run-z", "post_pr_failure_terminal", "completed");

    // Before re-stamping, the newest checkpoint is the terminal step.
    let newest = load_checkpoint_with_conn(&conn, "run-z")
        .expect("load")
        .expect("checkpoint");
    assert_eq!(newest.step_id, "post_pr_failure_terminal");

    set_resume_point(&conn, "run-z", "watch_pr_checks").expect("set resume point");

    // After re-stamping, the resume loader selects the re-armed checkpoint.
    let resumed = load_checkpoint_with_conn(&conn, "run-z")
        .expect("load")
        .expect("checkpoint");
    assert_eq!(resumed.step_id, "watch_pr_checks");
    assert_eq!(
        resumed.state_snapshot.status,
        CHECKPOINT_STATUS_READY_TO_RESUME
    );

    // The terminal checkpoint row is preserved (history not erased).
    let all = list_checkpoints(&conn, "run-z").expect("list");
    assert_eq!(all.len(), 2);
    assert!(all.iter().any(|c| c.step_id == "post_pr_failure_terminal"));
}

#[test]
fn set_resume_point_missing_checkpoint_errors() {
    // @plan:PLAN-20260623-LUTHER-CONTINUATION
    let conn = Connection::open_in_memory().expect("open db");
    init_checkpoint_table(&conn).expect("init checkpoint table");
    let err = set_resume_point(&conn, "run-missing", "nope").unwrap_err();
    assert!(matches!(err, PersistenceError::NotFound(_)));
}
