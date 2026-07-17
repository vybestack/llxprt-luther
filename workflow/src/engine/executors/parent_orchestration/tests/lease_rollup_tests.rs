//! Child lease, rollup persistence, and auto-merge recording tests.

use super::super::*;
use super::support::*;

#[test]
fn failed_child_lease_relaunches_fresh_workflow() {
    let temp = tempfile::tempdir().unwrap();
    let mut context = context(temp.path());
    context.set("current_step_id", "launch_or_resume_child_workflow");
    let state = OrchestrationState::from_context(&context, &json!({})).unwrap();
    let db_path = temp.path().join("checkpoints.db");
    crate::persistence::init_database(&db_path).unwrap();
    let mut conn = open_parent_orchestration_connection(&db_path).unwrap();
    let child = unique_child_issue_number();
    let lease = try_claim(&conn, &state.repo, child, &state.child_config_id)
        .unwrap()
        .unwrap();
    update_lease_status(
        &conn,
        &lease.lease_id,
        LeaseStatus::Failed,
        Some("old-child-run"),
    )
    .unwrap();

    let action = prepare_child_lease_with_conn(&state, child, &mut conn).unwrap();

    match action {
        ChildLeaseAction::Launch(lease) => {
            assert_eq!(lease.status, LeaseStatus::Claimed);
            assert_eq!(lease.run_id, None);
        }
        _ => panic!("failed child lease should launch fresh workflow"),
    }
}

#[test]
fn terminal_lease_relaunch_is_atomic_across_two_connections() {
    // Two independent database connections both observe the *same* terminal
    // (Failed) child lease and simultaneously try to relaunch it. The atomic
    // compare-and-swap must guarantee exactly one connection wins Launch and
    // the other observes the contention and waits, so no duplicate child
    // workflow is spawned.
    let temp = tempfile::tempdir().unwrap();
    let mut context = context(temp.path());
    context.set("current_step_id", "launch_or_resume_child_workflow");
    let state = OrchestrationState::from_context(&context, &json!({})).unwrap();
    let db_path = temp.path().join("checkpoints.db");
    crate::persistence::init_database(&db_path).unwrap();

    let child = unique_child_issue_number();
    let setup_conn = open_parent_orchestration_connection(&db_path).unwrap();
    let lease = try_claim(&setup_conn, &state.repo, child, &state.child_config_id)
        .unwrap()
        .unwrap();
    update_lease_status(
        &setup_conn,
        &lease.lease_id,
        LeaseStatus::Failed,
        Some("old-child-run"),
    )
    .unwrap();
    drop(setup_conn);

    // Both connections read the identical terminal lease snapshot before
    // either mutates it, mirroring two orchestrator passes racing on the same
    // row.
    let mut conn_a = open_parent_orchestration_connection(&db_path).unwrap();
    let mut conn_b = open_parent_orchestration_connection(&db_path).unwrap();
    let observed_a = get_lease_for_issue(&conn_a, &state.repo, child)
        .unwrap()
        .unwrap();
    let observed_b = get_lease_for_issue(&conn_b, &state.repo, child)
        .unwrap()
        .unwrap();
    assert_eq!(observed_a.status, LeaseStatus::Failed);
    assert_eq!(observed_b.status, LeaseStatus::Failed);

    let action_a = prepare_relaunchable_child(&mut conn_a, &observed_a).unwrap();
    let action_b = prepare_relaunchable_child(&mut conn_b, &observed_b).unwrap();

    let launches = [&action_a, &action_b]
        .iter()
        .filter(|action| matches!(action, ChildLeaseAction::Launch(_)))
        .count();
    let contended_waits = [&action_a, &action_b]
        .iter()
        .filter(|action| {
            matches!(
                action,
                ChildLeaseAction::Wait { reason, .. }
                    if reason == "child_lease_relaunch_contended"
            )
        })
        .count();

    assert_eq!(
        launches, 1,
        "exactly one connection must win the relaunch claim"
    );
    assert_eq!(
        contended_waits, 1,
        "the losing connection must wait on contention, not relaunch"
    );

    // The lease ends in a single fresh Claimed state with no run id, proving the
    // winner's compare-and-swap took effect and the loser did not overwrite it.
    let verify_conn = open_parent_orchestration_connection(&db_path).unwrap();
    let final_lease = get_lease_for_issue(&verify_conn, &state.repo, child)
        .unwrap()
        .unwrap();
    assert_eq!(final_lease.status, LeaseStatus::Claimed);
    assert_eq!(final_lease.run_id, None);
}

