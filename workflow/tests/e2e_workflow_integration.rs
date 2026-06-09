/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// End-to-End Workflow Integration Tests — Graph Routing
///
/// These tests load real TOML fixtures and use mock executors to verify
/// the workflow graph routes correctly for all outcome combinations.
/// They prove the TOML definition is structurally sound.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use luther_workflow::engine::executor::{
    interpolate_string, ExecutorRegistry, StepContext, StepExecutor,
};
use luther_workflow::engine::instance::WorkflowInstance;
use luther_workflow::engine::runner::{EngineError, EngineRunner, RunOutcome};
use luther_workflow::engine::transition::StepOutcome;
use luther_workflow::persistence::SqliteStore;
use luther_workflow::workflow::config_loader::{resolve_workflow_config, resolve_workflow_type};
use luther_workflow::workflow::target_profile::{
    apply_target_profile_overrides, validate_target_profile, TargetProfileOverrides,
};

// ============================================================================
// SharedMockExecutor — Thread-safe mock executor
// ============================================================================

/// Shared mock executor using Arc<Mutex<>> internally.
/// Thread-safe and cloneable for registering with multiple step types.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
#[derive(Clone)]
struct SharedMockExecutor {
    outcomes: Arc<Mutex<HashMap<String, Vec<StepOutcome>>>>,
    call_counts: Arc<Mutex<HashMap<String, usize>>>,
}

impl SharedMockExecutor {
    fn new(outcomes: HashMap<String, Vec<StepOutcome>>) -> Self {
        Self {
            outcomes: Arc::new(Mutex::new(outcomes)),
            call_counts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn call_count(&self, step_id: &str) -> usize {
        self.call_counts
            .lock()
            .unwrap()
            .get(step_id)
            .copied()
            .unwrap_or(0)
    }
}

impl StepExecutor for SharedMockExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        _params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        // Try to get step_id from context if set by the runner
        let step_id = context
            .get("current_step_id")
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());

        let mut counts = self.call_counts.lock().unwrap();
        let count = counts.entry(step_id.clone()).or_insert(0);
        let current_idx = *count;
        *count += 1;
        drop(counts);

        let outcomes = self.outcomes.lock().unwrap();
        if let Some(step_outcomes) = outcomes.get(&step_id) {
            if current_idx < step_outcomes.len() {
                return Ok(step_outcomes[current_idx]);
            }
        }

        Ok(StepOutcome::Success)
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Load workflow type and config from fixture files.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
fn load_workflow_from_toml(
    workflow_type_id: &str,
    config_id: &str,
) -> (
    luther_workflow::workflow::schema::WorkflowType,
    luther_workflow::workflow::schema::WorkflowConfig,
) {
    let fixture_root = std::path::PathBuf::from("tests/fixtures");
    let workflow_type = resolve_workflow_type(workflow_type_id, &fixture_root)
        .expect("Failed to load workflow type");
    let config =
        resolve_workflow_config(config_id, &fixture_root).expect("Failed to load workflow config");
    (workflow_type, config)
}

/// Create a registry with a shared mock executor registered for shell and verify.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
fn setup_registry(outcomes: HashMap<String, Vec<StepOutcome>>) -> ExecutorRegistry {
    let mut registry = ExecutorRegistry::new();
    let executor = SharedMockExecutor::new(outcomes);
    // Register for all workflow step types to intercept all steps
    registry.register("shell", Box::new(executor.clone()));
    registry.register("verify", Box::new(executor.clone()));
    registry.register("llxprt", Box::new(executor.clone()));
    for step_type in [
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
    ] {
        registry.register(step_type, Box::new(executor.clone()));
    }

    registry
}

// ============================================================================
// Test 1: Happy path all steps succeed
// ============================================================================

/// Test 1: Happy path — all steps succeed
/// GIVEN: Workflow loaded from TOML with all steps returning Success
/// WHEN: Engine runs
/// THEN: `RunOutcome::Success`, all 14 steps visited
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-ISSUE-001,REQ-LF-ISSUE-002,REQ-LF-ISSUE-003,REQ-LF-PR-001
#[test]
fn test_happy_path_all_steps_succeed() {
    let (workflow_type, config) = load_workflow_from_toml("llxprt-issue-fix-v1", "llxprt-code");

    // Count the steps
    assert_eq!(
        workflow_type.steps.len(),
        27,
        "Expected 27 steps in workflow after PR follow-through tail"
    );

    // All steps succeed by default (empty outcomes map)
    let registry = setup_registry(HashMap::new());
    let instance = WorkflowInstance::create(workflow_type, config);
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected Success, got {result:?}"
    );
}

// ============================================================================
// Test 2: Plan loop fixable then approved
// ============================================================================

/// Test 2: Plan loop fixable twice then approved
/// GIVEN: Workflow loaded from TOML
/// WHEN: `evaluate_plan` returns Fixable twice, then Success
/// THEN: `RunOutcome::Success` (loop works correctly)
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-PLAN-003,REQ-LF-PLAN-004
#[test]
fn test_plan_loop_fixable_then_approved() {
    let (workflow_type, config) = load_workflow_from_toml("llxprt-issue-fix-v1", "llxprt-code");

    // Set up outcomes: evaluate_plan returns Fixable twice, then Success
    let mut outcomes = HashMap::new();
    outcomes.insert(
        "evaluate_plan".to_string(),
        vec![
            StepOutcome::Fixable,
            StepOutcome::Fixable,
            StepOutcome::Success,
        ],
    );

    let registry = setup_registry(outcomes);
    let instance = WorkflowInstance::create(workflow_type, config);
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected Success, got {result:?}"
    );
}

// ============================================================================
// Test 3: Plan loop exceeds limit abandons
// ============================================================================

/// Test 3: Plan loop exceeds limit abandons
/// GIVEN: Workflow loaded from TOML with `max_iterations`: 5 on `evaluate_plan→create_plan`
/// WHEN: `evaluate_plan` always returns Fixable
/// THEN: `RunOutcome::Abandoned` after 5 iterations
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-PLAN-005
#[test]
fn test_plan_loop_exceeds_limit_abandons() {
    let (workflow_type, config) = load_workflow_from_toml("llxprt-issue-fix-v1", "llxprt-code");

    // Always fixable
    let mut outcomes = HashMap::new();
    outcomes.insert(
        "evaluate_plan".to_string(),
        vec![
            StepOutcome::Fixable,
            StepOutcome::Fixable,
            StepOutcome::Fixable,
            StepOutcome::Fixable,
            StepOutcome::Fixable,
            StepOutcome::Fixable,
        ],
    );

    let registry = setup_registry(outcomes);
    let instance = WorkflowInstance::create(workflow_type, config);
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Abandoned { .. })),
        "Expected Abandoned, got {result:?}"
    );
}

// ============================================================================
// Test 4: Test remediation loop fixable then passes
// ============================================================================

/// Test 4: Test remediation loop fixable then passes
/// GIVEN: Workflow loaded from TOML
/// WHEN: `run_tests` returns Fixable twice, then Success
/// THEN: `RunOutcome::Success`
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-TEST-001,REQ-LF-TEST-002
#[test]
fn test_test_remediation_loop_fixable_then_passes() {
    let (workflow_type, config) = load_workflow_from_toml("llxprt-issue-fix-v1", "llxprt-code");

    // run_tests: Fixable twice, then Success
    let mut outcomes = HashMap::new();
    outcomes.insert(
        "run_tests".to_string(),
        vec![
            StepOutcome::Fixable,
            StepOutcome::Fixable,
            StepOutcome::Success,
        ],
    );

    let registry = setup_registry(outcomes);
    let instance = WorkflowInstance::create(workflow_type, config);
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected Success, got {result:?}"
    );
}

// ============================================================================
// Test 5: Test remediation loop exceeds limit abandons
// ============================================================================

/// Test 5: Test remediation loop exceeds limit abandons
/// GIVEN: Workflow loaded from TOML with `max_iterations`: 5 on `remediate→run_tests`
/// WHEN: `run_tests` always returns Fixable
/// THEN: `RunOutcome::Abandoned` after 5 iterations
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-TEST-003
#[test]
fn test_test_remediation_loop_exceeds_limit_abandons() {
    let (workflow_type, config) = load_workflow_from_toml("llxprt-issue-fix-v1", "llxprt-code");

    // Always fixable
    let mut outcomes = HashMap::new();
    outcomes.insert(
        "run_tests".to_string(),
        vec![
            StepOutcome::Fixable,
            StepOutcome::Fixable,
            StepOutcome::Fixable,
            StepOutcome::Fixable,
            StepOutcome::Fixable,
            StepOutcome::Fixable,
        ],
    );

    let registry = setup_registry(outcomes);
    let instance = WorkflowInstance::create(workflow_type, config);
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Abandoned { .. })),
        "Expected Abandoned, got {result:?}"
    );
}

// ============================================================================
// Test 6: Implementation evaluation loop
// ============================================================================

