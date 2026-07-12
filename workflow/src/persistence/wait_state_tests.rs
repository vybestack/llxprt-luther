use super::*;
use chrono::Duration;
use serde_json::json;

fn conn() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    crate::persistence::leases::init_leases_table(&c).unwrap();
    crate::persistence::sqlite::init_runs_schema(&c).unwrap();
    init_wait_states_table(&c).unwrap();
    c
}

fn record(run_id: &str, next_poll_at: DateTime<Utc>) -> WaitStateRecord {
    let mut wait_record = WaitStateRecord::new(run_id, "cfg");
    wait_record.lease_id = Some(run_id.to_string());
    wait_record.workflow_type = "issue-fix".to_string();
    wait_record.repository = "o/r".to_string();
    wait_record.issue_number = 62;
    wait_record.pr_number = Some(7);
    wait_record.wait_condition = json!({ "checks": "pending" });
    wait_record.next_poll_at = next_poll_at;
    wait_record.resume_step = "collect_ci_failures".to_string();
    wait_record.checkpoint_id = "cp-1".to_string();
    wait_record
}

fn suspension_id(c: &Connection, run_id: &str) -> String {
    get_wait_state(c, run_id).unwrap().unwrap().suspension_id
}

fn insert_lease(c: &Connection, run_id: &str, issue_number: u64, status: &str) {
    c.execute(
        "INSERT INTO issue_leases
                (lease_id, issue_repo, issue_number, config_id, run_id, status,
                 claimed_at, updated_at, heartbeat_at)
             VALUES (?1,'o/r',?2,'cfg',?1,?3,?4,?4,?4)",
        params![run_id, issue_number, status, Utc::now().to_rfc3339()],
    )
    .unwrap();
}

#[test]
fn init_migrates_legacy_wait_rows_with_stable_suspension_ids() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE wait_states (
            run_id TEXT PRIMARY KEY, lease_id TEXT, workflow_type TEXT NOT NULL,
            config_id TEXT NOT NULL, repository TEXT NOT NULL, issue_number INTEGER NOT NULL,
            pr_number INTEGER, head_sha TEXT, wait_kind TEXT NOT NULL,
            wait_condition_json TEXT NOT NULL, last_observed_state_json TEXT NOT NULL,
            next_poll_at TEXT NOT NULL, poll_interval_seconds INTEGER NOT NULL,
            max_wait_seconds INTEGER, resume_step TEXT NOT NULL, checkpoint_id TEXT NOT NULL,
            poll_count INTEGER NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL
         );
         INSERT INTO wait_states VALUES (
            'legacy-run', 'legacy-lease', 'wf', 'cfg', 'o/r', 1, NULL, NULL,
            'pr_checks', 'null', 'null', '2026-01-01T00:00:00Z', 30, NULL,
            'watch', 'checkpoint', 0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'
         );",
    )
    .unwrap();

    init_wait_states_table(&c).unwrap();
    let first = get_wait_state(&c, "legacy-run")
        .unwrap()
        .unwrap()
        .suspension_id;
    assert!(!first.is_empty());

    init_wait_states_table(&c).unwrap();
    let second = get_wait_state(&c, "legacy-run")
        .unwrap()
        .unwrap()
        .suspension_id;
    assert_eq!(
        second, first,
        "migration must populate the generation only once"
    );
}

#[test]
fn persist_external_wait_rejects_incomplete_canonical_identity_before_writes() {
    let c = conn();
    let _lease_id = seed_running_run(&c, "run-incomplete", 198);
    let mut wait = WaitStateRecord::new("run-incomplete", "cfg");
    wait.resume_step = "watch_pr_checks".to_string();

    let error = persist_external_wait(&c, &wait).unwrap_err();
    assert!(matches!(
        error,
        ExternalWaitError::IdentityIncomplete {
            field: "lease_id",
            ..
        }
    ));
    assert!(get_wait_state(&c, "run-incomplete").unwrap().is_none());
    let run = crate::persistence::get_run_with_conn(&c, "run-incomplete")
        .unwrap()
        .unwrap();
    assert_eq!(run.status, RunStatus::Initialized);
}