#[test]
fn child_lease_claim_contention_waits_without_error() {
    let temp = tempfile::tempdir().unwrap();
    let mut context = context(temp.path());
    context.set("current_step_id", "launch_or_resume_child_workflow");
    let state = OrchestrationState::from_context(&context, &json!({})).unwrap();
    let db_path = temp.path().join("checkpoints.db");
    crate::persistence::init_database(&db_path).unwrap();
    let conn = open_parent_orchestration_connection(&db_path).unwrap();

    let action = claim_child_lease(&state, unique_child_issue_number(), &conn).unwrap();

    match action {
        ChildLeaseAction::Launch(lease) => {
            let contended = claim_child_lease(&state, lease.issue_number, &conn).unwrap();
            match contended {
                ChildLeaseAction::Wait { lease, reason } => {
                    assert!(lease.is_none());
                    assert_eq!(reason, "child_lease_claim_contended");
                }
                _ => panic!("lost child lease claim should wait"),
            }
        }
        _ => panic!("first child lease claim should launch"),
    }
}

#[test]
fn auto_merge_is_gated_on_green_checks_and_review_state() {
    assert_eq!(auto_merge_block_reason(&ready_pr(17)), None);
    assert_eq!(
        auto_merge_block_reason(&pr_with_checks(17, Some("pending"), None)),
        Some("checks_not_passed")
    );
    assert_eq!(
        auto_merge_block_reason(&pr_with_checks(
            17,
            Some("passed"),
            Some("changes_requested")
        )),
        Some("changes_requested")
    );
    assert_eq!(
        auto_merge_block_reason(&pr_with_checks(17, Some("passed"), Some("review_required"))),
        Some("review_required")
    );
}

#[test]
fn auto_merge_block_reason_flags_unpassed_checks_and_reviews() {
    assert_eq!(
        auto_merge_block_reason(&pr_with_checks(1, Some("pending"), Some("approved"))),
        Some("checks_not_passed")
    );
    assert_eq!(
        auto_merge_block_reason(&pr_with_checks(
            1,
            Some("passed"),
            Some("changes_requested")
        )),
        Some("changes_requested")
    );
    assert_eq!(
        auto_merge_block_reason(&pr_with_checks(1, Some("passed"), Some("review_required"))),
        Some("review_required")
    );
    assert_eq!(
        auto_merge_block_reason(&pr_with_checks(1, Some("passed"), Some("approved"))),
        None
    );
}

#[test]
fn active_child_lease_blocks_parent_for_active_statuses() {
    use crate::persistence::leases::{IssueLease, LeaseStatus};
    fn lease(status: LeaseStatus) -> IssueLease {
        IssueLease {
            lease_id: "l".to_string(),
            issue_repo: "o/r".to_string(),
            issue_number: 1,
            config_id: "cfg".to_string(),
            run_id: Some("run".to_string()),
            status,
            claimed_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            heartbeat_at: chrono::Utc::now(),
        }
    }
    assert!(active_child_lease_blocks_parent(&lease(
        LeaseStatus::Running
    )));
    assert!(active_child_lease_blocks_parent(&lease(
        LeaseStatus::Claimed
    )));
    assert!(active_child_lease_blocks_parent(&lease(
        LeaseStatus::WaitingExternal
    )));
    assert!(active_child_lease_blocks_parent(&lease(
        LeaseStatus::ReadyToResume
    )));
    assert!(!active_child_lease_blocks_parent(&lease(
        LeaseStatus::Completed
    )));
    assert!(!active_child_lease_blocks_parent(&lease(
        LeaseStatus::Failed
    )));
}

#[test]
fn read_rollup_defaults_when_missing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let rollup = read_rollup(temp.path()).expect("read rollup");
    assert_eq!(rollup.parent_issue_number, 0);
    assert!(rollup.children.is_empty());
}

#[test]
fn update_rollup_persists_and_replaces_child_entries() {
    let temp = tempfile::tempdir().expect("tempdir");
    let artifact_dir = temp.path().join("children");
    let state = rollup_state(temp.path().to_path_buf(), Some(artifact_dir));

    let pr = merged_pr(7);
    update_rollup(&state, 5, Some("run-5"), "merged", Some(&pr)).expect("update rollup");

    let rollup = read_rollup(temp.path()).expect("read rollup");
    assert_eq!(rollup.parent_issue_number, 100);
    assert_eq!(rollup.children.len(), 1);
    let entry = &rollup.children[0];
    assert_eq!(entry.child_issue_number, 5);
    assert_eq!(entry.child_run_id.as_deref(), Some("run-5"));
    assert_eq!(entry.pr_number, Some(7));
    assert_eq!(entry.outcome.as_deref(), Some("merged"));
    assert!(entry.child_artifact_dir.is_some());

    // Re-updating the same child replaces (does not duplicate) its entry.
    update_rollup(&state, 5, Some("run-5b"), "non_actionable_child", None).expect("second update");
    let rollup = read_rollup(temp.path()).expect("read rollup again");
    assert_eq!(rollup.children.len(), 1);
    assert_eq!(rollup.children[0].child_run_id.as_deref(), Some("run-5b"));
    assert_eq!(
        rollup.children[0].non_actionable_reason.as_deref(),
        Some("child issue is explicitly non-actionable")
    );
}

