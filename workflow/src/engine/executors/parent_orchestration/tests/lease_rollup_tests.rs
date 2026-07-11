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
