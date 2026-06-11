use luther_workflow::engine::executor::ExecutorRegistry;
use luther_workflow::engine::executors::shell::ShellExecutor;
use luther_workflow::engine::executors::write_file::WriteFileExecutor;
use luther_workflow::engine::instance::WorkflowInstance;
use luther_workflow::engine::runner::{EngineRunner, RunOutcome};
use luther_workflow::workflow::schema::{
    GuardConfig, GuardLimits, RepoConfig, RuntimeConfig, StepDef, TransitionDef, WorkflowConfig,
    WorkflowType,
};

/// Helper: create executor registry with real executors for hello-world steps.
fn hello_world_registry() -> ExecutorRegistry {
    let mut registry = ExecutorRegistry::new();
    registry.register("shell", Box::new(ShellExecutor));
    registry.register("write_file", Box::new(WriteFileExecutor));
    registry
}

/// Helper: create hello-world workflow type programmatically.
fn hello_world_workflow_type(work_dir: &std::path::Path) -> WorkflowType {
    WorkflowType {
        workflow_type_id: "hello-world-v1".to_string(),
        steps: vec![
            StepDef {
                step_id: "init_project".to_string(),
                step_type: "shell".to_string(),
                description: Some("Init Rust project".to_string()),
                produces: None,
                consumes: None,
                terminal: None,
                parameters: Some(serde_json::json!({
                    "command": format!("cargo init --name hello_world {}", work_dir.display())
                })),
            },
            StepDef {
                step_id: "write_test".to_string(),
                step_type: "write_file".to_string(),
                description: Some("Write test".to_string()),
                produces: None,
                consumes: None,
                terminal: None,
                parameters: Some(serde_json::json!({
                    "path": "tests/greeting_test.rs",
                    "content": "#[test]\nfn test_greet() {\n    assert_eq!(hello_world::greet(\"World\"), \"Hello, World!\");\n}\n"
                })),
            },
            StepDef {
                step_id: "write_impl".to_string(),
                step_type: "write_file".to_string(),
                description: Some("Write implementation".to_string()),
                produces: None,
                consumes: None,
                terminal: None,
                parameters: Some(serde_json::json!({
                    "path": "src/lib.rs",
                    "content": "pub fn greet(name: &str) -> String {\n    format!(\"Hello, {}!\", name)\n}\n"
                })),
            },
            StepDef {
                step_id: "run_tests".to_string(),
                step_type: "shell".to_string(),
                description: Some("Run cargo test".to_string()),
                produces: None,
                consumes: None,
                terminal: None,
                parameters: Some(serde_json::json!({
                    "command": "cargo test"
                })),
            },
            StepDef {
                step_id: "complete".to_string(),
                step_type: "shell".to_string(),
                description: Some("Echo done".to_string()),
                produces: None,
                consumes: None,
                terminal: None,
                parameters: Some(serde_json::json!({
                    "command": "echo done"
                })),
            },
        ],
        transitions: vec![
            TransitionDef {
                from: "init_project".to_string(),
                to: "write_test".to_string(),
                condition: Some("success".to_string()),
                max_iterations: None,
            },
            TransitionDef {
                from: "write_test".to_string(),
                to: "write_impl".to_string(),
                condition: Some("success".to_string()),
                max_iterations: None,
            },
            TransitionDef {
                from: "write_impl".to_string(),
                to: "run_tests".to_string(),
                condition: Some("success".to_string()),
                max_iterations: None,
            },
            TransitionDef {
                from: "run_tests".to_string(),
                to: "complete".to_string(),
                condition: Some("success".to_string()),
                max_iterations: None,
            },
            TransitionDef {
                from: "run_tests".to_string(),
                to: "write_impl".to_string(),
                condition: Some("fixable".to_string()),
                max_iterations: None,
            },
        ],
        guards: GuardConfig::default(),
    }
}

/// Helper: create hello-world workflow config.
fn hello_world_config() -> WorkflowConfig {
    WorkflowConfig {
        config_id: "hello-world-config".to_string(),
        workflow_type_id: "hello-world-v1".to_string(),
        runtime: RuntimeConfig {
            timeout_seconds: 300,
            max_retries: 2,
            parallel_steps: None,
            log_level: None,
        },
        repo: RepoConfig {
            workspace_strategy: "temp_clone".to_string(),
            branch_template: "hello-world-{run_id}".to_string(),
            base_branch: Some("main".to_string()),
            workspace_root: None,
        },
        guard_limits: GuardLimits {
            max_iterations: Some(3),
            max_file_changes: Some(10),
            max_tokens: Some(1000),
            max_cost: Some(1.0),
        },
        variables: std::collections::HashMap::new(),
    }
}

/// @plan:PLAN-20260408-STEP-EXEC.P07
/// @requirement:REQ-EXEC-007
/// GIVEN: hello-world workflow type and config
/// WHEN: engine runs with real `shell+write_file` executors
/// THEN: workflow completes with `RunOutcome::Success`
#[test]
fn test_hello_world_workflow_end_to_end() {
    let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let work_dir = temp_dir.path().to_path_buf();

    let workflow_type = hello_world_workflow_type(&work_dir);
    let config = hello_world_config();

    let instance = WorkflowInstance::create(workflow_type, config);
    let registry = hello_world_registry();
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    // Override the context work_dir to point to our temp dir
    runner.set_work_dir(work_dir.clone());

    let outcome = runner.run().expect("Workflow should not return an error");

    assert_eq!(
        outcome,
        RunOutcome::Success,
        "Hello-world workflow should complete successfully"
    );

    // Verify artifacts were created
    assert!(
        work_dir.join("src/lib.rs").exists(),
        "lib.rs should exist after workflow"
    );
    assert!(
        work_dir.join("tests/greeting_test.rs").exists(),
        "test file should exist after workflow"
    );
}

