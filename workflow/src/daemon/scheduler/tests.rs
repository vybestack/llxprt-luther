use super::*;
use crate::adapters::github::GithubError;
use crate::adapters::github_issues::GithubIssue;
use crate::daemon::launcher::{LaunchRequest, WorkflowLaunchResult};
use crate::daemon::poller::{ExternalWaitPoller, PollClassification, PollDecision};
use crate::persistence::leases::{
    get_lease_for_issue, init_leases_table, try_claim, update_lease_status, LeaseStatus,
};
use crate::persistence::wait_state::WaitStateRecord;
use std::sync::Mutex;

mod scheduling;

fn blocked_artifact_root() -> (tempfile::TempDir, std::path::PathBuf) {
    let temp = tempfile::tempdir().expect("create artifact test directory");
    let blocked = temp.path().join("not-a-directory");
    std::fs::write(&blocked, b"file blocks directory creation")
        .expect("create deterministic artifact path blocker");
    (temp, blocked)
}

fn cfg(max: u32) -> DiscoveryConfig {
    DiscoveryConfig {
        enabled: true,
        repo: Some("o/r".to_string()),
        include_labels: vec!["ok".to_string()],
        exclude_labels: vec![],
        active_parent_label: None,
        issue_states: vec!["open".to_string()],
        assignee_filter: None,
        milestone_order: Some("none".to_string()),
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
        labels: vec!["ok".to_string()],
        assignee: None,
        milestone: None,
        body: None,
    }
}

struct MockQuery {
    issues: Vec<GithubIssue>,
}
impl GithubIssueQuery for MockQuery {
    fn list_issues(
        &self,
        _r: &str,
        _l: &[String],
        _s: &[String],
    ) -> Result<Vec<GithubIssue>, GithubError> {
        Ok(self.issues.clone())
    }
    fn has_open_pr_for_issue(&self, _r: &str, _n: u64) -> Result<bool, GithubError> {
        Ok(false)
    }
    fn list_milestones(&self, _r: &str) -> Result<Vec<String>, GithubError> {
        Ok(vec![])
    }
}

struct MockLauncher {
    launched: Mutex<Vec<u64>>,
}
impl WorkflowLauncher for MockLauncher {
    fn launch(&self, request: &LaunchRequest) -> Result<WorkflowLaunchResult, String> {
        self.launched.lock().unwrap().push(request.issue_number);
        Ok(WorkflowLaunchResult::CompletedSuccess)
    }
}
fn conn() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    crate::persistence::sqlite::init_runs_schema(&c).unwrap();
    init_leases_table(&c).unwrap();
    crate::persistence::wait_state::init_wait_states_table(&c).unwrap();
    crate::persistence::checkpoint::save_checkpoint_with_conn(
        &c,
        &crate::persistence::checkpoint::Checkpoint::new("run-wait", "watch_pr_checks"),
    )
    .unwrap();
    let mut metadata = crate::persistence::RunMetadata::new("run-wait", "wf", "cfg");
    metadata.status = crate::persistence::RunStatus::WaitingExternal;
    metadata.set_current_step("watch_pr_checks");
    crate::persistence::persist_run_with_conn(&c, &metadata).unwrap();
    c
}

/// A poller that reports a configurable sequence of classifications,
/// allowing a single run to be polled through multiple external waits.
struct ScriptedPoller {
    classifications: Mutex<Vec<PollClassification>>,
}

impl ScriptedPoller {
    fn new(classifications: Vec<PollClassification>) -> Self {
        Self {
            classifications: Mutex::new(classifications),
        }
    }
}

impl ExternalWaitPoller for ScriptedPoller {
    fn poll(&self, record: &WaitStateRecord) -> PollDecision {
        let mut queue = self.classifications.lock().unwrap();
        if queue.is_empty() {
            panic!(
                "ScriptedPoller exhausted: no more scripted classifications for run {}. \
                 Add more classifications to the test fixture.",
                record.run_id
            );
        }
        let classification = queue.remove(0);
        match classification {
            PollClassification::ReadyToResume => {
                PollDecision::ready(record, serde_json::json!({ "state": "ready" }))
            }
            PollClassification::StillWaiting => PollDecision::still_waiting(record),
            other => PollDecision {
                run_id: record.run_id.clone(),
                classification: other,
                next_poll_at: None,
                observed_state: serde_json::json!({}),
            },
        }
    }
}

/// A launcher whose resume always suspends again, modeling repeated
/// external waits where a resumed run pauses at another watch step.
struct RepeatedSuspendLauncher;

