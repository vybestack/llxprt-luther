use super::*;

// ---- prepare_resume_lease transaction-safety tests (issue-137) ----

/// Seed a `ReadyToResume` lease for `issue_number` owned by `run_id`,
/// including a claim receipt, run metadata, checkpoint, and exact workspace
/// ownership.
///
/// Issue 158 resume preparation requires ALL read-only checks to pass before
/// the CAS: metadata must have a resumable status, a non-empty current_step,
/// a resumable checkpoint, workspace ownership, and (for non-legacy rows)
/// launch provenance. This helper seeds the full set so the success-path test
/// can exercise the CAS acquisition.
fn seed_ready_to_resume(
    c: &Connection,
    issue_number: u64,
    run_id: &str,
    workflow_type: &str,
) -> String {
    use crate::persistence::RunStatus;
    let lease = try_claim(c, "o/r", issue_number, "cfg").unwrap().unwrap();
    update_lease_status(c, &lease.lease_id, LeaseStatus::Running, Some(run_id)).unwrap();
    crate::persistence::claim_metadata::upsert_claim_metadata(
        c,
        &crate::persistence::claim_metadata::ClaimMetadataReceipt {
            lease_id: lease.lease_id.clone(),
            assignee: "bot".to_string(),
            label: "claimed".to_string(),
            assignment_added: true,
            label_added: false,
            cleanup_pending: false,
        },
    )
    .unwrap();
    let mut metadata = crate::persistence::RunMetadata::new(run_id, workflow_type, "cfg");
    // Issue 158 slice 6: seed a workspace path with valid bootstrap ownership
    // evidence so the read-only ownership verification in prepare_resume_lease
    // succeeds before the CAS.
    let dir = tempfile::tempdir().expect("create temp workspace");
    let workspace = dir.path().join("ws");
    crate::engine::workspace_ownership::provision_workspace_owner_marker(&workspace, run_id)
        .expect("provision bootstrap ownership");
    metadata.workspace_path = Some(
        workspace
            .to_str()
            .expect("utf-8 workspace path")
            .to_string(),
    );
    // Issue 158 resume preparation: the continuation authorization check
    // requires a resumable status, a non-empty current_step, and a resumable
    // checkpoint at a safe-to-rerun step.
    metadata.status = RunStatus::ReadyToResume;
    metadata.repository = Some("o/r".to_string());
    metadata.issue_number = Some(issue_number as i64);
    metadata.current_step = Some("watch_pr_checks".to_string());
    // Leak the tempdir so the workspace survives for the verification. The
    // test process is short-lived so this is acceptable.
    std::mem::forget(dir);
    crate::persistence::persist_run_with_conn(c, &metadata).unwrap();
    // Seed a resumable checkpoint so select_resume_checkpoint succeeds.
    crate::persistence::checkpoint::save_checkpoint_with_conn(
        c,
        &crate::persistence::checkpoint::Checkpoint::new(run_id, "watch_pr_checks"),
    )
    .unwrap();
    // Also seed the claim_metadata table for init.
    crate::persistence::claim_metadata::init_claim_metadata_table(c).unwrap();
    update_lease_status(c, &lease.lease_id, LeaseStatus::ReadyToResume, Some(run_id)).unwrap();
    lease.lease_id
}

#[test]
fn resume_preparation_acquires_ready_to_resume_lease() {
    let c = conn();
    let lease_id = seed_ready_to_resume(&c, 20, "resume-ok", "wf-type");
    let lease = get_lease_for_issue(&c, "o/r", 20).unwrap().unwrap();
    let prepared = prepare_resume_lease(&lease, &c)
        .unwrap()
        .expect("should acquire");
    assert_eq!(prepared.run_id, "resume-ok");
    assert_eq!(prepared.workflow_type_id, "wf-type");
    assert!(prepared.claim_assignment_added);
    assert!(!prepared.claim_label_added);
    // Verify the prepared data builds a correct ClaimedLaunch.
    let claimed = prepared.into_claimed_launch(&lease);
    assert_eq!(claimed.lease_id, lease_id);
    assert_eq!(claimed.request.run_id, "resume-ok");
    assert!(claimed.request.daemon_managed_claim);
    let updated = get_lease_for_issue(&c, "o/r", 20).unwrap().unwrap();
    assert_eq!(updated.status, LeaseStatus::Running);
}