/// @plan:PLAN-20260408-STEP-EXEC.P07
/// @requirement:REQ-EXEC-001
/// GIVEN: a 2-step workflow with shell steps
/// WHEN: engine runs
/// THEN: both steps execute via `ShellExecutor`
#[test]
fn test_engine_dispatches_to_shell_executor() {
    let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let work_dir = temp_dir.path().to_path_buf();

    let workflow_type = WorkflowType {
        workflow_type_id: "shell-test".to_string(),
        steps: vec![
            StepDef {
                step_id: "step_a".to_string(),
                step_type: "shell".to_string(),
                description: None,
                produces: None,
                consumes: None,
                terminal: None,
                parameters: Some(serde_json::json!({"command": "echo hello"})),
            },
            StepDef {
                step_id: "step_b".to_string(),
                step_type: "shell".to_string(),
                description: None,
                produces: None,
                consumes: None,
                terminal: None,
                parameters: Some(serde_json::json!({"command": "echo world"})),
            },
        ],
        transitions: vec![TransitionDef {
            from: "step_a".to_string(),
            to: "step_b".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        }],
        guards: GuardConfig::default(),
    };

    let config = hello_world_config();
    let instance = WorkflowInstance::create(workflow_type, config);
    let registry = hello_world_registry();
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");
    runner.set_work_dir(work_dir);

    let outcome = runner.run().expect("Should not error");
    assert_eq!(outcome, RunOutcome::Success);
}

/// @plan:PLAN-20260408-STEP-EXEC.P07
/// @requirement:REQ-EXEC-001
/// GIVEN: a workflow with `write_file` steps
/// WHEN: engine runs
/// THEN: files are created on disk
#[test]
fn test_engine_dispatches_to_write_file_executor() {
    let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let work_dir = temp_dir.path().to_path_buf();

    let workflow_type = WorkflowType {
        workflow_type_id: "write-test".to_string(),
        steps: vec![StepDef {
            step_id: "write_step".to_string(),
            step_type: "write_file".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: Some(serde_json::json!({
                "path": "output.txt",
                "content": "hello from workflow"
            })),
        }],
        transitions: vec![],
        guards: GuardConfig::default(),
    };

    let config = hello_world_config();
    let instance = WorkflowInstance::create(workflow_type, config);
    let registry = hello_world_registry();
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");
    runner.set_work_dir(work_dir.clone());

    let outcome = runner.run().expect("Should not error");
    assert_eq!(outcome, RunOutcome::Success);

    let content = std::fs::read_to_string(work_dir.join("output.txt")).expect("File should exist");
    assert_eq!(content, "hello from workflow");
}

/// @plan:PLAN-20260408-STEP-EXEC.P07
/// @requirement:REQ-EXEC-005
/// GIVEN: step A sets a context value, step B uses it
/// WHEN: engine runs both steps
/// THEN: step B can read step A's output
#[test]
fn test_context_passes_between_steps_through_engine() {
    let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let work_dir = temp_dir.path().to_path_buf();

    let workflow_type = WorkflowType {
        workflow_type_id: "context-test".to_string(),
        steps: vec![
            StepDef {
                step_id: "produce".to_string(),
                step_type: "shell".to_string(),
                description: None,
                produces: None,
                consumes: None,
                terminal: None,
                parameters: Some(serde_json::json!({"command": "echo context_value_123"})),
            },
            StepDef {
                step_id: "consume".to_string(),
                step_type: "write_file".to_string(),
                description: None,
                produces: None,
                consumes: None,
                terminal: None,
                parameters: Some(serde_json::json!({
                    "path": "captured.txt",
                    "content": "{stdout}"
                })),
            },
        ],
        transitions: vec![TransitionDef {
            from: "produce".to_string(),
            to: "consume".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        }],
        guards: GuardConfig::default(),
    };

    let config = hello_world_config();
    let instance = WorkflowInstance::create(workflow_type, config);
    let registry = hello_world_registry();
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");
    runner.set_work_dir(work_dir.clone());

    let outcome = runner.run().expect("Should not error");
    assert_eq!(outcome, RunOutcome::Success);

    let content =
        std::fs::read_to_string(work_dir.join("captured.txt")).expect("File should exist");
    assert!(
        content.contains("context_value_123"),
        "Captured file should contain stdout from shell step, got: '{content}'"
    );
}

/// @plan:PLAN-20260408-STEP-EXEC.P07
/// @requirement:REQ-EXEC-002
/// GIVEN: a workflow with an unregistered `step_type`
/// WHEN: engine runs
/// THEN: outcome is Failure (unregistered type is fatal)
#[test]
fn test_unregistered_step_type_through_engine_produces_failure() {
    let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let work_dir = temp_dir.path().to_path_buf();

    let workflow_type = WorkflowType {
        workflow_type_id: "unregistered-test".to_string(),
        steps: vec![StepDef {
            step_id: "bad_step".to_string(),
            step_type: "nonexistent_executor".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: Some(serde_json::json!({})),
        }],
        transitions: vec![],
        guards: GuardConfig::default(),
    };

    let config = hello_world_config();
    let instance = WorkflowInstance::create(workflow_type, config);
    let registry = hello_world_registry(); // has shell + write_file, NOT nonexistent_executor
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");
    runner.set_work_dir(work_dir);

    let result = runner.run();
    // Should be an error because the executor is not registered
    assert!(
        result.is_err(),
        "Unregistered step_type should cause engine error"
    );
}