#[test]
fn wait_kind_serialization_roundtrips() {
    for kind in [
        WaitKind::PrChecks,
        WaitKind::CoderabbitReview,
        WaitKind::HumanReview,
        WaitKind::PrMerge,
        WaitKind::RateLimitBackoff,
        WaitKind::DependencyChildWorkflow,
        WaitKind::DependencyChildMerge,
    ] {
        assert_eq!(kind.to_string().parse::<WaitKind>().unwrap(), kind);
    }
}

#[test]
fn upsert_get_and_delete_roundtrip() {
    let c = conn();
    let now = Utc::now();
    upsert_wait_state(&c, &record("run-1", now)).unwrap();
    let fetched = get_wait_state(&c, "run-1").unwrap().unwrap();
    assert_eq!(fetched.repository, "o/r");
    assert_eq!(fetched.wait_kind, WaitKind::PrChecks);
    assert!(delete_wait_state(&c, "run-1").unwrap());
    assert!(get_wait_state(&c, "run-1").unwrap().is_none());
}

#[test]
fn list_pollable_orders_due_records_only() {
    let c = conn();
    let now = Utc::now();
    insert_lease(&c, "run-later", 1, "waiting_external");
    insert_lease(&c, "run-now", 2, "waiting_external");
    insert_lease(&c, "run-earlier", 3, "waiting_external");
    upsert_wait_state(&c, &record("run-later", now + Duration::minutes(5))).unwrap();
    upsert_wait_state(&c, &record("run-now", now)).unwrap();
    upsert_wait_state(&c, &record("run-earlier", now - Duration::minutes(5))).unwrap();
    let due = list_pollable_wait_states(&c, now).unwrap();
    let run_ids: Vec<&str> = due.iter().map(|r| r.run_id.as_str()).collect();
    assert_eq!(run_ids, vec!["run-earlier", "run-now"]);
}

#[test]
fn list_pollable_excludes_waits_without_protective_lease() {
    let c = conn();
    let now = Utc::now();
    insert_lease(&c, "run-active", 1, "waiting_external");
    insert_lease(&c, "run-done", 2, "completed");
    upsert_wait_state(&c, &record("run-active", now)).unwrap();
    upsert_wait_state(&c, &record("run-done", now)).unwrap();
    upsert_wait_state(&c, &record("run-orphan", now)).unwrap();

    let due = list_pollable_wait_states(&c, now).unwrap();

    let run_ids: Vec<&str> = due.iter().map(|r| r.run_id.as_str()).collect();
    assert_eq!(run_ids, vec!["run-active"]);
}

#[test]
fn list_pollable_excludes_wait_owned_by_a_different_run() {
    let c = conn();
    let now = Utc::now();
    insert_lease(&c, "run-old", 1, "waiting_external");
    upsert_wait_state(&c, &record("run-old", now)).unwrap();
    c.execute(
        "UPDATE issue_leases SET run_id = 'run-new' WHERE lease_id = 'run-old'",
        [],
    )
    .unwrap();

    let due = list_pollable_wait_states(&c, now).unwrap();

    assert!(
        due.is_empty(),
        "a reclaimed lease must not make its previous owner's wait pollable"
    );
}

