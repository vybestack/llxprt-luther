//! Tests for poll-skip observability: concurrent lease / wait-state / run-status
//! transitions and orphaned waits must surface as structured `SkippedPollDetail`
//! entries (and not be counted as applied), while never blocking unrelated
//! valid waits.

use super::*;

#[test]
fn orphaned_wait_is_observable_without_blocking_another_valid_wait() {
    let c = conn();
    seed_orphaned_external_wait(&c, "run-orphan", 139);
    seed_external_wait(&c, "run-valid", 140);
    let poller = ScriptedPoller::new(vec![
        PollClassification::StillWaiting,
        PollClassification::StillWaiting,
    ]);

    let summary = poll_due_waits(&c, &poller).unwrap();

    assert_eq!(summary.pollable_waits, 2);
    assert_eq!(summary.polls_applied, 1, "valid wait must still be applied");
    assert_eq!(summary.skipped_polls, 1);
    assert_eq!(summary.skipped_poll_details_dropped, 0);
    assert!(summary.skipped_poll_details.iter().any(|detail| {
        detail.run_id == "run-orphan" && detail.reason == SkippedPollReason::RunMissing
    }));
    let valid_wait = crate::persistence::wait_state::get_wait_state(&c, "run-valid")
        .unwrap()
        .unwrap();
    assert_eq!(valid_wait.poll_count, 1);
}

#[test]
fn skipped_poll_details_are_capped_per_pass_with_dropped_count() {
    let c = conn();
    let skipped_total = MAX_SKIPPED_POLL_DETAILS + 3;
    for index in 0..skipped_total {
        seed_orphaned_external_wait(&c, &format!("run-orphan-{index:03}"), 1_000 + index as u64);
    }
    let poller = ScriptedPoller::new(vec![PollClassification::StillWaiting; skipped_total]);

    let summary = poll_due_waits(&c, &poller).unwrap();

    assert_eq!(summary.skipped_polls, skipped_total);
    assert_eq!(summary.skipped_poll_details.len(), MAX_SKIPPED_POLL_DETAILS);
    assert_eq!(summary.skipped_poll_details_dropped, 3);
}

#[test]
fn poll_skip_for_concurrent_lease_transition_is_observable_in_summary() {
    // OCR 3565653883: a benign concurrent poll skip (lease already advanced)
    // must be visible in RunSummary via skipped_polls and skipped_poll_details,
    // not silently swallowed and not counted in polls_applied.
    let c = conn();
    seed_external_wait(&c, "run-skip-lease", 140);

    // The poller side-effect simulates a concurrent writer advancing the
    // lease to ReadyToResume between the poll list and the apply.
    let poller = RacingPoller {
        side_effect: |record| {
            let lease_id = record.lease_id.as_deref().unwrap();
            update_lease_status(
                &c,
                lease_id,
                LeaseStatus::ReadyToResume,
                Some(&record.run_id),
            )
            .unwrap();
        },
    };

    let q = MockQuery { issues: vec![] };
    let l = RepeatedSuspendLauncher;
    let target = pollable_target();

    let summary = run_single_target_once(&target, &q, &c, &l, &poller);

    assert_eq!(
        summary.polls_applied, 0,
        "a rejected poll must not count as applied"
    );
    assert_eq!(
        summary.skipped_polls, 1,
        "a rejected poll must be counted as skipped"
    );
    assert_eq!(summary.skipped_poll_details.len(), 1);
    let detail = &summary.skipped_poll_details[0];
    assert_eq!(detail.run_id, "run-skip-lease");
    assert_eq!(detail.reason, SkippedPollReason::LeaseTransitionRejected);
    assert_eq!(
        detail.lease_transition_reason,
        Some("lease_still_waiting_transition_rejected: lease has advanced past waiting or owned by another run")
    );
}

