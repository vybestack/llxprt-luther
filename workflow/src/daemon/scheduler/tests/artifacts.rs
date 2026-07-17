//! Tests for post-commit artifact-write warning observability. When a poll
//! decision's database transaction commits but the follow-up artifact writes
//! fail, the warnings must be aggregated into `RunSummary::artifact_warnings`
//! (capped per pass) while the poll still counts as applied.

use super::*;

#[test]
fn poll_artifact_warning_is_observable_in_summary() {
    // OCR artifact aggregation: when a committed poll decision's
    // post-commit artifact write fails, the warning must be visible in
    // RunSummary via artifact_warnings, and the poll must still count as
    // applied (the DB committed). The artifact_warning must carry the
    // run_id, phase, and error string.
    // We seed a complete pollable wait (including run metadata) and set an
    // invalid artifact root so the post-commit artifact write fails.
    let c = conn();
    seed_external_wait(&c, "run-artifact", 144);

    // A regular file cannot contain the per-run artifact directory on any
    // supported platform, so the write fails without relying on permissions.
    let (_artifact_temp, blocked_root) = blocked_artifact_root();
    let _env_guard = super::super::super::test_env::ArtifactEnvGuard::lock_and_set(&blocked_root);

    let poller = ScriptedPoller::new(vec![PollClassification::StillWaiting]);
    let q = MockQuery { issues: vec![] };
    let l = RepeatedSuspendLauncher;
    let target = pollable_target();

    let summary = run_single_target_once(&target, &q, &c, &l, &poller);

    // The poll must count as applied (DB committed).
    assert_eq!(
        summary.polls_applied, 1,
        "the committed poll must count as applied even with artifact warnings"
    );
    // The artifact warning must be visible.
    assert!(
        !summary.artifact_warnings.is_empty(),
        "artifact warnings must be recorded in RunSummary when post-commit writes fail"
    );
    let warning = &summary.artifact_warnings[0];
    assert_eq!(
        warning.run_id, "run-artifact",
        "artifact warning must carry the correct run_id"
    );
    assert!(
        !warning.error.is_empty(),
        "artifact warning must carry a non-empty error string"
    );
}

#[test]
fn dual_artifact_failure_aggregates_all_warnings_in_summary() {
    // CodeRabbit finding: when BOTH the PR-check snapshot write AND the
    // poll-result artifact write fail on the same committed poll, both
    // warnings must be collected and propagated into RunSummary — not just
    // the first. Previously, the early return on snapshot failure dropped
    // the poll-artifacts error when both paths failed.
    // We seed a PrChecks wait kind with an invalid artifact_root in the
    // wait_condition so the snapshot write is attempted and fails, and set
    // LUTHER_ARTIFACTS_ROOT to an invalid path so the poll-result artifact
    // also fails.
    let c = conn();
    seed_external_wait(&c, "run-dual", 145);
    // Mutate the seeded wait-state to trigger the PR-check snapshot path.
    // The snapshot write needs: wait_kind=PrChecks, artifact_root in
    // wait_condition, pr_number, head_sha, and a valid repo format.
    let mut ws = crate::persistence::wait_state::get_wait_state(&c, "run-dual")
        .unwrap()
        .expect("wait-state must exist after seeding");
    ws.wait_kind = crate::persistence::wait_state::WaitKind::PrChecks;
    ws.pr_number = Some(9);
    ws.head_sha = Some("head-dual".to_string());
    let (_snapshot_temp, blocked_snapshot_root) = blocked_artifact_root();
    ws.wait_condition = serde_json::json!({
        "artifact_root": blocked_snapshot_root,
        "head_ref": "feature",
        "base_ref": "main",
        "base_sha": "base-dual"
    });
    crate::persistence::wait_state::upsert_wait_state(&c, &ws).unwrap();

    // Use a separate regular-file blocker for the poll-result path so both
    // writes fail independently on every supported platform.
    let (_artifact_temp, blocked_artifact_root) = blocked_artifact_root();
    let _env_guard =
        super::super::super::test_env::ArtifactEnvGuard::lock_and_set(&blocked_artifact_root);

    let poller = ScriptedPoller::new(vec![PollClassification::StillWaiting]);
    let q = MockQuery { issues: vec![] };
    let l = RepeatedSuspendLauncher;
    let target = pollable_target();

    let summary = run_single_target_once(&target, &q, &c, &l, &poller);

    // The poll must count as applied (DB committed).
    assert_eq!(
        summary.polls_applied, 1,
        "the committed poll must count as applied even with dual artifact warnings"
    );
    // Both artifact warnings must be collected — not just the first.
    assert_eq!(
        summary.artifact_warnings.len(),
        2,
        "dual artifact failure must aggregate exactly one warning per failed artifact phase: {:?}",
        summary.artifact_warnings
    );
    // Verify both phases are represented.
    let phases: Vec<_> = summary.artifact_warnings.iter().map(|w| w.phase).collect();
    assert!(
        phases.contains(&crate::daemon::poller::ArtifactPhase::PrCheckSnapshot),
        "the PrCheckSnapshot phase must be among the aggregated warnings: {:?}",
        phases
    );
    assert!(
        phases.contains(&crate::daemon::poller::ArtifactPhase::PollResult),
        "the PollResult phase must be among the aggregated warnings: {:?}",
        phases
    );
    // All warnings must carry the correct run_id.
    for warning in &summary.artifact_warnings {
        assert_eq!(
            warning.run_id, "run-dual",
            "all aggregated warnings must carry the correct run_id"
        );
        assert!(
            !warning.error.is_empty(),
            "all aggregated warnings must carry a non-empty error string"
        );
    }
}