#[test]
fn list_pollable_excludes_non_waiting_external_leases() {
    // Only `waiting_external` leases are pollable: `ready_to_resume` is
    // handled by the resume path, and `running`/`claimed` are in-flight
    // launches that have not yet suspended.
    let c = conn();
    let now = Utc::now();
    insert_lease(&c, "run-waiting", 1, "waiting_external");
    insert_lease(&c, "run-ready", 2, "ready_to_resume");
    insert_lease(&c, "run-running", 3, "running");
    insert_lease(&c, "run-claimed", 4, "claimed");
    upsert_wait_state(&c, &record("run-waiting", now)).unwrap();
    upsert_wait_state(&c, &record("run-ready", now)).unwrap();
    upsert_wait_state(&c, &record("run-running", now)).unwrap();
    upsert_wait_state(&c, &record("run-claimed", now)).unwrap();

    let due = list_pollable_wait_states(&c, now).unwrap();

    let run_ids: Vec<&str> = due.iter().map(|r| r.run_id.as_str()).collect();
    assert_eq!(run_ids, vec!["run-waiting"]);
}

#[test]
fn update_after_poll_records_backoff_and_count() {
    let c = conn();
    let next = Utc::now() + Duration::minutes(10);
    let wait = record("run-1", Utc::now());
    let original_suspension_id = wait.suspension_id.clone();
    upsert_wait_state(&c, &wait).unwrap();
    update_wait_state_after_poll(
        &c,
        "run-1",
        &json!({ "state": "pending" }),
        next,
        0,
        &suspension_id(&c, "run-1"),
    )
    .unwrap();
    let fetched = get_wait_state(&c, "run-1").unwrap().unwrap();
    assert_eq!(fetched.poll_count, 1);
    assert_eq!(fetched.suspension_id, original_suspension_id);
    assert_eq!(fetched.last_observed_state, json!({ "state": "pending" }));
    assert_eq!(fetched.next_poll_at, next);
}

/// Helper: create a Running lease, run metadata, and checkpoint for a run.
fn seed_running_run(c: &Connection, run_id: &str, issue_number: u64) -> String {
    use crate::persistence::leases::{try_claim, update_lease_status, LeaseStatus};
    let lease = try_claim(c, "o/r", issue_number, "cfg").unwrap().unwrap();
    update_lease_status(c, &lease.lease_id, LeaseStatus::Running, Some(run_id)).unwrap();
    let metadata = crate::persistence::RunMetadata::new(run_id, "wf", "cfg");
    crate::persistence::sqlite::persist_run_with_conn(c, &metadata).unwrap();
    crate::persistence::checkpoint::save_checkpoint_with_conn(
        c,
        &crate::persistence::checkpoint::Checkpoint::new(run_id, "watch_pr_checks"),
    )
    .unwrap();
    lease.lease_id
}

/// Build a representative WaitStateRecord for `persist_external_wait` tests.
fn wait_record_for(run_id: &str, lease_id: &str, issue_number: u64) -> WaitStateRecord {
    let mut record = WaitStateRecord::new(run_id, "cfg");
    record.lease_id = Some(lease_id.to_string());
    record.workflow_type = "issue-fix".to_string();
    record.repository = "o/r".to_string();
    record.issue_number = issue_number;
    record.pr_number = Some(133);
    record.head_sha = Some("abc123".to_string());
    record.wait_kind = WaitKind::CoderabbitReview;
    record.wait_condition = json!({ "review": "pending", "minimum_approvals": 1 });
    record.last_observed_state = json!({ "status": "reviewing" });
    record.next_poll_at = Utc::now() + Duration::minutes(5);
    record.poll_interval_seconds = 90;
    record.max_wait_seconds = Some(3_600);
    record.resume_step = "watch_pr_checks".to_string();
    record.checkpoint_id = "checkpoint-external-wait".to_string();
    record.poll_count = 4;
    record
}

