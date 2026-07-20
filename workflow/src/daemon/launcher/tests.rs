use super::*;
use crate::persistence::leases::{count_active_leases_for_config, try_claim};
use crate::persistence::leases::{get_lease_for_issue, init_leases_table};
use std::sync::Mutex;

fn cfg(max: u32) -> DiscoveryConfig {
    DiscoveryConfig {
        enabled: true,
        repo: Some("o/r".to_string()),
        include_labels: vec![],
        exclude_labels: vec![],
        active_parent_label: None,
        issue_states: vec!["open".to_string()],
        approval_label: None,
        approval_actor: None,
        claim_assignee: None,
        claim_label: None,
        milestone_order: Some("semver".to_string()),
        max_concurrent_runs: Some(max),
        poll_interval_secs: Some(300),
        max_concurrent_active_runs: None,
        max_concurrent_runs_per_repository: None,
        max_concurrent_runs_per_config: None,
        route_parent_issues: false,
        parent_workflow_type_id: Some("parent-issue-orchestrator-v1".to_string()),
        parent_config_id: None,
        skip_children_of_active_parents: false,
    }
}

fn issue(number: u64) -> GithubIssue {
    GithubIssue {
        number,
        title: format!("Issue {number}"),
        state: "open".to_string(),
        labels: vec![],
        assignees: vec![],
        milestone: None,
        body: None,
    }
}

fn conn() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    init_leases_table(&c).unwrap();
    crate::persistence::sqlite::init_runs_schema(&c).unwrap();
    crate::persistence::wait_state::init_wait_states_table(&c).unwrap();
    c
}

/// Seed a complete, pollable external wait using the production-path
/// `persist_external_wait` function, establishing the full invariant:
/// run status, checkpoint, wait_states row, and waiting lease.
fn seed_complete_external_wait(
    c: &Connection,
    issue_number: u64,
    run_id: &str,
    resume_step: &str,
) -> String {
    let lease = try_claim(c, "o/r", issue_number, "cfg").unwrap().unwrap();
    update_lease_status(c, &lease.lease_id, LeaseStatus::Running, Some(run_id)).unwrap();
    // Seed run metadata + checkpoint (required by persist_external_wait).
    let metadata = crate::persistence::RunMetadata::new(run_id, "wf", "cfg");
    crate::persistence::persist_run_with_conn(c, &metadata).unwrap();
    crate::persistence::checkpoint::save_checkpoint_with_conn(
        c,
        &crate::persistence::checkpoint::Checkpoint::new(run_id, resume_step),
    )
    .unwrap();
    let mut record = crate::persistence::wait_state::WaitStateRecord::new(run_id, "cfg");
    record.lease_id = Some(lease.lease_id.clone());
    record.repository = "o/r".to_string();
    record.issue_number = issue_number;
    record.resume_step = resume_step.to_string();
    crate::persistence::persist_external_wait(c, &record).unwrap();
    lease.lease_id
}

/// Records launch requests and returns a preset success flag.
struct MockLauncher {
    result: WorkflowLaunchResult,
    requests: Mutex<Vec<LaunchRequest>>,
}

impl MockLauncher {
    fn new(result: WorkflowLaunchResult) -> Self {
        Self {
            result,
            requests: Mutex::new(Vec::new()),
        }
    }
}

impl WorkflowLauncher for MockLauncher {
    fn launch(&self, request: &LaunchRequest) -> Result<WorkflowLaunchResult, String> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(self.result.clone())
    }
}

#[test]
fn launch_wins_claim_and_completes() {
    let c = conn();
    let l = MockLauncher::new(WorkflowLaunchResult::CompletedSuccess);
    let outcome = claim_and_launch(
        &issue(1),
        &cfg(2),
        &c,
        &l,
        "cfg",
        &DaemonPathBases::default(),
    )
    .unwrap();
    match outcome {
        LaunchOutcome::Launched { success, .. } => assert!(success),
        other => panic!("unexpected: {other:?}"),
    }
    let lease = get_lease_for_issue(&c, "o/r", 1).unwrap().unwrap();
    assert_eq!(lease.status, LeaseStatus::Completed);
    assert!(lease.run_id.is_some());
    assert_eq!(l.requests.lock().unwrap().len(), 1);
    assert_eq!(l.requests.lock().unwrap()[0].issue_number, 1);
}