#[test]
fn ready_poll_aggregates_each_failed_poll_artifact_write() {
    let c = conn();
    seed_external_wait(&c, "run-three-artifacts", 146);
    let (_artifact_temp, blocked_root) = blocked_artifact_root();
    let _env_guard = super::super::super::test_env::ArtifactEnvGuard::lock_and_set(&blocked_root);

    let poller = ScriptedPoller::new(vec![PollClassification::ReadyToResume]);
    let q = MockQuery { issues: vec![] };
    let l = RepeatedSuspendLauncher;
    let target = pollable_target();

    let summary = run_single_target_once(&target, &q, &c, &l, &poller);

    assert_eq!(summary.polls_applied, 1);
    assert_eq!(
        summary.artifact_warnings.len(),
        3,
        "poll-result, wait-state, and resume-decision failures must each be observable: {:?}",
        summary.artifact_warnings
    );
    let phases: Vec<_> = summary
        .artifact_warnings
        .iter()
        .map(|warning| warning.phase)
        .collect();
    for expected in [
        crate::daemon::poller::ArtifactPhase::PollResult,
        crate::daemon::poller::ArtifactPhase::WaitState,
        crate::daemon::poller::ArtifactPhase::ResumeDecision,
    ] {
        assert!(
            phases.contains(&expected),
            "missing artifact phase: {expected:?}"
        );
    }
    assert!(summary
        .artifact_warnings
        .iter()
        .all(|warning| { warning.run_id == "run-three-artifacts" && !warning.error.is_empty() }));
}

#[test]
fn artifact_warning_details_are_capped_per_pass_with_dropped_count() {
    let mut summary = RunSummary::default();
    for index in 0..(MAX_ARTIFACT_WARNING_DETAILS + 3) {
        summary.record_artifact_warning(ArtifactWarningDetail {
            run_id: format!("run-warning-{index}"),
            phase: crate::daemon::poller::ArtifactPhase::PollResult,
            error: "disk full".to_string(),
        });
    }

    assert_eq!(
        summary.artifact_warnings.len(),
        MAX_ARTIFACT_WARNING_DETAILS
    );
    assert_eq!(summary.artifact_warnings_dropped, 3);
    assert_eq!(
        summary.artifact_warning_count(),
        MAX_ARTIFACT_WARNING_DETAILS + 3
    );
    assert_eq!(summary.artifact_warnings[0].run_id, "run-warning-0");
}

#[test]
fn lease_state_preserved_details_are_bounded_and_structured() {
    let mut summary = RunSummary::default();
    for index in 0..(MAX_LEASE_STATE_PRESERVED_DETAILS + 3) {
        record_outcome(
            LaunchOutcome::LeaseStatePreserved {
                run_id: format!("run-stale-{index}"),
                current_status: Some(LeaseStatus::ReadyToResume),
                current_run_id: Some(format!("run-current-{index}")),
            },
            false,
            &mut summary,
        );
    }

    assert_eq!(
        summary.lease_states_preserved,
        MAX_LEASE_STATE_PRESERVED_DETAILS + 3
    );
    assert_eq!(
        summary.lease_state_preserved_details.len(),
        MAX_LEASE_STATE_PRESERVED_DETAILS
    );
    assert_eq!(summary.lease_state_preserved_details_dropped, 3);
    assert_eq!(
        summary.lease_state_preserved_details[0],
        LeaseStatePreservedDetail {
            run_id: "run-stale-0".to_string(),
            current_status: Some(LeaseStatus::ReadyToResume),
            current_run_id: Some("run-current-0".to_string()),
        }
    );
}
