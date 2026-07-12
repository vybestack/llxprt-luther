use super::*;
use crate::persistence::leases::{get_lease_for_issue, init_leases_table, try_claim, LeaseStatus};
use crate::persistence::sqlite::{init_runs_schema, persist_run_with_conn};
use crate::persistence::wait_state::{get_wait_state, init_wait_states_table, upsert_wait_state};
use chrono::Duration;
use rusqlite::Connection;
use serde_json::json;

fn conn() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    crate::persistence::sqlite::init_runs_schema(&c).unwrap();
    init_leases_table(&c).unwrap();
    init_wait_states_table(&c).unwrap();
    c
}

fn wait_record(c: &Connection) -> WaitStateRecord {
    let lease = try_claim(c, "o/r", 62, "cfg").unwrap().unwrap();
    // The poller only runs against leases in a waiting/ready state. Seed
    // WaitingExternal so the conditional lease updates in apply_poll_decision
    // match the production invariant.
    crate::persistence::leases::update_lease_status(
        c,
        &lease.lease_id,
        crate::persistence::leases::LeaseStatus::WaitingExternal,
        Some("run-62"),
    )
    .unwrap();
    let mut record = WaitStateRecord::new("run-62", "cfg");
    record.lease_id = Some(lease.lease_id);
    record.repository = "o/r".to_string();
    record.issue_number = 62;
    record.pr_number = Some(7);
    record.head_sha = Some("head-a".to_string());
    record.wait_condition = json!({
        "check_policy": {
            "required": [{ "mode": "exact", "pattern": "ci" }],
            "missing_retry_attempts": 1,
            "api_error_retry_attempts": 1,
            "poll_interval_seconds": 1
        }
    });
    record.poll_interval_seconds = 1;
    record.resume_step = "watch_pr_checks".to_string();
    crate::persistence::checkpoint::save_checkpoint_with_conn(
        c,
        &crate::persistence::checkpoint::Checkpoint::new(&record.run_id, &record.resume_step),
    )
    .unwrap();
    let mut metadata = crate::persistence::RunMetadata::new(&record.run_id, "wf", "cfg");
    metadata.status = RunStatus::WaitingExternal;
    metadata.set_current_step(record.resume_step.clone());
    crate::persistence::persist_run_with_conn(c, &metadata).unwrap();
    record
}

struct ScriptedPrCheckRunner {
    responses: std::sync::Mutex<Vec<Result<String, GithubError>>>,
}

impl ScriptedPrCheckRunner {
    fn new(responses: Vec<Result<String, GithubError>>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses),
        }
    }
}

impl GithubCommandRunner for ScriptedPrCheckRunner {
    fn run(&self, argv: &[String]) -> Result<String, GithubError> {
        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
            let script = argv.join(" ");
            return Err(GithubError::CommandFailed {
                argv: argv.to_vec(),
                exit_code: None,
                stderr: format!(
                    "ScriptedPrCheckRunner exhausted: no more scripted responses for '{script}'. \
                     Add more responses to the test fixture or reduce the number of poll calls."
                ),
            });
        }
        responses.remove(0)
    }
}

#[test]
fn still_waiting_updates_backoff_without_active_lease() {
    let c = conn();
    let record = wait_record(&c);
    upsert_wait_state(&c, &record).unwrap();
    crate::persistence::leases::update_lease_status(
        &c,
        record.lease_id.as_deref().unwrap(),
        LeaseStatus::WaitingExternal,
        Some(&record.run_id),
    )
    .unwrap();
    // Use a distinctive observed_state so we can verify it is persisted
    // exactly — not silently replaced with null or a different value.
    let decision = PollDecision::still_waiting_with_state(
        &record,
        json!({ "classification": "still_waiting", "poller": "test-marker" }),
    );
    apply_poll_decision(&c, &record, &decision).unwrap();
    let updated = get_wait_state(&c, "run-62").unwrap().unwrap();
    assert_eq!(updated.poll_count, 1);
    // Read back the persisted last_observed_state_json to confirm it equals
    // the original JSON — proving no silent data corruption occurred.
    assert_eq!(
        updated.last_observed_state,
        json!({ "classification": "still_waiting", "poller": "test-marker" }),
        "persisted last_observed_state_json must equal the original decision observed_state"
    );
    let lease = get_lease_for_issue(&c, "o/r", 62).unwrap().unwrap();
    assert_eq!(lease.status, LeaseStatus::WaitingExternal);
}

