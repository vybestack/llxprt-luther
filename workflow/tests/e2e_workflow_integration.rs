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
use luther_workflow::persistence::{
    count_events_by_type, load_events, load_latest_event, EventType, SqliteStore,
};
use luther_workflow::workflow::config_loader::{resolve_workflow_config, resolve_workflow_type};
use luther_workflow::workflow::{DiffPathNormalization, TargetPathConfig};

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
    let mut config =
        resolve_workflow_config(config_id, &fixture_root).expect("Failed to load workflow config");
    if config.variables.contains_key("work_dir") {
        config.variables.insert(
            "work_dir".to_string(),
            std::env::temp_dir()
                .join("luther-e2e-workspaces")
                .join(uuid::Uuid::new_v4().to_string())
                .display()
                .to_string(),
        );
    }
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
    registry.register("workflow_auth_preflight", Box::new(executor.clone()));
    registry.register("command_manifest_group", Box::new(executor.clone()));
    for step_type in [
        "failure_cleanup",
        "task_charter",
        "scope_measure",
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
/// THEN: `RunOutcome::Success`, all configured steps visited
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-ISSUE-001,REQ-LF-ISSUE-002,REQ-LF-ISSUE-003,REQ-LF-PR-001
#[test]
fn test_happy_path_all_steps_succeed() {
    let (workflow_type, config) = load_workflow_from_toml("llxprt-issue-fix-v1", "llxprt-code");

    // Count the steps
    assert_eq!(
        workflow_type.steps.len(),
        33,
        "Expected scope-control gates plus workflow auth and PR follow-through steps"
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
    // Successful cleanup must preserve the original failed-work terminal identity
    assert!(
        matches!(result, Ok(RunOutcome::Abandoned { ref step_id, .. }) if step_id == "select_issue"),
        "Expected Abandoned preserving select_issue failure, got {result:?}"
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
        matches!(result, Ok(RunOutcome::Abandoned { ref step_id, .. }) if step_id == "setup_workspace"),
        "Expected Abandoned preserving setup_workspace failure, got {result:?}"
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
        matches!(result, Ok(RunOutcome::Abandoned { ref step_id, .. }) if step_id == "fetch_issue"),
        "Expected Abandoned preserving fetch_issue failure, got {result:?}"
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
        matches!(result, Ok(RunOutcome::Abandoned { ref step_id, .. }) if step_id == "implement"),
        "Expected Abandoned preserving implement failure, got {result:?}"
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
/// THEN: Returns the complete `WorkflowType`, including scope-control gates
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-SEP-003
#[test]
fn test_workflow_type_loads_from_toml() {
    let fixture_root = std::path::PathBuf::from("tests/fixtures");
    let workflow_type = resolve_workflow_type("llxprt-issue-fix-v1", &fixture_root)
        .expect("Failed to load workflow type");

    // Assert base workflow, scope-control gates, and PR follow-through tail steps.
    assert_eq!(workflow_type.steps.len(), 33, "Expected 33 steps");

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
        Some(&"gpt55high".to_string()),
        "Expected profile_planning to be 'gpt55high'"
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
    let (workflow_type, mut config) = load_workflow_from_toml("llxprt-issue-fix-v1", "llxprt-code");

    // Keep persistent ownership markers isolated across concurrent test/coverage
    // processes rather than sharing the fixture's production-like workspace.
    let temp_dir = tempfile::tempdir().expect("temp run directory");
    config.variables.insert(
        "work_dir".to_string(),
        temp_dir.path().join("workspace").display().to_string(),
    );
    let db_path = temp_dir.path().join("checkpoints.db");

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

    // The run registry must capture in-flight lifecycle data through the real
    // engine path, not just the terminal status (issue #50).
    // @plan:PLAN-20260404-INITIAL-RUNTIME.P05
    assert!(
        metadata.process_pid.is_some(),
        "process_pid should be recorded for a daemon-launched run"
    );
    assert!(
        metadata.current_step.is_some(),
        "current_step should be recorded"
    );
    assert!(
        metadata.previous_step.is_some(),
        "previous_step should be recorded after at least one transition"
    );
    assert!(
        metadata.previous_outcome.is_some(),
        "previous_outcome should be recorded after at least one transition"
    );

    // The append-only event history must be queryable and well-ordered:
    // each executed step emits a StepStart before its StepOutcome, and the
    // run finishes with a single TerminalState event.
    let conn = store.conn();
    let events = load_events(conn, &run_id).expect("Failed to load events");
    assert!(
        !events.is_empty(),
        "Event history should not be empty for a completed run"
    );
    assert_eq!(
        events[0].event_type,
        EventType::StepStart.to_string(),
        "First recorded event should be a StepStart"
    );

    let starts =
        count_events_by_type(conn, &run_id, EventType::StepStart).expect("Failed to count starts");
    let outcomes = count_events_by_type(conn, &run_id, EventType::StepOutcome)
        .expect("Failed to count outcomes");
    assert!(
        starts > 0 && starts == outcomes,
        "Each StepStart ({starts}) should pair with a StepOutcome ({outcomes})"
    );

    let terminal_count = count_events_by_type(conn, &run_id, EventType::TerminalState)
        .expect("Failed to count terminal events");
    assert_eq!(
        terminal_count, 1,
        "A completed run should record exactly one TerminalState event"
    );

    let latest = load_latest_event(conn, &run_id)
        .expect("Failed to load latest event")
        .expect("There should be a latest event");
    assert_eq!(
        latest.event_type,
        EventType::TerminalState.to_string(),
        "The final event should be the TerminalState event"
    );

    // Clean up
    let _ = std::fs::remove_file(&db_path);
}

// ============================================================================
// Phase 16: Post-PR workflow graph TDD
// ============================================================================

use luther_workflow::workflow::config_loader::{
    parse_workflow_config_toml, parse_workflow_type_toml, validate_workflow_type,
};
use luther_workflow::workflow::schema::{StepDef, WorkflowConfig, WorkflowType};
use luther_workflow::workflow::validation::validate_workflow_graph;

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
    seed_target_path_context(config, &mut context);
    if let Some(issue_number) = config.variables.get("primary_issue_number") {
        context.set("issue_number", issue_number);
    }
    context.set_current_step_id("setup_workspace");
    context.set("existing_pr_number", "0");
    context.set_current_step_id("run_tests");
    context
}

fn seed_target_path_context(config: &WorkflowConfig, context: &mut StepContext) {
    let target_paths = TargetPathConfig::from_repo_config(&config.repo);
    let repo_root = context.work_dir().clone();
    context.set("repo_root", &repo_root.to_string_lossy());
    context.set(
        "project_subdir",
        &path_value(target_paths.project_subdir.as_deref()),
    );
    context.set(
        "project_dir",
        &target_paths.project_dir(&repo_root).to_string_lossy(),
    );
    context.set(
        "artifact_base_dir",
        &target_paths.artifact_base_dir(&repo_root).to_string_lossy(),
    );
    context.set(
        "diff_path_base",
        &path_value(target_paths.diff_path_base.as_deref()),
    );
    context.set(
        "diff_path_base_dir",
        &target_paths.diff_base_dir(&repo_root).to_string_lossy(),
    );
    context.set(
        "diff_path_normalization",
        match target_paths.diff_path_normalization {
            DiffPathNormalization::RepoRelative => "repo_relative",
            DiffPathNormalization::BaseRelative => "base_relative",
        },
    );
}

fn path_value(path: Option<&std::path::Path>) -> String {
    path.map_or_else(String::new, |path| path.to_string_lossy().into_owned())
}

fn manifest_command_argv(config: &WorkflowConfig, command_id: &str) -> Vec<String> {
    let manifest = config
        .command_manifest
        .as_ref()
        .expect("command manifest exists");
    manifest
        .commands
        .iter()
        .find(|entry| entry.id == command_id)
        .unwrap_or_else(|| panic!("manifest command {command_id} exists"))
        .argv
        .clone()
}

fn manifest_group(config: &WorkflowConfig, group_id: &str) -> Vec<String> {
    config
        .command_manifest
        .as_ref()
        .expect("command manifest exists")
        .groups
        .get(group_id)
        .unwrap_or_else(|| panic!("manifest group {group_id} exists"))
        .clone()
}

fn run_tests_check_names(workflow_type: &WorkflowType) -> Vec<&str> {
    run_tests_step(workflow_type)
        .parameters
        .as_ref()
        .and_then(|params| params.get("checks"))
        .and_then(serde_json::Value::as_array)
        .expect("checks array exists")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect()
}

fn run_tests_step(workflow_type: &WorkflowType) -> &StepDef {
    workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "run_tests")
        .expect("run_tests step exists")
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
/// @pseudocode lines 1-53
fn load_workflow_toml(path: &str) -> WorkflowType {
    let content = std::fs::read_to_string(path).unwrap_or_else(|err| panic!("read {path}: {err}"));
    parse_workflow_type_toml(&content).unwrap_or_else(|err| panic!("parse {path}: {err}"))
}

fn load_workflow_config_toml(path: &str) -> WorkflowConfig {
    let content = std::fs::read_to_string(path).unwrap_or_else(|err| panic!("read {path}: {err}"));
    parse_workflow_config_toml(&content).unwrap_or_else(|err| panic!("parse {path}: {err}"))
}

fn assert_pr_check_policy(watch_params: &serde_json::Value) {
    let policy = watch_params
        .get("check_policy")
        .expect("watch_pr_checks must declare a structured check_policy");
    assert_eq!(
        policy
            .get("allow_unmatched_success")
            .and_then(serde_json::Value::as_bool),
        Some(true),
        "unmatched successful checks may coexist with configured required checks",
    );
    assert_eq!(
        policy
            .get("default_allow_skipped")
            .and_then(serde_json::Value::as_bool),
        Some(true),
        "skipped optional/unmatched checks are allowed by default for target repos",
    );
    assert_eq!(
        policy
            .get("missing_retry_attempts")
            .and_then(serde_json::Value::as_u64),
        Some(12),
        "missing required checks must remain retryable across daemon polls",
    );
    assert_eq!(
        policy
            .get("api_error_retry_attempts")
            .and_then(serde_json::Value::as_u64),
        Some(5),
        "transient GitHub API failures must not terminate on the first poll",
    );
    assert_eq!(
        policy
            .get("poll_interval_seconds")
            .and_then(serde_json::Value::as_u64),
        Some(300),
        "daemon polling should happen in bounded chunks",
    );
    let ignored = policy
        .get("ignored")
        .and_then(serde_json::Value::as_array)
        .expect("check_policy.ignored");
    assert!(
        ignored.iter().any(|entry| {
            entry.get("mode").and_then(serde_json::Value::as_str) == Some("prefix")
                && entry.get("pattern").and_then(serde_json::Value::as_str) == Some("CodeRabbit")
        }),
        "CodeRabbit status checks must be ignored by the PR-check wait policy",
    );
}

fn assert_required_pr_check_prefix(policy: &serde_json::Value, expected_prefix: &str) {
    let required = policy
        .get("required")
        .and_then(serde_json::Value::as_array)
        .expect("check_policy.required");
    assert!(
        required.iter().any(|entry| {
            entry.get("mode").and_then(serde_json::Value::as_str) == Some("prefix")
                && entry.get("pattern").and_then(serde_json::Value::as_str) == Some(expected_prefix)
                && entry
                    .get("allow_skipped")
                    .and_then(serde_json::Value::as_bool)
                    == Some(false)
        }),
        "watch_pr_checks must require {} and disallow skipped required checks",
        expected_prefix,
    );
}

fn assert_required_pr_check(policy: &serde_json::Value, expected_pattern: &str) {
    let required = policy
        .get("required")
        .and_then(serde_json::Value::as_array)
        .expect("check_policy.required");
    assert!(
        required.iter().any(|entry| {
            entry.get("mode").is_none()
                && entry.get("pattern").and_then(serde_json::Value::as_str)
                    == Some(expected_pattern)
                && entry
                    .get("allow_skipped")
                    .and_then(serde_json::Value::as_bool)
                    == Some(false)
        }),
        "watch_pr_checks must require {} and disallow skipped required checks",
        expected_pattern,
    );
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
#[allow(clippy::too_many_lines)]
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
    assert!(
        transition_targets(&workflow_type, "watch_pr_checks", Some("wait")).is_empty(),
        "PR check pending waits should checkpoint for daemon reactivation instead of blocking in-process",
    );
    assert!(
        transition_targets(&workflow_type, "collect_coderabbit_feedback", Some("wait")).is_empty(),
        "CodeRabbit pending waits should checkpoint for daemon reactivation instead of blocking in-process",
    );
    let watch_params = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "watch_pr_checks")
        .and_then(|step| step.parameters.as_ref())
        .expect("watch_pr_checks params");
    assert_eq!(
        watch_params
            .get("max_attempts")
            .and_then(serde_json::Value::as_u64),
        Some(1),
        "PR checks should observe once and suspend rather than hold an active worker",
    );
    let coderabbit_params = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "collect_coderabbit_feedback")
        .and_then(|step| step.parameters.as_ref())
        .expect("collect_coderabbit_feedback params");
    assert_eq!(
        coderabbit_params
            .get("max_observations")
            .and_then(serde_json::Value::as_u64),
        Some(1),
        "CodeRabbit collection should observe once and suspend rather than hold an active worker",
    );
    assert_eq!(
        watch_params
            .get("check_status_result_path")
            .and_then(serde_json::Value::as_str),
        Some("{artifact_dir}/pr-followup/current/pr-check-status.json"),
        "suspended PR check waits must retain artifact-backed poll identity",
    );
    assert_pr_check_policy(watch_params);
    let policy = watch_params
        .get("check_policy")
        .expect("watch_pr_checks must declare a structured check_policy");
    assert_required_pr_check_prefix(policy, "CI");
    assert_eq!(
        coderabbit_params
            .get("coderabbit_feedback_result_path")
            .and_then(serde_json::Value::as_str),
        Some("{artifact_dir}/pr-followup/current/coderabbit-feedback.json"),
        "suspended CodeRabbit waits must retain artifact-backed poll identity",
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
    let validator_params = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "validate_remediation_result")
        .and_then(|step| step.parameters.as_ref())
        .expect("validate_remediation_result params");
    assert_eq!(
        validator_params
            .get("max_stale_artifact_retries")
            .and_then(serde_json::Value::as_u64),
        Some(2),
        "stale pr-remediation-result scope must use a dedicated infrastructure retry budget",
    );
    // The retry-state path is computed by the store from the binding identity,
    // not configured as a dead parameter. This prevents misconfiguration where
    // the configured path could diverge from the store's computed path.
    for step_id in [
        "remediate_pr_followup",
        "validate_remediation_result",
        "post_pr_failure_terminal",
    ] {
        let step = workflow_type
            .steps
            .iter()
            .find(|step| step.step_id == step_id)
            .unwrap_or_else(|| panic!("missing {step_id} step"));
        assert!(
            step.parameters
                .as_ref()
                .is_none_or(|params| { params.get("remediation_retry_state_path").is_none() }),
            "{step_id} must not carry a dead remediation_retry_state_path parameter"
        );
    }
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

/// @plan:issue-136
#[test]
fn daemon_managed_primary_selection_does_not_repeat_claim_mutations() {
    let workflow_type = post_pr_workflow();
    let command = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "select_issue")
        .and_then(|step| step.parameters.as_ref())
        .and_then(|params| params.get("command"))
        .and_then(serde_json::Value::as_str)
        .expect("select_issue command exists");

    assert!(command.contains("if [ \"{daemon_managed_claim}\" != \"true\" ]; then"));
    assert!(command.contains("--add-assignee {assignee} --add-label \"{luther_label}\""));
}

/// @plan:issue-136
#[test]
fn abandonment_cleanup_is_claim_ownership_aware() {
    let workflow_type = post_pr_workflow();
    let command = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "abandon_and_log")
        .and_then(|step| step.parameters.as_ref())
        .and_then(|params| params.get("command"))
        .and_then(serde_json::Value::as_str)
        .expect("abandon command exists");

    assert!(command.contains(
        "[ \"{daemon_managed_claim}\" != \"true\" ] || [ \"{claim_label_added}\" = \"true\" ]"
    ));
    assert!(command.contains(
        "[ \"{daemon_managed_claim}\" != \"true\" ] || [ \"{claim_assignment_added}\" = \"true\" ]"
    ));
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
    let check_names = run_tests_check_names(&workflow_type);
    assert!(
        check_names.contains(&"command_manifest"),
        "run_tests should expand the manifest-backed local check group: {check_names:?}"
    );
    assert!(
        manifest_group(&config, "local").contains(&"format".to_string()),
        "target profile local manifest group should include format"
    );

    let format_argv = manifest_command_argv(&config, "format");
    assert_eq!(
        format_argv,
        ["npm", "run", "format:check"],
        "format should be declared as argv in the command manifest"
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
        llxprt_code_patterns.iter().any(String::is_empty),
        "llxprt-code profile should drive an issue-generic changed-path scope: {llxprt_code_patterns:?}"
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

/// @plan:issue-125
/// Dogfood prompts must steer toward semantic module decomposition and forbid
/// the include!()/split-file gate-evasion pattern that issue #125 removed.
#[test]
fn dogfood_prompts_require_semantic_decomposition() {
    let workflow_type = resolve_workflow_type(
        "llxprt-luther-dogfood-v1",
        &std::path::PathBuf::from("config"),
    )
    .expect("production dogfood workflow type should load");

    let create_plan_prompt = workflow_prompt(&workflow_type, "create_plan");
    assert!(
        create_plan_prompt.contains("semantic decomposition by responsibility"),
        "create_plan prompt should require semantic decomposition: {create_plan_prompt}"
    );
    assert!(
        create_plan_prompt.contains("Do not propose textual source stitching"),
        "create_plan prompt should forbid textual stitching: {create_plan_prompt}"
    );

    let implement_prompt = workflow_prompt(&workflow_type, "implement");
    assert!(
        implement_prompt.contains("Do not use include!() to reassemble Rust source modules"),
        "implement prompt should forbid include!() module assembly: {implement_prompt}"
    );
    assert!(
        implement_prompt.contains("semantic mod submodules"),
        "implement prompt should require cohesive submodules: {implement_prompt}"
    );

    let evaluate_plan_prompt = workflow_prompt(&workflow_type, "evaluate_plan");
    assert!(
        evaluate_plan_prompt.contains("include!() for Rust source module assembly"),
        "evaluate_plan prompt should reject include!() stitching: {evaluate_plan_prompt}"
    );
    assert!(
        evaluate_plan_prompt.contains("part_N, core_N"),
        "evaluate_plan prompt should reject numbered split files: {evaluate_plan_prompt}"
    );

    let remediate_prompt = workflow_prompt(&workflow_type, "remediate");
    assert!(
        remediate_prompt.contains("Do not use include!() to reassemble Rust modules"),
        "remediate prompt should forbid include!() assembly: {remediate_prompt}"
    );
    assert!(
        remediate_prompt.contains("honestly modular"),
        "remediate prompt should require honest modularity: {remediate_prompt}"
    );
}

fn workflow_prompt(
    workflow_type: &luther_workflow::workflow::schema::WorkflowType,
    step_id: &str,
) -> String {
    let step = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == step_id)
        .unwrap_or_else(|| panic!("{step_id} step exists"));
    let params = step
        .parameters
        .as_ref()
        .unwrap_or_else(|| panic!("{step_id} parameters exist"));
    params
        .get("prompt")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("{step_id} prompt exists"))
        .to_string()
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
fn dogfood_plan_gate_blocks_rejected_plan_artifacts() {
    let workflow_type = resolve_workflow_type(
        "llxprt-luther-dogfood-v1",
        &std::path::PathBuf::from("config"),
    )
    .expect("production dogfood workflow type should load");

    let plan_gate = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "plan_gate")
        .expect("plan_gate step exists");
    let command = plan_gate
        .parameters
        .as_ref()
        .and_then(|params| params.get("command"))
        .and_then(serde_json::Value::as_str)
        .expect("plan_gate command exists");

    assert!(
        command.contains("PLAN_NEEDS_REVISION"),
        "plan_gate should inspect plan-evaluation rejection artifacts: {command}"
    );
    assert!(
        command.contains("PLAN_BYTES") && command.contains("-lt 200"),
        "plan_gate should reject obvious placeholder-sized plans: {command}"
    );

    let setup_workspace = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "setup_workspace")
        .expect("setup_workspace step exists");
    let setup_command = setup_workspace
        .parameters
        .as_ref()
        .and_then(|params| params.get("command"))
        .and_then(serde_json::Value::as_str)
        .expect("setup_workspace command exists");
    assert!(
        setup_command.contains("{artifact_dir}/plan-feedback.md"),
        "setup_workspace should clear cross-run plan feedback artifacts: {setup_command}"
    );

    let prepare_plan = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "prepare_plan")
        .expect("prepare_plan step exists");
    let prepare_command = prepare_plan
        .parameters
        .as_ref()
        .and_then(|params| params.get("command"))
        .and_then(serde_json::Value::as_str)
        .expect("prepare_plan command exists");
    assert!(
        prepare_command.contains("plan-feedback.md")
            && prepare_command
                .contains("rm -f {artifact_dir}/plan.md {artifact_dir}/plan-evaluation.md"),
        "prepare_plan should preserve feedback and clear stale plan artifacts: {prepare_command}"
    );

    assert!(workflow_type
        .transitions
        .iter()
        .any(|transition| transition.from == "fetch_issue"
            && transition.to == "prepare_plan"
            && transition.condition.is_none()));
    assert!(workflow_type
        .transitions
        .iter()
        .any(|transition| transition.from == "prepare_plan"
            && transition.to == "create_plan"
            && transition.condition.as_deref() == Some("success")));
    assert!(workflow_type
        .transitions
        .iter()
        .any(|transition| transition.from == "evaluate_plan"
            && transition.to == "plan_gate"
            && transition.condition.as_deref() == Some("success")));
    assert!(workflow_type
        .transitions
        .iter()
        .any(|transition| transition.from == "evaluate_plan"
            && transition.to == "prepare_plan"
            && transition.condition.as_deref() == Some("fixable")));
    assert!(workflow_type
        .transitions
        .iter()
        .any(|transition| transition.from == "plan_gate"
            && transition.to == "workflow_auth_preflight_plan"
            && transition.condition.as_deref() == Some("success")));
    assert!(workflow_type
        .transitions
        .iter()
        .any(|transition| transition.from == "plan_gate"
            && transition.to == "prepare_plan"
            && transition.condition.as_deref() == Some("fixable")));
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
/// Issue #18: the Luther dogfood workflow is a Rust/cargo project, so its
/// verify step must select the `cargo` verification profile rather than relying
/// solely on full `check_commands` overrides for ecosystem-appropriate
/// defaults. Explicit overrides still take precedence; the profile supplies the
/// defaults for any non-overridden check type.
/// @requirement:REQ-LF-VERIFY-007
#[test]
fn dogfood_verify_step_uses_cargo_profile() {
    let workflow_type = resolve_workflow_type(
        "llxprt-luther-dogfood-v1",
        &std::path::PathBuf::from("config"),
    )
    .expect("production dogfood workflow type should load");

    let run_tests = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "run_tests")
        .expect("dogfood run_tests verify step exists");
    let params = run_tests
        .parameters
        .as_ref()
        .expect("run_tests parameters exist");

    assert_eq!(
        params.get("profile").and_then(serde_json::Value::as_str),
        Some("cargo"),
        "dogfood verify step must select the cargo verification profile so non-npm defaults apply"
    );
}

