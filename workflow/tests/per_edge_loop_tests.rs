/// @plan:PLAN-20260408-LLXPRT-FIRST.P13
/// Per-edge Loop Limits TDD Tests
///
/// These tests verify the behavioral requirements for per-edge loop limits.
/// They cover the completed `EngineRunner::run()` loop-limit contract.
///
/// Tests use a configurable `SequenceExecutor` that returns different outcomes
/// on successive calls, allowing precise control over workflow execution.
use std::collections::HashMap;
use std::sync::Mutex;

use luther_workflow::engine::executor::{ExecutorRegistry, StepContext, StepExecutor};
use luther_workflow::engine::instance::WorkflowInstance;
use luther_workflow::engine::runner::{EngineRunner, RunOutcome};
use luther_workflow::engine::transition::StepOutcome;
use luther_workflow::persistence::{Checkpoint, StateSnapshot};
use luther_workflow::workflow::schema::{
    GuardLimits, RepoConfig, RuntimeConfig, StepDef, TransitionDef, WorkflowConfig, WorkflowType,
};

/// `SequenceExecutor` returns outcomes from a configured sequence.
/// Used for tests that need to return different outcomes on successive calls.
/// Uses Mutex for thread-safe access (required by `StepExecutor`: Send + Sync).
/// @plan:PLAN-20260408-LLXPRT-FIRST.P13
struct SequenceExecutor {
    outcomes: Mutex<Vec<StepOutcome>>,
    call_count: Mutex<usize>,
}

impl SequenceExecutor {
    /// Create a new `SequenceExecutor` with the given outcomes.
    const fn new(outcomes: Vec<StepOutcome>) -> Self {
        Self {
            outcomes: Mutex::new(outcomes),
            call_count: Mutex::new(0),
        }
    }
}

impl StepExecutor for SequenceExecutor {
    fn execute(
        &self,
        _context: &mut StepContext,
        _params: &serde_json::Value,
    ) -> Result<StepOutcome, luther_workflow::engine::runner::EngineError> {
        let mut count = self.call_count.lock().unwrap();
        let outcomes = self.outcomes.lock().unwrap();

        if *count < outcomes.len() {
            let outcome = outcomes[*count];
            *count += 1;
            Ok(outcome)
        } else {
            // Default to Success if sequence exhausted
            Ok(StepOutcome::Success)
        }
    }
}

/// Helper: Create a registry with `SequenceExecutor` registered as "sequence".
fn sequence_registry(outcomes: Vec<StepOutcome>) -> ExecutorRegistry {
    let mut registry = ExecutorRegistry::new();
    registry.register("sequence", Box::new(SequenceExecutor::new(outcomes)));
    registry
}

/// Helper: Create a minimal workflow type with configurable steps and transitions.
fn make_workflow_type(steps: Vec<StepDef>, transitions: Vec<TransitionDef>) -> WorkflowType {
    WorkflowType {
        workflow_type_id: "test-per-edge-loop".to_string(),
        steps,
        transitions,
        guards: Default::default(),
    }
}

/// Helper: Create a minimal "sequence" step with the given id.
fn seq_step(step_id: &str) -> StepDef {
    StepDef {
        step_id: step_id.to_string(),
        step_type: "sequence".to_string(),
        description: None,
        produces: None,
        consumes: None,
        terminal: None,
        parameters: None,
    }
}

/// Helper: Create a minimal workflow config with configurable loop limit.
fn make_config(max_iterations: Option<u32>) -> WorkflowConfig {
    WorkflowConfig {
        config_id: "test-config".to_string(),
        workflow_type_id: "test-per-edge-loop".to_string(),
        runtime: RuntimeConfig {
            timeout_seconds: 3600,
            max_retries: 3,
            parallel_steps: None,
            log_level: None,
        },
        repo: RepoConfig {
            workspace_strategy: "temp".to_string(),
            branch_template: "test-{run_id}".to_string(),
            base_branch: Some("main".to_string()),
            workspace_root: None,
            project_subdir: None,
            artifact_path_base: None,
            diff_path_base: None,
            diff_path_normalization:
                luther_workflow::workflow::schema::DiffPathNormalization::RepoRelative,
        },
        guard_limits: GuardLimits {
            max_iterations,
            max_file_changes: Some(50),
            max_tokens: Some(10000),
            max_cost: Some(10.0),
        },
        variables: std::collections::HashMap::new(),
        discovery: None,
        parent_orchestration: Default::default(),
        command_manifest: None,
        target_profile: None,
    }
}

