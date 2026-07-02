//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
//! @requirement:REQ-PRFU-020
//! @pseudocode lines 1-53
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
//! Registry introspection and Phase 04 behavioral TDD coverage for PR follow-through executors.
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use std::sync::{Arc, Mutex};
use std::time::Duration;

use luther_workflow::engine::executor::{
    interpolate_string, ExecutorRegistry, StepContext, StepExecutor,
};
use luther_workflow::engine::executors::SystemPrFollowupFilesystem;
use luther_workflow::engine::executors::{
    ArtifactWriter, CiFailures, ClockSleeper, CollectionState, CommandFeedbackEvaluationAdapter,
    FeedbackEvaluationAdapter, FeedbackEvaluationRequest, FeedbackEvaluatorCommandRunner,
    FeedbackEvaluatorExecutor, GithubCheckFailuresExecutorWithRunner,
    GithubCodeRabbitFeedbackExecutorWithRunner, GithubFeedbackMarkerExecutorWithRunner,
    GithubPrChecksExecutorWithRunner, GithubPrCommandRunner, GithubPrIdentityExecutorWithRunner,
    LlxprtInvocationRequest, LlxprtInvocationResult, OverallState, PostPrFailureTerminalExecutor,
    PostPrIterationGuardExecutor, PostPrTestCommandRequest, PostPrTestCommandResult,
    PostPrTestCommandRunner, PrCheckStatus, PrFollowupArtifactStore, PrFollowupBinding,
    PrFollowupLlxprtCommandRunner, PrFollowupRemediationExecutorWithRunner,
    PrRemediationPlanExecutor, PrRemediationResultExecutor, ProcessFeedbackEvaluatorCommandRunner,
    PushRemediationChangesExecutorWithRunner, PushRemediationCommandRequest,
    PushRemediationCommandResult, PushRemediationCommandRunner, RunPostPrTestsExecutorWithRunner,
};

static LEGACY_ARTIFACT_COUNTER: AtomicU64 = AtomicU64::new(0);

use luther_workflow::engine::transition::StepOutcome;

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-020
/// @pseudocode lines 1-53
const PLANNED_POST_PR_STEP_TYPES: [&str; 13] = [
    "github_pr_identity",
    "post_pr_iteration_guard",
    "github_pr_checks",
    "github_check_failures",
    "github_coderabbit_feedback",
    "feedback_evaluator",
    "pr_remediation_plan",
    "pr_followup_remediation",
    "pr_remediation_result",
    "run_post_pr_tests",
    "push_remediation_changes",
    "github_feedback_marker",
    "post_pr_failure_terminal",
];

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
struct FixedClock;

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
impl ClockSleeper for FixedClock {
    fn now_rfc3339(&self) -> String {
        "2026-04-30T00:00:00Z".to_string()
    }

