use super::*;

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
        std::path::Path::new("config"),
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
        std::path::Path::new("config"),
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
        std::path::Path::new("config"),
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
        std::path::Path::new("config"),
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
        std::path::Path::new("config"),
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
    let claimed = claim_for_launch(
        &issue(7),
        &cfg(2),
        &c,
        "cfg",
        &DaemonPathBases::default(),
        std::path::Path::new("config"),
    )
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
    let claimed = claim_for_launch(
        &issue(8),
        &cfg(2),
        &c,
        "cfg",
        &DaemonPathBases::default(),
        std::path::Path::new("config"),
    )
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
    let claimed = claim_for_launch(
        &issue(81),
        &cfg(2),
        &c,
        "cfg",
        &DaemonPathBases::default(),
        std::path::Path::new("config"),
    )
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
fn ownership_denied_finalizes_lease_as_failed_not_cleanup_abandoned() {
    // An OwnershipDenied result must map to a terminal Failed lease via an
    // exact-owner Running CAS. It must NOT map to CleanupAbandoned, because
    // an ownership-denied workspace is unowned and cleanup cannot run there.
    // A Failed lease is terminal and non-resumable, so it is never selected
    // for cleanup continuation.
    let c = conn();
    let claimed = claim_for_launch(
        &issue(82),
        &cfg(2),
        &c,
        "cfg",
        &DaemonPathBases::default(),
        std::path::Path::new("config"),
    )
    .unwrap()
    .unwrap();

    let outcome = finish_lease_after_result(
        &c,
        &claimed.lease_id,
        &claimed.request.run_id,
        Ok(WorkflowLaunchResult::OwnershipDenied),
    )
    .unwrap();

    match outcome {
        LaunchOutcome::Launched { success, .. } => assert!(!success),
        other => panic!("unexpected outcome: {other:?}"),
    }
    let lease = get_lease_for_issue(&c, "o/r", 82).unwrap().unwrap();
    assert_eq!(
        lease.status,
        LeaseStatus::Failed,
        "OwnershipDenied must finalize as Failed, not CleanupAbandoned"
    );
}

#[test]
fn ownership_denied_does_not_overwrite_terminal_lease() {
    // Issue 158 finding 4: a stale OwnershipDenied result against a lease in
    // a terminal `Completed` state (same owner) is an explicit error. The
    // durable `Completed` state does not represent a consistent
    // ownership-denied terminal (which must be `Failed`), so the
    // inconsistency must surface rather than be silently masked as
    // `LeaseStatePreserved`. The durable `Completed` state is preserved
    // (the CAS rejects), but the error signals the divergence.
    let c = conn();
    let claimed = claim_for_launch(
        &issue(83),
        &cfg(2),
        &c,
        "cfg",
        &DaemonPathBases::default(),
        std::path::Path::new("config"),
    )
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
        Ok(WorkflowLaunchResult::OwnershipDenied),
    );
    assert!(
        outcome.is_err(),
        "an OwnershipDenied result against a same-owner Completed lease must \
         be an explicit error (inconsistent terminal), got: {outcome:?}"
    );
    // The durable Completed state is preserved (the CAS rejected).
    let lease = get_lease_for_issue(&c, "o/r", 83).unwrap().unwrap();
    assert_eq!(lease.status, LeaseStatus::Completed);
}

#[test]
fn ownership_denied_after_predecessor_failed_lease_is_idempotent() {
    // Daemon end-to-end (issue 158): the runner's
    // `persist_terminal_ownership_failure` sets the matching daemon lease
    // directly to Failed (exact-owner Running CAS) before the launcher
    // returns OwnershipDenied. The `finish_lease_after_result` finalizer's
    // OwnershipDenied CAS (Running -> Failed) must then REJECT idempotently
    // with the lease already in `Failed` for the exact same owner. That
    // same-owner `Failed` rejection must be treated as a successful
    // idempotent failed launch (`Launched { success: false }`) so the
    // scheduler's claim/label cleanup runs for the failed run. The durable
    // `Failed` state is preserved; the lease must never be left in
    // `CleanupAbandoned` (which would make it selectable for cleanup
    // continuation).
    let c = conn();
    let claimed = claim_for_launch(
        &issue(84),
        &cfg(2),
        &c,
        "cfg",
        &DaemonPathBases::default(),
        std::path::Path::new("config"),
    )
    .unwrap()
    .unwrap();
    // Simulate the runner's persist_terminal_ownership_failure setting the
    // lease directly to Failed (exact-owner Running CAS) before the launcher
    // returns.
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
        Ok(WorkflowLaunchResult::OwnershipDenied),
    )
    .unwrap();

    assert_eq!(
        outcome,
        LaunchOutcome::Launched {
            run_id: claimed.request.run_id.clone(),
            success: false,
        },
        "an OwnershipDenied result after the predecessor set the lease Failed \
         for the same owner must be treated as an idempotent failed launch so \
         scheduler claim/label cleanup runs"
    );
    let lease = get_lease_for_issue(&c, "o/r", 84).unwrap().unwrap();
    assert_eq!(
        lease.status,
        LeaseStatus::Failed,
        "the lease must remain Failed, never CleanupAbandoned"
    );
}