#[test]
fn rollup_has_outcome_matches_recorded_outcome() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state = rollup_state(temp.path().to_path_buf(), None);
    update_rollup(&state, 9, None, "blocked", None).expect("update rollup");

    assert!(rollup_has_outcome(&state, 9, "blocked").expect("has outcome"));
    assert!(!rollup_has_outcome(&state, 9, "merged").expect("no outcome"));
    assert!(!rollup_has_outcome(&state, 10, "blocked").expect("other child"));
}

#[test]
fn write_launch_artifact_writes_child_run_launch_json() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state = rollup_state(temp.path().to_path_buf(), None);
    write_launch_artifact(&state, serde_json::json!({"launched": true}))
        .expect("write launch artifact");
    let path = temp.path().join("child-run-launch.json");
    assert!(path.exists());
    let contents = std::fs::read_to_string(&path).expect("read artifact");
    assert!(contents.contains("launched"));
}

#[test]
fn record_blocked_child_writes_artifact_and_rollup() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state = rollup_state(temp.path().to_path_buf(), None);
    let query = MockQuery {
        issue: None,
        children: Vec::new(),
        pr: None,
    };
    let pr = ready_pr(11);
    let outcome =
        record_blocked_child(&state, &query, 11, Some(&pr), "blocked_reason").expect("blocked");
    assert!(matches!(outcome, StepOutcome::Fixable));

    // The blocking wait artifact is written.
    let wait_path = temp.path().join("child-merge-wait.json");
    assert!(wait_path.exists());
    let contents = std::fs::read_to_string(&wait_path).expect("read wait");
    assert!(contents.contains("blocked_reason"));

    // And the rollup records the block outcome for the child.
    assert!(rollup_has_outcome(&state, 11, "blocked_reason").expect("rollup outcome"));
}

#[test]
fn record_superseded_child_comments_and_blocks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state = rollup_state(temp.path().to_path_buf(), None);
    let query = MockQuery {
        issue: None,
        children: Vec::new(),
        pr: None,
    };
    let pr = ready_pr(12);
    let outcome = record_superseded_child(&state, &query, 12, Some(&pr)).expect("superseded");
    assert!(matches!(outcome, StepOutcome::Fixable));
    assert!(rollup_has_outcome(&state, 12, "superseded_child_pr").expect("rollup outcome"));
}

#[test]
fn attempt_auto_merge_disabled_returns_disabled_reason() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut state = rollup_state(temp.path().to_path_buf(), None);
    state.auto_merge_children = false;
    let query = MockQuery {
        issue: None,
        children: Vec::new(),
        pr: None,
    };
    let result = attempt_auto_merge_if_enabled(&state, &query, Some(&ready_pr(1)));
    assert_eq!(result["attempted"], serde_json::json!(false));
    assert_eq!(result["reason"], serde_json::json!("disabled"));
}

#[test]
fn attempt_auto_merge_enabled_without_pr_reports_missing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut state = rollup_state(temp.path().to_path_buf(), None);
    state.auto_merge_children = true;
    let query = MockQuery {
        issue: None,
        children: Vec::new(),
        pr: None,
    };
    let result = attempt_auto_merge_if_enabled(&state, &query, None);
    assert_eq!(result["reason"], serde_json::json!("missing_pr"));
}

#[test]
fn attempt_auto_merge_blocked_by_failing_checks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut state = rollup_state(temp.path().to_path_buf(), None);
    state.auto_merge_children = true;
    let query = MockQuery {
        issue: None,
        children: Vec::new(),
        pr: None,
    };
    let pr = pr_with_checks(2, Some("pending"), Some("approved"));
    let result = attempt_auto_merge_if_enabled(&state, &query, Some(&pr));
    assert_eq!(result["attempted"], serde_json::json!(false));
    assert_eq!(result["reason"], serde_json::json!("checks_not_passed"));
    assert_eq!(
        result["fallback"],
        serde_json::json!("wait_for_human_merge")
    );
}

#[test]
fn attempt_auto_merge_enabled_succeeds_when_ready() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut state = rollup_state(temp.path().to_path_buf(), None);
    state.auto_merge_children = true;
    let query = MockQuery {
        issue: None,
        children: Vec::new(),
        pr: None,
    };
    let result = attempt_auto_merge_if_enabled(&state, &query, Some(&ready_pr(3)));
    assert_eq!(result["attempted"], serde_json::json!(true));
    assert_eq!(result["enabled"], serde_json::json!(true));
    assert_eq!(result["pr_number"], serde_json::json!(3));
}