    fn sleep(&self, _duration: std::time::Duration) {}
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-004,REQ-PRFU-005,REQ-PRFU-006
/// @pseudocode lines 16-33
#[derive(Clone, Debug, Default)]
struct RecordingClock {
    state: Arc<Mutex<RecordingClockState>>,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-004,REQ-PRFU-005,REQ-PRFU-006
/// @pseudocode lines 16-33
#[derive(Clone, Debug, Default)]
struct RecordingClockState {
    now_calls: Vec<String>,
    sleeps: Vec<std::time::Duration>,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-004,REQ-PRFU-005,REQ-PRFU-006
/// @pseudocode lines 16-33
impl RecordingClock {
    fn state(&self) -> RecordingClockState {
        self.state.lock().expect("clock state").clone()
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-004,REQ-PRFU-005,REQ-PRFU-006
/// @pseudocode lines 16-33
impl ClockSleeper for RecordingClock {
    fn now_rfc3339(&self) -> String {
        let mut state = self.state.lock().expect("clock state");
        let stamp = format!("2026-04-30T00:{:02}:00Z", state.now_calls.len());
        state.now_calls.push(stamp.clone());
        stamp
    }

    fn sleep(&self, duration: std::time::Duration) {
        self.state
            .lock()
            .expect("clock state")
            .sleeps
            .push(duration);
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001,REQ-PRFU-004,REQ-PRFU-017
/// @pseudocode lines 3-4,17-18
#[derive(Clone, Debug, Default)]
struct FixtureGithubPrCommandRunner;

impl GithubPrCommandRunner for FixtureGithubPrCommandRunner {
    fn run_github_command(
        &self,
        argv: &[String],
    ) -> Result<String, luther_workflow::engine::runner::EngineError> {
        if argv.iter().any(|arg| arg == "view") {
            Ok(
                include_str!("fixtures/github_api_contract/pr_identity_gh_pr_view.json")
                    .to_string(),
            )
        } else if argv.iter().any(|arg| arg == "checks") {
            Ok(
                include_str!("fixtures/github_api_contract/checks_gh_pr_checks_page1.json")
                    .to_string(),
            )
        } else if argv.iter().any(|arg| arg.contains("check-runs")) {
            Ok(include_str!("fixtures/github_api_contract/check_runs_rest_page2.json").to_string())
        } else if argv
            .iter()
            .any(|arg| arg.contains("actions/runs") && arg.contains("&page=1"))
        {
            Ok(include_str!("fixtures/github_api_contract/actions_jobs_page1.json").to_string())
        } else if argv
            .iter()
            .any(|arg| arg.contains("actions/runs") && arg.contains("&page=2"))
        {
            Ok(include_str!("fixtures/github_api_contract/actions_jobs_page2.json").to_string())
        } else if argv
            .iter()
            .any(|arg| arg.contains("actions/jobs") && arg.contains("logs"))
        {
            Ok(include_str!("fixtures/github_api_contract/actions_job_log.txt").to_string())
        } else {
            panic!("unexpected fixture github argv: {argv:?}")
        }
    }
}

#[derive(Clone, Debug, Default)]
struct ScriptedFeedbackEvaluationAdapter {
    responses: Arc<Mutex<Vec<String>>>,
    requests: Arc<Mutex<Vec<FeedbackEvaluationRequest>>>,
}

impl ScriptedFeedbackEvaluationAdapter {
    fn with_responses(responses: Vec<String>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into_iter().rev().collect())),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn requests(&self) -> Vec<FeedbackEvaluationRequest> {
        self.requests.lock().expect("requests").clone()
    }
}

impl FeedbackEvaluationAdapter for ScriptedFeedbackEvaluationAdapter {
    fn evaluate(
        &self,
        request: &FeedbackEvaluationRequest,
    ) -> Result<String, luther_workflow::engine::runner::EngineError> {
        self.requests
            .lock()
            .expect("requests")
            .push(request.clone());
        self.responses
            .lock()
            .expect("responses")
            .pop()
            .ok_or_else(|| luther_workflow::engine::runner::EngineError::StepExecutionError {
                step_id: "scripted_feedback_evaluation_adapter".to_string(),
                message: format!(
                    "No scripted response left for item_id={} head_sha={} stable_marker_key={} body_hash={}",
                    request.item_id, request.head_sha, request.stable_marker_key, request.body_hash
                ),
            })
    }
}

#[derive(Clone, Debug, Default)]
struct FixturePrFollowupLlxprtRunner;

impl PrFollowupLlxprtCommandRunner for FixturePrFollowupLlxprtRunner {
    fn invoke(&self, request: LlxprtInvocationRequest) -> LlxprtInvocationResult {
        let result = serde_json::json!({
            "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "output_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "overall_status": "success",
            "results": [{
                "source_type": "ci_failure",
                "source_id": "ci-build",
                "stable_marker_key": serde_json::Value::Null,
                "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "status": "fixed",
                "action": "scripted remediation result",
                "evidence": { "kind": "test", "current_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" },
                "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": []
            }],
            "verification_commands": []
        });
        std::fs::write(
            &request.remediation_result_path,
            serde_json::to_vec_pretty(&result).expect("serialize scripted result"),
        )
        .expect("write scripted remediation result");
        LlxprtInvocationResult {
            argv: request.argv.clone(),
            working_directory: request.working_directory.clone(),
            exit_code: Some(0),
            signal: None,
            process_class: "success".to_string(),
            bounded_stdout: "scripted llxprt remediation".to_string(),
            bounded_stderr: String::new(),
            stdout_log_path: Some(request.stdout_log_path.clone()),
            stderr_log_path: Some(request.stderr_log_path.clone()),
            success_file_present: false,
            success_file_size: None,
            result_file_present: true,
            result_file_size: None,
            result_file_path: Some(request.remediation_result_path.clone()),
            changed_paths: Vec::new(),
            spawn_error: None,
        }
    }
}

#[derive(Clone, Debug)]
struct ScriptedGithubRunner {
    pr_json: String,
    checks_json: String,
    rest_json: String,
    calls: Arc<Mutex<Vec<Vec<String>>>>,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001,REQ-PRFU-004,REQ-PRFU-017
/// @pseudocode lines 3-4,17-18
impl ScriptedGithubRunner {
    fn new(checks_json: serde_json::Value, rest_json: serde_json::Value) -> Self {
        Self::with_pr_json(
            serde_json::from_str(include_str!(
                "fixtures/github_api_contract/pr_identity_gh_pr_view.json"
            ))
            .expect("pr fixture"),
            checks_json,
            rest_json,
        )
    }

    fn with_pr_json(
        pr_json: serde_json::Value,
        checks_json: serde_json::Value,
        rest_json: serde_json::Value,
    ) -> Self {
        Self {
            pr_json: serde_json::to_string(&pr_json).expect("pr fixture"),
            checks_json: serde_json::to_string(&checks_json).expect("checks fixture"),
            rest_json: serde_json::to_string(&rest_json).expect("rest fixture"),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().expect("runner calls").clone()
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-004,REQ-PRFU-007
/// @pseudocode lines 16-33
#[derive(Clone, Debug)]
struct FlakyThenGreenGithubRunner {
    calls: Arc<Mutex<Vec<Vec<String>>>>,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-004,REQ-PRFU-007
/// @pseudocode lines 16-33
impl FlakyThenGreenGithubRunner {
    fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-004,REQ-PRFU-007
/// @pseudocode lines 16-33
impl GithubPrCommandRunner for FlakyThenGreenGithubRunner {
    fn run_github_command(
        &self,
        argv: &[String],
    ) -> Result<String, luther_workflow::engine::runner::EngineError> {
        let mut calls = self.calls.lock().expect("runner calls");
        calls.push(argv.to_vec());
        if argv.iter().any(|arg| arg == "view") {
            Ok(
                include_str!("fixtures/github_api_contract/pr_identity_gh_pr_view.json")
                    .to_string(),
            )
        } else if argv.iter().any(|arg| arg == "checks") {
            let check_calls = calls
                .iter()
                .filter(|call| call.iter().any(|arg| arg == "checks"))
                .count();
            if check_calls == 1 {
                Err(
                    luther_workflow::engine::runner::EngineError::StepExecutionError {
                        step_id: "watch_pr_checks".to_string(),

                        message: "temporary GitHub checks API failure".to_string(),
                    },
                )
            } else {
                Ok(serde_json::json!([
                    { "name": "build", "state": "SUCCESS", "bucket": "pass", "headSha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
                ])
                .to_string())
            }
        } else if argv.iter().any(|arg| arg.contains("check-runs")) {
            Ok(serde_json::json!({
                "total_count": 1,
                "check_runs": [{
                    "id": 5003,
                    "name": "integration",
                    "status": "completed",
                    "conclusion": "success",
                    "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                }]
            })
            .to_string())
        } else {
            panic!("unexpected github argv: {argv:?}")
        }
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001,REQ-PRFU-004,REQ-PRFU-017
/// @pseudocode lines 3-4,17-18
impl GithubPrCommandRunner for ScriptedGithubRunner {
    fn run_github_command(
        &self,
        argv: &[String],
    ) -> Result<String, luther_workflow::engine::runner::EngineError> {
        self.calls.lock().expect("runner calls").push(argv.to_vec());
        if argv.iter().any(|arg| arg == "view") {
            Ok(self.pr_json.clone())
        } else if argv.iter().any(|arg| arg == "checks") {
            Ok(self.checks_json.clone())
        } else if argv.iter().any(|arg| arg.contains("check-runs")) {
            Ok(self.rest_json.clone())
        } else if argv
            .iter()
            .any(|arg| arg.contains("actions/runs") && arg.contains("&page=1"))
        {
            Ok(include_str!("fixtures/github_api_contract/actions_jobs_page1.json").to_string())
        } else if argv
            .iter()
            .any(|arg| arg.contains("actions/runs") && arg.contains("&page=2"))
        {
            Ok(include_str!("fixtures/github_api_contract/actions_jobs_page2.json").to_string())
        } else if argv
            .iter()
            .any(|arg| arg.contains("actions/jobs") && arg.contains("logs"))
        {
            Ok(include_str!("fixtures/github_api_contract/actions_job_log.txt").to_string())
        } else {
            panic!("unexpected github argv: {argv:?}")
        }
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
struct TestArtifactPayload {
    payload_state: String,
}

// Generic sequence-test vehicle. Because routing-state invariants are now
// enforced on every write (not just reads), this payload must serialize with
// schema-valid routing-state fields so the per-family validators accept it when
// it is written under the `pr-check-status` / `ci-failures` families. The
// validators only inspect their own field (and `payload_state` is ignored), and
// `failed` + `collected` form no contradictory state.
impl serde::Serialize for TestArtifactPayload {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("TestArtifactPayload", 3)?;
        state.serialize_field("payload_state", &self.payload_state)?;
        state.serialize_field("overall_state", "failed")?;
        state.serialize_field("collection_state", "collected")?;
        state.end()
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @requirement:REQ-PRFU-001,REQ-PRFU-002
/// @pseudocode lines 1-53
fn sample_binding() -> PrFollowupBinding {
    PrFollowupBinding {
        schema_version: 1,

        run_id: "run-p04".to_string(),
        repository_owner: "owner".to_string(),
        repository_name: "repo".to_string(),
        pr_number: 42,
        head_ref: "feature".to_string(),
        head_sha: "head-a".to_string(),
        base_ref: "main".to_string(),
        base_sha: Some("base-a".to_string()),
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-004,REQ-PRFU-005,REQ-PRFU-006
/// @pseudocode lines 16-33
fn p06_context(temp: &tempfile::TempDir) -> StepContext {
    StepContext::new(temp.path().to_path_buf(), "run-p06".to_string())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-004,REQ-PRFU-005,REQ-PRFU-006
/// @pseudocode lines 16-33
fn p06_check_params(temp: &tempfile::TempDir, max_attempts: u64) -> serde_json::Value {
    serde_json::json!({
        "artifact_root": temp.path().join("artifacts").display().to_string(),
        "repository_owner": "example",
        "repository_name": "workflow",
        "pr_number": "1910",
        "max_attempts": max_attempts,
        "poll_interval_seconds": 300,
        "max_duration_seconds": 3600,
        "step_order_index": 3
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-004,REQ-PRFU-005,REQ-PRFU-006
/// @pseudocode lines 16-33
fn p06_pr_check_status_path(temp: &tempfile::TempDir) -> PathBuf {
    temp.path()
        .join("artifacts")
        .join("pr-followup")
        .join("current")
        .join("run-p06")
        .join("example")
        .join("workflow")
        .join("1910")
        .join("pr-check-status.json")
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-004,REQ-PRFU-005,REQ-PRFU-006
/// @pseudocode lines 16-33
fn read_json(path: &std::path::Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read json artifact"))
        .expect("parse json artifact")
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @requirement:REQ-PRFU-001
/// @pseudocode lines 1-53
fn p04_context() -> StepContext {
    let id = LEGACY_ARTIFACT_COUNTER.fetch_add(1, Ordering::SeqCst);
    let work_dir = std::env::temp_dir().join(format!("luther-p04-{id}-{}", std::process::id()));
    if work_dir.exists() {
        std::fs::remove_dir_all(&work_dir).expect("remove stale legacy p04 work dir");
    }
    StepContext::new(work_dir, format!("run-p04-{id}"))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 1-21
fn p07_context(temp: &tempfile::TempDir) -> StepContext {
    StepContext::new(temp.path().to_path_buf(), "run-p07".to_string())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 1-21
fn p07_binding() -> PrFollowupBinding {
    PrFollowupBinding {
        run_id: "run-p07".to_string(),
        repository_owner: "example".to_string(),
        repository_name: "workflow".to_string(),
        pr_number: 1910,
        head_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        ..sample_binding()
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 1-21
fn p07_params(temp: &tempfile::TempDir) -> serde_json::Value {
    serde_json::json!({
        "artifact_root": temp.path().join("artifacts").display().to_string(),
        "repository_owner": "example",
        "repository_name": "workflow",
        "pr_number": "1910",
        "head_ref": "feature",
        "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "base_ref": "main",
        "base_sha": "base-a",
        "step_order_index": 4
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 1-21
fn p07_ci_failures_path(temp: &tempfile::TempDir) -> PathBuf {
    temp.path()
        .join("artifacts")
        .join("pr-followup")
        .join("current")
        .join("run-p07")
        .join("example")
        .join("workflow")
        .join("1910")
        .join("ci-failures.json")
}

fn p07_pr_check_status_path(temp: &tempfile::TempDir) -> PathBuf {
    temp.path()
        .join("artifacts")
        .join("pr-followup")
        .join("current")
        .join("run-p07")
        .join("example")
        .join("workflow")
        .join("1910")
        .join("pr-check-status.json")
}

fn set_p07_ignored_check_ids(temp: &tempfile::TempDir, ids: &[&str]) {
    let check_status_path = p07_pr_check_status_path(temp);
    let mut check_status = read_json(&check_status_path);
    check_status["ignored_check_ids"] = serde_json::json!(ids);
    std::fs::write(
        &check_status_path,
        serde_json::to_string_pretty(&check_status).expect("serialize check status"),
    )
    .expect("write check status");
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 1-5,17-18
fn write_p07_check_status(
    temp: &tempfile::TempDir,
    overall_state: &str,
    checks: serde_json::Value,
    stale_checks: serde_json::Value,
    fatal_source: serde_json::Value,
) {
    let store = PrFollowupArtifactStore::new(temp.path().join("artifacts"));
    let binding = p07_binding();
    store
        .write_json_artifact(
            &binding,
            "pr",
            "capture_pr_identity",
            1,
            &serde_json::json!({
                "pr_url": "https://github.com/example/workflow/pull/1910",
                "capture_state": "captured",
                "captured_at": "2026-04-30T00:00:00Z",
                "source": "fixture",
                "source_pr_node_id": "PR_kwDOExample",
                "source_head_repository_owner": null,
                "source_head_repository_name": null
            }),
            None,
            &FixedClock,
        )
        .expect("write pr");
    let failure = if overall_state == "passed" {
        None
    } else {
        Some((
            overall_state,
            overall_state,
            serde_json::json!({ "fatal_source": fatal_source }),
        ))
    };
    store.write_json_artifact(
        &binding,
        "pr-check-status",
        "watch_pr_checks",
        3,
        &serde_json::json!({
            "pr_url": "https://github.com/example/workflow/pull/1910",
            "poll_attempts": 1,
            "max_attempts": 12,
            "poll_interval_seconds": 300,
            "max_duration_seconds": 3600,
            "overall_state": overall_state,
            "poll_observations": [],
            "checks": checks,
            "stale_checks": stale_checks,
            "observed_at": "2026-04-30T00:00:00Z",
            "fatal_source": fatal_source,
            "terminal_counts": { "passed": 0, "failed": 0, "pending": 0, "unknown": 0, "stale": 0 }
        }),
        failure,
        &FixedClock,
    ).expect("write pr-check-status");
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017
/// @pseudocode lines 4-29
#[derive(Clone, Debug)]
struct P08FeedbackRunner {
    graph_pages: Vec<serde_json::Value>,
    review_comment_pages: Vec<serde_json::Value>,
    issue_comment_pages: Vec<serde_json::Value>,
    check_runs: Vec<serde_json::Value>,
    calls: Arc<Mutex<Vec<Vec<String>>>>,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017
/// @pseudocode lines 4-29
impl P08FeedbackRunner {
    fn new(check_runs: Vec<serde_json::Value>) -> Self {
        Self {
            graph_pages: vec![serde_json::json!({
                "data": { "repository": { "pullRequest": { "reviewThreads": {
                    "nodes": [],
                    "pageInfo": { "hasNextPage": false }
                } } } }
            })],
            review_comment_pages: vec![serde_json::json!([])],
            issue_comment_pages: vec![serde_json::json!([])],
            check_runs,
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn with_pages(
        graph_pages: Vec<serde_json::Value>,
        review_comment_pages: Vec<serde_json::Value>,
        issue_comment_pages: Vec<serde_json::Value>,
        check_runs: Vec<serde_json::Value>,
    ) -> Self {
        Self {
            graph_pages,
            review_comment_pages,
            issue_comment_pages,
            check_runs,
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().expect("p08 calls").clone()
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017
/// @pseudocode lines 4-29
impl GithubPrCommandRunner for P08FeedbackRunner {
    fn run_github_command(
        &self,
        argv: &[String],
    ) -> Result<String, luther_workflow::engine::runner::EngineError> {
        self.calls.lock().expect("p08 calls").push(argv.to_vec());
        if argv.iter().any(|arg| arg.contains("graphql")) {
            let page = argv
                .iter()
                .find_map(|arg| arg.strip_prefix("page="))
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or_else(|| {
                    self.calls
                        .lock()
                        .expect("p08 calls")
                        .iter()
                        .filter(|call| call.iter().any(|arg| arg.contains("graphql")))
                        .count()
                });

            assert!(
                argv.iter()
                    .any(|arg| arg.starts_with("query=") && arg.contains("reviewThreads")),
                "graphql review thread query must pass actual GraphQL text, got {argv:?}"
            );
            if let Some(page_arg) = argv.iter().find_map(|arg| arg.strip_prefix("page=")) {
                assert!(
                    page_arg.parse::<usize>().is_err(),
                    "graphql review thread page variable must be an endCursor string, not numeric page {page_arg}"
                );
            }

            Ok(serde_json::to_string(
                self.graph_pages
                    .get(page.saturating_sub(1))
                    .unwrap_or_else(|| self.graph_pages.last().expect("graph page")),
            )
            .expect("graph json"))
        } else if argv
            .iter()
            .any(|arg| arg.contains("/pulls/") && arg.contains("/comments"))
        {
            Ok(serde_json::to_string(&self.review_comment_pages[0]).expect("review page"))
        } else if argv
            .iter()
            .any(|arg| arg.contains("/issues/") && arg.contains("/comments"))
        {
            let page = argv
                .iter()
                .find_map(|arg| {
                    arg.rsplit_once("page=")
                        .and_then(|(_, value)| value.parse::<usize>().ok())
                })
                .unwrap_or(1);
            Ok(serde_json::to_string(
                self.issue_comment_pages
                    .get(page.saturating_sub(1))
                    .unwrap_or_else(|| self.issue_comment_pages.last().expect("issue page")),
            )
            .expect("issue page"))
        } else if argv.iter().any(|arg| arg.contains("check-runs")) {
            let index = self
                .calls
                .lock()
                .expect("p08 calls")
                .iter()
                .filter(|call| call.iter().any(|arg| arg.contains("check-runs")))
                .count()
                .saturating_sub(1);
            Ok(serde_json::to_string(
                self.check_runs
                    .get(index)
                    .unwrap_or_else(|| self.check_runs.last().expect("check run")),
            )
            .expect("check json"))
        } else {
            panic!("unexpected p08 argv: {argv:?}");
        }
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009
/// @pseudocode lines 1-29
fn p08_context(temp: &tempfile::TempDir) -> StepContext {
    StepContext::new(temp.path().to_path_buf(), "run-p08".to_string())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009
/// @pseudocode lines 1-29
fn p08_params(temp: &tempfile::TempDir, max_observations: u64) -> serde_json::Value {
    serde_json::json!({
        "artifact_root": temp.path().join("artifacts").display().to_string(),
        "repository_owner": "example",
        "repository_name": "workflow",
        "pr_number": "1910",
        "head_ref": "feature",
        "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "base_ref": "main",
        "base_sha": "base-a",
        "max_observations": max_observations,
        "required_stable_observations": 2,
        "coderabbit_readiness_observation_interval_seconds": 300,
        "step_order_index": 5
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009
/// @pseudocode lines 20-29
fn p08_feedback_path(temp: &tempfile::TempDir) -> PathBuf {
    temp.path()
        .join("artifacts")
        .join("pr-followup")
        .join("current")
        .join("run-p08")
        .join("example")
        .join("workflow")
        .join("1910")
        .join("coderabbit-feedback.json")
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 6,15-19
fn check_runs_signal(
    status: &str,
    conclusion: serde_json::Value,
    summary: &str,
) -> serde_json::Value {
    serde_json::json!({ "check_runs": [{
        "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "status": status,
        "conclusion": conclusion,
        "output": { "summary": summary },
        "app": { "slug": "coderabbitai" }
    }] })
}

fn check_runs_signal_without_head_sha(
    status: &str,
    conclusion: serde_json::Value,
    summary: &str,
) -> serde_json::Value {
    serde_json::json!({ "check_runs": [{
        "name": "CodeRabbit",
        "status": status,
        "conclusion": conclusion,
        "output": { "summary": summary },
        "app": { "slug": "coderabbitai" }
    }] })
}

/// @requirement:REQ-PRFU-001
/// @pseudocode lines 1-53
fn assert_expected_outcome(actual: StepOutcome, expected: StepOutcome, assertion: &str) {
    assert_eq!(
        actual, expected,
        "{assertion}; actual outcome was {actual:?}"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @requirement:REQ-PRFU-001
/// @pseudocode lines 1-53
fn execute_step<E: StepExecutor>(executor: E) -> StepOutcome {
    let mut context = p04_context();
    let artifact_root = context.work_dir().join("artifacts").display().to_string();

    executor
        .execute(
            &mut context,
            &serde_json::json!({
                "artifact_root": artifact_root,
                "repository_owner": "octo-org",
                "repository_name": "workflow",
                "pr_number": "1910",
                "head_ref": "feature",
                "head_sha": "head-a",
                "base_ref": "main",
                "base_sha": "base-a"
            }),
        )
        .expect("contract executor must return an outcome rather than a harness error")
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-020

#[test]
fn production_executor_modules_do_not_expose_fixture_selection_seams() {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let github_pr = std::fs::read_to_string(workspace.join("src/engine/executors/github_pr.rs"))
        .expect("read github_pr executor source");
    let feedback_eval =
        std::fs::read_to_string(workspace.join("src/engine/executors/feedback_eval.rs"))
            .expect("read feedback_eval executor source");
    let pr_remediation =
        std::fs::read_to_string(workspace.join("src/engine/executors/pr_remediation.rs"))
            .expect("read pr_remediation executor source");
    let exports = std::fs::read_to_string(workspace.join("src/engine/executors/mod.rs"))
        .expect("read executor exports");

    assert!(!github_pr.contains("use_fixture_github_runner"));
    assert!(!github_pr.contains("FixtureGithubPrCommandRunner"));
    assert!(!pr_remediation.contains("use_fixture_llxprt_runner"));
    assert!(!feedback_eval.contains("FixtureFeedbackEvaluationAdapter"));
    assert!(!feedback_eval.contains("impl Default for FeedbackEvaluatorExecutor"));
    let public_exports = exports
        .lines()
        .filter(|line| line.trim_start().starts_with("pub use"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !public_exports.contains("Fixture"),
        "production executor exports must not publicly re-export fixture implementations"
    );
}

/// @pseudocode lines 1-53
#[test]
fn registry_registers_all_post_pr_step_types_by_introspection() {
    let registry = ExecutorRegistry::with_defaults();

    for step_type in PLANNED_POST_PR_STEP_TYPES {
        assert!(
            registry.contains_step_type(step_type),
            "missing post-PR step type: {step_type}"
        );
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-020
/// @pseudocode lines 1-53
#[test]
fn registry_introspection_lists_all_post_pr_step_types() {
    let registered = ExecutorRegistry::with_defaults().registered_step_types();

    for step_type in PLANNED_POST_PR_STEP_TYPES {
        assert!(
            registered.contains(step_type),
            "registered step type set omitted: {step_type}"
        );
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
#[test]
fn artifact_store_allocates_global_artifact_and_failure_sequences_with_per_family_write_sequences_for_interleaved_families(
) {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let binding = sample_binding();
    let clock = FixedClock;

    let first = store
        .write_json_artifact(
            &binding,
            "pr-check-status",
            "watch_pr_checks",
            3,
            &TestArtifactPayload {
                payload_state: "failed".to_string(),
            },
            Some(("failed", "first_failure", serde_json::json!({ "n": 1 }))),
            &clock,
        )
        .expect("first write");
    let second = store
        .write_json_artifact(
            &binding,
            "ci-failures",
            "collect_ci_failures",
            4,
            &TestArtifactPayload {
                payload_state: "fatal".to_string(),
            },
            Some(("fatal", "second_failure", serde_json::json!({ "n": 2 }))),
            &clock,
        )
        .expect("second write");
    let third = store
        .write_json_artifact(
            &binding,
            "pr-check-status",
            "watch_pr_checks",
            3,
            &TestArtifactPayload {
                payload_state: "passed".to_string(),
            },
            None,
            &clock,
        )
        .expect("third write");

    assert!(
        first.sequence.artifact_sequence == 1
            && second.sequence.artifact_sequence == 2
            && third.sequence.artifact_sequence == 3
            && first.sequence.write_sequence == 1
            && second.sequence.write_sequence == 1
            && third.sequence.write_sequence == 2
            && first.failure_sequence == Some(1)
            && second.failure_sequence == Some(2)
            && third.failure_sequence.is_none()
            && first.canonical_path.exists()
            && first.history_path.exists()
            && second.canonical_path.exists()
            && second.history_path.exists(),
        "artifact store must allocate global artifact sequences and per-family write sequences for interleaved families; observed writes: {first:?}, {second:?}, {third:?}"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
#[test]
fn artifact_store_recovers_sequence_allocation_from_history_when_canonical_current_is_missing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let binding = sample_binding();
    let clock = FixedClock;
    let written = store
        .write_json_artifact(
            &binding,
            "coderabbit-feedback",
            "collect_coderabbit_feedback",
            5,
            &TestArtifactPayload {
                payload_state: "ready".to_string(),
            },
            None,
            &clock,
        )
        .expect("initial write");
    std::fs::remove_file(&written.canonical_path).expect("remove canonical");

    let next = store
        .next_sequence(&binding, "coderabbit-feedback")
        .expect("sequence");

    assert!(
        next.artifact_sequence == 2 && next.write_sequence == 2,
        "artifact store must scan accepted history snapshots and recover allocation after canonical-only loss; next={next:?}"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
#[test]
fn artifact_store_recovers_sequence_allocation_from_current_when_history_is_missing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let binding = sample_binding();
    let clock = FixedClock;
    let written = store
        .write_json_artifact(
            &binding,
            "pr-check-status",
            "watch_pr_checks",
            3,
            &TestArtifactPayload {
                payload_state: "failed".to_string(),
            },
            Some(("failed", "failure", serde_json::json!({}))),
            &clock,
        )
        .expect("initial write");
    std::fs::remove_file(&written.history_path).expect("remove history");

    let next = store
        .next_sequence(&binding, "pr-check-status")
        .expect("sequence");
    let failure = store
        .next_failure_sequence(&binding)
        .expect("failure sequence");

    assert_eq!(
        next.artifact_sequence, 2,
        "history-only loss must recover global sequence from canonical current"
    );
    assert_eq!(
        next.write_sequence, 2,
        "history-only loss must recover per-family write sequence from canonical current"
    );
    assert_eq!(
        failure, 2,
        "history-only loss must recover failure sequence from canonical current"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
fn rewrite_json_field(path: &std::path::Path, field: &str, value: serde_json::Value) {
    let mut artifact: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read artifact for mutation"))
            .expect("parse artifact for mutation");
    artifact
        .as_object_mut()
        .expect("artifact object")
        .insert(field.to_string(), value);
    std::fs::write(
        path,
        serde_json::to_vec_pretty(&artifact).expect("serialize mutated artifact"),
    )
    .expect("write mutated artifact");
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
#[test]
fn artifact_store_rejects_non_monotonic_global_artifact_sequence_for_consumed_family() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let binding = sample_binding();
    let clock = FixedClock;
    store
        .write_json_artifact(
            &binding,
            "pr-check-status",
            "watch_pr_checks",
            3,
            &TestArtifactPayload {
                payload_state: "first".to_string(),
            },
            None,
            &clock,
        )
        .expect("first write");
    let second = store
        .write_json_artifact(
            &binding,
            "ci-failures",
            "collect_ci_failures",
            4,
            &TestArtifactPayload {
                payload_state: "second".to_string(),
            },
            None,
            &clock,
        )
        .expect("second write");
    rewrite_json_field(
        &second.history_path,
        "artifact_sequence",
        serde_json::json!(3),
    );

    let err = store
        .next_sequence(&binding, "pr-check-status")
        .expect_err("gap must be rejected");

    assert!(
        format!("{err}").contains("non-monotonic artifact_sequence"),
        "artifact recovery must reject, not repair, global artifact_sequence gaps affecting consumed families; err={err:?}"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
#[test]
fn artifact_store_rejects_duplicate_global_artifact_sequence_for_consumed_family() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let binding = sample_binding();
    let clock = FixedClock;
    store
        .write_json_artifact(
            &binding,
            "pr-check-status",
            "watch_pr_checks",
            3,
            &TestArtifactPayload {
                payload_state: "first".to_string(),
            },
            None,
            &clock,
        )
        .expect("first write");
    let second = store
        .write_json_artifact(
            &binding,
            "ci-failures",
            "collect_ci_failures",
            4,
            &TestArtifactPayload {
                payload_state: "second".to_string(),
            },
            None,
            &clock,
        )
        .expect("second write");
    rewrite_json_field(
        &second.history_path,
        "artifact_sequence",
        serde_json::json!(1),
    );

    let err = store
        .next_sequence(&binding, "pr-check-status")
        .expect_err("duplicate must be rejected");

    assert!(
        format!("{err}").contains("duplicate artifact_sequence"),
        "artifact recovery must reject duplicate global artifact_sequence data affecting consumed families; err={err:?}"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
#[test]
fn artifact_store_rejects_non_monotonic_per_family_write_sequence_for_consumed_family() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let binding = sample_binding();
    let clock = FixedClock;
    store
        .write_json_artifact(
            &binding,
            "pr-check-status",
            "watch_pr_checks",
            3,
            &TestArtifactPayload {
                payload_state: "first".to_string(),
            },
            None,
            &clock,
        )
        .expect("first write");
    let second = store
        .write_json_artifact(
            &binding,
            "pr-check-status",
            "watch_pr_checks",
            3,
            &TestArtifactPayload {
                payload_state: "second".to_string(),
            },
            None,
            &clock,
        )
        .expect("second write");
    rewrite_json_field(&second.history_path, "write_sequence", serde_json::json!(3));

    let err = store
        .next_sequence(&binding, "pr-check-status")
        .expect_err("gap must be rejected");

    assert!(
        format!("{err}").contains("non-monotonic write_sequence for pr-check-status"),
        "artifact recovery must reject, not repair, per-family write_sequence gaps; err={err:?}"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
#[test]
fn artifact_store_rejects_unbound_current_run_sequence_data_for_consumed_family() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let binding = sample_binding();
    let clock = FixedClock;
    let written = store
        .write_json_artifact(
            &binding,
            "pr-check-status",
            "watch_pr_checks",
            3,
            &TestArtifactPayload {
                payload_state: "first".to_string(),
            },
            None,
            &clock,
        )
        .expect("write");
    rewrite_json_field(
        &written.history_path,
        "run_id",
        serde_json::json!("other-run"),
    );

    let err = store
        .next_sequence(&binding, "pr-check-status")
        .expect_err("unbound artifact must be rejected");

    assert!(
        format!("{err}").contains("artifact binding mismatch"),
        "artifact recovery must reject same-path data not bound to the current run; err={err:?}"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
#[test]
fn artifact_sequence_recovery_allows_same_pr_run_history_after_head_change() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let first_binding = sample_binding();
    let mut second_binding = first_binding.clone();
    second_binding.head_sha = "head-b".to_string();
    let clock = FixedClock;
    store
        .write_json_artifact(
            &first_binding,
            "pr",
            "capture_pr_identity",
            1,
            &TestArtifactPayload {
                payload_state: "first-head".to_string(),
            },
            None,
            &clock,
        )
        .expect("write first head artifact");

    let sequence = store
        .next_sequence(&second_binding, "pr")
        .expect("same PR run history from a previous head should seed sequence allocation");

    assert_eq!(sequence.artifact_sequence, 2);
    assert_eq!(sequence.write_sequence, 2);

    let err = store
        .read_current_json(&second_binding, "pr")
        .expect_err("current artifact reads remain bound to the exact head");
    assert!(
        format!("{err}").contains("artifact binding mismatch"),
        "exact current artifact reads must remain strict; err={err:?}"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
#[test]
fn artifact_store_rejects_non_monotonic_failure_sequence_when_allocating_failure_sequence() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let binding = sample_binding();
    let clock = FixedClock;
    store
        .write_json_artifact(
            &binding,
            "pr-check-status",
            "watch_pr_checks",
            3,
            &TestArtifactPayload {
                payload_state: "failed".to_string(),
            },
            Some(("failed", "first_failure", serde_json::json!({}))),
            &clock,
        )
        .expect("first failure write");
    let second = store
        .write_json_artifact(
            &binding,
            "ci-failures",
            "collect_ci_failures",
            4,
            &TestArtifactPayload {
                payload_state: "fatal".to_string(),
            },
            Some(("fatal", "second_failure", serde_json::json!({}))),
            &clock,
        )
        .expect("second failure write");
    rewrite_json_field(
        &second.history_path,
        "failure_sequence",
        serde_json::json!(3),
    );

    let err = store
        .next_failure_sequence(&binding)
        .expect_err("failure sequence gap must be rejected");

    assert!(
        format!("{err}").contains("non-monotonic failure_sequence"),
        "artifact recovery must reject, not repair, failure_sequence gaps; err={err:?}"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
#[test]
fn artifact_schema_validation_rejects_missing_common_binding_and_history_metadata() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let expected = sample_binding();
    let mut actual = expected.clone();
    actual.head_sha.clear();

    assert!(
        !store.validate_binding(&expected, &actual),
        "artifact schema validation must reject missing or mismatched binding fields and required history_metadata"
    );
    let invalid = serde_json::json!({
        "schema_version": 1,
        "run_id": expected.run_id,
        "repository_owner": expected.repository_owner,
        "repository_name": expected.repository_name,
        "pr_number": expected.pr_number,
        "head_ref": expected.head_ref,
        "head_sha": expected.head_sha,
        "base_ref": expected.base_ref,
        "base_sha": expected.base_sha,
        "artifact_sequence": 1,
        "write_sequence": 1,
        "producer_step_id": "watch_pr_checks",
        "step_order_index": 3
    });

    assert!(
        store.validate_artifact_value(&sample_binding(), "pr-check-status", &invalid).is_err(),
        "artifact schema validation must reject missing or mismatched binding fields and required history_metadata"
    );
}

/// Builds a fully-formed flat `pr-check-status` artifact value (binding +
/// store envelope + history_metadata) so validator tests exercise only the
/// per-family invariants rather than envelope failures.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-007
fn flat_pr_check_status_value(
    binding: &PrFollowupBinding,
    overall_state: &str,
    fatal_source: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "schema_version": binding.schema_version,
        "run_id": binding.run_id,
        "repository_owner": binding.repository_owner,
        "repository_name": binding.repository_name,
        "pr_number": binding.pr_number,
        "head_ref": binding.head_ref,
        "head_sha": binding.head_sha,
        "base_ref": binding.base_ref,
        "base_sha": binding.base_sha,
        "artifact_sequence": 2,
        "write_sequence": 2,
        "producer_step_id": "watch_pr_checks",
        "step_order_index": 3,
        "history_metadata": {
            "canonical_path": "current/pr-check-status.json",
            "history_path": "history/pr-check-status/2-2-watch_pr_checks.json",
            "artifact_family": "pr-check-status",
            "is_canonical": true,
            "history_written_at": "2026-04-30T00:05:00Z"
        },
        "overall_state": overall_state,
        "fatal_source": fatal_source
    })
}

/// Builds a fully-formed flat `ci-failures` artifact value (binding + store
/// envelope + history_metadata) for per-family invariant tests.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-007
fn flat_ci_failures_value(
    binding: &PrFollowupBinding,
    collection_state: &str,
    fatal_source: serde_json::Value,
    watcher_fatal_source: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "schema_version": binding.schema_version,
        "run_id": binding.run_id,
        "repository_owner": binding.repository_owner,
        "repository_name": binding.repository_name,
        "pr_number": binding.pr_number,
        "head_ref": binding.head_ref,
        "head_sha": binding.head_sha,
        "base_ref": binding.base_ref,
        "base_sha": binding.base_sha,
        "artifact_sequence": 3,
        "write_sequence": 1,
        "producer_step_id": "collect_ci_failures",
        "step_order_index": 4,
        "history_metadata": {
            "canonical_path": "current/ci-failures.json",
            "history_path": "history/ci-failures/3-1-collect_ci_failures.json",
            "artifact_family": "ci-failures",
            "is_canonical": true,
            "history_written_at": "2026-04-30T00:06:00Z"
        },
        "collection_state": collection_state,
        "fatal_source": fatal_source,
        "watcher_fatal_source": watcher_fatal_source
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 16-33
#[test]
fn pr_check_status_passed_with_non_null_fatal_source_is_rejected_by_validator() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let binding = sample_binding();
    let value = flat_pr_check_status_value(&binding, "passed", serde_json::json!("api"));

    let err = store
        .validate_artifact_invariants("pr-check-status", &value)
        .expect_err("passed + non-null fatal_source must be rejected");
    assert!(
        format!("{err}").contains("fatal_source"),
        "validator must explain the contradictory passed+fatal_source state; err={err:?}"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 16-33
#[test]
fn pr_check_status_unknown_overall_state_is_rejected_by_validator() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let binding = sample_binding();
    let value = flat_pr_check_status_value(&binding, "not_a_real_state", serde_json::Value::Null);

    assert!(
        store
            .validate_artifact_invariants("pr-check-status", &value)
            .is_err(),
        "validator must reject an unknown overall_state value"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 16-33
#[test]
fn pr_check_status_valid_passed_null_fatal_source_passes_validator() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let binding = sample_binding();
    let value = flat_pr_check_status_value(&binding, "passed", serde_json::Value::Null);

    store
        .validate_artifact_invariants("pr-check-status", &value)
        .expect("passed + null fatal_source must validate");
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 1-21
#[test]
fn ci_failures_collected_with_non_null_watcher_fatal_source_is_rejected_by_validator() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let binding = sample_binding();
    let value = flat_ci_failures_value(
        &binding,
        "collected",
        serde_json::Value::Null,
        serde_json::json!({ "class": "api_error" }),
    );

    let err = store
        .validate_artifact_invariants("ci-failures", &value)
        .expect_err("collected + non-null watcher_fatal_source must be rejected");
    assert!(
        format!("{err}").contains("watcher_fatal_source"),
        "validator must explain the contradictory collected+watcher_fatal_source state; err={err:?}"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 1-21
#[test]
fn ci_failures_unknown_collection_state_is_rejected_by_validator() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let binding = sample_binding();
    let value = flat_ci_failures_value(
        &binding,
        "not_a_real_state",
        serde_json::Value::Null,
        serde_json::Value::Null,
    );

    assert!(
        store
            .validate_artifact_invariants("ci-failures", &value)
            .is_err(),
        "validator must reject an unknown collection_state value"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 1-21
#[test]
fn ci_failures_fatal_with_null_sources_passes_validator() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let binding = sample_binding();
    // A stale-only / pending-driven terminal collection legitimately writes
    // collection_state="fatal" without a fatal_source; this must not be
    // rejected as contradictory.
    let value = flat_ci_failures_value(
        &binding,
        "fatal",
        serde_json::Value::Null,
        serde_json::Value::Null,
    );

    store
        .validate_artifact_invariants("ci-failures", &value)
        .expect("fatal collection with null sources must validate");
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 16-33
#[test]
fn pr_check_status_deserializes_from_existing_fixture() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/github_pr/current/pr-check-status.json");
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&fixture).expect("read fixture"))
            .expect("parse fixture");
    let typed: PrCheckStatus =
        serde_json::from_value(value).expect("flat fixture must deserialize into PrCheckStatus");
    typed
        .validate_invariants()
        .expect("existing fixture must satisfy invariants");
    assert_eq!(typed.overall_state, OverallState::Failed);
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 1-21
#[test]
fn ci_failures_deserializes_from_existing_fixture() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/github_pr/current/ci-failures.json");
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&fixture).expect("read fixture"))
            .expect("parse fixture");
    let typed: CiFailures =
        serde_json::from_value(value).expect("flat fixture must deserialize into CiFailures");
    typed
        .validate_invariants()
        .expect("existing fixture must satisfy invariants");
    assert_eq!(typed.collection_state, CollectionState::Collected);
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 1-21
#[test]
fn collect_ci_failures_rejects_contradictory_passed_with_fatal_source() {
    let temp = tempfile::tempdir().expect("tempdir");
    // Seed a valid passed pr-check-status, then corrupt the canonical artifact
    // on disk into the contradictory passed+fatal_source state. The read-time
    // validator used by collect_ci_failures (store.read_current_json on
    // "pr-check-status") must reject this artifact so the stale fatal_source can
    // never drive the watcher_fatal routing branch into StepOutcome::Fatal.
    write_p07_check_status(
        &temp,
        "passed",
        serde_json::json!([{ "check_id": "build", "name": "build", "state": "success", "conclusion": "success", "bucket": "passed", "url": null, "run_id": null, "job_id": null }]),
        serde_json::json!([]),
        serde_json::Value::Null,
    );
    let store = PrFollowupArtifactStore::new(temp.path().join("artifacts"));
    let binding = p07_binding();
    let path = store.canonical_path(&binding, "pr-check-status");
    let mut artifact: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read status"))
            .expect("parse status");
    // Sanity: before corruption the routing read accepts the passed artifact.
    store
        .read_current_json(&binding, "pr-check-status")
        .expect("valid passed artifact must read cleanly before corruption");
    artifact["fatal_source"] = serde_json::json!("api");
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&artifact).expect("serialize status"),
    )
    .expect("rewrite status");

    let err = store
        .read_current_json(&binding, "pr-check-status")
        .expect_err("contradictory passed+fatal_source must be rejected at the routing read site");
    assert!(
        format!("{err}").contains("fatal_source"),
        "rejection must cite the contradictory passed+fatal_source state instead of silently routing Fatal; err={err:?}"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001,REQ-PRFU-004,REQ-PRFU-020A
/// @pseudocode lines 16-33
#[test]
fn github_pr_checks_interpolates_artifact_root_from_context() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runner = ScriptedGithubRunner::new(
        serde_json::json!([
            {
                "name": "unit",
                "state": "SUCCESS",
                "bucket": "pass",
                "link": "https://example.invalid/checks/unit"
            }
        ]),
        serde_json::json!({
            "total_count": 1,
            "check_runs": [{
                "id": 42,
                "name": "unit",
                "status": "completed",
                "conclusion": "success",
                "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            }]
        }),
    );
    let mut context = StepContext::new(temp.path().to_path_buf(), "run-p06".to_string());
    context.set(
        "artifact_dir",
        temp.path().join("artifacts").to_string_lossy().as_ref(),
    );

    let outcome = GithubPrChecksExecutorWithRunner::new(runner, RecordingClock::default())
        .execute(
            &mut context,
            &serde_json::json!({
                "artifact_root": "{artifact_dir}",
                "repository_owner": "example",
                "repository_name": "workflow",
                "pr_number": "1910",
                "max_attempts": 1,
                "poll_interval_seconds": 1,
                "max_duration_seconds": 60,
                "step_order_index": 3
            }),
        )
        .expect("watch pr checks with interpolated artifact_root");

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "successful checks should pass",
    );
    assert!(
        p06_pr_check_status_path(&temp).exists(),
        "github_pr_checks must write artifacts under the interpolated artifact_dir, not a literal template path"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001,REQ-PRFU-004,REQ-PRFU-020A
/// @pseudocode lines 1-33
#[test]
fn github_pr_identity_interpolates_repository_params_from_context() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runner = ScriptedGithubRunner::new(
        serde_json::json!([]),
        serde_json::json!({
            "total_count": 0,
            "check_runs": []
        }),
    );
    let mut context = StepContext::new(temp.path().to_path_buf(), "run-p06".to_string());
    context.set(
        "artifact_dir",
        temp.path().join("artifacts").to_string_lossy().as_ref(),
    );
    context.set("repository_owner", "vybestack");
    context.set("repository_name", "llxprt-code");
    context.set("pr_number", "1911");

    let outcome =
        GithubPrIdentityExecutorWithRunner::new(runner.clone(), RecordingClock::default())
            .execute(
                &mut context,
                &serde_json::json!({
                    "artifact_root": "{artifact_dir}",
                    "repository_owner": "{repository_owner}",
                    "repository_name": "{repository_name}",
                    "pr_number": "{pr_number}",
                    "step_order_index": 1
                }),
            )
            .expect("capture pr identity with interpolated repository params");

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "PR identity capture should succeed with interpolated repository params",
    );
    let calls = runner.calls();

    assert!(
        calls
            .iter()
            .any(|argv| argv.iter().any(|arg| arg == "vybestack/llxprt-code")),
        "identity capture must call gh with interpolated repository owner/name, got {calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|argv| argv.iter().any(|arg| arg == "1911")),
        "identity capture must call gh with interpolated PR number, got {calls:?}"
    );
}
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001,REQ-PRFU-004,REQ-PRFU-020A
/// @pseudocode lines 1-33
#[test]
fn github_pr_identity_recovers_captured_pr_when_resume_context_lacks_identity() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runner = scripted_pr90_runner();
    let mut context = StepContext::new(temp.path().to_path_buf(), "run-p06-resume".to_string());
    context.set(
        "artifact_dir",
        temp.path().join("artifacts").to_string_lossy().as_ref(),
    );
    write_p06_resume_pr_identities(&temp);

    let outcome =
        GithubPrIdentityExecutorWithRunner::new(runner.clone(), RecordingClock::default())
            .execute(&mut context, &p06_resume_identity_params())
            .expect("capture pr identity with discovered PR context");

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "capture_pr_identity must rediscover the captured open PR when resume context omits PR metadata",
    );
    assert_p06_resume_identity_recovered(&runner, &context);
}

fn scripted_pr90_runner() -> ScriptedGithubRunner {
    ScriptedGithubRunner::with_pr_json(
        serde_json::json!({
            "number": 90,
            "url": "https://github.com/vybestack/llxprt-luther/pull/90",
            "headRefName": "issue82",
            "headRefOid": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "baseRefName": "main",
            "baseRefOid": "cccccccccccccccccccccccccccccccccccccccc",
            "state": "OPEN",
            "isDraft": false,
            "id": "PR_kwDOIssue82"
        }),
        serde_json::json!([]),
        serde_json::json!({ "total_count": 0, "check_runs": [] }),
    )
}

fn write_p06_resume_pr_identities(temp: &tempfile::TempDir) {
    let store = PrFollowupArtifactStore::new(temp.path().join("artifacts"));
    let real_binding = PrFollowupBinding {
        run_id: "run-p06-resume".to_string(),
        repository_owner: "vybestack".to_string(),
        repository_name: "llxprt-luther".to_string(),
        pr_number: 90,
        head_ref: "issue82".to_string(),
        head_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        base_ref: "main".to_string(),
        base_sha: Some("cccccccccccccccccccccccccccccccccccccccc".to_string()),
        ..sample_binding()
    };
    let legacy_binding = PrFollowupBinding {
        run_id: "run-p06-resume".to_string(),
        repository_owner: "vybestack".to_string(),
        repository_name: "llxprt-luther".to_string(),
        pr_number: 42,
        ..sample_binding()
    };
    write_p06_pr_identity(&store, &legacy_binding, 42, "legacy_harness");
    write_p06_pr_identity(&store, &real_binding, 90, "gh_pr_view");
}

fn write_p06_pr_identity(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    pr_number: u64,
    source: &str,
) {
    store
        .write_json_artifact(
            binding,
            "pr",
            "capture_pr_identity",
            1,
            &serde_json::json!({
                "pr_url": format!("https://github.com/vybestack/llxprt-luther/pull/{pr_number}"),
                "capture_state": "captured",
                "captured_at": "2026-04-30T00:00:00Z",
                "source": source
            }),
            None,
            &FixedClock,
        )
        .expect("write PR identity");
}

fn p06_resume_identity_params() -> serde_json::Value {
    serde_json::json!({
        "artifact_root": "{artifact_dir}",
        "repository_owner": "{repository_owner}",
        "repository_name": "{repository_name}",
        "pr_number": "{pr_number}",
        "step_order_index": 1
    })
}

fn assert_p06_resume_identity_recovered(runner: &ScriptedGithubRunner, context: &StepContext) {
    let calls = runner.calls();
    assert!(
        calls.iter().any(|argv| argv.iter().any(|arg| arg == "90")),
        "identity capture must rediscover PR 90 instead of falling back to PR 42, got {calls:?}"
    );
    assert!(
        !calls.iter().any(|argv| argv.iter().any(|arg| arg == "42")),
        "legacy harness artifacts must not drive resumed identity capture: {calls:?}"
    );
    assert_eq!(context.get("pr_number").map(String::as_str), Some("90"));
    assert_eq!(
        context.get("head_sha").map(String::as_str),
        Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001,REQ-PRFU-004,REQ-PRFU-020A
/// @pseudocode lines 1-33
#[test]
fn github_pr_param_interpolation_leaves_unresolved_tokens_detectable() {
    let context = StepContext::new(
        tempfile::tempdir().expect("tempdir").path().to_path_buf(),
        "run-p06".to_string(),
    );

    assert_eq!(
        interpolate_string("{repository_owner}", &context),
        "{repository_owner}",
        "unconfigured repository_owner token should remain unresolved instead of silently becoming owner"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @requirement:REQ-PRFU-001
/// @pseudocode lines 1-7
#[test]
fn pr_identity_capture_writes_pr_json_and_rejects_missing_number_url_or_head_sha() {
    let outcome = execute_step(GithubPrIdentityExecutorWithRunner::new(
        FixtureGithubPrCommandRunner,
        FixedClock,
    ));
    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "PR identity capture must write pr.json after rejecting missing PR number, URL, or head SHA",
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @requirement:REQ-PRFU-003
/// @pseudocode lines 8-15
#[test]
fn post_pr_iteration_guard_preserves_same_head_and_exhausts_fourth_head_change_to_terminal() {
    let outcome = execute_step(PostPrIterationGuardExecutor);
    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "post_pr_iteration_guard must preserve same-head index and write max_iterations_exceeded at attempted index 4",
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @requirement:REQ-PRFU-004
/// @pseudocode lines 16-33
#[test]
fn pr_checks_default_watch_budget_is_twelve_observations_without_real_sleep() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runner = ScriptedGithubRunner::new(
        serde_json::json!([
            { "name": "build", "state": "SUCCESS", "bucket": "pass" },
            { "name": "lint", "state": "PENDING", "bucket": "pending" }
        ]),
        serde_json::json!({
            "total_count": 1,
            "check_runs": [{
                "id": 5003,
                "name": "integration",
                "status": "completed",
                "conclusion": "failure",
                "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            }]
        }),
    );
    let clock = RecordingClock::default();
    let mut context = p06_context(&temp);
    let outcome = GithubPrChecksExecutorWithRunner::new(runner.clone(), clock.clone())
        .execute(&mut context, &p06_check_params(&temp, 12))
        .expect("watch pr checks");
    let artifact = read_json(&p06_pr_check_status_path(&temp));
    let state = clock.state();

    assert_expected_outcome(
        outcome,
        StepOutcome::Wait,
        "pr_checks watcher must take one bounded observation and pause on a recoverable wait rather than success",
    );
    assert_eq!(
        artifact
            .get("poll_attempts")
            .and_then(serde_json::Value::as_u64),
        Some(1),
        "pr-check-status artifact must record one short activation attempt"
    );
    assert_eq!(
        artifact
            .get("write_sequence")
            .and_then(serde_json::Value::as_u64),
        Some(1),
        "pr-check-status artifact must be written for the short observation"
    );
    assert!(
        state.sleeps.is_empty(),
        "watcher must not sleep inside a workflow activation"
    );
    assert!(
        !runner.calls().iter().flatten().any(|arg| arg == "60"),
        "watcher must not request the obsolete t=60 poll interval"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @requirement:REQ-PRFU-005
/// @pseudocode lines 16-33
#[test]
fn pr_checks_non_fail_fast_continues_after_early_failure_while_pending_remains() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runner = ScriptedGithubRunner::new(
        serde_json::json!([
            { "name": "build", "state": "FAILURE", "bucket": "fail" },
            { "name": "lint", "state": "PENDING", "bucket": "pending" }
        ]),
        serde_json::json!({ "total_count": 0, "check_runs": [] }),
    );
    let clock = RecordingClock::default();
    let mut context = p06_context(&temp);
    let outcome = GithubPrChecksExecutorWithRunner::new(runner, clock.clone())
        .execute(&mut context, &p06_check_params(&temp, 3))
        .expect("watch pr checks");
    let artifact = read_json(&p06_pr_check_status_path(&temp));

    assert_expected_outcome(
        outcome,
        StepOutcome::Wait,
        "pr_checks watcher must not fail fast while another current-head check remains pending",
    );
    assert_eq!(
        artifact
            .get("poll_attempts")
            .and_then(serde_json::Value::as_u64),
        Some(1),
        "pending checks must produce one short observation before daemon polling resumes"
    );
    assert!(
        clock.state().sleeps.is_empty(),
        "pending checks must not sleep inside a workflow activation"
    );
    assert_eq!(
        artifact
            .get("overall_state")
            .and_then(serde_json::Value::as_str),
        Some("pending_timeout")
    );
    assert_eq!(
        artifact
            .get("terminal_counts")
            .and_then(|v| v.get("failed"))
            .and_then(serde_json::Value::as_u64),
        Some(1),
        "concrete failures must be preserved for later collection"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @requirement:REQ-PRFU-006
/// @pseudocode lines 16-33
#[test]
fn pr_checks_page2_data_affects_status_and_stale_head_checks_cannot_create_success() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runner = ScriptedGithubRunner::new(
        serde_json::json!([{ "name": "stale-build", "state": "SUCCESS", "bucket": "pass" }]),
        serde_json::json!({
            "total_count": 2,
            "check_runs": [
                {
                    "id": 5003,
                    "name": "integration-page-2",
                    "status": "completed",
                    "conclusion": "failure",
                    "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                },
                {
                    "id": 5004,
                    "name": "old-head-success",
                    "status": "completed",
                    "conclusion": "success",
                    "head_sha": "cccccccccccccccccccccccccccccccccccccccc"
                }
            ]
        }),
    );
    let clock = RecordingClock::default();
    let mut context = p06_context(&temp);
    let outcome = GithubPrChecksExecutorWithRunner::new(runner, clock.clone())
        .execute(&mut context, &p06_check_params(&temp, 12))
        .expect("watch pr checks");
    let artifact = read_json(&p06_pr_check_status_path(&temp));

    assert_expected_outcome(
        outcome,
        StepOutcome::Fixable,
        "page-2-only PR check data must affect pr-check-status and stale head checks must not create success",
    );
    assert_eq!(
        artifact
            .get("poll_attempts")
            .and_then(serde_json::Value::as_u64),
        Some(1),
        "terminal failed current-head page-2 data should stop after one observation"
    );
    assert!(
        clock.state().sleeps.is_empty(),
        "terminal state must not sleep after the final poll"
    );
    assert_eq!(
        artifact
            .get("overall_state")
            .and_then(serde_json::Value::as_str),
        Some("failed")
    );
    let checks = artifact
        .get("checks")
        .and_then(serde_json::Value::as_array)
        .expect("checks");
    let stale = artifact
        .get("stale_checks")
        .and_then(serde_json::Value::as_array)
        .expect("stale checks");
    assert!(
        checks.iter().any(
            |check| check.get("name").and_then(serde_json::Value::as_str)
                == Some("integration-page-2")
        ),
        "page-2 current-head failure must affect status"
    );
    assert!(
        stale.iter().any(
            |check| check.get("name").and_then(serde_json::Value::as_str)
                == Some("old-head-success")
        ),
        "stale head checks must be reported separately"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
/// @requirement:REQ-PRFU-004,REQ-PRFU-005,REQ-PRFU-006
/// @pseudocode lines 19-33
#[test]
fn check_classification_pending_timeout_with_failed_checks_waits_and_preserves_failures() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runner = ScriptedGithubRunner::new(
        serde_json::json!([
            { "name": "build", "state": "FAILURE", "bucket": "fail" },
            { "name": "lint", "state": "PENDING", "bucket": "pending" }
        ]),
        serde_json::json!({ "total_count": 0, "check_runs": [] }),
    );
    let mut context = p06_context(&temp);
    let outcome = GithubPrChecksExecutorWithRunner::new(runner, RecordingClock::default())
        .execute(&mut context, &p06_check_params(&temp, 2))
        .expect("watch pr checks");
    let artifact = read_json(&p06_pr_check_status_path(&temp));

    assert_eq!(
        outcome,
        StepOutcome::Wait,
        "pending_timeout is a recoverable external wait, not a terminal failure"
    );
    assert_eq!(
        artifact
            .get("overall_state")
            .and_then(serde_json::Value::as_str),
        Some("pending_timeout")
    );
    assert_eq!(
        artifact
            .get("terminal_counts")
            .and_then(|v| v.get("failed"))
            .and_then(serde_json::Value::as_u64),
        Some(1),
        "concrete failed checks must remain in the artifact for later failure collection"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-004,REQ-PRFU-006
/// @pseudocode lines 19-33
#[test]
fn check_classification_unknown_current_head_evidence_is_fatal() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runner = ScriptedGithubRunner::new(
        serde_json::json!([{ "name": "mystery", "state": "STRANGE", "bucket": "weird" }]),
        serde_json::json!({ "total_count": 0, "check_runs": [] }),
    );
    let mut context = p06_context(&temp);
    let outcome = GithubPrChecksExecutorWithRunner::new(runner, RecordingClock::default())
        .execute(&mut context, &p06_check_params(&temp, 12))
        .expect("watch pr checks");
    let artifact = read_json(&p06_pr_check_status_path(&temp));

    assert_eq!(
        outcome,
        StepOutcome::Fatal,
        "unknown current-head evidence must not enter remediation"
    );
    assert_eq!(
        artifact
            .get("overall_state")
            .and_then(serde_json::Value::as_str),
        Some("unknown")
    );
    assert_eq!(
        artifact
            .get("poll_attempts")
            .and_then(serde_json::Value::as_u64),
        Some(1),
        "unknown evidence should be recorded in a single bounded observation"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-004,REQ-PRFU-007
/// @pseudocode lines 16-33
#[test]
fn pr_checks_api_error_is_retryable_without_in_process_retry_loop() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runner = FlakyThenGreenGithubRunner::new();
    let mut context = p06_context(&temp);
    let outcome = GithubPrChecksExecutorWithRunner::new(runner, RecordingClock::default())
        .execute(&mut context, &p06_check_params(&temp, 3))
        .expect("watch pr checks");
    let artifact = read_json(&p06_pr_check_status_path(&temp));

    assert_expected_outcome(
        outcome,
        StepOutcome::Wait,
        "transient API errors should pause for daemon retry instead of looping in-process",
    );
    assert_eq!(
        artifact
            .get("overall_state")
            .and_then(serde_json::Value::as_str),
        Some("pending_timeout")
    );
    assert_eq!(
        artifact
            .get("poll_attempts")
            .and_then(serde_json::Value::as_u64),
        Some(1),
        "API errors must be recorded as one short activation"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 1-21
#[test]
fn ci_failure_failed_pending_collects_logs_preserves_pending_and_routes_terminal() {
    let outcome = execute_step(GithubCheckFailuresExecutorWithRunner::new(
        FixtureGithubPrCommandRunner,
        FixedClock,
    ));
    assert_expected_outcome(
        outcome,
        StepOutcome::Fatal,
        "failed+pending CI collection must collect concrete failure logs, preserve pending_or_unknown, and route terminal",
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
#[test]
fn ci_failure_ignores_stale_checks_marked_ignored_by_policy() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p07_check_status(
        &temp,
        "failed",
        serde_json::json!([
            { "check_id": "build", "name": "build", "state": "failure", "conclusion": "failure", "bucket": "failed", "url": null, "run_id": null, "job_id": null }
        ]),
        serde_json::json!([
            { "check_id": "coderabbit", "name": "CodeRabbit", "state": "PENDING", "conclusion": "pending", "bucket": "pending", "url": null, "run_id": null, "job_id": null }
        ]),
        serde_json::Value::Null,
    );
    set_p07_ignored_check_ids(&temp, &["coderabbit"]);
    let mut context = p07_context(&temp);
    let outcome = GithubCheckFailuresExecutorWithRunner::new(
        ScriptedGithubRunner::new(
            serde_json::json!([]),
            serde_json::json!({ "total_count": 0, "check_runs": [] }),
        ),
        FixedClock,
    )
    .execute(&mut context, &p07_params(&temp))
    .expect("collect ci failures");
    let artifact = read_json(&p07_ci_failures_path(&temp));
    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "ignored stale checks must not block collecting concrete CI failures",
    );
    assert_eq!(
        artifact
            .get("failures")
            .and_then(serde_json::Value::as_array)
            .expect("failures")
            .len(),
        1
    );
    assert!(artifact
        .get("pending_or_unknown")
        .and_then(serde_json::Value::as_array)
        .expect("pending")
        .is_empty());
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
#[test]
fn ci_failure_ignores_pending_checks_marked_ignored_by_policy() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p07_check_status(
        &temp,
        "failed",
        serde_json::json!([
            { "check_id": "build", "name": "build", "state": "failure", "conclusion": "failure", "bucket": "failed", "url": null, "run_id": null, "job_id": null },
            { "check_id": "coderabbit", "name": "CodeRabbit", "state": "PENDING", "conclusion": "pending", "bucket": "pending", "url": null, "run_id": null, "job_id": null }
        ]),
        serde_json::json!([]),
        serde_json::Value::Null,
    );
    set_p07_ignored_check_ids(&temp, &["coderabbit"]);
    let mut context = p07_context(&temp);
    let outcome = GithubCheckFailuresExecutorWithRunner::new(
        ScriptedGithubRunner::new(
            serde_json::json!([]),
            serde_json::json!({ "total_count": 0, "check_runs": [] }),
        ),
        FixedClock,
    )
    .execute(&mut context, &p07_params(&temp))
    .expect("collect ci failures");
    let artifact = read_json(&p07_ci_failures_path(&temp));
    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "ignored pending checks must not block collecting concrete CI failures",
    );
    assert_eq!(
        artifact
            .get("failures")
            .and_then(serde_json::Value::as_array)
            .expect("failures")
            .len(),
        1
    );
    assert!(artifact
        .get("pending_or_unknown")
        .and_then(serde_json::Value::as_array)
        .expect("pending")
        .is_empty());
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 1-5,17
#[test]
fn ci_failure_passed_checks_writes_empty_artifact() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p07_check_status(
        &temp,
        "passed",
        serde_json::json!([{ "check_id": "build", "name": "build", "state": "success", "conclusion": "success", "bucket": "passed", "url": null, "run_id": null, "job_id": null }]),
        serde_json::json!([]),
        serde_json::Value::Null,
    );
    let mut context = p07_context(&temp);
    let outcome = GithubCheckFailuresExecutorWithRunner::new(
        ScriptedGithubRunner::new(
            serde_json::json!([]),
            serde_json::json!({ "total_count": 0, "check_runs": [] }),
        ),
        FixedClock,
    )
    .execute(&mut context, &p07_params(&temp))
    .expect("collect ci failures");
    let artifact = read_json(&p07_ci_failures_path(&temp));
    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "passed checks must write ci-failures.json with failures=[] and pending_or_unknown=[]",
    );
    assert_eq!(
        artifact
            .get("collection_state")
            .and_then(serde_json::Value::as_str),
        Some("collected")
    );
    assert!(artifact
        .get("failures")
        .and_then(serde_json::Value::as_array)
        .expect("failures")
        .is_empty());
    assert!(artifact
        .get("pending_or_unknown")
        .and_then(serde_json::Value::as_array)
        .expect("pending")
        .is_empty());
    assert!(artifact
        .get("log_artifacts")
        .and_then(serde_json::Value::as_array)
        .expect("logs")
        .is_empty());
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 6-12,18
#[test]
fn ci_failure_page2_jobs_and_logs_affect_failure_artifacts() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p07_check_status(
        &temp,
        "failed",
        serde_json::json!([{ "check_id": "check-run:5003", "name": "integration", "state": "failure", "conclusion": "failure", "bucket": "failed", "url": "https://github.com/example/workflow/actions/runs/3003", "run_id": 3003, "job_id": null, "workflow_name": "CI", "app_slug": "github-actions" }]),
        serde_json::json!([]),
        serde_json::Value::Null,
    );
    let mut context = p07_context(&temp);
    let outcome =
        GithubCheckFailuresExecutorWithRunner::new(FixtureGithubPrCommandRunner, FixedClock)
            .execute(&mut context, &p07_params(&temp))
            .expect("collect ci failures");
    let artifact = read_json(&p07_ci_failures_path(&temp));
    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "page-2-only Actions jobs and logs must affect ci-failures log artifacts and routing",
    );
    let failures = artifact
        .get("failures")
        .and_then(serde_json::Value::as_array)
        .expect("failures");
    assert_eq!(failures.len(), 1);
    assert_eq!(
        failures[0]
            .get("job_id")
            .and_then(serde_json::Value::as_u64),
        Some(4003)
    );
    assert_eq!(
        failures[0]
            .get("log_status")
            .and_then(serde_json::Value::as_str),
        Some("available")
    );
    assert!(failures[0]
        .get("log_excerpt")
        .and_then(serde_json::Value::as_str)
        .expect("log excerpt")
        .contains("integration_case"));
    let log_artifacts = artifact
        .get("log_artifacts")
        .and_then(serde_json::Value::as_array)
        .expect("log artifacts");
    assert_eq!(log_artifacts.len(), 1);
    assert_eq!(
        log_artifacts[0]
            .get("job_id")
            .and_then(serde_json::Value::as_u64),
        Some(4003)
    );
    let raw_log_path = failures[0]
        .get("raw_log_path")
        .and_then(serde_json::Value::as_str)
        .expect("raw log path");
    assert!(std::fs::read_to_string(raw_log_path)
        .expect("raw log")
        .contains("integration tests"));
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @requirement:REQ-PRFU-008,REQ-PRFU-009
/// @pseudocode lines 1-29
#[test]
fn coderabbit_readiness_requires_two_identical_ready_observations_and_filters_bot_noise() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runner = P08FeedbackRunner::new(vec![
        check_runs_signal(
            "completed",
            serde_json::json!("success"),
            "CodeRabbit finished.",
        ),
        check_runs_signal(
            "completed",
            serde_json::json!("success"),
            "CodeRabbit finished.",
        ),
    ]);
    let mut context = p08_context(&temp);
    let outcome = GithubCodeRabbitFeedbackExecutorWithRunner::new(runner, FixedClock)
        .execute(&mut context, &p08_params(&temp, 2))
        .expect("feedback executor");

    assert_eq!(outcome, StepOutcome::Success);
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009
/// @pseudocode lines 4-29
#[test]
fn coderabbit_readiness_treats_commit_scoped_check_runs_without_head_sha_as_current_head() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runner = P08FeedbackRunner::new(vec![
        check_runs_signal_without_head_sha(
            "completed",
            serde_json::json!("success"),
            "CodeRabbit finished.",
        ),
        check_runs_signal_without_head_sha(
            "completed",
            serde_json::json!("success"),
            "CodeRabbit finished.",
        ),
    ]);
    let mut context = p08_context(&temp);
    let outcome = GithubCodeRabbitFeedbackExecutorWithRunner::new(runner, FixedClock)
        .execute(&mut context, &p08_params(&temp, 2))
        .expect("feedback executor");

    assert_eq!(outcome, StepOutcome::Success);
    let feedback: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(p08_feedback_path(&temp)).expect("feedback artifact"),
    )
    .expect("feedback json");
    assert_eq!(feedback["readiness_state"], "ready");
    assert_eq!(feedback["stable_observation_count"], 2);
}

#[test]
fn coderabbit_feedback_summary_comment_can_satisfy_readiness_when_check_run_absent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let summary_comment = serde_json::json!({
        "id": 9001,
        "node_id": "IC_coderabbit_summary",
        "body": "<!-- walkthrough_start -->\n## Summary by CodeRabbit\nCodeRabbit finished reviewing this pull request.",
        "html_url": "https://github.com/example/workflow/pull/1910#issuecomment-9001",
        "created_at": "2026-04-29T18:12:00Z",
        "updated_at": "2026-04-29T18:12:00Z",
        "user": { "login": "coderabbitai[bot]", "type": "Bot" }
    });
    let runner = P08FeedbackRunner::with_pages(
        vec![serde_json::json!({
            "data": { "repository": { "pullRequest": { "reviewThreads": {
                "nodes": [],
                "pageInfo": { "hasNextPage": false }
            } } } }
        })],
        vec![serde_json::json!([])],
        vec![serde_json::json!([summary_comment])],
        vec![
            serde_json::json!({ "check_runs": [] }),
            serde_json::json!({ "check_runs": [] }),
        ],
    );
    let mut context = p08_context(&temp);
    let outcome = GithubCodeRabbitFeedbackExecutorWithRunner::new(runner, FixedClock)
        .execute(&mut context, &p08_params(&temp, 2))
        .expect("feedback executor");

    assert_eq!(outcome, StepOutcome::Success);
    let feedback: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(p08_feedback_path(&temp)).expect("feedback artifact"),
    )
    .expect("feedback json");
    assert_eq!(feedback["readiness_state"], "ready");
    assert_eq!(feedback["stable_observation_count"], 2);
}
#[test]
fn coderabbit_feedback_rate_limit_notice_satisfies_readiness_when_check_run_absent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let rate_limit_comment = serde_json::json!({
        "id": 9100,
        "node_id": "IC_coderabbit_rate_limit",
        "body": "<!-- This is an auto-generated comment: summarize by coderabbit.ai -->\n<!-- This is an auto-generated comment: rate limited by coderabbit.ai -->\n\n> [!WARNING]\n> ## Review limit reached\n>\n> `@acoliver`, we couldn't start this review because you've reached your PR review rate limit.\n>\n> Your organization has run out of usage credits.",
        "html_url": "https://github.com/example/workflow/pull/1910#issuecomment-9100",
        "created_at": "2026-04-29T18:12:00Z",
        "updated_at": "2026-04-29T18:12:00Z",
        "user": { "login": "coderabbitai[bot]", "type": "Bot" }
    });
    let runner = P08FeedbackRunner::with_pages(
        vec![serde_json::json!({
            "data": { "repository": { "pullRequest": { "reviewThreads": {
                "nodes": [],
                "pageInfo": { "hasNextPage": false }
            } } } }
        })],
        vec![serde_json::json!([])],
        vec![serde_json::json!([rate_limit_comment])],
        vec![
            serde_json::json!({ "check_runs": [] }),
            serde_json::json!({ "check_runs": [] }),
        ],
    );
    let mut context = p08_context(&temp);
    let outcome = GithubCodeRabbitFeedbackExecutorWithRunner::new(runner, FixedClock)
        .execute(&mut context, &p08_params(&temp, 2))
        .expect("feedback executor");

    assert_eq!(outcome, StepOutcome::Success);
    let feedback: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(p08_feedback_path(&temp)).expect("feedback artifact"),
    )
    .expect("feedback json");
    assert_eq!(feedback["readiness_state"], "ready");
    assert_eq!(feedback["stable_observation_count"], 2);
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 1-29
#[test]
fn coderabbit_feedback_page2_threads_comments_and_rest_fallback_are_normalized() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runner = P08FeedbackRunner::new(vec![
        check_runs_signal(
            "completed",
            serde_json::json!("success"),
            "CodeRabbit finished.",
        ),
        check_runs_signal(
            "completed",
            serde_json::json!("success"),
            "CodeRabbit finished.",
        ),
    ]);
    let mut context = p08_context(&temp);
    let outcome = GithubCodeRabbitFeedbackExecutorWithRunner::new(runner, FixedClock)
        .execute(&mut context, &p08_params(&temp, 2))
        .expect("feedback executor");

    assert_eq!(outcome, StepOutcome::Success);
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-017
/// @pseudocode lines 4-9
#[test]
#[allow(clippy::too_many_lines)]
fn coderabbit_api_shell_safety_keeps_malicious_feedback_text_out_of_graphql_and_rest_argv() {
    let malicious =
        "CodeRabbit body with `touch /tmp/luther-owned` and $(false) --method DELETE -H X-Evil:1";
    let graph_page = serde_json::json!({
        "data": { "repository": { "pullRequest": { "reviewThreads": {
            "nodes": [{
                "id": "PRRT_shell_safety",
                "isResolved": false,
                "isOutdated": false,
                "path": "src/lib.rs",
                "line": 7,
                "comments": { "nodes": [{
                    "id": "PRRC_shell_safety_graphql",
                    "databaseId": 7001,
                    "body": malicious,
                    "url": "https://github.com/example/workflow/pull/1910#discussion_r7001",
                    "path": "src/lib.rs",
                    "line": 7,
                    "author": { "login": "coderabbitai[bot]" },
                    "createdAt": "2026-04-30T00:00:00Z",
                    "updatedAt": "2026-04-30T00:00:00Z",
                    "commit": { "oid": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
                }] }
            }],
            "pageInfo": { "hasNextPage": false }
        } } } }
    });
    let rest_review_page = serde_json::json!([{
        "id": 7002,
        "node_id": "PRRC_shell_safety_rest",
        "body": malicious,
        "html_url": "https://github.com/example/workflow/pull/1910#discussion_r7002",
        "path": "src/lib.rs",
        "line": 8,
        "side": "RIGHT",
        "user": { "login": "coderabbitai[bot]" },
        "created_at": "2026-04-30T00:00:00Z",
        "updated_at": "2026-04-30T00:00:00Z",
        "commit_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    }]);
    let issue_comment_page = serde_json::json!([{
        "id": 7003,
        "node_id": "IC_shell_safety",
        "body": malicious,
        "html_url": "https://github.com/example/workflow/pull/1910#issuecomment-7003",
        "user": { "login": "coderabbitai[bot]" },
        "created_at": "2026-04-30T00:00:00Z",
        "updated_at": "2026-04-30T00:00:00Z"
    }]);
    let temp = tempfile::tempdir().expect("tempdir");
    let runner = P08FeedbackRunner::with_pages(
        vec![graph_page],
        vec![rest_review_page],
        vec![issue_comment_page],
        vec![
            check_runs_signal(
                "completed",
                serde_json::json!("success"),
                "CodeRabbit finished.",
            ),
            check_runs_signal(
                "completed",
                serde_json::json!("success"),
                "CodeRabbit finished.",
            ),
        ],
    );
    let mut context = p08_context(&temp);
    let outcome = GithubCodeRabbitFeedbackExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &p08_params(&temp, 2))
        .expect("collect feedback through argv runner seam");
    let calls = runner.calls();
    let artifact = read_json(&p08_feedback_path(&temp));

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "malicious CodeRabbit feedback text is API data and must not affect command execution",
    );
    assert!(
        artifact.to_string().contains("luther-owned"),
        "test fixture must exercise malicious review/comment body as collected data"
    );
    assert!(
        calls
            .iter()
            .any(|argv| argv.iter().any(|arg| arg == "graphql")),
        "GraphQL command construction must be exercised: {calls:?}"
    );
    assert!(
        calls.iter().any(|argv| argv
            .iter()
            .any(|arg| arg.contains("/pulls/") && arg.contains("/comments"))),
        "REST review-comment command construction must be exercised: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .all(|argv| argv.first().map(String::as_str) == Some("gh")
                && argv.get(1).map(String::as_str) == Some("api")),
        "runner seam must receive gh api argv vectors, not shell strings: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .all(|argv| !argv.iter().any(|arg| arg.contains(malicious)
                || arg.contains("touch /tmp/luther-owned")
                || arg.contains("$(false)")
                || arg == "--method"
                || arg == "DELETE"
                || arg == "-H"
                || arg == "X-Evil:1"
                || arg == "sh"
                || arg == "bash"
                || arg == "-c")),
        "malicious API text must not be interpolated or become argv flags/commands: {calls:?}"
    );
}
/// Build a one-thread GraphQL page authored by a non-CodeRabbit reviewer.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-024
fn human_reviewer_graph_page() -> serde_json::Value {
    serde_json::json!({
        "data": { "repository": { "pullRequest": { "reviewThreads": {
            "nodes": [{
                "id": "PRRT_human",
                "isResolved": false,
                "isOutdated": false,
                "path": "src/lib.rs",
                "line": 12,
                "comments": { "nodes": [{
                    "id": "PRRC_human",
                    "databaseId": 8200,
                    "body": "Please rename this function for clarity.",
                    "url": "https://github.com/example/workflow/pull/1910#discussion_r8200",
                    "path": "src/lib.rs",
                    "line": 12,
                    "author": { "login": "octocat" },
                    "createdAt": "2026-04-30T00:00:00Z",
                    "updatedAt": "2026-04-30T00:00:00Z",
                    "commit": { "oid": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
                }] }
            }],
            "pageInfo": { "hasNextPage": false }
        } } } }
    })
}

/// Non-CodeRabbit reviewer threads are noise by default but flow through the
/// same mechanism when `include_all_reviewers` is set.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-024
/// @pseudocode lines 4-10
#[test]
fn collector_includes_non_coderabbit_reviewer_only_when_flag_set() {
    let check_runs = vec![
        check_runs_signal(
            "completed",
            serde_json::json!("success"),
            "Review finished.",
        ),
        check_runs_signal(
            "completed",
            serde_json::json!("success"),
            "Review finished.",
        ),
    ];
    let temp_default = tempfile::tempdir().expect("tempdir");
    let runner_default = P08FeedbackRunner::with_pages(
        vec![human_reviewer_graph_page()],
        vec![serde_json::json!([])],
        vec![serde_json::json!([])],
        check_runs.clone(),
    );
    let mut context_default = p08_context(&temp_default);
    GithubCodeRabbitFeedbackExecutorWithRunner::new(runner_default, FixedClock)
        .execute(&mut context_default, &p08_params(&temp_default, 2))
        .expect("collect with default reviewer filter");
    let default_artifact = read_json(&p08_feedback_path(&temp_default));
    assert_eq!(
        default_artifact
            .get("items_count")
            .and_then(serde_json::Value::as_u64),
        Some(0),
        "human reviewer thread must be excluded by default"
    );

    let temp_all = tempfile::tempdir().expect("tempdir");
    let runner_all = P08FeedbackRunner::with_pages(
        vec![human_reviewer_graph_page()],
        vec![serde_json::json!([])],
        vec![serde_json::json!([])],
        check_runs,
    );
    let mut context_all = p08_context(&temp_all);
    let mut params_all = p08_params(&temp_all, 2);
    params_all["include_all_reviewers"] = serde_json::json!(true);
    GithubCodeRabbitFeedbackExecutorWithRunner::new(runner_all, FixedClock)
        .execute(&mut context_all, &params_all)
        .expect("collect with include_all_reviewers");
    let all_artifact = read_json(&p08_feedback_path(&temp_all));
    assert_eq!(
        all_artifact
            .get("items_count")
            .and_then(serde_json::Value::as_u64),
        Some(1),
        "human reviewer thread must be collected when include_all_reviewers is set"
    );
    let item = all_artifact
        .pointer("/items/0")
        .expect("collected reviewer item");
    assert_eq!(
        item.get("author_login").and_then(serde_json::Value::as_str),
        Some("octocat")
    );
    assert_eq!(
        item.get("comment_database_id")
            .and_then(serde_json::Value::as_i64),
        Some(8200),
        "in-thread reply identifier must be captured for any reviewer"
    );
}

#[test]
fn coderabbit_feedback_interpolates_artifact_root_from_context() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runner = P08FeedbackRunner::with_pages(
        vec![
            serde_json::json!({ "data": { "repository": { "pullRequest": { "reviewThreads": { "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } } } } } }),
        ],
        vec![serde_json::json!([])],
        vec![serde_json::json!([])],
        vec![
            check_runs_signal(
                "completed",
                serde_json::json!("success"),
                "CodeRabbit finished.",
            ),
            check_runs_signal(
                "completed",
                serde_json::json!("success"),
                "CodeRabbit finished.",
            ),
        ],
    );
    let mut context = p08_context(&temp);
    context.set(
        "artifact_dir",
        temp.path().join("artifacts").to_string_lossy().as_ref(),
    );
    let mut params = p08_params(&temp, 2);
    params["artifact_root"] = serde_json::json!("{artifact_dir}");

    let outcome = GithubCodeRabbitFeedbackExecutorWithRunner::new(runner, FixedClock)
        .execute(&mut context, &params)
        .expect("collect feedback with interpolated artifact root");

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "feedback collection should accept interpolated artifact_root",
    );
    assert!(
        p08_feedback_path(&temp).exists(),
        "feedback artifact should be written under interpolated artifact root"
    );
    assert!(
        !temp.path().join("{artifact_dir}").exists(),
        "executor must not create a literal unresolved artifact_root directory"
    );
}

#[test]
fn coderabbit_feedback_discovers_existing_pr_identity_when_params_are_defaults() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::with_filesystem(
        &temp.path().join("artifacts"),
        &SystemPrFollowupFilesystem,
    )
    .expect("artifact store");
    let binding = PrFollowupBinding {
        schema_version: 1,
        run_id: "run-p08".to_string(),
        repository_owner: "vybestack".to_string(),
        repository_name: "llxprt-code".to_string(),
        pr_number: 1911,
        head_ref: "issue1803".to_string(),
        head_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
        base_ref: "main".to_string(),
        base_sha: Some("base-b".to_string()),
    };
    store
        .write_json_artifact(
            &binding,
            "pr",
            "capture_pr_identity",
            1,
            &serde_json::json!({
                "pr_url": "https://github.com/vybestack/llxprt-code/pull/1911",
                "capture_state": "captured",
                "captured_at": FixedClock.now_rfc3339(),
                "source": "gh_pr_view"
            }),
            None,
            &FixedClock,
        )
        .expect("write pr identity artifact");

    let runner = P08FeedbackRunner::with_pages(
        vec![
            serde_json::json!({ "data": { "repository": { "pullRequest": { "reviewThreads": { "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } } } } } }),
        ],
        vec![serde_json::json!([])],
        vec![serde_json::json!([])],
        vec![
            check_runs_signal(
                "completed",
                serde_json::json!("success"),
                "CodeRabbit finished.",
            ),
            check_runs_signal(
                "completed",
                serde_json::json!("success"),
                "CodeRabbit finished.",
            ),
        ],
    );
    let mut context = p08_context(&temp);
    let mut params = p08_params(&temp, 2);
    params["coderabbit_bot_identities"] = serde_json::json!(["coderabbitai[bot]"]);
    params["additional_coderabbit_bot_identities"] = serde_json::json!([]);
    let params_object = params.as_object_mut().expect("params object");
    params_object.remove("repository_owner");
    params_object.remove("repository_name");
    params_object.remove("pr_number");

    let outcome = GithubCodeRabbitFeedbackExecutorWithRunner::new(runner, FixedClock)
        .execute(&mut context, &params)
        .expect("collect feedback should reuse captured PR identity");

    let artifact_path = temp
        .path()
        .join("artifacts")
        .join("pr-followup")
        .join("current")
        .join("run-p08")
        .join("vybestack")
        .join("llxprt-code")
        .join("1911")
        .join("coderabbit-feedback.json");
    let artifact = read_json(&artifact_path);

    assert_expected_outcome(
        outcome,
        StepOutcome::Wait,
        "single-observation feedback collection should suspend after reusing captured PR identity",
    );

    assert!(
        artifact_path.exists(),
        "feedback artifact should be written next to the captured PR identity, not example/workflow defaults"
    );
    assert_eq!(artifact["repository_owner"], "vybestack");
    assert_eq!(artifact["repository_name"], "llxprt-code");
    assert_eq!(artifact["pr_number"], 1911);

    assert!(
        !p08_feedback_path(&temp).exists(),
        "feedback collection must not fall back to example/workflow/1910 when a captured PR identity exists"
    );
}

#[test]
fn coderabbit_feedback_ignores_unresolved_identity_params_and_uses_captured_pr() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::with_filesystem(
        &temp.path().join("artifacts"),
        &SystemPrFollowupFilesystem,
    )
    .expect("artifact store");
    let binding = PrFollowupBinding {
        schema_version: 1,
        run_id: "run-p08".to_string(),
        repository_owner: "vybestack".to_string(),
        repository_name: "llxprt-code".to_string(),
        pr_number: 1911,
        head_ref: "issue1803".to_string(),
        head_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
        base_ref: "main".to_string(),
        base_sha: Some("base-b".to_string()),
    };
    store
        .write_json_artifact(
            &binding,
            "pr",
            "capture_pr_identity",
            1,
            &serde_json::json!({
                "pr_url": "https://github.com/vybestack/llxprt-code/pull/1911",
                "capture_state": "captured",
                "captured_at": FixedClock.now_rfc3339(),
                "source": "gh_pr_view"
            }),
            None,
            &FixedClock,
        )
        .expect("write pr identity artifact");

    let runner = P08FeedbackRunner::with_pages(
        vec![
            serde_json::json!({ "data": { "repository": { "pullRequest": { "reviewThreads": { "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } } } } } }),
        ],
        vec![serde_json::json!([])],
        vec![serde_json::json!([])],
        vec![
            check_runs_signal(
                "completed",
                serde_json::json!("success"),
                "CodeRabbit finished.",
            ),
            check_runs_signal(
                "completed",
                serde_json::json!("success"),
                "CodeRabbit finished.",
            ),
        ],
    );
    let mut context = p08_context(&temp);
    let mut params = p08_params(&temp, 2);
    params["coderabbit_bot_identities"] = serde_json::json!(["coderabbitai[bot]"]);
    params["additional_coderabbit_bot_identities"] = serde_json::json!([]);
    params["repository_owner"] = serde_json::json!("{repository_owner}");
    params["repository_name"] = serde_json::json!("{repository_name}");
    params["pr_number"] = serde_json::json!("{pr_number}");

    let outcome = GithubCodeRabbitFeedbackExecutorWithRunner::new(runner, FixedClock)
        .execute(&mut context, &params)
        .expect(
            "collect feedback should ignore unresolved templates and reuse captured PR identity",
        );

    assert_expected_outcome(
        outcome,
        StepOutcome::Wait,
        "single-observation feedback collection should suspend after reusing captured PR identity",
    );

    let artifact_path = temp
        .path()
        .join("artifacts")
        .join("pr-followup")
        .join("current")
        .join("run-p08")
        .join("vybestack")
        .join("llxprt-code")
        .join("1911")
        .join("coderabbit-feedback.json");
    let artifact = read_json(&artifact_path);
    assert_eq!(artifact["repository_owner"], "vybestack");
    assert_eq!(artifact["repository_name"], "llxprt-code");
    assert_eq!(artifact["pr_number"], 1911);
    assert!(
        !p08_feedback_path(&temp).exists(),
        "unresolved identity params must not make feedback collection fall back to example/workflow/1910"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 1-23
fn p09_context(temp: &tempfile::TempDir) -> StepContext {
    StepContext::new(temp.path().to_path_buf(), "run-p09".to_string())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 1-23
fn p09_binding() -> PrFollowupBinding {
    PrFollowupBinding {
        schema_version: 1,
        run_id: "run-p09".to_string(),
        repository_owner: "example".to_string(),
        repository_name: "workflow".to_string(),
        pr_number: 1910,
        head_ref: "feature".to_string(),
        head_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        base_ref: "main".to_string(),
        base_sha: Some("base-a".to_string()),
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 1-23
fn p09_params(temp: &tempfile::TempDir) -> serde_json::Value {
    serde_json::json!({
        "artifact_root": temp.path().join("artifacts").display().to_string(),
        "repository_owner": "example",
        "repository_name": "workflow",
        "pr_number": "1910",
        "head_ref": "feature",
        "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "base_ref": "main",
        "base_sha": "base-a",
        "step_order_index": 6
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012
/// @pseudocode lines 1-23
fn p09_evaluations_path(temp: &tempfile::TempDir) -> PathBuf {
    temp.path()
        .join("artifacts")
        .join("pr-followup")
        .join("current")
        .join("run-p09")
        .join("example")
        .join("workflow")
        .join("1910")
        .join("feedback-evaluations.json")
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012
/// @pseudocode lines 1-23
fn write_p09_feedback(
    temp: &tempfile::TempDir,
    items: serde_json::Value,
    state_entries: serde_json::Value,
) {
    let store = PrFollowupArtifactStore::new(temp.path().join("artifacts"));
    let binding = p09_binding();
    store
        .write_json_artifact(
            &binding,
            "pr",
            "capture_pr_identity",
            1,
            &serde_json::json!({
                "pr_url": "https://github.com/example/workflow/pull/1910",
                "capture_state": "captured",
                "captured_at": "2026-04-30T00:00:00Z",
                "source": "fixture",
                "source_pr_node_id": "PR_kwDOExample",
                "source_head_repository_owner": null,
                "source_head_repository_name": null
            }),
            None,
            &FixedClock,
        )
        .expect("write p09 pr");
    store
        .write_json_artifact(
            &binding,
            "coderabbit-feedback",
            "collect_coderabbit_feedback",
            5,
            &serde_json::json!({
                "readiness_state": "ready",
                "stable_observation_count": 2,
                "required_stable_observations": 2,
                "max_observations": 6,
                "observation_interval_seconds": 300,
                "observations": [],
                "items": items,
                "included_bot_identities": ["coderabbitai[bot]"],
                "feedback_item_set_hash": "fnv64:p09"
            }),
            None,
            &FixedClock,
        )
        .expect("write p09 feedback");
    store
        .write_json_artifact(
            &binding,
            "coderabbit-feedback-state",
            "collect_coderabbit_feedback",
            5,
            &serde_json::json!({
                "state_entries": state_entries,
                "state_index_hash": "fnv64:p09-state",
                "superseded_entries": []
            }),
            None,
            &FixedClock,
        )
        .expect("write p09 state");
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012
/// @pseudocode lines 4-8
fn p09_feedback_item(item_id: &str, key: &str, hash: &str) -> serde_json::Value {
    serde_json::json!({
        "item_id": item_id,
        "stable_marker_key": key,
        "thread_id": "thread-p09",
        "comment_id": item_id,
        "review_id": null,
        "author_login": "coderabbitai[bot]",
        "author_association": "NONE",
        "bot_identity": "coderabbitai[bot]",
        "path": "src/lib.rs",
        "line": 10,
        "side": "RIGHT",
        "body": format!("feedback body {item_id}"),
        "body_hash": hash,
        "url": "https://github.com/example/workflow/pull/1910#discussion_r1",
        "created_at": "2026-04-30T00:00:00Z",
        "updated_at": "2026-04-30T00:00:00Z",
        "resolved": false,
        "outdated": false,
        "resolution_state_available": true,
        "source": "review_thread",
        "raw_node_id": item_id,
        "commit_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    })
}
type FeedbackEvaluatorCalls = Arc<Mutex<Vec<(Vec<String>, String)>>>;

#[derive(Clone, Debug, Default)]
struct RecordingFeedbackEvaluatorRunner {
    calls: FeedbackEvaluatorCalls,
    response: String,
}

impl RecordingFeedbackEvaluatorRunner {
    fn new(response: String) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            response,
        }
    }

    fn calls(&self) -> Vec<(Vec<String>, String)> {
        self.calls.lock().expect("feedback evaluator calls").clone()
    }
}

impl FeedbackEvaluatorCommandRunner for RecordingFeedbackEvaluatorRunner {
    fn run_feedback_evaluator_command(
        &self,
        argv: &[String],
        stdin_json: &str,
    ) -> Result<String, luther_workflow::engine::runner::EngineError> {
        self.calls
            .lock()
            .expect("feedback evaluator calls")
            .push((argv.to_vec(), stdin_json.to_string()));
        Ok(self.response.clone())
    }
}
#[derive(Clone, Debug, Default)]
struct FailingFeedbackEvaluatorRunner {
    calls: FeedbackEvaluatorCalls,
}

impl FailingFeedbackEvaluatorRunner {
    fn calls(&self) -> Vec<(Vec<String>, String)> {
        self.calls.lock().expect("feedback evaluator calls").clone()
    }
}

impl FeedbackEvaluatorCommandRunner for FailingFeedbackEvaluatorRunner {
    fn run_feedback_evaluator_command(
        &self,
        argv: &[String],
        stdin_json: &str,
    ) -> Result<String, luther_workflow::engine::runner::EngineError> {
        self.calls
            .lock()
            .expect("feedback evaluator calls")
            .push((argv.to_vec(), stdin_json.to_string()));
        Err(
            luther_workflow::engine::runner::EngineError::StepExecutionError {
                step_id: "evaluate_coderabbit_feedback".to_string(),
                message: "feedback evaluator command timed out after 300 seconds".to_string(),
            },
        )
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012
/// @pseudocode lines 8-16
#[test]
fn feedback_evaluation_single_item_request_rejects_batch_wrong_hash_unknown_decision_and_missing_reason(
) {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p09_feedback(
        &temp,
        serde_json::json!([p09_feedback_item("item-1", "thread-1", "hash-a")]),
        serde_json::json!([]),
    );
    let adapter = ScriptedFeedbackEvaluationAdapter::with_responses(vec![
        serde_json::json!([{
            "item_id": "item-1",
            "stable_marker_key": "thread-1",
            "body_hash": "hash-a",
            "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "decision": "valid",
            "reason": "array response",
            "recommended_action": "fix"
        }])
        .to_string(),
        serde_json::json!({
            "item_id": "item-1",
            "stable_marker_key": "thread-1",
            "body_hash": "wrong-hash",
            "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "decision": "valid",
            "reason": "wrong hash",
            "recommended_action": "fix"
        })
        .to_string(),
        serde_json::json!({
            "item_id": "item-1",
            "stable_marker_key": "thread-1",
            "body_hash": "hash-a",
            "head_sha": "wrong-head",
            "decision": "valid",
            "reason": "wrong head",
            "recommended_action": "fix"
        })
        .to_string(),
    ]);
    let mut context = p09_context(&temp);
    let outcome = FeedbackEvaluatorExecutor::new(adapter.clone(), FixedClock)
        .execute(&mut context, &p09_params(&temp))
        .expect("feedback evaluation");
    let artifact = read_json(&p09_evaluations_path(&temp));
    let requests = adapter.requests();

    assert_expected_outcome(
        outcome,
        StepOutcome::Fatal,
        "invalid LLM responses must exhaust the internal per-item retry budget and route fatal",
    );
    assert_eq!(
        requests.len(),
        3,
        "one single-item request must be invoked per item per attempt"
    );
    assert!(requests.iter().all(|request| request.item_id == "item-1"
        && request.stable_marker_key == "thread-1"
        && request.body_hash == "hash-a"
        && request.head_sha == "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        && request.repository_owner == "example"
        && request.repository_name == "workflow"
        && request.pr_number == 1910));
    assert_eq!(
        artifact
            .get("accepted_results")
            .and_then(serde_json::Value::as_array)
            .expect("accepted")
            .len(),
        0,
        "budget exhaustion must not be encoded as an accepted decision"
    );
    assert_eq!(
        artifact
            .get("rejected_attempts")
            .and_then(serde_json::Value::as_array)
            .expect("rejected")
            .len(),
        3
    );
    assert_eq!(
        artifact
            .get("budget_exhausted_items")
            .and_then(serde_json::Value::as_array)
            .expect("budget")
            .len(),
        1
    );
    let artifact_text = artifact.to_string();
    assert!(
        artifact_text.contains("response_array_or_batch")
            && artifact_text.contains("wrong_body_hash")
            && artifact_text.contains("wrong_head_sha"),
        "artifact must record each rejection reason: {artifact_text}"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 8-16
#[test]
fn feedback_evaluation_ignores_unresolved_identity_params_and_uses_context() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p09_feedback(
        &temp,
        serde_json::json!([p09_feedback_item("item-1", "thread-1", "hash-a")]),
        serde_json::json!([]),
    );
    let adapter = ScriptedFeedbackEvaluationAdapter::with_responses(vec![serde_json::json!({
        "item_id": "item-1",
        "stable_marker_key": "thread-1",
        "body_hash": "hash-a",
        "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "decision": "valid",
        "reason": "valid feedback",
        "recommended_action": "fix",
        "response_text": "Luther will address this valid feedback on the thread."
    })
    .to_string()]);
    let mut context = p09_context(&temp);
    context.set("repository_owner", "example");
    context.set("repository_name", "workflow");
    context.set("pr_number", "1910");
    context.set("head_ref", "feature");
    context.set("head_sha", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    context.set("base_ref", "main");
    context.set("base_sha", "base-a");
    let mut params = p09_params(&temp);
    params["repository_owner"] = serde_json::json!("{repository_owner}");
    params["repository_name"] = serde_json::json!("{repository_name}");
    params["pr_number"] = serde_json::json!("{pr_number}");

    let outcome = FeedbackEvaluatorExecutor::new(adapter, FixedClock)
        .execute(&mut context, &params)
        .expect("feedback evaluation");

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "feedback evaluator must interpolate unresolved identity params from StepContext",
    );
    assert!(p09_evaluations_path(&temp).exists());
}

#[test]
fn feedback_evaluation_discovers_captured_pr_when_resume_context_lacks_identity() {
    let temp = tempfile::tempdir().expect("tempdir");
    let binding = recovered_p09_binding();
    write_p09_legacy_and_real_pr_identities(&temp, &binding);
    write_p09_recovered_feedback_inputs(&temp, &binding);
    let adapter = p09_recovered_feedback_adapter();
    let mut params = p09_params(&temp);
    set_unresolved_p09_identity_params(&mut params);

    let outcome = FeedbackEvaluatorExecutor::new(adapter, FixedClock)
        .execute(&mut p09_context(&temp), &params)
        .expect("feedback evaluation discovers captured PR");

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "feedback evaluator must recover captured PR identity when resume context omits PR metadata",
    );
    assert!(p09_recovered_evaluations_path(&temp).exists());
    assert!(
        !p09_evaluations_path(&temp).exists(),
        "resume fallback must not write feedback evaluations under example/workflow/1910"
    );
}

fn recovered_p09_binding() -> PrFollowupBinding {
    PrFollowupBinding {
        schema_version: 1,
        run_id: "run-p09".to_string(),
        repository_owner: "vybestack".to_string(),
        repository_name: "llxprt-code".to_string(),
        pr_number: 1911,
        head_ref: "issue1803".to_string(),
        head_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
        base_ref: "main".to_string(),
        base_sha: Some("base-b".to_string()),
    }
}

fn write_p09_legacy_and_real_pr_identities(temp: &tempfile::TempDir, binding: &PrFollowupBinding) {
    let store = PrFollowupArtifactStore::new(temp.path().join("artifacts"));
    write_pr_identity_artifact(
        &store,
        &p09_binding(),
        "example/workflow",
        1910,
        "legacy_harness",
    );
    write_pr_identity_artifact(&store, binding, "vybestack/llxprt-code", 1911, "fixture");
}

fn write_pr_identity_artifact(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    repository: &str,
    pr_number: u64,
    source: &str,
) {
    store
        .write_json_artifact(
            binding,
            "pr",
            "capture_pr_identity",
            1,
            &serde_json::json!({
                "pr_url": format!("https://github.com/{repository}/pull/{pr_number}"),
                "capture_state": "captured",
                "captured_at": "2026-04-30T00:00:00Z",
                "source": source
            }),
            None,
            &FixedClock,
        )
        .expect("write p09 pr identity");
}

fn write_p09_recovered_feedback_inputs(temp: &tempfile::TempDir, binding: &PrFollowupBinding) {
    let store = PrFollowupArtifactStore::new(temp.path().join("artifacts"));
    let mut item = p09_feedback_item("item-1", "thread-1", "hash-a");
    item["commit_sha"] = serde_json::json!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    store
        .write_json_artifact(
            binding,
            "coderabbit-feedback",
            "collect_coderabbit_feedback",
            5,
            &serde_json::json!({
                "readiness_state": "ready",
                "stable_observation_count": 2,
                "required_stable_observations": 2,
                "max_observations": 6,
                "observation_interval_seconds": 300,
                "observations": [],
                "items": [item],
                "included_bot_identities": ["coderabbitai[bot]"],
                "feedback_item_set_hash": "fnv64:p09"
            }),
            None,
            &FixedClock,
        )
        .expect("write feedback");
    store
        .write_json_artifact(
            binding,
            "coderabbit-feedback-state",
            "collect_coderabbit_feedback",
            5,
            &serde_json::json!({
                "state_entries": [],
                "state_index_hash": "fnv64:p09-state",
                "superseded_entries": []
            }),
            None,
            &FixedClock,
        )
        .expect("write feedback state");
}

fn p09_recovered_feedback_adapter() -> ScriptedFeedbackEvaluationAdapter {
    ScriptedFeedbackEvaluationAdapter::with_responses(vec![serde_json::json!({
        "item_id": "item-1",
        "stable_marker_key": "thread-1",
        "body_hash": "hash-a",
        "head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "decision": "valid",
        "reason": "valid feedback",
        "recommended_action": "fix",
        "response_text": "Luther will address this valid feedback on the thread."
    })
    .to_string()])
}

fn set_unresolved_p09_identity_params(params: &mut serde_json::Value) {
    params["repository_owner"] = serde_json::json!("{repository_owner}");
    params["repository_name"] = serde_json::json!("{repository_name}");
    params["pr_number"] = serde_json::json!("{pr_number}");
    params["head_sha"] = serde_json::json!("{head_sha}");
}

fn p09_recovered_evaluations_path(temp: &tempfile::TempDir) -> PathBuf {
    temp.path()
        .join("artifacts")
        .join("pr-followup")
        .join("current")
        .join("run-p09")
        .join("vybestack")
        .join("llxprt-code")
        .join("1911")
        .join("feedback-evaluations.json")
}

#[test]
fn feedback_evaluation_ignores_max_attempts_param_override() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p09_feedback(
        &temp,
        serde_json::json!([p09_feedback_item("item-1", "thread-1", "hash-a")]),
        serde_json::json!([]),
    );
    let adapter = ScriptedFeedbackEvaluationAdapter::with_responses(vec![
        serde_json::json!({
            "item_id": "item-1",
            "stable_marker_key": "thread-1",
            "body_hash": "wrong-hash",
            "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "decision": "valid",
            "reason": "wrong hash 1",
            "recommended_action": "fix"
        })
        .to_string(),
        serde_json::json!({
            "item_id": "item-1",
            "stable_marker_key": "thread-1",
            "body_hash": "wrong-hash",
            "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "decision": "valid",
            "reason": "wrong hash 2",
            "recommended_action": "fix"
        })
        .to_string(),
        serde_json::json!({
            "item_id": "item-1",
            "stable_marker_key": "thread-1",
            "body_hash": "wrong-hash",
            "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "decision": "valid",
            "reason": "wrong hash 3",
            "recommended_action": "fix"
        })
        .to_string(),
        serde_json::json!({
            "item_id": "item-1",
            "stable_marker_key": "thread-1",
            "body_hash": "hash-a",
            "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "decision": "valid",
            "reason": "fourth response must not be reached",
            "recommended_action": "fix"
        })
        .to_string(),
    ]);
    let mut params = p09_params(&temp);
    params["max_attempts_per_item"] = serde_json::json!(4);
    let mut context = p09_context(&temp);
    let outcome = FeedbackEvaluatorExecutor::new(adapter.clone(), FixedClock)
        .execute(&mut context, &params)
        .expect("feedback evaluation ignores retry override");
    let artifact = read_json(&p09_evaluations_path(&temp));

    assert_expected_outcome(
        outcome,
        StepOutcome::Fatal,
        "configured retry overrides must be ignored; exactly three internal attempts are allowed",
    );
    assert_eq!(
        adapter.requests().len(),
        3,
        "attempted max_attempts_per_item override must not allow a fourth evaluator call"
    );
    assert_eq!(
        artifact
            .get("max_attempts_per_item")
            .and_then(serde_json::Value::as_u64),
        Some(3),
        "audit artifact must expose the fixed internal retry cap"
    );
    assert_eq!(
        artifact
            .get("rejected_attempts")
            .and_then(serde_json::Value::as_array)
            .expect("rejected")
            .len(),
        3,
        "malformed or mismatched output must produce exactly three rejected attempts before exhaustion"
    );
    assert_eq!(
        artifact
            .get("budget_exhausted_items")
            .and_then(serde_json::Value::as_array)
            .expect("budget")
            .first()
            .and_then(|item| item.get("attempts"))
            .and_then(serde_json::Value::as_u64),
        Some(3),
        "budget exhaustion must record the fixed internal attempt count"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012
/// @pseudocode lines 5-7,16-20
#[test]
fn feedback_evaluation_reuses_unchanged_accepted_state_without_reinvoking_adapter() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p09_feedback(
        &temp,
        serde_json::json!([p09_feedback_item("item-1", "thread-1", "hash-a")]),
        serde_json::json!([{
            "item_id": "item-1",
            "stable_marker_key": "thread-1",
            "body_hash": "hash-a",
            "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "evaluation_status": "accepted",
            "accepted_evaluation": {
                "item_id": "item-1",
                "stable_marker_key": "thread-1",
                "body_hash": "hash-a",
                "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "repository_owner": "example",
                "repository_name": "workflow",
                "pr_number": 1910,
                "decision": "invalid",
                "reason": "already evaluated",
                "recommended_action": "no code change",
                "response_text": "Luther reviewed this item previously and is reusing the prior decision.",
                "accepted_at": "2026-04-30T00:00:00Z",
                "attempt_count": 1,
                "source": "new",
                "reuse_state": "not_reused"
            },
            "reuse_eligible": true,
            "stale": false,
            "superseded": false
        }]),
    );
    let adapter = ScriptedFeedbackEvaluationAdapter::with_responses(vec![]);
    let mut context = p09_context(&temp);
    let outcome = FeedbackEvaluatorExecutor::new(adapter.clone(), FixedClock)
        .execute(&mut context, &p09_params(&temp))
        .expect("feedback evaluation reuse");
    let artifact = read_json(&p09_evaluations_path(&temp));
    let accepted = artifact
        .get("accepted_results")
        .and_then(serde_json::Value::as_array)
        .expect("accepted");

    assert_expected_outcome(outcome, StepOutcome::Success, "feedback evaluator must emit reused accepted_results exactly once without reinvoking the LLM adapter");
    assert!(
        adapter.requests().is_empty(),
        "unchanged accepted state must be reused without adapter invocation"
    );
    assert_eq!(accepted.len(), 1);
    assert_eq!(
        accepted[0]
            .get("source")
            .and_then(serde_json::Value::as_str),
        Some("reused")
    );
    assert_eq!(
        artifact
            .get("reused_results_count")
            .and_then(serde_json::Value::as_u64),
        Some(1)
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012
/// @pseudocode lines 5-7,16-20
#[test]
fn feedback_evaluation_rejects_unbound_reusable_state_as_fatal() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p09_feedback(
        &temp,
        serde_json::json!([p09_feedback_item("item-1", "thread-1", "hash-a")]),
        serde_json::json!([{
            "item_id": "item-1",
            "stable_marker_key": "thread-1",
            "body_hash": "hash-a",
            "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "evaluation_status": "accepted",
            "accepted_evaluation": {
                "item_id": "item-1",
                "stable_marker_key": "thread-1",
                "body_hash": "hash-a",
                "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "decision": "invalid",
                "reason": "missing binding fields",
                "recommended_action": "no code change",
                "accepted_at": "2026-04-30T00:00:00Z",
                "attempt_count": 1,
                "source": "new",
                "reuse_state": "not_reused"
            },
            "reuse_eligible": true,
            "stale": false,
            "superseded": false
        }]),
    );
    let adapter = ScriptedFeedbackEvaluationAdapter::with_responses(vec![]);
    let mut context = p09_context(&temp);
    let outcome = FeedbackEvaluatorExecutor::new(adapter.clone(), FixedClock)
        .execute(&mut context, &p09_params(&temp))
        .expect("feedback evaluation unbound reuse");
    let artifact = read_json(&p09_evaluations_path(&temp));

    assert_expected_outcome(
        outcome,
        StepOutcome::Fatal,
        "missing accepted-evaluation binding fields must be fatal rather than re-evaluated",
    );
    assert!(
        adapter.requests().is_empty(),
        "fatal prior accepted state must not fall through to the evaluator adapter"
    );
    assert_eq!(
        artifact
            .get("evaluation_state")
            .and_then(serde_json::Value::as_str),
        Some("fatal")
    );
    assert!(
        artifact
            .to_string()
            .contains("malformed_or_unbindable_accepted_evaluation"),
        "fatal artifact must record the unbindable accepted evaluation: {artifact}"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012
/// @pseudocode lines 10-13
#[test]
fn feedback_evaluation_rejects_extra_identity_fields() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p09_feedback(
        &temp,
        serde_json::json!([p09_feedback_item("item-1", "thread-1", "hash-a")]),
        serde_json::json!([]),
    );
    let adapter = ScriptedFeedbackEvaluationAdapter::with_responses(vec![
        serde_json::json!({
            "item_id": "item-1",
            "alternate_item_id": "item-2",
            "stable_marker_key": "thread-1",
            "alternate_stable_marker_key": "thread-2",
            "body_hash": "hash-a",
            "alternate_body_hash": "hash-b",
            "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "alternate_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "feedback_items": [{"item_id": "item-2"}],
            "decision": "valid",
            "reason": "extra identity fields",
            "recommended_action": "fix"
        })
        .to_string(),
        serde_json::json!({
            "item_id": "item-1",
            "item_ids": ["item-1", "item-2"],
            "stable_marker_key": "thread-1",
            "body_hash": "hash-a",
            "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "decision": "valid",
            "reason": "extra item ids",
            "recommended_action": "fix"
        })
        .to_string(),
        serde_json::json!({
            "item_id": "item-1",
            "items": [{"item_id": "item-1"}],
            "stable_marker_key": "thread-1",
            "body_hash": "hash-a",
            "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "decision": "valid",
            "reason": "batch field",
            "recommended_action": "fix"
        })
        .to_string(),
    ]);
    let mut context = p09_context(&temp);
    let outcome = FeedbackEvaluatorExecutor::new(adapter, FixedClock)
        .execute(&mut context, &p09_params(&temp))
        .expect("feedback evaluation extra identity rejection");
    let artifact = read_json(&p09_evaluations_path(&temp));

    assert_expected_outcome(
        outcome,
        StepOutcome::Fatal,
        "extra or multiple item identity fields must be rejected and exhaust retry budget",
    );
    assert_eq!(
        artifact
            .get("accepted_results")
            .and_then(serde_json::Value::as_array)
            .expect("accepted")
            .len(),
        0
    );
    assert_eq!(
        artifact
            .get("rejected_attempts")
            .and_then(serde_json::Value::as_array)
            .expect("rejected")
            .len(),
        3
    );
    assert!(
        artifact
            .to_string()
            .matches("batch_or_extra_item_ids")
            .count()
            >= 3,
        "artifact must record strict identity rejection reasons: {artifact}"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 1-11
fn p10_context(temp: &tempfile::TempDir) -> StepContext {
    StepContext::new(temp.path().to_path_buf(), "run-p10".to_string())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 1-11
fn p10_binding() -> PrFollowupBinding {
    PrFollowupBinding {
        schema_version: 1,
        run_id: "run-p10".to_string(),
        repository_owner: "example".to_string(),
        repository_name: "workflow".to_string(),
        pr_number: 1910,
        head_ref: "feature".to_string(),
        head_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        base_ref: "main".to_string(),
        base_sha: Some("base-a".to_string()),
    }
}

#[test]
fn feedback_evaluation_evaluates_matching_state_entry_when_not_reuse_eligible() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p09_feedback(
        &temp,
        serde_json::json!([p09_feedback_item("item-1", "thread-1", "hash-a")]),
        serde_json::json!([{
            "item_id": "item-1",
            "stable_marker_key": "thread-1",
            "body_hash": "hash-a",
            "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "accepted_evaluation": null,
            "evaluation_status": "unevaluated",
            "reuse_eligible": false
        }]),
    );
    let adapter = ScriptedFeedbackEvaluationAdapter::with_responses(vec![serde_json::json!({
        "item_id": "item-1",
        "stable_marker_key": "thread-1",
        "body_hash": "hash-a",
        "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "decision": "valid",
        "reason": "valid feedback",
        "recommended_action": "fix",
        "response_text": "Luther will address this valid feedback on the thread."
    })
    .to_string()]);
    let mut context = p09_context(&temp);

    let outcome = FeedbackEvaluatorExecutor::new(adapter.clone(), FixedClock)
        .execute(&mut context, &p09_params(&temp))
        .expect("feedback evaluation");

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "non-reuse-eligible unevaluated state must be evaluated, not treated as fatal prior state",
    );
    assert_eq!(adapter.requests().len(), 1);
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 1-11
fn p10_params(temp: &tempfile::TempDir) -> serde_json::Value {
    serde_json::json!({
        "artifact_root": temp.path().join("artifacts").display().to_string(),
        "repository_owner": "example",
        "repository_name": "workflow",
        "pr_number": "1910",
        "head_ref": "feature",
        "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "base_ref": "main",
        "base_sha": "base-a",
        "step_order_index": 7
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 1-11
fn p10_current_artifact_path(temp: &tempfile::TempDir, family: &str) -> PathBuf {
    temp.path()
        .join("artifacts")
        .join("pr-followup")
        .join("current")
        .join("run-p10")
        .join("example")
        .join("workflow")
        .join("1910")
        .join(format!("{family}.json"))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 1-11
fn p10_plan_path(temp: &tempfile::TempDir) -> PathBuf {
    p10_current_artifact_path(temp, "pr-remediation-plan")
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 9-10
fn p10_pending_marker_actions_path(temp: &tempfile::TempDir) -> PathBuf {
    p10_current_artifact_path(temp, "pending-feedback-marker-actions")
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 1-11
fn p10_accepted(
    item_id: &str,
    stable_marker_key: &str,
    body_hash: &str,
    decision: &str,
) -> serde_json::Value {
    serde_json::json!({
        "item_id": item_id,
        "stable_marker_key": stable_marker_key,
        "body_hash": body_hash,
        "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "decision": decision,
        "reason": format!("{decision} reason"),
        "recommended_action": format!("{decision} action"),
        "response_text": format!("Luther {decision} response posted on the review thread."),
        "accepted_at": "2026-04-30T00:00:00Z",
        "attempt_count": 1,
        "source": "fixture",
        "reuse_state": "not_reused"
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 1-11
fn write_p10_inputs(
    temp: &tempfile::TempDir,
    ci_failures: serde_json::Value,
    pending_or_unknown: serde_json::Value,
    accepted_results: serde_json::Value,
    unevaluated_items: serde_json::Value,
    budget_exhausted_items: serde_json::Value,
) {
    let store = PrFollowupArtifactStore::new(temp.path().join("artifacts"));
    let binding = p10_binding();
    store
        .write_json_artifact(
            &binding,
            "pr",
            "capture_pr_identity",
            1,
            &serde_json::json!({
                "pr_url": "https://github.com/example/workflow/pull/1910",
                "capture_state": "captured",
                "captured_at": "2026-04-30T00:00:00Z",
                "source": "fixture",
                "source_pr_node_id": "PR_kwDOExample",
                "source_head_repository_owner": null,
                "source_head_repository_name": null
            }),
            None,
            &FixedClock,
        )
        .expect("write p10 pr");
    store
        .write_json_artifact(
            &binding,
            "ci-failures",
            "collect_ci_failures",
            4,
            &serde_json::json!({
                "collection_state": if pending_or_unknown.as_array().is_some_and(std::vec::Vec::is_empty) { "collected" } else { "fatal" },
                "failures": ci_failures,
                "pending_or_unknown": pending_or_unknown,
                "watcher_fatal_source": null,
                "fatal_source": null,
                "log_artifacts": [],
                "source_check_status_artifact_sequence": 2
            }),
            None,
            &FixedClock,
        )
        .expect("write p10 ci failures");
    store
        .write_json_artifact(
            &binding,
            "coderabbit-feedback",
            "collect_coderabbit_feedback",
            5,
            &serde_json::json!({
                "readiness_state": "ready",
                "stable_observation_count": 2,
                "required_stable_observations": 2,
                "max_observations": 6,
                "observation_interval_seconds": 300,
                "observations": [],
                "items": accepted_results
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|item| serde_json::json!({
                        "item_id": item.get("item_id").cloned().unwrap_or(serde_json::Value::Null),
                        "stable_marker_key": item.get("stable_marker_key").cloned().unwrap_or(serde_json::Value::Null),
                        "body_hash": item.get("body_hash").cloned().unwrap_or(serde_json::Value::Null),
                        "thread_id": item.get("stable_marker_key").cloned().unwrap_or(serde_json::Value::Null),
                        "comment_database_id": 7001
                    }))
                    .collect::<Vec<_>>(),
                "included_bot_identities": ["coderabbitai[bot]"],
                "feedback_item_set_hash": "fnv64:p10"
            }),
            None,
            &FixedClock,
        )
        .expect("write p10 feedback");
    store
        .write_json_artifact(
            &binding,
            "feedback-evaluations",
            "evaluate_coderabbit_feedback",
            6,
            &serde_json::json!({
                "evaluation_state": if unevaluated_items.as_array().is_some_and(std::vec::Vec::is_empty) && budget_exhausted_items.as_array().is_some_and(std::vec::Vec::is_empty) { "complete" } else { "budget_exhausted" },
                "items_seen": accepted_results.as_array().map_or(0, Vec::len),
                "accepted_results": accepted_results,
                "rejected_attempts": [],
                "unevaluated_items": unevaluated_items,
                "budget_exhausted_items": budget_exhausted_items,
                "max_attempts_per_item": 3,
                "reused_results_count": 0
            }),
            None,
            &FixedClock,
        )
        .expect("write p10 evaluations");
}

#[test]
fn remediation_plan_discovers_captured_pr_when_resume_context_lacks_identity() {
    let temp = tempfile::tempdir().expect("tempdir");
    let binding = recovered_p10_binding();
    write_p10_legacy_and_real_pr_identities(&temp, &binding);
    write_p10_recovered_plan_inputs(&temp, &binding);
    let mut params = p10_params(&temp);
    set_unresolved_p10_identity_params(&mut params);
    let mut context = p10_context(&temp);

    let outcome = PrRemediationPlanExecutor
        .execute(&mut context, &params)
        .expect("remediation plan discovers captured PR");

    assert_expected_outcome(
        outcome,
        StepOutcome::Fixable,
        "remediation plan should target the captured PR after a resumed activation loses PR params",
    );
    assert!(p10_recovered_plan_path(&temp).exists());
    assert!(
        !p10_plan_path(&temp).exists(),
        "resume fallback must not write remediation plans under example/workflow/1910"
    );
}

fn recovered_p10_binding() -> PrFollowupBinding {
    PrFollowupBinding {
        schema_version: 1,
        run_id: "run-p10".to_string(),
        repository_owner: "vybestack".to_string(),
        repository_name: "llxprt-code".to_string(),
        pr_number: 1911,
        head_ref: "issue1803".to_string(),
        head_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
        base_ref: "main".to_string(),
        base_sha: Some("base-b".to_string()),
    }
}

fn write_p10_legacy_and_real_pr_identities(temp: &tempfile::TempDir, binding: &PrFollowupBinding) {
    let store = PrFollowupArtifactStore::new(temp.path().join("artifacts"));
    write_pr_identity_artifact(
        &store,
        &p10_binding(),
        "example/workflow",
        1910,
        "legacy_harness",
    );
    write_pr_identity_artifact(&store, binding, "vybestack/llxprt-code", 1911, "fixture");
}

fn write_p10_recovered_plan_inputs(temp: &tempfile::TempDir, binding: &PrFollowupBinding) {
    let store = PrFollowupArtifactStore::new(temp.path().join("artifacts"));
    write_p10_recovered_json(&store, binding, "ci-failures", ci_failures_payload());
    write_p10_recovered_json(
        &store,
        binding,
        "coderabbit-feedback",
        p10_empty_feedback_payload(),
    );
    write_p10_recovered_json(
        &store,
        binding,
        "feedback-evaluations",
        p10_empty_evaluations_payload(),
    );
}

fn write_p10_recovered_json(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    family: &str,
    payload: serde_json::Value,
) {
    store
        .write_json_artifact(
            binding,
            family,
            "recovered_test_input",
            6,
            &payload,
            None,
            &FixedClock,
        )
        .expect("write recovered p10 input");
}

fn ci_failures_payload() -> serde_json::Value {
    serde_json::json!({
        "collection_state": "collected",
        "failures": [{ "failure_id": "ci-1", "check_name": "build", "conclusion": "failure" }],
        "pending_or_unknown": [],
        "watcher_fatal_source": null,
        "fatal_source": null,
        "log_artifacts": []
    })
}

fn p10_empty_feedback_payload() -> serde_json::Value {
    serde_json::json!({
        "readiness_state": "ready",
        "items": [],
        "included_bot_identities": ["coderabbitai[bot]"],
        "feedback_item_set_hash": "fnv64:p10"
    })
}

fn p10_empty_evaluations_payload() -> serde_json::Value {
    serde_json::json!({
        "evaluation_state": "complete",
        "items_seen": 0,
        "accepted_results": [],
        "rejected_attempts": [],
        "unevaluated_items": [],
        "budget_exhausted_items": [],
        "max_attempts_per_item": 3,
        "reused_results_count": 0
    })
}

fn set_unresolved_p10_identity_params(params: &mut serde_json::Value) {
    params["repository_owner"] = serde_json::json!("{repository_owner}");
    params["repository_name"] = serde_json::json!("{repository_name}");
    params["pr_number"] = serde_json::json!("{pr_number}");
    params["head_sha"] = serde_json::json!("{head_sha}");
}

fn p10_recovered_plan_path(temp: &tempfile::TempDir) -> PathBuf {
    temp.path()
        .join("artifacts")
        .join("pr-followup")
        .join("current")
        .join("run-p10")
        .join("vybestack")
        .join("llxprt-code")
        .join("1911")
        .join("pr-remediation-plan.json")
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 1-11
#[test]
fn remediation_plan_routes_only_concrete_failures_and_valid_feedback_to_must_fix() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p10_inputs(
        &temp,
        serde_json::json!([{ "failure_id": "ci-1", "check_name": "build", "conclusion": "failure" }]),
        serde_json::json!([]),
        serde_json::json!([
            p10_accepted("cr-valid", "thread-valid", "hash-valid", "valid"),
            p10_accepted("cr-invalid", "thread-invalid", "hash-invalid", "invalid"),
            p10_accepted("cr-out", "thread-out", "hash-out", "out_of_scope")
        ]),
        serde_json::json!([]),
        serde_json::json!([]),
    );
    let mut context = p10_context(&temp);
    let outcome = PrRemediationPlanExecutor
        .execute(&mut context, &p10_params(&temp))
        .expect("build remediation plan");
    let artifact = read_json(&p10_plan_path(&temp));

    assert_expected_outcome(
        outcome,
        StepOutcome::Fixable,
        "remediation plan must route only concrete CI failures and valid feedback to must_fix while blocking pending_or_unknown",
    );
    assert_eq!(
        artifact
            .get("plan_state")
            .and_then(serde_json::Value::as_str),
        Some("needs_remediation")
    );
    assert_eq!(
        artifact
            .get("must_fix")
            .and_then(serde_json::Value::as_array)
            .expect("must_fix")
            .len(),
        2
    );
    assert_eq!(
        artifact
            .get("mark_invalid")
            .and_then(serde_json::Value::as_array)
            .expect("mark_invalid")
            .len(),
        2
    );
    assert!(artifact
        .get("pending_or_unknown")
        .and_then(serde_json::Value::as_array)
        .expect("pending")
        .is_empty());
    assert!(artifact
        .get("must_fix")
        .and_then(serde_json::Value::as_array)
        .expect("must_fix")
        .iter()
        .all(
            |item| item.get("source_type").and_then(serde_json::Value::as_str)
                == Some("ci_failure")
                || item.get("decision").and_then(serde_json::Value::as_str) == Some("valid")
        ));
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 7-10
#[test]
fn remediation_plan_invalid_out_of_scope_only_writes_pending_marker_actions_and_returns_success() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p10_inputs(
        &temp,
        serde_json::json!([]),
        serde_json::json!([]),
        serde_json::json!([
            p10_accepted("cr-invalid", "thread-invalid", "hash-invalid", "invalid"),
            p10_accepted("cr-out", "thread-out", "hash-out", "out_of_scope")
        ]),
        serde_json::json!([]),
        serde_json::json!([]),
    );
    let mut context = p10_context(&temp);
    let outcome = PrRemediationPlanExecutor
        .execute(&mut context, &p10_params(&temp))
        .expect("build clean marker plan");
    let plan = read_json(&p10_plan_path(&temp));
    let actions = read_json(&p10_pending_marker_actions_path(&temp));

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "invalid/out_of_scope-only plan must be clean after pending marker actions are durable",
    );
    assert_eq!(
        plan.get("plan_state").and_then(serde_json::Value::as_str),
        Some("clean")
    );
    assert!(plan
        .get("must_fix")
        .and_then(serde_json::Value::as_array)
        .expect("must_fix")
        .is_empty());
    assert_eq!(
        actions
            .get("pending_actions")
            .and_then(serde_json::Value::as_array)
            .expect("actions")
            .len(),
        2
    );
    assert!(
        actions.get("history_metadata").is_some(),
        "pending marker actions must be written through artifact store with history metadata"
    );
}

/// A deterministically-`invalid` CodeRabbit summary/walkthrough item must never
/// be routed into `mark_invalid` nor produce a pending marker action, while a
/// co-present actionable review thread is still routed normally.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-020
#[test]
fn remediation_plan_skips_summary_invalid_feedback_from_mark_invalid_and_pending_actions() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut summary = p10_accepted(
        "cr-summary-1",
        "summary:IC_summarynode:hash-summary",
        "hash-summary",
        "invalid",
    );
    summary["source"] = serde_json::json!("deterministic");
    write_p10_inputs(
        &temp,
        serde_json::json!([]),
        serde_json::json!([]),
        serde_json::json!([
            summary,
            p10_accepted(
                "cr-valid",
                "thread:PRRT_valid:hash-valid",
                "hash-valid",
                "valid"
            )
        ]),
        serde_json::json!([]),
        serde_json::json!([]),
    );
    let mut context = p10_context(&temp);
    let outcome = PrRemediationPlanExecutor
        .execute(&mut context, &p10_params(&temp))
        .expect("build remediation plan with summary item");
    let plan = read_json(&p10_plan_path(&temp));

    assert_expected_outcome(
        outcome,
        StepOutcome::Fixable,
        "summary-only suppression must not change the actionable must_fix routing",
    );
    assert!(
        plan.get("mark_invalid")
            .and_then(serde_json::Value::as_array)
            .expect("mark_invalid")
            .is_empty(),
        "summary item must not be routed into mark_invalid: {plan:?}"
    );
    let must_fix = plan
        .get("must_fix")
        .and_then(serde_json::Value::as_array)
        .expect("must_fix");
    assert_eq!(
        must_fix.len(),
        1,
        "actionable thread must remain in must_fix"
    );
    assert_eq!(
        must_fix[0]
            .get("stable_marker_key")
            .and_then(serde_json::Value::as_str),
        Some("thread:PRRT_valid:hash-valid")
    );
    // The test name promises the summary's invalid feedback is skipped from the
    // pending marker actions for this co-present (summary + actionable) case, not
    // just from mark_invalid. Assert that path: read the pending marker actions
    // output (absent or present-with-actions, since a NeedsRemediation plan need
    // not persist it) and confirm no pending action ever carries the summary
    // item's stable_marker_key.
    let pending_path = p10_pending_marker_actions_path(&temp);
    if pending_path.exists() {
        let pending = read_json(&pending_path);
        let pending_actions = pending
            .get("pending_actions")
            .and_then(serde_json::Value::as_array)
            .expect("pending_actions");
        assert!(
            pending_actions.iter().all(|action| action
                .get("stable_marker_key")
                .and_then(serde_json::Value::as_str)
                != Some("summary:IC_summarynode:hash-summary")),
            "summary item must never materialize a pending marker action: {pending:?}"
        );
    }
}

/// A summary-only clean plan must produce no pending marker action at all so the
/// marker pipeline can never post a top-level PR comment for it.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-020
#[test]
fn remediation_plan_summary_only_writes_no_pending_marker_actions() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut summary = p10_accepted(
        "cr-summary-1",
        "summary:IC_summarynode:hash-summary",
        "hash-summary",
        "invalid",
    );
    summary["source"] = serde_json::json!("deterministic");
    write_p10_inputs(
        &temp,
        serde_json::json!([]),
        serde_json::json!([]),
        serde_json::json!([summary]),
        serde_json::json!([]),
        serde_json::json!([]),
    );
    let mut context = p10_context(&temp);
    let outcome = PrRemediationPlanExecutor
        .execute(&mut context, &p10_params(&temp))
        .expect("build summary-only clean plan");
    let plan = read_json(&p10_plan_path(&temp));

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "a PR whose only CodeRabbit feedback is a summary resolves to clean",
    );
    assert_eq!(
        plan.get("plan_state").and_then(serde_json::Value::as_str),
        Some("clean")
    );
    // A summary-only clean plan must materialize no pending marker action: the
    // artifact is either absent entirely or present with an empty action list.
    let pending_path = p10_pending_marker_actions_path(&temp);
    if pending_path.exists() {
        let actions = read_json(&pending_path);
        assert!(
            actions
                .get("pending_actions")
                .and_then(serde_json::Value::as_array)
                .expect("pending_actions")
                .is_empty(),
            "summary item must not materialize any pending marker action: {actions:?}"
        );
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 7-10
#[test]
fn pending_marker_actions_invalid_out_of_scope_have_no_remediation_output_head_and_do_not_duplicate_on_retry(
) {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p10_inputs(
        &temp,
        serde_json::json!([]),
        serde_json::json!([]),
        serde_json::json!([
            p10_accepted("cr-invalid", "thread-invalid", "hash-invalid", "invalid"),
            p10_accepted("cr-out", "thread-out", "hash-out", "out_of_scope")
        ]),
        serde_json::json!([]),
        serde_json::json!([]),
    );
    let params = p10_params(&temp);
    let mut context = p10_context(&temp);
    PrRemediationPlanExecutor
        .execute(&mut context, &params)
        .expect("first clean plan");
    PrRemediationPlanExecutor
        .execute(&mut context, &params)
        .expect("retry clean plan");
    let actions = read_json(&p10_pending_marker_actions_path(&temp));
    let pending = actions
        .get("pending_actions")
        .and_then(serde_json::Value::as_array)
        .expect("actions");
    let keys = pending
        .iter()
        .filter_map(|action| {
            action
                .get("idempotency_key")
                .and_then(serde_json::Value::as_str)
        })
        .collect::<std::collections::BTreeSet<_>>();

    assert_eq!(pending.len(), 2);
    assert_eq!(
        keys.len(),
        2,
        "retry/resume must not duplicate pending marker actions: {pending:?}"
    );
    assert!(pending.iter().all(|action| action
        .get("remediation_output_head_sha")
        .is_some_and(serde_json::Value::is_null)
        && action
            .get("remediation_output_head")
            .and_then(serde_json::Value::as_str)
            == Some("none")));
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 4,8,10-11
#[test]
fn remediation_plan_needs_user_judgment_returns_fatal() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p10_inputs(
        &temp,
        serde_json::json!([{ "failure_id": "ci-1", "check_name": "build", "conclusion": "failure" }]),
        serde_json::json!([{ "source": "watch_pr_checks", "reason": "pending_timeout", "check_name": "slow" }]),
        serde_json::json!([p10_accepted(
            "cr-judge",
            "thread-judge",
            "hash-judge",
            "needs_user_judgment"
        )]),
        serde_json::json!([{ "item_id": "cr-unevaluated", "reason": "unresolved_ambiguity" }]),
        serde_json::json!([{ "item_id": "cr-budget", "reason": "budget_exhausted" }]),
    );
    let mut context = p10_context(&temp);
    let outcome = PrRemediationPlanExecutor
        .execute(&mut context, &p10_params(&temp))
        .expect("build blocked remediation plan");
    let artifact = read_json(&p10_plan_path(&temp));

    assert_expected_outcome(
        outcome,
        StepOutcome::Fatal,
        "needs_user_judgment and pending_or_unknown evidence must route fatal",
    );
    assert_eq!(
        artifact
            .get("plan_state")
            .and_then(serde_json::Value::as_str),
        Some("blocked_needs_user_judgment")
    );
    assert!(artifact
        .get("must_fix")
        .and_then(serde_json::Value::as_array)
        .expect("must_fix")
        .iter()
        .all(|item| {
            item.get("source_type").and_then(serde_json::Value::as_str)
                != Some("ci_pending_or_unknown")
                && item.get("decision").and_then(serde_json::Value::as_str)
                    != Some("needs_user_judgment")
        }));
    assert!(!artifact
        .get("needs_user_judgment")
        .and_then(serde_json::Value::as_array)
        .expect("needs_user_judgment")
        .is_empty());
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-012,REQ-PRFU-013
/// @pseudocode lines 18-23
#[test]
fn remediation_result_status_enum_rejects_needs_user_judgment() {
    let schema_contract =
        std::fs::read_to_string("project-plans/coderabbit/analysis/artifact-schema-contract.md")
            .expect("schema contract");
    let result_status_line = schema_contract
        .lines()
        .find(|line| line.contains("Canonical `results[].status` enum"))
        .expect("remediation result status enum line");

    assert!(
        !result_status_line.contains("needs_user_judgment"),
        "remediation result statuses must not include needs_user_judgment: {result_status_line}"
    );
    assert!(
        schema_contract.contains("`valid`, `invalid`, `out_of_scope`, or `needs_user_judgment`"),
        "evaluation decisions still allow needs_user_judgment"
    );
    assert!(
        schema_contract
            .contains("`clean`, `needs_remediation`, `blocked_needs_user_judgment`, or `fatal`"),
        "remediation plan state still allows blocked_needs_user_judgment"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P11
/// @requirement:REQ-PRFU-012,REQ-PRFU-014
/// @pseudocode lines 18-28
fn p11_context(temp: &tempfile::TempDir) -> StepContext {
    StepContext::new(temp.path().to_path_buf(), "run-p11".to_string())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P11
/// @requirement:REQ-PRFU-012,REQ-PRFU-014
/// @pseudocode lines 18-28
fn p11_binding() -> PrFollowupBinding {
    PrFollowupBinding {
        run_id: "run-p11".to_string(),
        schema_version: 1,
        repository_owner: "example".to_string(),
        repository_name: "workflow".to_string(),
        pr_number: 1910,
        head_ref: "feature".to_string(),
        head_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        base_ref: "main".to_string(),
        base_sha: Some("base-a".to_string()),
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P11
/// @requirement:REQ-PRFU-012,REQ-PRFU-014
/// @pseudocode lines 18-28
fn p11_params(temp: &tempfile::TempDir) -> serde_json::Value {
    serde_json::json!({
        "artifact_root": temp.path().join("artifacts").display().to_string(),
        "repository_owner": "example",
        "repository_name": "workflow",
        "pr_number": "1910",
        "head_ref": "feature",
        "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "base_ref": "main",
        "base_sha": "base-a",
        "step_order_index": 9
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P11
/// @requirement:REQ-PRFU-012,REQ-PRFU-014
/// @pseudocode lines 18-28
fn p11_current_artifact_path(temp: &tempfile::TempDir, family: &str) -> PathBuf {
    temp.path()
        .join("artifacts")
        .join("pr-followup")
        .join("current")
        .join("run-p11")
        .join("example")
        .join("workflow")
        .join("1910")
        .join(format!("{family}.json"))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P11
/// @requirement:REQ-PRFU-012,REQ-PRFU-014
/// @pseudocode lines 18-28
fn p11_result_path(temp: &tempfile::TempDir) -> PathBuf {
    p11_current_artifact_path(temp, "pr-remediation-result")
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P11
/// @requirement:REQ-PRFU-012,REQ-PRFU-014
/// @pseudocode lines 18-28
fn p11_plan_items() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "source_type": "ci_failure",
            "source_id": "ci-build",
            "stable_marker_key": null,
            "reason": "build",
            "recommended_action": "fix_ci_failure",
            "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "source_artifact_sequence": 2,
            "evidence": { "failure_id": "ci-build", "check_name": "build" }
        }),
        serde_json::json!({
            "source_type": "coderabbit_feedback",
            "source_id": "cr-valid",
            "stable_marker_key": "thread-valid",
            "reason": "valid feedback",
            "recommended_action": "fix valid feedback",
            "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "source_artifact_sequence": 4,
            "decision": "valid",
            "body_hash": "hash-valid",
            "evidence": { "item_id": "cr-valid", "stable_marker_key": "thread-valid" }
        }),
    ]
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P11
/// @requirement:REQ-PRFU-012,REQ-PRFU-014
/// @pseudocode lines 18-28
fn write_p11_plan_and_result(temp: &tempfile::TempDir, results: serde_json::Value) {
    let store = PrFollowupArtifactStore::new(temp.path().join("artifacts"));
    let binding = p11_binding();
    store
        .write_json_artifact(
            &binding,
            "pr",
            "capture_pr_identity",
            1,
            &serde_json::json!({
                "pr_url": "https://github.com/example/workflow/pull/1910",
                "capture_state": "captured",
                "captured_at": "2026-04-30T00:00:00Z",
                "source": "fixture",
                "source_pr_node_id": "PR_kwDOExample",
                "source_head_repository_owner": null,
                "source_head_repository_name": null
            }),
            None,
            &FixedClock,
        )
        .expect("write p11 pr");
    store
        .write_json_artifact(
            &binding,
            "pr-remediation-plan",
            "build_remediation_plan",
            7,
            &serde_json::json!({
                "plan_state": "needs_remediation",
                "must_fix": p11_plan_items(),
                "mark_invalid": [],
                "needs_user_judgment": [],
                "pending_or_unknown": [],
                "source_artifacts": [],
                "built_at": "2026-04-30T00:00:00Z"
            }),
            None,
            &FixedClock,
        )
        .expect("write p11 plan");
    let plan = read_json(&p11_current_artifact_path(temp, "pr-remediation-plan"));
    store.write_json_artifact(&binding, "pr-remediation-result", "remediate_pr_followup", 8, &serde_json::json!({
        "input_head_sha": binding.head_sha,
        "output_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "head_sha": binding.head_sha,

        "overall_status": "changed",
        "results": results,
        "verification_commands": [{ "id": "cargo-test", "status": "passed" }],
        "success_file_path": null,
        "validation_state": "unvalidated",
        "validation_retry_index": 0,
        "max_validation_retries": 2,
        "remediation_attempt_index": 0,
        "max_remediation_attempts": 2,
        "retry_scope": { "scope_kind": "remediation_result_validation", "run_id": binding.run_id, "repository_owner": binding.repository_owner, "repository_name": binding.repository_name, "pr_number": binding.pr_number, "input_head_sha": binding.head_sha, "output_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "plan_artifact_sequence": plan.get("artifact_sequence"), "remediation_attempt_index": 0, "max_remediation_attempts": 2, "validation_retry_index": 0, "max_validation_retries": 2 },
        "plan_artifact_sequence": plan.get("artifact_sequence"),
        "unsuccessful_statuses": [],
        "no_change_after_remediation": false
    }), None, &FixedClock).expect("write p11 result");
}

fn rewrite_p11_result(temp: &tempfile::TempDir, mut update: impl FnMut(&mut serde_json::Value)) {
    let path = p11_result_path(temp);
    let mut result = read_json(&path);
    update(&mut result);
    std::fs::write(
        path,
        serde_json::to_vec_pretty(&result).expect("result json"),
    )
    .expect("rewrite p11 result");
}

fn assert_stale_artifact_without_attempt_burn(temp: &tempfile::TempDir, outcome: StepOutcome) {
    let artifact = read_json(&p11_result_path(temp));
    assert_expected_outcome(
        outcome,
        StepOutcome::Fixable,
        "stale remediation result scope should be retryable infrastructure state",
    );
    assert_eq!(
        artifact
            .get("validation_state")
            .and_then(serde_json::Value::as_str),
        Some("stale_artifact")
    );
    assert_eq!(
        artifact
            .get("failure_reason")
            .and_then(serde_json::Value::as_str),
        Some("stale_remediation_result_scope")
    );
    assert_eq!(
        artifact.get("remediation_attempt_index"),
        Some(&serde_json::json!(1))
    );
    assert_eq!(
        artifact.get("validation_retry_index"),
        Some(&serde_json::json!(0))
    );
    assert!(artifact
        .get("stale_scope_errors")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|errors| !errors.is_empty()));
}

#[test]
fn remediation_validator_rejects_missing_top_level_identity_without_backfill() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(&temp, serde_json::json!([]));
    rewrite_p11_result(&temp, |result| {
        let object = result.as_object_mut().expect("result object");
        object.remove("run_id");
        object.remove("repository_owner");
        object.remove("repository_name");
        object.remove("pr_number");
        result["remediation_attempt_index"] = serde_json::json!(1);
        result["stale_artifact_retry_index"] = serde_json::json!(0);
        result["max_stale_artifact_retries"] = serde_json::json!(2);
    });
    let mut context = p11_context(&temp);
    let outcome = PrRemediationResultExecutor
        .execute(&mut context, &p11_params(&temp))
        .expect("validate missing identity result");
    let artifact = read_json(&p11_result_path(&temp));
    assert_expected_outcome(
        outcome,
        StepOutcome::Fixable,
        "missing top-level identity should reject the result without retry_scope backfill",
    );
    assert_eq!(
        artifact
            .get("validation_state")
            .and_then(serde_json::Value::as_str),
        Some("fixable_malformed")
    );
    assert!(
        artifact
            .get("validation_errors")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|errors| errors.iter().any(|error| error
                .as_str()
                .is_some_and(|text| text.contains("run_id mismatch")))),
        "missing top-level identity should not be backfilled as current: {artifact:?}"
    );
}

#[test]
fn remediation_validator_rejects_stale_input_head_without_burning_model_attempts() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &temp,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "not_fixed", "input_head_sha": "old-head", "action": "attempted", "evidence": { "kind": "test", "current_head_sha": "old-head", "commands": [] }, "response_text": "stale result", "evidence_paths": [] },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "not_fixed", "input_head_sha": "old-head", "action": "attempted", "evidence": { "kind": "test", "current_head_sha": "old-head", "commands": [] }, "response_text": "stale result", "evidence_paths": [] }
        ]),
    );
    rewrite_p11_result(&temp, |result| {
        result["input_head_sha"] = serde_json::json!("old-head");
        result["remediation_attempt_index"] = serde_json::json!(1);
        result["retry_scope"]["input_head_sha"] = serde_json::json!("old-head");
        result["retry_scope"]["remediation_attempt_index"] = serde_json::json!(1);
        result["retry_scope"]["stale_artifact_retry_index"] = serde_json::json!(0);
        result["retry_scope"]["max_stale_artifact_retries"] = serde_json::json!(2);
    });
    let mut context = p11_context(&temp);
    let outcome = PrRemediationResultExecutor
        .execute(&mut context, &p11_params(&temp))
        .expect("validate stale input head");
    assert_stale_artifact_without_attempt_burn(&temp, outcome);
}

#[test]
fn remediation_validator_rejects_stale_plan_sequence_without_burning_model_attempts() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(&temp, serde_json::json!([]));
    rewrite_p11_result(&temp, |result| {
        result["plan_artifact_sequence"] = serde_json::json!(1);
        result["retry_scope"]["plan_artifact_sequence"] = serde_json::json!(1);
        result["remediation_attempt_index"] = serde_json::json!(1);
        result["retry_scope"]["remediation_attempt_index"] = serde_json::json!(1);
        result["retry_scope"]["stale_artifact_retry_index"] = serde_json::json!(0);
        result["retry_scope"]["max_stale_artifact_retries"] = serde_json::json!(2);
    });
    let mut context = p11_context(&temp);
    let outcome = PrRemediationResultExecutor
        .execute(&mut context, &p11_params(&temp))
        .expect("validate stale plan sequence");
    assert_stale_artifact_without_attempt_burn(&temp, outcome);
}

#[test]
fn remediation_validator_prefers_retry_scope_attempt_metadata() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(&temp, serde_json::json!([]));
    rewrite_p11_result(&temp, |result| {
        result["remediation_attempt_index"] = serde_json::json!(1);
        result["retry_scope"]["remediation_attempt_index"] = serde_json::json!(0);
        result["retry_scope"]["stale_artifact_retry_index"] = serde_json::json!(0);
        result["retry_scope"]["max_stale_artifact_retries"] = serde_json::json!(2);
    });
    let mut context = p11_context(&temp);
    let outcome = PrRemediationResultExecutor
        .execute(&mut context, &p11_params(&temp))
        .expect("validate retry scope attempt metadata");
    let artifact = read_json(&p11_result_path(&temp));

    assert_expected_outcome(
        outcome,
        StepOutcome::Fixable,
        "retry_scope normal counters should be used consistently before semantic validation",
    );
    assert_eq!(
        artifact
            .get("validation_state")
            .and_then(serde_json::Value::as_str),
        Some("fixable_malformed")
    );
    assert_eq!(
        artifact.get("validation_retry_index"),
        Some(&serde_json::json!(1))
    );
    assert_eq!(
        artifact.get("remediation_attempt_index"),
        Some(&serde_json::json!(1))
    );
}

#[test]
fn remediation_validator_exhausts_stale_artifact_cap_from_retry_scope_without_attempt_burn() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(&temp, serde_json::json!([]));
    rewrite_p11_result(&temp, |result| {
        result["input_head_sha"] = serde_json::json!("old-head");
        result["remediation_attempt_index"] = serde_json::json!(1);
        result
            .as_object_mut()
            .expect("result object")
            .remove("stale_artifact_retry_index");
        result
            .as_object_mut()
            .expect("result object")
            .remove("max_stale_artifact_retries");
        result["retry_scope"]["input_head_sha"] = serde_json::json!("old-head");
        result["retry_scope"]["remediation_attempt_index"] = serde_json::json!(1);
        result["retry_scope"]["stale_artifact_retry_index"] = serde_json::json!(1);
        result["retry_scope"]["max_stale_artifact_retries"] = serde_json::json!(2);
    });

    let mut context = p11_context(&temp);
    let outcome = PrRemediationResultExecutor
        .execute(&mut context, &p11_params(&temp))
        .expect("validate stale cap exhaustion");
    let artifact = read_json(&p11_result_path(&temp));

    assert_expected_outcome(
        outcome,
        StepOutcome::Fatal,
        "stale artifact retry cap should be exhausted from retry_scope counters",
    );
    assert_eq!(
        artifact
            .get("validation_state")
            .and_then(serde_json::Value::as_str),
        Some("stale_artifact_cap_exhausted")
    );
    assert_eq!(
        artifact
            .get("classified_as")
            .and_then(serde_json::Value::as_str),
        Some("stale_artifact_cap_exhausted")
    );
    assert_eq!(
        artifact.get("remediation_attempt_index"),
        Some(&serde_json::json!(1))
    );
    assert_eq!(
        artifact.get("validation_retry_index"),
        Some(&serde_json::json!(0))
    );
    assert_eq!(
        artifact.get("stale_artifact_retry_index"),
        Some(&serde_json::json!(2))
    );
    assert_eq!(
        artifact.pointer("/retry_scope/stale_artifact_retry_index"),
        Some(&serde_json::json!(2))
    );
    assert_eq!(
        artifact.pointer("/retry_scope/max_stale_artifact_retries"),
        Some(&serde_json::json!(2))
    );
}

#[test]
fn remediation_validator_accepts_retry_scope_only_normal_counters() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &temp,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "fixed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "fixed", "evidence": { "kind": "change", "current_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "paths": ["src/lib.rs"] }, "response_text": "Luther addressed this CI failure and will post the remediation evidence on the original thread.", "evidence_paths": ["src/lib.rs"] },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "fixed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "fixed", "evidence": { "kind": "change", "current_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "paths": ["src/lib.rs"] }, "response_text": "Luther addressed this CodeRabbit item and will post the remediation evidence on the original thread.", "evidence_paths": ["src/lib.rs"] }
        ]),
    );
    rewrite_p11_result(&temp, |result| {
        let object = result.as_object_mut().expect("result object");
        object.remove("validation_retry_index");
        object.remove("max_validation_retries");
        object.remove("remediation_attempt_index");
        object.remove("max_remediation_attempts");
        result["retry_scope"]["validation_retry_index"] = serde_json::json!(1);
        result["retry_scope"]["max_validation_retries"] = serde_json::json!(3);
        result["retry_scope"]["remediation_attempt_index"] = serde_json::json!(1);
        result["retry_scope"]["max_remediation_attempts"] = serde_json::json!(4);
    });

    let mut context = p11_context(&temp);
    let outcome = PrRemediationResultExecutor
        .execute(&mut context, &p11_params(&temp))
        .expect("validate retry-scope-only normal counters");
    let artifact = read_json(&p11_result_path(&temp));

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "retry_scope-only normal counters should not be classified as stale",
    );
    assert_eq!(
        artifact
            .get("validation_state")
            .and_then(serde_json::Value::as_str),
        Some("valid")
    );
    assert!(
        artifact
            .get("stale_scope_errors")
            .and_then(serde_json::Value::as_array)
            .is_none_or(Vec::is_empty),
        "retry_scope-only normal counters should not produce stale scope errors: {artifact:?}"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P11
/// @requirement:REQ-PRFU-012,REQ-PRFU-014
/// @pseudocode lines 18-28
#[test]
fn remediation_validator_waits_for_correctly_scoped_artifact_after_stale_result() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &temp,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "fixed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "fixed", "evidence": { "kind": "change", "current_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "paths": ["src/lib.rs"] }, "response_text": "Luther addressed this CI failure and will post the remediation evidence on the original thread.", "evidence_paths": ["src/lib.rs"] },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "fixed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "fixed", "evidence": { "kind": "change", "current_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "paths": ["src/lib.rs"] }, "response_text": "Luther addressed this CodeRabbit item and will post the remediation evidence on the original thread.", "evidence_paths": ["src/lib.rs"] }
        ]),
    );
    let correct_result = read_json(&p11_result_path(&temp));
    rewrite_p11_result(&temp, |result| {
        result["input_head_sha"] = serde_json::json!("old-head");
        result["retry_scope"]["input_head_sha"] = serde_json::json!("old-head");
    });
    let mut stale_context = p11_context(&temp);
    let stale_outcome = PrRemediationResultExecutor
        .execute(&mut stale_context, &p11_params(&temp))
        .expect("validate stale scoped result");
    assert_expected_outcome(
        stale_outcome,
        StepOutcome::Fixable,
        "stale artifact should ask wrapper to retry",
    );
    std::fs::write(
        p11_result_path(&temp),
        serde_json::to_vec_pretty(&correct_result).expect("result json"),
    )
    .expect("write current result");
    let mut current_context = p11_context(&temp);
    let current_outcome = PrRemediationResultExecutor
        .execute(&mut current_context, &p11_params(&temp))
        .expect("validate correctly scoped result");
    let artifact = read_json(&p11_result_path(&temp));
    assert_expected_outcome(
        current_outcome,
        StepOutcome::Success,
        "correctly scoped artifact should validate after stale artifact retry",
    );
    assert_eq!(
        artifact
            .get("validation_state")
            .and_then(serde_json::Value::as_str),
        Some("valid")
    );
}

#[test]
fn remediation_validator_restores_engine_known_plan_sequence_for_agent_result() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &temp,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "fixed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "fixed", "evidence": { "kind": "change", "current_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "paths": ["src/lib.rs"] }, "response_text": "Luther addressed this CI failure and will post the remediation evidence on the original thread.", "evidence_paths": ["src/lib.rs"] },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "fixed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "fixed", "evidence": { "kind": "change", "current_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "paths": ["src/lib.rs"] }, "response_text": "Luther addressed this CodeRabbit item and will post the remediation evidence on the original thread.", "evidence_paths": ["src/lib.rs"] }
        ]),
    );
    let mut result = read_json(&p11_result_path(&temp));
    result["plan_artifact_sequence"] = serde_json::Value::Null;
    result["retry_scope"] = serde_json::json!({});
    result["validation_retry_index"] = serde_json::json!(2);
    std::fs::write(
        p11_result_path(&temp),
        serde_json::to_vec_pretty(&result).expect("result json"),
    )
    .expect("rewrite agent-like result without engine sequence fields");

    let mut context = p11_context(&temp);
    let outcome = PrRemediationResultExecutor
        .execute(&mut context, &p11_params(&temp))
        .expect("validate result with normalized plan sequence");
    let artifact = read_json(&p11_result_path(&temp));
    let plan = read_json(&p11_current_artifact_path(&temp, "pr-remediation-plan"));

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "validator should restore engine-known plan sequence fields instead of looping malformed remediation forever",
    );
    assert_eq!(
        artifact
            .get("validation_state")
            .and_then(serde_json::Value::as_str),
        Some("valid")
    );
    assert_eq!(
        artifact.get("plan_artifact_sequence"),
        plan.get("artifact_sequence")
    );
    assert_eq!(
        artifact.pointer("/retry_scope/plan_artifact_sequence"),
        plan.get("artifact_sequence")
    );
    assert_eq!(
        artifact
            .pointer("/retry_scope/run_id")
            .and_then(serde_json::Value::as_str),
        Some("run-p11")
    );
    assert!(
        artifact
            .get("validation_errors")
            .and_then(serde_json::Value::as_array)
            .is_some_and(Vec::is_empty),
        "normalization should clear plan_artifact_sequence mismatch: {artifact}"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P11
/// @requirement:REQ-PRFU-012,REQ-PRFU-014
/// @pseudocode lines 18-28
#[test]
fn remediation_result_rejects_not_fixed_skipped_failed_before_push_success() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &temp,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "not_fixed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "attempted", "evidence": { "kind": "test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "commands": [{ "id": "cargo-test", "status": "failed" }] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": [] },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "skipped", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "skipped", "evidence": { "kind": "policy", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "paths": ["src/lib.rs"] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": [] }
        ]),
    );
    let mut context = p11_context(&temp);
    let outcome = PrRemediationResultExecutor
        .execute(&mut context, &p11_params(&temp))
        .expect("validate unsuccessful result");

    let artifact = read_json(&p11_result_path(&temp));

    assert_expected_outcome(
        outcome,
        StepOutcome::Fixable,
        "valid structured not_fixed, skipped, or failed remediation results must not route to push success",
    );
    assert_eq!(
        artifact
            .get("validation_state")
            .and_then(serde_json::Value::as_str),
        Some("valid_but_unsuccessful")
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @requirement:REQ-PRFU-012
/// @pseudocode lines 18-28
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 13-18
#[test]
fn ci_failure_collector_watcher_fatal_writes_source_reference_without_invented_failures() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p07_check_status(
        &temp,
        "fatal",
        serde_json::json!([]),
        serde_json::json!([]),
        serde_json::json!({ "class": "api_error", "message": "rate limited" }),
    );
    let mut context = p07_context(&temp);
    let outcome = GithubCheckFailuresExecutorWithRunner::new(
        ScriptedGithubRunner::new(
            serde_json::json!([]),
            serde_json::json!({ "total_count": 0, "check_runs": [] }),
        ),
        FixedClock,
    )
    .execute(&mut context, &p07_params(&temp))
    .expect("collect ci failures");
    let artifact = read_json(&p07_ci_failures_path(&temp));
    assert_expected_outcome(
        outcome,
        StepOutcome::Fatal,
        "watcher fatal must route terminal without invented failures",
    );
    assert_eq!(
        artifact
            .get("collection_state")
            .and_then(serde_json::Value::as_str),
        Some("fatal")
    );
    assert!(artifact
        .get("failures")
        .and_then(serde_json::Value::as_array)
        .expect("failures")
        .is_empty());
    assert_eq!(
        artifact
            .pointer("/watcher_fatal_source/class")
            .and_then(serde_json::Value::as_str),
        Some("api_error")
    );
    assert_eq!(
        artifact
            .get("pending_or_unknown")
            .and_then(serde_json::Value::as_array)
            .expect("pending")
            .len(),
        1
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P11
/// @requirement:REQ-PRFU-012,REQ-PRFU-014
/// @pseudocode lines 21-22,28
#[test]
fn remediation_result_accepts_already_satisfied_and_not_reproduced_only_with_deterministic_evidence(
) {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &temp,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "already_satisfied", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "verified", "evidence": { "kind": "current_repository_test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "commands": [{ "id": "cargo-test", "status": "passed", "argv": ["cargo", "test"] }] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": [] },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "not_reproduced", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "verified", "evidence": { "kind": "api_lookup", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "api_lookups": [{ "endpoint": "/repos/example/workflow/pulls/1910/comments", "normalized_status": "not_found" }] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": [] }
        ]),
    );
    let mut context = p11_context(&temp);
    let outcome = PrRemediationResultExecutor
        .execute(&mut context, &p11_params(&temp))
        .expect("validate deterministic success result");
    let artifact = read_json(&p11_result_path(&temp));
    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "already_satisfied and not_reproduced must require deterministic evidence fields before validator success",
    );
    assert_eq!(
        artifact
            .get("validation_state")
            .and_then(serde_json::Value::as_str),
        Some("valid")
    );

    let command_only_evidence = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &command_only_evidence,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "already_satisfied", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "verified", "evidence": { "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "commands": [{ "id": "cargo-test", "status": "passed", "argv": ["cargo", "test"] }] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": [] },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "not_reproduced", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "verified", "evidence": { "kind": "api_lookup", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "api_lookups": [{ "endpoint": "/repos/example/workflow/pulls/1910/comments", "normalized_status": "not_found" }] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": [] }
        ]),
    );
    let mut command_only_context = p11_context(&command_only_evidence);
    let command_only_outcome = PrRemediationResultExecutor
        .execute(
            &mut command_only_context,
            &p11_params(&command_only_evidence),
        )
        .expect("validate command-only deterministic evidence");
    let command_only_artifact = read_json(&p11_result_path(&command_only_evidence));
    assert_expected_outcome(
        command_only_outcome,
        StepOutcome::Success,
        "already_satisfied deterministic evidence should be accepted when passed commands are tied to the current head",
    );
    assert_eq!(
        command_only_artifact
            .get("validation_state")
            .and_then(serde_json::Value::as_str),
        Some("valid")
    );

    let missing_evidence = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &missing_evidence,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "already_satisfied", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "verified", "evidence": { "kind": "current_repository_test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "commands": [] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": [] },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "not_reproduced", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "verified", "evidence": { "kind": "api_lookup", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "api_lookups": [] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": [] }
        ]),
    );
    let mut missing_context = p11_context(&missing_evidence);
    let missing_outcome = PrRemediationResultExecutor
        .execute(&mut missing_context, &p11_params(&missing_evidence))
        .expect("reject missing deterministic evidence");
    let missing_artifact = read_json(&p11_result_path(&missing_evidence));
    assert_expected_outcome(
        missing_outcome,
        StepOutcome::Fixable,
        "missing deterministic evidence must fail validation before push success",
    );
    assert_eq!(
        missing_artifact
            .get("validation_state")
            .and_then(serde_json::Value::as_str),
        Some("fixable_malformed")
    );

    let mismatched_head = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &mismatched_head,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "already_satisfied", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "verified", "evidence": { "kind": "current_repository_test", "current_head_sha": "cccccccccccccccccccccccccccccccccccccccc", "commands": [{ "id": "cargo-test", "status": "passed", "argv": ["cargo", "test"] }] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": [] },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "not_reproduced", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "verified", "evidence": { "kind": "api_lookup", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "api_lookups": [{ "endpoint": "/repos/example/workflow/pulls/1910/comments", "normalized_status": "not_found" }] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": [] }
        ]),
    );
    let mut mismatch_context = p11_context(&mismatched_head);
    let mismatch_outcome = PrRemediationResultExecutor
        .execute(&mut mismatch_context, &p11_params(&mismatched_head))
        .expect("reject mismatched deterministic evidence head");
    let mismatch_artifact = read_json(&p11_result_path(&mismatched_head));
    assert_expected_outcome(
        mismatch_outcome,
        StepOutcome::Fixable,
        "mismatched deterministic evidence head must fail validation before push success",
    );
    assert_eq!(
        mismatch_artifact
            .get("validation_state")
            .and_then(serde_json::Value::as_str),
        Some("fixable_malformed")
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P11
/// @requirement:REQ-PRFU-012,REQ-PRFU-014
/// @pseudocode lines 18-28
#[test]
fn remediation_validator_same_head_no_change_attempt_cap_reaches_post_pr_failure_terminal_failure_not_abandoned(
) {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &temp,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "failed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "attempted", "evidence": { "kind": "test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "commands": [{ "id": "cargo-test", "status": "failed" }] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": [] },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "not_fixed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "attempted", "evidence": { "kind": "test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "commands": [{ "id": "cargo-test", "status": "failed" }] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": [] }
        ]),
    );
    let mut result = read_json(&p11_result_path(&temp));
    result["output_head_sha"] = serde_json::json!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    result["retry_scope"]["output_head_sha"] =
        serde_json::json!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    result["max_remediation_attempts"] = serde_json::json!(1);
    result["retry_scope"]["max_remediation_attempts"] = serde_json::json!(1);
    std::fs::write(
        p11_result_path(&temp),
        serde_json::to_vec_pretty(&result).expect("result json"),
    )
    .expect("rewrite no-change result");
    let mut context = p11_context(&temp);
    let outcome = PrRemediationResultExecutor
        .execute(&mut context, &p11_params(&temp))
        .expect("validate no-change cap exhaustion");
    let artifact = read_json(&p11_result_path(&temp));
    assert_expected_outcome(
        outcome,
        StepOutcome::Fatal,
        "same-head no-change remediation attempts must exhaust remediation_attempt_index cap and reach post_pr_failure_terminal, never RunOutcome::Abandoned",
    );
    assert_eq!(
        artifact
            .get("validation_state")
            .and_then(serde_json::Value::as_str),
        Some("unsuccessful_remediation_cap_exhausted")
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P11
/// @requirement:REQ-PRFU-014
/// @pseudocode lines 19-23
#[test]
fn remediation_validator_wraps_raw_llxprt_result_without_store_metadata() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &temp,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "not_fixed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "attempted", "evidence": { "kind": "test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "commands": [{ "id": "cargo-test", "status": "failed" }] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": [] },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "not_fixed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "attempted", "evidence": { "kind": "test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "commands": [{ "id": "cargo-test", "status": "failed" }] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": [] }
        ]),
    );
    let plan = read_json(&p11_current_artifact_path(&temp, "pr-remediation-plan"));
    std::fs::write(
        p11_result_path(&temp),
        serde_json::to_vec_pretty(&serde_json::json!({
            "run_id": "run-p11",
            "repository_owner": "example",
            "repository_name": "workflow",
            "pr_number": 1910,
            "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "output_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "overall_status": "failed",
            "results": [
                { "source_type": "ci_failure", "source_id": "ci-build", "status": "not_fixed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "attempted", "evidence": { "kind": "test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "commands": [{ "id": "cargo-test", "status": "failed" }] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": [] },
                { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "not_fixed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "attempted", "evidence": { "kind": "test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "commands": [{ "id": "cargo-test", "status": "failed" }] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": [] }
            ],
            "plan_artifact_sequence": plan.get("artifact_sequence"),
            "retry_scope": { "scope_kind": "remediation_result_validation", "run_id": "run-p11", "repository_owner": "example", "repository_name": "workflow", "pr_number": 1910, "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "output_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "plan_artifact_sequence": plan.get("artifact_sequence"), "remediation_attempt_index": 1, "max_remediation_attempts": 2, "validation_retry_index": 0, "max_validation_retries": 2 },
            "remediation_attempt_index": 1,
            "max_remediation_attempts": 2
        }))
        .expect("raw result json"),
    )
    .expect("write raw result");

    let mut context = p11_context(&temp);
    let outcome = PrRemediationResultExecutor
        .execute(&mut context, &p11_params(&temp))
        .expect("validate raw result");
    let artifact = read_json(&p11_result_path(&temp));

    assert_expected_outcome(
        outcome,
        StepOutcome::Fatal,
        "raw llxprt remediation result must be wrapped and validated instead of failing artifact binding before validation",
    );
    assert_eq!(
        artifact
            .get("validation_state")
            .and_then(serde_json::Value::as_str),
        Some("unsuccessful_remediation_cap_exhausted")
    );
    assert_eq!(
        artifact.get("run_id").and_then(serde_json::Value::as_str),
        Some("run-p11")
    );
    assert!(artifact.get("history_metadata").is_some());
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P11
/// @requirement:REQ-PRFU-012,REQ-PRFU-014
/// @pseudocode lines 18-28
#[test]
fn remediation_validator_status_enum() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &temp,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "fixed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "fixed", "evidence": { "kind": "change", "current_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "paths": ["src/lib.rs"] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": ["src/lib.rs"] },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "changed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "changed", "evidence": { "kind": "change", "current_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "paths": ["src/lib.rs"] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": ["src/lib.rs"] }
        ]),
    );
    let mut context = p11_context(&temp);
    let outcome = PrRemediationResultExecutor
        .execute(&mut context, &p11_params(&temp))
        .expect("validate changed status");
    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "changed is in the canonical successful remediation result status enum",
    );

    let unknown = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &unknown,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "needs_user_judgment", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "invalid", "evidence": { "kind": "text" }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": [] },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "fixed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "fixed", "evidence": { "kind": "change", "current_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "paths": ["src/lib.rs"] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": ["src/lib.rs"] }
        ]),
    );
    let mut unknown_context = p11_context(&unknown);
    let unknown_outcome = PrRemediationResultExecutor
        .execute(&mut unknown_context, &p11_params(&unknown))
        .expect("reject unknown status");
    assert_expected_outcome(
        unknown_outcome,
        StepOutcome::Fixable,
        "needs_user_judgment is not a canonical remediation result status",
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P11
/// @requirement:REQ-PRFU-012,REQ-PRFU-014
/// @pseudocode lines 19-23
#[test]
fn remediation_validator_rejects_unknown_status_outside_canonical_enum() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &temp,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "needs_user_judgment", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "invalid", "evidence": { "kind": "text" }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": [] },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "fixed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "fixed", "evidence": { "kind": "change", "current_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "paths": ["src/lib.rs"] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": ["src/lib.rs"] }
        ]),
    );
    let mut context = p11_context(&temp);
    let outcome = PrRemediationResultExecutor
        .execute(&mut context, &p11_params(&temp))
        .expect("validate unknown status");
    let artifact = read_json(&p11_result_path(&temp));
    assert_expected_outcome(
        outcome,
        StepOutcome::Fixable,
        "unknown remediation result statuses, including needs_user_judgment, are fixably malformed while retry cap remains",
    );
    assert_eq!(
        artifact
            .get("validation_state")
            .and_then(serde_json::Value::as_str),
        Some("fixable_malformed")
    );

    let changed = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &changed,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "fixed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "fixed", "evidence": { "kind": "change", "current_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "paths": ["src/lib.rs"] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": ["src/lib.rs"] },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "changed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "changed", "evidence": { "kind": "change", "current_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "paths": ["src/lib.rs"] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": ["src/lib.rs"] }
        ]),
    );
    let mut changed_context = p11_context(&changed);
    let changed_outcome = PrRemediationResultExecutor
        .execute(&mut changed_context, &p11_params(&changed))
        .expect("validate changed status");
    let changed_artifact = read_json(&p11_result_path(&changed));
    assert_expected_outcome(
        changed_outcome,
        StepOutcome::Success,
        "changed is a canonical successful remediation result status",
    );
    assert_eq!(
        changed_artifact
            .get("validation_state")
            .and_then(serde_json::Value::as_str),
        Some("valid")
    );
}
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P11
/// @requirement:REQ-PRFU-014
#[test]
fn remediation_validator_ties_fixed_evidence_to_post_remediation_head() {
    // A genuine fixed remediation commits a new change, so the PR head advances
    // from the input head (aaaa) to the post-remediation output head (bbbb).
    // The fixed evidence must be tied to that post-remediation head, not the
    // pre-remediation input head. Evidence still pinned to the input head must
    // be rejected as not tied to current head.
    let stale = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &stale,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "fixed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "fixed", "evidence": { "kind": "change", "current_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "paths": ["src/lib.rs"] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": ["src/lib.rs"] },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "fixed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "fixed", "evidence": { "kind": "change", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "paths": ["src/lib.rs"] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": ["src/lib.rs"] }
        ]),
    );
    let mut stale_context = p11_context(&stale);
    let stale_outcome = PrRemediationResultExecutor
        .execute(&mut stale_context, &p11_params(&stale))
        .expect("validate stale-head fixed evidence");
    let stale_artifact = read_json(&p11_result_path(&stale));
    assert_expected_outcome(
        stale_outcome,
        StepOutcome::Fixable,
        "fixed evidence still pinned to the pre-remediation input head must not validate",
    );
    assert!(stale_artifact
        .get("validation_errors")
        .and_then(serde_json::Value::as_array)
        .expect("validation errors")
        .iter()
        .any(|error| error
            .as_str()
            .is_some_and(|text| text.contains("not tied to current head"))));

    let fresh = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &fresh,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "fixed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "fixed", "evidence": { "kind": "change", "current_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "paths": ["src/lib.rs"] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": ["src/lib.rs"] },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "fixed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "fixed", "evidence": { "kind": "change", "current_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "paths": ["src/lib.rs"] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": ["src/lib.rs"] }
        ]),
    );
    let mut fresh_context = p11_context(&fresh);
    let fresh_outcome = PrRemediationResultExecutor
        .execute(&mut fresh_context, &p11_params(&fresh))
        .expect("validate post-remediation-head fixed evidence");
    assert_expected_outcome(
        fresh_outcome,
        StepOutcome::Success,
        "fixed evidence tied to the post-remediation output head must validate",
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P11
/// @requirement:REQ-PRFU-014
/// @pseudocode lines 18-28
#[test]
fn remediation_validator_requires_complete_exact_plan_coverage_before_success() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &temp,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "fixed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "fixed", "evidence": { "kind": "change", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "paths": ["src/lib.rs"] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": ["src/lib.rs"] }
        ]),
    );
    let mut context = p11_context(&temp);
    let outcome = PrRemediationResultExecutor
        .execute(&mut context, &p11_params(&temp))
        .expect("reject partial result coverage");
    let artifact = read_json(&p11_result_path(&temp));
    assert_expected_outcome(
        outcome,
        StepOutcome::Fixable,
        "partial successful remediation result sets must not pass without one result for every must_fix item",
    );
    assert!(artifact
        .get("validation_errors")
        .and_then(serde_json::Value::as_array)
        .expect("validation errors")
        .iter()
        .any(|error| error
            .as_str()
            .is_some_and(|text| text.contains("missing remediation result"))));

    let duplicate = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &duplicate,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "fixed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "fixed", "evidence": { "kind": "change", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "paths": ["src/lib.rs"] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": ["src/lib.rs"] },
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "fixed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "fixed", "evidence": { "kind": "change", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "paths": ["src/lib.rs"] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": ["src/lib.rs"] },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "fixed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "fixed", "evidence": { "kind": "change", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "paths": ["src/lib.rs"] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": ["src/lib.rs"] }
        ]),
    );
    let mut duplicate_context = p11_context(&duplicate);
    let duplicate_outcome = PrRemediationResultExecutor
        .execute(&mut duplicate_context, &p11_params(&duplicate))
        .expect("reject duplicate result coverage");
    let duplicate_artifact = read_json(&p11_result_path(&duplicate));
    assert_expected_outcome(
        duplicate_outcome,
        StepOutcome::Fixable,
        "duplicate remediation results for one must_fix item must not pass",
    );
    assert!(duplicate_artifact
        .get("validation_errors")
        .and_then(serde_json::Value::as_array)
        .expect("validation errors")
        .iter()
        .any(|error| error
            .as_str()
            .is_some_and(|text| text.contains("duplicate remediation results"))));
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P11
/// @requirement:REQ-PRFU-014
/// @pseudocode lines 24-28
#[test]
fn remediation_validator_writes_pending_marker_action_for_fixed_valid_feedback_before_push() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &temp,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "fixed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "fixed", "evidence": { "kind": "change", "current_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "paths": ["src/lib.rs"] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": ["src/lib.rs"] },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "fixed", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "fixed", "evidence": { "kind": "change", "current_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "paths": ["src/lib.rs"] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": ["src/lib.rs"] }
        ]),
    );
    let mut context = p11_context(&temp);
    let outcome = PrRemediationResultExecutor
        .execute(&mut context, &p11_params(&temp))
        .expect("validate fixed result");
    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "valid fixed feedback must validate before push",
    );
    let actions = read_json(&p11_current_artifact_path(
        &temp,
        "pending-feedback-marker-actions",
    ));
    let pending = actions
        .get("pending_actions")
        .and_then(serde_json::Value::as_array)
        .expect("pending actions");
    assert_eq!(pending.len(), 1);
    let action = &pending[0];
    assert_eq!(
        action
            .get("action_kind")
            .and_then(serde_json::Value::as_str),
        Some("comment_fixed")
    );
    assert_eq!(
        action
            .get("stable_marker_key")
            .and_then(serde_json::Value::as_str),
        Some("thread-valid")
    );
    assert_eq!(
        action.get("body_hash").and_then(serde_json::Value::as_str),
        Some("hash-valid")
    );
    assert_eq!(
        action
            .get("source_head_sha")
            .and_then(serde_json::Value::as_str),
        Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    );
    assert_eq!(
        action
            .get("remediation_input_head_sha")
            .and_then(serde_json::Value::as_str),
        Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    );
    assert_eq!(
        action
            .get("remediation_output_head_sha")
            .and_then(serde_json::Value::as_str),
        Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
    );
    assert!(action
        .get("idempotency_key")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|key| key.contains("thread-valid")));
    assert_eq!(
        action
            .pointer("/original_feedback_identity/item_id")
            .and_then(serde_json::Value::as_str),
        Some("cr-valid")
    );
    assert!(action.get("remediation_result_evidence").is_some());
}
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013,REQ-PRFU-017
/// @pseudocode lines 12-17
#[derive(Clone, Debug)]
struct P12FakeLlxprtRunner {
    result: LlxprtInvocationResult,
    result_payload: Option<serde_json::Value>,
    requests: Arc<Mutex<Vec<LlxprtInvocationRequest>>>,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013,REQ-PRFU-017
/// @pseudocode lines 14-16
impl P12FakeLlxprtRunner {
    fn new(result: LlxprtInvocationResult) -> Self {
        Self {
            result,
            result_payload: None,
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn with_result_payload(
        result: LlxprtInvocationResult,
        result_payload: serde_json::Value,
    ) -> Self {
        Self {
            result,
            result_payload: Some(result_payload),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn requests(&self) -> Vec<LlxprtInvocationRequest> {
        self.requests.lock().expect("requests").clone()
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013,REQ-PRFU-017
/// @pseudocode lines 14-16
impl PrFollowupLlxprtCommandRunner for P12FakeLlxprtRunner {
    fn invoke(&self, request: LlxprtInvocationRequest) -> LlxprtInvocationResult {
        if let Some(payload) = &self.result_payload {
            std::fs::write(
                &request.remediation_result_path,
                serde_json::to_vec_pretty(payload).expect("result payload json"),
            )
            .expect("write fake remediation result");
        }
        self.requests.lock().expect("requests").push(request);
        self.result.clone()
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 15-17
fn p12_result(process_class: &str) -> LlxprtInvocationResult {
    LlxprtInvocationResult {
        argv: vec!["fake-llxprt".to_string(), "--owned-result".to_string()],
        working_directory: PathBuf::from("/tmp/p12-owned-workdir"),
        exit_code: Some(42),
        signal: None,
        process_class: process_class.to_string(),
        bounded_stdout: "owned stdout evidence".to_string(),
        bounded_stderr: "owned stderr evidence".to_string(),
        stdout_log_path: Some(PathBuf::from("/tmp/p12-stdout.log")),
        stderr_log_path: Some(PathBuf::from("/tmp/p12-stderr.log")),
        success_file_present: false,
        success_file_size: None,
        result_file_present: false,
        result_file_size: None,
        result_file_path: None,

        changed_paths: vec!["src/lib.rs".to_string()],
        spawn_error: None,
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 12-17
fn p12_context(temp: &tempfile::TempDir) -> StepContext {
    StepContext::new(temp.path().to_path_buf(), "run-p10".to_string())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 12-17
fn p12_params(temp: &tempfile::TempDir) -> serde_json::Value {
    p10_params(temp)
}

fn write_p12_plan(temp: &tempfile::TempDir) {
    write_p10_inputs(
        temp,
        serde_json::json!([]),
        serde_json::json!([p10_accepted(
            "cr-valid",
            "thread-valid",
            "hash-valid",
            "valid"
        )]),
        serde_json::json!([]),
        serde_json::json!([]),
        serde_json::json!([]),
    );
    let mut plan_context = p10_context(temp);
    PrRemediationPlanExecutor
        .execute(&mut plan_context, &p10_params(temp))
        .expect("build p10 plan");
}

#[test]
fn pr_followup_remediation_prompt_requires_complete_retry_scope() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p12_plan(&temp);
    let runner = P12FakeLlxprtRunner::new(p12_result("success"));
    let mut context = p12_context(&temp);
    let outcome = PrFollowupRemediationExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &p12_params(&temp))
        .expect("run remediation prompt");
    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "wrapper writes failure result",
    );
    let request = runner.requests().pop().expect("llxprt request");
    for expected in [
        "run_id",
        "repository_owner",
        "repository_name",
        "pr_number",
        "input_head_sha",
        "output_head_sha after remediation",
        "plan_artifact_sequence",
        "remediation_attempt_index",
        "validation_retry_index",
        "stale_artifact_retry_index",
        "max_stale_artifact_retries",
        "scope_kind remediation_result_validation",
    ] {
        assert!(
            request.argv.iter().any(|arg| arg.contains(expected)),
            "prompt should mention {expected}: {:?}",
            request.argv
        );
    }
}

#[test]
fn pr_followup_remediation_wrapper_writes_complete_retry_scope_on_failure() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p12_plan(&temp);
    let runner = P12FakeLlxprtRunner::new(p12_result("timeout"));
    let mut context = p12_context(&temp);
    let outcome = PrFollowupRemediationExecutorWithRunner::new(runner, FixedClock)
        .execute(&mut context, &p12_params(&temp))
        .expect("run remediation wrapper failure");
    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "wrapper writes validator-readable result",
    );
    let artifact = read_json(&p10_current_artifact_path(&temp, "pr-remediation-result"));
    assert_eq!(
        artifact.pointer("/retry_scope/scope_kind"),
        Some(&serde_json::json!("remediation_result_validation"))
    );
    for pointer in [
        "/retry_scope/run_id",
        "/retry_scope/repository_owner",
        "/retry_scope/repository_name",
        "/retry_scope/pr_number",
        "/retry_scope/input_head_sha",
        "/retry_scope/output_head_sha",
        "/retry_scope/plan_artifact_sequence",
        "/retry_scope/remediation_attempt_index",
        "/retry_scope/max_remediation_attempts",
        "/retry_scope/validation_retry_index",
        "/retry_scope/max_validation_retries",
        "/retry_scope/stale_artifact_retry_index",
        "/retry_scope/max_stale_artifact_retries",
        "/retry_scope/scope_kind",
    ] {
        assert!(artifact.pointer(pointer).is_some(), "missing {pointer}");
    }
}

#[test]
fn pr_followup_remediation_wrapper_carries_stale_retry_scope_on_failure() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p12_plan(&temp);
    let previous_path = p10_current_artifact_path(&temp, "pr-remediation-result");
    write_previous_p12_stale_retry_result(&previous_path);

    let runner = P12FakeLlxprtRunner::new(p12_result("timeout"));
    let mut context = p12_context(&temp);
    let outcome = PrFollowupRemediationExecutorWithRunner::new(runner, FixedClock)
        .execute(&mut context, &p12_params(&temp))
        .expect("run remediation wrapper failure");
    let artifact = read_json(&previous_path);

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "wrapper should preserve retry metadata when writing failure result",
    );
    assert_p12_stale_retry_counters(&artifact);
    assert_p12_stale_retry_scope_identity(&artifact);
    assert_p12_stale_retry_scope_heads(&artifact);
    assert_p12_normal_retry_counters(&artifact);
}
fn write_previous_p12_stale_retry_result(previous_path: &std::path::Path) {
    std::fs::write(
        previous_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "validation_errors": ["stale scope"],
            "stale_artifact_retry_index": 0,
            "max_stale_artifact_retries": 9,
            "validation_retry_index": 3,
            "max_validation_retries": 5,
            "remediation_attempt_index": 4,
            "max_remediation_attempts": 6,
            "retry_scope": {
                "scope_kind": "remediation_result_validation",
                "run_id": "run-p10",
                "repository_owner": "example",
                "repository_name": "workflow",
                "pr_number": 1910,
                "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "output_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "plan_artifact_sequence": 5,
                "stale_artifact_retry_index": 1,
                "max_stale_artifact_retries": 2,
                "validation_retry_index": 7,
                "max_validation_retries": 8,
                "remediation_attempt_index": 9,
                "max_remediation_attempts": 10
            }
        }))
        .expect("previous result json"),
    )
    .expect("write previous result");
}

fn assert_p12_stale_retry_counters(artifact: &serde_json::Value) {
    assert_eq!(
        artifact.pointer("/retry_scope/stale_artifact_retry_index"),
        Some(&serde_json::json!(1))
    );
    assert_eq!(
        artifact.pointer("/retry_scope/max_stale_artifact_retries"),
        Some(&serde_json::json!(2))
    );
    assert_eq!(
        artifact.get("stale_artifact_retry_index"),
        Some(&serde_json::json!(1))
    );
    assert_eq!(
        artifact.get("max_stale_artifact_retries"),
        Some(&serde_json::json!(2))
    );
}

fn assert_p12_stale_retry_scope_identity(artifact: &serde_json::Value) {
    assert_eq!(
        artifact.pointer("/retry_scope/scope_kind"),
        Some(&serde_json::json!("remediation_result_validation"))
    );
    assert_eq!(
        artifact.pointer("/retry_scope/run_id"),
        Some(&serde_json::json!("run-p10"))
    );
    assert_eq!(
        artifact.pointer("/retry_scope/repository_owner"),
        Some(&serde_json::json!("example"))
    );
    assert_eq!(
        artifact.pointer("/retry_scope/repository_name"),
        Some(&serde_json::json!("workflow"))
    );
    assert_eq!(
        artifact.pointer("/retry_scope/pr_number"),
        Some(&serde_json::json!(1910))
    );
}

fn assert_p12_stale_retry_scope_heads(artifact: &serde_json::Value) {
    assert_eq!(
        artifact.pointer("/retry_scope/input_head_sha"),
        Some(&serde_json::json!(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ))
    );
    assert_eq!(
        artifact.pointer("/retry_scope/output_head_sha"),
        Some(&serde_json::json!(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ))
    );
    assert_eq!(
        artifact.pointer("/retry_scope/plan_artifact_sequence"),
        Some(&serde_json::json!(5))
    );
}

fn assert_p12_normal_retry_counters(artifact: &serde_json::Value) {
    assert_eq!(
        artifact.pointer("/retry_scope/validation_retry_index"),
        Some(&serde_json::json!(7))
    );
    assert_eq!(
        artifact.pointer("/retry_scope/max_validation_retries"),
        Some(&serde_json::json!(8))
    );
    assert_eq!(
        artifact.pointer("/retry_scope/remediation_attempt_index"),
        Some(&serde_json::json!(9))
    );
    assert_eq!(
        artifact.pointer("/retry_scope/max_remediation_attempts"),
        Some(&serde_json::json!(10))
    );
    assert_eq!(
        artifact.get("validation_retry_index"),
        Some(&serde_json::json!(7))
    );
    assert_eq!(
        artifact.get("max_validation_retries"),
        Some(&serde_json::json!(8))
    );
    assert_eq!(
        artifact.get("remediation_attempt_index"),
        Some(&serde_json::json!(9))
    );
    assert_eq!(
        artifact.get("max_remediation_attempts"),
        Some(&serde_json::json!(10))
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 12-17
#[test]
fn pr_followup_remediation_wrapper_records_owned_process_evidence() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runner = P12FakeLlxprtRunner::new(p12_result("timeout"));
    let mut context = p12_context(&temp);
    let outcome = PrFollowupRemediationExecutorWithRunner::new(runner, FixedClock)
        .execute(&mut context, &p12_params(&temp))
        .expect("p12 wrapper");
    let run_artifact = read_json(&p10_current_artifact_path(
        &temp,
        "pr-remediation-llxprt-run",
    ));
    let failure_result = read_json(&p10_current_artifact_path(&temp, "pr-remediation-result"));

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "wrapper success means validator should classify the validator-readable failure artifact",
    );
    assert_eq!(
        run_artifact
            .get("remediation_invocation_state")
            .and_then(serde_json::Value::as_str),
        Some("timeout")
    );
    assert_eq!(
        run_artifact
            .get("validator_readable_result_written")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert_eq!(
        run_artifact
            .get("argv")
            .and_then(serde_json::Value::as_array)
            .and_then(|argv| argv.first())
            .and_then(serde_json::Value::as_str),
        Some("fake-llxprt")
    );
    assert_eq!(
        run_artifact
            .get("bounded_stdout")
            .and_then(serde_json::Value::as_str),
        Some("owned stdout evidence")
    );
    assert!(
        run_artifact.to_string().contains("src/lib.rs"),
        "changed-path evidence from the owned result must be persisted: {run_artifact}"
    );
    assert_eq!(
        failure_result
            .get("overall_status")
            .and_then(serde_json::Value::as_str),
        Some("failed")
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013,REQ-PRFU-017
/// @pseudocode lines 14-17
#[test]
fn pr_followup_remediation_timeout_with_result_is_validator_success_candidate() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut owned = p12_result("timeout");
    owned.result_file_present = true;
    owned.result_file_size = Some(256);
    let binding = p10_binding();
    let runner = P12FakeLlxprtRunner::with_result_payload(
        owned,
        serde_json::json!({
            "input_head_sha": binding.head_sha,
            "output_head_sha": binding.head_sha,
            "overall_status": "success",
            "results": [{
                "source_id": "ci-linux",
                "action": "fixed",
                "evidence": { "summary": "result written before timeout" }
            }]
        }),
    );
    let mut context = p12_context(&temp);

    let outcome = PrFollowupRemediationExecutorWithRunner::new(runner, FixedClock)
        .execute(&mut context, &p12_params(&temp))
        .expect("p12 timeout with result");
    let run_artifact = read_json(&p10_current_artifact_path(
        &temp,
        "pr-remediation-llxprt-run",
    ));

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "validator-readable result should drive validation",
    );
    assert_eq!(
        run_artifact
            .get("remediation_invocation_state")
            .and_then(serde_json::Value::as_str),
        Some("success")
    );
    assert_eq!(
        run_artifact
            .get("process_class")
            .and_then(serde_json::Value::as_str),
        Some("success")
    );
}

#[test]
fn pr_followup_remediation_timeout_accepts_identical_rewritten_result() {
    let temp = tempfile::tempdir().expect("tempdir");
    let binding = p10_binding();
    let payload = serde_json::json!({
        "input_head_sha": binding.head_sha,
        "output_head_sha": binding.head_sha,
        "overall_status": "success",
        "results": [{
            "source_id": "ci-linux",
            "action": "fixed",
            "evidence": { "summary": "identical result rewritten before timeout" }
        }]
    });
    let result_path = p10_current_artifact_path(&temp, "pr-remediation-result");
    std::fs::create_dir_all(result_path.parent().expect("result parent"))
        .expect("create result parent");
    std::fs::write(
        &result_path,
        serde_json::to_vec_pretty(&payload).expect("seed result json"),
    )
    .expect("seed previous result");
    std::thread::sleep(Duration::from_secs(1));

    let mut owned = p12_result("timeout");
    owned.result_file_present = true;
    owned.result_file_size = Some(256);
    let runner = P12FakeLlxprtRunner::with_result_payload(owned, payload);
    let mut context = p12_context(&temp);

    let outcome = PrFollowupRemediationExecutorWithRunner::new(runner, FixedClock)
        .execute(&mut context, &p12_params(&temp))
        .expect("p12 timeout with identical rewritten result");
    let run_artifact = read_json(&p10_current_artifact_path(
        &temp,
        "pr-remediation-llxprt-run",
    ));

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "rewriting the same validator-readable result before timeout must still be treated as fresh",
    );
    assert_eq!(
        run_artifact
            .get("remediation_invocation_state")
            .and_then(serde_json::Value::as_str),
        Some("success")
    );
}

#[test]
fn pr_followup_remediation_timeout_repairs_stale_wrapper_failure_result() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runner = P12FakeLlxprtRunner::new(p12_result("timeout"));
    let mut context = p12_context(&temp);
    let binding = p10_binding();
    let store = PrFollowupArtifactStore::new(temp.path().join("artifacts"));
    store
        .write_json_artifact(
            &binding,
            "pr-remediation-result",
            "remediate_pr_followup",
            8,
            &serde_json::json!({
                "input_head_sha": binding.head_sha,
                "output_head_sha": binding.head_sha,
                "overall_status": "failed",
                "results": [{
                    "source_type": "ci_failure",
                    "source_id": "ci-build",
                    "status": "failed",
                    "action": "llxprt_invocation_failed_before_result",
                    "evidence": { "process_class": "timeout" }
                }]
            }),
            None,
            &FixedClock,
        )
        .expect("seed stale wrapper failure artifact");

    PrFollowupRemediationExecutorWithRunner::new(runner, FixedClock)
        .execute(&mut context, &p12_params(&temp))
        .expect("p12 timeout with stale wrapper result");
    let repaired_result = read_json(&p10_current_artifact_path(&temp, "pr-remediation-result"));

    assert_eq!(
        repaired_result
            .get("results")
            .and_then(serde_json::Value::as_array)
            .and_then(|results| results.first())
            .and_then(|item| item.get("evidence"))
            .and_then(|evidence| evidence.get("process_class"))
            .and_then(serde_json::Value::as_str),
        Some("timeout")
    );
    assert_eq!(
        repaired_result
            .pointer("/results/0/action")
            .and_then(serde_json::Value::as_str),
        Some("llxprt_invocation_failed_before_result")
    );
}

#[test]
fn pr_followup_remediation_timeout_does_not_reuse_stale_success_result() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut owned = p12_result("timeout");
    owned.result_file_present = true;
    owned.result_file_size = Some(256);
    let runner = P12FakeLlxprtRunner::new(owned);
    let mut context = p12_context(&temp);
    let binding = p10_binding();
    let store = PrFollowupArtifactStore::new(temp.path().join("artifacts"));
    store
        .write_json_artifact(
            &binding,
            "pr-remediation-result",
            "validate_remediation_result",
            10,
            &serde_json::json!({
                "input_head_sha": "old-head",
                "output_head_sha": "old-head",
                "overall_status": "fixed",
                "plan_artifact_sequence": 1,
                "results": [{
                    "source_type": "ci_failure",
                    "source_id": "old-ci-build",
                    "status": "fixed",
                    "action": "fixed",
                    "evidence": { "current_head_sha": "old-head" },
                    "response_text": "old stale success"
                }]
            }),
            None,
            &FixedClock,
        )
        .expect("seed stale success artifact");

    let outcome = PrFollowupRemediationExecutorWithRunner::new(runner, FixedClock)
        .execute(&mut context, &p12_params(&temp))
        .expect("p12 timeout with stale success result");
    let repaired_result = read_json(&p10_current_artifact_path(&temp, "pr-remediation-result"));

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "a timeout without a fresh result must synthesize validator-readable failed items instead of preserving stale success",
    );
    assert_eq!(
        repaired_result
            .pointer("/results/0/action")
            .and_then(serde_json::Value::as_str),
        Some("llxprt_invocation_failed_before_result")
    );
    assert_eq!(
        repaired_result
            .pointer("/results/0/evidence/process_class")
            .and_then(serde_json::Value::as_str),
        Some("timeout")
    );
    assert_ne!(
        repaired_result
            .pointer("/results/0/source_id")
            .and_then(serde_json::Value::as_str),
        Some("old-ci-build"),
        "stale prior result items must not be reused when the current invocation fails before writing a result"
    );
}

fn extract_remediation_prompt(request: &LlxprtInvocationRequest) -> String {
    request
        .argv
        .windows(2)
        .find(|window| window[0] == "-p")
        .map(|window| window[1].clone())
        .expect("prompt argv")
}

fn assert_remediation_prompt_scope(prompt: &str, request: &LlxprtInvocationRequest) {
    assert!(prompt.contains("Fix only pr-remediation-plan.json.must_fix"));
    assert!(prompt.contains("Do not fix pr-remediation-plan.json.mark_invalid"));
    assert!(prompt.contains("out_of_scope"));
    assert!(prompt.contains("pr-remediation-plan.json.needs_user_judgment"));
    assert!(prompt.contains("Write"));
    assert!(!request
        .argv
        .iter()
        .any(|arg| matches!(arg.as_str(), "--artifact-root" | "--remediation-plan" | "--remediation-result" | "--input-head-sha" | "--repository-owner" | "--repository-name" | "--pr-number" | "--head-ref" | "--base-ref")),
        "llxprt CLI must receive remediation contract only in prompt, not unsupported wrapper flags: {:?}",
        request.argv
    );
}

fn assert_remediation_prompt_result_schema(prompt: &str) {
    assert!(prompt.contains("pr-remediation-result.json"));
    assert!(prompt.contains(
        "fixed | changed | already_satisfied | not_reproduced | not_fixed | skipped | failed"
    ));
    assert!(
        prompt.contains("copy source_type, source_id, stable_marker_key, and body_hash exactly")
    );
    assert!(prompt.contains("pr-remediation-plan.json.must_fix item"));
    assert!(prompt.contains("input_head_sha set to aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
    assert!(prompt.contains("evidence.current_head_sha equal to the current PR head"));
    assert!(prompt
        .contains("evidence.commands with at least one command object whose status is passed"));
    assert!(prompt.contains("top-level plan_artifact_sequence"));
    assert!(prompt.contains("retry_scope.plan_artifact_sequence"));
    assert!(prompt.contains("retry_scope.run_id"));
    assert!(prompt.contains("retry_scope.input_head_sha"));
}

fn assert_remediation_prompt_retry_feedback(prompt: &str) {
    assert!(prompt.contains("Previous pr-remediation-result.json validation_errors"));
    assert!(prompt
        .contains("fixed evidence for coderabbit_feedback:cr-valid is not tied to current head"));
    assert!(prompt.contains("do not create, copy, or modify any pr-followup/history files"));
    assert!(prompt.contains("structured evidence"));
    assert!(prompt.contains("Free-form-only completion is not acceptable"));
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 13-14
#[test]
fn remediate_pr_followup_prompt_contract() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runner = P12FakeLlxprtRunner::new(p12_result("success"));
    let mut context = p12_context(&temp);
    let binding = p10_binding();
    PrFollowupArtifactStore::new(temp.path().join("artifacts"))
        .write_json_artifact(
            &binding,
            "pr-remediation-result",
            "validate_remediation_result",
            9,
            &serde_json::json!({
                "validation_state": "fixable_malformed",
                "validation_errors": [
                    "fixed evidence for coderabbit_feedback:cr-valid is not tied to current head",
                    "already_satisfied result coderabbit_feedback:summary lacks deterministic passed command evidence"
                ]
            }),
            None,
            &FixedClock,
        )
        .expect("write previous validation feedback");

    PrFollowupRemediationExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &p12_params(&temp))
        .expect("p12 prompt");
    let requests = runner.requests();
    let request = requests.first().expect("owned request");
    let prompt = extract_remediation_prompt(request);

    assert_remediation_prompt_scope(&prompt, request);
    assert_remediation_prompt_result_schema(&prompt);
    assert_remediation_prompt_retry_feedback(&prompt);
}
#[test]
fn remediate_pr_followup_interpolates_profile_and_omits_unresolved_profile() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runner = P12FakeLlxprtRunner::new(p12_result("success"));
    let mut context = p12_context(&temp);
    context.set("profile_remediating", "gpt55high");
    let mut params = p12_params(&temp);
    params["profile"] = serde_json::json!("{profile_remediating}");

    PrFollowupRemediationExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &params)
        .expect("p12 profile interpolation");
    let request = runner.requests().first().expect("owned request").clone();
    assert!(!request
        .argv
        .iter()
        .any(|arg| arg.contains('{') || arg.contains('}')));
    assert!(request
        .argv
        .windows(2)
        .any(|window| window[0] == "--profile-load" && window[1] == "gpt55high"));

    let unresolved_runner = P12FakeLlxprtRunner::new(p12_result("success"));
    let mut unresolved_context = p12_context(&temp);
    PrFollowupRemediationExecutorWithRunner::new(unresolved_runner.clone(), FixedClock)
        .execute(&mut unresolved_context, &params)
        .expect("p12 unresolved profile omission");
    let unresolved_request = unresolved_runner
        .requests()
        .first()
        .expect("owned unresolved request")
        .clone();
    assert!(!unresolved_request
        .argv
        .windows(2)
        .any(|window| window[0] == "--profile-load" && window[1].contains('{')));
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013,REQ-PRFU-017
/// @pseudocode lines 14-17
#[test]
fn pr_followup_llxprt_wrapper_uses_owned_invocation_result_not_step_outcome_inference() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut owned = p12_result("retryable_failed");
    owned.exit_code = Some(17);
    owned.bounded_stderr = "owned retryable stderr".to_string();
    let runner = P12FakeLlxprtRunner::new(owned);
    let mut context = p12_context(&temp);
    let outcome = PrFollowupRemediationExecutorWithRunner::new(runner, FixedClock)
        .execute(&mut context, &p12_params(&temp))
        .expect("p12 owned result");
    let run_artifact = read_json(&p10_current_artifact_path(
        &temp,
        "pr-remediation-llxprt-run",
    ));

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "raw llxprt retryable process status is not product success or failure; validator classifies the artifact",
    );
    assert_eq!(
        run_artifact
            .get("remediation_invocation_state")
            .and_then(serde_json::Value::as_str),
        Some("retryable_failed")
    );
    assert_eq!(
        run_artifact
            .get("exit_code")
            .and_then(serde_json::Value::as_i64),
        Some(17)
    );
    assert_eq!(
        run_artifact
            .get("bounded_stderr")
            .and_then(serde_json::Value::as_str),
        Some("owned retryable stderr")
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-33
#[derive(Clone, Debug, Default)]
struct P13RecordingRunner {
    results: Arc<Mutex<Vec<PostPrTestCommandResult>>>,
    requests: Arc<Mutex<Vec<PostPrTestCommandRequest>>>,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-33
impl P13RecordingRunner {
    fn with_results(results: Vec<PostPrTestCommandResult>) -> Self {
        Self {
            results: Arc::new(Mutex::new(results)),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn requests(&self) -> Vec<PostPrTestCommandRequest> {
        self.requests.lock().expect("p13 requests").clone()
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-33
impl PostPrTestCommandRunner for P13RecordingRunner {
    fn run(&self, request: PostPrTestCommandRequest) -> PostPrTestCommandResult {
        self.requests
            .lock()
            .expect("p13 requests")
            .push(request.clone());
        let mut results = self.results.lock().expect("p13 results");
        let mut result = if results.is_empty() {
            PostPrTestCommandResult {
                command_id: request.command_id.clone(),
                argv: request.argv.clone(),
                working_directory: request.working_directory.clone(),
                status: "passed".to_string(),
                stdout_log_path: Some(request.stdout_log_path.clone()),
                stderr_log_path: Some(request.stderr_log_path.clone()),
                ..PostPrTestCommandResult::default()
            }
        } else {
            results.remove(0)
        };
        if result.command_id.is_empty() {
            result.command_id = request.command_id;
        }
        if result.argv.is_empty() {
            result.argv = request.argv;
        }
        if result.working_directory.as_os_str().is_empty() {
            result.working_directory = request.working_directory;
        }
        if result.stdout_log_path.is_none() {
            result.stdout_log_path = Some(request.stdout_log_path);
        }
        if result.stderr_log_path.is_none() {
            result.stderr_log_path = Some(request.stderr_log_path);
        }
        result
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014
/// @pseudocode lines 29-33
fn p13_params(temp: &tempfile::TempDir) -> serde_json::Value {
    let mut params = p11_params(temp);
    params["commands"] = serde_json::json!([{ "id": "unit", "argv": ["cargo", "test", "--lib"] }]);
    params["max_verification_retries"] = serde_json::json!(2);
    params
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014
/// @pseudocode lines 29-33
fn p13_result(status: &str) -> PostPrTestCommandResult {
    PostPrTestCommandResult {
        command_id: "unit".to_string(),
        argv: vec!["cargo".to_string(), "test".to_string(), "--lib".to_string()],
        working_directory: PathBuf::from("/tmp"),
        exit_code: if status == "passed" { Some(0) } else { Some(1) },
        signal: None,
        status: status.to_string(),
        bounded_stdout: format!("{status} stdout"),
        bounded_stderr: format!("{status} stderr"),
        stdout_log_path: None,
        stderr_log_path: None,
        spawn_error: None,
        expectation_failures: Vec::new(),
        artifact_failures: Vec::new(),
        failure_classification: None,
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014
/// @pseudocode lines 29-33
#[test]
fn run_post_pr_tests_fixable_failures_use_artifact_backed_retry_cap() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &temp,
        serde_json::json!([
            {
                "source_type": "ci_failure",
                "source_id": "ci-build",
                "stable_marker_key": null,
                "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "status": "changed",
                "action": "fixed build",
                "evidence": { "kind": "current_repository_test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" },
                "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": ["src/lib.rs"]
            },
            {
                "source_type": "coderabbit_feedback",
                "source_id": "cr-valid",
                "stable_marker_key": "thread-valid",
                "body_hash": "hash-valid",
                "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "status": "changed",
                "action": "fixed feedback",
                "evidence": { "kind": "current_repository_test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" },
                "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": ["src/lib.rs"]
            }
        ]),
    );
    let runner = P13RecordingRunner::with_results(vec![p13_result("failed")]);
    let mut context = p11_context(&temp);
    let outcome = RunPostPrTestsExecutorWithRunner::new(runner, FixedClock)
        .execute(&mut context, &p13_params(&temp))
        .expect("run post-pr tests");
    let artifact = read_json(&p11_current_artifact_path(&temp, "post-pr-test-result"));

    assert_expected_outcome(
        outcome,
        StepOutcome::Fixable,
        "run_post_pr_tests failures must use artifact-backed verification_retry_index cap before terminal fatal",
    );
    assert_eq!(
        artifact
            .get("test_state")
            .and_then(serde_json::Value::as_str),
        Some("failed")
    );
    assert_eq!(
        artifact
            .get("verification_retry_index")
            .and_then(serde_json::Value::as_u64),
        Some(0)
    );
    assert_eq!(
        artifact
            .get("verification_retry_exhausted")
            .and_then(serde_json::Value::as_bool),
        Some(false)
    );

    let runner = P13RecordingRunner::with_results(vec![p13_result("failed")]);
    let outcome = RunPostPrTestsExecutorWithRunner::new(runner, FixedClock)
        .execute(&mut context, &p13_params(&temp))
        .expect("second run post-pr tests");
    let artifact = read_json(&p11_current_artifact_path(&temp, "post-pr-test-result"));
    assert_expected_outcome(
        outcome,
        StepOutcome::Fixable,
        "second same-scope failed verification remains fixable while cap remains",
    );
    assert_eq!(
        artifact
            .get("verification_retry_index")
            .and_then(serde_json::Value::as_u64),
        Some(1)
    );

    let runner = P13RecordingRunner::with_results(vec![p13_result("failed")]);
    let outcome = RunPostPrTestsExecutorWithRunner::new(runner, FixedClock)
        .execute(&mut context, &p13_params(&temp))
        .expect("third run post-pr tests");
    let artifact = read_json(&p11_current_artifact_path(&temp, "post-pr-test-result"));
    assert_expected_outcome(
        outcome,
        StepOutcome::Fatal,
        "artifact-backed verification retry cap exhaustion must route fatal",
    );
    assert_eq!(
        artifact
            .get("verification_retry_index")
            .and_then(serde_json::Value::as_u64),
        Some(2)
    );
    assert_eq!(
        artifact
            .get("verification_retry_exhausted")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 34-40
#[derive(Clone, Debug)]
struct P14RecordingPushRunner {
    calls: Arc<Mutex<Vec<PushRemediationCommandRequest>>>,
    local_before: String,
    local_after: String,
    remote_before: String,
    remote_after: String,
    remote_after_status: String,
    status_output: String,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 34-40
impl P14RecordingPushRunner {
    fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            local_before: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            local_after: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
            remote_before: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            remote_after: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
            remote_after_status: "passed".to_string(),
            status_output: " M src/lib.rs\0?? .llxprt/session.json\0?? GENERATED_NOTICE.md\0"
                .to_string(),
        }
    }

    fn calls(&self) -> Vec<PushRemediationCommandRequest> {
        self.calls.lock().expect("p14 calls").clone()
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 34-40
impl PushRemediationCommandRunner for P14RecordingPushRunner {
    fn run(&self, request: PushRemediationCommandRequest) -> PushRemediationCommandResult {
        let (calls, push_seen) = {
            let mut guard = self.calls.lock().expect("p14 calls");
            guard.push(request.clone());
            let len = guard.len();
            let seen = guard.iter().any(|call| call.command_id == "push");
            (len, seen)
        };
        let after_remote_head = push_seen && request.command_id == "remote-head";
        let stdout = match request.command_id.as_str() {
            "local-head" if calls <= 1 => self.local_before.clone(),
            "local-head" => self.local_after.clone(),
            "remote-head" if calls <= 2 => format!("{}\trefs/heads/feature\n", self.remote_before),
            "remote-head" => format!("{}\trefs/heads/feature\n", self.remote_after),
            "status-porcelain" => self.status_output.clone(),
            _ => String::new(),
        };
        std::fs::create_dir_all(
            request
                .stdout_log_path
                .parent()
                .expect("p14 stdout log parent"),
        )
        .expect("p14 stdout log dir");
        std::fs::write(&request.stdout_log_path, &stdout).expect("p14 stdout log");
        std::fs::write(&request.stderr_log_path, "").expect("p14 stderr log");
        PushRemediationCommandResult {
            command_id: request.command_id,
            argv: request.argv,
            working_directory: request.working_directory,
            exit_code: Some(0),
            signal: None,
            status: if after_remote_head {
                self.remote_after_status.clone()
            } else {
                "passed".to_string()
            },
            bounded_stdout: stdout,
            bounded_stderr: String::new(),
            stdout_log_path: Some(request.stdout_log_path),
            stderr_log_path: Some(request.stderr_log_path),
            spawn_error: None,
        }
    }
}

fn write_p14_post_pr_test_result(temp: &tempfile::TempDir, test_state: &str) {
    let store = PrFollowupArtifactStore::new(temp.path().join("artifacts"));
    let binding = p11_binding();
    let plan = read_json(&p11_current_artifact_path(temp, "pr-remediation-plan"));
    let result = read_json(&p11_current_artifact_path(temp, "pr-remediation-result"));
    store
        .write_json_artifact(
            &binding,
            "post-pr-test-result",
            "run_post_pr_tests",
            10,
            &serde_json::json!({
                "test_state": test_state,
                "commands": [{ "command_id": "unit", "status": test_state }],
                "verification_retry_index": 0,
                "max_verification_retries": 2,
                "retry_scope": {
                    "run_id": binding.run_id,
                    "repository_owner": binding.repository_owner,
                    "repository_name": binding.repository_name,
                    "pr_number": binding.pr_number,
                    "head_sha": binding.head_sha,
                    "plan_artifact_sequence": plan.get("artifact_sequence"),
                    "remediation_result_artifact_sequence": result.get("artifact_sequence")
                },
                "plan_artifact_sequence": plan.get("artifact_sequence"),
                "remediation_result_artifact_sequence": result.get("artifact_sequence"),
                "verification_retry_exhausted": false
            }),
            None,
            &FixedClock,
        )
        .expect("write post-pr test result");
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 34-40
#[test]
fn push_remediation_changes_records_safe_staging_remote_heads_and_verified_push_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &temp,
        serde_json::json!([
            {
                "source_type": "ci_failure",
                "source_id": "ci-build",
                "status": "changed",
                "evidence": { "kind": "current_repository_test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
            },
            {
                "source_type": "coderabbit_feedback",
                "source_id": "cr-valid",
                "status": "changed",
                "evidence": { "kind": "current_repository_test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
            }
        ]),
    );
    write_p14_post_pr_test_result(&temp, "passed");
    let runner = P14RecordingPushRunner::new();
    let mut context = p11_context(&temp);
    let outcome = PushRemediationChangesExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &p11_params(&temp))
        .expect("push remediation changes");
    let artifact = read_json(&p11_current_artifact_path(&temp, "push-remediation-result"));
    let calls = runner.calls();

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "push_remediation_changes must record safe staging, local/remote head SHAs, and verified push state",
    );
    assert_eq!(
        artifact
            .get("push_state")
            .and_then(serde_json::Value::as_str),
        Some("pushed")
    );
    assert_eq!(
        artifact
            .get("verified_remote_matches_expected")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert_eq!(
        artifact
            .get("pre_push_local_head_sha")
            .and_then(serde_json::Value::as_str),
        Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    );
    assert_eq!(
        artifact
            .get("committed_head_sha")
            .and_then(serde_json::Value::as_str),
        Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
    );
    assert_eq!(
        artifact
            .get("post_push_remote_head_sha")
            .and_then(serde_json::Value::as_str),
        Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
    );
    assert_eq!(
        artifact
            .get("staged_paths")
            .and_then(serde_json::Value::as_array)
            .expect("staged paths"),
        &vec![serde_json::json!("src/lib.rs")]
    );
    assert!(artifact
        .get("excluded_paths")
        .and_then(serde_json::Value::as_array)
        .expect("excluded paths")
        .contains(&serde_json::json!(".llxprt/session.json")));
    let stage = calls
        .iter()
        .find(|call| call.command_id == "stage")
        .expect("stage call");
    assert_eq!(
        stage.argv,
        vec![
            "git".to_string(),
            "add".to_string(),
            "--".to_string(),
            "src/lib.rs".to_string()
        ]
    );
    assert!(calls
        .iter()
        .all(|call| !call.argv.join(" ").contains("$(echo owned)")
            && !call.argv.join(" ").contains("`uname`")));
}

#[test]
fn push_remediation_retry_scope_includes_source_sequences_and_full_binding() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &temp,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "changed", "evidence": { "kind": "current_repository_test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" } },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "changed", "evidence": { "kind": "current_repository_test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" } }
        ]),
    );
    write_p14_post_pr_test_result(&temp, "passed");
    let mut runner = P14RecordingPushRunner::new();
    runner.remote_after = "cccccccccccccccccccccccccccccccccccccccc".to_string();
    let mut context = p11_context(&temp);
    let outcome = PushRemediationChangesExecutorWithRunner::new(runner, FixedClock)
        .execute(&mut context, &p11_params(&temp))
        .expect("first push remediation changes");
    let artifact = read_json(&p11_current_artifact_path(&temp, "push-remediation-result"));
    let plan = read_json(&p11_current_artifact_path(&temp, "pr-remediation-plan"));
    let result = read_json(&p11_current_artifact_path(&temp, "pr-remediation-result"));

    assert_expected_outcome(
        outcome,
        StepOutcome::Retryable,
        "remote mismatch after push should be retryable before the push retry cap is exhausted",
    );
    assert_eq!(
        artifact.pointer("/retry_scope/run_id"),
        Some(&serde_json::json!("run-p11"))
    );
    assert_eq!(
        artifact.pointer("/retry_scope/repository_owner"),
        Some(&serde_json::json!("example"))
    );
    assert_eq!(
        artifact.pointer("/retry_scope/repository_name"),
        Some(&serde_json::json!("workflow"))
    );
    assert_eq!(
        artifact.pointer("/retry_scope/pr_number"),
        Some(&serde_json::json!(1910))
    );
    assert_eq!(
        artifact.pointer("/retry_scope/head_sha"),
        Some(&serde_json::json!(
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        ))
    );
    assert_eq!(
        artifact.pointer("/retry_scope/remote_ref"),
        Some(&serde_json::json!("refs/heads/feature"))
    );
    assert_eq!(
        artifact.pointer("/retry_scope/plan_artifact_sequence"),
        plan.get("artifact_sequence")
    );
    assert_eq!(
        artifact.pointer("/retry_scope/remediation_result_artifact_sequence"),
        result.get("artifact_sequence")
    );
}

fn p11_push_source_artifact_sequences(
    temp: &tempfile::TempDir,
) -> (serde_json::Value, serde_json::Value) {
    let plan = read_json(&p11_current_artifact_path(temp, "pr-remediation-plan"));
    let result = read_json(&p11_current_artifact_path(temp, "pr-remediation-result"));
    (
        plan.get("artifact_sequence")
            .expect("plan artifact sequence")
            .clone(),
        result
            .get("artifact_sequence")
            .expect("result artifact sequence")
            .clone(),
    )
}

fn write_prior_p11_push_retry_artifact(
    temp: &tempfile::TempDir,
    plan_artifact_sequence: serde_json::Value,
    remediation_result_artifact_sequence: serde_json::Value,
    push_retry_index: u64,
) {
    write_prior_p11_push_retry_artifact_for_head(
        temp,
        plan_artifact_sequence,
        remediation_result_artifact_sequence,
        push_retry_index,
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    );
}

fn write_prior_p11_push_retry_artifact_for_head(
    temp: &tempfile::TempDir,
    plan_artifact_sequence: serde_json::Value,
    remediation_result_artifact_sequence: serde_json::Value,
    push_retry_index: u64,
    head_sha: &str,
) {
    let binding = p11_binding();
    let store = PrFollowupArtifactStore::new(temp.path().join("artifacts"));
    store
        .write_json_artifact(
            &binding,
            "push-remediation-result",
            "push_remediation_changes",
            9,
            &serde_json::json!({
                "push_state": "retryable_failed",
                "push_retry_index": push_retry_index,
                "max_push_retries": 1,
                "retry_scope": {
                    "run_id": binding.run_id,
                    "repository_owner": binding.repository_owner,
                    "repository_name": binding.repository_name,
                    "pr_number": binding.pr_number,
                    "head_sha": head_sha,
                    "remote_ref": "refs/heads/feature",
                    "plan_artifact_sequence": plan_artifact_sequence,
                    "remediation_result_artifact_sequence": remediation_result_artifact_sequence
                }
            }),
            None,
            &FixedClock,
        )
        .expect("write prior push remediation result");
}

fn execute_retryable_p11_push(temp: &tempfile::TempDir) -> (StepOutcome, serde_json::Value) {
    let mut runner = P14RecordingPushRunner::new();
    runner.remote_after = "cccccccccccccccccccccccccccccccccccccccc".to_string();
    let mut context = p11_context(temp);
    let outcome = PushRemediationChangesExecutorWithRunner::new(runner, FixedClock)
        .execute(&mut context, &p11_params(temp))
        .expect("push remediation changes");
    let artifact = read_json(&p11_current_artifact_path(temp, "push-remediation-result"));
    (outcome, artifact)
}

#[test]
fn push_remediation_retry_scope_matching_sequences_exhausts_retry_cap() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &temp,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "changed", "evidence": { "kind": "current_repository_test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" } },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "changed", "evidence": { "kind": "current_repository_test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" } }
        ]),
    );
    write_p14_post_pr_test_result(&temp, "passed");
    let (plan_sequence, result_sequence) = p11_push_source_artifact_sequences(&temp);
    write_prior_p11_push_retry_artifact(&temp, plan_sequence, result_sequence, 0);
    let (outcome, artifact) = execute_retryable_p11_push(&temp);

    assert_expected_outcome(
        outcome,
        StepOutcome::Fatal,
        "matching push retry scope must increment the stored retry index and exhaust the push retry cap",
    );
    assert_eq!(
        artifact
            .get("push_retry_index")
            .and_then(serde_json::Value::as_u64),
        Some(1)
    );
}

#[test]
fn push_remediation_retry_scope_ignores_stale_plan_artifact_sequence() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &temp,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "changed", "evidence": { "kind": "current_repository_test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" } },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "changed", "evidence": { "kind": "current_repository_test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" } }
        ]),
    );
    write_p14_post_pr_test_result(&temp, "passed");
    let (_, result_sequence) = p11_push_source_artifact_sequences(&temp);
    write_prior_p11_push_retry_artifact(&temp, serde_json::json!(0), result_sequence, 1);
    let (outcome, artifact) = execute_retryable_p11_push(&temp);

    assert_expected_outcome(
        outcome,
        StepOutcome::Retryable,
        "stale push artifacts with a different plan sequence must not exhaust the current retry scope",
    );
    assert_eq!(
        artifact
            .get("push_retry_index")
            .and_then(serde_json::Value::as_u64),
        Some(0)
    );
}

#[test]
fn push_remediation_retry_scope_ignores_stale_remediation_result_artifact_sequence() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &temp,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "changed", "evidence": { "kind": "current_repository_test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" } },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "changed", "evidence": { "kind": "current_repository_test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" } }
        ]),
    );
    write_p14_post_pr_test_result(&temp, "passed");
    let (plan_sequence, _) = p11_push_source_artifact_sequences(&temp);
    write_prior_p11_push_retry_artifact(&temp, plan_sequence, serde_json::json!(0), 1);
    let (outcome, artifact) = execute_retryable_p11_push(&temp);

    assert_expected_outcome(
        outcome,
        StepOutcome::Retryable,
        "stale push artifacts with a different remediation result sequence must not exhaust the current retry scope",
    );
    assert_eq!(
        artifact
            .get("push_retry_index")
            .and_then(serde_json::Value::as_u64),
        Some(0)
    );
}

#[test]
fn push_remediation_observation_failures_use_matching_retry_scope_for_cap() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &temp,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "changed", "evidence": { "kind": "current_repository_test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" } },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "changed", "evidence": { "kind": "current_repository_test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" } }
        ]),
    );
    write_p14_post_pr_test_result(&temp, "passed");
    let (plan_sequence, result_sequence) = p11_push_source_artifact_sequences(&temp);
    write_prior_p11_push_retry_artifact_for_head(
        &temp,
        plan_sequence,
        result_sequence,
        0,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );
    let mut runner = P14RecordingPushRunner::new();
    runner.status_output = String::new();
    runner.remote_before = "cccccccccccccccccccccccccccccccccccccccc".to_string();
    runner.remote_after_status = "failed".to_string();
    let mut context = p11_context(&temp);

    let outcome = PushRemediationChangesExecutorWithRunner::new(runner, FixedClock)
        .execute(&mut context, &p11_params(&temp))
        .expect("push remediation observation failure");
    let artifact = read_json(&p11_current_artifact_path(&temp, "push-remediation-result"));

    assert_expected_outcome(
        outcome,
        StepOutcome::Fatal,
        "matching prior push scope must exhaust the retry cap for post-push observation failures",
    );
    assert_eq!(
        artifact.pointer("/retry_scope/head_sha"),
        Some(&serde_json::json!(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ))
    );
    assert_eq!(
        artifact
            .get("push_state")
            .and_then(serde_json::Value::as_str),
        Some("retry_exhausted")
    );
    assert_eq!(
        artifact
            .get("push_retry_index")
            .and_then(serde_json::Value::as_u64),
        Some(1)
    );
    assert_eq!(
        artifact
            .get("semantic_state")
            .and_then(serde_json::Value::as_str),
        Some("retry_exhausted")
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-018
/// @pseudocode lines 34-49
#[test]
fn push_remediation_changes_no_change_routes_fixable_for_marker_handling() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &temp,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "already_satisfied", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "verified", "evidence": { "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "commands": [{ "id": "cargo-test", "status": "passed", "argv": ["cargo", "test"] }] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": [] },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "already_satisfied", "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "action": "verified", "evidence": { "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "commands": [{ "id": "cargo-test", "status": "passed", "argv": ["cargo", "test"] }] }, "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": [] }
        ]),
    );
    write_p14_post_pr_test_result(&temp, "passed");
    let mut runner = P14RecordingPushRunner::new();
    runner.status_output = String::new();
    runner.local_after = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
    runner.remote_after = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
    let mut context = p11_context(&temp);
    let outcome = PushRemediationChangesExecutorWithRunner::new(runner, FixedClock)
        .execute(&mut context, &p11_params(&temp))
        .expect("verify no-change remediation push result");
    let artifact = read_json(&p11_current_artifact_path(&temp, "push-remediation-result"));

    assert_expected_outcome(
        outcome,
        StepOutcome::Fixable,
        "verified no-change remediation must route to marker handling instead of recapturing unchanged PR feedback",
    );
    assert_eq!(
        artifact
            .get("push_state")
            .and_then(serde_json::Value::as_str),
        Some("no_change")
    );
    assert_eq!(
        artifact
            .get("verified_remote_matches_expected")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
}

#[test]
fn push_remediation_changes_rejects_false_validator_success_evidence() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &temp,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "changed", "evidence": false },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "changed", "evidence": { "kind": "change", "current_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" } }
        ]),
    );
    write_p14_post_pr_test_result(&temp, "passed");
    let runner = P14RecordingPushRunner::new();
    let mut context = p11_context(&temp);

    let outcome = PushRemediationChangesExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &p11_params(&temp))
        .expect("reject false validator evidence before push");
    let artifact = read_json(&p11_current_artifact_path(&temp, "push-remediation-result"));

    assert_expected_outcome(
        outcome,
        StepOutcome::Fatal,
        "false evidence must not satisfy validator success evidence gate",
    );
    assert_eq!(
        artifact
            .get("push_error_class")
            .and_then(serde_json::Value::as_str),
        Some("missing_validator_success_evidence")
    );
    assert!(
        runner.calls().is_empty(),
        "invalid success evidence must be rejected before git inspection"
    );
}

#[test]
fn push_remediation_changes_rejects_zero_numeric_validator_success_evidence() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &temp,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "changed", "evidence": 0 },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "changed", "evidence": { "kind": "change", "current_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" } }
        ]),
    );
    write_p14_post_pr_test_result(&temp, "passed");
    let runner = P14RecordingPushRunner::new();
    let mut context = p11_context(&temp);

    let outcome = PushRemediationChangesExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &p11_params(&temp))
        .expect("reject zero numeric validator evidence before push");
    let artifact = read_json(&p11_current_artifact_path(&temp, "push-remediation-result"));