/// Issue #75: local project verification for Luther issue fixes includes the
/// Luther-owned OCR wrapper so the same review contract is exercised before
/// push without embedding raw profile-provided shell snippets in the workflow.
#[test]
fn reusable_issue_fix_run_tests_includes_profile_driven_ocr_review() {
    let workflow_type = post_pr_workflow();
    let config = workflow_config("llxprt-luther-issue-fix");
    let check_names = run_tests_check_names(&workflow_type);
    assert!(
        check_names.contains(&"command_manifest"),
        "issue-fix verification should expand the local manifest group: {check_names:?}"
    );
    assert!(
        manifest_group(&config, "local").contains(&"ocr_review".to_string()),
        "issue-fix target profile should include OCR review in its local manifest group"
    );

    let command = manifest_command_argv(&config, "ocr_review");
    assert_eq!(
        command,
        ["cargo", "xtask", "ocr-review", "--preview"],
        "llxprt-luther issue-fix profile should run the local OCR preview wrapper through argv"
    );
    assert_eq!(
        config.repo.project_subdir.as_deref(),
        Some("workflow"),
        "Luther OCR review should inherit the profile default project cwd so the cargo xtask alias is available"
    );
}

/// Issue #78: relocating GitHub Actions workflows to the repository root must
/// still satisfy Luther's own issue-fix diff gate and changed-path scope.
#[test]
fn reusable_issue_fix_scope_accepts_root_github_workflows() {
    let workflow_type = post_pr_workflow();
    let config = workflow_config("llxprt-luther-issue-fix");
    let context = context_from_config(&config);

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
    let interpolated_patterns = patterns
        .iter()
        .filter_map(serde_json::Value::as_str)
        .map(|pattern| interpolate_string(pattern, &context))
        .collect::<Vec<_>>();
    assert_eq!(interpolated_patterns, ["workflow"]);
    assert!(
        ".github/workflows/pr-quality.yml".contains(&interpolated_patterns[0]),
        "root workflow relocations should satisfy the issue-fix changed-path gate through their path segment"
    );

    let run_tests = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "run_tests")
        .expect("run_tests step exists");
    let diff_gate = run_tests
        .parameters
        .as_ref()
        .and_then(|params| params.get("diff_or_existing_pr"))
        .expect("diff_or_existing_pr parameters exist");
    assert_eq!(
        diff_gate
            .get("required_path_regex")
            .and_then(serde_json::Value::as_str),
        Some("{diff_required_path_regex}")
    );
    assert!(
        interpolate_string(
            diff_gate
                .get("required_path_regex")
                .and_then(serde_json::Value::as_str)
                .expect("diff regex exists"),
            &context,
        )
        .contains("workflow/(src/|config/|tests/|docs/)"),
        "issue-fix diff gate should accept Luther source/config/test/doc changes"
    );
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
                && transition.to == "prepare_plan"
                && transition.condition.as_deref() == Some("fixable")
        })
        .expect("create_plan fixable transition should prepare a fresh retry");
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
    assert!(
        llxprt_code_static_plan.trim().is_empty(),
        "generic llxprt-code profile should let the planning agent read the selected issue instead of replaying an issue-specific static plan: {llxprt_code_static_plan}"
    );

    let alt_config = workflow_config("llxprt-code-alt");
    let alt_context = context_from_config(&alt_config);
    let alt_static_plan = interpolate_string(static_plan, &alt_context);
    assert!(
        alt_static_plan.contains("StreamProcessor.ts")
            && alt_static_plan.contains("cancellation state"),
        "alternate profile can still provide target-specific static plan content: {alt_static_plan}"
    );
    assert_ne!(
        llxprt_code_static_plan, alt_static_plan,
        "blank generic planning and explicit target-specific planning should remain distinguishable"
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

#[test]
fn luther_profile_uses_nested_project_paths_without_manifest_path_prefixes() {
    let config = workflow_config("llxprt-luther");
    assert_eq!(config.repo.project_subdir.as_deref(), Some("workflow"));
    assert_eq!(config.repo.artifact_path_base.as_deref(), Some("."));
    assert_eq!(config.repo.diff_path_base.as_deref(), Some("workflow"));
    assert_eq!(
        config.repo.diff_path_normalization,
        DiffPathNormalization::RepoRelative
    );

    for command_id in ["format", "check", "build", "test", "clippy"] {
        let argv = manifest_command_argv(&config, command_id);
        assert!(
            !argv.iter().any(|arg| arg == "workflow/Cargo.toml"),
            "{command_id} should run from project_subdir rather than embedding manifest path prefixes: {argv:?}"
        );
    }
    assert!(
        manifest_command_argv(&config, "ocr_review")
            .iter()
            .any(|arg| arg == "xtask"),
        "ocr review should run from the default project cwd"
    );
}

#[test]
fn luther_profile_keeps_shared_path_values_as_daemon_bases() {
    let config = workflow_config("llxprt-luther");
    assert_eq!(
        config.variables.get("work_dir").map(String::as_str),
        Some("/tmp/luther-workspaces/llxprt-luther")
    );
    assert_eq!(
        config.variables.get("artifact_dir").map(String::as_str),
        Some("/tmp/luther-artifacts/llxprt-luther")
    );
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P17
/// @requirement:REQ-LF-VERIFY-002
#[test]
fn run_tests_mirrors_ci_lint_typecheck_and_build_before_push() {
    let workflow_type = post_pr_workflow();
    let config = workflow_config("llxprt-code");
    let check_names = run_tests_check_names(&workflow_type);

    assert!(
        check_names.contains(&"command_manifest"),
        "run_tests should expand the target profile local manifest group: {check_names:?}"
    );

    let local_group = manifest_group(&config, "local");
    for expected in ["lint", "typecheck", "build", "test", "format"] {
        assert!(
            local_group.contains(&expected.to_string()),
            "target profile local manifest group should include {expected}: {local_group:?}"
        );
    }

    assert_eq!(
        manifest_command_argv(&config, "lint"),
        ["npm", "run", "lint"],
        "pre-PR verification should run lint from the command manifest"
    );
    assert_eq!(
        manifest_command_argv(&config, "typecheck"),
        ["npm", "run", "typecheck"],
        "pre-PR verification should run workspace type checking"
    );
    assert_eq!(
        manifest_command_argv(&config, "build"),
        ["npm", "run", "build"],
        "pre-PR verification should run the full build rather than only core"
    );
    assert_eq!(
        manifest_command_argv(&config, "ocr_review"),
        [
            "echo",
            "Local OCR wrapper is not configured for this target profile"
        ],
        "workflow type should source local OCR review command from the manifest"
    );
    assert!(
        config
            .command_manifest
            .as_ref()
            .expect("command manifest exists")
            .commands
            .iter()
            .all(|entry| entry.id != "build_core"),
        "run_tests should not use the narrow core-only build gate"
    );
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P17
/// @requirement:REQ-LF-IMPLEMENT-001,REQ-LF-VERIFY-003
#[test]
fn dogfood_remediation_waits_for_agent_or_diff_instead_of_idle_abandoning() {
    let workflow_type = resolve_workflow_type(
        "llxprt-luther-dogfood-v1",
        &std::path::PathBuf::from("config"),
    )
    .expect("production dogfood workflow type should load");
    let remediate = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "remediate")
        .expect("dogfood remediate step exists");
    let params = remediate
        .parameters
        .as_ref()
        .expect("remediate params exist");

    assert_eq!(
        params.get("success_on_diff"),
        Some(&serde_json::json!(true))
    );
    assert_eq!(
        params.get("early_success_on_diff"),
        Some(&serde_json::json!(false))
    );
    assert_eq!(params.get("min_runtime_before_success_seconds"), None);
    assert_eq!(params.get("max_runtime_after_required_diff_seconds"), None);
    assert_eq!(
        params.get("idle_timeout_seconds"),
        Some(&serde_json::json!(1800))
    );
    assert_eq!(
        params.get("timeout_seconds"),
        Some(&serde_json::json!(3600))
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

#[test]
fn push_changes_only_keeps_global_luther_runtime_staging_exclusion() {
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
        command.contains("':!.luther'"),
        "push_changes must exclude .luther directory: {command}"
    );
    assert!(
        command.contains("':!.luther/**'"),
        "push_changes must exclude .luther/** contents: {command}"
    );
    assert!(
        command.contains(r#"[ "{target_has_commit_restore_paths}" = "1" ]"#),
        "target-specific generated-file restore guard should use a boolean flag, not shell-quoted path words: {command}"
    );
    assert!(
        command.contains("git restore --worktree -- {target_commit_restore_paths}"),
        "target-specific generated-file restores should be injected from target config, not hardcoded: {command}"
    );
    assert!(
        command.contains("{target_commit_exclude_pathspecs}"),
        "target-specific generated-file exclusions should be injected from target config, not hardcoded: {command}"
    );
    assert!(
        !command.contains("NOTICES.txt"),
        "target-specific generated-file exclusions belong in target config, not shared staging shell: {command}"
    );
}

#[test]
fn llxprt_code_target_config_injects_generated_file_staging_exclusion() {
    let config = workflow_config("llxprt-code");
    assert_eq!(
        config
            .variables
            .get("target_commit_restore_paths")
            .map(String::as_str),
        Some("'packages/vscode-ide-companion/NOTICES.txt'")
    );
    assert_eq!(
        config
            .variables
            .get("target_has_commit_restore_paths")
            .map(String::as_str),
        Some("1")
    );
    assert_eq!(
        config
            .variables
            .get("target_commit_exclude_pathspecs")
            .map(String::as_str),
        Some("':!packages/vscode-ide-companion/NOTICES.txt'")
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

/// @plan:PLAN-20260408-LLXPRT-FIRST.P17
/// @requirement:REQ-LF-EXEC-001
#[test]
fn implement_step_selects_explicit_change_detection_mode() {
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
            .get("change_detection_mode")
            .and_then(serde_json::Value::as_str),
        Some("include_untracked"),
        "implement creates new files, so changed-path detection must explicitly include untracked entries rather than relying on an implicit default"
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
    let diff_gate = params
        .get("diff_or_existing_pr")
        .expect("diff_or_existing_pr parameters exist");
    assert_eq!(
        diff_gate
            .get("existing_pr_number")
            .and_then(serde_json::Value::as_str),
        Some("{setup_workspace.existing_pr_number}")
    );
    assert_eq!(
        diff_gate
            .get("required_path_regex")
            .and_then(serde_json::Value::as_str),
        Some("{diff_required_path_regex}")
    );
    let failure_message = interpolate_string(
        diff_gate
            .get("failure_message")
            .and_then(serde_json::Value::as_str)
            .expect("failure_message exists"),
        &context,
    );
    assert!(
        failure_message.contains("No issue #0 source/test diff found"),
        "diff_or_existing_pr should fail empty branches unless setup_workspace found an open reusable PR while allowing any issue-specific changed path: {failure_message}"
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

    let run_tests = run_tests_step(&workflow_type);
    let diff_gate = run_tests
        .parameters
        .as_ref()
        .and_then(|params| params.get("diff_or_existing_pr"))
        .expect("diff_or_existing_pr parameters exist");
    let diff_regex_template = diff_gate
        .get("required_path_regex")
        .and_then(serde_json::Value::as_str)
        .expect("diff regex exists");

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
    let llxprt_code_test_argv = manifest_command_argv(&llxprt_code_config, "test");
    let alt_test_argv = manifest_command_argv(&alt_config, "test");
    let llxprt_code_diff_regex = interpolate_string(diff_regex_template, &llxprt_code_context);
    let alt_diff_regex = interpolate_string(diff_regex_template, &alt_context);

    assert!(
        llxprt_code_plan.trim().is_empty()
            && llxprt_code_test_argv
                == ["npm", "run", "test", "--workspace", "@vybestack/llxprt-code"]
            && llxprt_code_diff_regex == ".",
        "generic llxprt-code profile should let planning follow the selected issue while running the package test command and allowing any changed path"
    );
    assert!(
        alt_plan.contains("StreamProcessor.ts")
            && alt_test_argv.iter().any(|arg| arg == "StreamProcessor")
            && alt_diff_regex.contains("packages/core/src/core/"),
        "alternate profile should inject distinct planning, test, and diff scope"
    );
    assert_ne!(llxprt_code_plan, alt_plan);

    assert_ne!(llxprt_code_test_argv, alt_test_argv);
    assert_ne!(llxprt_code_diff_regex, alt_diff_regex);
}

#[test]
fn llxprt_jefe_runs_custom_gates_from_manifest_without_workflow_edits() {
    let workflow_type = post_pr_workflow();
    let jefe_config = workflow_config("llxprt-jefe");

    let check_names = run_tests_check_names(&workflow_type);
    assert_eq!(
        check_names,
        [
            "diff_or_existing_pr",
            "command_manifest",
            "diff_or_existing_pr"
        ],
        "generic workflow should delegate repo-specific gates to the target manifest"
    );

    let local_group = manifest_group(&jefe_config, "local");
    assert!(
        local_group.contains(&"coverage".to_string())
            && local_group.contains(&"source_length".to_string()),
        "llxprt-jefe should add custom coverage and source-size gates through config only: {local_group:?}"
    );
    assert_eq!(
        manifest_command_argv(&jefe_config, "coverage"),
        [
            "cargo",
            "llvm-cov",
            "--workspace",
            "--all-features",
            "--locked",
            "--fail-under-lines",
            "80",
        ],
        "coverage gate should be fully declared as argv in the target config"
    );
}

#[test]
fn target_profiles_cover_current_and_skeleton_repositories() {
    for config_id in ["llxprt-luther", "llxprt-code", "llxprt-jefe", "codepuppy"] {
        let config = workflow_config(config_id);
        let profile = config
            .target_profile
            .as_ref()
            .expect("target profile present");
        assert!(
            profile.command_groups.contains_key("local")
                && profile.command_groups.contains_key("post_pr")
                && config.command_manifest.is_some(),
            "{config_id} should use shared target profile command groups"
        );
        assert!(
            config.variables.contains_key("target_repo")
                && config.variables.contains_key("diff_required_path_regex")
                && config
                    .variables
                    .contains_key("target_guidance_implementation"),
            "{config_id} should derive legacy prompt/diff variables from target_profile"
        );
    }
}

#[test]
fn shared_workflow_bootstrap_is_manifest_based_and_setup_is_generic() {
    let workflow_type = load_workflow_toml("config/workflows/llxprt-issue-fix-v1.toml");
    let setup = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "setup_workspace")
        .expect("setup_workspace step exists");
    let setup_command = setup
        .parameters
        .as_ref()
        .and_then(|params| params.get("command"))
        .and_then(serde_json::Value::as_str)
        .expect("setup_workspace command exists");
    assert!(!setup_command.contains("npm ci"));
    assert!(!setup_command.contains("node_modules"));

    let bootstrap = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "bootstrap_workspace")
        .expect("bootstrap_workspace step exists");
    assert_eq!(bootstrap.step_type, "command_manifest_group");
    let bootstrap_params = bootstrap
        .parameters
        .as_ref()
        .expect("bootstrap_workspace parameters");
    assert_eq!(
        bootstrap_params
            .get("command_manifest_group")
            .and_then(serde_json::Value::as_str),
        Some("{target_bootstrap_command_group}")
    );
    assert_eq!(
        bootstrap_params
            .get("allow_empty_group")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );

    let config = workflow_config("llxprt-code");
    let resolved_group = config
        .variables
        .get("target_bootstrap_command_group")
        .map(String::as_str)
        .expect("target_bootstrap_command_group variable");
    let manifest = config.command_manifest.expect("command manifest");
    assert!(
        manifest.groups.contains_key(resolved_group),
        "resolved bootstrap group '{resolved_group}' must exist in command manifest groups"
    );
}

#[test]
fn llxprt_code_bootstrap_uses_structured_npm_manifest_commands() {
    let config = workflow_config("llxprt-code");
    assert_eq!(
        config
            .variables
            .get("target_bootstrap_command_group")
            .map(String::as_str),
        Some("bootstrap")
    );
    let manifest = config.command_manifest.expect("command manifest");
    let missing_install = manifest
        .commands
        .iter()
        .find(|entry| entry.id == "install_dependencies_when_missing")
        .expect("install_dependencies_when_missing command");
    assert_eq!(missing_install.argv, ["npm", "ci", "--ignore-scripts"]);
    assert_eq!(missing_install.run_if_missing_any, ["node_modules"]);
    assert!(missing_install.run_if_present_all.is_empty());
    assert!(missing_install.remove_before_run.is_empty());

    let incomplete_install = manifest
        .commands
        .iter()
        .find(|entry| entry.id == "reinstall_dependencies_when_incomplete")
        .expect("reinstall_dependencies_when_incomplete command");
    assert_eq!(incomplete_install.argv, ["npm", "ci", "--ignore-scripts"]);
    assert_eq!(incomplete_install.run_if_present_all, ["node_modules"]);
    assert!(incomplete_install
        .run_if_missing_any
        .iter()
        .any(|path| path == "node_modules/esbuild/index.js"));
    assert_eq!(incomplete_install.remove_before_run, ["node_modules"]);
    assert_eq!(
        manifest.groups.get("bootstrap"),
        Some(&vec![
            "install_dependencies_when_missing".to_string(),
            "reinstall_dependencies_when_incomplete".to_string(),
            "repair_node_bin_permissions".to_string()
        ])
    );
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

#[test]
fn parent_issue_orchestrator_workflow_loads_with_target_configs() {
    let production = load_workflow_toml("config/workflows/parent-issue-orchestrator-v1.toml");
    let fixture =
        load_workflow_toml("tests/fixtures/workflows/valid/parent-issue-orchestrator-v1.toml");
    assert_eq!(production.workflow_type_id, "parent-issue-orchestrator-v1");
    assert_eq!(production.steps.len(), 12);
    assert_eq!(fixture.steps.len(), production.steps.len());
    assert!(production
        .steps
        .iter()
        .all(|step| step.step_type == "parent_orchestration"));
    assert!(production.transitions.iter().any(|transition| {
        transition.from == "evaluate_parent_completion"
            && transition.to == "close_or_report_parent"
            && transition.condition.as_deref() == Some("fixable")
    }));
    validate_workflow_graph(&production)
        .expect("parent orchestrator production workflow graph validates");
    validate_workflow_type(&production).expect("parent orchestrator production workflow validates");

    for config_id in ["parent-orchestrator-luther", "parent-orchestrator-code"] {
        let fixture_config = workflow_config(config_id);
        let production_config =
            load_workflow_config_toml(&format!("config/workflow-configs/{config_id}.toml"));
        assert_eq!(fixture_config.config_id, config_id);
        assert_eq!(production_config.config_id, config_id);
        assert_eq!(
            production_config.workflow_type_id,
            "parent-issue-orchestrator-v1"
        );
        assert_eq!(
            production_config.workflow_type_id,
            fixture_config.workflow_type_id
        );
        assert_eq!(
            production_config.parent_orchestration,
            fixture_config.parent_orchestration
        );
        assert!(!production_config.parent_orchestration.auto_merge_children);
        assert!(production_config.parent_orchestration.wait_for_human_merge);
        assert_eq!(
            production_config
                .parent_orchestration
                .merge_poll_interval_seconds,
            300
        );
        assert_eq!(
            production_config
                .parent_orchestration
                .max_child_merge_wait_seconds,
            None
        );
        let expected_child_config = if config_id == "parent-orchestrator-luther" {
            "llxprt-luther"
        } else {
            "llxprt-code"
        };
        assert_eq!(
            production_config.parent_orchestration.child_config_id,
            expected_child_config
        );
    }
}

#[test]
fn production_and_fixture_llxprt_luther_configs_are_equivalent() {
    let production_path = "config/workflow-configs/llxprt-luther.toml";
    let fixture_path = "tests/fixtures/workflow-configs/valid/llxprt-luther.toml";
    let production =
        std::fs::read_to_string(production_path).expect("read production luther config TOML");
    let fixture = std::fs::read_to_string(fixture_path).expect("read fixture luther config TOML");
    assert_eq!(
        production, fixture,
        "fixture llxprt-luther config must track production daemon path-base config"
    );
    load_workflow_config_toml(production_path);
    load_workflow_config_toml(fixture_path);
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

#[test]
fn dogfood_scope_control_dominates_mutation_and_push() {
    let workflow = load_workflow_toml("config/workflows/llxprt-luther-dogfood-v1.toml");
    for (step_id, step_type) in [
        ("task_charter", "task_charter"),
        ("scope_measure", "scope_measure"),
        ("scope_measure_pre_push", "scope_measure"),
    ] {
        let step = workflow
            .steps
            .iter()
            .find(|step| step.step_id == step_id)
            .unwrap_or_else(|| panic!("missing {step_id} step"));
        assert_eq!(step.step_type, step_type);
        assert_eq!(
            step.parameters
                .as_ref()
                .and_then(|params| params.get("artifact_dir"))
                .and_then(serde_json::Value::as_str),
            Some("{artifact_dir}")
        );
    }
    for (from, to, condition) in [
        ("setup_workspace", "task_charter", Some("success")),
        ("task_charter", "route_pr_path", Some("success")),
        ("workflow_auth_preflight_plan", "implement", Some("success")),
        ("implement", "scope_measure", Some("success")),
        ("scope_measure", "run_tests", Some("success")),
        (
            "workflow_auth_preflight_pre_push",
            "scope_measure_pre_push",
            Some("success"),
        ),
        ("scope_measure_pre_push", "push_changes", Some("success")),
    ] {
        assert!(workflow.transitions.iter().any(|transition| {
            transition.from == from
                && transition.to == to
                && transition.condition.as_deref() == condition
        }));
    }
    for step_id in ["task_charter", "scope_measure", "scope_measure_pre_push"] {
        let fatal = workflow
            .transitions
            .iter()
            .find(|transition| {
                transition.from == step_id
                    && transition.to == "abandon_and_log"
                    && transition.condition.as_deref() == Some("fatal")
            })
            .unwrap_or_else(|| panic!("missing fatal route for {step_id}"));
        assert_eq!(fatal.max_iterations, Some(1));
    }
    for (from, to, iterations) in [
        ("task_charter", "route_pr_path", 1),
        ("scope_measure", "run_tests", 2),
        (
            "workflow_auth_preflight_pre_push",
            "scope_measure_pre_push",
            2,
        ),
        ("scope_measure_pre_push", "push_changes", 2),
    ] {
        let transition = workflow
            .transitions
            .iter()
            .find(|transition| {
                transition.from == from
                    && transition.to == to
                    && transition.condition.as_deref() == Some("success")
            })
            .unwrap_or_else(|| panic!("missing {from} to {to}"));
        assert_eq!(transition.max_iterations, Some(iterations));
    }
}

#[test]
fn production_and_fixture_llxprt_luther_dogfood_are_equivalent() {
    let production = std::fs::read_to_string("config/workflows/llxprt-luther-dogfood-v1.toml")
        .expect("read production dogfood workflow TOML");
    let fixture =
        std::fs::read_to_string("tests/fixtures/workflows/valid/llxprt-luther-dogfood-v1.toml")
            .expect("read fixture dogfood workflow TOML");
    assert_eq!(
        production, fixture,
        "fixture dogfood workflow TOML must mirror production TOML exactly"
    );

    let production =
        toml::from_str::<toml::Value>(&production).expect("parse production dogfood workflow TOML");
    let production = serde_json::to_value(production).expect("serialize production workflow");
    let fixture_json =
        std::fs::read_to_string("tests/fixtures/workflows/valid/llxprt-luther-dogfood-v1.json")
            .expect("read fixture dogfood workflow JSON");
    let fixture_json = serde_json::from_str::<serde_json::Value>(&fixture_json)
        .expect("parse fixture dogfood workflow JSON");
    assert_eq!(production, fixture_json);
}

#[test]
fn remediation_retry_maxima_are_authoritative_and_consistent_in_shipped_workflows() {
    let fixture_json =
        std::fs::read_to_string("tests/fixtures/workflows/valid/llxprt-issue-fix-v1.json")
            .expect("read fixture workflow JSON");
    let fixture_json = luther_workflow::workflow::parse_workflow_type_json(&fixture_json)
        .expect("parse fixture workflow JSON");
    let workflows = [
        load_workflow_toml("config/workflows/llxprt-issue-fix-v1.toml"),
        load_workflow_toml("config/workflows/llxprt-luther-dogfood-v1.toml"),
        load_workflow_toml("tests/fixtures/workflows/valid/llxprt-issue-fix-v1.toml"),
        fixture_json,
    ];

    for workflow in &workflows {
        let validation_parameters = workflow
            .steps
            .iter()
            .find(|step| step.step_id == "validate_remediation_result")
            .and_then(|step| step.parameters.as_ref())
            .expect("shipped workflow validation parameters");
        assert_eq!(
            validation_parameters
                .get("remediation_step_order_index")
                .and_then(serde_json::Value::as_u64),
            Some(9),
            "{} must preserve remediation receipt provenance at step 9",
            workflow.workflow_type_id
        );
        for step_id in [
            "remediate_pr_followup",
            "validate_remediation_result",
            "post_pr_failure_terminal",
        ] {
            let parameters = workflow
                .steps
                .iter()
                .find(|step| step.step_id == step_id)
                .and_then(|step| step.parameters.as_ref())
                .unwrap_or_else(|| {
                    panic!(
                        "{} is missing {step_id} parameters",
                        workflow.workflow_type_id
                    )
                });
            for maximum in [
                "max_remediation_attempts",
                "max_validation_retries",
                "max_stale_artifact_retries",
            ] {
                assert_eq!(
                    parameters.get(maximum).and_then(serde_json::Value::as_u64),
                    Some(2),
                    "{} {step_id} must declare authoritative {maximum}",
                    workflow.workflow_type_id,
                );
            }
        }
    }
}

// ============================================================================
// Issue #12: Loop limits and terminal routing as a workflow-level contract
// ============================================================================

#[test]
fn dogfood_pr_check_wait_policy_targets_luther_pr_quality_checks() {
    let issue_fix = load_workflow_toml("config/workflows/llxprt-issue-fix-v1.toml");
    let dogfood = load_workflow_toml("config/workflows/llxprt-luther-dogfood-v1.toml");
    let issue_policy = watch_pr_check_policy(&issue_fix);
    let dogfood_policy = watch_pr_check_policy(&dogfood);

    assert_pr_check_policy(watch_pr_check_params(&issue_fix));
    assert_pr_check_policy(watch_pr_check_params(&dogfood));
    assert_eq!(
        policy_without_required(issue_policy),
        policy_without_required(dogfood_policy),
        "dogfood and issue-fix workflows must keep the same durable PR check wait defaults",
    );
    assert_required_pr_check_prefix(issue_policy, "CI");
    for required_check in [
        "Tests (lib + integration)",
        "Format (rustfmt)",
        "Lint (clippy + structural)",
        "Release readiness (release build)",
        "Coverage (llvm-cov gate)",
        "Docs (cargo doc)",
        "Security (cargo audit)",
        "OpenCodeReview",
    ] {
        assert_required_pr_check(dogfood_policy, required_check);
    }
}

fn watch_pr_check_params(workflow_type: &WorkflowType) -> &serde_json::Value {
    workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == "watch_pr_checks")
        .and_then(|step| step.parameters.as_ref())
        .expect("watch_pr_checks params")
}

fn watch_pr_check_policy(workflow_type: &WorkflowType) -> &serde_json::Value {
    watch_pr_check_params(workflow_type)
        .get("check_policy")
        .expect("watch_pr_checks check_policy")
}

fn policy_without_required(policy: &serde_json::Value) -> serde_json::Value {
    let mut policy = policy.clone();
    if let Some(object) = policy.as_object_mut() {
        object.remove("required");
    }
    policy
}

/// Issue #12: the shipped llxprt-issue-fix-v1 workflow must satisfy the new
/// loop-limit, terminal-routing, and PR-remediation-cap invariants enforced by
/// the graph validator, so the production workflow still loads after the rules
/// are added.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
#[test]
fn production_workflow_satisfies_loop_terminal_remediation_contract() {
    let workflow_type = post_pr_workflow();
    validate_workflow_graph(&workflow_type)
        .expect("production workflow must satisfy the loop/terminal/remediation contract");
    validate_workflow_type(&workflow_type).expect("production workflow must pass full validation");
}

/// Issue #12: every loop-back transition in the production workflow declares an
/// explicit `max_iterations` cap (loop-backs must not silently fall back to the
/// global default).
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
#[test]
fn production_workflow_loop_backs_declare_explicit_caps() {
    let workflow_type = post_pr_workflow();
    let index_of: HashMap<&str, usize> = workflow_type
        .steps
        .iter()
        .enumerate()
        .map(|(idx, step)| (step.step_id.as_str(), idx))
        .collect();

    for transition in &workflow_type.transitions {
        let (Some(&from_idx), Some(&to_idx)) = (
            index_of.get(transition.from.as_str()),
            index_of.get(transition.to.as_str()),
        ) else {
            continue;
        };
        if to_idx <= from_idx {
            assert!(
                transition.max_iterations.is_some(),
                "loop-back {} --{}--> {} must declare an explicit max_iterations",
                transition.from,
                effective_condition(transition.condition.as_deref()),
                transition.to,
            );
        }
    }
}

/// Issue #12: the PR remediation iteration guard declares a positive
/// `max_post_pr_remediation_iterations` cap, validated before execution.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
#[test]
fn production_workflow_pr_iteration_guard_has_positive_cap() {
    let workflow_type = post_pr_workflow();
    let guard = workflow_type
        .steps
        .iter()
        .find(|step| step.step_type == "post_pr_iteration_guard")
        .expect("production workflow must declare a post_pr_iteration_guard step");
    let cap = guard
        .parameters
        .as_ref()
        .and_then(|params| params.get("max_post_pr_remediation_iterations"))
        .and_then(serde_json::Value::as_u64);
    assert!(
        matches!(cap, Some(value) if value > 0),
        "post_pr_iteration_guard must declare a positive max_post_pr_remediation_iterations, got {cap:?}"
    );
}