// ---------------------------------------------------------------------------
// CleanupAbandoned protection regression tests (issue 137)
// ---------------------------------------------------------------------------

/// Build an `OrchestrationState`, initialized database connection, and a
/// freshly claimed lease for a unique child issue. Returns the state, the
/// connection, and the lease so each test can set the lease to the scenario
/// status before calling `finish_child_launch`.
fn lease_finalization_harness() -> (
    OrchestrationState,
    rusqlite::Connection,
    crate::persistence::leases::IssueLease,
) {
    let temp = tempfile::tempdir().unwrap();
    let mut context = context(temp.path());
    context.set("current_step_id", "launch_or_resume_child_workflow");
    let state = OrchestrationState::from_context(&context, &json!({})).unwrap();
    let db_path = temp.path().join("checkpoints.db");
    crate::persistence::init_database(&db_path).unwrap();
    let conn = open_parent_orchestration_connection(&db_path).unwrap();
    let child = unique_child_issue_number();
    let lease = try_claim(&conn, &state.repo, child, &state.child_config_id)
        .unwrap()
        .unwrap();
    // Keep the temp dir alive for the duration of the test by leaking it;
    // these tests create no meaningful artifacts and the OS reclaims the space.
    std::mem::forget(temp);
    (state, conn, lease)
}

/// Construct a `ChildWorkflowLaunchRequest` for a failed child run, keyed on
/// the given lease and run id.
fn child_request(
    child: u64,
    lease: &crate::persistence::leases::IssueLease,
    run_id: &str,
) -> ChildWorkflowLaunchRequest {
    ChildWorkflowLaunchRequest {
        workflow_type_id: "wf".to_string(),
        config_id: "cfg".to_string(),
        run_id: run_id.to_string(),
        repo: lease.issue_repo.clone(),
        issue_number: child,
        work_dir: None,
        artifact_dir: None,
        config_root: PathBuf::from("/config"),
    }
}

#[test]
fn finish_child_launch_preserves_cleanup_abandoned_lease_on_failure() {
    // Issue 137 regression: when the engine runner has protected the lease as
    // CleanupAbandoned during failure cleanup, finish_child_launch must NOT
    // overwrite it with reclaimable Failed. The conditional lease update
    // excludes CleanupAbandoned from the expected-status set, so the protected
    // state survives and a duplicate relaunch is prevented.
    let (state, conn, lease) = lease_finalization_harness();
    let child = lease.issue_number;
    let run_id = "child-run-protected";
    // Simulate the engine runner protecting the lease during failure cleanup.
    update_lease_status(
        &conn,
        &lease.lease_id,
        LeaseStatus::CleanupAbandoned,
        Some(run_id),
    )
    .unwrap();
    let lease_snapshot = get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .unwrap();
    let mut context = StepContext::new(state.artifact_root.join("work"), "run-parent".to_string());
    let query = MockQuery {
        issue: None,
        children: Vec::new(),
        pr: None,
    };
    let request = child_request(child, &lease_snapshot, run_id);
    let completion = ChildLaunchCompletion {
        child,
        lease: &lease_snapshot,
        request: &request,
        result: ChildWorkflowRunResult::CompletedFailure,
        run_status: Some(RunStatus::Failed),
        pr: None,
    };

    let outcome = finish_child_launch(&state, &mut context, &query, &conn, completion).unwrap();

    assert!(matches!(outcome, StepOutcome::Fixable));
    let final_lease = get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .unwrap();
    assert_eq!(
        final_lease.status,
        LeaseStatus::CleanupAbandoned,
        "CleanupAbandoned must survive child finalization; a reclaimable Failed \
         would allow a duplicate relaunch while cleanup is owned"
    );
    assert_eq!(
        final_lease.run_id.as_deref(),
        Some(run_id),
        "the owned run id must be preserved"
    );
}

#[test]
fn finish_child_launch_transitions_running_lease_to_failed_on_failure() {
    // Regression: the normal failure path (lease is Running) must still
    // transition to Failed so the issue becomes reclaimable.
    let (state, conn, lease) = lease_finalization_harness();
    let child = lease.issue_number;
    let run_id = "child-run-normal";
    update_lease_status(&conn, &lease.lease_id, LeaseStatus::Running, Some(run_id)).unwrap();
    let lease_snapshot = get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .unwrap();
    let mut context = StepContext::new(state.artifact_root.join("work"), "run-parent".to_string());
    let query = MockQuery {
        issue: None,
        children: Vec::new(),
        pr: None,
    };
    let request = child_request(child, &lease_snapshot, run_id);
    let completion = ChildLaunchCompletion {
        child,
        lease: &lease_snapshot,
        request: &request,
        result: ChildWorkflowRunResult::CompletedFailure,
        run_status: Some(RunStatus::Failed),
        pr: None,
    };

    let outcome = finish_child_launch(&state, &mut context, &query, &conn, completion).unwrap();

    assert!(matches!(outcome, StepOutcome::Fixable));
    let final_lease = get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .unwrap();
    assert_eq!(
        final_lease.status,
        LeaseStatus::Failed,
        "a Running lease must transition to Failed on a failed child run"
    );
}