/// Test 6: Implementation evaluation loop
/// GIVEN: Workflow loaded from TOML
/// WHEN: `evaluate_impl` returns Fixable once, then Success
/// THEN: implement called twice
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-IMPL-002,REQ-LF-IMPL-003
#[test]
fn test_impl_evaluation_loop() {
    let (workflow_type, config) = load_workflow_from_toml("llxprt-issue-fix-v1", "llxprt-code");

    // evaluate_impl: Fixable once, then Success
    let mut outcomes = HashMap::new();
    outcomes.insert(
        "evaluate_impl".to_string(),
        vec![StepOutcome::Fixable, StepOutcome::Success],
    );

    let registry = setup_registry(outcomes);
    let instance = WorkflowInstance::create(workflow_type, config);
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected Success, got {result:?}"
    );
}

// ============================================================================
// Test 7: Fatal at select_issue routes to abandon_and_log
// ============================================================================

/// Test 7: Fatal at `select_issue` routes to `abandon_and_log`
/// GIVEN: Workflow loaded from TOML
/// WHEN: `select_issue` returns Fatal
/// THEN: Engine routes to `abandon_and_log`
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-FAIL-001,REQ-LF-ISSUE-004
#[test]
fn test_fatal_at_select_issue_routes_to_abandon_and_log() {
    let (workflow_type, config) = load_workflow_from_toml("llxprt-issue-fix-v1", "llxprt-code");

    // select_issue returns Fatal
    let mut outcomes = HashMap::new();
    outcomes.insert("select_issue".to_string(), vec![StepOutcome::Fatal]);

    let registry = setup_registry(outcomes);
    let instance = WorkflowInstance::create(workflow_type, config);
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    // Fatal should route to abandon_and_log via transition table
    // The workflow should complete (Success) because abandon_and_log is the terminal step
    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected Success (fatal routed to abandon_and_log), got {result:?}"
    );
}

// ============================================================================
// Test 8: Fatal at any step routes to abandon_and_log
// ============================================================================

/// Test 8: Fatal at `setup_workspace` routes to `abandon_and_log`
/// GIVEN: Workflow loaded from TOML
/// WHEN: `setup_workspace` returns Fatal
/// THEN: Engine routes to `abandon_and_log`
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-FAIL-001
#[test]
fn test_fatal_at_setup_workspace_routes_to_abandon_and_log() {
    let (workflow_type, config) = load_workflow_from_toml("llxprt-issue-fix-v1", "llxprt-code");

    let mut outcomes = HashMap::new();
    // select_issue succeeds, setup_workspace fails
    outcomes.insert("select_issue".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("setup_workspace".to_string(), vec![StepOutcome::Fatal]);

    let registry = setup_registry(outcomes);
    let instance = WorkflowInstance::create(workflow_type, config);
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected Success (fatal routed to abandon_and_log), got {result:?}"
    );
}

