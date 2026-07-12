use super::*;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

fn conn() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    init_leases_table(&c).unwrap();
    c
}

fn assert_issue_number_overflow(error: rusqlite::Error, issue_number: u64) {
    let rusqlite::Error::ToSqlConversionFailure(source) = error else {
        panic!("expected ToSqlConversionFailure, got {error}");
    };
    assert!(
        source.to_string().contains(&issue_number.to_string()),
        "conversion diagnostic must include the rejected issue number"
    );
    assert!(
        source
            .source()
            .and_then(|cause| cause.downcast_ref::<std::num::TryFromIntError>())
            .is_some(),
        "conversion error must retain TryFromIntError in its source chain"
    );
}

#[test]
fn status_display_fromstr_round_trip() {
    for status in [
        LeaseStatus::Pending,
        LeaseStatus::Claimed,
        LeaseStatus::Running,
        LeaseStatus::WaitingExternal,
        LeaseStatus::ReadyToResume,
        LeaseStatus::Completed,
        LeaseStatus::Failed,
        LeaseStatus::Abandoned,
        LeaseStatus::Stale,
    ] {
        let s = status.to_string();
        assert_eq!(s.parse::<LeaseStatus>().unwrap(), status);
    }
}

#[test]
fn create_then_get_round_trip() {
    let c = conn();
    let claimed = try_claim(&c, "o/r", 7, "cfg").unwrap().unwrap();
    let fetched = get_lease_for_issue(&c, "o/r", 7).unwrap().unwrap();
    assert_eq!(fetched.issue_number, 7);
    assert_eq!(fetched.config_id, "cfg");
    assert_eq!(fetched.lease_id, claimed.lease_id);
    assert_eq!(fetched.status, LeaseStatus::Claimed);
}

#[test]
fn try_claim_second_attempt_loses() {
    let c = conn();
    let first = try_claim(&c, "o/r", 1, "cfg-a").unwrap();
    let second = try_claim(&c, "o/r", 1, "cfg-b").unwrap();
    assert!(first.is_some());
    assert!(second.is_none(), "duplicate claim must be rejected");
}

#[test]
fn try_claim_reclaims_terminal_lease() {
    // A finished/abandoned issue must be pickable again on a later pass,
    // matching blocks_duplicate_work(); otherwise the daemon can never
    // re-work an issue whose prior run failed or was abandoned.
    for terminal in LeaseStatus::RECLAIMABLE {
        let c = conn();
        let first = try_claim(&c, "o/r", 1, "cfg-a").unwrap().unwrap();
        update_lease_status(&c, &first.lease_id, terminal, Some("run-old")).unwrap();

        let reclaim = try_claim(&c, "o/r", 1, "cfg-b").unwrap();
        assert!(
            reclaim.is_some(),
            "terminal lease ({terminal}) must be reclaimable"
        );
        let reclaim = reclaim.unwrap();
        assert_ne!(
            reclaim.lease_id, first.lease_id,
            "reclaim must mint a fresh lease id"
        );

        let fetched = get_lease_for_issue(&c, "o/r", 1).unwrap().unwrap();
        assert_eq!(fetched.lease_id, reclaim.lease_id);
        assert_eq!(fetched.status, LeaseStatus::Claimed);
        assert_eq!(fetched.config_id, "cfg-b");
        assert_eq!(
            fetched.run_id, None,
            "a fresh claim must clear the prior run id"
        );
        // Exactly one lease row per issue is preserved.
        assert_eq!(list_all_leases(&c).unwrap().len(), 1);
    }
}

#[test]
fn try_claim_does_not_reclaim_active_lease() {
    // Claimed and Running leases still hold the issue: a concurrent claim
    // must lose and must not disturb the in-flight lease.
    for active in [LeaseStatus::Claimed, LeaseStatus::Running] {
        let c = conn();
        let first = try_claim(&c, "o/r", 2, "cfg-a").unwrap().unwrap();
        update_lease_status(&c, &first.lease_id, active, Some("run-live")).unwrap();

        let second = try_claim(&c, "o/r", 2, "cfg-b").unwrap();
        assert!(
            second.is_none(),
            "active lease ({active}) must not be reclaimable"
        );

        let fetched = get_lease_for_issue(&c, "o/r", 2).unwrap().unwrap();
        assert_eq!(
            fetched.lease_id, first.lease_id,
            "in-flight lease preserved"
        );
        assert_eq!(fetched.status, active);
        assert_eq!(fetched.config_id, "cfg-a");
        assert_eq!(fetched.run_id.as_deref(), Some("run-live"));
    }
}