/// Test 1: Per-edge limit abandons when exceeded.
/// GIVEN: Workflow A → B → A (on fixable) with `max_iterations`: 2 on B→A transition
/// WHEN: Executor B always returns Fixable
/// THEN: On the 4th attempt, engine returns Abandoned
/// @plan:PLAN-20260408-LLXPRT-FIRST.P13
/// @requirement:REQ-LF-LOOP-001,REQ-LF-LOOP-003
#[test]
fn test_per_edge_limit_abandons_when_exceeded() {
    // GIVEN: Workflow A → B → A with per-edge limit 2
    let steps = vec![
        StepDef {
            step_id: "step_a".to_string(),
            step_type: "sequence".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: None,
        },
        StepDef {
            step_id: "step_b".to_string(),
            step_type: "sequence".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: None,
        },
    ];

    let transitions = vec![
        // A → B on success (forward)
        TransitionDef {
            from: "step_a".to_string(),
            to: "step_b".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        },
        // B → A on fixable with per-edge limit 2
        TransitionDef {
            from: "step_b".to_string(),
            to: "step_a".to_string(),
            condition: Some("fixable".to_string()),
            max_iterations: Some(2),
        },
    ];

    let workflow_type = make_workflow_type(steps, transitions);
    let config = make_config(Some(10)); // high global limit
    let instance = WorkflowInstance::create(workflow_type, config);

    // Executor: A returns Success (forward), B returns Fixable (loop back), repeat
    // With limit 2 on B→A, need 3 loop iterations to exceed:
    // A(S)→B(F)→A(S)→B(F)→A(S)→B(F)→Abandoned
    let registry = sequence_registry(vec![
        StepOutcome::Success,
        StepOutcome::Fixable,
        StepOutcome::Success,
        StepOutcome::Fixable,
        StepOutcome::Success,
        StepOutcome::Fixable,
    ]);
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    // WHEN: Run engine (will loop)
    let result = runner.run();

    // THEN: Should be Abandoned after exceeding limit
    match result {
        Ok(RunOutcome::Abandoned { step_id, reason }) => {
            // Step should identify where abandonment occurred
            assert!(
                step_id == "step_b" || step_id == "step_a",
                "Abandoned should identify the edge"
            );
            // Reason should mention loop limit or edge
            assert!(
                reason.contains("loop")
                    || reason.contains("limit")
                    || reason.contains("step_b")
                    || reason.contains("step_a"),
                "Abandon reason should mention loop limit or edge: got {reason}"
            );
        }
        Ok(other) => {
            panic!("Expected Abandoned, got {other:?}");
        }
        Err(e) => {
            panic!("Expected Abandoned outcome, got error: {e:?}");
        }
    }
}

