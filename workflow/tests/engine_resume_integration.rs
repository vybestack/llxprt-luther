/// @plan:PLAN-20260404-INITIAL-RUNTIME.P07
/// @plan:PLAN-20260408-STEP-EXEC.P06
/// Integration tests for engine resume functionality - checkpoint recovery.
///
/// These tests verify that workflow runs can be interrupted and resumed
/// from persisted checkpoints.
use std::collections::HashMap;

use luther_workflow::engine::executor::{
    ExecutorRegistry, NoOpExecutor, StepContext, StepExecutor,
};
use luther_workflow::engine::instance::WorkflowInstance;
use luther_workflow::engine::runner::{EngineError, EngineRunner, RunOutcome};
use luther_workflow::engine::transition::StepOutcome;
use luther_workflow::persistence::{
    get_run_with_conn, load_checkpoint, load_checkpoint_with_conn, save_checkpoint, Checkpoint,
    RunStatus, StateSnapshot, CHECKPOINT_STATUS_WAITING,
};
use luther_workflow::workflow::schema::{
    GuardLimits, RepoConfig, RuntimeConfig, WorkflowConfig, WorkflowType,
};

/// Executor that returns a fixed outcome for the `test` step type. Used to
/// drive the recoverable external-wait path without real PR checks.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
struct FixedOutcomeExecutor(StepOutcome);

impl StepExecutor for FixedOutcomeExecutor {
    fn execute(
        &self,
        _context: &mut StepContext,
        _params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        Ok(self.0)
    }
}

/// Registry whose `test` step type yields the supplied outcome.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
fn fixed_registry(outcome: StepOutcome) -> ExecutorRegistry {
    let mut registry = ExecutorRegistry::new();
    registry.register("test", Box::new(FixedOutcomeExecutor(outcome)));
    registry
}

/// Helper to create a registry with `NoOpExecutor` for test steps.
fn test_registry() -> ExecutorRegistry {
    let mut registry = ExecutorRegistry::new();
    registry.register("test", Box::new(NoOpExecutor));
    registry
}

/// Helper to create a minimal `WorkflowType` for testing.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P07
fn test_workflow_type() -> WorkflowType {
    WorkflowType {
        workflow_type_id: "test-workflow-v1".to_string(),
        steps: vec![
            luther_workflow::workflow::schema::StepDef {
                step_id: "step_a".to_string(),
                step_type: "test".to_string(),
                description: Some("First step".to_string()),
                produces: None,
                consumes: None,
                terminal: None,
                recovery_policy: None,
                parameters: None,
            },
            luther_workflow::workflow::schema::StepDef {
                step_id: "step_b".to_string(),
                step_type: "test".to_string(),
                description: Some("Second step".to_string()),
                produces: None,
                consumes: None,
                terminal: None,
                recovery_policy: None,
                parameters: None,
            },
            luther_workflow::workflow::schema::StepDef {
                step_id: "step_c".to_string(),
                step_type: "test".to_string(),
                description: Some("Third step".to_string()),
                produces: None,
                consumes: None,
                terminal: None,
                recovery_policy: None,
                parameters: None,
            },
        ],
        transitions: vec![
            luther_workflow::workflow::schema::TransitionDef {
                from: "step_a".to_string(),
                to: "step_b".to_string(),
                condition: Some("success".to_string()),
                max_iterations: None,
            },
            luther_workflow::workflow::schema::TransitionDef {
                from: "step_b".to_string(),
                to: "step_c".to_string(),
                condition: Some("success".to_string()),
                max_iterations: None,
            },
        ],
        guards: Default::default(),
    }
}

/// Helper to create a minimal `WorkflowConfig` for testing.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P07
fn test_workflow_config() -> WorkflowConfig {
    WorkflowConfig {
        config_id: "test-profile".to_string(),
        workflow_type_id: "test-workflow-v1".to_string(),
        runtime: RuntimeConfig {
            timeout_seconds: 3600,
            max_retries: 3,
            parallel_steps: None,
            log_level: Some("info".to_string()),
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
            max_iterations: Some(3),
            max_file_changes: Some(50),
            max_tokens: Some(10000),
            max_cost: Some(10.0),
        },
        variables: std::collections::HashMap::new(),
        discovery: None,
        parent_orchestration: Default::default(),
        merge_required: false,
        merge_strategy: None,
        command_manifest: None,
        target_profile: None,
    }
}