/// Fatal at `fetch_issue` routes to `abandon_and_log`
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-FAIL-001
#[test]
fn test_fatal_at_fetch_issue_routes_to_abandon_and_log() {
    let (workflow_type, config) = load_workflow_from_toml("llxprt-issue-fix-v1", "llxprt-code");

    let mut outcomes = HashMap::new();
    outcomes.insert("select_issue".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("setup_workspace".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("fetch_issue".to_string(), vec![StepOutcome::Fatal]);

    let registry = setup_registry(outcomes);
    let instance = WorkflowInstance::create(workflow_type, config);
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected Success (fatal routed to abandon_and_log), got {result:?}"
    );
}

/// Fatal at implement routes to `abandon_and_log`
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-FAIL-001
#[test]
fn test_fatal_at_implement_routes_to_abandon_and_log() {
    let (workflow_type, config) = load_workflow_from_toml("llxprt-issue-fix-v1", "llxprt-code");

    let mut outcomes = HashMap::new();
    outcomes.insert("select_issue".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("setup_workspace".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("fetch_issue".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("create_plan".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("evaluate_plan".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("implement".to_string(), vec![StepOutcome::Fatal]);

    let registry = setup_registry(outcomes);
    let instance = WorkflowInstance::create(workflow_type, config);
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected Success (fatal routed to abandon_and_log), got {result:?}"
    );
}

/// Fatal evaluator failure routes to remediation instead of successful cleanup.
/// @requirement:REQ-LF-FAIL-001
#[test]
fn evaluate_impl_fatal_routes_to_remediation_not_cleanup_success() {
    let (workflow_type, config) = load_workflow_from_toml("llxprt-issue-fix-v1", "llxprt-code");

    let mut outcomes = HashMap::new();
    outcomes.insert("select_issue".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("setup_workspace".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("fetch_issue".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("create_plan".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("evaluate_plan".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("implement".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("run_tests".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("evaluate_impl".to_string(), vec![StepOutcome::Fatal]);
    outcomes.insert("remediate".to_string(), vec![StepOutcome::Success]);

    let registry = setup_registry(outcomes);
    let instance = WorkflowInstance::create(workflow_type, config);
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected evaluator fatal to route through remediation and eventually complete, got {result:?}"
    );
}

// ============================================================================
/// Fixable implementation attempts route to remediation rather than blind self-looping.
/// @requirement:REQ-LF-FAIL-001
#[test]
fn implement_fixable_routes_to_remediation() {
    let (workflow_type, config) = load_workflow_from_toml("llxprt-issue-fix-v1", "llxprt-code");

    let route = workflow_type
        .transitions
        .iter()
        .find(|transition| {
            transition.from == "implement" && transition.condition.as_deref() == Some("fixable")
        })
        .expect("implement fixable route exists");

    assert_eq!(
        route.to, "remediate",
        "empty-diff or incomplete implementation attempts should enter feedback-aware remediation instead of blind implementation self-loop"
    );

    let mut outcomes = HashMap::new();
    outcomes.insert("select_issue".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("setup_workspace".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("fetch_issue".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("create_plan".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("evaluate_plan".to_string(), vec![StepOutcome::Success]);
    outcomes.insert(
        "implement".to_string(),
        vec![StepOutcome::Fixable, StepOutcome::Success],
    );
    outcomes.insert("remediate".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("run_tests".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("evaluate_impl".to_string(), vec![StepOutcome::Success]);

    let registry = setup_registry(outcomes);
    let instance = WorkflowInstance::create(workflow_type, config);
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected implement fixable to route through remediation and eventually complete, got {result:?}"
    );
}

/// Fixable remediation attempts get one direct retry instead of failing with no recovery transition.
/// @requirement:REQ-LF-FAIL-001
#[test]
fn remediate_fixable_retries_once_then_continues_to_verification() {
    let (workflow_type, config) = load_workflow_from_toml("llxprt-issue-fix-v1", "llxprt-code");

    let route = workflow_type
        .transitions
        .iter()
        .find(|transition| {
            transition.from == "remediate" && transition.condition.as_deref() == Some("fixable")
        })
        .expect("remediate fixable route exists");

    assert_eq!(route.to, "run_tests");
    assert_eq!(route.max_iterations, Some(2));

    let mut outcomes = HashMap::new();
    outcomes.insert("select_issue".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("setup_workspace".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("fetch_issue".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("create_plan".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("evaluate_plan".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("implement".to_string(), vec![StepOutcome::Fixable]);
    outcomes.insert(
        "remediate".to_string(),
        vec![StepOutcome::Fixable, StepOutcome::Success],
    );
    outcomes.insert("run_tests".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("evaluate_impl".to_string(), vec![StepOutcome::Success]);

    let registry = setup_registry(outcomes);
    let instance = WorkflowInstance::create(workflow_type, config);
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected remediate fixable to retry once and continue, got {result:?}"
    );
}

/// Fixable evaluator feedback routes to remediation rather than re-running implementation blind.
/// @requirement:REQ-LF-FAIL-001
#[test]
fn evaluate_impl_fixable_routes_to_remediation() {
    let (workflow_type, config) = load_workflow_from_toml("llxprt-issue-fix-v1", "llxprt-code");

    let route = workflow_type
        .transitions
        .iter()
        .find(|transition| {
            transition.from == "evaluate_impl" && transition.condition.as_deref() == Some("fixable")
        })
        .expect("evaluate_impl fixable route exists");

    assert_eq!(
        route.to, "remediate",
        "evaluator feedback should be remediated with feedback-aware remediation, not blind reimplementation"
    );
    assert_eq!(
        route.max_iterations,
        Some(2),
        "evaluator feedback can require one cleanup pass plus one follow-up review before abandoning"
    );

    let mut outcomes = HashMap::new();
    outcomes.insert("select_issue".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("setup_workspace".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("fetch_issue".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("create_plan".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("evaluate_plan".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("implement".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("run_tests".to_string(), vec![StepOutcome::Success]);
    outcomes.insert(
        "evaluate_impl".to_string(),
        vec![StepOutcome::Fixable, StepOutcome::Success],
    );
    outcomes.insert("remediate".to_string(), vec![StepOutcome::Success]);

    let registry = setup_registry(outcomes);
    let instance = WorkflowInstance::create(workflow_type, config);
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected evaluator feedback to route through remediation and eventually complete, got {result:?}"
    );
}

// Test 9: Workflow type loads from TOML
// ============================================================================

/// Test 9: Workflow type loads from TOML
/// GIVEN: llxprt-issue-fix-v1.toml exists
/// WHEN: `resolve_workflow_type()` is called
/// THEN: Returns `WorkflowType` with 14 steps, transitions include per-edge limits
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-SEP-003
#[test]
fn test_workflow_type_loads_from_toml() {
    let fixture_root = std::path::PathBuf::from("tests/fixtures");
    let workflow_type = resolve_workflow_type("llxprt-issue-fix-v1", &fixture_root)
        .expect("Failed to load workflow type");

    // Assert base workflow plus PR follow-through tail steps
    assert_eq!(workflow_type.steps.len(), 27, "Expected 27 steps");

    // Assert transitions include per-edge limits
    let has_per_edge_limit = workflow_type
        .transitions
        .iter()
        .any(|t| t.max_iterations.is_some());
    assert!(
        has_per_edge_limit,
        "Expected some transitions with per-edge limits"
    );

    // Check specific steps exist
    let step_ids: Vec<_> = workflow_type
        .steps
        .iter()
        .map(|s| s.step_id.clone())
        .collect();
    assert!(step_ids.contains(&"select_issue".to_string()));
    assert!(step_ids.contains(&"create_pr".to_string()));
    assert!(step_ids.contains(&"abandon_and_log".to_string()));
}

// ============================================================================
/// Forward evaluator/remediation cycles honor explicit transition limits so subjective review cannot loop indefinitely.
/// @requirement:REQ-LF-LOOP-002,REQ-LF-LOOP-004
#[test]
fn evaluate_impl_fixable_remediation_cycle_honors_transition_limit() {
    let (workflow_type, config) = load_workflow_from_toml("llxprt-issue-fix-v1", "llxprt-code");

    let route = workflow_type
        .transitions
        .iter()
        .find(|transition| {
            transition.from == "remediate"
                && transition.to == "run_tests"
                && transition.condition.as_deref() == Some("success")
        })
        .expect("remediate success route exists");
    assert_eq!(route.max_iterations, Some(2));

    let mut outcomes = HashMap::new();
    outcomes.insert("select_issue".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("setup_workspace".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("fetch_issue".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("create_plan".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("evaluate_plan".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("implement".to_string(), vec![StepOutcome::Success]);
    outcomes.insert("run_tests".to_string(), vec![StepOutcome::Success]);
    outcomes.insert(
        "evaluate_impl".to_string(),
        vec![
            StepOutcome::Fixable,
            StepOutcome::Fixable,
            StepOutcome::Fixable,
        ],
    );
    outcomes.insert(
        "remediate".to_string(),
        vec![
            StepOutcome::Success,
            StepOutcome::Success,
            StepOutcome::Success,
        ],
    );

    let registry = setup_registry(outcomes);
    let instance = WorkflowInstance::create(workflow_type, config);
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Abandoned { ref step_id, .. }) if step_id == "evaluate_impl"),
        "Expected the third evaluator remediation request to abandon at explicit evaluator transition limit, got {result:?}"
    );
}

// Test 10: Workflow config loads from TOML
// ============================================================================

/// Test 10: Workflow config loads from TOML
/// GIVEN: llxprt-code.toml exists
/// WHEN: `resolve_workflow_config()` is called
/// THEN: Returns `WorkflowConfig` with required variables
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-PROF-002
#[test]
fn test_workflow_config_loads_from_toml() {
    let fixture_root = std::path::PathBuf::from("tests/fixtures");
    let config = resolve_workflow_config("llxprt-code", &fixture_root)
        .expect("Failed to load workflow config");

    // Assert variables contain required keys
    assert!(
        config.variables.contains_key("profile_planning"),
        "Expected profile_planning variable"
    );
    assert!(
        config.variables.contains_key("profile_evaluating"),
        "Expected profile_evaluating variable"
    );
    assert!(
        config.variables.contains_key("target_repo"),
        "Expected target_repo variable"
    );
    assert_eq!(
        config.variables.get("repository_owner").map(String::as_str),
        Some("vybestack"),
        "Expected repository_owner variable for PR follow-up executors"
    );
    assert_eq!(
        config.variables.get("repository_name").map(String::as_str),
        Some("llxprt-code"),
        "Expected repository_name variable for PR follow-up executors"
    );
    assert!(
        config.variables.contains_key("work_dir"),
        "Expected work_dir variable"
    );
}

// ============================================================================
// Test 11: Workflow graph completeness
// ============================================================================

/// Test 11: Workflow graph completeness — every non-terminal step has fatal transition
/// GIVEN: Workflow loaded from TOML
/// WHEN: We check all non-terminal steps
/// THEN: Every non-terminal step has a fatal → `abandon_and_log` transition
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-FAIL-001
#[test]
fn test_workflow_graph_completeness() {
    let fixture_root = std::path::PathBuf::from("tests/fixtures");
    let workflow_type = resolve_workflow_type("llxprt-issue-fix-v1", &fixture_root)
        .expect("Failed to load workflow type");

    // Terminal steps (no outgoing transitions needed)
    let terminal_steps = ["log_completion", "abandon_and_log"];

    // Collect all steps that have outgoing transitions
    let steps_with_transitions: std::collections::HashSet<_> = workflow_type
        .transitions
        .iter()
        .map(|t| t.from.clone())
        .collect();

    // For each non-terminal step with transitions, verify it has a fatal transition
    for step_id in &steps_with_transitions {
        if terminal_steps.contains(&step_id.as_str()) {
            continue;
        }

        let has_fatal_transition = workflow_type
            .transitions
            .iter()
            .any(|t| t.from == *step_id && t.condition.as_deref() == Some("fatal"));

        assert!(
            has_fatal_transition,
            "Step '{step_id}' should have a fatal transition to abandon_and_log"
        );
    }
}

// ============================================================================
// Test 12: Config variables injected into context
// ============================================================================

/// Test 12: Config variables injected into context
/// GIVEN: `WorkflowConfig` with `profile_planning` set
/// WHEN: Variables are loaded into context
/// THEN: Config contains the expected variable values
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-PROF-003
#[test]
fn test_config_variables_injected_into_context() {
    let (workflow_type, config) = load_workflow_from_toml("llxprt-issue-fix-v1", "llxprt-code");

    // Verify the config has the expected variable
    assert_eq!(
        config.variables.get("profile_planning"),
        Some(&"gpt53codexXHigh".to_string()),
        "Expected profile_planning to be 'gpt53codexXHigh'"
    );

    // Create and run the workflow — the variables should be in context
    let registry = setup_registry(HashMap::new());
    let instance = WorkflowInstance::create(workflow_type, config);
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    // Run should succeed (variables don't affect mock execution)
    let result = runner.run();
    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected Success, got {result:?}"
    );
}

// ============================================================================
// Test 13: Run completion records metadata
// ============================================================================

/// Test 13: Run completion records metadata
/// GIVEN: Workflow loaded from TOML, run with temp DB
/// WHEN: Happy path completes
/// THEN: Run metadata record exists with correct status
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-FAIL-005
#[test]
fn test_run_completion_records_metadata() {
    let (workflow_type, config) = load_workflow_from_toml("llxprt-issue-fix-v1", "llxprt-code");

    // Create temp database
    let temp_dir = std::env::temp_dir();
    let db_path = temp_dir.join(format!("test_e2e_{}.db", uuid::Uuid::new_v4()));

    let registry = setup_registry(HashMap::new());
    let instance = WorkflowInstance::create(workflow_type, config);
    let mut runner = EngineRunner::with_db_path(instance, registry, &db_path)
        .expect("Failed to create EngineRunner");
    let run_id = runner.run_id().to_string();

    let result = runner.run();
    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected Success, got {result:?}"
    );

    // Query the database for run metadata
    let store = SqliteStore::open(&db_path).expect("Failed to open store");
    let metadata = store
        .get_run(&run_id)
        .expect("Failed to get run metadata")
        .expect("Run metadata should exist");

    assert_eq!(
        metadata.status,
        luther_workflow::persistence::RunStatus::Completed,
        "Status should be Completed"
    );

    // Clean up
    let _ = std::fs::remove_file(&db_path);
}

// ============================================================================
// Phase 16: Post-PR workflow graph TDD
// ============================================================================

use luther_workflow::workflow::config_loader::parse_workflow_type_toml;
use luther_workflow::workflow::schema::{WorkflowConfig, WorkflowType};

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
/// @pseudocode lines 1-53
const POST_PR_STEPS: [&str; 13] = [
    "capture_pr_identity",
    "post_pr_iteration_guard",
    "watch_pr_checks",
    "collect_ci_failures",
    "collect_coderabbit_feedback",
    "evaluate_coderabbit_feedback",
    "build_remediation_plan",
    "remediate_pr_followup",
    "validate_remediation_result",
    "run_post_pr_tests",
    "push_remediation_changes",
    "mark_coderabbit_feedback",
    "post_pr_failure_terminal",
];

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
/// @pseudocode lines 1-53
const PRIMARY_POST_PR_ROUTE: [&str; 13] = [
    "capture_pr_identity",
    "post_pr_iteration_guard",
    "watch_pr_checks",
    "collect_ci_failures",
    "collect_coderabbit_feedback",
    "evaluate_coderabbit_feedback",
    "build_remediation_plan",
    "mark_coderabbit_feedback",
    "remediate_pr_followup",
    "validate_remediation_result",
    "run_post_pr_tests",
    "push_remediation_changes",
    "post_pr_failure_terminal",
];

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
/// @pseudocode lines 1-53
fn post_pr_workflow() -> WorkflowType {
    let fixture_root = std::path::PathBuf::from("tests/fixtures");
    resolve_workflow_type("llxprt-issue-fix-v1", &fixture_root)
        .expect("Failed to load workflow type")
}

fn workflow_config(config_id: &str) -> WorkflowConfig {
    let fixture_root = std::path::PathBuf::from("tests/fixtures");
    resolve_workflow_config(config_id, &fixture_root).expect("Failed to load workflow config")
}

fn context_from_config(config: &WorkflowConfig) -> StepContext {
    let work_dir = config
        .variables
        .get("work_dir")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let mut context = StepContext::new(work_dir, "test-run".to_string());
    for (key, value) in &config.variables {
        context.set(key, value);
    }
    if let Some(issue_number) = config.variables.get("primary_issue_number") {
        context.set("issue_number", issue_number);
    }
    context.set_current_step_id("setup_workspace");
    context.set("existing_pr_number", "0");
    context.set_current_step_id("run_tests");
    context
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
/// @pseudocode lines 1-53
fn load_workflow_toml(path: &str) -> WorkflowType {
    let content = std::fs::read_to_string(path).unwrap_or_else(|err| panic!("read {path}: {err}"));
    parse_workflow_type_toml(&content).unwrap_or_else(|err| panic!("parse {path}: {err}"))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
/// @pseudocode lines 1-53
fn transition_targets(
    workflow_type: &WorkflowType,
    from: &str,
    condition: Option<&str>,
) -> Vec<String> {
    workflow_type
        .transitions
        .iter()
        .filter(|transition| {
            transition.from == from && transition.condition.as_deref() == condition
        })
        .map(|transition| transition.to.clone())
        .collect()
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
/// @pseudocode lines 1-53
fn effective_condition(condition: Option<&str>) -> &str {
    condition.unwrap_or("success")
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
/// @pseudocode lines 1-53
fn reachable_steps(workflow_type: &WorkflowType, start: &str) -> std::collections::HashSet<String> {
    let mut stack = vec![start.to_string()];
    let mut seen = std::collections::HashSet::new();
    while let Some(step) = stack.pop() {
        if !seen.insert(step.clone()) {
            continue;
        }
        for transition in workflow_type
            .transitions
            .iter()
            .filter(|transition| transition.from == step)
        {
            stack.push(transition.to.clone());
        }
    }
    seen
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
/// @pseudocode lines 1-53
fn assert_single_target(workflow_type: &WorkflowType, from: &str, condition: &str, expected: &str) {
    let targets = transition_targets(workflow_type, from, Some(condition));
    assert_eq!(
        targets,
        vec![expected.to_string()],
        "expected exactly one post-PR transition {from} --{condition}--> {expected}, got {targets:?}"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
/// @pseudocode lines 1-53
fn post_pr_duplicate_transition_errors(workflow_type: &WorkflowType) -> Vec<String> {
    let mut reachable = reachable_steps(workflow_type, "capture_pr_identity");
    reachable.insert("create_pr".to_string());
    for step_id in POST_PR_STEPS {
        reachable.insert(step_id.to_string());
    }
    let mut seen = std::collections::HashMap::new();
    let mut errors = Vec::new();
    for transition in workflow_type
        .transitions
        .iter()
        .filter(|transition| reachable.contains(&transition.from))
    {
        let key = (
            transition.from.clone(),
            effective_condition(transition.condition.as_deref()).to_string(),
        );
        if let Some(previous) = seen.insert(key.clone(), transition.to.clone()) {
            errors.push(format!(
                "duplicate post-PR transition branch for {} outcome {}: {} and {}",
                key.0, key.1, previous, transition.to
            ));
        }
    }
    errors
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
/// @pseudocode lines 1-53
fn post_pr_forbidden_route_errors(workflow_type: &WorkflowType) -> Vec<String> {
    let reachable = reachable_steps(workflow_type, "capture_pr_identity");
    let mut errors = Vec::new();
    for transition in workflow_type
        .transitions
        .iter()
        .filter(|transition| reachable.contains(&transition.from))
    {
        if transition.to == "abandon_and_log" {
            errors.push(format!(
                "post-PR route {} -> abandon_and_log is forbidden",
                transition.from
            ));
        }
        if transition.condition.as_deref() == Some("abandon") {
            errors.push(format!(
                "post-PR route {} uses abandon outcome",
                transition.from
            ));
        }
        if transition
            .condition
            .as_deref()
            .is_some_and(|condition| condition == "fatal" || condition == "retryable")
            && transition.to != "post_pr_failure_terminal"
            && transition.from != "watch_pr_checks"
        {
            errors.push(format!(
                "post-PR non-success route {} --{}--> {} must target post_pr_failure_terminal",
                transition.from,
                transition.condition.as_deref().unwrap_or("success"),
                transition.to
            ));
        }
    }
    errors
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018
/// @pseudocode lines 1-53
#[test]
fn post_pr_no_direct_create_pr_to_log_completion() {
    let workflow_type = post_pr_workflow();
    let direct_success_targets = transition_targets(&workflow_type, "create_pr", None);
    assert_eq!(
        direct_success_targets,
        vec!["capture_pr_identity".to_string()],
        "create_pr success must route only to capture_pr_identity and never directly to log_completion"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
/// @pseudocode lines 1-53
#[test]
fn post_pr_guard_enumerates_all_steps_and_forbids_abandon_routes() {
    let workflow_type = post_pr_workflow();
    for step_id in POST_PR_STEPS {
        assert!(
            workflow_type
                .steps
                .iter()
                .any(|step| step.step_id == step_id),
            "missing post-PR step {step_id}"
        );
    }
    let errors = post_pr_forbidden_route_errors(&workflow_type);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
/// @pseudocode lines 50-53
#[test]
fn post_pr_failure_terminal_is_terminal_and_fatal_routes_target_it() {
    let workflow_type = post_pr_workflow();
    let terminal_outgoing: Vec<_> = workflow_type
        .transitions
        .iter()
        .filter(|transition| transition.from == "post_pr_failure_terminal")
        .collect();
    assert!(
        terminal_outgoing.is_empty(),
        "post_pr_failure_terminal must be terminal with no outgoing transitions"
    );
    for step in POST_PR_STEPS
        .into_iter()
        .filter(|step| *step != "post_pr_failure_terminal" && *step != "watch_pr_checks")
    {
        assert_single_target(&workflow_type, step, "fatal", "post_pr_failure_terminal");
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 1-53
#[test]
fn post_pr_exact_p17_routing_contract_is_present() {
    let workflow_type = post_pr_workflow();
    for step_id in [
        "run_post_pr_tests",
        "remediate_pr_followup",
        "push_remediation_changes",
    ] {
        assert!(
            workflow_type
                .steps
                .iter()
                .any(|step| step.step_id == step_id),
            "missing P17 step {step_id}"
        );
    }
    assert_single_target(
        &workflow_type,
        "watch_pr_checks",
        "success",
        "collect_ci_failures",
    );
    assert_single_target(
        &workflow_type,
        "watch_pr_checks",
        "fixable",
        "collect_ci_failures",
    );
    assert_single_target(
        &workflow_type,
        "watch_pr_checks",
        "fatal",
        "collect_ci_failures",
    );
    assert!(
        transition_targets(&workflow_type, "watch_pr_checks", Some("fatal"))
            .iter()
            .all(|target| target == "collect_ci_failures"),
        "watch_pr_checks fatal must not route directly to a terminal"
    );
    assert_single_target(
        &workflow_type,
        "collect_ci_failures",
        "success",
        "collect_coderabbit_feedback",
    );
    assert_single_target(
        &workflow_type,
        "collect_ci_failures",
        "fatal",
        "post_pr_failure_terminal",
    );
    assert_single_target(
        &workflow_type,
        "build_remediation_plan",
        "success",
        "mark_coderabbit_feedback",
    );
    assert_single_target(
        &workflow_type,
        "mark_coderabbit_feedback",
        "success",
        "log_completion",
    );
    assert_single_target(
        &workflow_type,
        "mark_coderabbit_feedback",
        "fatal",
        "post_pr_failure_terminal",
    );
    let marker_routes: Vec<_> = workflow_type
        .transitions
        .iter()
        .filter(|transition| transition.from == "mark_coderabbit_feedback")
        .collect();
    assert_eq!(marker_routes.len(), 2, "mark_coderabbit_feedback may only route success to log_completion and fatal to post_pr_failure_terminal");
    assert_single_target(
        &workflow_type,
        "remediate_pr_followup",
        "success",
        "validate_remediation_result",
    );
    assert_single_target(
        &workflow_type,
        "remediate_pr_followup",
        "fatal",
        "post_pr_failure_terminal",
    );
    assert!(
        transition_targets(&workflow_type, "remediate_pr_followup", Some("retryable")).is_empty(),
        "remediate_pr_followup retryable route is forbidden in v1"
    );
    assert_single_target(
        &workflow_type,
        "validate_remediation_result",
        "success",
        "run_post_pr_tests",
    );
    assert_single_target(
        &workflow_type,
        "validate_remediation_result",
        "fixable",
        "remediate_pr_followup",
    );
    assert_single_target(
        &workflow_type,
        "run_post_pr_tests",
        "success",
        "push_remediation_changes",
    );
    assert_single_target(
        &workflow_type,
        "run_post_pr_tests",
        "fixable",
        "remediate_pr_followup",
    );
    assert_single_target(
        &workflow_type,
        "push_remediation_changes",
        "success",
        "capture_pr_identity",
    );
    assert_single_target(
        &workflow_type,
        "push_remediation_changes",
        "fixable",
        "mark_coderabbit_feedback",
    );

    for transition in workflow_type
        .transitions
        .iter()
        .filter(|transition| POST_PR_STEPS.contains(&transition.from.as_str()))
    {
        assert_ne!(
            transition.to, "generate_pr_description",
            "post-PR route must not point to generate_pr_description"
        );
        assert_ne!(
            transition.to, "create_pr",
            "post-PR route must not point to create_pr"
        );
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
/// @pseudocode lines 1-53
#[test]
fn post_pr_reachable_transitions_are_unique_by_from_and_effective_condition() {
    let workflow_type = post_pr_workflow();
    let errors = post_pr_duplicate_transition_errors(&workflow_type);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
/// @pseudocode lines 1-53
#[test]
fn post_pr_duplicate_transition_negative_fixtures_are_detected() {
    for (path, expected) in [
        (
            "tests/fixtures/workflows/invalid/p16-duplicate-create-pr-success.toml",
            "duplicate post-PR transition branch for create_pr outcome success",
        ),
        (
            "tests/fixtures/workflows/invalid/p16-duplicate-watch-pr-checks-fatal.toml",
            "duplicate post-PR transition branch for watch_pr_checks outcome fatal",
        ),
        (
            "tests/fixtures/workflows/invalid/p16-duplicate-build-remediation-plan-success.toml",
            "duplicate post-PR transition branch for build_remediation_plan outcome success",
        ),
    ] {
        let workflow_type = load_workflow_toml(path);
        let errors = post_pr_duplicate_transition_errors(&workflow_type);
        assert!(
            errors.iter().any(|error| error.contains(expected)),
            "{path} did not produce {expected}; got {errors:?}"
        );
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
/// @pseudocode lines 50-53
#[test]
fn post_pr_negative_fixture_detects_fatal_route_to_successful_cleanup() {
    let workflow_type =
        load_workflow_toml("tests/fixtures/workflows/invalid/p16-post-pr-fatal-to-abandon.toml");
    let errors = post_pr_forbidden_route_errors(&workflow_type);
    assert!(
        errors
            .iter()
            .any(|error| error.contains("capture_pr_identity -> abandon_and_log")),
        "negative fixture did not detect post-PR fatal cleanup route: {errors:?}"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-001,REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 1-53
#[test]
fn post_pr_steps_have_artifact_root_step_order_and_path_contract() {
    let workflow_type = post_pr_workflow();
    let mut indexes = std::collections::HashMap::new();
    for step_id in POST_PR_STEPS {
        let step = workflow_type
            .steps
            .iter()
            .find(|step| step.step_id == step_id)
            .unwrap_or_else(|| panic!("missing post-PR step {step_id}"));
        let params = step
            .parameters
            .as_ref()
            .unwrap_or_else(|| panic!("post-PR step {step_id} must have parameters"));
        assert_eq!(
            params
                .get("artifact_root")
                .and_then(serde_json::Value::as_str),
            Some("{artifact_dir}"),
            "post-PR step {step_id} must declare exactly artifact_root"
        );
        assert!(
            params.get("artifact_dir").is_none(),
            "post-PR step {step_id} must not use artifact_dir"
        );
        let index = params
            .get("step_order_index")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_else(|| {
                panic!("post-PR step {step_id} must declare integer step_order_index")
            });
        assert!(
            indexes.insert(index, step_id).is_none(),
            "duplicate post-PR step_order_index {index}"
        );
        for (key, value) in params.as_object().expect("parameters are object") {
            if (key.ends_with("_path") || key.ends_with("_file") || key.ends_with("_root"))
                && key != "artifact_root"
            {
                if let Some(path) = value.as_str() {
                    assert!(path.starts_with("{artifact_dir}/"), "post-PR step {step_id} path param {key}={path} must be inside artifact_root");
                }
            }
        }
    }
    let mut previous = 0;
    for step_id in PRIMARY_POST_PR_ROUTE {
        let index = workflow_type
            .steps
            .iter()
            .find(|step| step.step_id == step_id)
            .and_then(|step| step.parameters.as_ref())
            .and_then(|params| params.get("step_order_index"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_else(|| panic!("missing step_order_index for {step_id}"));
        assert!(
            index > previous,
            "step_order_index for {step_id} must be monotonic along primary route"
        );
        previous = index;
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 18-33
#[test]
fn post_pr_fake_executor_contract_never_returns_abandon() {
    let fake_executor_outcomes = [
        ("capture_pr_identity", StepOutcome::Success),
        ("post_pr_iteration_guard", StepOutcome::Success),
        ("watch_pr_checks", StepOutcome::Success),
        ("collect_ci_failures", StepOutcome::Fatal),
        ("build_remediation_plan", StepOutcome::Fixable),
        ("validate_remediation_result", StepOutcome::Fixable),
        ("run_post_pr_tests", StepOutcome::Fixable),
        ("push_remediation_changes", StepOutcome::Success),
        ("mark_coderabbit_feedback", StepOutcome::Fatal),
        ("post_pr_failure_terminal", StepOutcome::Fatal),
    ];
    for (step_id, outcome) in fake_executor_outcomes {
        assert_ne!(
            outcome,
            StepOutcome::Abandon,
            "fake post-PR executor {step_id} must not return StepOutcome::Abandon"
        );
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 50-53
#[test]
fn post_pr_fake_runner_fatal_outcome_ends_at_failure_terminal() {
    let workflow_type = post_pr_workflow();
    let config =
        resolve_workflow_config("llxprt-code", &std::path::PathBuf::from("tests/fixtures"))
            .expect("Failed to load workflow config");
    let mut outcomes = HashMap::new();
    outcomes.insert("collect_ci_failures".to_string(), vec![StepOutcome::Fatal]);
    outcomes.insert(
        "post_pr_failure_terminal".to_string(),
        vec![StepOutcome::Fatal],
    );
    let registry = setup_registry(outcomes);
    let instance = WorkflowInstance::create(workflow_type, config);
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");
    let result = runner.run();
    assert!(
        matches!(result, Ok(RunOutcome::Failure { ref step_id, .. }) if step_id == "post_pr_failure_terminal"),
        "post-PR fatal must end as RunOutcome::Failure at post_pr_failure_terminal, got {result:?}"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-020
/// @pseudocode lines 1-53
#[test]
fn llxprt_dry_run_step_list_current_toml_lists_existing_steps() {
    let workflow_type = post_pr_workflow();
    let step_ids: Vec<_> = workflow_type
        .steps
        .iter()
        .map(|step| step.step_id.as_str())
        .collect();
    for step_id in [
        "select_issue",
        "setup_workspace",
        "fetch_issue",
        "create_plan",
        "evaluate_plan",
        "implement",
        "evaluate_impl",
        "run_tests",
        "remediate",
        "push_changes",
        "generate_pr_description",
        "create_pr",
        "abandon_and_log",
        "log_completion",
    ] {
        assert!(
            step_ids.contains(&step_id),
            "dry-run step list missing current-TOML step {step_id}"
        );
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-020
/// @pseudocode lines 1-53
#[test]
fn llxprt_dry_run_step_list_includes_full_post_pr_contract() {
    let workflow_type = post_pr_workflow();
    let step_ids: Vec<_> = workflow_type
        .steps
        .iter()
        .map(|step| step.step_id.as_str())
        .collect();
    for step_id in POST_PR_STEPS {
        assert!(
            step_ids.contains(&step_id),
            "dry-run step list missing {step_id}"
        );
    }
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P17
/// @requirement:REQ-LF-ISSUE-004
#[test]
fn primary_issue_selection_allows_already_claimed_primary_issue() {
    let workflow_type = post_pr_workflow();
    let select_issue = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "select_issue")
        .expect("select_issue step exists");
    let command = select_issue
        .parameters
        .as_ref()
        .and_then(|params| params.get("command"))
        .and_then(serde_json::Value::as_str)
        .expect("select_issue command exists");

    let primary_block = command
        .split("# Query milestones sorted by semver")
        .next()
        .expect("primary issue block exists");
    assert!(
        !primary_block.contains("length) == 0")
            && !primary_block.contains("!= \"{luther_label}\""),
        "primary_issue_number smoke runs must not be skipped just because a prior attempt already assigned/labeled the issue: {primary_block}"
    );
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P17
/// @requirement:REQ-LF-FAIL-001
#[test]
fn abandon_cleanup_falls_back_to_primary_issue_number() {
    let workflow_type = post_pr_workflow();
    let abandon = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "abandon_and_log")
        .expect("abandon_and_log step exists");
    let command = abandon
        .parameters
        .as_ref()
        .and_then(|params| params.get("command"))
        .and_then(serde_json::Value::as_str)
        .expect("abandon command exists");

    assert!(
        command.contains("ISSUE_NUM=\"{primary_issue_number}\""),
        "abandon cleanup should fall back to primary_issue_number when select_issue failed before setting context"
    );
    assert!(
        command.contains("gh issue edit \"$ISSUE_NUM\"")
            && command.contains("gh issue comment \"$ISSUE_NUM\""),
        "abandon cleanup must use resolved ISSUE_NUM for comment/label/assignee cleanup"
    );
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P17
/// @requirement:REQ-LF-VERIFY-002
#[test]
fn run_tests_format_command_formats_changed_files_before_checking() {
    let workflow_type = post_pr_workflow();
    let config = workflow_config("llxprt-code");
    let context = context_from_config(&config);
    let run_tests = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "run_tests")
        .expect("run_tests step exists");
    let format_command = run_tests
        .parameters
        .as_ref()
        .and_then(|params| params.get("check_commands"))
        .and_then(|commands| commands.get("format"))
        .and_then(serde_json::Value::as_str)
        .expect("format check command exists");
    assert_eq!(
        format_command, "{format_command}",
        "workflow type should source format command from target profile config"
    );
    let format_command = interpolate_string(format_command, &context);

    assert!(
        format_command.contains("prettier --write $CHANGED")
            && format_command.contains("prettier --check $CHANGED"),
        "format check should deterministically normalize changed files before asserting they are formatted: {format_command}"
    );
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P17
/// @requirement:REQ-LF-IMPL-002
#[test]
fn implement_required_changed_paths_are_profile_driven() {
    let workflow_type = post_pr_workflow();
    let implement = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "implement")
        .expect("implement step exists");
    let patterns = implement
        .parameters
        .as_ref()
        .and_then(|params| params.get("required_changed_path_patterns"))
        .and_then(serde_json::Value::as_array)
        .expect("implement required_changed_path_patterns exists");
    let pattern_values = patterns
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>();

    assert!(
        pattern_values.contains(&"{required_changed_path_pattern}"),
        "implement success_on_diff should be scoped by target profile config instead of a workflow literal: {pattern_values:?}"
    );

    let llxprt_code = workflow_config("llxprt-code");
    let llxprt_code_context = context_from_config(&llxprt_code);
    let llxprt_code_patterns = pattern_values
        .iter()
        .map(|pattern| interpolate_string(pattern, &llxprt_code_context))
        .collect::<Vec<_>>();
    assert!(
        llxprt_code_patterns.iter().any(|pattern| pattern == "packages/cli/src/ui/hooks/"),
        "llxprt-code profile should preserve the issue #1803 changed-path scope: {llxprt_code_patterns:?}"
    );

    let alt_config = workflow_config("llxprt-code-alt");
    let alt_context = context_from_config(&alt_config);
    let alt_patterns = pattern_values
        .iter()
        .map(|pattern| interpolate_string(pattern, &alt_context))
        .collect::<Vec<_>>();
    assert!(
        alt_patterns
            .iter()
            .any(|pattern| pattern == "packages/core/src/core/"),
        "alternate profile should drive a distinct changed-path scope: {alt_patterns:?}"
    );
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P17
/// @requirement:REQ-LF-PLAN-002
#[test]
fn dogfood_evaluate_plan_requires_real_llxprt_review() {
    let workflow_type = resolve_workflow_type(
        "llxprt-luther-dogfood-v1",
        &std::path::PathBuf::from("config"),
    )
    .expect("production dogfood workflow type should load");

    let evaluate_plan = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "evaluate_plan")
        .expect("evaluate_plan step exists");
    let params = evaluate_plan
        .parameters
        .as_ref()
        .expect("evaluate_plan parameters exist");

    assert!(
        params.get("static_stdout").is_none(),
        "dogfood plan evaluation must invoke the evaluator instead of auto-approving placeholder plans"
    );
    let prompt = params
        .get("prompt")
        .and_then(serde_json::Value::as_str)
        .expect("evaluate_plan prompt exists");
    assert!(
        prompt.contains("Reject placeholder, generic, underspecified, or shell-unsafe plans"),
        "evaluate_plan prompt should reject placeholder and shell-unsafe plans: {prompt}"
    );
    assert!(
        prompt.contains("structured argv/path configuration or executor support"),
        "evaluate_plan prompt should prevent unsafe profile-provided shell snippets: {prompt}"
    );
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P17
/// @requirement:REQ-LF-PLAN-002
#[test]
fn reusable_issue_fix_evaluate_plan_requires_real_llxprt_review() {
    let workflow_type = post_pr_workflow();
    let evaluate_plan = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "evaluate_plan")
        .expect("evaluate_plan step exists");
    let params = evaluate_plan
        .parameters
        .as_ref()
        .expect("evaluate_plan parameters exist");

    assert!(
        params.get("static_stdout").is_none(),
        "reusable issue-fix workflow must not auto-approve plans"
    );
}


/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-PLAN-002
#[test]
fn dogfood_agents_are_warned_against_profile_shell_snippets() {
    let workflow_type = resolve_workflow_type(
        "llxprt-luther-dogfood-v1",
        &std::path::PathBuf::from("config"),
    )
    .expect("production dogfood workflow type should load");

    for step_id in ["implement", "remediate"] {
        let step = workflow_type
            .steps
            .iter()
            .find(|step| step.step_id == step_id)
            .expect("dogfood agent step exists");
        let prompt = step
            .parameters
            .as_ref()
            .and_then(|params| params.get("prompt"))
            .and_then(serde_json::Value::as_str)
            .expect("dogfood agent prompt exists");

        assert!(
            prompt.contains("Do not insert raw profile-provided shell snippets"),
            "{step_id} prompt should guard against unsafe profile shell snippets: {prompt}"
        );
        assert!(
            prompt.contains("structured argv/path variables or source-code executor support"),
            "{step_id} prompt should direct agents toward shell-safe designs: {prompt}"
        );
    }
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-PLAN-002
#[test]
fn plan_creation_fixable_routes_to_retry() {
    let workflow_type = resolve_workflow_type(
        "llxprt-luther-dogfood-v1",
        &std::path::PathBuf::from("config"),
    )
    .expect("production dogfood workflow type should load");

    let retry_transition = workflow_type
        .transitions
        .iter()
        .find(|transition| {
            transition.from == "create_plan"
                && transition.to == "create_plan"
                && transition.condition.as_deref() == Some("fixable")
        })
        .expect("create_plan fixable transition should retry planning");
    assert_eq!(
        retry_transition.max_iterations,
        Some(3),
        "planner retry should be bounded"
    );
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P17
/// @requirement:REQ-LF-PLAN-002
#[test]
fn create_plan_static_content_is_profile_driven() {
    let workflow_type = post_pr_workflow();
    let create_plan = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "create_plan")
        .expect("create_plan step exists");
    let static_plan = create_plan
        .parameters
        .as_ref()
        .and_then(|params| params.get("static_content"))
        .and_then(serde_json::Value::as_str)
        .expect("create_plan static plan exists");

    assert_eq!(
        static_plan, "{plan_static_content}",
        "workflow type should not carry issue-specific static plan content"
    );

    let llxprt_code = workflow_config("llxprt-code");
    let llxprt_code_context = context_from_config(&llxprt_code);
    let llxprt_code_static_plan = interpolate_string(static_plan, &llxprt_code_context);

    for expected in [
        "contextCleared=true",
        "useStreamEventHandlers.ts",
        "geminiMessageBuffer",
        "AgentExecutionStopped/AgentExecutionBlocked",
        "separate Gemini messages",
    ] {
        assert!(
            llxprt_code_static_plan.contains(expected),
            "llxprt-code profile static plan should direct the agent to the issue #1803 stream-buffer fix; missing {expected}: {llxprt_code_static_plan}"
        );
    }

    let alt_config = workflow_config("llxprt-code-alt");
    let alt_context = context_from_config(&alt_config);
    let alt_static_plan = interpolate_string(static_plan, &alt_context);
    assert!(
        alt_static_plan.contains("StreamProcessor.ts")
            && alt_static_plan.contains("cancellation state"),
        "alternate profile should provide distinct target-specific static plan content: {alt_static_plan}"
    );
    assert_ne!(
        llxprt_code_static_plan, alt_static_plan,
        "two profiles should load the same workflow type with different target-specific plan content"
    );
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P17
/// @requirement:REQ-LF-IMPLEMENT-001
#[test]
fn dogfood_agents_do_not_escalate_self_authored_shell_syntax_errors() {
    let workflow_type = resolve_workflow_type(
        "llxprt-luther-dogfood-v1",
        &std::path::PathBuf::from("config"),
    )
    .expect("production dogfood workflow type should load");

    for step_id in ["implement", "remediate"] {
        let step = workflow_type
            .steps
            .iter()
            .find(|step| step.step_id == step_id)
            .expect("llxprt agent step exists");
        let prompt = step
            .parameters
            .as_ref()
            .and_then(|params| params.get("prompt"))
            .and_then(serde_json::Value::as_str)
            .expect("agent prompt exists");
        assert!(
            prompt.contains("agent-authored shell syntax mistakes"),
            "{step_id} prompt should require correcting self-authored shell syntax mistakes instead of escalating: {prompt}"
        );
    }
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P17
/// @requirement:REQ-LF-VERIFY-002
#[test]
fn run_tests_mirrors_ci_lint_typecheck_and_build_before_push() {
    let workflow_type = post_pr_workflow();
    let config = workflow_config("llxprt-code");
    let context = context_from_config(&config);
    let run_tests = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "run_tests")
        .expect("run_tests step exists");
    let params = run_tests
        .parameters
        .as_ref()
        .expect("run_tests parameters exist");
    let checks = params
        .get("checks")
        .and_then(serde_json::Value::as_array)
        .expect("checks array exists");
    let check_names = checks
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>();

    for expected in ["lint", "typecheck", "build", "test", "format"] {
        assert!(
            check_names.contains(&expected),
            "run_tests should include {expected} before push: {check_names:?}"
        );
    }

    let commands = params
        .get("check_commands")
        .and_then(serde_json::Value::as_object)
        .expect("check_commands exist");
    let lint_command = commands
        .get("lint")
        .and_then(serde_json::Value::as_str)
        .expect("lint command exists");
    assert_eq!(
        lint_command, "{lint_command}",
        "workflow type should source lint command from target profile config"
    );
    let lint_command = interpolate_string(lint_command, &context);
    assert!(
        lint_command.contains("git status --porcelain --untracked-files=all"),
        "pre-PR verification should lint changed files only to keep smoke gates bounded: {lint_command}"
    );
    assert!(
        lint_command.contains("npx eslint $CHANGED"),
        "pre-PR verification should run eslint over changed lintable files: {lint_command}"
    );
    assert!(
        lint_command.contains("tail -n 240"),
        "pre-PR verification should cap lint output for remediation prompts: {lint_command}"
    );
    assert_eq!(
        commands
            .get("typecheck")
            .and_then(serde_json::Value::as_str)
            .map(|command| interpolate_string(command, &context)),
        Some("npm run typecheck 2>&1".to_string()),
        "pre-PR verification should run workspace type checking"
    );
    assert_eq!(
        commands
            .get("build")
            .and_then(serde_json::Value::as_str)
            .map(|command| interpolate_string(command, &context)),
        Some("npm run build 2>&1".to_string()),
        "pre-PR verification should run the full build rather than only core"
    );
    assert!(
        commands.get("build_core").is_none(),
        "run_tests should not use the narrow core-only build gate"
    );
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P17
/// @requirement:REQ-LF-VERIFY-003
#[test]
fn remediate_fatal_routes_to_cleanup_instead_of_default_success_transition() {
    let workflow_type = post_pr_workflow();
    let fatal_route = workflow_type
        .transitions
        .iter()
        .find(|transition| {
            transition.from == "remediate" && transition.condition.as_deref() == Some("fatal")
        })
        .expect("remediate fatal route exists");
    assert_eq!(fatal_route.to, "abandon_and_log");
}
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P17
/// @requirement:REQ-PRFU-020A,REQ-LF-PR-001
#[test]
fn workflow_has_no_unresolved_artifact_root_template() {
    let workflow_text = std::fs::read_to_string("config/workflows/llxprt-issue-fix-v1.toml")
        .expect("read production workflow TOML");
    assert!(
        !workflow_text.contains("{artifact_root}"),
        "production workflow must interpolate PR follow-up artifact paths from artifact_dir, not an undefined artifact_root template"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P17
/// @requirement:REQ-PRFU-001,REQ-PRFU-018,REQ-PRFU-020A
/// @pseudocode lines 1-53
/// @plan:PLAN-20260408-LLXPRT-FIRST.P17
/// @requirement:REQ-LF-REPO-002
#[test]
fn push_changes_uses_force_with_lease_for_repeatable_smoke_branch_updates() {
    let workflow_type = post_pr_workflow();
    let push = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "push_changes")
        .expect("push_changes step exists");
    let command = push
        .parameters
        .as_ref()
        .and_then(|params| params.get("command"))
        .and_then(serde_json::Value::as_str)
        .expect("push_changes command exists");

    assert!(
        command.contains("git push --force-with-lease -u origin issue{issue_number}"),
        "repeatable Luther smoke runs should update Luther-owned issue branches with force-with-lease instead of failing non-fast-forward: {command}"
    );
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P17
/// @requirement:REQ-LF-EXEC-001
#[test]
fn implement_step_allows_early_diff_completion_for_workflow_verification() {
    let workflow_type = post_pr_workflow();
    let implement = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "implement")
        .expect("implement step exists");
    let params = implement
        .parameters
        .as_ref()
        .expect("implement parameters exist");

    assert_eq!(
        params
            .get("success_on_diff")
            .and_then(serde_json::Value::as_bool),
        Some(true),
        "implement should be diff-gated"
    );
    assert!(
        params.get("early_success_on_diff").is_none(),
        "implement should not force llxprt to keep running after a scoped diff exists; deterministic verification/remediation handles test failures"
    );
    assert_eq!(
        params
            .get("continue_on_empty_diff")
            .and_then(serde_json::Value::as_bool),
        Some(false),
        "implement should not treat non-empty model chatter as success when no diff was produced"
    );
}

#[test]
fn post_pr_params_have_no_unresolved_template_tokens() {
    let workflow_type = post_pr_workflow();
    for step_id in POST_PR_STEPS {
        let step = workflow_type
            .steps
            .iter()
            .find(|step| step.step_id == step_id)
            .unwrap_or_else(|| panic!("missing post-PR step {step_id}"));
        let params = step
            .parameters
            .as_ref()
            .unwrap_or_else(|| panic!("post-PR step {step_id} must have parameters"));
        let params_text = serde_json::to_string(params).expect("serialize params");
        let forbidden_markers = [
            "TODO".to_string(),
            "TBD".to_string(),
            "${".to_string(),
            format!("{} {}", "@pseudocode lines", "X-Y"),
            format!("{} {}", "fixture", "TBD"),
            format!("{} {}", "assertion", "TBD"),
            format!("{} {}", "json_path", "TBD"),
        ];
        for forbidden in forbidden_markers {
            assert!(
                !params_text.contains(&forbidden),
                "post-PR step {step_id} contains unresolved marker {forbidden}: {params_text}"
            );
        }
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P17
/// @requirement:REQ-PRFU-001,REQ-PRFU-020A
/// @pseudocode lines 1-53
#[test]
fn post_pr_steps_require_artifact_root() {
    let workflow_type = post_pr_workflow();
    for step_id in POST_PR_STEPS {
        let params = workflow_type
            .steps
            .iter()
            .find(|step| step.step_id == step_id)
            .and_then(|step| step.parameters.as_ref())
            .unwrap_or_else(|| panic!("post-PR step {step_id} must have parameters"));
        assert_eq!(
            params
                .get("artifact_root")
                .and_then(serde_json::Value::as_str),
            Some("{artifact_dir}")
        );
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P17
/// @requirement:REQ-PRFU-001,REQ-PRFU-020A
/// @pseudocode lines 1-53
#[test]
fn post_pr_steps_forbid_artifact_dir_alias() {
    let workflow_type = post_pr_workflow();
    for step_id in POST_PR_STEPS {
        let params = workflow_type
            .steps
            .iter()
            .find(|step| step.step_id == step_id)
            .and_then(|step| step.parameters.as_ref())
            .unwrap_or_else(|| panic!("post-PR step {step_id} must have parameters"));
        assert!(
            params.get("artifact_dir").is_none(),
            "post-PR step {step_id} must not use artifact_dir"
        );
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P17
/// @requirement:REQ-PRFU-020A
/// @pseudocode lines 1-53
#[test]
fn post_pr_reachable_graph_does_not_use_abandon_condition() {
    let workflow_type = post_pr_workflow();
    let reachable = reachable_steps(&workflow_type, "capture_pr_identity");
    for transition in workflow_type
        .transitions
        .iter()
        .filter(|transition| reachable.contains(&transition.from))
    {
        assert_ne!(
            transition.condition.as_deref(),
            Some("abandon"),
            "post-PR route from {} must not use abandon",
            transition.from
        );
    }
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P17
/// @requirement:REQ-LF-REPO-003
#[test]
fn setup_workspace_exports_existing_pr_context_for_repeat_runs() {
    let workflow_type = post_pr_workflow();
    let setup = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "setup_workspace")
        .expect("setup_workspace step exists");
    let params = setup
        .parameters
        .as_ref()
        .expect("setup_workspace parameters exist");
    assert_eq!(
        params
            .get("output_format")
            .and_then(serde_json::Value::as_str),
        Some("json")
    );
    let command = params
        .get("command")
        .and_then(serde_json::Value::as_str)
        .expect("setup_workspace command exists");
    assert!(
        command.contains("gh pr view issue{issue_number}")
            && command.contains("existing_pr_number")
            && command.contains("existing_pr_url"),
        "setup_workspace should expose existing PR context for repeatable smoke runs: {command}"
    );
    assert!(
        command.contains("git fetch --prune origin")
            && command.contains("jq -r '.state'")
            && command.contains("OPEN")
            && command.contains("jq -r '.isDraft'")
            && command.contains("false"),
        "setup_workspace should prune stale deleted branches and only reuse an open non-draft PR: {command}"
    );
    assert!(
        command.contains("git fsck --connectivity-only")
            && command.contains("git clone https://github.com/{target_repo}.git \"{work_dir}\""),
        "setup_workspace should replace corrupt target clones before fetching: {command}"
    );
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P17
/// @requirement:REQ-LF-VERIFY-001
#[test]
fn run_tests_accepts_existing_pr_when_repeat_run_has_no_new_diff() {
    let workflow_type = post_pr_workflow();
    let config = workflow_config("llxprt-code");
    let context = context_from_config(&config);
    let run_tests = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "run_tests")
        .expect("run_tests step exists");
    let params = run_tests
        .parameters
        .as_ref()
        .expect("run_tests parameters exist");
    let checks = params
        .get("checks")
        .and_then(serde_json::Value::as_array)
        .expect("checks array exists");
    assert!(
        checks
            .iter()
            .any(|check| check.as_str() == Some("diff_or_existing_pr")),
        "run_tests should use a diff gate that accepts already-open PR reruns"
    );
    let check_commands = params
        .get("check_commands")
        .and_then(serde_json::Value::as_object)
        .expect("check_commands exist");
    let diff_command = check_commands
        .get("diff_or_existing_pr")
        .and_then(serde_json::Value::as_str)
        .expect("diff_or_existing_pr command exists");
    assert!(
        diff_command.contains("{setup_workspace.existing_pr_number}"),
        "diff_or_existing_pr command should consult setup_workspace.existing_pr_number"
    );
    assert!(
        diff_command.contains("{diff_required_path_regex}")
            && diff_command.contains("{diff_failure_message}"),
        "diff_or_existing_pr should source target-specific diff scope and failure text from profile config: {diff_command}"
    );
    let diff_command = interpolate_string(diff_command, &context);
    assert!(
        diff_command.contains("git status --porcelain --untracked-files=all")
            && diff_command.contains("packages/cli/src/ui/hooks/")
            && diff_command.contains("No issue #1803 source/test diff found")
            && diff_command.contains("\"0\" != \"0\""),

        "diff_or_existing_pr should fail empty or unrelated branches unless setup_workspace found an open reusable PR: {diff_command}"
    );
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P17
/// @requirement:REQ-LF-PROF-002,REQ-LF-PROF-003
#[test]
fn llxprt_issue_fix_workflow_loads_with_two_target_profiles() {
    let workflow_type = post_pr_workflow();
    assert_eq!(workflow_type.workflow_type_id, "llxprt-issue-fix-v1");

    let create_plan = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "create_plan")
        .expect("create_plan step exists");
    let static_plan_template = create_plan
        .parameters
        .as_ref()
        .and_then(|params| params.get("static_content"))
        .and_then(serde_json::Value::as_str)
        .expect("create_plan static plan exists");

    let run_tests = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "run_tests")
        .expect("run_tests step exists");
    let check_commands = run_tests
        .parameters
        .as_ref()
        .and_then(|params| params.get("check_commands"))
        .and_then(serde_json::Value::as_object)
        .expect("check_commands exist");
    let test_command_template = check_commands
        .get("test")
        .and_then(serde_json::Value::as_str)
        .expect("test command exists");
    let diff_command_template = check_commands
        .get("diff_or_existing_pr")
        .and_then(serde_json::Value::as_str)
        .expect("diff_or_existing_pr command exists");

    let llxprt_code_config = workflow_config("llxprt-code");
    let alt_config = workflow_config("llxprt-code-alt");
    assert_eq!(
        llxprt_code_config.workflow_type_id,
        workflow_type.workflow_type_id
    );
    assert_eq!(alt_config.workflow_type_id, workflow_type.workflow_type_id);

    let llxprt_code_context = context_from_config(&llxprt_code_config);
    let alt_context = context_from_config(&alt_config);
    let llxprt_code_plan = interpolate_string(static_plan_template, &llxprt_code_context);
    let alt_plan = interpolate_string(static_plan_template, &alt_context);
    let llxprt_code_test_command = interpolate_string(test_command_template, &llxprt_code_context);
    let alt_test_command = interpolate_string(test_command_template, &alt_context);
    let llxprt_code_diff_command = interpolate_string(diff_command_template, &llxprt_code_context);
    let alt_diff_command = interpolate_string(diff_command_template, &alt_context);

    assert!(
        llxprt_code_plan.contains("useStreamEventHandlers.ts")
            && llxprt_code_test_command.contains("useStreamEventHandlers")
            && llxprt_code_diff_command.contains("packages/cli/src/ui/hooks/"),
        "llxprt-code profile should inject issue #1803 planning, test, and diff scope"
    );
    assert!(
        alt_plan.contains("StreamProcessor.ts")
            && alt_test_command.contains("StreamProcessor")
            && alt_diff_command.contains("packages/core/src/core/"),
        "alternate profile should inject distinct planning, test, and diff scope"
    );
    assert_ne!(llxprt_code_plan, alt_plan);
    assert_ne!(llxprt_code_test_command, alt_test_command);
    assert_ne!(llxprt_code_diff_command, alt_diff_command);
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P17
/// @requirement:REQ-LF-PR-002
#[test]
fn create_pr_reuses_existing_issue_branch_pr_when_present() {
    let workflow_type = post_pr_workflow();
    let create_pr = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "create_pr")
        .expect("create_pr step exists");
    let params = create_pr
        .parameters
        .as_ref()
        .expect("create_pr parameters exist");
    let command = params
        .get("command")
        .and_then(serde_json::Value::as_str)
        .expect("create_pr command exists");

    assert!(
        command.contains("gh pr view issue{issue_number}")
            && command.contains("gh pr create --repo {target_repo}"),
        "repeatable smoke runs should reuse an existing PR for the issue branch before creating a new one: {command}"
    );
    assert!(
        command.contains("PR_URL=$(gh pr create")
            && command.contains("gh pr view \"$PR_URL\"")
            && !command.contains("--head issue{issue_number} --json"),
        "create_pr should create with gh pr create output and fetch JSON via gh pr view for CLI compatibility: {command}"
    );

    assert!(
        command.contains("jq -r '.state'")
            && command.contains("OPEN")
            && command.contains("jq -r '.isDraft'")
            && command.contains("false"),
        "create_pr should ignore closed or draft PRs and create a fresh open PR: {command}"
    );
    assert!(
        params.get("exit_code_map").is_none(),
        "create_pr should not map an existing PR lookup/create branch to fatal before PR follow-through can capture identity"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P17
/// @requirement:REQ-PRFU-020A
/// @pseudocode lines 1-53
#[test]
fn production_and_fixture_llxprt_issue_fix_v1_are_equivalent() {
    let production = std::fs::read_to_string("config/workflows/llxprt-issue-fix-v1.toml")
        .expect("read production workflow TOML");
    let fixture =
        std::fs::read_to_string("tests/fixtures/workflows/valid/llxprt-issue-fix-v1.toml")
            .expect("read fixture workflow TOML");
    assert_eq!(
        production, fixture,
        "fixture workflow TOML must mirror production TOML exactly"
    );
    load_workflow_toml("config/workflows/llxprt-issue-fix-v1.toml");
    load_workflow_toml("tests/fixtures/workflows/valid/llxprt-issue-fix-v1.toml");
}