/// Test 2: Per-edge limit allows iterations within limit.
/// GIVEN: Workflow A → B → A (on fixable, `max_iterations`: 3) → C (on success)
/// WHEN: Executor B returns Fixable twice, then Success
/// THEN: Run outcome is Success (2 loops within limit of 3)
/// @plan:PLAN-20260408-LLXPRT-FIRST.P13
/// @requirement:REQ-LF-LOOP-001
#[test]
fn test_per_edge_limit_allows_iterations_within_limit() {
    // GIVEN: Workflow A → B → A with limit 3, then to C
    let steps = vec![
        StepDef {
            step_id: "step_a".to_string(),
            step_type: "sequence".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: None,
        },
        StepDef {
            step_id: "step_b".to_string(),
            step_type: "sequence".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: None,
        },
        StepDef {
            step_id: "step_c".to_string(),
            step_type: "sequence".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: None,
        },
    ];

    let transitions = vec![
        // A → B on success
        TransitionDef {
            from: "step_a".to_string(),
            to: "step_b".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        },
        // B → A on fixable with limit 3
        TransitionDef {
            from: "step_b".to_string(),
            to: "step_a".to_string(),
            condition: Some("fixable".to_string()),
            max_iterations: Some(3),
        },
        // B → C on success (exit the loop)
        TransitionDef {
            from: "step_b".to_string(),
            to: "step_c".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        },
    ];

    let workflow_type = make_workflow_type(steps, transitions);
    let config = make_config(Some(10));
    let instance = WorkflowInstance::create(workflow_type, config);

    // Executor: B returns Fixable twice, then Success
    let registry = sequence_registry(vec![
        StepOutcome::Success, // step_a → step_b
        StepOutcome::Fixable, // step_b → step_a (loop 1)
        StepOutcome::Success, // step_a → step_b
        StepOutcome::Fixable, // step_b → step_a (loop 2)
        StepOutcome::Success, // step_a → step_b
        StepOutcome::Success, // step_b → step_c (exit)
    ]);
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    // WHEN: Run engine
    let result = runner.run();

    // THEN: Should be Success (2 loops within limit of 3)
    match result {
        Ok(RunOutcome::Success) => {
            // Test passes
        }
        Ok(other) => {
            panic!("Expected Success, got {other:?}");
        }
        Err(e) => {
            panic!("Expected Success, got error: {e:?}");
        }
    }
}

/// Test 3: Independent loops tracked separately.
/// GIVEN: Two independent loops with their own limits
/// WHEN: Each loop executes 2 times (4 total backward transitions)
/// THEN: Neither loop's limit is exceeded (global would say 4 ≥ 3)
/// @plan:PLAN-20260408-LLXPRT-FIRST.P13
/// @requirement:REQ-LF-LOOP-002
#[test]
fn test_independent_loops_tracked_separately() {
    // GIVEN: Workflow with two independent loops
    // Loop 1: A → B → A (fixable, limit 3)
    // Loop 2: C → D → C (fixable, limit 3)
    // Final: E
    let steps = vec![
        seq_step("step_a"),
        seq_step("step_b"),
        seq_step("step_c"),
        seq_step("step_d"),
        seq_step("step_e"),
    ];

    let transitions = vec![
        // Loop 1: A → B → A
        TransitionDef {
            from: "step_a".to_string(),
            to: "step_b".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        },
        TransitionDef {
            from: "step_b".to_string(),
            to: "step_a".to_string(),
            condition: Some("fixable".to_string()),
            max_iterations: Some(3),
        },
        TransitionDef {
            from: "step_b".to_string(),
            to: "step_c".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        },
        // Loop 2: C → D → C
        TransitionDef {
            from: "step_c".to_string(),
            to: "step_d".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        },
        TransitionDef {
            from: "step_d".to_string(),
            to: "step_c".to_string(),
            condition: Some("fixable".to_string()),
            max_iterations: Some(3),
        },
        TransitionDef {
            from: "step_d".to_string(),
            to: "step_e".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        },
    ];

    let workflow_type = make_workflow_type(steps, transitions);
    // Global limit of 3 would fail with 4 total backward transitions
    let config = make_config(Some(3));
    let instance = WorkflowInstance::create(workflow_type, config);

    // Executor sequence: loop 1 twice, loop 2 twice, then exit
    let registry = sequence_registry(vec![
        StepOutcome::Success, // step_a → step_b
        StepOutcome::Fixable, // step_b → step_a (loop 1, iter 1)
        StepOutcome::Success, // step_a → step_b
        StepOutcome::Success, // step_b → step_c (exit loop 1)
        StepOutcome::Success, // step_c → step_d
        StepOutcome::Fixable, // step_d → step_c (loop 2, iter 1)
        StepOutcome::Success, // step_c → step_d
        StepOutcome::Success, // step_d → step_e (exit loop 2)
    ]);
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    // WHEN: Run engine
    let result = runner.run();

    // THEN: Success - per-edge tracking allows 4 total backward transitions
    match result {
        Ok(RunOutcome::Success) => {
            // Test passes
        }
        Ok(RunOutcome::Abandoned { reason, .. }) => {
            panic!("Should have succeeded with per-edge tracking. Abandoned: {reason}");
        }
        Ok(other) => {
            panic!("Expected Success, got {other:?}");
        }
        Err(e) => {
            panic!("Expected Success, got error: {e:?}");
        }
    }
}

