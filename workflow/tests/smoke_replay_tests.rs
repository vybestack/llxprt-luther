//! Deterministic smoke-trace replay tests (Luther issue #19).
//!
//! These tests load committed normalized smoke traces and replay the recorded
//! per-step outcomes through the real `EngineRunner`, re-deriving identical
//! engine routing **offline** (no network, no `gh`, no auth). This makes live
//! smoke failures deterministically reproducible.
//!
//! @plan:PLAN-LUTHER-ISSUE-19-SMOKE-REPLAY
//! @requirement:REQ-SMOKE-REPLAY-002

use std::sync::{Arc, Mutex};

use luther_workflow::engine::executor::{ExecutorRegistry, StepContext, StepExecutor};
use luther_workflow::engine::instance::WorkflowInstance;
use luther_workflow::engine::runner::{EngineError, EngineRunner, RunOutcome};
use luther_workflow::engine::transition::StepOutcome;
use luther_workflow::persistence::trace::{load_trace, SmokeTrace, TraceEvent};
use luther_workflow::workflow::config_loader::{resolve_workflow_config, resolve_workflow_type};

// ============================================================================
// TraceReplayExecutor — replays recorded per-step outcomes in order
// ============================================================================

/// Replays a trace's ordered `(step_id, outcome)` events through the engine.
/// On each `execute` it reads `current_step_id`, asserts it matches the next
/// recorded event's `step_id` (failing fast on sequence divergence), and
/// returns the recorded outcome parsed into a `StepOutcome`.
#[derive(Clone)]
struct TraceReplayExecutor {
    events: Arc<Vec<TraceEvent>>,
    cursor: Arc<Mutex<usize>>,
}

impl TraceReplayExecutor {
    fn new(events: Vec<TraceEvent>) -> Self {
        Self {
            events: Arc::new(events),
            cursor: Arc::new(Mutex::new(0)),
        }
    }
}

fn parse_outcome(s: &str) -> Result<StepOutcome, EngineError> {
    match s {
        "success" => Ok(StepOutcome::Success),
        "retryable" => Ok(StepOutcome::Retryable),
        "fatal" => Ok(StepOutcome::Fatal),
        "fixable" => Ok(StepOutcome::Fixable),
        "abandon" => Ok(StepOutcome::Abandon),
        other => Err(EngineError::InvalidState(format!(
            "unknown recorded outcome: {other}"
        ))),
    }
}

impl StepExecutor for TraceReplayExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        _params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        let step_id = context
            .get("current_step_id")
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());

        let mut cursor = self.cursor.lock().unwrap();
        let idx = *cursor;
        let event = self.events.get(idx).ok_or_else(|| {
            EngineError::InvalidState(format!(
                "replay ran past recorded trace at step '{step_id}' (recorded {} events)",
                self.events.len()
            ))
        })?;
        if event.step_id != step_id {
            return Err(EngineError::InvalidState(format!(
                "trace sequence divergence at seq {idx}: recorded '{}', engine routed to '{}'",
                event.step_id, step_id
            )));
        }
        *cursor += 1;
        parse_outcome(&event.outcome)
    }
}

// ============================================================================
// Harness helpers
// ============================================================================

/// All step types used by `llxprt-issue-fix-v1` (mirrors
/// `e2e_workflow_integration::setup_registry`).
const STEP_TYPES: &[&str] = &[
    "shell",
    "verify",
    "llxprt",
    "workflow_auth_preflight",
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

fn load_fixture(name: &str) -> SmokeTrace {
    let path = std::path::PathBuf::from("tests/fixtures/smoke-traces").join(name);
    load_trace(&path).unwrap_or_else(|e| panic!("failed to load fixture {name}: {e}"))
}

fn build_registry(trace: &SmokeTrace) -> ExecutorRegistry {
    let mut registry = ExecutorRegistry::new();
    let executor = TraceReplayExecutor::new(trace.events.clone());
    for step_type in STEP_TYPES {
        registry.register(step_type, Box::new(executor.clone()));
    }
    registry
}

fn run_replay(trace: &SmokeTrace) -> Result<RunOutcome, EngineError> {
    let fixture_root = std::path::PathBuf::from("tests/fixtures");
    let workflow_type = resolve_workflow_type(&trace.workflow_type_id, &fixture_root)
        .expect("Failed to load workflow type from trace");
    let config = resolve_workflow_config(&trace.config_id, &fixture_root)
        .expect("Failed to load workflow config from trace");
    let registry = build_registry(trace);
    let instance = WorkflowInstance::create(workflow_type, config);
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");
    runner.run()
}

// ============================================================================
// Tests
// ============================================================================

/// Replays the all-success happy path and asserts terminal `RunOutcome::Success`.
/// @requirement:REQ-SMOKE-REPLAY-002
#[test]
fn test_replay_success_trace() {
    let trace = load_fixture("success-select-and-fetch.json");
    assert!(trace
        .final_outcome
        .matches_run_outcome(&RunOutcome::Success));

    let result = run_replay(&trace).expect("replay should not error");
    assert!(
        matches!(result, RunOutcome::Success),
        "Expected Success, got {result:?}"
    );
    assert!(
        trace.final_outcome.matches_run_outcome(&result),
        "recorded final_outcome should match replayed outcome"
    );
}

/// Replays the `create_plan -> fatal -> abandon_and_log` route and asserts the
/// terminal outcome matches the recorded failure at `abandon_and_log`.
/// @requirement:REQ-SMOKE-REPLAY-002
#[test]
fn test_replay_failure_trace() {
    let trace = load_fixture("failure-abandon.json");

    let result = run_replay(&trace).expect("replay should not error");
    match &result {
        RunOutcome::Failure { step_id, .. } => {
            assert_eq!(step_id, "abandon_and_log", "Expected failure at terminal");
        }
        other => panic!("Expected Failure at abandon_and_log, got {other:?}"),
    }
    assert!(
        trace.final_outcome.matches_run_outcome(&result),
        "recorded final_outcome should match replayed outcome: {result:?}"
    );
}

/// Feeds a deliberately reordered trace and asserts the replay executor fails
/// fast on sequence divergence (guards against silent routing drift).
/// @requirement:REQ-SMOKE-REPLAY-002
#[test]
fn test_replay_detects_sequence_divergence() {
    let mut trace = load_fixture("success-select-and-fetch.json");
    // Swap the first two events so the recorded step_id no longer matches the
    // engine's actual starting step.
    trace.events.swap(0, 1);

    let result = run_replay(&trace);
    match result {
        Err(EngineError::StepExecutionError { message, .. }) => {
            assert!(
                message.contains("divergence"),
                "Expected divergence message, got: {message}"
            );
        }
        Err(EngineError::InvalidState(message)) => {
            assert!(
                message.contains("divergence"),
                "Expected divergence message, got: {message}"
            );
        }
        other => panic!("Expected divergence error, got {other:?}"),
    }
}