#[test]
fn apply_poll_decision_rejects_an_active_outer_transaction() {
    let c = conn();
    let record = wait_record(&c);
    upsert_wait_state(&c, &record).unwrap();
    let decision = PollDecision::still_waiting(&record);
    let outer = c.unchecked_transaction().unwrap();

    let error = apply_poll_decision(&c, &record, &decision).unwrap_err();

    assert!(matches!(error, PollApplyError::Sqlite(_)));
    outer.rollback().unwrap();
    assert_eq!(
        get_wait_state(&c, &record.run_id)
            .unwrap()
            .unwrap()
            .poll_count,
        0,
        "a rejected nested transaction must not apply the poll decision"
    );
}

#[test]
fn ready_decision_marks_lease_ready_to_resume() {
    let c = conn();
    let record = wait_record(&c);
    upsert_wait_state(&c, &record).unwrap();
    let decision = PollDecision::ready(&record, json!({ "checks": "success" }));
    apply_poll_decision(&c, &record, &decision).unwrap();
    let lease = get_lease_for_issue(&c, "o/r", 62).unwrap().unwrap();
    assert_eq!(lease.status, LeaseStatus::ReadyToResume);
    assert!(get_wait_state(&c, "run-62").unwrap().is_none());
}

#[test]
fn terminal_failure_marks_lease_failed_and_cleans_up_wait_state() {
    let c = conn();
    let record = wait_record(&c);
    upsert_wait_state(&c, &record).unwrap();
    let decision = PollDecision {
        run_id: record.run_id.clone(),
        classification: PollClassification::TerminalFailure,
        next_poll_at: None,
        observed_state: json!({ "state": "closed" }),
    };

    apply_poll_decision(&c, &record, &decision).unwrap();

    let lease = get_lease_for_issue(&c, "o/r", 62).unwrap().unwrap();
    assert_eq!(lease.status, LeaseStatus::Failed);
    assert!(get_wait_state(&c, "run-62").unwrap().is_none());
}

#[test]
fn timed_out_marks_lease_failed_and_cleans_up_wait_state() {
    let c = conn();
    let record = wait_record(&c);
    upsert_wait_state(&c, &record).unwrap();
    let decision = PollDecision {
        run_id: record.run_id.clone(),
        classification: PollClassification::TimedOut,
        next_poll_at: None,
        observed_state: json!({ "state": "timed_out" }),
    };

    apply_poll_decision(&c, &record, &decision).unwrap();

    let lease = get_lease_for_issue(&c, "o/r", 62).unwrap().unwrap();
    assert_eq!(lease.status, LeaseStatus::Failed);
    assert!(get_wait_state(&c, "run-62").unwrap().is_none());
}

#[test]
fn transient_failure_requires_existing_wait_state() {
    let c = conn();
    let record = wait_record(&c);
    let decision = PollDecision {
        run_id: record.run_id.clone(),
        classification: PollClassification::TransientFailure,
        next_poll_at: None,
        observed_state: json!({ "state": "api_error" }),
    };

    let err = apply_poll_decision(&c, &record, &decision).unwrap_err();
    assert!(
        matches!(
            err,
            PollApplyError::WaitStateConcurrentTransition(ref run_id)
                if run_id == &record.run_id
        ),
        "missing wait-state should surface as a concurrent-transition error: {err}"
    );
}
#[test]
fn still_waiting_missing_wait_state_does_not_write_poll_artifact() {
    let c = conn();
    let mut record = wait_record(&c);
    record.run_id = "run-no-artifact".to_string();
    // Persist run metadata for the renamed run so the failure is specifically
    // the missing wait_states row, not a missing run (which is a distinct
    // RunMissing integrity error).
    let mut metadata = crate::persistence::RunMetadata::new(&record.run_id, "wf", "cfg");
    metadata.status = RunStatus::WaitingExternal;
    metadata.set_current_step(record.resume_step.clone());
    crate::persistence::persist_run_with_conn(&c, &metadata).unwrap();
    crate::persistence::checkpoint::save_checkpoint_with_conn(
        &c,
        &crate::persistence::checkpoint::Checkpoint::new(&record.run_id, &record.resume_step),
    )
    .unwrap();
    let artifact_root = tempfile::tempdir().unwrap();
    // The combined lock+set ensures the env mutation is serialized across all
    // tests and restored on drop — even if the test body panics.
    let _env_guard = super::super::test_env::ArtifactEnvGuard::lock_and_set(artifact_root.path());
    let decision = PollDecision::still_waiting(&record);

    let err = apply_poll_decision(&c, &record, &decision).unwrap_err();

    assert!(
        matches!(
            err,
            PollApplyError::WaitStateConcurrentTransition(ref run_id)
                if run_id == &record.run_id
        ),
        "missing wait-state should surface as a concurrent-transition error: {err}"
    );

    let run_dir = artifact_root.path().join(&record.run_id);
    assert!(
        !run_dir.exists(),
        "poll artifacts should not be written before the wait-state update succeeds"
    );
}