#[test]
fn concurrent_terminal_reclaim_has_one_winner() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("leases.db");
    let seed = Connection::open(&path).unwrap();
    init_leases_table(&seed).unwrap();
    let previous = try_claim(&seed, "o/r", 3, "cfg-old").unwrap().unwrap();
    update_lease_status(
        &seed,
        &previous.lease_id,
        LeaseStatus::Failed,
        Some("run-old"),
    )
    .unwrap();
    drop(seed);

    let barrier = Arc::new(Barrier::new(2));
    let claims = ["cfg-a", "cfg-b"].map(|config_id| {
        let path = path.clone();
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            let connection = Connection::open(path).unwrap();
            connection.busy_timeout(Duration::from_secs(5)).unwrap();
            barrier.wait();
            try_claim(&connection, "o/r", 3, config_id).unwrap()
        })
    });
    let results = claims.map(|claim| claim.join().unwrap());
    let winner = results.into_iter().flatten().collect::<Vec<_>>();
    assert_eq!(winner.len(), 1, "exactly one reclaim must win");

    let connection = Connection::open(path).unwrap();
    let fetched = get_lease_for_issue(&connection, "o/r", 3).unwrap().unwrap();
    assert_eq!(fetched.lease_id, winner[0].lease_id);
    assert_eq!(fetched.status, LeaseStatus::Claimed);
    assert_eq!(fetched.run_id, None);
    assert_eq!(list_all_leases(&connection).unwrap().len(), 1);
}

#[test]
fn update_status_transitions() {
    let c = conn();
    let lease = try_claim(&c, "o/r", 2, "cfg").unwrap().unwrap();
    update_lease_status(&c, &lease.lease_id, LeaseStatus::Running, Some("run-9")).unwrap();
    let fetched = get_lease_for_issue(&c, "o/r", 2).unwrap().unwrap();
    assert_eq!(fetched.status, LeaseStatus::Running);
    assert_eq!(fetched.run_id.as_deref(), Some("run-9"));
}

#[test]
fn conditional_update_applies_when_status_matches() {
    let c = conn();
    let lease = try_claim(&c, "o/r", 50, "cfg").unwrap().unwrap();
    update_lease_status(&c, &lease.lease_id, LeaseStatus::Running, None).unwrap();
    let applied = update_lease_status_conditional(
        &c,
        &lease.lease_id,
        LeaseStatus::WaitingExternal,
        &[LeaseStatus::Running],
        Some("run-cond"),
        None,
    )
    .unwrap();
    assert!(applied, "transition from Running must apply");
    let fetched = get_lease_for_issue(&c, "o/r", 50).unwrap().unwrap();
    assert_eq!(fetched.status, LeaseStatus::WaitingExternal);
    assert_eq!(fetched.run_id.as_deref(), Some("run-cond"));
}

#[test]
fn conditional_update_rejected_when_status_mismatched() {
    // a terminal lease must not regress.
    let c = conn();
    let lease = try_claim(&c, "o/r", 51, "cfg").unwrap().unwrap();
    update_lease_status(&c, &lease.lease_id, LeaseStatus::Failed, None).unwrap();
    let applied = update_lease_status_conditional(
        &c,
        &lease.lease_id,
        LeaseStatus::WaitingExternal,
        &[LeaseStatus::Running, LeaseStatus::WaitingExternal],
        Some("run-cond"),
        None,
    )
    .unwrap();
    assert!(!applied, "transition from Failed must be rejected");
    let fetched = get_lease_for_issue(&c, "o/r", 51).unwrap().unwrap();
    assert_eq!(fetched.status, LeaseStatus::Failed);
}

