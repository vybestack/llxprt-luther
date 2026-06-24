//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P18
//! @requirement:REQ-PRFU-020,REQ-PRFU-024,REQ-PRFU-034
//! Deterministic replay coverage for the full post-PR follow-up tail.
//!
//! These tests drive the real `EngineRunner` across the production
//! `llxprt-issue-fix-v1` workflow tail (capture_pr_identity -> ... ->
//! log_completion / post_pr_failure_terminal). Every external seam (GitHub,
//! the feedback-evaluation LLM, post-PR test commands, git push commands) is
//! replaced with a deterministic in-memory scripted runner plus a no-sleep
//! clock, so the entire state machine — classification, artifact emission,
//! `fatal_source` propagation, and loop-back routing — is exercised without
//! any network, `gh`, or `git` access.
//!
//! The goal (issue #8) is that live smoke becomes a confirmation rather than
//! the first place new state-machine bugs surface.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

use luther_workflow::engine::executor::ExecutorRegistry;
use luther_workflow::engine::executors::ClockSleeper;
use luther_workflow::engine::executors::{
    FeedbackEvaluationAdapter, FeedbackEvaluationRequest, FeedbackEvaluatorExecutor,
    GithubCheckFailuresExecutorWithRunner, GithubCodeRabbitFeedbackExecutorWithRunner,
    GithubFeedbackMarkerExecutorWithRunner, GithubPrChecksExecutorWithRunner,
    GithubPrCommandRunner, GithubPrIdentityExecutorWithRunner, LlxprtInvocationRequest,
    LlxprtInvocationResult, NoOpExecutor, PostPrFailureTerminalExecutor,
    PostPrIterationGuardExecutor, PostPrTestCommandRequest, PostPrTestCommandResult,
    PostPrTestCommandRunner, PrFollowupLlxprtCommandRunner,
    PrFollowupRemediationExecutorWithRunner, PrRemediationPlanExecutor,
    PrRemediationResultExecutor, PushRemediationChangesExecutorWithRunner,
    PushRemediationCommandRequest, PushRemediationCommandResult, PushRemediationCommandRunner,
    RunPostPrTestsExecutorWithRunner, ShellExecutor,
};
use luther_workflow::engine::instance::WorkflowInstance;
use luther_workflow::engine::runner::{EngineError, EngineRunner, RunOutcome};
use luther_workflow::workflow::config_loader::{resolve_workflow_config, resolve_workflow_type};
use luther_workflow::workflow::schema::{WorkflowConfig, WorkflowType};

use serde_json::{json, Value};
use tempfile::TempDir;

// ============================================================================
// Deterministic constants shared across scenarios.
// ============================================================================

const HEAD_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const NEXT_HEAD_SHA: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const BASE_SHA: &str = "cccccccccccccccccccccccccccccccccccccccc";
const REPO_OWNER: &str = "example";
const REPO_NAME: &str = "workflow";
const PR_NUMBER: &str = "1910";
const ISSUE_NUMBER: &str = "1234";