#[test]
fn action_required_pr_check_is_terminal() {
    let record = wait_record(&conn());
    let decision = classify_terminal_or_pending(
        &record,
        vec![json!({ "name": "ci", "conclusion": "action_required" })],
    );
    assert_eq!(decision.classification, PollClassification::ReadyToResume);
}

#[test]
fn terminal_pr_checks_are_ready_to_resume() {
    let record = wait_record(&conn());
    let decision = classify_terminal_or_pending(
        &record,
        vec![json!({ "name": "ci", "state": "success", "bucket": "pass" })],
    );
    assert_eq!(decision.classification, PollClassification::ReadyToResume);
}

#[test]
fn failed_pr_checks_are_ready_to_resume() {
    let record = wait_record(&conn());
    let decision = classify_terminal_or_pending(
        &record,
        vec![json!({ "name": "ci", "state": "failure", "bucket": "fail" })],
    );
    assert_eq!(decision.classification, PollClassification::ReadyToResume);
}

#[test]
fn unknown_pr_checks_are_terminal_failure() {
    let record = wait_record(&conn());
    let decision = classify_terminal_or_pending(
        &record,
        vec![json!({ "name": "ci", "state": "mystery", "bucket": "unknown" })],
    );
    assert_eq!(decision.classification, PollClassification::TerminalFailure);
}

#[test]
fn pending_pr_checks_keep_waiting() {
    let record = wait_record(&conn());
    let decision = classify_terminal_or_pending(
        &record,
        vec![json!({ "name": "ci", "state": "in_progress", "bucket": "pending" })],
    );
    assert_eq!(decision.classification, PollClassification::StillWaiting);
}

#[test]
fn later_passing_public_checks_resume_after_pending_snapshot() {
    let c = conn();
    let mut record = wait_record(&c);
    let artifact_root = tempfile::tempdir().unwrap();
    record.wait_condition["artifact_root"] =
        json!(artifact_root.path().to_string_lossy().to_string());
    record.wait_condition["head_ref"] = json!("feature");
    record.wait_condition["base_ref"] = json!("main");
    record.wait_condition["base_sha"] = json!("base-a");
    record.last_observed_state = json!({
        "overall_state": "pending_timeout",
        "poll_state": {
            "missing_attempts": 0,
            "api_error_attempts": 0,
            "poll_attempts": 1
        },
        "checks": [{
            "name": "ci",
            "state": "in_progress",
            "bucket": "pending",
            "head_sha": "head-a"
        }]
    });
    upsert_wait_state(&c, &record).unwrap();
    let runner = ScriptedPrCheckRunner::new(vec![
        Ok(json!([{ "name": "ci", "state": "SUCCESS", "bucket": "pass" }]).to_string()),
        Ok(json!({ "total_count": 0, "check_runs": [] }).to_string()),
    ]);
    let poller = SystemExternalWaitPoller::with_runner(runner);

    let decision = poller.poll(&record);
    apply_poll_decision(&c, &record, &decision).unwrap();

    assert_eq!(decision.classification, PollClassification::ReadyToResume);
    assert_eq!(
        decision
            .observed_state
            .get("overall_state")
            .and_then(Value::as_str),
        Some("passed"),
        "a later passing public check snapshot must override an earlier pending_timeout",
    );
    assert_eq!(
        decision
            .observed_state
            .pointer("/poll_state/poll_attempts")
            .and_then(Value::as_u64),
        Some(2),
        "daemon polls must carry the durable poll counter forward",
    );
    assert!(
        get_wait_state(&c, &record.run_id).unwrap().is_none(),
        "ready PR checks should delete the durable wait state",
    );
    let lease = get_lease_for_issue(&c, &record.repository, record.issue_number)
        .unwrap()
        .expect("lease");
    assert_eq!(lease.status, LeaseStatus::ReadyToResume);
    let status_path = artifact_root
        .path()
        .join("pr-followup")
        .join("current")
        .join(&record.run_id)
        .join("o")
        .join("r")
        .join("7")
        .join("pr-check-status.json");
    assert!(
        status_path.exists(),
        "ready PR check polling should persist a pr-check-status snapshot"
    );
}

