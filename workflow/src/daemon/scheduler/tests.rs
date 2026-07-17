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

mod artifacts;
mod poll_skips;
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
        approval_label: None,
        approval_actor: None,
        claim_assignee: None,
        claim_label: None,
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
        assignees: vec![],
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
    crate::persistence::claim_metadata::init_claim_metadata_table(&c).unwrap();
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
fn seed_claim_receipt(conn: &Connection, lease_id: &str) {
    crate::persistence::claim_metadata::upsert_claim_metadata(
        conn,
        &crate::persistence::claim_metadata::ClaimMetadataReceipt {
            lease_id: lease_id.to_string(),
            assignee: String::new(),
            label: String::new(),
            assignment_added: false,
            label_added: false,
            cleanup_pending: false,
        },
    )
    .unwrap();
}

/// Seed a complete, pollable external wait using the production-path
/// `persist_external_wait` function, establishing the full invariant:
/// run status, checkpoint, wait_states row, and waiting lease.
fn seed_external_wait(conn: &Connection, run_id: &str, issue_number: u64) {
    let lease = try_claim(conn, "o/r", issue_number, "cfg")
        .unwrap()
        .unwrap();
    seed_claim_receipt(conn, &lease.lease_id);
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

/// Simulate the production engine persisting a suspended external wait after
/// the mock launcher returns `SuspendedExternalWait`. Writes the run metadata,
/// checkpoint, and wait_states row that the real engine would persist, then
/// delegates to [`simulate_replacement_wait_row`] for the wait_states row.
fn persist_engine_suspended_wait(
    conn: &Connection,
    run_id: &str,
    lease_id: &str,
    issue_number: u64,
) {
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
    simulate_replacement_wait_row(conn, run_id, lease_id, issue_number);
}

struct InterruptedClaimQuery {
    issue: Mutex<GithubIssue>,
}

impl GithubIssueQuery for InterruptedClaimQuery {
    fn list_issues(
        &self,
        _repo: &str,
        _labels: &[String],
        _states: &[String],
    ) -> Result<Vec<GithubIssue>, GithubError> {
        Ok(Vec::new())
    }

    fn has_open_pr_for_issue(&self, _repo: &str, _number: u64) -> Result<bool, GithubError> {
        Ok(false)
    }

    fn list_milestones(&self, _repo: &str) -> Result<Vec<String>, GithubError> {
        Ok(Vec::new())
    }

    fn get_issue(&self, _repo: &str, _number: u64) -> Result<Option<GithubIssue>, GithubError> {
        Ok(Some(self.issue.lock().unwrap().clone()))
    }

    fn remove_assignee(&self, _repo: &str, _number: u64, login: &str) -> Result<(), GithubError> {
        self.issue
            .lock()
            .unwrap()
            .assignees
            .retain(|value| !value.eq_ignore_ascii_case(login));
        Ok(())
    }
}

#[test]
fn scheduler_reconciles_interrupted_claim_and_makes_issue_reclaimable() {
    use crate::persistence::claim_metadata::{
        get_claim_metadata, upsert_claim_metadata, ClaimMetadataReceipt,
    };

    let c = conn();
    let lease = try_claim(&c, "o/r", 136, "cfg").unwrap().unwrap();
    upsert_claim_metadata(
        &c,
        &ClaimMetadataReceipt {
            lease_id: lease.lease_id,
            assignee: "acoliver".to_owned(),
            label: "Luther working".to_owned(),
            assignment_added: true,
            label_added: false,
            cleanup_pending: true,
        },
    )
    .unwrap();
    let query = InterruptedClaimQuery {
        issue: Mutex::new(GithubIssue {
            number: 136,
            title: "interrupted claim".to_owned(),
            state: "open".to_owned(),
            labels: vec!["OK for Luther".to_owned()],
            assignees: vec!["reviewer".to_owned(), "acoliver".to_owned()],
            milestone: None,
            body: None,
        }),
    };
    let launcher = MockLauncher {
        launched: Mutex::new(Vec::new()),
    };
    let target = pollable_target();
    let poller = ScriptedPoller::new(Vec::new());

    run_single_target_once(&target, &query, &c, &launcher, &poller);

    assert_eq!(
        query.issue.lock().unwrap().assignees,
        ["reviewer".to_owned()]
    );
    let lease = get_lease_for_issue(&c, "o/r", 136).unwrap().unwrap();
    assert_eq!(lease.status, LeaseStatus::Abandoned);
    assert!(
        !get_claim_metadata(&c, &lease.lease_id)
            .unwrap()
            .unwrap()
            .cleanup_pending
    );
    assert!(try_claim(&c, "o/r", 136, "cfg").unwrap().is_some());
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

// ---- Issue-137: launch->wait->poll->resume lifecycle with zero/one claim fields ----

/// A launcher that suspends on first launch and completes on resume, modelling
/// the external-wait lifecycle. It records the resume request so tests can
/// verify the claim ownership flags were reconstructed from the receipt.
struct SuspendThenCompleteLauncher {
    resume_requests: Mutex<Vec<LaunchRequest>>,
}

impl SuspendThenCompleteLauncher {
    fn new() -> Self {
        Self {
            resume_requests: Mutex::new(Vec::new()),
        }
    }
}

impl WorkflowLauncher for SuspendThenCompleteLauncher {
    fn launch(&self, _request: &LaunchRequest) -> Result<WorkflowLaunchResult, String> {
        Ok(WorkflowLaunchResult::SuspendedExternalWait)
    }
    fn resume(&self, request: &LaunchRequest) -> Result<WorkflowLaunchResult, String> {
        self.resume_requests.lock().unwrap().push(request.clone());
        Ok(WorkflowLaunchResult::CompletedSuccess)
    }
}

/// Build a [`DiscoveryConfig`] with specified claim fields and `max_concurrent_runs`.
fn cfg_with_claim(
    max: u32,
    claim_assignee: Option<&str>,
    claim_label: Option<&str>,
) -> DiscoveryConfig {
    DiscoveryConfig {
        claim_assignee: claim_assignee.map(str::to_string),
        claim_label: claim_label.map(str::to_string),
        ..cfg(max)
    }
}

/// Build a scheduler target for a config with the given claim fields.
fn target_with_claim(
    config_id: &str,
    max: u32,
    claim_assignee: Option<&str>,
    claim_label: Option<&str>,
) -> SchedulerTarget {
    SchedulerTarget::new(
        config_id.to_string(),
        cfg_with_claim(max, claim_assignee, claim_label),
        DaemonPathBases::default(),
        BTreeMap::new(),
    )
}

/// Verify the complete launch->wait->poll->resume lifecycle works for a
/// config with the given claim fields. The receipt must be persisted at claim
/// time so the resume path can reconstruct ownership, regardless of how many
/// claim fields are configured.
fn run_launch_wait_poll_resume_lifecycle(
    claim_assignee: Option<&str>,
    claim_label: Option<&str>,
    issue_number: u64,
) {
    let c = conn();
    let q = MockQuery {
        issues: vec![issue(issue_number)],
    };
    let launcher = SuspendThenCompleteLauncher::new();
    let target = target_with_claim("cfg", 1, claim_assignee, claim_label);

    // Pass 1: launch -> suspend at external wait.
    let poller_suspend = ScriptedPoller::new(Vec::new());
    let summary1 = run_single_target_once(&target, &q, &c, &launcher, &poller_suspend);
    assert_eq!(
        summary1.launched + summary1.suspended,
        1,
        "first pass must launch exactly one issue"
    );
    assert_eq!(
        summary1.suspended, 1,
        "first pass must suspend at the external wait"
    );

    // The lease must now be WaitingExternal and have a persisted receipt
    // (even with zero/one claim fields).
    let lease = get_lease_for_issue(&c, "o/r", issue_number)
        .unwrap()
        .expect("lease must exist after launch");
    assert_eq!(lease.status, LeaseStatus::WaitingExternal);
    let run_id = lease.run_id.clone().expect("run_id must be set");
    let receipt = crate::persistence::claim_metadata::get_claim_metadata(&c, &lease.lease_id)
        .unwrap()
        .expect("a receipt must be persisted for the lease");
    assert_eq!(receipt.lease_id, lease.lease_id);

    // Simulate the production engine persisting the external wait. The mock
    // launcher cannot do this, so persist_engine_suspended_wait writes the
    // run metadata, checkpoint, and wait_states row.
    persist_engine_suspended_wait(&c, &run_id, &lease.lease_id, issue_number);

    // Pass 2: poll classifies ready-to-resume -> resume -> complete.
    let summary2 = run_single_target_once(
        &target,
        &q,
        &c,
        &launcher,
        &ScriptedPoller::new(vec![PollClassification::ReadyToResume]),
    );
    assert_eq!(
        summary2.polls_applied, 1,
        "the external wait must be polled"
    );
    assert_eq!(
        summary2.resumed, 1,
        "the resumed run must complete successfully"
    );

    // The resume request must carry the reconstructed claim ownership flags.
    let resume_requests = launcher.resume_requests.lock().unwrap();
    assert_eq!(resume_requests.len(), 1, "exactly one resume must occur");
    assert_eq!(resume_requests[0].run_id, run_id);
    assert!(
        resume_requests[0].daemon_managed_claim,
        "resume request must mark the claim as daemon-managed"
    );

    // The lease must now be Completed.
    let final_lease = get_lease_for_issue(&c, "o/r", issue_number)
        .unwrap()
        .expect("lease must exist after resume");
    assert_eq!(
        final_lease.status,
        LeaseStatus::Completed,
        "the lease must reach Completed after a successful resume"
    );
}

#[test]
fn launch_wait_poll_resume_with_zero_claim_fields() {
    // Issue-137: the full launch->wait->poll->resume lifecycle must work when
    // the config has no claim_assignee and no claim_label. Previously,
    // acquire_lease_with_receipt skipped persisting a receipt when neither
    // field was set, so prepare_resume_lease would skip the resume and strand
    // the lease permanently in WaitingExternal/ReadyToResume.
    run_launch_wait_poll_resume_lifecycle(None, None, 160);
}

#[test]
fn launch_wait_poll_resume_with_one_claim_field() {
    // Issue-137: the full launch->wait->poll->resume lifecycle must work when
    // the config has exactly one optional claim field (assignee but no label).
    run_launch_wait_poll_resume_lifecycle(Some("acoliver"), None, 161);
}