impl WorkflowLauncher for RepeatedSuspendLauncher {
    fn launch(&self, _request: &LaunchRequest) -> Result<WorkflowLaunchResult, String> {
        Ok(WorkflowLaunchResult::SuspendedExternalWait)
    }
    fn resume(&self, _request: &LaunchRequest) -> Result<WorkflowLaunchResult, String> {
        Ok(WorkflowLaunchResult::SuspendedExternalWait)
    }
}

/// A poller that simulates a concurrent writer racing between the poll
/// list and the apply. When `poll()` is called it performs the side-effect
/// closure before returning a `StillWaiting` decision, so
/// `apply_poll_decision` encounters the raced state.
struct RacingPoller<F>
where
    F: Fn(&WaitStateRecord),
{
    side_effect: F,
}

impl<F> ExternalWaitPoller for RacingPoller<F>
where
    F: Fn(&WaitStateRecord),
{
    fn poll(&self, record: &WaitStateRecord) -> PollDecision {
        (self.side_effect)(record);
        PollDecision::still_waiting(record)
    }
}

/// Seed a complete, pollable external wait using the production-path
/// `persist_external_wait` function, establishing the full invariant:
/// run status, checkpoint, wait_states row, and waiting lease.
fn seed_external_wait(conn: &Connection, run_id: &str, issue_number: u64) {
    let lease = try_claim(conn, "o/r", issue_number, "cfg")
        .unwrap()
        .unwrap();
    update_lease_status(conn, &lease.lease_id, LeaseStatus::Running, Some(run_id)).unwrap();
    crate::persistence::persist_run_with_conn(
        conn,
        &crate::persistence::RunMetadata::new(run_id, "wf", "cfg"),
    )
    .unwrap();
    crate::persistence::checkpoint::save_checkpoint_with_conn(
        conn,
        &crate::persistence::checkpoint::Checkpoint::new(run_id, "watch_pr_checks"),
    )
    .unwrap();
    let mut record = WaitStateRecord::new(run_id, "cfg");
    record.lease_id = Some(lease.lease_id.clone());
    record.repository = "o/r".to_string();
    record.issue_number = issue_number;
    record.resume_step = "watch_pr_checks".to_string();
    crate::persistence::persist_external_wait(conn, &record).unwrap();
}

fn seed_orphaned_external_wait(conn: &Connection, run_id: &str, issue_number: u64) {
    let lease = try_claim(conn, "o/r", issue_number, "cfg")
        .unwrap()
        .unwrap();
    update_lease_status(
        conn,
        &lease.lease_id,
        LeaseStatus::WaitingExternal,
        Some(run_id),
    )
    .unwrap();
    let mut record = WaitStateRecord::new(run_id, "cfg");
    record.lease_id = Some(lease.lease_id);
    record.repository = "o/r".to_string();
    record.issue_number = issue_number;
    record.resume_step = "watch_pr_checks".to_string();
    crate::persistence::wait_state::upsert_wait_state(conn, &record).unwrap();
}

fn pollable_target() -> SchedulerTarget {
    SchedulerTarget::new(
        "cfg".to_string(),
        cfg(1),
        DaemonPathBases::default(),
        BTreeMap::new(),
    )
}

fn run_single_target_once(
    target: &SchedulerTarget,
    q: &dyn GithubIssueQuery,
    c: &Connection,
    launcher: &dyn WorkflowLauncher,
    poller: &dyn ExternalWaitPoller,
) -> RunSummary {
    run_multi_target_once_with_poller(
        std::slice::from_ref(target),
        std::slice::from_ref(&q),
        c,
        launcher,
        poller,
    )
    .unwrap()
}

/// Simulate the production resume path writing a replacement wait row
/// after a resumed run suspends again (`persist_external_wait`).
fn simulate_replacement_wait_row(
    conn: &Connection,
    run_id: &str,
    lease_id: &str,
    issue_number: u64,
) {
    let mut record = WaitStateRecord::new(run_id, "cfg");
    record.lease_id = Some(lease_id.to_string());
    record.repository = "o/r".to_string();
    record.issue_number = issue_number;
    record.resume_step = "watch_pr_checks".to_string();
    crate::persistence::persist_external_wait(conn, &record).unwrap();
}

#[test]
fn still_waiting_poll_keeps_lease_waiting_external() {
    // Issue 131 invariant: a `still_waiting` poll decision must keep the
    // lease `waiting_external` so the run remains resumable. The wait is
    // seeded via the production-path `persist_external_wait`.
    let c = conn();
    seed_external_wait(&c, "run-waiting", 131);
    let q = MockQuery { issues: vec![] };
    let l = RepeatedSuspendLauncher;
    let target = pollable_target();
    let poller = ScriptedPoller::new(vec![PollClassification::StillWaiting]);

    let summary = run_single_target_once(&target, &q, &c, &l, &poller);

    assert_eq!(summary.polls_applied, 1);
    assert_eq!(summary.resumed, 0);
    let lease = get_lease_for_issue(&c, "o/r", 131).unwrap().unwrap();
    assert_eq!(
        lease.status,
        LeaseStatus::WaitingExternal,
        "still-waiting poll must keep the lease waiting_external"
    );
}