#[test]
fn finish_child_launch_transitions_running_lease_to_ready_to_resume_on_success() {
    let (state, conn, lease) = lease_finalization_harness();
    let child = lease.issue_number;
    let run_id = "child-run-success";
    update_lease_status(&conn, &lease.lease_id, LeaseStatus::Running, Some(run_id)).unwrap();
    let lease_snapshot = get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .unwrap();
    let request = child_request(child, &lease_snapshot, run_id);
    let completion = ChildLaunchCompletion {
        child,
        lease: &lease_snapshot,
        request: &request,
        result: ChildWorkflowRunResult::CompletedSuccess,
        run_status: Some(RunStatus::Completed),
        pr: None,
    };
    let mut context = StepContext::new(state.artifact_root.join("work"), "run-parent".to_string());
    let query = MockQuery {
        issue: None,
        children: Vec::new(),
        pr: None,
    };

    let outcome = finish_child_launch(&state, &mut context, &query, &conn, completion).unwrap();

    assert!(matches!(outcome, StepOutcome::Success));
    let final_lease = get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .unwrap();
    assert_eq!(
        final_lease.status,
        LeaseStatus::ReadyToResume,
        "a Running lease must transition to ReadyToResume on a successful child run"
    );
}

#[test]
fn finish_child_launch_preserves_terminal_completed_lease() {
    // A lease already advanced to Completed by a concurrent writer must not be
    // overwritten by a stale child finalization decision.
    let (state, conn, lease) = lease_finalization_harness();
    let child = lease.issue_number;
    let run_id = "child-run-stale";
    update_lease_status(&conn, &lease.lease_id, LeaseStatus::Completed, Some(run_id)).unwrap();
    let lease_snapshot = get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .unwrap();
    let mut context = StepContext::new(state.artifact_root.join("work"), "run-parent".to_string());
    let query = MockQuery {
        issue: None,
        children: Vec::new(),
        pr: None,
    };
    let request = child_request(child, &lease_snapshot, run_id);
    let completion = ChildLaunchCompletion {
        child,
        lease: &lease_snapshot,
        request: &request,
        result: ChildWorkflowRunResult::CompletedFailure,
        run_status: Some(RunStatus::Failed),
        pr: None,
    };

    let _ = finish_child_launch(&state, &mut context, &query, &conn, completion).unwrap();

    let final_lease = get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .unwrap();
    assert_eq!(
        final_lease.status,
        LeaseStatus::Completed,
        "a Completed lease must not be overwritten by a stale failure classification"
    );
}

// ---------------------------------------------------------------------------
// Stale child finalization side-effect suppression (issue 137)
// ---------------------------------------------------------------------------

/// Assert that no finalization side effects were emitted for a stale child
/// launch: no child-run-launch artifact, no rollup entry, no wait artifact,
/// no terminal-state artifact, no context mutation, no label removal, and no
/// comments.
fn assert_zero_finalization_side_effects(
    state: &OrchestrationState,
    context: &StepContext,
    query: &RecordingMockQuery,
    child: u64,
) {
    // No GitHub side-effecting operations (label removal, comments).
    let ops = query.take();
    assert!(
        ops.is_empty(),
        "stale finalization must not emit GitHub side effects, got: {ops:?}"
    );

    // No launch artifact written.
    let launch_path = state.artifact_root.join("child-run-launch.json");
    assert!(
        !launch_path.exists(),
        "stale finalization must not write the launch artifact"
    );

    // No rollup entry for the child.
    let rollup = read_rollup(&state.artifact_root).expect("read rollup");
    assert!(
        !rollup
            .children
            .iter()
            .any(|entry| entry.child_issue_number == child),
        "stale finalization must not record a rollup entry"
    );

    // No wait-state artifact.
    let wait_path = state.artifact_root.join("child-merge-wait.json");
    assert!(
        !wait_path.exists(),
        "stale finalization must not write a wait artifact"
    );

    // No terminal-state artifact (only emitted on CompletedFailure).
    let terminal_path = state.artifact_root.join("child-terminal-state.json");
    assert!(
        !terminal_path.exists(),
        "stale finalization must not write a terminal-state artifact"
    );

    // Context must not be mutated with child_run_id / child_pr_number.
    assert!(
        context.get("child_run_id").is_none(),
        "stale finalization must not set child_run_id in the context"
    );
    assert!(
        context.get("child_pr_number").is_none(),
        "stale finalization must not set child_pr_number in the context"
    );
}