#[test]
fn resume_preparation_skips_when_run_metadata_missing() {
    // Issue-137: a missing run row must skip the resume *before* the CAS
    // acquisition, leaving the lease in ReadyToResume rather than
    // stranding it in Running without a valid workflow type.
    let c = conn();
    let lease = try_claim(&c, "o/r", 21, "cfg").unwrap().unwrap();
    update_lease_status(
        &c,
        &lease.lease_id,
        LeaseStatus::Running,
        Some("no-metadata"),
    )
    .unwrap();
    crate::persistence::claim_metadata::upsert_claim_metadata(
        &c,
        &crate::persistence::claim_metadata::ClaimMetadataReceipt {
            lease_id: lease.lease_id.clone(),
            assignee: "bot".to_string(),
            label: "claimed".to_string(),
            assignment_added: true,
            label_added: false,
            cleanup_pending: false,
        },
    )
    .unwrap();
    update_lease_status(
        &c,
        &lease.lease_id,
        LeaseStatus::ReadyToResume,
        Some("no-metadata"),
    )
    .unwrap();

    let lease = get_lease_for_issue(&c, "o/r", 21).unwrap().unwrap();
    let result = prepare_resume_lease(&lease, &c).unwrap();
    assert!(result.is_err(), "missing run metadata must skip the resume");

    let durable = get_lease_for_issue(&c, "o/r", 21).unwrap().unwrap();
    assert_eq!(
        durable.status,
        LeaseStatus::ReadyToResume,
        "the lease must remain ReadyToResume, not stranded in Running"
    );
}

#[test]
fn resume_preparation_skips_when_run_metadata_corrupt() {
    // Issue-137: a corrupt run row (unparseable status) must cause
    // workflow_type_id_for_resume to fail, and the error must propagate
    // *before* the CAS acquisition so the lease stays ReadyToResume.
    let c = conn();
    seed_ready_to_resume(&c, 22, "corrupt-meta", "wf-type");
    // Corrupt the run status to an unparseable value so that
    // get_run_with_conn returns a decode error.
    c.execute(
        "UPDATE runs SET status = 'not-a-valid-status' WHERE run_id = 'corrupt-meta'",
        [],
    )
    .unwrap();

    let lease = get_lease_for_issue(&c, "o/r", 22).unwrap().unwrap();
    let result = prepare_resume_lease(&lease, &c);
    assert!(
        result.is_err(),
        "a corrupt run row must propagate a DB decode error"
    );

    let durable = get_lease_for_issue(&c, "o/r", 22).unwrap().unwrap();
    assert_eq!(
        durable.status,
        LeaseStatus::ReadyToResume,
        "the lease must remain ReadyToResume when run metadata is corrupt"
    );
}

#[test]
fn resume_preparation_preserves_ready_to_resume_on_stale_cas() {
    // Issue-137: a stale CAS (the lease was advanced away from
    // ReadyToResume by a concurrent writer) must skip the resume and leave
    // the durable state intact.
    let c = conn();
    seed_ready_to_resume(&c, 23, "stale-cas", "wf-type");
    // Simulate a concurrent writer advancing the lease to Completed before
    // prepare_resume_lease runs its CAS.
    let lease = get_lease_for_issue(&c, "o/r", 23).unwrap().unwrap();
    update_lease_status(
        &c,
        &lease.lease_id,
        LeaseStatus::Completed,
        Some("stale-cas"),
    )
    .unwrap();

    let result = prepare_resume_lease(&lease, &c).unwrap();
    assert!(
        result.is_err(),
        "a stale CAS must skip the resume rather than launch"
    );

    let durable = get_lease_for_issue(&c, "o/r", 23).unwrap().unwrap();
    assert_eq!(
        durable.status,
        LeaseStatus::Completed,
        "the durable Completed state must be preserved by the stale CAS rejection"
    );
}

#[test]
fn resume_preparation_skips_when_claim_receipt_missing() {
    // A lease without a claim-metadata receipt cannot be safely resumed
    // because the claim-ownership flags are unknown. The resume must skip
    // before any acquisition.
    let c = conn();
    let lease = try_claim(&c, "o/r", 24, "cfg").unwrap().unwrap();
    update_lease_status(
        &c,
        &lease.lease_id,
        LeaseStatus::Running,
        Some("no-receipt"),
    )
    .unwrap();
    // Deliberately do NOT write claim_metadata.
    let metadata = crate::persistence::RunMetadata::new("no-receipt", "wf-type", "cfg");
    crate::persistence::persist_run_with_conn(&c, &metadata).unwrap();
    update_lease_status(
        &c,
        &lease.lease_id,
        LeaseStatus::ReadyToResume,
        Some("no-receipt"),
    )
    .unwrap();

    let lease = get_lease_for_issue(&c, "o/r", 24).unwrap().unwrap();
    let result = prepare_resume_lease(&lease, &c).unwrap();
    assert!(
        result.is_err(),
        "a missing claim receipt must skip the resume"
    );

    let durable = get_lease_for_issue(&c, "o/r", 24).unwrap().unwrap();
    assert_eq!(
        durable.status,
        LeaseStatus::ReadyToResume,
        "the lease must remain ReadyToResume when the receipt is missing"
    );
}

