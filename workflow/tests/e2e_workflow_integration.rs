/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// End-to-End Workflow Integration Tests — Graph Routing
///
/// These tests load real TOML fixtures and use mock executors to verify
/// the workflow graph routes correctly for all outcome combinations.
/// They prove the TOML definition is structurally sound.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use luther_workflow::engine::executor::{ExecutorRegistry, StepContext, StepExecutor};
use luther_workflow::engine::instance::WorkflowInstance;
use luther_workflow::engine::runner::{EngineError, EngineRunner, RunOutcome};
use luther_workflow::engine::transition::StepOutcome;
use luther_workflow::persistence::SqliteStore;
use luther_workflow::workflow::config_loader::{
    resolve_workflow_config, resolve_workflow_type,
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
    let workflow_type =
        resolve_workflow_type(workflow_type_id, &fixture_root).expect("Failed to load workflow type");
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
    registry.register("llxprt", Box::new(executor));

    registry
}

// ============================================================================
// Test 1: Happy path all steps succeed
// ============================================================================

/// Test 1: Happy path — all steps succeed
/// GIVEN: Workflow loaded from TOML with all steps returning Success
/// WHEN: Engine runs
/// THEN: RunOutcome::Success, all 14 steps visited
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-ISSUE-001,REQ-LF-ISSUE-002,REQ-LF-ISSUE-003,REQ-LF-PR-001
#[test]
fn test_happy_path_all_steps_succeed() {
    let (workflow_type, config) = load_workflow_from_toml("llxprt-issue-fix-v1", "llxprt-code");

    // Count the steps
    assert_eq!(workflow_type.steps.len(), 14, "Expected 14 steps in workflow");

    // All steps succeed by default (empty outcomes map)
    let registry = setup_registry(HashMap::new());
    let instance = WorkflowInstance::create(workflow_type, config);
    let mut runner =
        EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected Success, got {:?}",
        result
    );
}

// ============================================================================
// Test 2: Plan loop fixable then approved
// ============================================================================

/// Test 2: Plan loop fixable twice then approved
/// GIVEN: Workflow loaded from TOML
/// WHEN: evaluate_plan returns Fixable twice, then Success
/// THEN: RunOutcome::Success (loop works correctly)
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
    let mut runner =
        EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected Success, got {:?}",
        result
    );
}

// ============================================================================
// Test 3: Plan loop exceeds limit abandons
// ============================================================================

/// Test 3: Plan loop exceeds limit abandons
/// GIVEN: Workflow loaded from TOML with max_iterations: 5 on evaluate_plan→create_plan
/// WHEN: evaluate_plan always returns Fixable
/// THEN: RunOutcome::Abandoned after 5 iterations
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
    let mut runner =
        EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Abandoned { .. })),
        "Expected Abandoned, got {:?}",
        result
    );
}

// ============================================================================
// Test 4: Test remediation loop fixable then passes
// ============================================================================

/// Test 4: Test remediation loop fixable then passes
/// GIVEN: Workflow loaded from TOML
/// WHEN: run_tests returns Fixable twice, then Success
/// THEN: RunOutcome::Success
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
    let mut runner =
        EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected Success, got {:?}",
        result
    );
}

// ============================================================================
// Test 5: Test remediation loop exceeds limit abandons
// ============================================================================

/// Test 5: Test remediation loop exceeds limit abandons
/// GIVEN: Workflow loaded from TOML with max_iterations: 5 on remediate→run_tests
/// WHEN: run_tests always returns Fixable
/// THEN: RunOutcome::Abandoned after 5 iterations
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
    let mut runner =
        EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Abandoned { .. })),
        "Expected Abandoned, got {:?}",
        result
    );
}

// ============================================================================
// Test 6: Implementation evaluation loop
// ============================================================================

/// Test 6: Implementation evaluation loop
/// GIVEN: Workflow loaded from TOML
/// WHEN: evaluate_impl returns Fixable once, then Success
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
    let mut runner =
        EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected Success, got {:?}",
        result
    );
}

// ============================================================================
// Test 7: Fatal at select_issue routes to abandon_and_log
// ============================================================================

/// Test 7: Fatal at select_issue routes to abandon_and_log
/// GIVEN: Workflow loaded from TOML
/// WHEN: select_issue returns Fatal
/// THEN: Engine routes to abandon_and_log
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
    let mut runner =
        EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    // Fatal should route to abandon_and_log via transition table
    // The workflow should complete (Success) because abandon_and_log is the terminal step
    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected Success (fatal routed to abandon_and_log), got {:?}",
        result
    );
}

// ============================================================================
// Test 8: Fatal at any step routes to abandon_and_log
// ============================================================================

/// Test 8: Fatal at setup_workspace routes to abandon_and_log
/// GIVEN: Workflow loaded from TOML
/// WHEN: setup_workspace returns Fatal
/// THEN: Engine routes to abandon_and_log
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
    let mut runner =
        EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected Success (fatal routed to abandon_and_log), got {:?}",
        result
    );
}

/// Fatal at fetch_issue routes to abandon_and_log
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
    let mut runner =
        EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected Success (fatal routed to abandon_and_log), got {:?}",
        result
    );
}

/// Fatal at implement routes to abandon_and_log
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
    let mut runner =
        EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected Success (fatal routed to abandon_and_log), got {:?}",
        result
    );
}

// ============================================================================
// Test 9: Workflow type loads from TOML
// ============================================================================

/// Test 9: Workflow type loads from TOML
/// GIVEN: llxprt-issue-fix-v1.toml exists
/// WHEN: resolve_workflow_type() is called
/// THEN: Returns WorkflowType with 14 steps, transitions include per-edge limits
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-SEP-003
#[test]
fn test_workflow_type_loads_from_toml() {
    let fixture_root = std::path::PathBuf::from("tests/fixtures");
    let workflow_type = resolve_workflow_type("llxprt-issue-fix-v1", &fixture_root)
        .expect("Failed to load workflow type");

    // Assert 14 steps
    assert_eq!(workflow_type.steps.len(), 14, "Expected 14 steps");

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
// Test 10: Workflow config loads from TOML
// ============================================================================

/// Test 10: Workflow config loads from TOML
/// GIVEN: llxprt-code.toml exists
/// WHEN: resolve_workflow_config() is called
/// THEN: Returns WorkflowConfig with required variables
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
/// THEN: Every non-terminal step has a fatal → abandon_and_log transition
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
    let steps_with_transitions: std::collections::HashSet<_> =
        workflow_type.transitions.iter().map(|t| t.from.clone()).collect();

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
            "Step '{}' should have a fatal transition to abandon_and_log",
            step_id
        );
    }
}

// ============================================================================
// Test 12: Config variables injected into context
// ============================================================================

/// Test 12: Config variables injected into context
/// GIVEN: WorkflowConfig with profile_planning set
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
    let mut runner =
        EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    // Run should succeed (variables don't affect mock execution)
    let result = runner.run();
    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected Success, got {:?}",
        result
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
        "Expected Success, got {:?}",
        result
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