#[test]
fn timed_out_wait_returns_timeout_decision_before_polling() {
    let mut record = wait_record(&conn());
    record.max_wait_seconds = Some(1);
    record.created_at = Utc::now() - Duration::seconds(5);

    let decision = timeout_decision(&record).expect("expired wait should time out");

    assert_eq!(decision.classification, PollClassification::TimedOut);
    assert_eq!(
        decision
            .observed_state
            .get("classification")
            .and_then(Value::as_str),
        Some("timed_out")
    );
}
#[test]
fn dependency_child_workflow_observes_child_run_id() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("checkpoints.db");
    let c = Connection::open(&db_path).unwrap();
    init_runs_schema(&c).unwrap();
    let mut child =
        crate::persistence::RunMetadata::new("child-run-63", "llxprt-issue-fix-v1", "cfg");
    child.status = RunStatus::WaitingExternal;
    child.set_current_step("watch_pr_checks");
    persist_run_with_conn(&c, &child).unwrap();
    let mut record = WaitStateRecord::new("parent-run-62", "parent-cfg");
    record.wait_kind = WaitKind::DependencyChildWorkflow;
    record.head_sha = Some("child-run-63".to_string());

    // The DependencyChildWorkflow wait kind reads child-run status from the
    // shared checkpoint DB, never invoking the gh command runner. An empty
    // script is intentional: if the runner were accidentally called the
    // ScriptedPrCheckRunner would return an error (not a panic), surfacing
    // the misuse. The child is initially WaitingExternal so the first poll
    // must classify StillWaiting.
    let poller = SystemExternalWaitPoller::with_runner_and_db_path(
        ScriptedPrCheckRunner::new(Vec::new()),
        db_path,
    );
    let waiting = poller.poll(&record);
    assert_eq!(waiting.classification, PollClassification::StillWaiting);
    assert_eq!(
        waiting
            .observed_state
            .get("child_run_id")
            .and_then(Value::as_str),
        Some("child-run-63")
    );

    let mut child = get_run_with_conn(&c, "child-run-63").unwrap().unwrap();
    child.status = RunStatus::ReadyToResume;
    persist_run_with_conn(&c, &child).unwrap();

    let ready = poller.poll(&record);
    assert_eq!(ready.classification, PollClassification::ReadyToResume);
    assert_eq!(
        ready
            .observed_state
            .get("child_status")
            .and_then(Value::as_str),
        Some("ready_to_resume")
    );
}

#[test]
fn pr_merge_closed_without_merge_is_terminal_failure() {
    let record = wait_record(&conn());

    let decision = classify_pr_merge_state(&record, json!({ "state": "CLOSED" }));

    assert_eq!(decision.classification, PollClassification::TerminalFailure);
}

#[test]
fn coderabbit_completion_notice_is_ready() {
    let state = json!({
        "issue_comments": [{
            "user": {"login": "coderabbitai[bot]"},
            "body": "CodeRabbit finished reviewing this pull request"
        }],
        "review_comments": []
    });
    assert!(coderabbit_is_ready(&state));
}

#[test]
fn coderabbit_rate_limit_notice_is_not_ready() {
    let state = json!({
        "issue_comments": [{
            "user": {"login": "coderabbitai[bot]"},
            "body": "CodeRabbit review limit reached for this repository"
        }],
        "review_comments": []
    });
    assert!(!coderabbit_is_ready(&state));
}

#[test]
fn coderabbit_error_notice_with_completion_text_is_not_ready() {
    let state = json!({
        "issue_comments": [{
            "user": {"login": "coderabbitai[bot]"},
            "body": "Summary by CodeRabbit\nCodeRabbit finished reviewing this pull request, but the review failed due to quota limits."
        }],
        "review_comments": []
    });
    assert!(!coderabbit_is_ready(&state));
}

#[test]
fn coderabbit_error_words_from_human_comments_do_not_block_readiness() {
    let state = json!({
        "issue_comments": [
            {
                "user": {"login": "coderabbitai[bot]"},
                "body": "CodeRabbit finished reviewing this pull request"
            },
            {
                "user": {"login": "human-reviewer"},
                "body": "I saw an error in a previous failed run."
            }
        ],
        "review_comments": []
    });
    assert!(coderabbit_is_ready(&state));
}

#[test]
fn nested_comment_bodies_are_collected_when_parent_has_body() {
    let state = json!({
        "issue_comments": [{
            "body": "wrapper",
            "children": [{"body": "CodeRabbit finished reviewing"}]
        }],
        "review_comments": []
    });

    assert!(comments_text(&state).contains("coderabbit finished reviewing"));
}

#[test]
fn coderabbit_readiness_ignores_non_body_json_fields() {
    let state = json!({
        "issue_comments": [{"metadata": "CodeRabbit finished reviewing"}],
        "review_comments": []
    });
    assert!(!coderabbit_is_ready(&state));
}

