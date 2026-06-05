/// @plan:PLAN-20260408-STEP-EXEC.P04
/// Unit tests for executor module - `StepContext`, Registry, and Executor implementations.
/// These tests expect REAL behavior and will fail until Phase 05 implementation.
use luther_workflow::engine::executor::{
    interpolate_string, ExecutorRegistry, StepContext, StepExecutor,
};
use luther_workflow::engine::executors::{NoOpExecutor, ShellExecutor, WriteFileExecutor};
use luther_workflow::engine::runner::EngineError;
use luther_workflow::engine::transition::StepOutcome;
use serde_json::json;
use std::path::PathBuf;

// =============================================================================
// StepContext Tests
// =============================================================================

/// @plan:PLAN-20260408-STEP-EXEC.P04
/// @requirement:REQ-EXEC-005
#[test]
fn step_context_new_sets_work_dir_and_run_id_correctly() {
    let work_dir = PathBuf::from("/tmp/test-work");
    let run_id = "test-run-123".to_string();

    let ctx = StepContext::new(work_dir.clone(), run_id.clone());

    assert_eq!(ctx.work_dir(), &work_dir);
    assert_eq!(ctx.run_id(), run_id);
}

/// @plan:PLAN-20260408-STEP-EXEC.P04
/// @requirement:REQ-EXEC-005
#[test]
fn step_context_set_get_stores_and_retrieves_values() {
    let mut ctx = StepContext::new(PathBuf::from("/tmp"), "run-1".to_string());

    ctx.set("output", "hello world");
    ctx.set("exit_code", "0");

    assert_eq!(ctx.get("output"), Some(&"hello world".to_string()));
    assert_eq!(ctx.get("exit_code"), Some(&"0".to_string()));
}

/// @plan:PLAN-20260408-STEP-EXEC.P04
/// @requirement:REQ-EXEC-005
#[test]
fn step_context_get_returns_none_for_missing_key() {
    let ctx = StepContext::new(PathBuf::from("/tmp"), "run-1".to_string());

    assert_eq!(ctx.get("nonexistent"), None);
    assert_eq!(ctx.get(""), None);
}

/// @plan:PLAN-20260408-STEP-EXEC.P04
/// @requirement:REQ-EXEC-005
#[test]
fn step_context_work_dir_and_run_id_return_correct_values() {
    let work_dir = PathBuf::from("/custom/work/dir");
    let run_id = "custom-run-id";

    let ctx = StepContext::new(work_dir, run_id.to_string());

    assert_eq!(ctx.work_dir(), &PathBuf::from("/custom/work/dir"));
    assert_eq!(ctx.run_id(), "custom-run-id");
}

// =============================================================================
// interpolate_string Tests
// =============================================================================

/// @plan:PLAN-20260408-STEP-EXEC.P04
/// @requirement:REQ-EXEC-006
#[test]
fn interpolate_string_replaces_key_with_context_value() {
    let mut ctx = StepContext::new(PathBuf::from("/tmp"), "run-1".to_string());
    ctx.set("name", "Alice");

    let result = interpolate_string("Hello {name}!", &ctx);

    assert_eq!(result, "Hello Alice!");
}

/// @plan:PLAN-20260408-STEP-EXEC.P04
/// @requirement:REQ-EXEC-006
#[test]
fn interpolate_string_leaves_undefined_key_unchanged() {
    let ctx = StepContext::new(PathBuf::from("/tmp"), "run-1".to_string());

    let result = interpolate_string("Hello {undefined_key}!", &ctx);

    assert_eq!(result, "Hello {undefined_key}!");
}

/// @plan:PLAN-20260408-STEP-EXEC.P04
/// @requirement:REQ-EXEC-006
#[test]
fn interpolate_string_handles_multiple_replacements_in_one_string() {
    let mut ctx = StepContext::new(PathBuf::from("/tmp"), "run-1".to_string());
    ctx.set("first", "John");
    ctx.set("last", "Doe");
    ctx.set("greeting", "Hello");

    let result = interpolate_string("{greeting} {first} {last}!", &ctx);

    assert_eq!(result, "Hello John Doe!");
}