#[test]
fn resume_preparation_skips_when_workflow_type_empty() {
    // A run row with an empty workflow_type_id cannot resolve a workflow,
    // so the resume must skip before acquisition.
    let c = conn();
    let lease = try_claim(&c, "o/r", 25, "cfg").unwrap().unwrap();
    update_lease_status(&c, &lease.lease_id, LeaseStatus::Running, Some("empty-wf")).unwrap();
    crate::persistence::claim_metadata::upsert_claim_metadata(
        &c,
        &crate::persistence::claim_metadata::ClaimMetadataReceipt {
            lease_id: lease.lease_id.clone(),
            assignee: "bot".to_string(),
            label: "claimed".to_string(),
            assignment_added: true,
            label_added: false,
            cleanup_pending: false,
        },
    )
    .unwrap();
    let metadata = crate::persistence::RunMetadata::new("empty-wf", "", "cfg");
    crate::persistence::persist_run_with_conn(&c, &metadata).unwrap();
    update_lease_status(
        &c,
        &lease.lease_id,
        LeaseStatus::ReadyToResume,
        Some("empty-wf"),
    )
    .unwrap();

    let lease = get_lease_for_issue(&c, "o/r", 25).unwrap().unwrap();
    let result = prepare_resume_lease(&lease, &c).unwrap();
    assert!(
        result.is_err(),
        "an empty workflow_type_id must skip the resume"
    );

    let durable = get_lease_for_issue(&c, "o/r", 25).unwrap().unwrap();
    assert_eq!(
        durable.status,
        LeaseStatus::ReadyToResume,
        "the lease must remain ReadyToResume when the workflow type is empty"
    );
}

// ---- prepare_resume_lease ownership-denied terminal guard (issue 158 finding 2) ----
//
// An ownership-denied terminal is a distinct non-resumable terminal state. The
// continuation identity guard must reject it *before* the lease CAS, leaving
// the durable ReadyToResume state intact and never stranding the lease in
// Running. This is the read-only continuation identity/authorization completed
// before the daemon/child lease CAS.

/// Seed an ownership-denied terminal run row for `run_id`. The run is marked
/// `Abandoned` with a complete `FailureCleanupState` whose `ownership_denied`
/// is `true` and `cleanup_succeeded` is `false`, matching the engine's
/// `persist_terminal_ownership_failure` path.
fn seed_ownership_denied_terminal(c: &Connection, run_id: &str, workflow_type: &str) {
    use crate::persistence::{FailureCleanupState, RunStatus};
    let mut metadata = crate::persistence::RunMetadata::new(run_id, workflow_type, "cfg");
    metadata.status = RunStatus::Abandoned;
    metadata.failure_cleanup = Some(crate::persistence::FailureCleanupState {
        schema_version: FailureCleanupState::SCHEMA_VERSION,
        failed_step: "remediate".to_string(),
        failure_outcome: "fatal".to_string(),
        failure_reason: "workspace ownership denied before cleanup step".to_string(),
        failed_checkpoint_id: "remediate@2026-01-01T00:00:00Z".to_string(),
        failed_state_snapshot: crate::persistence::checkpoint::StateSnapshot::default(),
        cleanup_step: "abandon_and_log".to_string(),
        cleanup_succeeded: false,
        captured_at: chrono::Utc::now(),
        cleanup_completed_at: None,
        recovery_consumed_at: None,
        ownership_denied: true,
    });
    crate::persistence::persist_run_with_conn(c, &metadata).unwrap();
}

#[test]
fn resume_preparation_skips_ownership_denied_terminal_before_cas() {
    // Issue 158 finding 2: an ownership-denied terminal is non-resumable and
    // must be rejected before the lease CAS, leaving the durable
    // ReadyToResume state intact (never stranded in Running).
    let c = conn();
    seed_ready_to_resume(&c, 40, "ownership-denied", "wf-type");
    seed_ownership_denied_terminal(&c, "ownership-denied", "wf-type");

    let lease = get_lease_for_issue(&c, "o/r", 40).unwrap().unwrap();
    let result = prepare_resume_lease(&lease, &c).unwrap();
    assert!(
        result.is_err(),
        "an ownership-denied terminal must skip the resume"
    );

    let durable = get_lease_for_issue(&c, "o/r", 40).unwrap().unwrap();
    assert_eq!(
        durable.status,
        LeaseStatus::ReadyToResume,
        "the lease must remain ReadyToResume, not stranded in Running"
    );
}

// ---- CompletedSuccess/CompletedFailure exact-owner Running CAS (issue-137) ----