#[test]
fn conditional_update_empty_expected_is_noop() {
    let c = conn();
    let lease = try_claim(&c, "o/r", 52, "cfg").unwrap().unwrap();
    let applied = update_lease_status_conditional(
        &c,
        &lease.lease_id,
        LeaseStatus::WaitingExternal,
        &[],
        None,
        None,
    )
    .unwrap();
    assert!(!applied, "empty expected set is a no-op");
}

#[test]
fn count_active_only_counts_claimed_and_running() {
    let c = conn();
    let l1 = try_claim(&c, "o/r", 10, "cfg").unwrap().unwrap();
    let l2 = try_claim(&c, "o/r", 11, "cfg").unwrap().unwrap();
    let l3 = try_claim(&c, "o/r", 12, "cfg").unwrap().unwrap();
    update_lease_status(&c, &l2.lease_id, LeaseStatus::Running, None).unwrap();
    update_lease_status(&c, &l3.lease_id, LeaseStatus::Completed, None).unwrap();
    // l1 Claimed + l2 Running = 2 active; l3 Completed excluded.
    assert_eq!(count_active_leases_for_config(&c, "cfg").unwrap(), 2);
    let _ = l1;
}

#[test]
fn waiting_external_blocks_duplicates_but_not_active_capacity() {
    let c = conn();
    let lease = try_claim(&c, "o/r", 13, "cfg").unwrap().unwrap();
    update_lease_status(
        &c,
        &lease.lease_id,
        LeaseStatus::WaitingExternal,
        Some("run-13"),
    )
    .unwrap();
    let duplicate = try_claim(&c, "o/r", 13, "cfg").unwrap();
    assert!(duplicate.is_none());
    assert_eq!(count_active_leases_for_config(&c, "cfg").unwrap(), 0);
    let fetched = get_lease_for_issue(&c, "o/r", 13).unwrap().unwrap();
    assert!(fetched.status.blocks_duplicate_work());
}

fn lease_with_heartbeat(
    lease_id: &str,
    issue_number: u64,
    run_id: Option<&str>,
    status: LeaseStatus,
    heartbeat_at: chrono::DateTime<Utc>,
) -> IssueLease {
    IssueLease {
        lease_id: lease_id.to_string(),
        issue_repo: "o/r".to_string(),
        issue_number,
        config_id: "cfg".to_string(),
        run_id: run_id.map(str::to_string),
        status,
        claimed_at: heartbeat_at,
        updated_at: heartbeat_at,
        heartbeat_at,
    }
}

#[test]
fn stale_sweep_ignores_deliberately_waiting_leases() {
    let c = conn();
    let old = Utc::now() - chrono::Duration::seconds(10_000);
    create_lease(
        &c,
        &lease_with_heartbeat(
            "waiting-1",
            32,
            Some("run-32"),
            LeaseStatus::WaitingExternal,
            old,
        ),
    )
    .unwrap();
    assert_eq!(mark_stale_leases(&c, 300).unwrap(), 0);
    let lease = get_lease_for_issue(&c, "o/r", 32).unwrap().unwrap();
    assert_eq!(lease.status, LeaseStatus::WaitingExternal);
}

#[test]
fn stale_sweep_recovers_overdue_ready_to_resume_leases() {
    let c = conn();
    let old = Utc::now() - chrono::Duration::seconds(10_000);
    create_lease(
        &c,
        &lease_with_heartbeat(
            "ready-1",
            33,
            Some("run-33"),
            LeaseStatus::ReadyToResume,
            old,
        ),
    )
    .unwrap();
    assert_eq!(mark_stale_ready_to_resume_leases(&c, 300).unwrap(), 1);
    let lease = get_lease_for_issue(&c, "o/r", 33).unwrap().unwrap();
    assert_eq!(lease.status, LeaseStatus::Stale);
}

#[test]
fn list_by_status_filters() {
    let c = conn();
    let l1 = try_claim(&c, "o/r", 20, "cfg").unwrap().unwrap();
    let _l2 = try_claim(&c, "o/r", 21, "cfg").unwrap().unwrap();
    update_lease_status(&c, &l1.lease_id, LeaseStatus::Completed, None).unwrap();
    let completed = list_leases_by_status(&c, LeaseStatus::Completed).unwrap();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].issue_number, 20);
    let claimed = list_leases_by_status(&c, LeaseStatus::Claimed).unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].issue_number, 21);
}