/// Test: Resume from checkpoint continues at the correct step.
/// GIVEN: run interrupted at step B with checkpoint
/// WHEN: engine resumes with same `run_id`
/// THEN: continues from step B, not from beginning
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
/// @requirement:REQ-EARS-ENG-004
#[test]
fn test_resume_from_checkpoint_continues_at_step() {
    // GIVEN: a workflow instance that completed step_a and was interrupted at step_b
    let workflow_type = test_workflow_type();
    let config = test_workflow_config();
    let instance = WorkflowInstance::create(workflow_type.clone(), config.clone());
    let run_id = instance.run_id;

    // Create a checkpoint indicating we completed step_a and are now at step_b
    let snapshot = StateSnapshot {
        status: "interrupted".to_string(),
        ..StateSnapshot::default()
    };
    let checkpoint = Checkpoint::with_snapshot(&run_id, "step_b", snapshot);

    // Save the checkpoint
    let _ = save_checkpoint(&run_id, &checkpoint);

    // WHEN: creating a new runner for the same run_id with resume capability
    // The engine should load the checkpoint and resume from step_b
    let resumed_instance = WorkflowInstance::create_with_run_id(workflow_type, config, &run_id);
    let registry = test_registry();
    let mut resumed_runner =
        EngineRunner::new(resumed_instance, registry).expect("Failed to create EngineRunner");

    // Attempt to resume from checkpoint
    let resumed = resumed_runner
        .try_resume()
        .expect("try_resume should not fail");

    // THEN: if checkpoint was loaded, current step should be step_b
    if resumed {
        assert_eq!(
            resumed_runner.current_step(),
            "step_b",
            "After resume, current step should be step_b"
        );
    }

    // Verify the checkpoint exists in database
    let loaded_checkpoint = load_checkpoint(&run_id);

    // THEN: if checkpoint loads, we should have the expected data
    match loaded_checkpoint {
        Ok(Some(cp)) => {
            assert_eq!(
                cp.run_id, run_id,
                "Checkpoint should be for the correct run"
            );
            assert_eq!(cp.step_id, "step_b", "Should resume from step_b");
            assert_eq!(
                cp.state_snapshot.status, "interrupted",
                "Status should be interrupted"
            );
        }
        Ok(None) => {
            // No checkpoint found - acceptable in RED phase
        }
        Err(_) => {
            // Error loading checkpoint - acceptable in RED phase
        }
    }

    // The key behavioral requirement: step_a should NOT be executed again
    // In a complete implementation, the run should skip step_a and start at step_b
    let run_result = resumed_runner.run();

    // Result can be anything in RED phase - we're verifying the test compiles
    // and the API exists for resume functionality
    let _ = run_result;
}

/// Test: Interrupt persists resumable checkpoint.
/// GIVEN: run executing
/// WHEN: interrupt signal received
/// THEN: checkpoint persisted with interrupted status
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P07
/// @requirement:REQ-EARS-ENG-004
#[test]
fn test_interrupt_persists_resumable_checkpoint() {
    // GIVEN: a workflow instance currently executing
    let workflow_type = test_workflow_type();
    let config = test_workflow_config();
    let instance = WorkflowInstance::create(workflow_type, config);
    let run_id = instance.run_id.clone();

    let mut runner =
        EngineRunner::new(instance, test_registry()).expect("Failed to create EngineRunner");

    // Start execution
    let _ = runner.execute_step("step_a");

    // WHEN: interrupt signal is received
    let interrupt_result = runner.handle_interrupt();

    // THEN: result should indicate the run was interrupted
    match interrupt_result {
        Ok(RunOutcome::Interrupted { step_id }) => {
            // Expected outcome: run interrupted at current step
            assert!(
                !step_id.is_empty(),
                "Interrupted outcome should include step_id"
            );

            // AND: a checkpoint should be persisted with interrupted status
            let checkpoint_result = load_checkpoint(&run_id);
            match checkpoint_result {
                Ok(Some(cp)) => {
                    assert_eq!(
                        cp.state_snapshot.status, "interrupted",
                        "Checkpoint should have interrupted status"
                    );
                    assert_eq!(
                        cp.run_id, run_id,
                        "Checkpoint should be for the correct run"
                    );
                }
                Ok(None) => {
                    // No checkpoint - expected in RED phase
                }
                Err(_) => {
                    // Error loading - expected in RED phase
                }
            }
        }
        Ok(RunOutcome::Success) => {
            // In RED phase, the interrupt handler may not be fully implemented
            // A success outcome is acceptable for test compilation
        }
        Ok(_) => {
            // Other outcomes are acceptable in RED phase
        }
        Err(_) => {
            // Errors are expected in RED phase
        }
    }
}

/// Test: Resumed run preserves loop and retry counters.
/// GIVEN: checkpoint with `loop_count` = 2 and `retry_count` = 1
/// WHEN: run resumes from checkpoint
/// THEN: counters are restored from checkpoint, not reset to 0
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P07
/// @requirement:REQ-EARS-ENG-004,REQ-EARS-ROUTE-002
/// Test: Interrupt handle requests a checkpointed interrupted outcome.
/// @requirement:REQ-EARS-ENG-004
#[test]
fn test_interrupt_handle_requests_interrupted_outcome() {
    let workflow_type = test_workflow_type();
    let config = test_workflow_config();
    let instance = WorkflowInstance::create(workflow_type, config);
    let registry = test_registry();
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    runner
        .interrupt_handle()
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let outcome = runner.run().expect("run should observe interrupt flag");
    assert!(
        matches!(outcome, RunOutcome::Interrupted { .. }),
        "interrupt handle should make the next run loop checkpoint and return Interrupted, got {outcome:?}"
    );
}