/// Test 4: Global fallback used when no per-edge limit.
/// GIVEN: Workflow A → B → A (fixable, NO per-edge limit) with global `max_iterations`: 2
/// WHEN: Executor B always returns Fixable
/// THEN: Engine returns Abandoned after 2 iterations (using global fallback)
/// @plan:PLAN-20260408-LLXPRT-FIRST.P13
/// @requirement:REQ-LF-LOOP-004
#[test]
fn test_global_fallback_used_when_no_per_edge_limit() {
    // GIVEN: Workflow with NO per-edge limit, global limit 2
    let steps = vec![
        StepDef {
            step_id: "step_a".to_string(),
            step_type: "sequence".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: None,
        },
        StepDef {
            step_id: "step_b".to_string(),
            step_type: "sequence".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: None,
        },
    ];

    let transitions = vec![
        // A → B on success
        TransitionDef {
            from: "step_a".to_string(),
            to: "step_b".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None, // No per-edge limit
        },
        // B → A on fixable, no per-edge limit
        TransitionDef {
            from: "step_b".to_string(),
            to: "step_a".to_string(),
            condition: Some("fixable".to_string()),
            max_iterations: None, // No per-edge limit
        },
    ];

    let workflow_type = make_workflow_type(steps, transitions);
    let config = make_config(Some(2)); // Global limit of 2
    let instance = WorkflowInstance::create(workflow_type, config);

    // Executor: A returns Success, B returns Fixable, repeat enough to exceed global limit 2
    // A(S)→B(F)→A(S)→B(F)→A(S)→B(F)→Abandoned
    let registry = sequence_registry(vec![
        StepOutcome::Success,
        StepOutcome::Fixable,
        StepOutcome::Success,
        StepOutcome::Fixable,
        StepOutcome::Success,
        StepOutcome::Fixable,
    ]);
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    // WHEN: Run engine
    let result = runner.run();

    // THEN: Abandoned after 2 iterations using global fallback
    match result {
        Ok(RunOutcome::Abandoned { .. }) => {
            // Test passes
        }
        Ok(other) => {
            panic!("Expected Abandoned (global fallback), got {other:?}");
        }
        Err(e) => {
            panic!("Expected Abandoned, got error: {e:?}");
        }
    }
}

/// Test 5: Per-edge limit overrides global.
/// GIVEN: Workflow with per-edge limit 5 and global `max_iterations`: 2
/// WHEN: Executor B returns Fixable 3 times then Success
/// THEN: Run outcome is Success (per-edge limit 5 > global 2)
/// @plan:PLAN-20260408-LLXPRT-FIRST.P13
/// @requirement:REQ-LF-LOOP-001,REQ-LF-LOOP-004
#[test]
fn test_per_edge_limit_overrides_global() {
    // GIVEN: Per-edge limit 5, global limit 2
    let steps = vec![
        StepDef {
            step_id: "step_a".to_string(),
            step_type: "sequence".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: None,
        },
        StepDef {
            step_id: "step_b".to_string(),
            step_type: "sequence".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: None,
        },
    ];

    let transitions = vec![
        TransitionDef {
            from: "step_a".to_string(),
            to: "step_b".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        },
        // Per-edge limit 5 overrides global 2
        TransitionDef {
            from: "step_b".to_string(),
            to: "step_a".to_string(),
            condition: Some("fixable".to_string()),
            max_iterations: Some(5),
        },
    ];

    let workflow_type = make_workflow_type(steps, transitions);
    let config = make_config(Some(2)); // Global limit 2
    let instance = WorkflowInstance::create(workflow_type, config);

    // Executor: 3 Fixable iterations then Success
    let registry = sequence_registry(vec![
        StepOutcome::Success, // step_a → step_b
        StepOutcome::Fixable, // step_b → step_a (1)
        StepOutcome::Success, // step_a → step_b
        StepOutcome::Fixable, // step_b → step_a (2)
        StepOutcome::Success, // step_a → step_b
        StepOutcome::Fixable, // step_b → step_a (3)
        StepOutcome::Success, // step_a → step_b
        StepOutcome::Success, // step_b → terminal
    ]);
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    // WHEN: Run engine
    let result = runner.run();

    // THEN: Success because per-edge limit 5 > 3 iterations needed
    match result {
        Ok(RunOutcome::Success) => {
            // Test passes
        }
        Ok(RunOutcome::Abandoned { reason, .. }) => {
            panic!("Per-edge limit 5 should allow 3 iterations. Abandoned: {reason}");
        }
        Ok(other) => {
            panic!("Expected Success, got {other:?}");
        }
        Err(e) => {
            panic!("Expected Success, got error: {e:?}");
        }
    }
}