#[test]
fn persist_external_wait_establishes_complete_invariant() {
    let c = conn();
    let lease_id = seed_running_run(&c, "run-ext", 201);
    let expected_wait = wait_record_for("run-ext", &lease_id, 201);
    persist_external_wait(&c, &expected_wait).unwrap();

    // All four facets of the invariant must hold.
    assert!(has_pollable_external_wait(&c, "run-ext").unwrap());
    let run = crate::persistence::sqlite::get_run_with_conn(&c, "run-ext")
        .unwrap()
        .unwrap();
    assert_eq!(run.status, RunStatus::WaitingExternal);
    let wait = get_wait_state(&c, "run-ext").unwrap().unwrap();
    assert_eq!(
        wait, expected_wait,
        "every external-wait field must persist"
    );
    assert_eq!(wait.wait_kind, WaitKind::CoderabbitReview);
    assert_eq!(
        wait.wait_condition,
        json!({ "review": "pending", "minimum_approvals": 1 })
    );
    assert_eq!(wait.next_poll_at, expected_wait.next_poll_at);
    assert_eq!(wait.checkpoint_id, "checkpoint-external-wait");
    assert_eq!(wait.resume_step, "watch_pr_checks");
    let lease = crate::persistence::leases::get_lease_for_issue(&c, "o/r", 201)
        .unwrap()
        .unwrap();
    assert_eq!(
        lease.status,
        crate::persistence::leases::LeaseStatus::WaitingExternal
    );
}

#[test]
fn persist_external_wait_rejects_terminal_lease() {
    // a terminal lease must not be overwritten.
    let c = conn();
    let lease_id = seed_running_run(&c, "run-term", 202);
    // Mark the lease terminal (simulating poller classification).
    crate::persistence::leases::update_lease_status(
        &c,
        &lease_id,
        crate::persistence::leases::LeaseStatus::Failed,
        Some("run-term"),
    )
    .unwrap();

    let result = persist_external_wait(&c, &wait_record_for("run-term", &lease_id, 202));
    assert!(result.is_err(), "must reject when lease is terminal");

    // The wait-state row must not have been written (transaction rolled back).
    assert!(get_wait_state(&c, "run-term").unwrap().is_none());
    // The lease must remain terminal.
    let lease = crate::persistence::leases::get_lease_for_issue(&c, "o/r", 202)
        .unwrap()
        .unwrap();
    assert_eq!(
        lease.status,
        crate::persistence::leases::LeaseStatus::Failed
    );
}

#[test]
fn has_pollable_external_wait_false_for_missing_wait_row() {
    // run status alone is insufficient; a pollable wait
    // row must exist.
    let c = conn();
    let lease_id = seed_running_run(&c, "run-norow", 203);
    // Mark run WaitingExternal but leave no wait_states row.
    let mut run = crate::persistence::sqlite::get_run_with_conn(&c, "run-norow")
        .unwrap()
        .unwrap();
    run.status = RunStatus::WaitingExternal;
    crate::persistence::sqlite::persist_run_with_conn(&c, &run).unwrap();
    crate::persistence::leases::update_lease_status(
        &c,
        &lease_id,
        crate::persistence::leases::LeaseStatus::WaitingExternal,
        Some("run-norow"),
    )
    .unwrap();

    assert!(
        !has_pollable_external_wait(&c, "run-norow").unwrap(),
        "missing wait_states row means the wait is not pollable"
    );
}

#[test]
fn has_pollable_external_wait_false_for_non_waiting_lease() {
    // even with a wait row, a non-waiting lease means
    // the run is not pollable (the poller already classified it).
    let c = conn();
    let lease_id = seed_running_run(&c, "run-ready", 204);
    persist_external_wait(&c, &wait_record_for("run-ready", &lease_id, 204)).unwrap();
    // Poller transitions the lease to ReadyToResume.
    crate::persistence::leases::update_lease_status(
        &c,
        &lease_id,
        crate::persistence::leases::LeaseStatus::ReadyToResume,
        Some("run-ready"),
    )
    .unwrap();

    assert!(
        !has_pollable_external_wait(&c, "run-ready").unwrap(),
        "a ready_to_resume lease is not pollable"
    );
}