#[test]
fn second_claim_is_rejected() {
    let c = conn();
    let l = MockLauncher::new(WorkflowLaunchResult::CompletedSuccess);
    // First wins and completes (lease no longer active).
    claim_and_launch(
        &issue(1),
        &cfg(2),
        &c,
        &l,
        "cfg",
        &DaemonPathBases::default(),
    )
    .unwrap();
    // Pre-existing active claim from another config blocks relaunch.
    try_claim(&c, "o/r", 2, "other").unwrap();
    let outcome = claim_and_launch(
        &issue(2),
        &cfg(2),
        &c,
        &l,
        "cfg",
        &DaemonPathBases::default(),
    )
    .unwrap();
    assert_eq!(outcome, LaunchOutcome::Skipped(SkipReason::HasActiveLease));
}

#[test]
fn failed_run_marks_lease_failed() {
    let c = conn();
    let l = MockLauncher::new(WorkflowLaunchResult::CompletedFailure);
    let outcome = claim_and_launch(
        &issue(3),
        &cfg(2),
        &c,
        &l,
        "cfg",
        &DaemonPathBases::default(),
    )
    .unwrap();
    match outcome {
        LaunchOutcome::Launched { success, .. } => assert!(!success),
        other => panic!("unexpected: {other:?}"),
    }
    let lease = get_lease_for_issue(&c, "o/r", 3).unwrap().unwrap();
    assert_eq!(lease.status, LeaseStatus::Failed);
}

#[test]
fn suspended_run_marks_lease_waiting_external() {
    let c = conn();
    let l = MockLauncher::new(WorkflowLaunchResult::SuspendedExternalWait);
    let outcome = claim_and_launch(
        &issue(5),
        &cfg(2),
        &c,
        &l,
        "cfg",
        &DaemonPathBases::default(),
    )
    .unwrap();
    let run_id = match outcome {
        LaunchOutcome::WaitingExternal { run_id } => run_id,
        other => panic!("unexpected: {other:?}"),
    };
    let lease = get_lease_for_issue(&c, "o/r", 5).unwrap().unwrap();
    assert_eq!(lease.status, LeaseStatus::WaitingExternal);
    assert_eq!(lease.run_id.as_deref(), Some(run_id.as_str()));
    assert_eq!(count_active_leases_for_config(&c, "cfg").unwrap(), 0);
}

#[test]
fn error_after_complete_external_wait_keeps_lease_resumable() {
    // Issue 131 invariant: when the launcher returns an error after a
    // complete pollable external wait has been persisted via the
    // production path (`persist_external_wait`), the lease must stay
    // WaitingExternal so the daemon poller can resume it.
    let c = conn();
    let run_id = "run-complete-wait";
    let lease_id = seed_complete_external_wait(&c, 6, run_id, "watch_pr_checks");

    let outcome = finish_lease_after_result(
        &c,
        &lease_id,
        run_id,
        Err("downstream wrapper error after persist".to_string()),
    )
    .unwrap();

    assert_eq!(
        outcome,
        LaunchOutcome::WaitingExternal {
            run_id: run_id.to_string(),
        },
        "an error after a complete external wait must not mark the lease Failed"
    );
    let lease = get_lease_for_issue(&c, "o/r", 6).unwrap().unwrap();
    assert_eq!(lease.status, LeaseStatus::WaitingExternal);
}