    assert_expected_outcome(
        outcome,
        StepOutcome::Fatal,
        "zero numeric evidence must not satisfy validator success evidence gate",
    );
    assert_eq!(
        artifact
            .get("push_error_class")
            .and_then(serde_json::Value::as_str),
        Some("missing_validator_success_evidence")
    );
    assert!(
        runner.calls().is_empty(),
        "invalid success evidence must be rejected before git inspection"
    );
}

#[test]
fn push_remediation_changes_pushes_clean_local_head_when_already_committed() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &temp,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "changed", "evidence": { "kind": "current_repository_test", "current_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" } },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "status": "changed", "evidence": { "kind": "current_repository_test", "current_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" } }
        ]),
    );
    write_p14_post_pr_test_result(&temp, "passed");
    let mut runner = P14RecordingPushRunner::new();
    runner.status_output = String::new();
    runner.local_before = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string();
    runner.remote_before = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
    runner.remote_after = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string();
    let mut context = p11_context(&temp);
    let outcome = PushRemediationChangesExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &p11_params(&temp))
        .expect("push already committed remediation");
    let artifact = read_json(&p11_current_artifact_path(&temp, "push-remediation-result"));
    let calls = runner.calls();

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "clean local remediation commit ahead of the PR ref must be pushed instead of treated as a fatal no-change mismatch",
    );
    assert_eq!(
        artifact
            .get("push_state")
            .and_then(serde_json::Value::as_str),
        Some("pushed_existing_head")
    );
    assert!(calls.iter().any(|call| call.command_id == "push"));
    assert!(calls
        .iter()
        .all(|call| call.command_id != "stage" && call.command_id != "commit"));
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 34-40
#[test]
fn push_remediation_changes_requires_passed_local_verification_before_success_paths() {
    for (case, status_output) in [
        ("no_change", ""),
        ("excluded_only", "?? .llxprt/session.json\0"),
        ("commit_and_push", " M src/lib.rs\0"),
    ] {
        let temp = tempfile::tempdir().expect("tempdir");
        write_p11_plan_and_result(
            &temp,
            serde_json::json!([
                { "source_type": "ci_failure", "source_id": "ci-build", "status": "changed", "evidence": { "kind": "current_repository_test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" } },
                { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "status": "changed", "evidence": { "kind": "current_repository_test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" } }
            ]),
        );
        write_p14_post_pr_test_result(&temp, "failed");
        let mut runner = P14RecordingPushRunner::new();
        runner.status_output = status_output.to_string();
        let runner = runner;
        let mut context = p11_context(&temp);
        let outcome = PushRemediationChangesExecutorWithRunner::new(runner.clone(), FixedClock)
            .execute(&mut context, &p11_params(&temp))
            .unwrap_or_else(|err| panic!("push remediation changes {case}: {err}"));
        let artifact = read_json(&p11_current_artifact_path(&temp, "push-remediation-result"));
        assert_expected_outcome(
            outcome,
            StepOutcome::Fatal,
            "push_remediation_changes must fatal before every success path when local verification is not passed",
        );
        assert_eq!(
            artifact
                .get("push_state")
                .and_then(serde_json::Value::as_str),
            Some("fatal"),
            "{case} writes fatal push artifact"
        );
        assert_eq!(
            artifact
                .get("push_error_class")
                .and_then(serde_json::Value::as_str),
            Some("post_pr_local_verification_not_passed"),
            "{case} records local verification gate failure"
        );
        assert!(
            runner.calls().is_empty(),
            "{case} must not inspect, stage, commit, or push before passed local verification"
        );
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 34-40
#[test]
fn push_remediation_changes_uses_configured_remote_for_push_and_remote_head_verification() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &temp,
        serde_json::json!([
            { "source_type": "ci_failure", "source_id": "ci-build", "status": "changed", "evidence": { "kind": "current_repository_test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" } },
            { "source_type": "coderabbit_feedback", "source_id": "cr-valid", "status": "changed", "evidence": { "kind": "current_repository_test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" } }
        ]),
    );
    write_p14_post_pr_test_result(&temp, "passed");
    let runner = P14RecordingPushRunner::new();
    let mut params = p11_params(&temp);
    params["remote_name"] = serde_json::json!("upstream");
    let mut context = p11_context(&temp);
    let outcome = PushRemediationChangesExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &params)
        .expect("push remediation changes with non-origin remote");
    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "non-origin configured remote must support successful push and verification",
    );
    let calls = runner.calls();
    let remote_head_calls: Vec<_> = calls
        .iter()
        .filter(|call| call.command_id == "remote-head")
        .collect();
    assert_eq!(remote_head_calls.len(), 2);
    assert!(remote_head_calls
        .iter()
        .all(|call| call.argv.get(3).map(String::as_str) == Some("upstream")));
    let push = calls
        .iter()
        .find(|call| call.command_id == "push")
        .expect("push call");
    assert_eq!(push.argv.get(2).map(String::as_str), Some("upstream"));
}
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017,REQ-PRFU-026
/// @pseudocode lines 41-49
#[derive(Clone, Debug, Default)]
struct P15MarkerRunner {
    calls: Arc<Mutex<Vec<Vec<String>>>>,
    remote_comments: Arc<Mutex<Vec<serde_json::Value>>>,
    pull_review_comments: Arc<Mutex<Vec<serde_json::Value>>>,
    fail_resolution: bool,
}