#[test]
fn only_approved_human_review_is_ready() {
    assert!(review_decision_ready(
        &json!({ "reviewDecision": "APPROVED" })
    ));
    assert!(!review_decision_ready(
        &json!({ "reviewDecision": "REVIEW_REQUIRED" })
    ));
    assert!(!review_decision_ready(
        &json!({ "reviewDecision": "CHANGES_REQUESTED" })
    ));
}

// ---- Race / rejection regression tests (issue 131) ----

#[test]
fn ready_decision_rolls_back_when_lease_already_terminal() {
    // Race regression: if the poller classifies ReadyToResume but a
    // concurrent writer has already marked the lease terminal (Failed),
    // the conditional lease transition is rejected and the entire
    // transaction must roll back — the wait_states row must survive and
    // the lease must remain Failed.
    let c = conn();
    let record = wait_record(&c);
    upsert_wait_state(&c, &record).unwrap();
    // Simulate a concurrent writer racing ahead to Failed.
    crate::persistence::leases::update_lease_status(
        &c,
        record.lease_id.as_deref().unwrap(),
        LeaseStatus::Failed,
        Some(&record.run_id),
    )
    .unwrap();

    let decision = PollDecision::ready(&record, json!({ "checks": "success" }));
    let result = apply_poll_decision(&c, &record, &decision);
    assert!(
        result.is_err(),
        "ready decision must error when lease is already terminal"
    );

    // The wait_states row must survive (transaction rolled back).
    assert!(
        get_wait_state(&c, &record.run_id).unwrap().is_some(),
        "wait_states row must survive a rejected ready transition"
    );
    // The lease must remain Failed.
    let lease = get_lease_for_issue(&c, "o/r", 62).unwrap().unwrap();
    assert_eq!(
        lease.status,
        LeaseStatus::Failed,
        "a terminal lease must not be overwritten by a stale ready decision"
    );
}

#[test]
fn terminal_failure_rolls_back_when_lease_already_terminal() {
    // Race regression: if the poller classifies TerminalFailure but the
    // lease is already terminal from a concurrent writer, the
    // transaction must roll back and the wait_states row must survive.
    let c = conn();
    let record = wait_record(&c);
    upsert_wait_state(&c, &record).unwrap();
    // Race ahead to Completed (a different terminal state).
    crate::persistence::leases::update_lease_status(
        &c,
        record.lease_id.as_deref().unwrap(),
        LeaseStatus::Completed,
        Some(&record.run_id),
    )
    .unwrap();

    let decision = PollDecision {
        run_id: record.run_id.clone(),
        classification: PollClassification::TerminalFailure,
        next_poll_at: None,
        observed_state: json!({ "state": "closed" }),
    };
    let result = apply_poll_decision(&c, &record, &decision);
    assert!(
        result.is_err(),
        "terminal decision must error when lease is already terminal"
    );
    assert!(
        get_wait_state(&c, &record.run_id).unwrap().is_some(),
        "wait_states row must survive a rejected terminal transition"
    );
    let lease = get_lease_for_issue(&c, "o/r", 62).unwrap().unwrap();
    assert_eq!(lease.status, LeaseStatus::Completed);
}

#[test]
fn still_waiting_rolls_back_when_lease_already_ready() {
    // Race regression: if the poller says StillWaiting but a concurrent
    // writer has already advanced the lease to ReadyToResume, the
    // still-waiting update must not regress it. The transaction rolls
    // back.
    let c = conn();
    let record = wait_record(&c);
    upsert_wait_state(&c, &record).unwrap();
    // Race ahead to ReadyToResume.
    crate::persistence::leases::update_lease_status(
        &c,
        record.lease_id.as_deref().unwrap(),
        LeaseStatus::ReadyToResume,
        Some(&record.run_id),
    )
    .unwrap();

    let decision = PollDecision::still_waiting(&record);
    let result = apply_poll_decision(&c, &record, &decision);
    assert!(
        result.is_err(),
        "still-waiting decision must error when lease is already ready"
    );
    // The wait_states row must survive (transaction rolled back).
    assert!(
        get_wait_state(&c, &record.run_id).unwrap().is_some(),
        "wait_states row must survive a rejected still-waiting transition"
    );
    let lease = get_lease_for_issue(&c, "o/r", 62).unwrap().unwrap();
    assert_eq!(
        lease.status,
        LeaseStatus::ReadyToResume,
        "a ReadyToResume lease must not regress to WaitingExternal"
    );
}