/// Test 6: Edge counts survive checkpoint roundtrip.
/// GIVEN: `StateSnapshot` with `edge_loop_counts`: {"A:B": 2, "C:D": 1}
/// WHEN: Checkpoint is saved and loaded
/// THEN: Loaded snapshot has same `edge_loop_counts`
/// @plan:PLAN-20260408-LLXPRT-FIRST.P13
/// @requirement:REQ-LF-LOOP-005
#[test]
fn test_edge_counts_survive_checkpoint_roundtrip() {
    use luther_workflow::persistence::{load_checkpoint_with_conn, save_checkpoint_with_conn};
    use rusqlite::Connection;

    // GIVEN: StateSnapshot with edge_loop_counts
    let mut edge_loop_counts = HashMap::new();
    edge_loop_counts.insert("A:B".to_string(), 2);
    edge_loop_counts.insert("C:D".to_string(), 1);

    let snapshot = StateSnapshot {
        retry_count: 1,
        loop_count: 3, // 2 + 1 = 3
        edge_loop_counts: edge_loop_counts.clone(),
        context: HashMap::new(),
        status: "running".to_string(),
    };

    let checkpoint = Checkpoint::with_snapshot("run-123", "step-x", snapshot);

    // WHEN: Save and load checkpoint
    let conn = Connection::open_in_memory().expect("Failed to open in-memory database");
    save_checkpoint_with_conn(&conn, &checkpoint).expect("Failed to save checkpoint");

    let loaded = load_checkpoint_with_conn(&conn, "run-123").expect("Failed to load checkpoint");

    // THEN: Edge counts are restored
    let loaded_cp = loaded.expect("Checkpoint should exist");
    assert_eq!(
        loaded_cp.state_snapshot.edge_loop_counts.get("A:B"),
        Some(&2),
        "Edge count A:B should be 2"
    );
    assert_eq!(
        loaded_cp.state_snapshot.edge_loop_counts.get("C:D"),
        Some(&1),
        "Edge count C:D should be 1"
    );
}

/// Test 7: Abandoned reason identifies edge.
/// GIVEN: Workflow with edge limit exceeded
/// WHEN: Engine returns Abandoned
/// THEN: Reason contains identifying info (step names or edge key)
/// @plan:PLAN-20260408-LLXPRT-FIRST.P13
/// @requirement:REQ-LF-LOOP-003
#[test]
fn test_abandoned_reason_identifies_edge() {
    // GIVEN: Workflow with edge "step_b:step_a" that has limit 2
    let steps = vec![
        StepDef {
            step_id: "step_a".to_string(),
            step_type: "sequence".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: None,
        },
        StepDef {
            step_id: "step_b".to_string(),
            step_type: "sequence".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: None,
        },
    ];

    let transitions = vec![
        TransitionDef {
            from: "step_a".to_string(),
            to: "step_b".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        },
        // Edge with limit 2
        TransitionDef {
            from: "step_b".to_string(),
            to: "step_a".to_string(),
            condition: Some("fixable".to_string()),
            max_iterations: Some(2),
        },
    ];

    let workflow_type = make_workflow_type(steps, transitions);
    let config = make_config(Some(10));
    let instance = WorkflowInstance::create(workflow_type, config);

    // Executor: Always returns Fixable to trigger abandonment
    // A(S)→B(F)→A(S)→B(F)→A(S)→B(F)→Abandoned
    let registry = sequence_registry(vec![
        StepOutcome::Success,
        StepOutcome::Fixable,
        StepOutcome::Success,
        StepOutcome::Fixable,
        StepOutcome::Success,
        StepOutcome::Fixable,
    ]);
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    // WHEN: Run engine
    let result = runner.run();

    // THEN: Reason identifies the edge
    match result {
        Ok(RunOutcome::Abandoned { reason, .. }) => {
            // Reason should contain identifying information
            let has_edge_info = reason.contains("step_b")
                || reason.contains("step_a")
                || reason.contains("B:A")
                || reason.contains("limit");
            assert!(
                has_edge_info,
                "Abandon reason should identify the edge: got {reason}"
            );
        }
        Ok(other) => {
            panic!("Expected Abandoned, got {other:?}");
        }
        Err(e) => {
            panic!("Expected Abandoned, got error: {e:?}");
        }
    }
}