#[test]
fn ownership_denied_missing_lease_is_explicit_error() {
    // Issue 158 finding 4: a missing lease during ownership-denied
    // finalization is an explicit error, not a silent
    // `LeaseStatePreserved`. The daemon flow always has a claimed lease;
    // reaching the finalizer with a `lease_id` that has no row is a
    // corruption signal that must surface rather than be masked.
    let c = conn();
    let outcome = finish_lease_after_result(
        &c,
        "missing-lease-id",
        "run-missing",
        Ok(WorkflowLaunchResult::OwnershipDenied),
    );
    assert!(
        outcome.is_err(),
        "a missing lease for OwnershipDenied finalization must be an explicit \
         error, got: {outcome:?}"
    );
    let err = outcome.unwrap_err();
    assert!(
        err.to_string().contains("missing"),
        "error should explain the lease is missing, got: {err}"
    );
}

#[test]
fn ownership_denied_foreign_owner_lease_is_explicit_error() {
    // Issue 158 finding 4: an ownership-denied finalization against a lease
    // owned by a different run is an explicit error. This stale launcher
    // must not claim cleanup authority over a foreign lease.
    let c = conn();
    let claimed = claim_for_launch(
        &issue(85),
        &cfg(2),
        &c,
        "cfg",
        &DaemonPathBases::default(),
        std::path::Path::new("config"),
    )
    .unwrap()
    .unwrap();
    // Another run owns the lease and already set it to Failed.
    update_lease_status(
        &c,
        &claimed.lease_id,
        LeaseStatus::Failed,
        Some("run-foreign"),
    )
    .unwrap();

    let outcome = finish_lease_after_result(
        &c,
        &claimed.lease_id,
        "run-stale",
        Ok(WorkflowLaunchResult::OwnershipDenied),
    );
    assert!(
        outcome.is_err(),
        "an OwnershipDenied result against a foreign-owned lease must be an \
         explicit error, got: {outcome:?}"
    );
    let err = outcome.unwrap_err();
    assert!(
        err.to_string().contains("inconsistent"),
        "error should explain the inconsistent state, got: {err}"
    );
}

#[test]
fn ownership_denied_resumable_status_mismatch_is_explicit_error() {
    // Issue 158 finding 4: an ownership-denied finalization against a lease
    // in a resumable/non-terminal status (e.g. `WaitingExternal`) for the
    // same owner is an explicit error. The durable state does not represent
    // a consistent ownership-denied terminal.
    let c = conn();
    let claimed = claim_for_launch(
        &issue(86),
        &cfg(2),
        &c,
        "cfg",
        &DaemonPathBases::default(),
        std::path::Path::new("config"),
    )
    .unwrap()
    .unwrap();
    // Same owner, but the lease is in WaitingExternal (not Running or Failed).
    update_lease_status(
        &c,
        &claimed.lease_id,
        LeaseStatus::WaitingExternal,
        Some(&claimed.request.run_id),
    )
    .unwrap();

    let outcome = finish_lease_after_result(
        &c,
        &claimed.lease_id,
        &claimed.request.run_id,
        Ok(WorkflowLaunchResult::OwnershipDenied),
    );
    assert!(
        outcome.is_err(),
        "an OwnershipDenied result against a resumable-status lease must be \
         an explicit error, got: {outcome:?}"
    );
    let err = outcome.unwrap_err();
    assert!(
        err.to_string().contains("inconsistent"),
        "error should explain the inconsistent state, got: {err}"
    );
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
        std::path::Path::new("config"),
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
        std::path::Path::new("config"),
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
    let claimed = claim_for_launch(
        &issue(7),
        &cfg(2),
        &c,
        "cfg",
        &bases,
        std::path::Path::new("config"),
    )
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
    let claimed = claim_for_launch(
        &issue(10),
        &cfg(2),
        &c,
        "cfg",
        &DaemonPathBases::default(),
        std::path::Path::new("config"),
    )
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