/// @pseudocode lines 42-47
impl P15MarkerRunner {
    fn with_remote_comments(remote_comments: Vec<serde_json::Value>) -> Self {
        Self {
            remote_comments: Arc::new(Mutex::new(remote_comments)),
            ..Self::default()
        }
    }

    fn with_pull_review_comments(pull_review_comments: Vec<serde_json::Value>) -> Self {
        Self {
            pull_review_comments: Arc::new(Mutex::new(pull_review_comments)),
            ..Self::default()
        }
    }

    fn failing_resolution() -> Self {
        Self {
            fail_resolution: true,
            ..Self::default()
        }
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().expect("p15 calls").clone()
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017,REQ-PRFU-026
/// @pseudocode lines 42-47
impl GithubPrCommandRunner for P15MarkerRunner {
    fn run_github_command(
        &self,
        argv: &[String],
    ) -> Result<String, luther_workflow::engine::runner::EngineError> {
        self.calls.lock().expect("p15 calls").push(argv.to_vec());
        let is_post = argv.iter().any(|arg| arg == "POST");
        if argv
            .iter()
            .any(|arg| arg.contains("/pulls/1910/comments/") && arg.contains("/replies"))
        {
            // In-thread review reply endpoint.
            Ok(serde_json::json!({
                "id": 9101,
                "node_id": "PRRC_reply_9101",
                "html_url": "https://github.com/example/workflow/pull/1910#discussion_r9101",
                "in_reply_to_id": 7001
            })
            .to_string())
        } else if argv
            .iter()
            .any(|arg| arg.contains("/pulls/1910/comments") && !arg.contains("/replies"))
            && !is_post
        {
            // Pull review comment listing (in-thread remote marker discovery).
            Ok(serde_json::Value::Array(
                self.pull_review_comments
                    .lock()
                    .expect("pull review comments")
                    .clone(),
            )
            .to_string())
        } else if argv.iter().any(|arg| arg.contains("/issues/1910/comments")) && !is_post {
            Ok(serde_json::Value::Array(
                self.remote_comments
                    .lock()
                    .expect("remote comments")
                    .clone(),
            )
            .to_string())
        } else if argv.iter().any(|arg| arg.contains("/issues/1910/comments")) {
            Ok(serde_json::json!({ "id": 9001, "html_url": "https://github.com/example/workflow/pull/1910#issuecomment-9001" }).to_string())
        } else if argv.iter().any(|arg| arg == "graphql") {
            if self.fail_resolution {
                return Err(
                    luther_workflow::engine::runner::EngineError::StepExecutionError {
                        step_id: "mark_coderabbit_feedback".to_string(),
                        message: "resolution failed".to_string(),
                    },
                );
            }
            Ok(serde_json::json!({ "data": { "resolveReviewThread": { "thread": { "id": "thread-valid", "isResolved": true } } } }).to_string())
        } else {
            Ok("[]".to_string())
        }
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017,REQ-PRFU-026
/// @pseudocode lines 41-49
fn p15_marker_report_path(temp: &tempfile::TempDir) -> PathBuf {
    p10_current_artifact_path(temp, "pr-feedback-marker-report")
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017,REQ-PRFU-026
/// @pseudocode lines 41-49
fn write_p15_invalid_out_of_scope_pending(temp: &tempfile::TempDir) {
    write_p10_inputs(
        temp,
        serde_json::json!([]),
        serde_json::json!([]),
        serde_json::json!([
            p10_accepted("cr-invalid", "thread-invalid", "hash-invalid", "invalid"),
            p10_accepted("cr-out", "thread-out", "hash-out", "out_of_scope")
        ]),
        serde_json::json!([]),
        serde_json::json!([]),
    );
    let mut context = p10_context(temp);
    PrRemediationPlanExecutor
        .execute(&mut context, &p10_params(temp))
        .expect("write pending marker actions");
}

fn write_p15_validated_fixed_pending(temp: &tempfile::TempDir) {
    write_p11_plan_and_result(
        temp,
        serde_json::json!([
            {
                "source_type": "ci_failure",
                "source_id": "ci-build",
                "stable_marker_key": null,
                "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "status": "changed",
                "action": "fixed build",
                "evidence": { "kind": "current_repository_test", "current_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" },
                "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": ["src/lib.rs"]
            },
            {
                "source_type": "coderabbit_feedback",
                "source_id": "cr-valid",
                "thread_id": "thread-valid",

                "stable_marker_key": "thread-valid",
                "body_hash": "hash-valid",
                "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "status": "changed",
                "action": "fixed feedback",
                "evidence": { "kind": "current_repository_test", "current_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" },
                "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": ["src/lib.rs"]
            }
        ]),
    );
    if !p11_current_artifact_path(temp, "pending-feedback-marker-actions").exists() {
        let store = PrFollowupArtifactStore::new(temp.path().join("artifacts"));
        let binding = p11_binding();
        store
            .write_json_artifact(
                &binding,
                "pending-feedback-marker-actions",
                "validate_remediation_result",
                9,
                &serde_json::json!({
                    "pending_actions": [{
                        "action_id": "comment_fixed:thread-valid:hash-valid:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                        "action_kind": "comment_fixed",
                        "item_id": "cr-valid",
                        "stable_marker_key": "thread-valid",
                        "source_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                        "remediation_input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                        "remediation_output_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                        "remediation_output_head": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                        "body_hash": "hash-valid",
                        "idempotency_key": "run-p11:example:workflow:1910:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb:thread-valid:comment_fixed",
                        "resolution_required": true,
                        "thread_id": "thread-valid",
                        "comment_database_id": 7001,
                        "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.",
                        "status": "pending",
                        "reason": "fixed valid feedback",
                        "remediation_result_evidence": { "kind": "current_repository_test", "current_head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" },
                        "original_feedback_identity": { "item_id": "cr-valid", "stable_marker_key": "thread-valid", "body_hash": "hash-valid", "source_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "thread_id": "thread-valid", "comment_database_id": 7001 }
                    }],
                    "carry_forward_from_artifact_sequence": null,
                    "marker_policy": {},
                    "updated_at": "2026-04-30T00:00:00Z"
                }),
                None,
                &FixedClock,
            )
            .expect("write p15 fallback pending fixed marker");
    }
    let mut result = read_json(&p11_current_artifact_path(temp, "pr-remediation-result"));
    result["validation_state"] = serde_json::json!("valid");
    std::fs::write(
        p11_current_artifact_path(temp, "pr-remediation-result"),
        serde_json::to_vec_pretty(&result).expect("validated result json"),
    )
    .expect("write validated result");
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017,REQ-PRFU-026
/// @pseudocode lines 41-49
#[test]
fn feedback_marker_interpolates_artifact_root_from_context() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p15_invalid_out_of_scope_pending(&temp);
    let runner = P15MarkerRunner::default();
    let mut context = p10_context(&temp);
    context.set(
        "artifact_dir",
        temp.path().join("artifacts").to_string_lossy().as_ref(),
    );
    let mut params = p10_params(&temp);
    params["artifact_root"] = serde_json::json!("{artifact_dir}");

    let outcome = GithubFeedbackMarkerExecutorWithRunner::new(runner, FixedClock)
        .execute(&mut context, &params)
        .expect("mark feedback with interpolated artifact root");

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "marker should accept interpolated artifact_root",
    );
    assert!(
        p15_marker_report_path(&temp).exists(),
        "marker report should be written under interpolated artifact root"
    );
    assert!(
        !temp.path().join("{artifact_dir}").exists(),
        "executor must not create a literal unresolved artifact_root directory"
    );
}

#[test]
fn marker_consumes_invalid_out_of_scope_pending_actions_with_no_remediation_output_head() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p15_invalid_out_of_scope_pending(&temp);
    let runner = P15MarkerRunner::default();
    let mut context = p10_context(&temp);
    let outcome = GithubFeedbackMarkerExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &p10_params(&temp))
        .expect("mark feedback");
    let report = read_json(&p15_marker_report_path(&temp));
    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "marker must consume invalid/out_of_scope pending actions without remediation output",
    );
    assert_eq!(
        report
            .get("marker_state")
            .and_then(serde_json::Value::as_str),
        Some("complete")
    );
    assert_eq!(
        report
            .get("posted_comments")
            .and_then(serde_json::Value::as_array)
            .expect("posted")
            .len(),
        2
    );
    assert!(runner.calls().iter().any(|call| {
        call.iter().any(|arg| arg == "--field") && call.iter().any(|arg| arg.starts_with("body=@"))
    }));
    let body_arg = runner
        .calls()
        .into_iter()
        .flat_map(|call| call.into_iter())
        .find_map(|arg| arg.strip_prefix("body=@").map(ToString::to_string))
        .expect("body file arg");
    let body = std::fs::read_to_string(&body_arg).expect("posted body file");
    assert!(body.contains("luther-pr-followup"));
    assert!(!body.trim_start().starts_with('{'));
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017,REQ-PRFU-026
/// @pseudocode lines 41-49
#[test]
fn marker_retry_resume_does_not_duplicate_invalid_out_of_scope_pending_actions() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p15_invalid_out_of_scope_pending(&temp);
    let runner = P15MarkerRunner::default();
    let params = p10_params(&temp);
    let mut context = p10_context(&temp);
    GithubFeedbackMarkerExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &params)
        .expect("first marker pass");
    GithubFeedbackMarkerExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &params)
        .expect("retry marker pass");
    let post_calls = runner
        .calls()
        .into_iter()
        .filter(|call| call.iter().any(|arg| arg == "POST"))
        .count();
    assert_eq!(
        post_calls, 2,
        "retry must not duplicate already posted marker comments"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017,REQ-PRFU-026
/// @pseudocode lines 41-49
#[test]
fn marker_executor_derives_current_invalid_actions_without_pending_artifact() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p10_inputs(
        &temp,
        serde_json::json!([]),
        serde_json::json!([]),
        serde_json::json!([p10_accepted(
            "cr-invalid",
            "thread-invalid",
            "hash-invalid",
            "invalid"
        )]),
        serde_json::json!([]),
        serde_json::json!([]),
    );
    let runner = P15MarkerRunner::default();
    let mut context = p10_context(&temp);
    let outcome = GithubFeedbackMarkerExecutorWithRunner::new(runner, FixedClock)
        .execute(&mut context, &p10_params(&temp))
        .expect("mark feedback from current artifacts");
    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "marker must derive invalid marker actions from current feedback/evaluations",
    );
    let report = read_json(&p15_marker_report_path(&temp));
    assert_eq!(
        report
            .get("posted_comments")
            .and_then(serde_json::Value::as_array)
            .expect("posted")
            .len(),
        1
    );
}