#[test]
fn mark_stale_flips_overdue_only() {
    let c = conn();
    // Fresh claim — should not go stale.
    let fresh = try_claim(&c, "o/r", 30, "cfg").unwrap().unwrap();
    // Create an overdue lease with an old heartbeat.
    let old = Utc::now() - chrono::Duration::seconds(10_000);
    create_lease(
        &c,
        &lease_with_heartbeat("stale-1", 31, None, LeaseStatus::Running, old),
    )
    .unwrap();
    let recovered = mark_stale_leases(&c, 300).unwrap();
    assert_eq!(recovered, 1);
    let fresh_now = get_lease_for_issue(&c, "o/r", 30).unwrap().unwrap();
    assert_eq!(fresh_now.status, LeaseStatus::Claimed);
    let stale_now = get_lease_for_issue(&c, "o/r", 31).unwrap().unwrap();
    assert_eq!(stale_now.status, LeaseStatus::Stale);
    let _ = fresh;
}

#[test]
fn list_by_config_and_all() {
    let c = conn();
    try_claim(&c, "o/r", 40, "cfg-a").unwrap();
    try_claim(&c, "o/r", 41, "cfg-b").unwrap();
    assert_eq!(list_leases_by_config(&c, "cfg-a").unwrap().len(), 1);
    assert_eq!(list_all_leases(&c).unwrap().len(), 2);
}

#[test]
fn touch_heartbeat_updates_timestamp() {
    let c = conn();
    let lease = try_claim(&c, "o/r", 50, "cfg").unwrap().unwrap();
    // Pin the heartbeat to a deterministic past timestamp so the assertion
    // is not dependent on system clock resolution or scheduler timing.
    let past = (Utc::now() - chrono::Duration::seconds(3600)).to_rfc3339();
    c.execute(
        "UPDATE issue_leases SET heartbeat_at = ?1 WHERE lease_id = ?2",
        params![past, &lease.lease_id],
    )
    .unwrap();
    let before = get_lease_for_issue(&c, "o/r", 50).unwrap().unwrap();
    touch_lease_heartbeat(&c, &lease.lease_id).unwrap();
    let after = get_lease_for_issue(&c, "o/r", 50).unwrap().unwrap();
    assert!(
        after.heartbeat_at > before.heartbeat_at,
        "heartbeat must advance from the pinned past value to the current time"
    );
}

#[test]
fn create_lease_explicit_record() {
    let c = conn();
    let now = Utc::now();
    let lease = IssueLease {
        lease_id: "explicit-1".to_string(),
        issue_repo: "o/r".to_string(),
        issue_number: 60,
        config_id: "cfg".to_string(),
        run_id: Some("run-1".to_string()),
        status: LeaseStatus::Pending,
        claimed_at: now,
        updated_at: now,
        heartbeat_at: now,
    };
    create_lease(&c, &lease).unwrap();
    let fetched = get_lease_for_issue(&c, "o/r", 60).unwrap().unwrap();
    assert_eq!(fetched.lease_id, "explicit-1");
    assert_eq!(fetched.status, LeaseStatus::Pending);
}

#[test]
fn create_lease_rejects_issue_number_above_sqlite_integer_range() {
    let c = conn();
    let now = Utc::now();
    let lease = IssueLease {
        lease_id: "overflow".to_string(),
        issue_repo: "o/r".to_string(),
        issue_number: u64::MAX,
        config_id: "cfg".to_string(),
        run_id: None,
        status: LeaseStatus::Pending,
        claimed_at: now,
        updated_at: now,
        heartbeat_at: now,
    };

    let error = create_lease(&c, &lease).expect_err("out-of-range issue number must fail");
    assert_issue_number_overflow(error, u64::MAX);
    let count: i64 = c
        .query_row("SELECT COUNT(*) FROM issue_leases", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0, "failed conversion must not insert a lease");
}