#[test]
fn stale_finalization_on_completed_lease_has_zero_side_effects() {
    // Issue 137: a lease already advanced to Completed by a concurrent writer
    // must not be overwritten by a stale child finalization decision, AND
    // finish_child_launch must emit zero side effects (no artifacts, no
    // context mutation, no rollup, no label removal, no comments).
    let (state, conn, lease) = lease_finalization_harness();
    let child = lease.issue_number;
    let run_id = "child-run-completed";
    update_lease_status(&conn, &lease.lease_id, LeaseStatus::Completed, Some(run_id)).unwrap();
    let lease_snapshot = get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .unwrap();
    let mut context = StepContext::new(state.artifact_root.join("work"), "run-parent".to_string());
    let query = RecordingMockQuery::new();
    let request = child_request(child, &lease_snapshot, run_id);
    let completion = ChildLaunchCompletion {
        child,
        lease: &lease_snapshot,
        request: &request,
        result: ChildWorkflowRunResult::CompletedFailure,
        run_status: Some(RunStatus::Failed),
        pr: None,
    };

    let outcome = finish_child_launch(&state, &mut context, &query, &conn, completion).unwrap();
    assert_eq!(outcome, StepOutcome::Fixable);

    assert_zero_finalization_side_effects(&state, &context, &query, child);

    let final_lease = get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .unwrap();
    assert_eq!(
        final_lease.status,
        LeaseStatus::Completed,
        "a Completed lease must not be overwritten by a stale failure classification"
    );
    assert_eq!(
        final_lease.run_id.as_deref(),
        Some(run_id),
        "the completed lease's run id must be preserved"
    );
}

#[test]
fn stale_finalization_on_reassigned_owner_has_zero_side_effects() {
    // Issue 137: when the lease's run id has been reassigned to a foreign owner
    // by a concurrent writer, finish_child_launch must treat the conditional
    // update as rejected and emit zero side effects. Only an exact same-owner
    // idempotent match may apply side effects.
    let (state, conn, lease) = lease_finalization_harness();
    let child = lease.issue_number;
    let run_id = "child-run-original";
    // The lease is Running (a valid source status for the failure transition)
    // but the run id now belongs to a foreign owner.
    update_lease_status(
        &conn,
        &lease.lease_id,
        LeaseStatus::Running,
        Some("foreign-run"),
    )
    .unwrap();
    let lease_snapshot = get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .unwrap();
    let mut context = StepContext::new(state.artifact_root.join("work"), "run-parent".to_string());
    let query = RecordingMockQuery::new();
    let request = child_request(child, &lease_snapshot, run_id);
    let completion = ChildLaunchCompletion {
        child,
        lease: &lease_snapshot,
        request: &request,
        result: ChildWorkflowRunResult::CompletedFailure,
        run_status: Some(RunStatus::Failed),
        pr: None,
    };

    let outcome = finish_child_launch(&state, &mut context, &query, &conn, completion).unwrap();
    assert_eq!(outcome, StepOutcome::Fixable);

    assert_zero_finalization_side_effects(&state, &context, &query, child);

    let final_lease = get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .unwrap();
    assert_eq!(
        final_lease.status,
        LeaseStatus::Running,
        "a foreign-owned Running lease must not be transitioned by a stale owner"
    );
    assert_eq!(
        final_lease.run_id.as_deref(),
        Some("foreign-run"),
        "the foreign owner's run id must be preserved"
    );
}

#[test]
fn stale_finalization_on_missing_lease_has_zero_side_effects() {
    // Issue 137: if the lease row has been deleted (Missing), finish_child_launch
    // must emit zero side effects rather than fabricating artifacts and comments
    // for a lease that no longer exists.
    let (state, conn, lease) = lease_finalization_harness();
    let child = lease.issue_number;
    let run_id = "child-run-missing";
    // Delete the lease row to simulate a concurrent cleanup that removed it.
    conn.execute(
        "DELETE FROM issue_leases WHERE lease_id = ?1",
        rusqlite::params![lease.lease_id],
    )
    .unwrap();
    assert!(get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .is_none());
    // The completion references the now-deleted lease snapshot.
    let lease_snapshot = lease;
    let mut context = StepContext::new(state.artifact_root.join("work"), "run-parent".to_string());
    let query = RecordingMockQuery::new();
    let request = child_request(child, &lease_snapshot, run_id);
    let completion = ChildLaunchCompletion {
        child,
        lease: &lease_snapshot,
        request: &request,
        result: ChildWorkflowRunResult::CompletedFailure,
        run_status: Some(RunStatus::Failed),
        pr: None,
    };

    let outcome = finish_child_launch(&state, &mut context, &query, &conn, completion).unwrap();
    assert_eq!(outcome, StepOutcome::Fixable);

    assert_zero_finalization_side_effects(&state, &context, &query, child);

    // The lease remains absent.
    assert!(
        get_lease_for_issue(&conn, &state.repo, child)
            .unwrap()
            .is_none(),
        "the missing lease must remain absent"
    );
}