#[test]
fn get_wait_state_returns_err_on_observed_state_decode_failure() {
    let c = conn();
    let lease_id = seed_running_run(&c, "run-corrupt", 205);
    persist_external_wait(&c, &wait_record_for("run-corrupt", &lease_id, 205)).unwrap();
    c.execute(
        "UPDATE wait_states SET last_observed_state_json = 'NOT-JSON' WHERE run_id = ?1",
        rusqlite::params!["run-corrupt"],
    )
    .unwrap();

    let result = get_wait_state(&c, "run-corrupt");
    assert!(
        result.is_err(),
        "the wait-state read must surface malformed observed-state JSON"
    );
}

#[test]
fn persist_external_wait_rejects_terminal_run_status() {
    // a run already in a terminal state (e.g. Failed)
    // must not be resurrected back to WaitingExternal. The transaction
    // must roll back so the wait-state row is not written.
    let c = conn();
    let lease_id = seed_running_run(&c, "run-term-2", 210);
    // Simulate a concurrent path marking the run Failed.
    let mut run = crate::persistence::sqlite::get_run_with_conn(&c, "run-term-2")
        .unwrap()
        .unwrap();
    run.status = RunStatus::Failed;
    crate::persistence::sqlite::persist_run_with_conn(&c, &run).unwrap();

    let result = persist_external_wait(&c, &wait_record_for("run-term-2", &lease_id, 210));
    assert!(result.is_err(), "must reject when run is already terminal");
    assert!(
        matches!(
            result.unwrap_err(),
            ExternalWaitError::RunAlreadyTerminal { .. }
        ),
        "must return RunAlreadyTerminal domain error"
    );

    // The run status must remain Failed (not resurrected).
    let run = crate::persistence::sqlite::get_run_with_conn(&c, "run-term-2")
        .unwrap()
        .unwrap();
    assert_eq!(run.status, RunStatus::Failed);
    // The wait-state row must not have been written.
    assert!(get_wait_state(&c, "run-term-2").unwrap().is_none());
}

#[test]
fn persist_external_wait_returns_run_missing_for_absent_metadata() {
    // when run metadata is absent (integrity failure),
    // persist_external_wait must return RunMissing rather than a rusqlite
    // QueryReturnedNoRows variant. Seed a checkpoint so set_resume_point
    // succeeds, isolating the test to the mark_run_waiting_external path.
    let c = conn();
    // Insert a lease but no run metadata.
    insert_lease(&c, "orphan-run", 999, "running");
    crate::persistence::checkpoint::save_checkpoint_with_conn(
        &c,
        &crate::persistence::checkpoint::Checkpoint::new("orphan-run", "watch_pr_checks"),
    )
    .unwrap();

    let result = persist_external_wait(&c, &wait_record_for("orphan-run", "orphan-run", 999));
    assert!(result.is_err());
    assert!(
        matches!(
            result.unwrap_err(),
            ExternalWaitError::RunMissing(ref id) if id == "orphan-run"
        ),
        "must return RunMissing domain error for absent run metadata"
    );
}

#[test]
fn persist_external_wait_preserves_checkpoint_error_source() {
    // a checkpoint persistence error must surface as
    // ExternalWaitError::Checkpoint with the original source preserved,
    // not as a rusqlite ToSqlConversionFailure that loses the type chain.
    let c = conn();
    // Seed a running run but no checkpoint for the resume step —
    // set_resume_point returns NotFound when no checkpoint exists.
    let lease_id = seed_running_run(&c, "run-noresume", 211);
    // Delete the checkpoint that seed_running_run created so
    // set_resume_point fails with NotFound.
    c.execute(
        "DELETE FROM checkpoints WHERE run_id = ?1",
        rusqlite::params!["run-noresume"],
    )
    .unwrap();

    let result = persist_external_wait(&c, &wait_record_for("run-noresume", &lease_id, 211));
    assert!(result.is_err());
    // The error must be ExternalWaitError::Checkpoint, preserving the
    // original CheckpointPersistenceError source chain.
    assert!(
        matches!(
            result.unwrap_err(),
            ExternalWaitError::Checkpoint(
                crate::persistence::checkpoint::PersistenceError::NotFound(_)
            )
        ),
        "checkpoint error must be preserved as ExternalWaitError::Checkpoint"
    );
}

