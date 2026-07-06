use super::*;
use crate::persistence::leases::{get_lease_for_issue, init_leases_table, try_claim};
use crate::persistence::sqlite::{init_runs_schema, persist_run_with_conn};
use crate::persistence::wait_state::{get_wait_state, init_wait_states_table, upsert_wait_state};
use chrono::Duration;
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
    fn run(&self, _argv: &[String]) -> Result<String, GithubError> {
        self.responses.lock().unwrap().remove(0)
    }
}

static ARTIFACT_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn restore_artifact_root(old_root: Option<std::ffi::OsString>) {
    match old_root {
        Some(value) => std::env::set_var("LUTHER_ARTIFACTS_ROOT", value),
        None => std::env::remove_var("LUTHER_ARTIFACTS_ROOT"),
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
    let decision = PollDecision::still_waiting(&record);
    apply_poll_decision(&c, &record, &decision).unwrap();
    let updated = get_wait_state(&c, "run-62").unwrap().unwrap();
    assert_eq!(updated.poll_count, 1);
    let lease = get_lease_for_issue(&c, "o/r", 62).unwrap().unwrap();
    assert_eq!(lease.status, LeaseStatus::WaitingExternal);
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
    assert!(format!("{err}").contains("Query returned no rows"));
}
#[test]
fn still_waiting_missing_wait_state_does_not_write_poll_artifact() {
    let _guard = ARTIFACT_ENV_LOCK.lock().unwrap();
    let c = conn();
    let mut record = wait_record(&c);
    record.run_id = "run-no-artifact".to_string();
    let artifact_root = tempfile::tempdir().unwrap();
    let old_root = std::env::var_os("LUTHER_ARTIFACTS_ROOT");
    std::env::set_var("LUTHER_ARTIFACTS_ROOT", artifact_root.path());
    let decision = PollDecision::still_waiting(&record);

    let err = apply_poll_decision(&c, &record, &decision).unwrap_err();

    restore_artifact_root(old_root);
    assert!(format!("{err}").contains("Query returned no rows"));

    let run_dir = artifact_root.path().join(&record.run_id);
    assert!(
        !run_dir.exists(),
        "poll artifacts should not be written before the wait-state update succeeds"
    );
}

#[test]
fn action_required_pr_check_keeps_waiting() {
    let record = wait_record(&conn());
    let decision = classify_terminal_or_pending(
        &record,
        vec![json!({ "name": "ci", "conclusion": "action_required" })],
    );
    assert_eq!(decision.classification, PollClassification::StillWaiting);
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