#[test]
fn replacement_wait_row_after_re_suspend_is_pollable() {
    // Issue 131 invariant: after a resumed run suspends again, the
    // replacement wait row must be pollable — i.e. the wait_states row,
    // run status, and lease are all consistently `WaitingExternal`.
    // In production, `run_daemon_runner` calls `persist_external_wait_state`
    // (which delegates to `persist_external_wait`) before returning
    // `SuspendedExternalWait`. The mock launcher cannot do this, so this
    // test simulates the production resume-suspend cycle directly:
    //   1. Seed a complete external wait.
    //   2. Poller classifies ReadyToResume (deletes old wait row, marks
    //      lease ReadyToResume).
    //   3. The production resume path would re-suspend and call
    //      `persist_external_wait` to write the replacement wait row.
    //      We simulate that here, then verify the replacement row is
    //      pollable on the next scheduler pass.
    let c = conn();
    seed_external_wait(&c, "run-repeat", 133);
    let q = MockQuery { issues: vec![] };
    let l = RepeatedSuspendLauncher;
    let target = pollable_target();

    // Pass 1: poll says ready -> resume -> suspend again.
    let poller1 = ScriptedPoller::new(vec![PollClassification::ReadyToResume]);
    let summary1 = run_single_target_once(&target, &q, &c, &l, &poller1);
    assert_eq!(summary1.polls_applied, 1);
    assert_eq!(
        summary1.suspended, 1,
        "resume that suspends again counts as suspended"
    );
    let lease = get_lease_for_issue(&c, "o/r", 133).unwrap().unwrap();
    assert_eq!(
        lease.status,
        LeaseStatus::WaitingExternal,
        "a resumed run that suspends again must keep a resumable lease"
    );

    // The ReadyToResume poll deleted the old wait row; the production
    // resume path writes a replacement via `persist_external_wait`. The
    // lease is currently `WaitingExternal` (set by
    // `finish_lease_after_result`), so `persist_external_wait`'s
    // conditional update will accept it.
    simulate_replacement_wait_row(&c, "run-repeat", &lease.lease_id, 133);

    // The replacement wait row must be pollable on the next pass.
    let poller2 = ScriptedPoller::new(vec![PollClassification::StillWaiting]);
    let summary2 = run_single_target_once(&target, &q, &c, &l, &poller2);
    assert_eq!(
        summary2.polls_applied, 1,
        "the replacement wait row must be pollable on the next pass"
    );
}

#[test]
fn terminal_poll_does_not_regress_to_waiting_on_concurrent_suspend() {
    // Issue 131 invariant (interleaving): if the poller marks a run
    // terminal and a stale launcher then tries to set the lease
    // WaitingExternal, the conditional update must prevent the regression.
    let c = conn();
    seed_external_wait(&c, "run-interleave", 135);
    let lease = get_lease_for_issue(&c, "o/r", 135).unwrap().unwrap();

    // Simulate the poller classifying the run as terminal.
    crate::persistence::leases::update_lease_status_conditional(
        &c,
        &lease.lease_id,
        LeaseStatus::Failed,
        &[LeaseStatus::WaitingExternal, LeaseStatus::Running],
        Some("run-interleave"),
        None,
    )
    .unwrap();

    // Now simulate a stale launcher trying to set it WaitingExternal
    // (as if the engine was still running when the poller classified).
    let applied = crate::persistence::leases::update_lease_status_conditional(
        &c,
        &lease.lease_id,
        LeaseStatus::WaitingExternal,
        &[LeaseStatus::Running, LeaseStatus::WaitingExternal],
        Some("run-interleave"),
        None,
    )
    .unwrap();

    assert!(
        !applied,
        "a terminal lease must not regress to waiting_external"
    );
    let final_lease = get_lease_for_issue(&c, "o/r", 135).unwrap().unwrap();
    assert_eq!(final_lease.status, LeaseStatus::Failed);
}

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
    let _env_guard = super::super::test_env::ArtifactEnvGuard::lock_and_set(&blocked_root);

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
    let _env_guard = super::super::test_env::ArtifactEnvGuard::lock_and_set(&blocked_artifact_root);

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
    let _env_guard = super::super::test_env::ArtifactEnvGuard::lock_and_set(&blocked_root);

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