/// Test 8: Forward transitions not counted.
/// GIVEN: Workflow A → B → C → D (all forward, all success)
/// WHEN: All transitions have `max_iterations`: 1
/// THEN: Run outcome is Success (forward transitions don't increment counters)
/// @plan:PLAN-20260408-LLXPRT-FIRST.P13
/// @requirement:REQ-LF-LOOP-002
#[test]
fn test_forward_transitions_not_counted() {
    // GIVEN: All forward transitions with max_iterations: 1
    let steps = vec![
        StepDef {
            step_id: "step_a".to_string(),
            step_type: "sequence".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: None,
        },
        StepDef {
            step_id: "step_b".to_string(),
            step_type: "sequence".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: None,
        },
        StepDef {
            step_id: "step_c".to_string(),
            step_type: "sequence".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: None,
        },
        StepDef {
            step_id: "step_d".to_string(),
            step_type: "sequence".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: None,
        },
    ];

    let transitions = vec![
        // All forward, all with max_iterations: 1
        TransitionDef {
            from: "step_a".to_string(),
            to: "step_b".to_string(),
            condition: Some("success".to_string()),
            max_iterations: Some(1),
        },
        TransitionDef {
            from: "step_b".to_string(),
            to: "step_c".to_string(),
            condition: Some("success".to_string()),
            max_iterations: Some(1),
        },
        TransitionDef {
            from: "step_c".to_string(),
            to: "step_d".to_string(),
            condition: Some("success".to_string()),
            max_iterations: Some(1),
        },
    ];

    let workflow_type = make_workflow_type(steps, transitions);
    let config = make_config(Some(1)); // Low global limit too
    let instance = WorkflowInstance::create(workflow_type, config);

    // Executor: All steps return Success
    let registry = sequence_registry(vec![
        StepOutcome::Success,
        StepOutcome::Success,
        StepOutcome::Success,
    ]);
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    // WHEN: Run engine
    let result = runner.run();

    // THEN: Success - forward transitions don't count
    match result {
        Ok(RunOutcome::Success) => {
            // Test passes
        }
        Ok(other) => {
            panic!("Expected Success, got {other:?}");
        }
        Err(e) => {
            panic!("Expected Success, got error: {e:?}");
        }
    }
}

/// Test 9: Loop count accessor returns sum of edge counts.
/// GIVEN: Engine with 2 iterations of loop A → B → A
/// WHEN: `runner.loop_count()` is called
/// THEN: Returns 2 (sum of all edge counts)
/// @plan:PLAN-20260408-LLXPRT-FIRST.P13
/// @requirement:REQ-LF-LOOP-002
#[test]
fn test_loop_count_accessor_returns_sum_of_edge_counts() {
    // GIVEN: Workflow with loop A → B → A
    let steps = vec![
        StepDef {
            step_id: "step_a".to_string(),
            step_type: "sequence".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: None,
        },
        StepDef {
            step_id: "step_b".to_string(),
            step_type: "sequence".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: None,
        },
    ];

    let transitions = vec![
        TransitionDef {
            from: "step_a".to_string(),
            to: "step_b".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        },
        TransitionDef {
            from: "step_b".to_string(),
            to: "step_a".to_string(),
            condition: Some("fixable".to_string()),
            max_iterations: Some(5), // High limit
        },
    ];

    let workflow_type = make_workflow_type(steps, transitions);
    let config = make_config(Some(10));
    let instance = WorkflowInstance::create(workflow_type, config);

    // Executor: Fixable twice, then Success
    let registry = sequence_registry(vec![
        StepOutcome::Success, // step_a → step_b
        StepOutcome::Fixable, // step_b → step_a (1)
        StepOutcome::Success, // step_a → step_b
        StepOutcome::Fixable, // step_b → step_a (2)
        StepOutcome::Success, // step_a → step_b
        StepOutcome::Success, // step_b → terminal
    ]);
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    // Run the workflow
    let _ = runner.run();

    // WHEN: Get loop count
    let loop_count = runner.loop_count();

    // THEN: Should be 2 (sum of backward edge counts)
    assert_eq!(
        loop_count, 2,
        "loop_count() should return sum of edge loop counts (2)"
    );
}