#[test]
fn poll_skip_for_concurrent_wait_state_transition_is_observable_in_summary() {
    // OCR 3565653883: a benign concurrent wait-state skip must also be
    // observable in RunSummary.
    let c = conn();
    seed_external_wait(&c, "run-skip-wait", 141);

    // The poller side-effect deletes the wait-states row so
    // update_wait_state_after_poll returns false.
    let poller = RacingPoller {
        side_effect: |record| {
            crate::persistence::wait_state::delete_wait_state(&c, &record.run_id).unwrap();
        },
    };

    let q = MockQuery { issues: vec![] };
    let l = RepeatedSuspendLauncher;
    let target = pollable_target();

    let summary = run_single_target_once(&target, &q, &c, &l, &poller);

    assert_eq!(summary.polls_applied, 0);
    assert_eq!(summary.skipped_polls, 1);
    assert_eq!(summary.skipped_poll_details.len(), 1);
    let detail = &summary.skipped_poll_details[0];
    assert_eq!(detail.run_id, "run-skip-wait");
    assert_eq!(
        detail.reason,
        SkippedPollReason::WaitStateConcurrentTransition
    );
}

#[test]
fn still_waiting_poll_does_not_regress_completed_lease_to_waiting() {
    // OCR 3565653889: a still-waiting poll must not pull a terminal lease
    // back to WaitingExternal. The expected-status list for
    // apply_still_waiting is WaitingExternal only, so a Completed lease
    // must cause LeaseTransitionRejected, which surfaces as a skipped
    // poll in RunSummary.
    let c = conn();
    seed_external_wait(&c, "run-completed", 142);

    // The poller side-effect advances the lease to Completed, simulating a
    // concurrent terminal transition while a stale poller pass runs.
    let poller = RacingPoller {
        side_effect: |record| {
            let lease_id = record.lease_id.as_deref().unwrap();
            update_lease_status(&c, lease_id, LeaseStatus::Completed, Some(&record.run_id))
                .unwrap();
        },
    };

    let q = MockQuery { issues: vec![] };
    let l = RepeatedSuspendLauncher;
    let target = pollable_target();

    let summary = run_single_target_once(&target, &q, &c, &l, &poller);

    assert_eq!(summary.polls_applied, 0);
    assert_eq!(summary.skipped_polls, 1);
    // The lease must remain Completed, not regress to WaitingExternal.
    let final_lease = get_lease_for_issue(&c, "o/r", 142).unwrap().unwrap();
    assert_eq!(
        final_lease.status,
        LeaseStatus::Completed,
        "a Completed lease must not regress to WaitingExternal"
    );
}

#[test]
fn poll_skip_for_run_status_concurrent_transition_is_observable_in_summary() {
    // OCR 3565653883 / issue-131: a benign concurrent poll skip where the
    // run is already terminal (stale poller's status update rejected) must
    // be visible in RunSummary via skipped_polls and skipped_poll_details
    // with reason RunStatusConcurrentTransition, not silently swallowed and
    // not counted in polls_applied.
    let c = conn();
    seed_external_wait(&c, "run-skip-status", 143);

    // The poller side-effect advances the run to a terminal status
    // (Completed) between the poll list and the apply, so the stale poller's
    // conditional status update is rejected.
    let poller = RacingPoller {
        side_effect: |record| {
            let mut run = crate::persistence::RunMetadata::new(&record.run_id, "wf", "cfg");
            run.status = crate::persistence::RunStatus::Completed;
            crate::persistence::sqlite::persist_run_with_conn(&c, &run).unwrap();
        },
    };

    let q = MockQuery { issues: vec![] };
    let l = RepeatedSuspendLauncher;
    let target = pollable_target();

    let summary = run_single_target_once(&target, &q, &c, &l, &poller);

    assert_eq!(
        summary.polls_applied, 0,
        "a rejected poll must not count as applied"
    );
    assert_eq!(
        summary.skipped_polls, 1,
        "a rejected poll must be counted as skipped"
    );
    assert_eq!(summary.skipped_poll_details.len(), 1);
    let detail = &summary.skipped_poll_details[0];
    assert_eq!(detail.run_id, "run-skip-status");
    assert_eq!(
        detail.reason,
        SkippedPollReason::RunStatusConcurrentTransition
    );
    assert_eq!(detail.step_id, "watch_pr_checks");
}