#[test]
fn stale_finalization_on_cleanup_abandoned_has_zero_side_effects() {
    // Issue 137: a protected CleanupAbandoned lease (even same-owner) must not
    // be reclaimed or receive side effects during finalization. CleanupAbandoned
    // requires explicit continuation and must never become reclaimable here.
    let (state, conn, lease) = lease_finalization_harness();
    let child = lease.issue_number;
    let run_id = "child-run-protected";
    update_lease_status(
        &conn,
        &lease.lease_id,
        LeaseStatus::CleanupAbandoned,
        Some(run_id),
    )
    .unwrap();
    let lease_snapshot = get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .unwrap();
    let mut context = StepContext::new(state.artifact_root.join("work"), "run-parent".to_string());
    let query = RecordingMockQuery::new();
    let request = child_request(child, &lease_snapshot, run_id);
    let completion = ChildLaunchCompletion {
        child,
        lease: &lease_snapshot,
        request: &request,
        result: ChildWorkflowRunResult::CompletedFailure,
        run_status: Some(RunStatus::Failed),
        pr: None,
    };

    let outcome = finish_child_launch(&state, &mut context, &query, &conn, completion).unwrap();
    assert_eq!(outcome, StepOutcome::Fixable);

    assert_zero_finalization_side_effects(&state, &context, &query, child);

    let final_lease = get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .unwrap();
    assert_eq!(
        final_lease.status,
        LeaseStatus::CleanupAbandoned,
        "CleanupAbandoned must survive child finalization and never become reclaimable"
    );
    assert_eq!(
        final_lease.run_id.as_deref(),
        Some(run_id),
        "the owned run id must be preserved"
    );
}

#[test]
fn idempotent_same_owner_failure_allows_side_effects() {
    // Issue 137: the only rejection case that may apply side effects is an
    // exact same-owner idempotent match — the lease already holds the terminal
    // result this finalization would have produced (Failed), owned by the very
    // same run id. In that case the launch artifact and rollup entry are
    // permitted (the failure path's label removal and comment still apply).
    let (state, conn, lease) = lease_finalization_harness();
    let child = lease.issue_number;
    let run_id = "child-run-idempotent";
    // The lease already reached Failed with the same run id (idempotent match).
    update_lease_status(&conn, &lease.lease_id, LeaseStatus::Failed, Some(run_id)).unwrap();
    let lease_snapshot = get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .unwrap();
    let mut context = StepContext::new(state.artifact_root.join("work"), "run-parent".to_string());
    let query = RecordingMockQuery::new();
    let request = child_request(child, &lease_snapshot, run_id);
    let completion = ChildLaunchCompletion {
        child,
        lease: &lease_snapshot,
        request: &request,
        result: ChildWorkflowRunResult::CompletedFailure,
        run_status: Some(RunStatus::Failed),
        pr: None,
    };

    let outcome = finish_child_launch(&state, &mut context, &query, &conn, completion).unwrap();
    assert_eq!(outcome, StepOutcome::Fixable);

    // Side effects ARE applied on an idempotent same-owner match.
    let launch_path = state.artifact_root.join("child-run-launch.json");
    assert!(
        launch_path.exists(),
        "idempotent match must write the launch artifact"
    );
    let rollup = read_rollup(&state.artifact_root).expect("read rollup");
    assert!(
        rollup
            .children
            .iter()
            .any(|entry| entry.child_issue_number == child),
        "idempotent match must record a rollup entry"
    );

    let final_lease = get_lease_for_issue(&conn, &state.repo, child)
        .unwrap()
        .unwrap();
    assert_eq!(
        final_lease.status,
        LeaseStatus::Failed,
        "the idempotent Failed lease must be preserved"
    );
    assert_eq!(
        final_lease.run_id.as_deref(),
        Some(run_id),
        "the idempotent run id must be preserved"
    );
}