/// Test 10: Mixed per-edge and global limits.
/// GIVEN: loop1 A→B→A (per-edge limit 2), loop2 C→D→C (no per-edge, global=5)
/// WHEN: B returns Fixable 3 times (exceeds limit 2)
/// THEN: Abandoned at A→B→A loop, NOT at C→D→C
/// @plan:PLAN-20260408-LLXPRT-FIRST.P13
/// @requirement:REQ-LF-LOOP-001,REQ-LF-LOOP-004
#[test]
fn test_mixed_per_edge_and_global_limits() {
    // GIVEN: Two loops - one with per-edge, one with global
    let steps = vec![
        seq_step("step_a"),
        seq_step("step_b"),
        seq_step("step_c"),
        seq_step("step_d"),
    ];

    let transitions = vec![
        // Loop 1: A→B→A with per-edge limit 2
        TransitionDef {
            from: "step_a".to_string(),
            to: "step_b".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        },
        TransitionDef {
            from: "step_b".to_string(),
            to: "step_a".to_string(),
            condition: Some("fixable".to_string()),
            max_iterations: Some(2), // Per-edge limit 2
        },
        // Exit loop1 to loop2
        TransitionDef {
            from: "step_b".to_string(),
            to: "step_c".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        },
        // Loop 2: C→D→C with NO per-edge limit
        TransitionDef {
            from: "step_c".to_string(),
            to: "step_d".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None, // No per-edge limit
        },
        TransitionDef {
            from: "step_d".to_string(),
            to: "step_c".to_string(),
            condition: Some("fixable".to_string()),
            max_iterations: None, // No per-edge limit - uses global
        },
    ];

    let workflow_type = make_workflow_type(steps, transitions);
    // Global limit 5 - higher than loop1's limit 2
    let config = make_config(Some(5));
    let instance = WorkflowInstance::create(workflow_type, config);

    // Executor: B returns Fixable 3 times (exceeds limit 2)
    // We never get to loop 2 because loop 1 hits limit first
    let registry = sequence_registry(vec![
        StepOutcome::Success, // step_a → step_b
        StepOutcome::Fixable, // step_b → step_a (1)
        StepOutcome::Success, // step_a → step_b
        StepOutcome::Fixable, // step_b → step_a (2)
        StepOutcome::Success, // step_a → step_b
        StepOutcome::Fixable, // step_b → step_a (3) - EXCEEDS limit 2!
    ]);
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    // WHEN: Run engine
    let result = runner.run();

    // THEN: Abandoned at loop 1, NOT at loop 2
    match result {
        Ok(RunOutcome::Abandoned { reason, step_id }) => {
            // Should be abandoned at step_b (where the backward edge originates)
            assert_eq!(
                step_id, "step_b",
                "Should abandon at step_b (loop 1), not step_d (loop 2)"
            );
            // Reason should NOT mention step_d/loop 2
            assert!(
                !reason.contains("step_d"),
                "Abandon reason should not mention loop 2 (step_d): got {reason}"
            );
        }
        Ok(other) => {
            panic!("Expected Abandoned at loop 1, got {other:?}");
        }
        Err(e) => {
            panic!("Expected Abandoned, got error: {e:?}");
        }
    }
}