#[test]
fn error_with_incomplete_wait_marks_lease_failed() {
    // Issue 131 invariant: when only the run status is WaitingExternal
    // but no pollable wait_states row exists (incomplete invariant), the
    // lease must go Failed — never strand capacity on an un-pollable run.
    let c = conn();
    let claimed = claim_for_launch(&issue(7), &cfg(2), &c, "cfg", &DaemonPathBases::default())
        .unwrap()
        .unwrap();
    // Seed run status only — no wait_states row (incomplete invariant).
    let mut metadata = crate::persistence::RunMetadata::new(&claimed.request.run_id, "wf", "cfg");
    metadata.status = crate::persistence::RunStatus::WaitingExternal;
    crate::persistence::persist_run_with_conn(&c, &metadata).unwrap();

    let outcome = finish_lease_after_result(
        &c,
        &claimed.lease_id,
        &claimed.request.run_id,
        Err("persist wait state: missing checkpoint".to_string()),
    )
    .unwrap();

    match outcome {
        LaunchOutcome::Launched { success, .. } => assert!(!success),
        other => panic!("unexpected: {other:?}"),
    }
    let lease = get_lease_for_issue(&c, "o/r", 7).unwrap().unwrap();
    assert_eq!(lease.status, LeaseStatus::Failed);
}

#[test]
fn error_when_invariant_check_fails_compensates_lease_to_failed() {
    // Issue 131 invariant: when `has_pollable_external_wait` itself
    // returns an error (e.g. a corrupt wait_states row causing a decode
    // failure), the launcher must compensate the lease to Failed rather
    // than propagating the error and leaving a Running lease stranded.
    let c = conn();
    let run_id = "run-decode-err";
    let lease_id = seed_complete_external_wait(&c, 9, run_id, "watch_pr_checks");
    c.execute("DROP TABLE runs", []).unwrap();

    let outcome = finish_lease_after_result(
        &c,
        &lease_id,
        run_id,
        Err("downstream wrapper error after persist".to_string()),
    )
    .unwrap();

    match outcome {
        LaunchOutcome::Launched { success, .. } => assert!(!success),
        other => panic!("unexpected: {other:?}"),
    }
    let lease = get_lease_for_issue(&c, "o/r", 9).unwrap().unwrap();
    assert_eq!(
        lease.status,
        LeaseStatus::Failed,
        "an invariant-query failure must compensate to Failed, not strand the lease"
    );
}

#[test]
fn suspended_external_wait_does_not_overwrite_terminal_lease() {
    // Issue 131 invariant: a SuspendedExternalWait result must not
    // overwrite a lease that has already transitioned to a terminal
    // state (e.g. the poller classified it while the engine was still
    // running). The conditional lease update leaves the terminal lease
    // intact.
    let c = conn();
    let claimed = claim_for_launch(&issue(8), &cfg(2), &c, "cfg", &DaemonPathBases::default())
        .unwrap()
        .unwrap();
    // Simulate the poller marking the lease terminal while the engine ran.
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
        Ok(WorkflowLaunchResult::SuspendedExternalWait),
    )
    .unwrap();
    assert_eq!(
        outcome,
        LaunchOutcome::LeaseStatePreserved {
            run_id: claimed.request.run_id.clone(),
            current_status: Some(LeaseStatus::Failed),
            current_run_id: Some(claimed.request.run_id.clone()),
        },
        "a rejected stale suspend must report the durable lease state"
    );
    let lease = get_lease_for_issue(&c, "o/r", 8).unwrap().unwrap();
    assert_eq!(
        lease.status,
        LeaseStatus::Failed,
        "a terminal lease must not be overwritten by a stale suspend"
    );
}

#[test]
fn suspended_external_wait_reports_and_preserves_ready_to_resume_lease() {
    let c = conn();
    let claimed = claim_for_launch(&issue(81), &cfg(2), &c, "cfg", &DaemonPathBases::default())
        .unwrap()
        .unwrap();
    update_lease_status(
        &c,
        &claimed.lease_id,
        LeaseStatus::ReadyToResume,
        Some(&claimed.request.run_id),
    )
    .unwrap();

    let outcome = finish_lease_after_result(
        &c,
        &claimed.lease_id,
        &claimed.request.run_id,
        Ok(WorkflowLaunchResult::SuspendedExternalWait),
    )
    .unwrap();

    assert_eq!(
        outcome,
        LaunchOutcome::LeaseStatePreserved {
            run_id: claimed.request.run_id.clone(),
            current_status: Some(LeaseStatus::ReadyToResume),
            current_run_id: Some(claimed.request.run_id.clone()),
        }
    );
    let lease = get_lease_for_issue(&c, "o/r", 81).unwrap().unwrap();
    assert_eq!(lease.status, LeaseStatus::ReadyToResume);
}

