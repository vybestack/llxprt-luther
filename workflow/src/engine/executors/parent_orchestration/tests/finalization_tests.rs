//! CleanupAbandoned protection and stale child finalization regression tests.
//!
//! These tests guard issue 137: a protected `CleanupAbandoned` lease must
//! survive child finalization (never becoming reclaimable `Failed`), and a
//! stale finalization decision against a lease the orchestrator no longer owns
//! must emit zero side effects.

use super::super::*;
use super::support::*;

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
    //
    // Blocker 4: the returned step outcome must be derived from the durable
    // lease state (Completed → Success), never from the stale process result
    // (which was CompletedFailure → Fixable). The concurrent writer that won
    // the CAS is authoritative, so the orchestrator observes the child as
    // successfully completed even though this in-process result was a failure.
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
    assert_eq!(outcome, StepOutcome::Success);

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
    //
    // Blocker 4: the returned step outcome must be derived from the durable
    // lease state. The lease is Running (held by a foreign owner), so the
    // outcome is Wait — a concurrent writer is actively driving the child.
    // It must NOT be Fixable (the stale process result's classification).
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
    assert_eq!(outcome, StepOutcome::Wait);

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
    assert_eq!(
        query.take(),
        vec![
            RecordedGithubOperation::RemoveLabel {
                number: child,
                label: state.luther_label.clone(),
            },
            RecordedGithubOperation::CommentIssue {
                number: state.parent_issue_number,
                body: format!(
                    "Parent orchestration is paused because child issue #{child} reached a terminal failed workflow state."
                ),
            },
        ]
    );

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