#[test]
fn lease_lookups_reject_issue_number_above_sqlite_integer_range() {
    let c = conn();

    for error in [
        get_lease_for_issue(&c, "o/r", u64::MAX)
            .expect_err("single lookup must reject an out-of-range issue number"),
        get_leases_for_issues(&c, "o/r", &[7, u64::MAX])
            .expect_err("batch lookup must reject an out-of-range issue number"),
    ] {
        assert_issue_number_overflow(error, u64::MAX);
    }
}

#[test]
fn conditional_update_rejects_empty_new_run_id_without_mutating_lease() {
    let c = conn();
    let lease = try_claim(&c, "o/r", 52, "cfg").unwrap().unwrap();
    update_lease_status(&c, &lease.lease_id, LeaseStatus::Running, Some("run-keep")).unwrap();

    let error = update_lease_status_conditional(
        &c,
        &lease.lease_id,
        LeaseStatus::WaitingExternal,
        &[LeaseStatus::Running],
        Some(""),
        Some("run-keep"),
    )
    .expect_err("an empty new run ID must be rejected");

    let rusqlite::Error::ToSqlConversionFailure(source) = error else {
        panic!("expected ToSqlConversionFailure, got {error}");
    };
    assert!(
        source.downcast_ref::<InvalidRunIdError>().is_some(),
        "conversion source must preserve the invalid-run-id error type"
    );
    let fetched = get_lease_for_issue(&c, "o/r", 52).unwrap().unwrap();
    assert_eq!(fetched.status, LeaseStatus::Running);
    assert_eq!(fetched.run_id.as_deref(), Some("run-keep"));
}

#[test]
fn conditional_update_preserves_run_id_when_none() {
    // None must preserve the existing run_id — the
    // column must never be nulled out by a conditional transition.
    let c = conn();
    let lease = try_claim(&c, "o/r", 53, "cfg").unwrap().unwrap();
    update_lease_status(&c, &lease.lease_id, LeaseStatus::Running, Some("run-keep")).unwrap();
    let applied = update_lease_status_conditional(
        &c,
        &lease.lease_id,
        LeaseStatus::WaitingExternal,
        &[LeaseStatus::Running],
        None,
        None,
    )
    .unwrap();
    assert!(applied);
    let fetched = get_lease_for_issue(&c, "o/r", 53).unwrap().unwrap();
    assert_eq!(fetched.status, LeaseStatus::WaitingExternal);
    assert_eq!(
        fetched.run_id.as_deref(),
        Some("run-keep"),
        "None run_id must preserve the existing value, not null it"
    );
}

#[test]
fn conditional_update_rejects_stale_run_id_owner() {
    // the expected_run_id guard is the core concurrency
    // protection — a stale writer whose run was superseded by a new run
    // (via lease reclaim) must not mutate the reclaimed lease.
    let c = conn();
    let lease = try_claim(&c, "o/r", 60, "cfg-a").unwrap().unwrap();
    update_lease_status(&c, &lease.lease_id, LeaseStatus::Running, Some("run-old")).unwrap();
    // Simulate a concurrent reclaim: the lease is failed then re-claimed
    // by a new run.
    update_lease_status(&c, &lease.lease_id, LeaseStatus::Failed, Some("run-old")).unwrap();
    let reclaimed = try_claim(&c, "o/r", 60, "cfg-b").unwrap().unwrap();
    update_lease_status(
        &c,
        &reclaimed.lease_id,
        LeaseStatus::Running,
        Some("run-new"),
    )
    .unwrap();

    // The stale "run-old" writer tries to transition the new lease — it
    // must be rejected because the lease now belongs to "run-new".
    let applied = update_lease_status_conditional(
        &c,
        &reclaimed.lease_id,
        LeaseStatus::WaitingExternal,
        &[LeaseStatus::Running],
        Some("run-old"),
        Some("run-old"),
    )
    .unwrap();
    assert!(
        !applied,
        "stale run_id must not mutate a lease owned by a newer run"
    );

    // The matching "run-new" writer must succeed.
    let applied = update_lease_status_conditional(
        &c,
        &reclaimed.lease_id,
        LeaseStatus::WaitingExternal,
        &[LeaseStatus::Running],
        Some("run-new"),
        Some("run-new"),
    )
    .unwrap();
    assert!(
        applied,
        "matching run_id must be able to transition its own lease"
    );
    let fetched = get_lease_for_issue(&c, "o/r", 60).unwrap().unwrap();
    assert_eq!(fetched.status, LeaseStatus::WaitingExternal);
    assert_eq!(fetched.run_id.as_deref(), Some("run-new"));
}