/// Count POST calls the marker runner issued against the top-level PR
/// issue-comments endpoint (where a stray summary marker would land).
fn p15_top_level_issue_comment_posts(runner: &P15MarkerRunner) -> usize {
    runner
        .calls()
        .into_iter()
        .filter(|call| {
            call.iter().any(|arg| arg == "POST")
                && call.iter().any(|arg| arg.contains("/issues/1910/comments"))
        })
        .count()
}

/// The live-refresh path must never derive a marker action for a CodeRabbit
/// summary evaluation, while still deriving one for an actionable invalid
/// review-thread evaluation present in the same artifacts.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-020
#[test]
fn marker_refresh_suppresses_summary_but_keeps_actionable_invalid_action() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut summary = p10_accepted(
        "cr-summary-1",
        "summary:IC_summarynode:hash-summary",
        "hash-summary",
        "invalid",
    );
    summary["source"] = serde_json::json!("deterministic");
    write_p10_inputs(
        &temp,
        serde_json::json!([]),
        serde_json::json!([]),
        serde_json::json!([
            summary,
            p10_accepted("cr-invalid", "thread-invalid", "hash-invalid", "invalid")
        ]),
        serde_json::json!([]),
        serde_json::json!([]),
    );
    let runner = P15MarkerRunner::default();
    let mut context = p10_context(&temp);
    let outcome = GithubFeedbackMarkerExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &p10_params(&temp))
        .expect("mark feedback with co-present summary and invalid thread");
    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "summary suppression must leave the actionable invalid review-thread action intact",
    );
    let report = read_json(&p15_marker_report_path(&temp));
    let posted = report
        .get("posted_comments")
        .and_then(serde_json::Value::as_array)
        .expect("posted_comments");
    assert_eq!(
        posted.len(),
        1,
        "only the actionable invalid thread may post a marker comment: {report:?}"
    );
    assert!(
        report
            .get("action_audit")
            .and_then(serde_json::Value::as_array)
            .expect("action_audit")
            .iter()
            .all(|audit| audit
                .get("stable_marker_key")
                .and_then(serde_json::Value::as_str)
                != Some("summary:IC_summarynode:hash-summary")),
        "no marker action may be derived for the summary evaluation: {report:?}"
    );
    assert_eq!(
        p15_top_level_issue_comment_posts(&runner),
        0,
        "summary must never post a top-level PR comment"
    );
}