#[test]
fn stale_success_does_not_overwrite_terminal_lease() {
    // Issue-137: a stale CompletedSuccess result whose run_id no longer
    // owns the lease must not overwrite a newer durable terminal state.
    let c = conn();
    let claimed = claim_for_launch(
        &issue(30),
        &cfg(2),
        &c,
        "cfg",
        &DaemonPathBases::default(),
        std::path::Path::new("config"),
    )
    .unwrap()
    .unwrap();
    // Simulate a concurrent writer advancing the lease to Failed with the
    // same run_id before the stale launcher returns CompletedSuccess.
    update_lease_status(
        &c,
        &claimed.lease_id,
        LeaseStatus::Failed,
        Some(&claimed.request.run_id),
    )
    .unwrap();

    let outcome = finish_lease_after_result(
        &c,
        &claimed.lease_id,
        &claimed.request.run_id,
        Ok(WorkflowLaunchResult::CompletedSuccess),
    )
    .unwrap();

    assert_eq!(
        outcome,
        LaunchOutcome::LeaseStatePreserved {
            run_id: claimed.request.run_id.clone(),
            current_status: Some(LeaseStatus::Failed),
            current_run_id: Some(claimed.request.run_id.clone()),
        },
        "a stale CompletedSuccess must not overwrite a terminal Failed lease"
    );
    let lease = get_lease_for_issue(&c, "o/r", 30).unwrap().unwrap();
    assert_eq!(
        lease.status,
        LeaseStatus::Failed,
        "the durable Failed state must be preserved"
    );
}

#[test]
fn stale_failure_does_not_overwrite_terminal_lease() {
    // Issue-137: a stale CompletedFailure result whose run_id no longer
    // owns the lease must not overwrite a newer durable terminal state.
    let c = conn();
    let claimed = claim_for_launch(
        &issue(31),
        &cfg(2),
        &c,
        "cfg",
        &DaemonPathBases::default(),
        std::path::Path::new("config"),
    )
    .unwrap()
    .unwrap();
    // Simulate a concurrent writer advancing the lease to Completed with
    // the same run_id before the stale launcher returns CompletedFailure.
    update_lease_status(
        &c,
        &claimed.lease_id,
        LeaseStatus::Completed,
        Some(&claimed.request.run_id),
    )
    .unwrap();

    let outcome = finish_lease_after_result(
        &c,
        &claimed.lease_id,
        &claimed.request.run_id,
        Ok(WorkflowLaunchResult::CompletedFailure),
    )
    .unwrap();

    assert_eq!(
        outcome,
        LaunchOutcome::LeaseStatePreserved {
            run_id: claimed.request.run_id.clone(),
            current_status: Some(LeaseStatus::Completed),
            current_run_id: Some(claimed.request.run_id.clone()),
        },
        "a stale CompletedFailure must not overwrite a terminal Completed lease"
    );
    let lease = get_lease_for_issue(&c, "o/r", 31).unwrap().unwrap();
    assert_eq!(
        lease.status,
        LeaseStatus::Completed,
        "the durable Completed state must be preserved"
    );
}

#[test]
fn stale_success_on_wrong_owner_preserves_lease() {
    // Issue-137: when the lease run_id was superseded by a concurrent
    // reclaim, the stale success result must not apply.
    let c = conn();
    let claimed = claim_for_launch(
        &issue(32),
        &cfg(2),
        &c,
        "cfg",
        &DaemonPathBases::default(),
        std::path::Path::new("config"),
    )
    .unwrap()
    .unwrap();
    // Simulate a concurrent reclaim assigning a new run_id while leaving
    // the status Running.
    update_lease_status(
        &c,
        &claimed.lease_id,
        LeaseStatus::Running,
        Some("new-owner-run"),
    )
    .unwrap();

    let outcome = finish_lease_after_result(
        &c,
        &claimed.lease_id,
        &claimed.request.run_id,
        Ok(WorkflowLaunchResult::CompletedSuccess),
    )
    .unwrap();

    assert_eq!(
        outcome,
        LaunchOutcome::LeaseStatePreserved {
            run_id: claimed.request.run_id.clone(),
            current_status: Some(LeaseStatus::Running),
            current_run_id: Some("new-owner-run".to_string()),
        },
        "a stale success must not apply when the owner changed"
    );
    let lease = get_lease_for_issue(&c, "o/r", 32).unwrap().unwrap();
    assert_eq!(lease.status, LeaseStatus::Running);
    assert_eq!(lease.run_id.as_deref(), Some("new-owner-run"));
}

#[test]
fn success_on_missing_lease_reports_preserved() {
    // Issue-137: a terminal result for a missing lease must report
    // LeaseStatePreserved rather than silently succeeding.
    let c = conn();
    let outcome = finish_lease_after_result(
        &c,
        "missing-lease-success",
        "run-missing-success",
        Ok(WorkflowLaunchResult::CompletedSuccess),
    )
    .unwrap();
    assert_eq!(
        outcome,
        LaunchOutcome::LeaseStatePreserved {
            run_id: "run-missing-success".to_string(),
            current_status: None,
            current_run_id: None,
        }
    );
}