#[test]
fn concurrency_limit_blocks_and_records() {
    let c = conn();
    let l = MockLauncher::new(WorkflowLaunchResult::CompletedSuccess);
    // Pre-fill one active running lease to occupy the only slot.
    let pre = try_claim(&c, "o/r", 100, "cfg").unwrap().unwrap();
    update_lease_status(&c, &pre.lease_id, LeaseStatus::Running, Some("r0")).unwrap();
    // max=1 => claiming a new issue over-claims and must be released.
    let outcome = claim_and_launch(
        &issue(4),
        &cfg(1),
        &c,
        &l,
        "cfg",
        &DaemonPathBases::default(),
    )
    .unwrap();
    assert_eq!(
        outcome,
        LaunchOutcome::Skipped(SkipReason::ConcurrencyLimitReached)
    );
    let lease = get_lease_for_issue(&c, "o/r", 4).unwrap().unwrap();
    assert_eq!(lease.status, LeaseStatus::Abandoned);
    assert!(l.requests.lock().unwrap().is_empty());
}

#[test]
fn lease_running_during_launch() {
    let c = conn();
    let l = MockLauncher::new(WorkflowLaunchResult::CompletedSuccess);
    let outcome = claim_for_launch(
        &issue(5),
        &cfg(2),
        &c,
        "cfg",
        &DaemonPathBases {
            work_dir_base: None,
            artifact_dir_base: None,
        },
    )
    .unwrap()
    .unwrap();
    let lease = get_lease_for_issue(&c, "o/r", 5).unwrap().unwrap();
    assert_eq!(lease.status, LeaseStatus::Running);
    assert_eq!(
        lease.run_id.as_deref(),
        Some(outcome.request.run_id.as_str())
    );
    finish_lease_after_result(
        &c,
        &outcome.lease_id,
        &outcome.request.run_id,
        l.launch(&outcome.request),
    )
    .unwrap();
}

#[test]
fn claim_for_launch_attaches_generated_run_id_paths() {
    let c = conn();
    let bases = DaemonPathBases {
        work_dir_base: Some(std::path::PathBuf::from(
            "/tmp/luther-workspaces/llxprt-luther",
        )),
        artifact_dir_base: Some(std::path::PathBuf::from(
            "/tmp/luther-artifacts/llxprt-luther",
        )),
    };
    let claimed = claim_for_launch(&issue(7), &cfg(2), &c, "cfg", &bases)
        .unwrap()
        .unwrap();
    let work = claimed.request.work_dir.as_deref().unwrap();
    let artifact = claimed.request.artifact_dir.as_deref().unwrap();
    // The run-id path component matches the generated internal run_id.
    assert!(work.to_str().unwrap().ends_with(&claimed.request.run_id));
    assert!(artifact
        .to_str()
        .unwrap()
        .ends_with(&claimed.request.run_id));
    // And contains the issue segment.
    assert!(work.to_str().unwrap().contains("issue-7"));
    assert!(artifact.to_str().unwrap().contains("issue-7"));
}

#[test]
fn compensate_error_does_not_overwrite_terminal_lease() {
    let c = conn();
    let claimed = claim_for_launch(&issue(10), &cfg(2), &c, "cfg", &DaemonPathBases::default())
        .unwrap()
        .unwrap();
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
        Err("stale error after concurrent completion".to_string()),
    )
    .unwrap();

    assert_eq!(
        outcome,
        LaunchOutcome::LeaseStatePreserved {
            run_id: claimed.request.run_id.clone(),
            current_status: Some(LeaseStatus::Completed),
            current_run_id: Some(claimed.request.run_id.clone()),
        }
    );
    let lease = get_lease_for_issue(&c, "o/r", 10).unwrap().unwrap();
    assert_eq!(
        lease.status,
        LeaseStatus::Completed,
        "a Completed lease must not be overwritten by a stale error compensation"
    );
}