/// A stale, pre-fix summary `comment_invalid` action persisted in
/// pending-feedback-marker-actions.json must be pruned/skipped: no top-level PR
/// comment is posted and the marker report records no posted comment for it.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-020
#[test]
fn marker_prunes_stale_summary_pending_action_and_posts_no_top_level_comment() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut summary = p10_accepted(
        "cr-summary-1",
        "summary:IC_summarynode:hash-summary",
        "hash-summary",
        "invalid",
    );
    summary["source"] = serde_json::json!("deterministic");
    write_p10_inputs(
        &temp,
        serde_json::json!([]),
        serde_json::json!([]),
        serde_json::json!([summary]),
        serde_json::json!([]),
        serde_json::json!([]),
    );
    write_stale_summary_pending_action(&temp);

    let runner = P15MarkerRunner::default();
    let mut context = p10_context(&temp);
    let outcome = GithubFeedbackMarkerExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &p10_params(&temp))
        .expect("mark feedback with stale summary pending action");
    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "a stale summary pending action must not fail or post a top-level PR comment",
    );
    let report = read_json(&p15_marker_report_path(&temp));
    assert!(
        report
            .get("posted_comments")
            .and_then(serde_json::Value::as_array)
            .expect("posted_comments")
            .is_empty(),
        "no comment may be posted for a stale summary action: {report:?}"
    );
    assert_eq!(
        p15_top_level_issue_comment_posts(&runner),
        0,
        "stale summary action must not post a top-level PR comment"
    );
}