#[test]
fn conditional_update_expected_run_id_allows_matching_owner() {
    // a matching expected_run_id must pass the guard
    // even when the lease is in the expected status set.
    let c = conn();
    let lease = try_claim(&c, "o/r", 61, "cfg").unwrap().unwrap();
    update_lease_status(&c, &lease.lease_id, LeaseStatus::Running, Some("run-61")).unwrap();

    let applied = update_lease_status_conditional(
        &c,
        &lease.lease_id,
        LeaseStatus::ReadyToResume,
        &[LeaseStatus::Running, LeaseStatus::WaitingExternal],
        Some("run-61"),
        Some("run-61"),
    )
    .unwrap();
    assert!(applied, "matching owner must transition");
    let fetched = get_lease_for_issue(&c, "o/r", 61).unwrap().unwrap();
    assert_eq!(fetched.status, LeaseStatus::ReadyToResume);
}

#[test]
fn conditional_update_matching_owner_can_change_run_id() {
    let c = conn();
    let lease = try_claim(&c, "o/r", 63, "cfg").unwrap().unwrap();
    update_lease_status(
        &c,
        &lease.lease_id,
        LeaseStatus::WaitingExternal,
        Some("run-current"),
    )
    .unwrap();

    let applied = update_lease_status_conditional(
        &c,
        &lease.lease_id,
        LeaseStatus::ReadyToResume,
        &[LeaseStatus::WaitingExternal],
        Some("run-next"),
        Some("run-current"),
    )
    .unwrap();

    assert!(
        applied,
        "the durable owner must satisfy the ownership guard"
    );
    let fetched = get_lease_for_issue(&c, "o/r", 63).unwrap().unwrap();
    assert_eq!(fetched.status, LeaseStatus::ReadyToResume);
    assert_eq!(fetched.run_id.as_deref(), Some("run-next"));
}

#[test]
fn conditional_update_expected_run_id_rejects_mismatched_owner() {
    // A mismatched expected_run_id guard must reject a stale writer even
    // when the lease status itself matches.
    let c = conn();
    let lease = try_claim(&c, "o/r", 62, "cfg").unwrap().unwrap();
    update_lease_status(
        &c,
        &lease.lease_id,
        LeaseStatus::WaitingExternal,
        Some("run-62"),
    )
    .unwrap();

    let applied = update_lease_status_conditional(
        &c,
        &lease.lease_id,
        LeaseStatus::ReadyToResume,
        &[LeaseStatus::WaitingExternal],
        None,
        Some("run-different"),
    )
    .unwrap();
    assert!(
        !applied,
        "mismatched expected_run_id must reject even when status matches"
    );
    let fetched = get_lease_for_issue(&c, "o/r", 62).unwrap().unwrap();
    assert_eq!(fetched.status, LeaseStatus::WaitingExternal);
}