#[test]
fn compensate_error_with_pollable_wait_does_not_overwrite_ready_lease() {
    // Race regression: when the launcher hits an error and the lease has
    // already been advanced to ReadyToResume by the poller, the invariant
    // check (has_pollable_external_wait) returns false (a ReadyToResume
    // lease is not "pollable"), so the launcher tries to fail it. The
    // conditional fail transition must reject because ReadyToResume is not
    // in the expected set [Running, WaitingExternal], preserving the
    // poller's classification.
    let c = conn();
    let run_id = "run-race-ready";
    let lease_id = seed_complete_external_wait(&c, 11, run_id, "watch_pr_checks");
    // Simulate the poller advancing to ReadyToResume while the engine ran.
    update_lease_status(&c, &lease_id, LeaseStatus::ReadyToResume, Some(run_id)).unwrap();

    let outcome = finish_lease_after_result(
        &c,
        &lease_id,
        run_id,
        Err("stale error after ready classification".to_string()),
    )
    .unwrap();

    assert_eq!(
        outcome,
        LaunchOutcome::LeaseStatePreserved {
            run_id: run_id.to_string(),
            current_status: Some(LeaseStatus::ReadyToResume),
            current_run_id: Some(run_id.to_string()),
        }
    );
    let lease = get_lease_for_issue(&c, "o/r", 11).unwrap().unwrap();
    assert_eq!(
        lease.status,
        LeaseStatus::ReadyToResume,
        "a ReadyToResume lease must not be overwritten by a stale error compensation"
    );
}

#[test]
fn compensate_error_reports_missing_lease_as_preserved() {
    let c = conn();
    let outcome = finish_lease_after_result(
        &c,
        "missing-lease",
        "missing-run",
        Err("launch failed after lease deletion".to_string()),
    )
    .unwrap();

    assert_eq!(
        outcome,
        LaunchOutcome::LeaseStatePreserved {
            run_id: "missing-run".to_string(),
            current_status: None,
            current_run_id: None,
        }
    );
}

// ---- prepare_resume_lease transaction-safety tests (issue-137) ----

/// Seed a `ReadyToResume` lease for `issue_number` owned by `run_id`,
/// including a claim-metadata receipt and a run row with `workflow_type`.
/// Returns the lease id so the caller can query durable state afterwards.
fn seed_ready_to_resume(
    c: &Connection,
    issue_number: u64,
    run_id: &str,
    workflow_type: &str,
) -> String {
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
    let metadata = crate::persistence::RunMetadata::new(run_id, workflow_type, "cfg");
    crate::persistence::persist_run_with_conn(c, &metadata).unwrap();
    update_lease_status(c, &lease.lease_id, LeaseStatus::ReadyToResume, Some(run_id)).unwrap();
    lease.lease_id
}

#[test]
fn resume_preparation_acquires_ready_to_resume_lease() {
    let c = conn();
    let lease_id = seed_ready_to_resume(&c, 20, "resume-ok", "wf-type");
    let lease = get_lease_for_issue(&c, "o/r", 20).unwrap().unwrap();
    let claimed = prepare_resume_lease(&lease, &c)
        .unwrap()
        .expect("should acquire");
    assert_eq!(claimed.lease_id, lease_id);
    assert_eq!(claimed.request.run_id, "resume-ok");
    assert_eq!(claimed.request.workflow_type_id.as_deref(), Some("wf-type"));
    assert!(claimed.request.daemon_managed_claim);
    assert!(claimed.request.claim_assignment_added);
    assert!(!claimed.request.claim_label_added);
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

// ---- CompletedSuccess/CompletedFailure exact-owner Running CAS (issue-137) ----

#[test]
fn stale_success_does_not_overwrite_terminal_lease() {
    // Issue-137: a stale CompletedSuccess result whose run_id no longer
    // owns the lease must not overwrite a newer durable terminal state.
    let c = conn();
    let claimed = claim_for_launch(&issue(30), &cfg(2), &c, "cfg", &DaemonPathBases::default())
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
    let claimed = claim_for_launch(&issue(31), &cfg(2), &c, "cfg", &DaemonPathBases::default())
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
    let claimed = claim_for_launch(&issue(32), &cfg(2), &c, "cfg", &DaemonPathBases::default())
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