#[test]
fn missing_run_metadata_surfaces_as_run_missing_not_concurrent_transition() {
    // Integrity regression: a pollable wait-state with no backing run
    // metadata is an integrity failure, not a benign concurrent transition.
    // The error variant must be RunMissing so the scheduler can surface it.
    let c = conn();
    let record = wait_record(&c);
    upsert_wait_state(&c, &record).unwrap();
    // Delete the run metadata row, simulating a corrupted or partially
    // cleaned-up state where the wait-state outlived the run.
    c.execute(
        "DELETE FROM runs WHERE run_id = ?1",
        rusqlite::params![record.run_id],
    )
    .unwrap();

    let decision = PollDecision::still_waiting(&record);
    let err = apply_poll_decision(&c, &record, &decision).unwrap_err();
    assert!(
        matches!(err, PollApplyError::RunMissing { ref run_id, ref step_id } if run_id == &record.run_id && step_id == &record.resume_step),
        "missing run metadata must surface as RunMissing, got: {err}"
    );
}

#[test]
fn non_object_observed_state_is_not_corrupted_by_artifact_error() {
    // The post-commit artifact-error annotation must not panic or corrupt
    // non-object observed_state values (String, Number, Bool, Array). The
    // observed_state should survive unchanged when it cannot be annotated.
    let c = conn();
    let mut record = wait_record(&c);
    // A regular file cannot contain snapshot directories on any supported
    // platform, so this deterministically forces the snapshot write to fail.
    let artifact_temp = tempfile::tempdir().expect("create artifact test directory");
    let blocked_root = artifact_temp.path().join("not-a-directory");
    std::fs::write(&blocked_root, b"file blocks directory creation")
        .expect("create deterministic artifact path blocker");
    record.wait_kind = WaitKind::PrChecks;
    record.wait_condition = json!({
        "artifact_root": blocked_root,
        "head_ref": "feature",
        "base_ref": "main",
        "base_sha": "base-a",
        "check_policy": {
            "required": [{ "mode": "exact", "pattern": "ci" }],
            "missing_retry_attempts": 1,
            "api_error_retry_attempts": 1,
            "poll_interval_seconds": 1
        }
    });
    upsert_wait_state(&c, &record).unwrap();
    // Use a non-object observed_state (a JSON string) that would panic
    // under unguarded index-assignment.
    let decision = PollDecision {
        run_id: record.run_id.clone(),
        classification: PollClassification::StillWaiting,
        next_poll_at: None,
        observed_state: json!("a string, not an object"),
    };
    // This must not panic even though the PR-check snapshot write fails and
    // the observed_state is a non-object scalar.
    let result = apply_poll_decision(&c, &record, &decision);
    assert!(
        result.is_ok(),
        "apply_poll_decision must return Ok without corrupting non-object observed_state: {result:?}"
    );

    // Strengthen: read back the persisted wait-state and confirm the
    // observed_state was not silently corrupted to null or an object by
    // the artifact-error annotation path. The committed DB value must
    // equal the original non-object string.
    let persisted = get_wait_state(&c, &record.run_id)
        .expect("read wait-state")
        .expect("wait-state must survive a successful still-waiting apply");
    assert_eq!(
        persisted.last_observed_state,
        json!("a string, not an object"),
        "persisted observed_state must equal the original non-object string,          not null or an object — the artifact-error annotation must not          corrupt committed DB state"
    );
}

#[test]
fn still_waiting_rejects_running_lease_instead_of_regressing() {
    // OCR 3565653889: a still-waiting poll must not regress a Running lease
    // to WaitingExternal. The expected-status list is WaitingExternal only,
    // so a Running lease must produce LeaseTransitionRejected.
    let c = conn();
    let record = wait_record(&c);
    upsert_wait_state(&c, &record).unwrap();
    // Advance the lease to Running — the engine is actively executing.
    crate::persistence::leases::update_lease_status(
        &c,
        record.lease_id.as_deref().unwrap(),
        LeaseStatus::Running,
        Some(&record.run_id),
    )
    .unwrap();

    let decision = PollDecision::still_waiting(&record);
    let err = apply_poll_decision(&c, &record, &decision).unwrap_err();
    assert!(
        matches!(err, PollApplyError::LeaseTransitionRejected { .. }),
        "a Running lease must not regress to WaitingExternal: {err}"
    );
    // The lease must remain Running.
    let lease = get_lease_for_issue(&c, "o/r", 62).unwrap().unwrap();
    assert_eq!(
        lease.status,
        LeaseStatus::Running,
        "a Running lease must not be pulled back to WaitingExternal"
    );
}

// ---- mark_run_status conditional/atomic race tests (OCR 3565693837) ----