#[test]
fn cleanup_abandoned_lease_requires_explicit_continuation() {
    // Issue 137: a protected CleanupAbandoned lease must be treated as
    // non-actionable by the parent orchestrator. prepare_child_lease_with_conn
    // must return Wait so no duplicate child workflow is launched until an
    // explicit continuation resolves the abandonment.
    let temp = tempfile::tempdir().unwrap();
    let mut context = context(temp.path());
    context.set("current_step_id", "launch_or_resume_child_workflow");
    let state = OrchestrationState::from_context(&context, &json!({})).unwrap();
    let db_path = temp.path().join("checkpoints.db");
    crate::persistence::init_database(&db_path).unwrap();
    let mut conn = open_parent_orchestration_connection(&db_path).unwrap();
    let child = unique_child_issue_number();
    let lease = try_claim(&conn, &state.repo, child, &state.child_config_id)
        .unwrap()
        .unwrap();
    update_lease_status(
        &conn,
        &lease.lease_id,
        LeaseStatus::CleanupAbandoned,
        Some("abandoned-run"),
    )
    .unwrap();

    let action = prepare_child_lease_with_conn(&state, child, &mut conn).unwrap();

    match action {
        ChildLeaseAction::Wait { lease, reason } => {
            assert!(
                lease.is_some(),
                "the protected lease must be returned so the caller can inspect it"
            );
            assert_eq!(
                reason, "cleanup_abandoned_requires_continuation",
                "CleanupAbandoned must not be reclaimable; it requires explicit continuation"
            );
            let returned = lease.unwrap();
            assert_eq!(returned.status, LeaseStatus::CleanupAbandoned);
        }
        ChildLeaseAction::Launch(_) | ChildLeaseAction::Resume(_) => {
            panic!("CleanupAbandoned lease must wait, not launch or resume");
        }
    }
}

// ---------------------------------------------------------------------------
// Child workspace isolation tests (issue 137)
// ---------------------------------------------------------------------------

/// Build an `OrchestrationState` with a work_dir for child workspace isolation
/// tests.
fn workspace_isolation_state() -> OrchestrationState {
    let temp = tempfile::tempdir().unwrap();
    let mut state = rollup_state(temp.path().to_path_buf(), None);
    state.work_dir = Some(temp.path().join("parent-work"));
    std::fs::create_dir_all(state.work_dir.as_ref().unwrap()).unwrap();
    std::mem::forget(temp);
    state
}

#[test]
fn child_request_derives_isolated_work_dir_not_parent() {
    // The child request must derive an isolated workspace under
    // children/issue-N/run-id, not clone the parent's work_dir.
    let state = workspace_isolation_state();
    let parent_work_dir = state.work_dir.clone().unwrap();
    let child = unique_child_issue_number();
    let request = child_request_with_run_id(&state, child, "child-run-1".to_string());

    let child_work = request.work_dir.expect("child work_dir must be set");
    assert_ne!(
        child_work, parent_work_dir,
        "child workspace must not be the parent work_dir"
    );
    assert_eq!(
        child_work,
        child_work_dir(&parent_work_dir, child, "child-run-1"),
        "child work_dir must follow the isolated children/issue-N/run-id layout"
    );
}

#[test]
fn child_relaunches_get_distinct_isolated_work_dirs() {
    // A relaunched child (same issue, new run id) must get a distinct workspace
    // so the prior run's worktree is not overwritten.
    let state = workspace_isolation_state();
    let parent_work_dir = state.work_dir.clone().unwrap();
    let child = unique_child_issue_number();

    let first = child_request_with_run_id(&state, child, "child-run-1".to_string());
    let second = child_request_with_run_id(&state, child, "child-run-2".to_string());

    assert_ne!(
        first.work_dir, second.work_dir,
        "relaunched children must get distinct workspaces"
    );
    // Both are under the parent work_dir but isolated per run.
    assert!(first
        .work_dir
        .as_ref()
        .unwrap()
        .starts_with(&parent_work_dir));
    assert!(second
        .work_dir
        .as_ref()
        .unwrap()
        .starts_with(&parent_work_dir));
}

#[test]
fn sibling_children_get_distinct_isolated_work_dirs() {
    // Two different child issues must get distinct workspaces even with the
    // same run id, proving sibling isolation.
    let state = workspace_isolation_state();
    let parent_work_dir = state.work_dir.clone().unwrap();
    let child_a = unique_child_issue_number();
    let child_b = unique_child_issue_number();

    let req_a = child_request_with_run_id(&state, child_a, "shared-run".to_string());
    let req_b = child_request_with_run_id(&state, child_b, "shared-run".to_string());

    assert_ne!(
        req_a.work_dir, req_b.work_dir,
        "sibling children must get distinct workspaces"
    );
    assert!(req_a
        .work_dir
        .as_ref()
        .unwrap()
        .starts_with(&parent_work_dir));
    assert!(req_b
        .work_dir
        .as_ref()
        .unwrap()
        .starts_with(&parent_work_dir));
}

#[test]
fn child_work_dir_layout_is_deterministic() {
    let base = Path::new("/tmp/luther-parent");
    let path = child_work_dir(base, 42, "run-xyz");
    assert_eq!(path, base.join("children").join("issue-42").join("run-xyz"));
}