#[test]
fn persist_external_wait_lease_rejection_returns_domain_error() {
    // when the conditional lease update is rejected,
    // the error must be ExternalWaitError::LeaseTransitionRejected, not
    // a rusqlite ToSqlConversionFailure.
    let c = conn();
    let lease_id = seed_running_run(&c, "run-rej", 212);
    // Advance the lease past the expected statuses so the conditional
    // update matches zero rows.
    crate::persistence::leases::update_lease_status(
        &c,
        &lease_id,
        crate::persistence::leases::LeaseStatus::ReadyToResume,
        Some("run-rej"),
    )
    .unwrap();

    let result = persist_external_wait(&c, &wait_record_for("run-rej", &lease_id, 212));
    assert!(result.is_err());
    assert!(
        matches!(
            result.unwrap_err(),
            ExternalWaitError::LeaseTransitionRejected { ref run_id }
                if run_id == "run-rej"
        ),
        "must return LeaseTransitionRejected domain error"
    );
    // The wait-state row must not have been written (transaction rolled back).
    assert!(get_wait_state(&c, "run-rej").unwrap().is_none());
}

#[test]
fn persist_external_wait_atomic_terminal_guard_rejects_all_terminal_statuses() {
    // OCR mark_run_waiting_external atomic guard: verify the conditional
    // UPDATE rejects every terminal status with RunAlreadyTerminal.
    for terminal in [
        RunStatus::Completed,
        RunStatus::Failed,
        RunStatus::Abandoned,
        RunStatus::Merged,
        RunStatus::Cancelled,
    ] {
        let c = conn();
        let issue_num = match terminal {
            RunStatus::Completed => 220,
            RunStatus::Failed => 221,
            RunStatus::Abandoned => 222,
            RunStatus::Merged => 223,
            RunStatus::Cancelled => 224,
            _ => unreachable!(),
        };
        let lease_id = seed_running_run(&c, "run-atom", issue_num);
        // Atomically set the run to the terminal status.
        let mut run = crate::persistence::sqlite::get_run_with_conn(&c, "run-atom")
            .unwrap()
            .unwrap();
        run.status = terminal.clone();
        crate::persistence::sqlite::persist_run_with_conn(&c, &run).unwrap();

        let result = persist_external_wait(&c, &wait_record_for("run-atom", &lease_id, issue_num));
        assert!(result.is_err(), "must reject terminal status {terminal}");
        assert!(
            matches!(
                result.unwrap_err(),
                ExternalWaitError::RunAlreadyTerminal { ref current, .. }
                    if *current == terminal
            ),
            "must return RunAlreadyTerminal for {terminal}"
        );
        // The run status must remain terminal (not resurrected).
        let run = crate::persistence::sqlite::get_run_with_conn(&c, "run-atom")
            .unwrap()
            .unwrap();
        assert_eq!(run.status, terminal);
        // The wait-state row must not have been written.
        assert!(get_wait_state(&c, "run-atom").unwrap().is_none());
    }
}

#[test]
fn persist_external_wait_atomic_guard_distinguishes_missing_from_terminal() {
    // OCR mark_run_waiting_external atomic guard: the missing-vs-terminal
    // classification must be correct after the atomic UPDATE matches zero
    // rows. A missing run must surface as RunMissing, not RunAlreadyTerminal.
    let c = conn();
    // Insert a lease but no run metadata row at all.
    insert_lease(&c, "run-genuinely-missing", 1_000, "running");
    crate::persistence::checkpoint::save_checkpoint_with_conn(
        &c,
        &crate::persistence::checkpoint::Checkpoint::new(
            "run-genuinely-missing",
            "watch_pr_checks",
        ),
    )
    .unwrap();

    let result = persist_external_wait(
        &c,
        &wait_record_for("run-genuinely-missing", "run-genuinely-missing", 999),
    );
    assert!(result.is_err());
    assert!(
        matches!(
            result.unwrap_err(),
            ExternalWaitError::RunMissing(ref id)
                if id == "run-genuinely-missing"
        ),
        "a genuinely missing run must surface as RunMissing, not RunAlreadyTerminal"
    );
}