/// The 13 post-PR `step_type`s the replay registry must cover.
const POST_PR_STEP_TYPES: [&str; 13] = [
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

// ============================================================================
// No-sleep clock: deterministic timestamps, no real sleeping.
// ============================================================================

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P18
/// @requirement:REQ-PRFU-020
#[derive(Clone, Debug, Default)]
struct NoSleepClock {
    ticks: Arc<Mutex<u64>>,
}

impl ClockSleeper for NoSleepClock {
    fn now_rfc3339(&self) -> String {
        let mut ticks = self.ticks.lock().expect("clock ticks");
        let hours = *ticks / 3600;
        let minutes = (*ticks % 3600) / 60;
        let seconds = *ticks % 60;
        let stamp = format!("2026-04-30T{hours:02}:{minutes:02}:{seconds:02}Z");
        *ticks += 1;
        stamp
    }

    fn sleep(&self, _duration: std::time::Duration) {}
}

// ============================================================================
// Scenario fixtures: in-memory payloads driving every scripted seam.
// ============================================================================

/// A single scripted GitHub-checks observation. The runner pops one of these
/// per `gh pr checks` poll so successive polls can change state (e.g. pending
/// -> passed, or failed -> passed across a remediation loop-back).
#[derive(Clone, Debug)]
struct CheckObservation {
    /// Raw `gh pr checks --json ...` array payload for this poll.
    gh_pr_checks: Value,
    /// REST `commits/{sha}/check-runs` payload for this poll.
    rest_check_runs: Value,
}

impl CheckObservation {
    fn all_passed() -> Self {
        Self {
            gh_pr_checks: json!([
                { "name": "build", "state": "SUCCESS", "bucket": "pass" },
                { "name": "test", "state": "SUCCESS", "bucket": "pass" }
            ]),
            rest_check_runs: json!({ "total_count": 0, "check_runs": [] }),
        }
    }

    fn one_failed() -> Self {
        Self {
            gh_pr_checks: json!([
                { "name": "build", "state": "FAILURE", "bucket": "fail",
                  "link": "https://github.com/example/workflow/actions/runs/9001" },
                { "name": "test", "state": "SUCCESS", "bucket": "pass" }
            ]),
            rest_check_runs: json!({ "total_count": 0, "check_runs": [] }),
        }
    }

    fn pending() -> Self {
        Self {
            gh_pr_checks: json!([
                { "name": "build", "state": "IN_PROGRESS", "bucket": "pending" }
            ]),
            rest_check_runs: json!({ "total_count": 0, "check_runs": [] }),
        }
    }

    /// Current head has no checks but stale checks exist on a different head:
    /// classifies as terminal "fatal" with a non-null fatal_source.
    fn stale_only_fatal() -> Self {
        Self {
            gh_pr_checks: json!([]),
            rest_check_runs: json!({
                "total_count": 1,
                "check_runs": [{
                    "id": 7001,
                    "name": "build",
                    "status": "completed",
                    "conclusion": "success",
                    "head_sha": "0000000000000000000000000000000000000000"
                }]
            }),
        }
    }
}

/// Everything a single replay scenario needs to feed every scripted seam.
#[derive(Clone, Debug)]
struct ScenarioFixtures {
    /// Ordered queue of check observations. The last is repeated if exhausted.
    check_observations: Vec<CheckObservation>,
    /// CodeRabbit review-thread graphql node pages.
    review_thread_pages: Vec<Value>,
    /// REST pull review comment pages.
    review_comment_pages: Vec<Value>,
    /// REST issue comment pages.
    issue_comment_pages: Vec<Value>,
    /// Readiness `check-runs` observations (one popped per readiness poll).
    readiness_pages: Vec<Value>,
    /// Ordered feedback-evaluation decisions (one popped per item evaluation).
    feedback_decisions: Vec<String>,
}

impl ScenarioFixtures {
    /// A clean PR: CodeRabbit summary present (ready, no actionable threads),
    /// no review threads, no decisions required.
    fn ready_no_actionable() -> Self {
        Self {
            check_observations: vec![CheckObservation::all_passed()],
            review_thread_pages: vec![empty_review_threads()],
            review_comment_pages: vec![json!([])],
            issue_comment_pages: vec![json!([coderabbit_summary_comment()])],
            readiness_pages: vec![coderabbit_ready_readiness(), coderabbit_ready_readiness()],
            feedback_decisions: Vec::new(),
        }
    }
}

// ============================================================================
// Canned GitHub payload builders.
// ============================================================================

fn pr_identity_view() -> Value {
    json!({
        "number": PR_NUMBER.parse::<u64>().unwrap(),
        "url": "https://github.com/example/workflow/pull/1910",
        "headRefName": format!("luther/issue-{ISSUE_NUMBER}"),
        "headRefOid": HEAD_SHA,
        "baseRefName": "main",
        "baseRefOid": BASE_SHA,
        "state": "OPEN",
        "isDraft": false,
        "id": "PR_kwDOExample"
    })
}

fn empty_review_threads() -> Value {
    json!({
        "data": { "repository": { "pullRequest": { "reviewThreads": {
            "nodes": [],
            "pageInfo": { "hasNextPage": false }
        } } } }
    })
}

/// An issue comment from CodeRabbit that the executor treats as a non-actionable
/// readiness summary (drives `ready_signal = true`).
fn coderabbit_summary_comment() -> Value {
    json!({
        "id": 5000,
        "user": { "login": "coderabbitai[bot]" },
        "author": { "login": "coderabbitai[bot]" },
        "body": "Summary by CodeRabbit\n\nCodeRabbit finished reviewing this pull request.",
        "html_url": "https://github.com/example/workflow/pull/1910#issuecomment-5000",
        "created_at": "2026-04-30T00:00:00Z",
        "updated_at": "2026-04-30T00:00:00Z"
    })
}

/// Readiness check-run payload signalling a completed CodeRabbit review on the
/// current head.
fn coderabbit_ready_readiness() -> Value {
    json!({
        "total_count": 1,
        "check_runs": [{
            "id": 6001,
            "name": "CodeRabbit",
            "status": "completed",
            "conclusion": "success",
            "head_sha": HEAD_SHA,
            "app": { "slug": "coderabbitai[bot]" },
            "output": { "summary": "CodeRabbit review completed" }
        }]
    })
}

// ============================================================================
// Scripted GitHub command runner.
// ============================================================================

/// Routes `gh`/`gh api` argv across the three different consumers in the tail:
/// PR identity (`gh pr view`), check watching (`gh pr checks` + REST
/// check-runs), CodeRabbit feedback (graphql review threads + REST comments +
/// readiness check-runs), and the feedback marker (issue comment POSTs +
/// graphql resolutions). Records every call for assertions.
#[derive(Clone)]
struct ReplayGithubRunner {
    pr_view: Value,
    checks: Arc<Mutex<VecDeque<CheckObservation>>>,
    last_check: CheckObservation,
    review_thread_pages: Vec<Value>,
    review_comment_pages: Vec<Value>,
    issue_comment_pages: Vec<Value>,
    readiness: Arc<Mutex<VecDeque<Value>>>,
    last_readiness: Value,
    calls: Arc<Mutex<Vec<Vec<String>>>>,
}

impl ReplayGithubRunner {
    fn new(scenario: &ScenarioFixtures) -> Self {
        let last_check = scenario
            .check_observations
            .last()
            .cloned()
            .unwrap_or_else(CheckObservation::all_passed);
        let last_readiness = scenario
            .readiness_pages
            .last()
            .cloned()
            .unwrap_or_else(|| json!({ "total_count": 0, "check_runs": [] }));
        Self {
            pr_view: pr_identity_view(),
            checks: Arc::new(Mutex::new(
                scenario.check_observations.iter().cloned().collect(),
            )),
            last_check,
            review_thread_pages: if scenario.review_thread_pages.is_empty() {
                vec![empty_review_threads()]
            } else {
                scenario.review_thread_pages.clone()
            },
            review_comment_pages: if scenario.review_comment_pages.is_empty() {
                vec![json!([])]
            } else {
                scenario.review_comment_pages.clone()
            },
            issue_comment_pages: if scenario.issue_comment_pages.is_empty() {
                vec![json!([])]
            } else {
                scenario.issue_comment_pages.clone()
            },
            readiness: Arc::new(Mutex::new(
                scenario.readiness_pages.iter().cloned().collect(),
            )),
            last_readiness,
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn count_calls(&self, predicate: impl Fn(&[String]) -> bool) -> usize {
        self.calls
            .lock()
            .expect("github calls")
            .iter()
            .filter(|call| predicate(call))
            .count()
    }

    fn pop_check(&self) -> CheckObservation {
        self.checks
            .lock()
            .expect("checks queue")
            .pop_front()
            .unwrap_or_else(|| self.last_check.clone())
    }

    fn pop_readiness(&self) -> Value {
        self.readiness
            .lock()
            .expect("readiness queue")
            .pop_front()
            .unwrap_or_else(|| self.last_readiness.clone())
    }
}

impl GithubPrCommandRunner for ReplayGithubRunner {
    fn run_github_command(&self, argv: &[String]) -> Result<String, EngineError> {
        self.calls.lock().expect("github calls").push(argv.to_vec());
        let has = |needle: &str| argv.iter().any(|arg| arg.contains(needle));
        let is = |needle: &str| argv.iter().any(|arg| arg == needle);

        if is("view") {
            return Ok(self.pr_view.to_string());
        }
        if is("checks") {
            return Ok(self.pop_check().gh_pr_checks.to_string());
        }
        if has("graphql") {
            // CodeRabbit review-thread query OR a marker resolution mutation.
            if argv
                .iter()
                .any(|arg| arg.starts_with("query=") && arg.contains("reviewThreads"))
            {
                // Guard: page cursor must be a string endCursor, never numeric.
                if let Some(page) = argv.iter().find_map(|arg| arg.strip_prefix("page=")) {
                    assert!(
                        page.parse::<u64>().is_err(),
                        "graphql page cursor must be a string, got numeric {page}"
                    );
                }
                let page = self
                    .calls
                    .lock()
                    .expect("github calls")
                    .iter()
                    .filter(|call| {
                        call.iter()
                            .any(|arg| arg.starts_with("query=") && arg.contains("reviewThreads"))
                    })
                    .count();
                let value = self
                    .review_thread_pages
                    .get(page.saturating_sub(1))
                    .or_else(|| self.review_thread_pages.last())
                    .cloned()
                    .unwrap_or_else(empty_review_threads);
                return Ok(value.to_string());
            }
            // Marker resolution mutation.
            return Ok(
                json!({ "data": { "resolveReviewThread": { "thread": { "id": "thread-x" } } } })
                    .to_string(),
            );
        }
        if has("/pulls/") && has("/comments") {
            return Ok(self.review_comment_pages[0].to_string());
        }
        if has("/issues/") && has("/comments") {
            if is("POST") || is("-X") || has("--method") {
                return Ok(json!({
                    "id": 9001,
                    "html_url": "https://github.com/example/workflow/pull/1910#issuecomment-9001"
                })
                .to_string());
            }
            let page = argv
                .iter()
                .find_map(|arg| {
                    arg.rsplit_once("page=")
                        .and_then(|(_, v)| v.parse::<usize>().ok())
                })
                .unwrap_or(1);
            let value = self
                .issue_comment_pages
                .get(page.saturating_sub(1))
                .or_else(|| self.issue_comment_pages.last())
                .cloned()
                .unwrap_or_else(|| json!([]));
            return Ok(value.to_string());
        }
        if has("check-runs") {
            // Distinguish the readiness poll (per_page=100&page=1, from the
            // feedback executor) from the checks-watch REST poll.
            if has("per_page=100") {
                return Ok(self.pop_readiness().to_string());
            }
            return Ok(self.pop_check().rest_check_runs.to_string());
        }
        if has("actions/jobs") && has("logs") {
            return Ok("scripted ci failure log line\nanother line\n".to_string());
        }
        // Any other actions/* metadata call: empty object is acceptable.
        Ok(json!({}).to_string())
    }
}

// ============================================================================
// Scripted feedback-evaluation adapter.
// ============================================================================

/// Returns one validated decision per evaluated item, echoing identity fields.
#[derive(Clone)]
struct ReplayFeedbackAdapter {
    decisions: Arc<Mutex<VecDeque<String>>>,
    requests: Arc<Mutex<Vec<FeedbackEvaluationRequest>>>,
}

impl ReplayFeedbackAdapter {
    fn new(decisions: Vec<String>) -> Self {
        Self {
            decisions: Arc::new(Mutex::new(decisions.into_iter().collect())),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl ReplayFeedbackAdapter {
    fn request_count(&self) -> usize {
        self.requests.lock().expect("decisions requests").len()
    }
}

impl FeedbackEvaluationAdapter for ReplayFeedbackAdapter {
    fn evaluate(&self, request: &FeedbackEvaluationRequest) -> Result<String, EngineError> {
        self.requests
            .lock()
            .expect("decisions requests")
            .push(request.clone());
        let decision = self
            .decisions
            .lock()
            .expect("decisions")
            .pop_front()
            .unwrap_or_else(|| "valid".to_string());
        let response = json!({
            "item_id": request.item_id,
            "stable_marker_key": request.stable_marker_key,
            "body_hash": request.body_hash,
            "head_sha": request.head_sha,
            "decision": decision,
            "reason": "scripted deterministic replay decision",
            "recommended_action": "scripted_action"
        });
        Ok(response.to_string())
    }
}

// ============================================================================
// Scripted remediation / test / push runners.
// ============================================================================

/// Writes the deterministic `pr-remediation-result.json` the validator expects.
/// `valid_success` controls whether the scripted result is well-formed and all
/// statuses are successful (drives validate -> Success vs Fatal).
#[derive(Clone)]
struct ReplayLlxprtRunner;

impl PrFollowupLlxprtCommandRunner for ReplayLlxprtRunner {
    fn invoke(&self, request: LlxprtInvocationRequest) -> LlxprtInvocationResult {
        // Read the plan to mirror its must_fix items into the result.
        let plan: Value = std::fs::read_to_string(&request.remediation_plan_path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_else(|| json!({}));
        let plan_sequence = plan
            .get("artifact_sequence")
            .cloned()
            .unwrap_or(Value::Null);
        let results: Vec<Value> = plan
            .get("must_fix")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|item| {
                let source_type = item
                    .get("source_type")
                    .and_then(Value::as_str)
                    .unwrap_or("ci_failure")
                    .to_string();
                let source_id = item
                    .get("source_id")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                let mut entry = json!({
                    "source_type": source_type,
                    "source_id": source_id,
                    "stable_marker_key": item.get("stable_marker_key").cloned().unwrap_or(Value::Null),
                    "input_head_sha": HEAD_SHA,
                    "output_head_sha": NEXT_HEAD_SHA,
                    "status": "fixed",
                    "action": "scripted remediation",
                    "evidence": { "kind": "current_repository_test", "current_head_sha": NEXT_HEAD_SHA },
                    "evidence_paths": ["src/lib.rs"]
                });
                if let Some(key) = item.get("stable_marker_key").and_then(Value::as_str) {
                    entry["thread_id"] = json!(key);
                    entry["body_hash"] = item.get("body_hash").cloned().unwrap_or(Value::Null);
                }
                entry
            })
            .collect();
        let result = json!({
            "input_head_sha": HEAD_SHA,
            "output_head_sha": NEXT_HEAD_SHA,
            "overall_status": "success",
            "plan_artifact_sequence": plan_sequence,
            "results": results,
            "verification_commands": []
        });
        std::fs::write(
            &request.remediation_result_path,
            serde_json::to_vec_pretty(&result).expect("serialize remediation result"),
        )
        .expect("write remediation result");
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
            success_file_present: true,
            success_file_size: None,
            result_file_present: true,
            result_file_size: None,
            result_file_path: Some(request.remediation_result_path.clone()),
            changed_paths: vec!["src/lib.rs".to_string()],
            spawn_error: None,
        }
    }
}

/// Post-PR test command runner: always reports passed.
#[derive(Clone)]
struct ReplayTestRunner;

impl PostPrTestCommandRunner for ReplayTestRunner {
    fn run(&self, request: PostPrTestCommandRequest) -> PostPrTestCommandResult {
        PostPrTestCommandResult {
            command_id: request.command_id,
            argv: request.argv,
            working_directory: request.working_directory,
            exit_code: Some(0),
            signal: None,
            status: "passed".to_string(),
            bounded_stdout: "scripted tests passed".to_string(),
            bounded_stderr: String::new(),
            stdout_log_path: Some(request.stdout_log_path),
            stderr_log_path: Some(request.stderr_log_path),
            spawn_error: None,
        }
    }
}

/// Push runner: models a worktree with one included change that is staged,
/// committed, and verified-pushed (drives push -> Success / loop-back).
#[derive(Clone)]
struct ReplayPushRunner {
    calls: Arc<Mutex<Vec<PushRemediationCommandRequest>>>,
}

impl ReplayPushRunner {
    fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl ReplayPushRunner {
    fn push_invocations(&self) -> usize {
        self.calls
            .lock()
            .expect("push calls")
            .iter()
            .filter(|call| call.command_id == "push")
            .count()
    }
}

impl PushRemediationCommandRunner for ReplayPushRunner {
    fn run(&self, request: PushRemediationCommandRequest) -> PushRemediationCommandResult {
        self.calls.lock().expect("push calls").push(request.clone());
        let stdout = match request.command_id.as_str() {
            // Local HEAD before commit == remote head; after commit advances.
            "local-head" => {
                let prior = self
                    .calls
                    .lock()
                    .expect("push calls count")
                    .iter()
                    .filter(|call| call.command_id == "local-head")
                    .count();
                if prior <= 1 {
                    HEAD_SHA.to_string()
                } else {
                    NEXT_HEAD_SHA.to_string()
                }
            }
            "remote-head" => {
                let prior = self
                    .calls
                    .lock()
                    .expect("push calls count")
                    .iter()
                    .filter(|call| call.command_id == "remote-head")
                    .count();
                if prior <= 1 {
                    format!("{HEAD_SHA}\trefs/heads/issue{ISSUE_NUMBER}\n")
                } else {
                    format!("{NEXT_HEAD_SHA}\trefs/heads/issue{ISSUE_NUMBER}\n")
                }
            }
            "status-porcelain" => " M src/lib.rs\0".to_string(),
            "commit" => String::new(),
            "push" => String::new(),
            _ => String::new(),
        };
        PushRemediationCommandResult {
            command_id: request.command_id,
            argv: request.argv,
            working_directory: request.working_directory,
            exit_code: Some(0),
            signal: None,
            status: "passed".to_string(),
            bounded_stdout: stdout,
            bounded_stderr: String::new(),
            stdout_log_path: Some(request.stdout_log_path),
            stderr_log_path: Some(request.stderr_log_path),
            spawn_error: None,
        }
    }
}

// ============================================================================
// Replay registry + tail harness.
// ============================================================================

struct ReplayHandles {
    github: ReplayGithubRunner,
    feedback: ReplayFeedbackAdapter,
    push: ReplayPushRunner,
}

/// Mirror of `ExecutorRegistry::with_defaults`, but wiring the scripted runners
/// and the `NoSleepClock` into every `*WithRunner` executor so the real
/// executor logic runs against deterministic data. Covers all 13 post-PR
/// `step_type`s plus `shell`/`noop` so the engine can reach the terminal
/// `log_completion` and `abandon_and_log` steps.
fn build_replay_registry(scenario: &ScenarioFixtures) -> (ExecutorRegistry, ReplayHandles) {
    let clock = NoSleepClock::default();
    let github = ReplayGithubRunner::new(scenario);
    let feedback = ReplayFeedbackAdapter::new(scenario.feedback_decisions.clone());
    let push = ReplayPushRunner::new();

    let mut registry = ExecutorRegistry::new();
    // Terminal/log steps.
    registry.register("shell", Box::new(ShellExecutor));
    registry.register("noop", Box::new(NoOpExecutor));

    registry.register(
        "github_pr_identity",
        Box::new(GithubPrIdentityExecutorWithRunner::new(
            github.clone(),
            clock.clone(),
        )),
    );
    registry.register(
        "post_pr_iteration_guard",
        Box::new(PostPrIterationGuardExecutor),
    );
    registry.register(
        "github_pr_checks",
        Box::new(GithubPrChecksExecutorWithRunner::new(
            github.clone(),
            clock.clone(),
        )),
    );
    registry.register(
        "github_check_failures",
        Box::new(GithubCheckFailuresExecutorWithRunner::new(
            github.clone(),
            clock.clone(),
        )),
    );
    registry.register(
        "github_coderabbit_feedback",
        Box::new(GithubCodeRabbitFeedbackExecutorWithRunner::new(
            github.clone(),
            clock.clone(),
        )),
    );
    registry.register(
        "feedback_evaluator",
        Box::new(FeedbackEvaluatorExecutor::new(
            feedback.clone(),
            clock.clone(),
        )),
    );
    registry.register("pr_remediation_plan", Box::new(PrRemediationPlanExecutor));
    registry.register(
        "pr_followup_remediation",
        Box::new(PrFollowupRemediationExecutorWithRunner::new(
            ReplayLlxprtRunner,
            clock.clone(),
        )),
    );
    registry.register(
        "pr_remediation_result",
        Box::new(PrRemediationResultExecutor),
    );
    registry.register(
        "run_post_pr_tests",
        Box::new(RunPostPrTestsExecutorWithRunner::new(
            ReplayTestRunner,
            clock.clone(),
        )),
    );
    registry.register(
        "push_remediation_changes",
        Box::new(PushRemediationChangesExecutorWithRunner::new(
            push.clone(),
            clock.clone(),
        )),
    );
    registry.register(
        "github_feedback_marker",
        Box::new(GithubFeedbackMarkerExecutorWithRunner::new(
            github.clone(),
            clock.clone(),
        )),
    );
    registry.register(
        "post_pr_failure_terminal",
        Box::new(PostPrFailureTerminalExecutor),
    );

    (
        registry,
        ReplayHandles {
            github,
            feedback,
            push,
        },
    )
}

/// Load the real workflow type + a config fixture whose `artifact_dir`/`work_dir`
/// point at per-test temp dirs and whose variables resolve every `{token}` the
/// post-PR steps interpolate.
fn load_tail_workflow(temp: &TempDir) -> (WorkflowType, WorkflowConfig) {
    let fixture_root = std::path::PathBuf::from("tests/fixtures");
    let workflow_type =
        resolve_workflow_type("llxprt-issue-fix-v1", &fixture_root).expect("resolve workflow type");
    let mut config =
        resolve_workflow_config("llxprt-code", &fixture_root).expect("resolve workflow config");

    let artifact_dir = temp.path().join("artifacts");
    let work_dir = temp.path().join("work");
    std::fs::create_dir_all(&artifact_dir).expect("create artifact dir");
    std::fs::create_dir_all(&work_dir).expect("create work dir");

    let vars = &mut config.variables;
    vars.insert(
        "artifact_dir".to_string(),
        artifact_dir.display().to_string(),
    );
    vars.insert("work_dir".to_string(), work_dir.display().to_string());
    vars.insert("repository_owner".to_string(), REPO_OWNER.to_string());
    vars.insert("repository_name".to_string(), REPO_NAME.to_string());
    vars.insert("pr_number".to_string(), PR_NUMBER.to_string());
    vars.insert("issue_number".to_string(), ISSUE_NUMBER.to_string());
    vars.insert("primary_issue_number".to_string(), ISSUE_NUMBER.to_string());
    vars.insert(
        "head_ref".to_string(),
        format!("luther/issue-{ISSUE_NUMBER}"),
    );
    vars.insert("head_sha".to_string(), HEAD_SHA.to_string());
    vars.insert("base_ref".to_string(), "main".to_string());
    vars.insert("base_sha".to_string(), BASE_SHA.to_string());
    vars.insert(
        "target_repo".to_string(),
        format!("{REPO_OWNER}/{REPO_NAME}"),
    );

    (workflow_type, config)
}

struct TailRun {
    outcome: RunOutcome,
    final_step: String,
    run_id: String,
    artifact_root: std::path::PathBuf,
    handles: ReplayHandles,
}

/// Advance a fresh instance to `capture_pr_identity` and run the tail to a
/// terminal state using the scripted replay registry.
fn run_tail(scenario: &ScenarioFixtures, temp: &TempDir) -> TailRun {
    let (workflow_type, config) = load_tail_workflow(temp);
    let (registry, handles) = build_replay_registry(scenario);

    let mut instance = WorkflowInstance::create(workflow_type, config);
    instance.transition_to("capture_pr_identity");

    let mut runner = EngineRunner::new(instance, registry).expect("create EngineRunner");
    let run_id = runner.run_id().to_string();
    let outcome = runner.run().expect("engine run");
    let final_step = runner.current_step().to_string();

    TailRun {
        outcome,
        final_step,
        run_id,
        artifact_root: temp.path().join("artifacts"),
        handles,
    }
}

// ============================================================================
// Assertion helpers.
// ============================================================================

fn current_artifact_path(run: &TailRun, family: &str) -> std::path::PathBuf {
    run.artifact_root
        .join("pr-followup")
        .join("current")
        .join(&run.run_id)
        .join(REPO_OWNER)
        .join(REPO_NAME)
        .join(PR_NUMBER)
        .join(format!("{family}.json"))
}

fn read_artifact(run: &TailRun, family: &str) -> Value {
    let path = current_artifact_path(run, family);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read artifact {family} at {}: {err}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|err| panic!("parse artifact {family}: {err}"))
}

fn assert_artifact_absent(run: &TailRun, family: &str) {
    let path = current_artifact_path(run, family);
    assert!(
        !path.exists(),
        "expected artifact {family} to be absent, found {}",
        path.display()
    );
}

fn assert_success_at_log_completion(run: &TailRun) {
    assert!(
        matches!(run.outcome, RunOutcome::Success),
        "expected RunOutcome::Success, got {:?}",
        run.outcome
    );
    assert_eq!(
        run.final_step, "log_completion",
        "expected the success path to terminate at log_completion"
    );
    assert_no_abandon(run);
}

fn assert_failure_terminal(run: &TailRun) {
    match &run.outcome {
        RunOutcome::Failure { step_id, .. } => {
            assert_eq!(
                step_id, "post_pr_failure_terminal",
                "failure must terminate at post_pr_failure_terminal"
            );
        }
        other => panic!("expected RunOutcome::Failure at terminal, got {other:?}"),
    }
}

/// Assert the run paused on a recoverable external wait at the given step
/// instead of routing to a terminal failure sink.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
fn assert_waiting_external_at(run: &TailRun, expected_step: &str) {
    match &run.outcome {
        RunOutcome::WaitingExternal { step_id, .. } => {
            assert_eq!(
                step_id, expected_step,
                "external wait must pause at the wait step"
            );
        }
        other => panic!("expected RunOutcome::WaitingExternal, got {other:?}"),
    }
}

fn assert_no_abandon(run: &TailRun) {
    assert!(
        !matches!(run.outcome, RunOutcome::Abandoned { .. }),
        "per-edge loop limits must not be exceeded: {:?}",
        run.outcome
    );
}

fn assert_check_status_consistent(run: &TailRun, expected_state: &str) {
    let status = read_artifact(run, "pr-check-status");
    assert_eq!(
        status.get("overall_state").and_then(Value::as_str),
        Some(expected_state),
        "pr-check-status overall_state mismatch"
    );
    // Invariant: a passed status must never carry a non-null fatal_source.
    if expected_state == "passed" {
        assert!(
            status
                .get("fatal_source")
                .map(Value::is_null)
                .unwrap_or(true),
            "passed check status must carry null fatal_source, got {:?}",
            status.get("fatal_source")
        );
    }
}

fn is_checks_poll(call: &[String]) -> bool {
    call.iter().any(|arg| arg == "checks")
}

// ============================================================================
// Harness-sanity tests.
// ============================================================================

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P18
/// @requirement:REQ-PRFU-020
#[test]
fn replay_registry_covers_all_post_pr_step_types() {
    let scenario = ScenarioFixtures::ready_no_actionable();
    let (registry, _handles) = build_replay_registry(&scenario);
    let registered = registry.registered_step_types();
    for step_type in POST_PR_STEP_TYPES {
        assert!(
            registry.contains_step_type(step_type),
            "replay registry missing post-PR step type {step_type}; registered: {registered:?}"
        );
    }
    // Terminal/log steps must also be reachable.
    assert!(registry.contains_step_type("shell"));
    assert!(registry.contains_step_type("noop"));
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P18
/// @requirement:REQ-PRFU-020
#[test]
fn replay_start_tail_begins_at_capture_pr_identity() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (workflow_type, config) = load_tail_workflow(&temp);
    let mut instance = WorkflowInstance::create(workflow_type, config);
    instance.transition_to("capture_pr_identity");
    assert_eq!(instance.current_state, "capture_pr_identity");

    // The artifact_dir override must point inside the temp dir, not the fixture
    // absolute path.
    let artifact_dir = instance
        .config
        .variables
        .get("artifact_dir")
        .expect("artifact_dir variable");
    assert!(
        Path::new(artifact_dir).starts_with(temp.path()),
        "artifact_dir override must live under the per-test temp dir, got {artifact_dir}"
    );
}

// ============================================================================
// Scenario 1: Green path.
// ============================================================================

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P18
/// @requirement:REQ-PRFU-020,REQ-PRFU-024
#[test]
fn replay_green_path_reaches_log_completion() {
    let temp = tempfile::tempdir().expect("tempdir");
    let scenario = ScenarioFixtures::ready_no_actionable();
    let run = run_tail(&scenario, &temp);

    assert_success_at_log_completion(&run);
    assert_check_status_consistent(&run, "passed");

    let feedback = read_artifact(&run, "coderabbit-feedback");
    assert_eq!(
        feedback.get("readiness_state").and_then(Value::as_str),
        Some("ready"),
        "green path must reach a ready CodeRabbit readiness state"
    );

    let plan = read_artifact(&run, "pr-remediation-plan");
    assert_eq!(
        plan.get("plan_state").and_then(Value::as_str),
        Some("clean"),
        "green path plan should be clean"
    );

    // No CI failures collected.
    let ci = read_artifact(&run, "ci-failures");
    assert_eq!(
        ci.get("collection_state").and_then(Value::as_str),
        Some("collected")
    );
}

// ============================================================================
// Scenario 2: Failed CI is fixed and loops back to success.
// ============================================================================

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P18
/// @requirement:REQ-PRFU-020,REQ-PRFU-024,REQ-PRFU-026
#[test]
fn replay_failed_ci_is_fixed_and_loops_to_success() {
    let temp = tempfile::tempdir().expect("tempdir");
    let scenario = ScenarioFixtures {
        // First poll: a real CI failure (Fixable). After the remediation loop
        // pushes and re-captures identity, the second poll is green.
        check_observations: vec![
            CheckObservation::one_failed(),
            CheckObservation::all_passed(),
        ],
        review_thread_pages: vec![empty_review_threads()],
        review_comment_pages: vec![json!([])],
        issue_comment_pages: vec![json!([coderabbit_summary_comment()])],
        readiness_pages: vec![coderabbit_ready_readiness(), coderabbit_ready_readiness()],
        feedback_decisions: Vec::new(),
    };
    let run = run_tail(&scenario, &temp);

    assert_success_at_log_completion(&run);
    assert_no_abandon(&run);

    // The watcher must have polled checks at least twice (failed then passed),
    // proving the outer push_remediation_changes -> capture_pr_identity loop ran.
    let check_polls = run.handles.github.count_calls(is_checks_poll);
    assert!(
        check_polls >= 2,
        "expected >=2 check polls across the remediation loop, got {check_polls}"
    );

    // The push artifact must show a verified push.
    let push = read_artifact(&run, "push-remediation-result");
    assert_eq!(
        push.get("push_state").and_then(Value::as_str),
        Some("pushed"),
        "remediation loop must end with a verified push"
    );
    assert!(
        run.handles.push.push_invocations() >= 1,
        "the remediation loop must invoke at least one git push"
    );
}

// ============================================================================
// Scenario 3: Pending checks time out to a recoverable external wait.
// ============================================================================

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P18
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
/// @requirement:REQ-PRFU-020,REQ-PRFU-024
#[test]
fn replay_pending_checks_time_out_to_recoverable_wait() {
    let temp = tempfile::tempdir().expect("tempdir");
    let scenario = ScenarioFixtures {
        // Always pending: the watcher exhausts max_attempts and classifies
        // pending_timeout. That is a recoverable external wait, so the run
        // pauses at watch_pr_checks instead of routing to the terminal sink.
        check_observations: vec![CheckObservation::pending()],
        review_thread_pages: vec![empty_review_threads()],
        review_comment_pages: vec![json!([])],
        issue_comment_pages: vec![json!([])],
        readiness_pages: vec![json!({ "total_count": 0, "check_runs": [] })],
        feedback_decisions: Vec::new(),
    };
    let run = run_tail(&scenario, &temp);

    assert_waiting_external_at(&run, "watch_pr_checks");
    assert_check_status_consistent(&run, "pending_timeout");
}

// ============================================================================
// Scenario 4: CodeRabbit fixable feedback resolved to success.
// ============================================================================

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P18
/// @requirement:REQ-PRFU-020,REQ-PRFU-024
#[test]
fn replay_coderabbit_fixable_feedback_resolved_to_success() {
    let temp = tempfile::tempdir().expect("tempdir");
    let scenario = ScenarioFixtures {
        check_observations: vec![CheckObservation::all_passed()],
        // First collection pass (two stable polls) sees an actionable thread.
        // After the remediation loop pushes the fix and the tail loops back, the
        // re-review reports the thread resolved (empty), mirroring how CodeRabbit
        // resolves a thread once the fix lands. That terminates the loop with a
        // clean plan -> mark -> log_completion.
        review_thread_pages: vec![
            coderabbit_actionable_threads(),
            coderabbit_actionable_threads(),
            empty_review_threads(),
        ],
        review_comment_pages: vec![json!([])],
        issue_comment_pages: vec![json!([coderabbit_summary_comment()])],
        readiness_pages: vec![coderabbit_ready_readiness(), coderabbit_ready_readiness()],
        feedback_decisions: vec!["valid".to_string()],
    };
    let run = run_tail(&scenario, &temp);

    assert_success_at_log_completion(&run);

    // The remediation result artifact proves the valid feedback drove a
    // remediation loop iteration before the thread was resolved.
    let result = read_artifact(&run, "pr-remediation-result");
    assert_eq!(
        result.get("overall_status").and_then(Value::as_str),
        Some("success"),
        "valid actionable feedback should drive a successful remediation result"
    );

    let marker = read_artifact(&run, "pr-feedback-marker-report");
    assert_eq!(
        marker.get("marker_state").and_then(Value::as_str),
        Some("complete"),
        "marker step must complete after resolving the thread"
    );
}

// ============================================================================
// Scenario 5: CodeRabbit invalid feedback rejected without remediation.
// ============================================================================

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P18
/// @requirement:REQ-PRFU-020,REQ-PRFU-024
#[test]
fn replay_coderabbit_invalid_feedback_rejected_without_remediation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let scenario = ScenarioFixtures {
        check_observations: vec![CheckObservation::all_passed()],
        review_thread_pages: vec![coderabbit_actionable_threads()],
        review_comment_pages: vec![json!([])],
        issue_comment_pages: vec![json!([coderabbit_summary_comment()])],
        readiness_pages: vec![coderabbit_ready_readiness(), coderabbit_ready_readiness()],
        feedback_decisions: vec!["invalid".to_string()],
    };
    let run = run_tail(&scenario, &temp);

    assert_success_at_log_completion(&run);

    let plan = read_artifact(&run, "pr-remediation-plan");
    assert_eq!(
        plan.get("plan_state").and_then(Value::as_str),
        Some("clean"),
        "invalid feedback must not produce must_fix items (clean plan)"
    );

    // No remediation result artifact: the remediation loop never ran.
    assert_artifact_absent(&run, "pr-remediation-result");

    // The invalid decision was still evaluated exactly once, and no push
    // occurred because no remediation loop ran.
    assert!(
        run.handles.feedback.request_count() >= 1,
        "the actionable thread must have been evaluated at least once"
    );
    assert_eq!(
        run.handles.push.push_invocations(),
        0,
        "rejecting invalid feedback must not push any changes"
    );
}

// ============================================================================
// Scenario 6: Terminal fatal propagates a non-null fatal_source.
// ============================================================================

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P18
/// @requirement:REQ-PRFU-020,REQ-PRFU-024,REQ-PRFU-034
#[test]
fn replay_terminal_fatal_propagates_fatal_source() {
    let temp = tempfile::tempdir().expect("tempdir");
    let scenario = ScenarioFixtures {
        // Stale-only checks on the current head => fatal classification with a
        // non-null fatal_source (the regression class this issue targets).
        check_observations: vec![CheckObservation::stale_only_fatal()],
        review_thread_pages: vec![empty_review_threads()],
        review_comment_pages: vec![json!([])],
        issue_comment_pages: vec![json!([])],
        readiness_pages: vec![json!({ "total_count": 0, "check_runs": [] })],
        feedback_decisions: Vec::new(),
    };
    let run = run_tail(&scenario, &temp);

    assert_failure_terminal(&run);

    let status = read_artifact(&run, "pr-check-status");
    assert_eq!(
        status.get("overall_state").and_then(Value::as_str),
        Some("fatal"),
        "stale-only checks must classify as fatal"
    );

    // The ci-failures artifact must carry the propagated (non-null) fatal_source
    // and must NOT report a contradictory passed state.
    let ci = read_artifact(&run, "ci-failures");
    assert_eq!(
        ci.get("collection_state").and_then(Value::as_str),
        Some("fatal"),
        "fatal check status must yield a fatal CI-failures collection"
    );
    assert_ne!(
        ci.get("collection_state").and_then(Value::as_str),
        Some("passed"),
        "fatal collection must never be labelled passed"
    );
}

// ============================================================================
// Additional canned payloads used by feedback scenarios.
// ============================================================================

/// One unresolved CodeRabbit review thread carrying an actionable comment.
fn coderabbit_actionable_threads() -> Value {
    json!({
        "data": { "repository": { "pullRequest": { "reviewThreads": {
            "nodes": [{
                "id": "thread-1",
                "isResolved": false,
                "isOutdated": false,
                "path": "src/lib.rs",
                "line": 10,
                "startLine": 10,
                "comments": { "nodes": [{
                    "id": "comment-1",
                    "body": "Please rename this variable for clarity.",
                    "author": { "login": "coderabbitai[bot]" },
                    "url": "https://github.com/example/workflow/pull/1910#discussion_r1",
                    "path": "src/lib.rs",
                    "line": 10,
                    "originalLine": 10,
                    "commit": { "oid": HEAD_SHA },
                    "createdAt": "2026-04-30T00:00:00Z",
                    "updatedAt": "2026-04-30T00:00:00Z"
                }] }
            }],
            "pageInfo": { "hasNextPage": false }
        } } } }
    })
}