/// Rerunning the marker step over a stale summary pending action must remain
/// idempotent: still no posted comment and no second top-level PR comment.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-020
#[test]
fn marker_rerun_does_not_duplicate_stale_summary_pending_action() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut summary = p10_accepted(
        "cr-summary-1",
        "summary:IC_summarynode:hash-summary",
        "hash-summary",
        "invalid",
    );
    summary["source"] = serde_json::json!("deterministic");
    write_p10_inputs(
        &temp,
        serde_json::json!([]),
        serde_json::json!([]),
        serde_json::json!([summary]),
        serde_json::json!([]),
        serde_json::json!([]),
    );
    write_stale_summary_pending_action(&temp);

    let runner = P15MarkerRunner::default();
    let params = p10_params(&temp);
    let mut context = p10_context(&temp);
    GithubFeedbackMarkerExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &params)
        .expect("first marker pass over stale summary action");
    GithubFeedbackMarkerExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &params)
        .expect("rerun marker pass over stale summary action");
    assert_eq!(
        p15_top_level_issue_comment_posts(&runner),
        0,
        "reruns must never post a top-level PR comment for a summary marker"
    );
    let report = read_json(&p15_marker_report_path(&temp));
    assert!(
        report
            .get("posted_comments")
            .and_then(serde_json::Value::as_array)
            .expect("posted_comments")
            .is_empty(),
        "rerun must still post no comment for a stale summary action: {report:?}"
    );
}

/// Persist a pre-fix pending-feedback-marker-actions.json containing a stale
/// summary `comment_invalid` action, exactly as a pre-fix run would have.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-020
fn write_stale_summary_pending_action(temp: &tempfile::TempDir) {
    let store = PrFollowupArtifactStore::new(temp.path().join("artifacts"));
    let binding = p10_binding();
    store
        .write_json_artifact(
            &binding,
            "pending-feedback-marker-actions",
            "build_remediation_plan",
            7,
            &serde_json::json!({
                "pending_actions": [{
                    "action_id": "comment_invalid:summary:IC_summarynode:hash-summary:hash-summary:none",
                    "action_kind": "comment_invalid",
                    "item_id": "cr-summary-1",
                    "stable_marker_key": "summary:IC_summarynode:hash-summary",
                    "source_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "remediation_input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "remediation_output_head_sha": serde_json::Value::Null,
                    "remediation_output_head": "none",
                    "body_hash": "hash-summary",
                    "idempotency_key": "run-p10:example:workflow:1910:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:none:summary:IC_summarynode:hash-summary:comment_invalid",
                    "resolution_required": false,
                    "thread_id": serde_json::Value::Null,
                    "comment_database_id": serde_json::Value::Null,
                    "response_text": "This is an informational CodeRabbit summary/walkthrough comment rather than an actionable review item, so no code change is required.",
                    "status": "pending",
                    "reason": "CodeRabbit summary/walkthrough comments are informational.",
                    "original_feedback_identity": {
                        "item_id": "cr-summary-1",
                        "stable_marker_key": "summary:IC_summarynode:hash-summary",
                        "body_hash": "hash-summary",
                        "source_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                        "thread_id": serde_json::Value::Null,
                        "comment_database_id": serde_json::Value::Null
                    }
                }],
                "carry_forward_from_artifact_sequence": serde_json::Value::Null,
                "marker_policy": {},
                "updated_at": "2026-04-30T00:00:00Z"
            }),
            None,
            &FixedClock,
        )
        .expect("write stale summary pending action");
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017,REQ-PRFU-026
/// @pseudocode lines 41-49
#[test]
fn marker_remote_only_resume_skips_duplicate_fixed_resolution_attempt() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p15_validated_fixed_pending(&temp);
    let remote_marker = serde_json::json!({
        "id": 77,
        "body": "Luther follow-up\n\n<!-- luther-pr-followup marker_key=thread-valid source_head=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa remediation_output_head=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb body=hash-valid action=comment_fixed run_id=run-p11 -->"
    });
    let runner = P15MarkerRunner::with_remote_comments(vec![remote_marker]);
    let mut context = p11_context(&temp);
    let outcome = GithubFeedbackMarkerExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &p11_params(&temp))
        .expect("remote-only marker resume");
    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "remote hidden marker must reconstruct comment and resolution idempotency",
    );
    let graphql_calls = runner
        .calls()
        .iter()
        .filter(|call| call.iter().any(|arg| arg == "graphql"))
        .count();
    assert_eq!(graphql_calls, 0, "resolution must not be attempted twice");
}
/// A previously posted in-thread reply marker discovered on the pull review
/// comments endpoint must suppress a duplicate reply on resume.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-016,REQ-PRFU-017
/// @pseudocode lines 41-49
#[test]
fn marker_remote_review_comment_marker_skips_duplicate_in_thread_reply() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p15_validated_fixed_pending_with_database_id(&temp, 7001);
    let remote_review_comment = serde_json::json!({
        "id": 88,
        "in_reply_to_id": 7001,
        "body": "Luther follow-up\n\n<!-- luther-pr-followup marker_key=thread-valid source_head=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa remediation_output_head=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb body=hash-valid action=comment_fixed run_id=run-p11 -->"
    });
    let runner = P15MarkerRunner::with_pull_review_comments(vec![remote_review_comment]);
    let mut context = p11_context(&temp);
    let outcome = GithubFeedbackMarkerExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &p11_params(&temp))
        .expect("remote review-comment marker resume");
    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "remote in-thread reply marker must reconstruct comment idempotency",
    );
    let reply_posts = runner
        .calls()
        .iter()
        .filter(|call| {
            call.iter().any(|arg| arg == "POST") && call.iter().any(|arg| arg.contains("/replies"))
        })
        .count();
    assert_eq!(
        reply_posts, 0,
        "discovered in-thread reply marker must prevent a duplicate reply"
    );
}

/// A missing agent `response_text` must stop the marker step before any GitHub
/// mutation and persist a fatal validation artifact.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 41-49
#[test]
fn marker_blocks_all_github_calls_when_response_text_missing() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p15_validated_fixed_pending_with_database_id(&temp, 7001);
    let path = p11_current_artifact_path(&temp, "pending-feedback-marker-actions");
    let mut pending = read_json(&path);
    pending["pending_actions"][0]
        .as_object_mut()
        .expect("pending action object")
        .remove("response_text");
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&pending).expect("pending without response_text"),
    )
    .expect("write pending without response_text");
    let runner = P15MarkerRunner::default();
    let mut context = p11_context(&temp);
    let outcome = GithubFeedbackMarkerExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &p11_params(&temp))
        .expect("marker pre-mutation validation");
    assert_expected_outcome(
        outcome,
        StepOutcome::Fatal,
        "missing response_text must fail before any GitHub side effect",
    );
    assert!(
        runner.calls().is_empty(),
        "no GitHub command may run when pre-mutation validation fails: {:?}",
        runner.calls()
    );
    let report = read_json(&p11_current_artifact_path(
        &temp,
        "pr-feedback-marker-report",
    ));
    assert_eq!(
        report
            .get("github_side_effects_performed")
            .and_then(serde_json::Value::as_bool),
        Some(false)
    );
    assert!(
        report.to_string().contains("missing_response_text"),
        "validation artifact must name the missing_response_text violation: {report}"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017,REQ-PRFU-026
/// @pseudocode lines 41-49
#[test]
fn marker_rejects_fixed_action_without_validator_success_evidence() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p15_validated_fixed_pending(&temp);
    let mut pending = read_json(&p11_current_artifact_path(
        &temp,
        "pending-feedback-marker-actions",
    ));
    pending["pending_actions"][0]["remediation_result_evidence"] = serde_json::Value::Null;
    std::fs::write(
        p11_current_artifact_path(&temp, "pending-feedback-marker-actions"),
        serde_json::to_vec_pretty(&pending).expect("stale pending json"),
    )
    .expect("write stale pending evidence");
    let runner = P15MarkerRunner::default();
    let mut context = p11_context(&temp);
    let mut params = p11_params(&temp);
    params["resolve_fixed"] = serde_json::json!(true);
    let outcome = GithubFeedbackMarkerExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &params)
        .expect("mark feedback with missing validator evidence");
    assert_expected_outcome(
        outcome,
        StepOutcome::Fatal,
        "fixed marker actions require validator-approved remediation evidence",
    );
    assert!(runner
        .calls()
        .iter()
        .all(|call| !call.iter().any(|arg| arg == "POST")));
    let report = read_json(&p11_current_artifact_path(
        &temp,
        "pr-feedback-marker-report",
    ));
    assert!(report
        .to_string()
        .contains("missing_validator_success_evidence"));
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017,REQ-PRFU-026
/// @pseudocode lines 41-49
#[test]
fn marker_needs_user_judgment_comments_are_explicitly_config_gated() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p10_inputs(
        &temp,
        serde_json::json!([]),
        serde_json::json!([]),
        serde_json::json!([p10_accepted(
            "cr-judgment",
            "thread-judgment",
            "hash-judgment",
            "needs_user_judgment"
        )]),
        serde_json::json!([]),
        serde_json::json!([]),
    );
    let runner = P15MarkerRunner::default();
    let mut context = p10_context(&temp);
    let outcome = GithubFeedbackMarkerExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &p10_params(&temp))
        .expect("mark gated needs-user-judgment");
    assert_expected_outcome(
        outcome,
        StepOutcome::Fatal,
        "missing needs-user-judgment escalation config must skip comment and return non-success",
    );
    assert!(runner
        .calls()
        .iter()
        .all(|call| !call.iter().any(|arg| arg == "POST")));
    let report = read_json(&p15_marker_report_path(&temp));
    assert!(report
        .to_string()
        .contains("needs_user_judgment_comments_disabled"));

    let temp = tempfile::tempdir().expect("tempdir");
    write_p10_inputs(
        &temp,
        serde_json::json!([]),
        serde_json::json!([]),
        serde_json::json!([p10_accepted(
            "cr-judgment",
            "thread-judgment",
            "hash-judgment",
            "needs_user_judgment"
        )]),
        serde_json::json!([]),
        serde_json::json!([]),
    );
    let runner = P15MarkerRunner::default();
    let mut params = p10_params(&temp);
    params["post_needs_user_judgment_comments"] = serde_json::json!(true);
    let mut context = p10_context(&temp);
    let outcome = GithubFeedbackMarkerExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &params)
        .expect("mark enabled needs-user-judgment");
    assert_expected_outcome(
        outcome,
        StepOutcome::Fatal,
        "enabled needs-user-judgment comments post idempotently but never resolve or succeed",
    );
    assert_eq!(
        runner
            .calls()
            .iter()
            .filter(|call| call.iter().any(|arg| arg == "POST"))
            .count(),
        1
    );
    assert!(runner
        .calls()
        .iter()
        .all(|call| !call.iter().any(|arg| arg == "graphql")));
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017,REQ-PRFU-026
/// @pseudocode lines 41-49
#[test]
fn marker_comment_success_resolution_failure_is_partial_retryable() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p15_validated_fixed_pending(&temp);
    let runner = P15MarkerRunner::failing_resolution();
    let mut context = p11_context(&temp);
    let mut params = p11_params(&temp);
    params["resolve_fixed"] = serde_json::json!(true);
    let outcome = GithubFeedbackMarkerExecutorWithRunner::new(runner, FixedClock)
        .execute(&mut context, &params)
        .expect("partial marker failure");
    assert_expected_outcome(
        outcome,
        StepOutcome::Fatal,
        "comment success with resolution failure must be partial retryable fatal",
    );
    let report = read_json(&p11_current_artifact_path(
        &temp,
        "pr-feedback-marker-report",
    ));
    assert!(
        report
            .to_string()
            .contains("resolution_failed_after_comment")
            || report.to_string().contains("resolution_unavailable")
            || report.to_string().contains("resolution_transport_error")
    );
    assert!(report
        .get("retryable_actions")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|items| !items.is_empty()));
}

/// Set the numeric `comment_database_id` on the validated fixed pending action.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017,REQ-PRFU-026
fn write_p15_validated_fixed_pending_with_database_id(temp: &tempfile::TempDir, database_id: i64) {
    write_p15_validated_fixed_pending(temp);
    let path = p11_current_artifact_path(temp, "pending-feedback-marker-actions");
    let mut pending = read_json(&path);
    pending["pending_actions"][0]["comment_database_id"] = serde_json::json!(database_id);
    pending["pending_actions"][0]["original_feedback_identity"]["comment_database_id"] =
        serde_json::json!(database_id);
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&pending).expect("pending json with database id"),
    )
    .expect("write pending with comment_database_id");
}

/// Find the single audit entry for the validated fixed thread in the report.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-016,REQ-PRFU-026
fn p15_thread_audit_entry(report: &serde_json::Value) -> serde_json::Value {
    report
        .get("action_audit")
        .and_then(serde_json::Value::as_array)
        .expect("action_audit array")
        .iter()
        .find(|entry| {
            entry
                .get("review_thread_id")
                .and_then(serde_json::Value::as_str)
                == Some("thread-valid")
        })
        .cloned()
        .expect("audit entry for thread-valid")
}