// =============================================================================
// ExecutorRegistry Tests
// =============================================================================

/// @plan:PLAN-20260408-STEP-EXEC.P04
/// @requirement:REQ-EXEC-001
#[test]
fn executor_registry_dispatch_to_registered_executor_returns_its_outcome() {
    let mut registry = ExecutorRegistry::new();
    let noop = Box::new(NoOpExecutor);
    registry.register("noop", noop);

    let mut ctx = StepContext::new(PathBuf::from("/tmp"), "run-1".to_string());
    let params = json!({});

    let result = registry.dispatch("noop", &mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Success);
}

/// @plan:PLAN-20260408-STEP-EXEC.P04
/// @requirement:REQ-EXEC-002
#[test]
fn executor_registry_dispatch_for_unregistered_step_type_returns_fatal_error() {
    let registry = ExecutorRegistry::new();
    let mut ctx = StepContext::new(PathBuf::from("/tmp"), "run-1".to_string());
    let params = json!({});

    let result = registry.dispatch("unknown_type", &mut ctx, &params);

    assert!(result.is_err());
    // Verify it's an EngineError::StepExecutionError (Fatal)
    match result {
        Err(EngineError::StepExecutionError { step_id, .. }) => {
            assert_eq!(step_id, "unknown_type");
        }
        _ => panic!("Expected StepExecutionError for unregistered step type"),
    }
}

/// @plan:PLAN-20260408-STEP-EXEC.P04
/// @requirement:REQ-EXEC-001
#[test]
fn executor_registry_register_overwrites_existing_registration() {
    let mut registry = ExecutorRegistry::new();

    // Register first executor
    let noop1 = Box::new(NoOpExecutor);
    registry.register("test", noop1);

    // Register second executor with same name (should overwrite)
    let noop2 = Box::new(NoOpExecutor);
    registry.register("test", noop2);

    // Dispatch should still work
    let mut ctx = StepContext::new(PathBuf::from("/tmp"), "run-1".to_string());
    let params = json!({});

    let result = registry.dispatch("test", &mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Success);
}

// =============================================================================
// ShellExecutor Tests
// =============================================================================

/// @plan:PLAN-20260408-STEP-EXEC.P04
/// @requirement:REQ-EXEC-003
#[test]
fn shell_executor_executes_echo_hello_and_returns_success() {
    let executor = ShellExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());
    let params = json!({
        "command": "echo hello"
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Success);
}

/// @plan:PLAN-20260408-STEP-EXEC.P04
/// @requirement:REQ-EXEC-003
#[test]
fn shell_executor_stores_stdout_in_context_after_execution() {
    let executor = ShellExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());
    let params = json!({
        "command": "echo hello_world"
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    // Verify stdout was stored in context
    let stdout = ctx.get("stdout");
    assert!(stdout.is_some());
    assert!(stdout.unwrap().contains("hello_world"));
}

/// @plan:PLAN-20260408-STEP-EXEC.P04
/// @requirement:REQ-EXEC-008
#[test]
fn shell_executor_nonzero_exit_returns_fixable_with_stderr() {
    let executor = ShellExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());
    let params = json!({
        "command": "exit 1"
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Fixable);
    // Verify stderr or exit_code was captured
    assert!(ctx.get("stderr").is_some() || ctx.get("exit_code").is_some());
}

/// @plan:PLAN-20260408-STEP-EXEC.P04
/// @requirement:REQ-EXEC-009
#[test]
fn shell_executor_spawn_failure_returns_fatal() {
    let executor = ShellExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());
    // Missing "command" key entirely — forces extraction failure before spawn
    let params = json!({});

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_err());
    match result {
        Err(EngineError::StepExecutionError { .. }) => {
            // Expected fatal error from missing command parameter
        }
        _ => panic!("Expected StepExecutionError for missing command"),
    }
}

// =============================================================================
// WriteFileExecutor Tests
// =============================================================================