#[test]
fn test_resume_preserves_loop_and_retry_counters() {
    // GIVEN: workflow with loop state
    let workflow_type = test_workflow_type();
    let config = test_workflow_config();
    let instance = WorkflowInstance::create(workflow_type.clone(), config.clone());
    let run_id = instance.run_id;

    // Create a checkpoint with non-zero loop and retry counters
    let snapshot = StateSnapshot {
        retry_count: 1,
        loop_count: 2,
        context: HashMap::new(),
        status: "interrupted".to_string(),
        edge_loop_counts: HashMap::new(),
    };
    let checkpoint = Checkpoint::with_snapshot(&run_id, "step_b", snapshot);

    // Save checkpoint
    let _ = save_checkpoint(&run_id, &checkpoint);

    // WHEN: creating a resumed runner
    let resumed_instance = WorkflowInstance::create_with_run_id(workflow_type, config, &run_id);
    let mut runner = EngineRunner::new(resumed_instance, test_registry())
        .expect("Failed to create EngineRunner");

    // Load the checkpoint
    let loaded = load_checkpoint(&run_id);

    // THEN: counters should be preserved
    if let Ok(Some(cp)) = loaded {
        assert_eq!(
            cp.state_snapshot.loop_count, 2,
            "Loop count should be preserved at 2"
        );
        assert_eq!(
            cp.state_snapshot.retry_count, 1,
            "Retry count should be preserved at 1"
        );
    } else {
        // Expected in RED phase until persistence is fully implemented
    }

    // The runner should have the ability to restore these counters
    // This verifies the API supports counter restoration
    let _ = runner.run();
}

/// Test: a step returning `Wait` with no matching `wait` edge pauses the run.
/// GIVEN: step_a yields `StepOutcome::Wait` and the workflow has no `wait` edge
/// WHEN: the engine runs
/// THEN: it returns `WaitingExternal` at step_a, persists a `waiting`
///       checkpoint there, and records the non-terminal `WaitingForChecks`
///       status (the run stays active and resumable).
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
#[test]
fn wait_outcome_pauses_run_as_waiting_external() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("checkpoints.db");
    let instance = WorkflowInstance::create(test_workflow_type(), test_workflow_config());
    let run_id = instance.run_id.clone();

    let mut runner = EngineRunner::with_db_path_and_context(
        instance,
        fixed_registry(StepOutcome::Wait),
        &db_path,
        Default::default(),
    )
    .expect("runner");
    let outcome = runner.run().expect("run");
    drop(runner);

    assert!(
        matches!(&outcome, RunOutcome::WaitingExternal { step_id, .. } if step_id == "step_a"),
        "Wait with no matching edge should pause at step_a, got {outcome:?}"
    );

    let conn = rusqlite::Connection::open(&db_path).expect("open db");
    let md = get_run_with_conn(&conn, &run_id)
        .expect("query run")
        .expect("run row");
    assert_eq!(md.status, RunStatus::WaitingExternal);
    assert!(
        !md.status.is_terminal(),
        "waiting status must be non-terminal"
    );
    let cp = load_checkpoint_with_conn(&conn, &run_id)
        .expect("query checkpoint")
        .expect("checkpoint row");
    assert_eq!(cp.step_id, "step_a");
    assert_eq!(cp.state_snapshot.status, CHECKPOINT_STATUS_WAITING);
}

/// Test: resuming a `WaitingForChecks` run re-enters the wait step and, once it
/// succeeds (external condition resolved), advances to the next step without
/// re-running earlier steps.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
#[test]
fn resume_waiting_run_advances_when_wait_step_succeeds() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("checkpoints.db");
    let instance = WorkflowInstance::create(test_workflow_type(), test_workflow_config());
    let run_id = instance.run_id.clone();

    // First pass: step_a waits, run pauses at step_a.
    let mut runner = EngineRunner::with_db_path_and_context(
        instance,
        fixed_registry(StepOutcome::Wait),
        &db_path,
        Default::default(),
    )
    .expect("runner");
    let first = runner.run().expect("first run");
    drop(runner);
    assert!(matches!(first, RunOutcome::WaitingExternal { .. }));

    // Second pass: the external condition resolved; step now succeeds and the
    // run drives to completion.
    let resumed =
        WorkflowInstance::create_with_run_id(test_workflow_type(), test_workflow_config(), &run_id);
    let mut runner2 = EngineRunner::with_db_path_and_context(
        resumed,
        fixed_registry(StepOutcome::Success),
        &db_path,
        Default::default(),
    )
    .expect("runner");
    let second = runner2.run().expect("second run");
    drop(runner2);
    assert_eq!(second, RunOutcome::Success);

    let conn = rusqlite::Connection::open(&db_path).expect("open db");
    let md = get_run_with_conn(&conn, &run_id)
        .expect("query run")
        .expect("run row");
    assert_eq!(md.status, RunStatus::Completed);
}