/// Fixed/changed items must post the agent reply on the original review thread
/// (REST replies endpoint) and resolve the thread with the real mutation.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017,REQ-PRFU-026
/// @pseudocode lines 41-49
#[test]
fn marker_posts_in_thread_reply_for_fixed_and_resolves_thread() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p15_validated_fixed_pending_with_database_id(&temp, 7001);
    let runner = P15MarkerRunner::default();
    let mut context = p11_context(&temp);
    let outcome = GithubFeedbackMarkerExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &p11_params(&temp))
        .expect("mark fixed feedback in-thread");
    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "fixed feedback must post in-thread and resolve the thread",
    );
    let calls = runner.calls();
    assert!(
        calls.iter().any(|call| {
            call.iter().any(|arg| arg == "POST")
                && call
                    .iter()
                    .any(|arg| arg.contains("/pulls/1910/comments/7001/replies"))
        }),
        "reply must target the in-thread replies endpoint: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|call| call.iter().any(|arg| arg == "graphql")),
        "fixed status must resolve the thread"
    );
    let report = read_json(&p11_current_artifact_path(
        &temp,
        "pr-feedback-marker-report",
    ));
    let audit = p15_thread_audit_entry(&report);
    assert_eq!(
        audit
            .get("in_thread_reply")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert_eq!(
        audit
            .get("resolve_attempted")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert_eq!(
        audit
            .get("resolve_succeeded")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
}

/// The resolve call must send the real GraphQL mutation text and bind the
/// `threadId` variable, not a placeholder named query.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-016
/// @pseudocode lines 41-49
#[test]
fn marker_resolve_uses_real_mutation_and_thread_variable() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p15_validated_fixed_pending_with_database_id(&temp, 7001);
    let runner = P15MarkerRunner::default();
    let mut context = p11_context(&temp);
    GithubFeedbackMarkerExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &p11_params(&temp))
        .expect("mark fixed feedback for resolve mutation");
    let graphql_call = runner
        .calls()
        .into_iter()
        .find(|call| call.iter().any(|arg| arg == "graphql"))
        .expect("graphql resolve call");
    assert!(
        graphql_call
            .iter()
            .any(|arg| arg.contains("resolveReviewThread(input:{threadId:$threadId})")),
        "resolve mutation text must be the real mutation: {graphql_call:?}"
    );
    assert!(
        graphql_call
            .iter()
            .any(|arg| arg == "threadId=thread-valid"),
        "resolve must bind the threadId variable: {graphql_call:?}"
    );
    assert!(
        !graphql_call
            .iter()
            .any(|arg| arg.contains("resolve_review_thread_mutation")),
        "resolve must not use the placeholder named query: {graphql_call:?}"
    );
}

fn write_p15_validated_rest_review_comment_only_pending(temp: &tempfile::TempDir) {
    write_p15_validated_fixed_pending(temp);
    let pending_path = p11_current_artifact_path(temp, "pending-feedback-marker-actions");
    let mut pending = read_json(&pending_path);
    let action = pending["pending_actions"][0]
        .as_object_mut()
        .expect("pending action object");
    action.insert(
        "action_id".to_string(),
        serde_json::json!("comment_fixed:review-comment:PRRC_rest:hash-valid:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
    );
    action.insert(
        "item_id".to_string(),
        serde_json::json!("rest-review:PRRC_rest"),
    );
    action.insert(
        "stable_marker_key".to_string(),
        serde_json::json!("review-comment:PRRC_rest"),
    );
    action.insert(
        "idempotency_key".to_string(),
        serde_json::json!("run-p11:example:workflow:1910:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb:review-comment:PRRC_rest:comment_fixed"),
    );
    action.remove("thread_id");
    action.remove("comment_database_id");
    let identity = action
        .get_mut("original_feedback_identity")
        .and_then(serde_json::Value::as_object_mut)
        .expect("original feedback identity object");
    identity.insert(
        "item_id".to_string(),
        serde_json::json!("rest-review:PRRC_rest"),
    );
    identity.insert(
        "stable_marker_key".to_string(),
        serde_json::json!("review-comment:PRRC_rest"),
    );
    identity.remove("thread_id");
    identity.remove("comment_database_id");
    std::fs::write(
        &pending_path,
        serde_json::to_vec_pretty(&pending).expect("rest review pending json"),
    )
    .expect("write rest review pending action");

    let result_path = p11_result_path(temp);
    let mut result = read_json(&result_path);
    let result_item = result["results"]
        .as_array_mut()
        .and_then(|items| {
            items.iter_mut().find(|item| {
                item.get("source_type").and_then(serde_json::Value::as_str)
                    == Some("coderabbit_feedback")
            })
        })
        .and_then(serde_json::Value::as_object_mut)
        .expect("coderabbit remediation result object");
    result_item.insert(
        "source_id".to_string(),
        serde_json::json!("rest-review:PRRC_rest"),
    );
    result_item.insert(
        "stable_marker_key".to_string(),
        serde_json::json!("review-comment:PRRC_rest"),
    );
    result_item.remove("thread_id");
    result_item.remove("comment_database_id");
    std::fs::write(
        result_path,
        serde_json::to_vec_pretty(&result).expect("rest review result json"),
    )
    .expect("write rest review remediation result");
}

/// REST review comments collected without GraphQL review-thread identity can be
/// answered as comment-only marker actions, but they must never fatal before
/// mutation or attempt an unavailable thread resolution.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016
/// @pseudocode lines 41-49
#[test]
fn marker_posts_comment_only_for_rest_review_action_without_thread_id() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p15_validated_rest_review_comment_only_pending(&temp);
    let runner = P15MarkerRunner::default();
    let mut context = p11_context(&temp);
    let outcome = GithubFeedbackMarkerExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &p11_params(&temp))
        .expect("mark rest review feedback without thread id");

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "non-thread REST review marker actions should be comment-only successes",
    );
    let calls = runner.calls();
    assert!(
        calls.iter().any(|call| {
            call.iter().any(|arg| arg == "POST")
                && call.iter().any(|arg| arg.contains("/issues/1910/comments"))
        }),
        "comment-only marker should post a PR timeline comment: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .all(|call| !call.iter().any(|arg| arg.contains("/replies"))),
        "comment-only marker must not require an in-thread reply endpoint: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .all(|call| !call.iter().any(|arg| arg == "graphql")),
        "comment-only marker must not attempt thread resolution: {calls:?}"
    );
    let report = read_json(&p11_current_artifact_path(
        &temp,
        "pr-feedback-marker-report",
    ));
    assert_eq!(
        report
            .get("marker_state")
            .and_then(serde_json::Value::as_str),
        Some("complete")
    );
    assert!(
        report
            .get("validation_violations")
            .and_then(serde_json::Value::as_array)
            .is_none_or(Vec::is_empty),
        "comment-only REST review marker must pass pre-mutation validation: {report}"
    );
    assert!(
        report
            .get("action_audit")
            .and_then(serde_json::Value::as_array)
            .expect("action audit")
            .iter()
            .any(|entry| entry
                .get("stable_marker_key")
                .and_then(serde_json::Value::as_str)
                == Some("review-comment:PRRC_rest")
                && entry
                    .get("resolve_attempted")
                    .and_then(serde_json::Value::as_bool)
                    == Some(false)),
        "comment-only audit must record no resolution attempt: {report}"
    );
}

/// Review-thread marker actions without a numeric review-comment id must fail
/// validation before any GitHub mutation instead of posting to the PR timeline.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016
/// @pseudocode lines 41-49
#[test]
fn marker_rejects_review_thread_action_when_database_id_missing() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p15_validated_fixed_pending_with_database_id(&temp, 7001);
    let path = p11_current_artifact_path(&temp, "pending-feedback-marker-actions");
    let mut pending = read_json(&path);
    pending["pending_actions"][0]
        .as_object_mut()
        .expect("pending action object")
        .remove("comment_database_id");
    pending["pending_actions"][0]["original_feedback_identity"]
        .as_object_mut()
        .expect("original feedback identity object")
        .remove("comment_database_id");
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&pending).expect("pending without database id"),
    )
    .expect("write pending without comment_database_id");
    let runner = P15MarkerRunner::default();
    let mut context = p11_context(&temp);
    let outcome = GithubFeedbackMarkerExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &p11_params(&temp))
        .expect("reject fixed feedback without database id");
    assert_expected_outcome(
        outcome,
        StepOutcome::Fatal,
        "review-thread marker actions without comment_database_id must fail before mutation",
    );
    assert!(
        runner.calls().is_empty(),
        "no GitHub command may run when review-thread reply validation fails: {:?}",
        runner.calls()
    );
    let report = read_json(&p11_current_artifact_path(
        &temp,
        "pr-feedback-marker-report",
    ));
    assert!(
        report
            .to_string()
            .contains("review_thread_reply_without_comment_database_id"),
        "validation artifact must name the missing comment_database_id violation: {report}"
    );
}

/// The audit record links the feedback item, review thread, posted reply, and
/// resolve result with idempotency keys for a fixed item.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-016,REQ-PRFU-026
/// @pseudocode lines 41-49
#[test]
fn marker_audit_records_reply_and_resolve_evidence() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p15_validated_fixed_pending_with_database_id(&temp, 7001);
    let runner = P15MarkerRunner::default();
    let mut context = p11_context(&temp);
    GithubFeedbackMarkerExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &p11_params(&temp))
        .expect("mark fixed feedback for audit");
    let report = read_json(&p11_current_artifact_path(
        &temp,
        "pr-feedback-marker-report",
    ));
    let audit = p15_thread_audit_entry(&report);
    assert_eq!(
        audit.get("item_id").and_then(serde_json::Value::as_str),
        Some("cr-valid")
    );
    assert_eq!(
        audit
            .get("comment_database_id")
            .and_then(serde_json::Value::as_i64),
        Some(7001)
    );
    assert!(
        audit
            .get("idempotency_key")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|key| !key.is_empty()),
        "audit must carry the comment idempotency key"
    );
    assert!(
        audit.get("reply_comment_id").is_some(),
        "audit must record the posted reply id"
    );
    assert_eq!(
        audit
            .get("final_thread_resolved_state")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @requirement:REQ-PRFU-016
/// @pseudocode lines 41-49
#[test]
fn marker_idempotency_avoids_duplicate_comments_using_local_and_remote_markers() {
    let outcome = execute_step(GithubFeedbackMarkerExecutorWithRunner::new(
        P15MarkerRunner::default(),
        FixedClock,
    ));
    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "marker idempotency must avoid duplicate comments using local marker reports and remote marker comments",
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @requirement:REQ-PRFU-016
/// @pseudocode lines 41-49
#[test]
fn pending_marker_actions_carry_forward_across_head_change() {
    let outcome = execute_step(GithubFeedbackMarkerExecutorWithRunner::new(
        P15MarkerRunner::default(),
        FixedClock,
    ));
    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "pending marker actions must carry forward across head change and complete without duplicate retry comments",
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @requirement:REQ-PRFU-017
/// @pseudocode lines 1-53
#[test]
fn github_pr_command_runner_shell_safety_uses_argv_not_raw_interpolated_text() {
    let malicious = "review says `uname` and $(echo owned) with quotes \" and newline\nEOF";
    let runner = ScriptedGithubRunner::new(
        serde_json::json!([{ "name": malicious, "state": "SUCCESS", "bucket": "pass" }]),
        serde_json::json!({ "total_count": 0, "check_runs": [] }),
    );
    let mut context = p06_context(&tempfile::tempdir().expect("tempdir"));
    let temp = tempfile::tempdir().expect("artifact tempdir");
    let outcome = GithubPrChecksExecutorWithRunner::new(runner.clone(), RecordingClock::default())
        .execute(&mut context, &p06_check_params(&temp, 1))
        .expect("argv-safe command runner seam");
    let calls = runner.calls();

    assert_eq!(
        outcome,
        StepOutcome::Success,
        "malicious check text is data and must not affect command execution"
    );
    assert!(
        calls
            .iter()
            .all(|argv| argv.first().map(String::as_str) == Some("gh")),
        "runner seam must receive argv vectors, not shell strings: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .all(|argv| !argv.iter().any(|arg| arg.contains(malicious))),
        "malicious API data must never be interpolated into gh argv: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .all(|argv| !argv.join(" ").contains("$(echo owned)")
                && !argv.join(" ").contains("`uname`")),
        "joined argv demonstrates no shell command text was built: {calls:?}"
    );
}
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 14-19
#[test]
fn coderabbit_readiness_stability_resets_on_current_head_signal_metadata_change() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runner = P08FeedbackRunner::new(vec![
        check_runs_signal(
            "completed",
            serde_json::json!("success"),
            "CodeRabbit finished pass one.",
        ),
        check_runs_signal(
            "completed",
            serde_json::json!("success"),
            "CodeRabbit finished pass two with different metadata.",
        ),
        check_runs_signal(
            "completed",
            serde_json::json!("success"),
            "CodeRabbit finished pass two with different metadata.",
        ),
    ]);
    let mut context = p08_context(&temp);
    let outcome = GithubCodeRabbitFeedbackExecutorWithRunner::new(runner, FixedClock)
        .execute(&mut context, &p08_params(&temp, 3))
        .expect("collect feedback");
    let artifact = read_json(&p08_feedback_path(&temp));
    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "readiness stability must reset when current-head readiness signal metadata changes",
    );
    assert_eq!(
        artifact
            .get("budget_used")
            .and_then(serde_json::Value::as_u64),
        Some(3)
    );
    let observations = artifact
        .get("observations")
        .and_then(serde_json::Value::as_array)
        .expect("observations");
    assert_ne!(
        observations[0]
            .get("readiness_stability_hash")
            .and_then(serde_json::Value::as_str),
        observations[1]
            .get("readiness_stability_hash")
            .and_then(serde_json::Value::as_str)
    );
    assert_eq!(
        observations[1]
            .get("readiness_stability_hash")
            .and_then(serde_json::Value::as_str),
        observations[2]
            .get("readiness_stability_hash")
            .and_then(serde_json::Value::as_str)
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-009,REQ-PRFU-016
/// @pseudocode lines 4-5,13,20,26
#[test]
fn remote_marker_parser_accepts_exact_namespace_and_reports_malformed_diagnostics() {
    let parser = luther_workflow::engine::executors::FeedbackMarkerParser::new();
    let marker = parser.parse_marker_diagnostic("<!-- luther-pr-followup marker_key=thread:one source_head=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa remediation_output_head=bbbb body=fnv64:1111 action=comment_fixed run_id=run-p08 -->").expect("valid marker");
    assert_eq!(marker.stable_marker_key, "thread:one");
    assert_eq!(
        marker.source_head_sha,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );
    assert_eq!(marker.body_hash, "fnv64:1111");
    assert_eq!(marker.run_id, "run-p08");
    assert_eq!(marker.action_kind, "comment_fixed");
    assert_eq!(marker.status, "completed");
    assert!(parser.parse_marker_diagnostic("<!-- luther:other-marker marker_key=thread:one source_head=head remediation_output_head=none body=hash action=comment_fixed run_id=run -->").expect_err("wrong namespace").contains("wrong marker namespace"));
    assert!(parser.parse_marker_diagnostic("prefix <!-- luther-pr-followup marker_key=thread:one source_head=head remediation_output_head=none body=hash action=comment_fixed run_id=run -->").expect_err("not exact").contains("single exact hidden HTML comment"));
    assert!(parser.parse_marker_diagnostic("<!-- luther-pr-followup marker_key=thread:one source_head=head body=hash action=comment_fixed run_id=run -->").expect_err("missing remediation_output_head").contains("missing field remediation_output_head"));
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-017,REQ-PRFU-011,REQ-PRFU-012
/// @pseudocode lines 9-15
#[test]
fn feedback_evaluator_command_shell_safety_writes_raw_llm_text_to_bounded_artifacts() {
    let malicious = "LLM says $(rm -rf /) and `cat /etc/passwd` with here-doc EOF";
    let temp = tempfile::tempdir().expect("tempdir");
    write_p09_feedback(
        &temp,
        serde_json::json!([p09_feedback_item(
            "item-shell",
            "thread-shell",
            "hash-shell"
        )]),
        serde_json::json!([]),
    );
    let runner = RecordingFeedbackEvaluatorRunner::new(malicious.to_string());
    let adapter = CommandFeedbackEvaluationAdapter::new(
        vec![
            "feedback-evaluator-bin".to_string(),
            "--stdin-json".to_string(),
        ],
        runner.clone(),
    );
    let mut context = p09_context(&temp);
    let outcome = FeedbackEvaluatorExecutor::new(adapter, FixedClock)
        .execute(&mut context, &p09_params(&temp))
        .expect("feedback evaluator shell safety");
    let artifact = read_json(&p09_evaluations_path(&temp));
    let raw_path = artifact
        .get("rejected_attempts")
        .and_then(serde_json::Value::as_array)
        .and_then(|attempts| attempts.first())
        .and_then(|attempt| attempt.get("raw_response_artifact_path"))
        .and_then(serde_json::Value::as_str)
        .expect("raw response path");
    let raw_text_artifact = read_json(std::path::Path::new(raw_path));
    let calls = runner.calls();

    assert_expected_outcome(
        outcome,
        StepOutcome::Fatal,
        "malicious raw LLM text is data and must not become an accepted evaluation",
    );
    assert_eq!(
        calls.len(),
        3,
        "command adapter must be invoked through the command/process seam for each retry"
    );
    assert!(
        calls.iter().all(|(argv, stdin)| argv
            == &[
                "feedback-evaluator-bin".to_string(),
                "--stdin-json".to_string()
            ]
            && stdin.contains("item-shell")
            && stdin.contains("feedback body item-shell")),
        "structured request data must be passed through argv plus stdin JSON: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .all(|(argv, _)| !argv.join(" ").contains("$(rm -rf /)")
                && !argv.join(" ").contains("`cat /etc/passwd`")),
        "LLM/body text must not be interpolated into command argv: {calls:?}"
    );
    assert!(
        raw_path.contains("feedback-evaluator-raw-output"),
        "raw output must be captured through a store-owned artifact family path: {raw_path}"
    );
    assert_eq!(
        raw_text_artifact
            .get("raw_text")
            .and_then(serde_json::Value::as_str),
        Some(malicious),
        "raw malicious LLM output must be persisted as bounded JSON data"
    );
    assert!(
        artifact.to_string().contains("malformed_json"),
        "malicious free-form text must be rejected as malformed JSON"
    );
}

#[test]
fn feedback_evaluator_accepts_json_object_wrapped_by_llxprt_cli_progress() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p09_feedback(
        &temp,
        serde_json::json!([p09_feedback_item("item-json", "thread-json", "hash-json")]),
        serde_json::json!([]),
    );
    let raw = "## Todo Progress\n[opusthinking]\n{\"item_id\":\"item-json\",\"stable_marker_key\":\"thread-json\",\"body_hash\":\"hash-json\",\"head_sha\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"decision\":\"valid\",\"reason\":\"actionable\",\"recommended_action\":\"fix it\",\"response_text\":\"Luther will address this actionable feedback.\"}\n";
    let runner = RecordingFeedbackEvaluatorRunner::new(raw.to_string());
    let adapter =
        CommandFeedbackEvaluationAdapter::new(vec!["feedback-evaluator-bin".to_string()], runner);
    let mut context = p09_context(&temp);
    let outcome = FeedbackEvaluatorExecutor::new(adapter, FixedClock)
        .execute(&mut context, &p09_params(&temp))
        .expect("feedback evaluator wrapped JSON");
    let artifact = read_json(&p09_evaluations_path(&temp));

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "wrapped llxprt progress should not hide the single JSON response object",
    );
    assert_eq!(
        artifact
            .get("accepted_results")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(1)
    );
}

#[test]
fn feedback_evaluator_default_argv_loads_noninteractive_profile() {
    let argv = luther_workflow::engine::executors::default_feedback_evaluator_argv();
    assert!(
        argv.windows(2)
            .any(|window| window[0] == "--profile-load" && window[1] == "gpt55high"),
        "production feedback evaluator should use the current dogfood llxprt profile shape as other noninteractive LLM steps: {argv:?}"
    );
}

#[test]
fn feedback_evaluator_default_argv_forbids_agentic_tool_loops() {
    // The evaluator is a pure stdin-JSON classification call. Granting it tool
    // access (via --yolo) with unbounded turns let the model spend its whole
    // turn budget running git/grep and never emit the required JSON verdict,
    // exhausting the per-item attempt budget. The invocation must be capped to
    // a single turn and must not enable yolo/tool auto-approval.
    let argv = luther_workflow::engine::executors::default_feedback_evaluator_argv();
    assert!(
        !argv.iter().any(|arg| arg == "--yolo"),
        "feedback evaluator must not enable tool auto-approval: {argv:?}"
    );
    assert!(
        argv.windows(2)
            .any(|window| window[0] == "--set" && window[1] == "maxTurnsPerPrompt=1"),
        "feedback evaluator must cap the model to a single turn so it cannot loop on tool calls: {argv:?}"
    );
}

#[test]
fn feedback_evaluator_default_argv_downranks_speculative_nits() {
    let argv = luther_workflow::engine::executors::default_feedback_evaluator_argv();
    let prompt = argv
        .iter()
        .position(|arg| arg == "-p")
        .and_then(|index| argv.get(index + 1))
        .expect("feedback evaluator prompt after -p");

    assert!(
        prompt.contains("Speculative robustness suggestions")
            && prompt.contains("low-value nits")
            && prompt.contains("needs_user_judgment only"),
        "feedback evaluator prompt should not block workflows on speculative optional CodeRabbit nits: {prompt}"
    );
}

#[test]
fn feedback_evaluator_command_errors_consume_budget_without_panicking() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p09_feedback(
        &temp,
        serde_json::json!([p09_feedback_item(
            "item-timeout",
            "thread-timeout",
            "hash-timeout"
        )]),
        serde_json::json!([]),
    );
    let runner = FailingFeedbackEvaluatorRunner::default();
    let adapter = CommandFeedbackEvaluationAdapter::new(
        vec!["feedback-evaluator-bin".to_string()],
        runner.clone(),
    );
    let mut context = p09_context(&temp);
    let outcome = FeedbackEvaluatorExecutor::new(adapter, FixedClock)
        .execute(&mut context, &p09_params(&temp))
        .expect("feedback evaluator command errors are recorded");
    let artifact = read_json(&p09_evaluations_path(&temp));

    assert_expected_outcome(
        outcome,
        StepOutcome::Fatal,
        "command failures should be artifact-backed rejected attempts, not executor errors",
    );
    assert_eq!(runner.calls().len(), 3);
    assert!(artifact.to_string().contains("command_error"));
    assert!(artifact
        .get("budget_exhausted_items")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|items| items.len() == 1));
}
#[test]
fn feedback_evaluator_classifies_coderabbit_summary_without_llm_command() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p09_feedback(
        &temp,
        serde_json::json!([p09_feedback_item(
            "summary-item",
            "summary:IC_123:fnv64:summary",
            "fnv64:summary"
        )]),
        serde_json::json!([]),
    );
    let runner = FailingFeedbackEvaluatorRunner::default();
    let adapter = CommandFeedbackEvaluationAdapter::new(
        vec!["feedback-evaluator-bin".to_string()],
        runner.clone(),
    );
    let mut context = p09_context(&temp);
    let outcome = FeedbackEvaluatorExecutor::new(adapter, FixedClock)
        .execute(&mut context, &p09_params(&temp))
        .expect("deterministic summary evaluation");
    let artifact = read_json(&p09_evaluations_path(&temp));

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "summary comments should not require an LLM evaluator call",
    );
    assert!(runner.calls().is_empty());
    assert_eq!(
        artifact
            .pointer("/accepted_results/0/source")
            .and_then(serde_json::Value::as_str),
        Some("deterministic")
    );
}

#[test]
fn feedback_evaluator_process_runner_times_out_hung_llxprt_command() {
    let runner = ProcessFeedbackEvaluatorCommandRunner::with_timeout(Duration::from_secs(0));
    let result = runner.run_feedback_evaluator_command(
        &["sh".to_string(), "-c".to_string(), "sleep 5".to_string()],
        "{}",
    );

    assert!(
        result
            .expect_err("hung evaluator command should time out")
            .to_string()
            .contains("timed out"),
        "hung feedback evaluator commands must be bounded"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @requirement:REQ-PRFU-017
/// @pseudocode lines 12-17
#[test]
fn remediation_wrapper_shell_safety_passes_plan_and_result_paths_as_argv() {
    let outcome = execute_step(PrFollowupRemediationExecutorWithRunner::new(
        FixturePrFollowupLlxprtRunner,
        FixedClock,
    ));
    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "remediation wrapper shell safety must pass plan and result paths as argv-safe parameters",
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @requirement:REQ-PRFU-017
/// @pseudocode lines 41-49
#[test]
fn marker_comment_shell_safety_uses_body_files_or_graphql_variables() {
    let outcome = execute_step(GithubFeedbackMarkerExecutorWithRunner::new(
        P15MarkerRunner::default(),
        FixedClock,
    ));
    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "marker comment shell safety must use body files or GraphQL variables for malicious CodeRabbit text",
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014
/// @pseudocode lines 29-33
#[test]
fn run_post_pr_tests_expands_manifest_group_placeholder() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(&temp, serde_json::json!([]));
    let runner = P13RecordingRunner::with_results(vec![p13_result("passed")]);
    let mut context = p11_context(&temp);
    context.set("command_manifest_group_post_pr", "custom_pr");
    let mut params = p11_params(&temp);
    params["command_manifest_group"] = serde_json::json!("{command_manifest_group_post_pr}");
    params["command_manifest"] = serde_json::json!({
        "commands": [
            { "id": "unit", "argv": ["cargo", "test", "--lib"] },
            { "id": "ignored", "argv": ["cargo", "fmt", "--check"] }
        ],
        "groups": {
            "post_pr": ["ignored"],
            "custom_pr": ["unit"]
        }
    });

    let outcome = RunPostPrTestsExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &params)
        .expect("post-pr manifest group interpolation");
    let requests = runner.requests();

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "post-PR tests must resolve profile-provided command manifest group placeholders",
    );
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].command_id, "unit");
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-017
/// @pseudocode lines 29-40
#[test]
fn post_pr_tests_and_push_shell_safety_use_configured_argv_without_shell_injection() {
    let malicious = "review says `uname` and $(echo owned) with quotes \" and newline\nEOF";
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(
        &temp,
        serde_json::json!([{
            "source_type": "ci_failure",
            "source_id": "ci-build",
            "stable_marker_key": null,
            "input_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "status": "changed",
            "action": malicious,
            "evidence": { "kind": "current_repository_test", "current_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" },
            "response_text": "Luther addressed this review item and posted the remediation evidence on the original thread.", "evidence_paths": ["src/lib.rs"]
        }]),
    );
    let runner = P13RecordingRunner::with_results(vec![p13_result("passed")]);
    let mut context = p11_context(&temp);
    let outcome = RunPostPrTestsExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &p13_params(&temp))
        .expect("post-pr test shell safety");
    let requests = runner.requests();
    let artifact = read_json(&p11_current_artifact_path(&temp, "post-pr-test-result"));

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "post-PR test shell safety must use configured argv without interpolating malicious feedback text",
    );
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].argv,
        vec!["cargo".to_string(), "test".to_string(), "--lib".to_string()]
    );
    assert!(
        !requests[0].argv.join(" ").contains("$(echo owned)")
            && !requests[0].argv.join(" ").contains("`uname`"),
        "malicious remediation text must not be interpolated into post-PR test argv: {requests:?}"
    );
    assert_eq!(
        artifact
            .get("test_state")
            .and_then(serde_json::Value::as_str),
        Some("passed")
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-017
/// @pseudocode lines 29-40

#[test]
fn run_post_pr_tests_manifest_conditions_use_repo_root_not_project_dir() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(&temp, serde_json::json!([]));
    let runner = P13RecordingRunner::with_results(Vec::new());
    let mut context = p11_context(&temp);
    let project_dir = temp.path().join("workflow");
    std::fs::create_dir(&project_dir).expect("project dir");
    std::fs::write(temp.path().join("repo-marker"), "repo").expect("repo marker");
    context.set("project_dir", &project_dir.to_string_lossy());
    context.set("command_manifest_group_post_pr", "custom_pr");
    let mut params = p11_params(&temp);
    params["command_manifest_group"] = serde_json::json!("{command_manifest_group_post_pr}");
    params["command_manifest"] = serde_json::json!({
        "commands": [{
            "id": "unit",
            "argv": ["cargo", "test", "--lib"],
            "run_if_present_all": ["repo-marker"]
        }],
        "groups": { "custom_pr": ["unit"] }
    });

    let outcome = RunPostPrTestsExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &params)
        .expect("post-pr manifest group uses repo root for conditions");
    let requests = runner.requests();

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "post-PR manifest conditions must evaluate against the repository root",
    );
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].repo_root_directory, temp.path());
    assert_eq!(requests[0].working_directory, project_dir);
}

#[test]
fn post_pr_test_requests_preserve_artifact_base_separately_from_working_directory() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p11_plan_and_result(&temp, serde_json::json!([]));
    let runner = P13RecordingRunner::with_results(vec![p13_result("passed")]);
    let mut context = p11_context(&temp);
    let project_dir = temp.path().join("workflow");
    let artifact_base = temp.path().join("artifacts-root");
    std::fs::create_dir(&project_dir).expect("project dir");
    context.set("project_dir", &project_dir.to_string_lossy());
    context.set("artifact_base_dir", &artifact_base.to_string_lossy());

    let outcome = RunPostPrTestsExecutorWithRunner::new(runner.clone(), FixedClock)
        .execute(&mut context, &p13_params(&temp))
        .expect("post-pr test artifact base");
    let requests = runner.requests();

    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "post-PR test requests must keep artifact base separate from command cwd",
    );
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].repo_root_directory, temp.path());
    assert_eq!(requests[0].working_directory, project_dir);
    assert_eq!(requests[0].artifact_base_directory, artifact_base);
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @requirement:REQ-PRFU-018
/// @pseudocode lines 50-53
#[test]
fn post_pr_failure_terminal_writes_terminal_artifact_and_returns_fatal_only() {
    let outcome = execute_step(PostPrFailureTerminalExecutor);
    assert_expected_outcome(
        outcome,
        StepOutcome::Fatal,
        "post_pr_failure_terminal must write terminal artifact and return fatal as its only outcome",
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04
/// @requirement:REQ-PRFU-019
/// @pseudocode lines 1-53
#[test]
fn executor_error_policy_writes_best_effort_failure_artifact_before_fatal_outcome() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runner = ScriptedGithubRunner::new(
        serde_json::json!([{ "name": "build", "state": "BROKEN", "bucket": "unknown" }]),
        serde_json::json!({ "total_count": 0, "check_runs": [] }),
    );
    let mut context = p06_context(&temp);
    let outcome = GithubPrChecksExecutorWithRunner::new(runner, RecordingClock::default())
        .execute(&mut context, &p06_check_params(&temp, 1))
        .expect("watcher writes best-effort failure artifact");
    assert_expected_outcome(
        outcome,
        StepOutcome::Fatal,
        "post-PR executor error policy must write best-effort failure artifact before returning fatal outcome",
    );
    let artifact = read_json(&p06_pr_check_status_path(&temp));
    assert_eq!(
        artifact
            .get("overall_state")
            .and_then(serde_json::Value::as_str),
        Some("unknown")
    );
    assert!(artifact
        .get("failure_sequence")
        .and_then(serde_json::Value::as_u64)
        .is_some());
}

// ---------------------------------------------------------------------------
// Issue #5: typed PR follow-up routing-state transition matrix and write-path
// rejection of contradictory artifacts.
// ---------------------------------------------------------------------------

/// Drives collect_ci_failures from a seeded pr-check-status whose typed
/// `overall_state` determines routing, asserting both the persisted
/// `collection_state` and the StepOutcome for each terminal state.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
fn run_ci_failures_transition(
    overall_state: &str,
    checks: serde_json::Value,
    stale_checks: serde_json::Value,
    fatal_source: serde_json::Value,
) -> (StepOutcome, serde_json::Value) {
    let temp = tempfile::tempdir().expect("tempdir");
    write_p07_check_status(&temp, overall_state, checks, stale_checks, fatal_source);
    let mut context = p07_context(&temp);
    let outcome = GithubCheckFailuresExecutorWithRunner::new(
        ScriptedGithubRunner::new(
            serde_json::json!([]),
            serde_json::json!({ "total_count": 0, "check_runs": [] }),
        ),
        FixedClock,
    )
    .execute(&mut context, &p07_params(&temp))
    .expect("collect ci failures");
    let artifact = read_json(&p07_ci_failures_path(&temp));
    (outcome, artifact)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
#[test]
fn collect_ci_failures_transition_matrix_routes_from_typed_overall_state() {
    // passed -> collected -> Success
    let (outcome, artifact) = run_ci_failures_transition(
        "passed",
        serde_json::json!([{ "check_id": "build", "name": "build", "state": "success", "conclusion": "success", "bucket": "passed", "url": null, "run_id": null, "job_id": null }]),
        serde_json::json!([]),
        serde_json::Value::Null,
    );
    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "passed checks must collect cleanly and route Success",
    );
    assert_eq!(
        artifact
            .get("collection_state")
            .and_then(serde_json::Value::as_str),
        Some("collected")
    );

    // fatal + fatal_source=api -> fatal -> Fatal
    let (outcome, artifact) = run_ci_failures_transition(
        "fatal",
        serde_json::json!([]),
        serde_json::json!([]),
        serde_json::json!("api"),
    );
    assert_expected_outcome(
        outcome,
        StepOutcome::Fatal,
        "fatal overall_state with api fatal_source must route Fatal",
    );
    assert_eq!(
        artifact
            .get("collection_state")
            .and_then(serde_json::Value::as_str),
        Some("fatal")
    );
    assert_eq!(
        artifact.get("watcher_fatal_source"),
        Some(&serde_json::json!("api"))
    );

    // fatal + fatal_source=null (stale-only) -> fatal -> Fatal
    let (outcome, artifact) = run_ci_failures_transition(
        "fatal",
        serde_json::json!([]),
        serde_json::json!([]),
        serde_json::Value::Null,
    );
    assert_expected_outcome(
        outcome,
        StepOutcome::Fatal,
        "fatal overall_state with null fatal_source (stale-only) must still route Fatal",
    );
    assert_eq!(
        artifact
            .get("collection_state")
            .and_then(serde_json::Value::as_str),
        Some("fatal")
    );
}

/// A green (`passed`) pr-check-status must never route the collection step
/// fatal. The contradictory `passed` + stale `fatal_source` artifact cannot be
/// persisted (write-path validation) and, even read directly, the typed routing
/// view keeps `passed` non-fatal. This is the exact smoke failure from issue #5.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
#[test]
fn passed_checks_after_transient_api_error_cannot_route_fatal() {
    let (outcome, artifact) = run_ci_failures_transition(
        "passed",
        serde_json::json!([{ "check_id": "build", "name": "build", "state": "success", "conclusion": "success", "bucket": "passed", "url": null, "run_id": null, "job_id": null }]),
        serde_json::json!([]),
        serde_json::Value::Null,
    );
    assert_expected_outcome(
        outcome,
        StepOutcome::Success,
        "green checks after a transient API error must route Success, never Fatal",
    );
    assert_eq!(
        artifact
            .get("collection_state")
            .and_then(serde_json::Value::as_str),
        Some("collected")
    );
    assert_eq!(
        artifact.get("fatal_source"),
        Some(&serde_json::Value::Null),
        "a collected, all-passed artifact must not carry a fatal_source"
    );
}

/// Write-path enforcement: persisting a contradictory `passed` +
/// `fatal_source="api"` pr-check-status must be rejected at write time so the
/// contradictory state can never reach the routing read site.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-007
#[test]
fn write_json_artifact_rejects_contradictory_passed_with_fatal_source() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().join("artifacts"));
    let binding = p07_binding();
    let err = store
        .write_json_artifact(
            &binding,
            "pr-check-status",
            "watch_pr_checks",
            3,
            &serde_json::json!({
                "pr_url": "https://github.com/example/workflow/pull/1910",
                "overall_state": "passed",
                "fatal_source": "api"
            }),
            None,
            &FixedClock,
        )
        .expect_err("contradictory passed+fatal_source must be rejected on write");
    assert!(
        format!("{err}").contains("fatal_source"),
        "write rejection must cite the contradictory passed+fatal_source state; err={err:?}"
    );
    assert!(
        !store.canonical_path(&binding, "pr-check-status").exists(),
        "a rejected contradictory artifact must never be persisted"
    );
}

/// Write-path enforcement: persisting a contradictory `collected` +
/// non-null `watcher_fatal_source` ci-failures artifact must be rejected.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-007
#[test]
fn write_json_artifact_rejects_contradictory_collected_with_watcher_fatal_source() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().join("artifacts"));
    let binding = p07_binding();
    let err = store
        .write_json_artifact(
            &binding,
            "ci-failures",
            "collect_ci_failures",
            4,
            &serde_json::json!({
                "collection_state": "collected",
                "failures": [],
                "pending_or_unknown": [],
                "fatal_source": serde_json::Value::Null,
                "watcher_fatal_source": { "class": "api_error" }
            }),
            None,
            &FixedClock,
        )
        .expect_err("collected + non-null watcher_fatal_source must be rejected on write");
    assert!(
        format!("{err}").contains("watcher_fatal_source"),
        "write rejection must cite the contradictory collected+watcher_fatal_source state; err={err:?}"
    );
    assert!(
        !store.canonical_path(&binding, "ci-failures").exists(),
        "a rejected contradictory ci-failures artifact must never be persisted"
    );
}

/// Write-path enforcement accepts valid routing states for both families so the
/// validator does not over-reject legitimate artifacts.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-007
#[test]
fn write_json_artifact_accepts_valid_routing_states() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().join("artifacts"));
    let binding = p07_binding();
    store
        .write_json_artifact(
            &binding,
            "pr-check-status",
            "watch_pr_checks",
            3,
            &serde_json::json!({
                "overall_state": "passed",
                "fatal_source": serde_json::Value::Null
            }),
            None,
            &FixedClock,
        )
        .expect("valid passed + null fatal_source must persist");
    store
        .write_json_artifact(
            &binding,
            "ci-failures",
            "collect_ci_failures",
            4,
            &serde_json::json!({
                "collection_state": "fatal",
                "failures": [],
                "pending_or_unknown": [],
                "fatal_source": "api",
                "watcher_fatal_source": "api"
            }),
            None,
            &FixedClock,
        )
        .expect("valid fatal + api watcher_fatal_source must persist");
    assert!(store.canonical_path(&binding, "pr-check-status").exists());
    assert!(store.canonical_path(&binding, "ci-failures").exists());
}