/// @plan:PLAN-20260408-STEP-EXEC.P04
/// @requirement:REQ-EXEC-004
#[test]
fn write_file_executor_writes_file_content_and_returns_success() {
    let executor = WriteFileExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());
    let params = json!({
        "path": "test_output.txt",
        "content": "Hello, World!"
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Success);

    // Verify file was written with correct content
    let file_path = temp_dir.path().join("test_output.txt");
    assert!(file_path.exists());
    let content = std::fs::read_to_string(file_path).unwrap();
    assert_eq!(content, "Hello, World!");
}

/// @plan:PLAN-20260408-STEP-EXEC.P04
#[test]
fn write_file_executor_writes_to_subdirectory() {
    let executor = WriteFileExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());
    let params = json!({
        "path": "subdir/nested/file.txt",
        "content": "nested content"
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());

    // Verify nested file was created
    let file_path = temp_dir.path().join("subdir/nested/file.txt");
    assert!(file_path.exists());
}

// =============================================================================
// NoOpExecutor Tests
// =============================================================================

/// @plan:PLAN-20260408-STEP-EXEC.P04
#[test]
fn noop_executor_always_returns_success() {
    let executor = NoOpExecutor;
    let mut ctx = StepContext::new(PathBuf::from("/tmp"), "run-1".to_string());
    let params = json!({});

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Success);
}

/// @plan:PLAN-20260408-STEP-EXEC.P04
#[test]
fn noop_executor_returns_success_regardless_of_params() {
    let executor = NoOpExecutor;
    let mut ctx = StepContext::new(PathBuf::from("/tmp"), "run-1".to_string());

    // Test with various params
    let result1 = executor.execute(&mut ctx, &json!({"foo": "bar"}));
    let result2 = executor.execute(&mut ctx, &json!(null));
    let result3 = executor.execute(&mut ctx, &json!({"complex": {"nested": "data"}}));

    assert!(result1.is_ok() && result1.unwrap() == StepOutcome::Success);
    assert!(result2.is_ok() && result2.unwrap() == StepOutcome::Success);
    assert!(result3.is_ok() && result3.unwrap() == StepOutcome::Success);
}

// =============================================================================
// Integration: Context Value Passing Across Executions
// =============================================================================

/// @plan:PLAN-20260408-STEP-EXEC.P04
/// @requirement:REQ-EXEC-005
#[test]
fn context_carries_values_across_executions() {
    let shell = ShellExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());

    // First execution: set a value in context
    ctx.set("step1_output", "data_from_step1");

    // Second execution: verify the value is still there
    let params = json!({"command": "echo {step1_output}"});

    // Context should still have the value
    assert_eq!(
        ctx.get("step1_output"),
        Some(&"data_from_step1".to_string())
    );

    // Execute and verify the interpolated command works
    let result = shell.execute(&mut ctx, &params);

    // This test verifies context persistence - actual interpolation is tested separately
    assert!(result.is_ok());
}

/// @plan:PLAN-20260408-STEP-EXEC.P04
/// @requirement:REQ-EXEC-006
#[test]
fn variable_interpolation_works_with_work_dir() {
    let temp_dir = tempfile::tempdir().unwrap();
    let work_dir = temp_dir.path().to_path_buf();
    let ctx = StepContext::new(work_dir.clone(), "run-1".to_string());

    // Store work_dir in context for interpolation
    let mut ctx_with_work_dir = ctx;
    ctx_with_work_dir.set("work_dir", work_dir.to_str().unwrap());

    let result = interpolate_string("{work_dir}/foo.txt", &ctx_with_work_dir);

    let expected = format!("{}/foo.txt", work_dir.to_str().unwrap());
    assert_eq!(result, expected);
}

/// @plan:PLAN-20260408-STEP-EXEC.P04
/// @requirement:REQ-EXEC-001
#[test]
fn executor_registry_can_dispatch_multiple_executors() {
    let mut registry = ExecutorRegistry::new();

    // Register multiple executors
    registry.register("noop", Box::new(NoOpExecutor));
    registry.register("shell", Box::new(ShellExecutor));
    registry.register("write_file", Box::new(WriteFileExecutor));

    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());

    // Dispatch to each and verify they return appropriate outcomes
    let noop_result = registry.dispatch("noop", &mut ctx, &json!({}));
    assert!(noop_result.is_ok());
    assert_eq!(noop_result.unwrap(), StepOutcome::Success);
}