#[test]
fn stale_poller_status_update_rejected_when_run_already_terminal() {
    // Race regression for mark_run_status conditional guard: if a stale
    // poller's apply_poll_decision tries to write WaitingExternal but a
    // concurrent writer has already advanced the run to a terminal status
    // (e.g. Completed), the conditional UPDATE must reject the write. The
    // error surfaces as RunStatusConcurrentTransition and the run's status
    // must remain the terminal value.
    let c = conn();
    let record = wait_record(&c);
    upsert_wait_state(&c, &record).unwrap();
    // Simulate a concurrent writer racing ahead to Completed (terminal).
    let mut terminal = crate::persistence::RunMetadata::new(&record.run_id, "wf", "cfg");
    terminal.status = RunStatus::Completed;
    terminal.set_current_step(record.resume_step.clone());
    crate::persistence::sqlite::persist_run_with_conn(&c, &terminal).unwrap();

    let decision = PollDecision::still_waiting(&record);
    let err = apply_poll_decision(&c, &record, &decision).unwrap_err();
    assert!(
        matches!(err, PollApplyError::RunStatusConcurrentTransition { ref run_id, ref step_id } if run_id == &record.run_id && step_id == &record.resume_step),
        "stale status update on a terminal run must be rejected as \
         RunStatusConcurrentTransition, got: {err}"
    );
    // The run must remain Completed — the stale poller must not regress it.
    let persisted_run = crate::persistence::sqlite::get_run_with_conn(&c, &record.run_id)
        .unwrap()
        .expect("run must exist");
    assert_eq!(
        persisted_run.status,
        RunStatus::Completed,
        "a terminal run must not be regressed by a stale poller status update"
    );
}

#[test]
fn stale_still_waiting_poller_cannot_regress_running_run_and_rolls_back() {
    let c = conn();
    let record = wait_record(&c);
    upsert_wait_state(&c, &record).unwrap();
    let before_wait = get_wait_state(&c, &record.run_id).unwrap().unwrap();

    let mut advanced = crate::persistence::sqlite::get_run_with_conn(&c, &record.run_id)
        .unwrap()
        .expect("run must exist");
    advanced.status = RunStatus::Running;
    advanced.set_current_step("execute_remediation");
    crate::persistence::sqlite::persist_run_with_conn(&c, &advanced).unwrap();

    let decision = PollDecision {
        run_id: record.run_id.clone(),
        classification: PollClassification::StillWaiting,
        next_poll_at: None,
        observed_state: json!({ "poller": "stale" }),
    };
    let err = apply_poll_decision(&c, &record, &decision).unwrap_err();
    assert!(
        matches!(err, PollApplyError::RunStatusConcurrentTransition { ref run_id, ref step_id } if run_id == &record.run_id && step_id == &record.resume_step),
        "an advanced non-terminal run must reject a stale waiting update: {err}"
    );

    let persisted_run = crate::persistence::sqlite::get_run_with_conn(&c, &record.run_id)
        .unwrap()
        .expect("run must exist");
    assert_eq!(persisted_run.status, RunStatus::Running);
    assert_eq!(
        persisted_run.current_step.as_deref(),
        Some("execute_remediation")
    );
    let after_wait = get_wait_state(&c, &record.run_id).unwrap().unwrap();
    assert_eq!(after_wait.poll_count, before_wait.poll_count);
    assert_eq!(
        after_wait.last_observed_state,
        before_wait.last_observed_state
    );
    let lease = get_lease_for_issue(&c, "o/r", record.issue_number)
        .unwrap()
        .expect("lease must exist");
    assert_eq!(lease.status, LeaseStatus::WaitingExternal);
}

#[test]
fn stale_poller_failed_update_rejected_when_run_already_completed() {
    // Race regression: a terminal-failure poll decision must not overwrite a
    // run that has already reached Completed via a concurrent writer. The
    // conditional UPDATE must reject the Failed status write.
    let c = conn();
    let record = wait_record(&c);
    upsert_wait_state(&c, &record).unwrap();
    // Race ahead to Completed (terminal).
    let mut terminal = crate::persistence::RunMetadata::new(&record.run_id, "wf", "cfg");
    terminal.status = RunStatus::Completed;
    terminal.set_current_step(record.resume_step.clone());
    crate::persistence::sqlite::persist_run_with_conn(&c, &terminal).unwrap();

    let decision = PollDecision {
        run_id: record.run_id.clone(),
        classification: PollClassification::TerminalFailure,
        next_poll_at: None,
        observed_state: json!({ "state": "closed" }),
    };
    let err = apply_poll_decision(&c, &record, &decision).unwrap_err();
    assert!(
        matches!(err, PollApplyError::RunStatusConcurrentTransition { ref run_id, ref step_id } if run_id == &record.run_id && step_id == &record.resume_step),
        "terminal-failure update on a Completed run must be rejected, got: {err}"
    );
    let persisted_run = crate::persistence::sqlite::get_run_with_conn(&c, &record.run_id)
        .unwrap()
        .expect("run must exist");
    assert_eq!(
        persisted_run.status,
        RunStatus::Completed,
        "a Completed run must not be regressed to Failed by a stale poller"
    );
}