#[test]
fn run_status_terminal_sql_matches_is_terminal() {
    // OCR 3565579058: the SQL terminal-status list must agree with
    // RunStatus::is_terminal so the conditional UPDATE guard and the
    // Rust method can never disagree.
    let all_statuses = [
        RunStatus::Initialized,
        RunStatus::Queued,
        RunStatus::Starting,
        RunStatus::Running,
        RunStatus::WaitingForChecks,
        RunStatus::WaitingExternal,
        RunStatus::ReadyToResume,
        RunStatus::Remediating,
        RunStatus::Blocked,
        RunStatus::Paused,
        RunStatus::Completed,
        RunStatus::Failed,
        RunStatus::Abandoned,
        RunStatus::Merged,
        RunStatus::Cancelled,
    ];
    for status in &all_statuses {
        let display = status.to_string();
        let parsed: RunStatus = display
            .parse()
            .unwrap_or_else(|error| panic!("displayed status {display} must parse: {error}"));
        assert_eq!(
            parsed, *status,
            "every RunStatus must round-trip through text"
        );

        let in_sql_list = RunStatus::TERMINAL_SQL.contains(&display.as_str());
        assert_eq!(
            status.is_terminal(),
            in_sql_list,
            "TERMINAL_SQL membership must match is_terminal for {status}"
        );
    }
    for value in RunStatus::TERMINAL_SQL {
        let parsed: RunStatus = value
            .parse()
            .unwrap_or_else(|error| panic!("TERMINAL_SQL value {value} must parse: {error}"));
        assert!(
            parsed.is_terminal(),
            "TERMINAL_SQL value {value} must parse as a terminal status"
        );
    }

    let terminal_count = all_statuses.iter().filter(|s| s.is_terminal()).count();
    assert_eq!(
        RunStatus::TERMINAL_SQL.len(),
        terminal_count,
        "TERMINAL_SQL length must match the number of terminal statuses"
    );
}

#[test]
fn update_wait_state_after_poll_rejects_stale_poll_count() {
    // OCR 3565817583: the optimistic poll_count version guard must reject
    // a stale refresh whose expected_poll_count no longer matches the
    // stored value. After a concurrent poller increments poll_count, a
    // second poller using the old poll_count must get Ok(false) instead of
    // silently overwriting (last-writer-wins).
    let c = conn();
    let now = Utc::now();
    upsert_wait_state(&c, &record("run-stale", now)).unwrap();

    // First poller reads poll_count=0 and refreshes.
    let applied = update_wait_state_after_poll(
        &c,
        "run-stale",
        &json!({ "poller": "first" }),
        now + Duration::minutes(1),
        0,
        &suspension_id(&c, "run-stale"),
    )
    .unwrap();
    assert!(
        applied,
        "first poller with matching poll_count=0 must succeed"
    );

    // Verify poll_count incremented.
    let after_first = get_wait_state(&c, "run-stale").unwrap().unwrap();
    assert_eq!(after_first.poll_count, 1);
    assert_eq!(
        after_first.last_observed_state,
        json!({ "poller": "first" })
    );

    // Second (stale) poller still thinks poll_count=0 — must be rejected.
    let applied = update_wait_state_after_poll(
        &c,
        "run-stale",
        &json!({ "poller": "second" }),
        now + Duration::minutes(2),
        0,
        &suspension_id(&c, "run-stale"),
    )
    .unwrap();
    assert!(
        !applied,
        "stale poll_count=0 must be rejected after the row advanced to poll_count=1"
    );

    // The row must reflect the first poller's write, not the stale second's.
    let after_second = get_wait_state(&c, "run-stale").unwrap().unwrap();
    assert_eq!(after_second.poll_count, 1);
    assert_eq!(
        after_second.last_observed_state,
        json!({ "poller": "first" }),
        "stale poller must not overwrite the first poller's observed_state"
    );
}