#[test]
fn conditional_update_bind_slot_count_matches_placeholders() {
    // guard against silent placeholder/bind mismatch if
    // the query shape changes. Verify the function works across different
    // expected_statuses lengths (1, 2, 4) and with/without expected_run_id
    // — any bind-slot mismatch would cause a runtime SQLite error.
    let c = conn();
    let lease70 = try_claim(&c, "o/r", 70, "cfg").unwrap().unwrap();
    update_lease_status(&c, &lease70.lease_id, LeaseStatus::Running, Some("run-bnd")).unwrap();
    let lease71 = try_claim(&c, "o/r", 71, "cfg").unwrap().unwrap();
    update_lease_status(
        &c,
        &lease71.lease_id,
        LeaseStatus::WaitingExternal,
        Some("run-bnd"),
    )
    .unwrap();
    let lease73 = try_claim(&c, "o/r", 73, "cfg").unwrap().unwrap();
    update_lease_status(&c, &lease73.lease_id, LeaseStatus::Running, Some("run-bnd")).unwrap();

    // 1 expected status, with expected_run_id
    let applied = update_lease_status_conditional(
        &c,
        &lease70.lease_id,
        LeaseStatus::Failed,
        &[LeaseStatus::Running],
        Some("run-bnd"),
        Some("run-bnd"),
    )
    .unwrap();
    assert!(applied, "1 expected status with expected_run_id must match");

    // 2 expected statuses, with expected_run_id
    let applied = update_lease_status_conditional(
        &c,
        &lease71.lease_id,
        LeaseStatus::Failed,
        &[LeaseStatus::WaitingExternal, LeaseStatus::Running],
        Some("run-bnd"),
        Some("run-bnd"),
    )
    .unwrap();
    assert!(
        applied,
        "2 expected statuses with expected_run_id must match"
    );

    // 4 expected statuses with no ownership guard; new_run_id is still provided
    let applied = update_lease_status_conditional(
        &c,
        &lease73.lease_id,
        LeaseStatus::Failed,
        &[
            LeaseStatus::WaitingExternal,
            LeaseStatus::ReadyToResume,
            LeaseStatus::Running,
            LeaseStatus::Claimed,
        ],
        Some("run-bnd"),
        None,
    )
    .unwrap();
    assert!(
        applied,
        "all four expected statuses with correct bind slots"
    );
}

#[test]
fn conditional_update_three_status_bind_slots_match_placeholders() {
    let c = conn();
    let lease = try_claim(&c, "o/r", 72, "cfg").unwrap().unwrap();
    update_lease_status(&c, &lease.lease_id, LeaseStatus::Running, Some("run-bnd")).unwrap();
    let expected = [
        LeaseStatus::WaitingExternal,
        LeaseStatus::ReadyToResume,
        LeaseStatus::Running,
    ];
    assert!(update_lease_status_conditional(
        &c,
        &lease.lease_id,
        LeaseStatus::Failed,
        &expected,
        Some("run-bnd"),
        Some("run-bnd"),
    )
    .unwrap());
}

#[test]
fn try_claim_bind_slot_count_matches_placeholders() {
    // try_claim builds anonymous placeholders from the same ordered parameter
    // groups it binds. Exercise both insert and reclaim paths so either query
    // shape fails here if those groups stop matching.
    let c = conn();
    let result = try_claim(&c, "o/r", 80, "cfg").unwrap();
    assert!(
        result.is_some(),
        "claim must succeed with correct bind slots"
    );

    // Reclaimable path also exercises the WHERE IN (?10..?13) bind slots.
    update_lease_status(
        &c,
        &result.unwrap().lease_id,
        LeaseStatus::Failed,
        Some("run-old"),
    )
    .unwrap();
    let reclaimed = try_claim(&c, "o/r", 80, "cfg-b").unwrap();
    assert!(
        reclaimed.is_some(),
        "reclaim must succeed with correct bind slots"
    );
}

#[test]
fn conditional_outcome_distinguishes_rejected_from_missing() {
    let c = conn();
    let lease = try_claim(&c, "o/r", 81, "cfg").unwrap().unwrap();
    update_lease_status(
        &c,
        &lease.lease_id,
        LeaseStatus::Failed,
        Some("run-current"),
    )
    .unwrap();

    let rejected = update_lease_status_conditional_outcome(
        &c,
        &lease.lease_id,
        LeaseStatus::WaitingExternal,
        &[LeaseStatus::Running],
        None,
        Some("run-stale"),
    )
    .unwrap();
    assert_eq!(
        rejected,
        ConditionalLeaseStatusOutcome::Rejected {
            current_status: LeaseStatus::Failed,
            current_run_id: Some("run-current".to_string()),
        }
    );

    let missing = update_lease_status_conditional_outcome(
        &c,
        "missing-lease",
        LeaseStatus::WaitingExternal,
        &[LeaseStatus::Running],
        None,
        Some("run-stale"),
    )
    .unwrap();
    assert_eq!(missing, ConditionalLeaseStatusOutcome::Missing);
}