#[test]
fn mark_run_status_allows_expected_non_terminal_transition() {
    // The explicit source-status guard must allow the legitimate
    // WaitingExternal → ReadyToResume transition.
    let c = conn();
    let record = wait_record(&c);
    upsert_wait_state(&c, &record).unwrap();

    let decision = PollDecision::ready(&record, json!({ "checks": "success" }));
    apply_poll_decision(&c, &record, &decision).unwrap();

    let persisted_run = crate::persistence::sqlite::get_run_with_conn(&c, &record.run_id)
        .unwrap()
        .expect("run must exist");
    assert_eq!(
        persisted_run.status,
        RunStatus::ReadyToResume,
        "the expected waiting-to-ready transition must succeed"
    );
}

#[test]
fn still_waiting_stale_poller_rejected_by_version_guard() {
    // Two pollers read the same WaitingExternal wait-state
    // (poll_count=0). The first poller's apply_still_waiting commits,
    // incrementing poll_count to 1. The second (stale) poller's
    // apply_still_waiting must be rejected by the optimistic poll_count
    // version guard — the lease transition is a no-op (WaitingExternal →
    // WaitingExternal), so ordering alone cannot prevent the stale refresh.
    // The poll_count guard ensures only the first poller's write wins.
    let c = conn();
    let record = wait_record(&c);
    upsert_wait_state(&c, &record).unwrap();
    crate::persistence::leases::update_lease_status(
        &c,
        record.lease_id.as_deref().unwrap(),
        LeaseStatus::WaitingExternal,
        Some(&record.run_id),
    )
    .unwrap();

    let decision1 = PollDecision {
        run_id: record.run_id.clone(),
        classification: PollClassification::StillWaiting,
        next_poll_at: None,
        observed_state: json!({ "poller": "first" }),
    };
    let outcome1 = apply_poll_decision(&c, &record, &decision1).unwrap();
    assert!(
        matches!(outcome1, PollApplyOutcome::Committed),
        "first poller must commit"
    );

    let after_first = get_wait_state(&c, &record.run_id).unwrap().unwrap();
    assert_eq!(after_first.poll_count, 1);
    assert_eq!(
        after_first.last_observed_state,
        json!({ "poller": "first" })
    );

    let decision2 = PollDecision {
        run_id: record.run_id.clone(),
        classification: PollClassification::StillWaiting,
        next_poll_at: None,
        observed_state: json!({ "poller": "second" }),
    };
    let err = apply_poll_decision(&c, &record, &decision2).unwrap_err();
    assert!(
        matches!(err, PollApplyError::WaitStateConcurrentTransition(ref id) if id == &record.run_id),
        "stale poller must be rejected by the optimistic version guard: {err}"
    );

    let after_second = get_wait_state(&c, &record.run_id).unwrap().unwrap();
    assert_eq!(
        after_second.poll_count, 1,
        "poll_count must not advance from a stale poller"
    );
    assert_eq!(
        after_second.last_observed_state,
        json!({ "poller": "first" }),
        "stale poller must not overwrite observed_state via last-writer-wins"
    );
}

#[test]
fn stale_cycle_a_ready_decision_cannot_mutate_replacement_cycle_b() {
    let c = conn();
    let cycle_a = wait_record(&c);
    upsert_wait_state(&c, &cycle_a).unwrap();

    let mut cycle_b = cycle_a.clone();
    cycle_b.suspension_id = uuid::Uuid::new_v4().to_string();
    cycle_b.poll_count = 0;
    cycle_b.last_observed_state = json!({ "cycle": "b" });
    upsert_wait_state(&c, &cycle_b).unwrap();

    let stale_decision = PollDecision::ready(&cycle_a, json!({ "cycle": "a", "ready": true }));
    let error = apply_poll_decision(&c, &cycle_a, &stale_decision).unwrap_err();
    assert!(matches!(
        error,
        PollApplyError::WaitStateConcurrentTransition(ref run_id) if run_id == &cycle_a.run_id
    ));

    let stored = get_wait_state(&c, &cycle_b.run_id).unwrap().unwrap();
    assert_eq!(stored.suspension_id, cycle_b.suspension_id);
    assert_eq!(stored.last_observed_state, json!({ "cycle": "b" }));
    let run = crate::persistence::get_run_with_conn(&c, &cycle_b.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(run.status, RunStatus::WaitingExternal);
    let lease = get_lease_for_issue(&c, "o/r", 62).unwrap().unwrap();
    assert_eq!(lease.status, LeaseStatus::WaitingExternal);
}