#[test]
fn update_wait_state_after_poll_rejects_replacement_suspension_at_same_poll_count() {
    let c = conn();
    let now = Utc::now();
    let cycle_a = record("run-aba", now);
    let cycle_a_id = cycle_a.suspension_id.clone();
    upsert_wait_state(&c, &cycle_a).unwrap();

    let cycle_b_id = "replacement-suspension";
    c.execute(
        "UPDATE wait_states
         SET suspension_id = ?1, last_observed_state_json = ?2
         WHERE run_id = ?3",
        params![cycle_b_id, json!({ "cycle": "b" }).to_string(), "run-aba"],
    )
    .unwrap();

    let applied = update_wait_state_after_poll(
        &c,
        "run-aba",
        &json!({ "cycle": "a", "stale": true }),
        now + Duration::minutes(1),
        0,
        &cycle_a_id,
    )
    .unwrap();
    assert!(!applied);
    let stored = get_wait_state(&c, "run-aba").unwrap().unwrap();
    assert_eq!(stored.suspension_id, cycle_b_id);
    assert_eq!(stored.last_observed_state, json!({ "cycle": "b" }));
    assert_eq!(stored.poll_count, 0);
}

#[test]
fn update_wait_state_after_poll_accepts_matching_poll_count() {
    // OCR 3565817583: a poller with the correct expected_poll_count must
    // succeed even after prior increments — the guard is optimistic, not
    // first-only.
    let c = conn();
    let now = Utc::now();
    upsert_wait_state(&c, &record("run-match", now)).unwrap();

    // First poll increments to poll_count=1.
    update_wait_state_after_poll(
        &c,
        "run-match",
        &json!({ "n": 1 }),
        now,
        0,
        &suspension_id(&c, "run-match"),
    )
    .unwrap();

    // Second poll with the correct expected_poll_count=1 must succeed.
    let applied = update_wait_state_after_poll(
        &c,
        "run-match",
        &json!({ "n": 2 }),
        now,
        1,
        &suspension_id(&c, "run-match"),
    )
    .unwrap();
    assert!(applied, "matching poll_count=1 must succeed");

    let fetched = get_wait_state(&c, "run-match").unwrap().unwrap();
    assert_eq!(fetched.poll_count, 2);
    assert_eq!(fetched.last_observed_state, json!({ "n": 2 }));
}

#[test]
fn has_pollable_external_wait_checks_joined_invariant_without_decoding_payloads() {
    let c = conn();
    let lease_id = seed_running_run(&c, "run-joined", 230);
    let wait = wait_record_for("run-joined", &lease_id, 230);
    persist_external_wait(&c, &wait).unwrap();
    c.execute(
        "UPDATE wait_states SET last_observed_state_json = 'NOT-JSON' WHERE run_id = ?1",
        params!["run-joined"],
    )
    .unwrap();

    assert!(has_pollable_external_wait(&c, "run-joined").unwrap());

    c.execute(
        "UPDATE issue_leases SET run_id = 'replacement-run' WHERE lease_id = ?1",
        params![lease_id],
    )
    .unwrap();
    assert!(!has_pollable_external_wait(&c, "run-joined").unwrap());
}

#[test]
fn upsert_rejects_empty_suspension_id_without_writing() {
    let c = conn();
    let mut wait = record("run-empty-suspension", Utc::now());
    wait.suspension_id.clear();

    let error = upsert_wait_state(&c, &wait).unwrap_err();

    assert!(matches!(
        error,
        WaitStateWriteError::EmptySuspensionId { ref run_id }
            if run_id == "run-empty-suspension"
    ));
    assert!(
        get_wait_state(&c, "run-empty-suspension")
            .unwrap()
            .is_none(),
        "validation must happen before any database write"
    );
}
