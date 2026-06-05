/// @plan:PLAN-20260408-LLXPRT-FIRST.P04
/// Enhanced `ShellExecutor` TDD tests for JSON parsing, stdin piping, outcome mapping.
/// These tests expect REAL behavior and will fail until Phase 05 implementation.
use luther_workflow::engine::executor::{StepContext, StepExecutor};
use luther_workflow::engine::executors::ShellExecutor;
use luther_workflow::engine::transition::StepOutcome;
use serde_json::json;

// =============================================================================
// JSON Output Parsing Tests (REQ-LF-SHELL-001)
// =============================================================================

/// @plan:PLAN-20260408-LLXPRT-FIRST.P04
/// @requirement:REQ-LF-SHELL-001
#[test]
fn test_shell_json_output_parsing_extracts_fields_to_context() {
    let executor = ShellExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());
    let params = json!({
        "command": "echo '{\"title\":\"Bug\",\"number\":42}'",
        "output_format": "json",
        "context_map": {
            "issue_title": ".title",
            "issue_num": ".number"
        }
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Success);
    assert_eq!(ctx.get("issue_title"), Some(&"Bug".to_string()));
    assert_eq!(ctx.get("issue_num"), Some(&"42".to_string()));
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P04
/// @requirement:REQ-LF-SHELL-001
#[test]
fn test_shell_json_nested_dot_path_extracts_deep_values() {
    let executor = ShellExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());
    let params = json!({
        "command": "echo '{\"data\":{\"stats\":{\"count\":7}}}'",
        "output_format": "json",
        "context_map": {
            "stats_count": ".data.stats.count"
        }
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(ctx.get("stats_count"), Some(&"7".to_string()));
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P04
/// @requirement:REQ-LF-SHELL-001
#[test]
fn test_shell_json_array_value_stored_as_json_string() {
    let executor = ShellExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());
    let params = json!({
        "command": "echo '{\"labels\":[\"bug\",\"urgent\"]}'",
        "output_format": "json",
        "context_map": {
            "issue_labels": ".labels"
        }
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    let labels = ctx.get("issue_labels").expect("labels should be set");
    // Should be stored as JSON array string
    assert!(labels.contains("bug"));
    assert!(labels.contains("urgent"));
    assert!(labels.contains('[') && labels.contains(']'));
}

// =============================================================================
// JSON Error Handling Tests (REQ-LF-SHELL-002, REQ-LF-SHELL-009)
// =============================================================================

/// @plan:PLAN-20260408-LLXPRT-FIRST.P04
/// @requirement:REQ-LF-SHELL-002
#[test]
fn test_shell_json_invalid_stdout_returns_fatal() {
    let executor = ShellExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());
    let params = json!({
        "command": "echo 'not json at all'",
        "output_format": "json",
        "context_map": {
            "data": ".value"
        }
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Fatal);
    assert!(ctx.get("json_parse_error").is_some());
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P04
/// @requirement:REQ-LF-SHELL-009
#[test]
fn test_shell_json_missing_dot_path_returns_fatal_with_available_keys() {
    let executor = ShellExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());
    let params = json!({
        "command": "echo '{\"title\":\"test\",\"body\":\"content\"}'",
        "output_format": "json",
        "context_map": {
            "foo": ".nonexistent"
        }
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Fatal);
    let error_info = ctx
        .get("json_path_error")
        .expect("error info should be set");
    assert!(error_info.contains("nonexistent"));
    assert!(error_info.contains("title") || error_info.contains("body"));
}

// =============================================================================
// Stdin Piping Tests (REQ-LF-SHELL-003, REQ-LF-SHELL-004, REQ-LF-SHELL-008)
// =============================================================================

/// @plan:PLAN-20260408-LLXPRT-FIRST.P04
/// @requirement:REQ-LF-SHELL-003
#[test]
fn test_shell_stdin_pipes_value_to_command() {
    let executor = ShellExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());
    let params = json!({
        "command": "cat",
        "stdin": "piped input"
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Success);
    let stdout = ctx.get("stdout").expect("stdout should be set");
    assert!(stdout.contains("piped input"));
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P04
/// @requirement:REQ-LF-SHELL-003
#[test]
fn test_shell_stdin_interpolates_context_variables() {
    let executor = ShellExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());
    ctx.set("greeting", "hello");
    let params = json!({
        "command": "cat",
        "stdin": "{greeting} world"
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    let stdout = ctx.get("stdout").expect("stdout should be set");
    assert!(stdout.contains("hello world"));
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P04
/// @requirement:REQ-LF-SHELL-004
#[test]
fn test_shell_stdin_file_pipes_file_contents() {
    let executor = ShellExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let work_dir = temp_dir.path();
    let input_file = work_dir.join("input.txt");
    std::fs::write(&input_file, "file contents").unwrap();

    let mut ctx = StepContext::new(work_dir.to_path_buf(), "run-1".to_string());
    let params = json!({
        "command": "cat",
        "stdin_file": "input.txt"
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Success);
    let stdout = ctx.get("stdout").expect("stdout should be set");
    assert!(stdout.contains("file contents"));
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P04
/// @requirement:REQ-LF-SHELL-008
#[test]
fn test_shell_stdin_file_missing_returns_fatal() {
    let executor = ShellExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());
    let params = json!({
        "command": "cat",
        "stdin_file": "does_not_exist.txt"
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Fatal);
}

// =============================================================================
// Outcome Pattern Matching Tests (REQ-LF-SHELL-005, REQ-LF-SHELL-006, REQ-LF-SHELL-007)
// =============================================================================

/// @plan:PLAN-20260408-LLXPRT-FIRST.P04
/// @requirement:REQ-LF-SHELL-005
#[test]
fn test_shell_outcome_on_stdout_maps_matching_string_to_outcome() {
    let executor = ShellExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());
    let params = json!({
        "command": "echo 'The plan is PLAN_APPROVED'",
        "outcome_on_stdout": {
            "PLAN_APPROVED": "success",
            "PLAN_NEEDS_REVISION": "fixable"
        }
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Success);
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P04
/// @requirement:REQ-LF-SHELL-005
#[test]
fn test_shell_outcome_on_stdout_fixable_mapping() {
    let executor = ShellExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());
    let params = json!({
        "command": "echo 'PLAN_NEEDS_REVISION'",
        "outcome_on_stdout": {
            "PLAN_APPROVED": "success",
            "PLAN_NEEDS_REVISION": "fixable"
        }
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Fixable);
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P04
/// @requirement:REQ-LF-SHELL-006
#[test]
fn test_shell_outcome_on_stdout_no_match_defaults_to_success() {
    let executor = ShellExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());
    let params = json!({
        "command": "echo 'random output that does not match'",
        "outcome_on_stdout": {
            "MAGIC": "fixable"
        }
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Success);
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P04
/// @requirement:REQ-LF-SHELL-007
#[test]
fn test_shell_outcome_on_stdout_nonzero_exit_returns_fixable_regardless() {
    let executor = ShellExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());
    let params = json!({
        "command": "echo 'APPROVED' && exit 1",
        "outcome_on_stdout": {
            "APPROVED": "success"
        }
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    // Non-zero exit code takes precedence over stdout pattern match
    assert_eq!(result.unwrap(), StepOutcome::Fixable);
}

// =============================================================================
// Backward Compatibility Test
// =============================================================================

/// @plan:PLAN-20260408-LLXPRT-FIRST.P04
/// @requirement:REQ-LF-SHELL-001
#[test]
fn test_shell_without_new_params_works_as_before() {
    let executor = ShellExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());
    // Just command - no output_format, no stdin, no outcome_on_stdout
    let params = json!({
        "command": "echo hello"
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Success);
    let stdout = ctx.get("stdout").expect("stdout should be set");
    assert!(stdout.contains("hello"));
}

// =============================================================================
// Exit Code Mapping Tests (REQ-LF-SHELL-010)
// =============================================================================

/// @plan:PLAN-20260408-LLXPRT-FIRST.P04
/// @requirement:REQ-LF-SHELL-010
#[test]
fn test_shell_exit_code_map_maps_nonzero_exit_to_specified_outcome() {
    let executor = ShellExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());
    let params = json!({
        "command": "exit 1",
        "exit_code_map": {
            "1": "fatal"
        }
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Fatal);
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P04
/// @requirement:REQ-LF-SHELL-010
#[test]
fn test_shell_exit_code_map_unmapped_nonzero_defaults_to_fixable() {
    let executor = ShellExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());
    let params = json!({
        "command": "exit 3",
        "exit_code_map": {
            "1": "fatal",
            "2": "fixable"
        }
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    // Exit code 3 is not in the map, should default to Fixable (standard non-zero behavior)
    assert_eq!(result.unwrap(), StepOutcome::Fixable);
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P04
/// @requirement:REQ-LF-SHELL-010
#[test]
fn test_shell_exit_code_map_zero_exit_ignores_map() {
    let executor = ShellExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());
    let params = json!({
        "command": "echo 'ok'",
        "exit_code_map": {
            "0": "fatal"
        }
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    // Exit code 0 is never mapped - always Success (unless outcome_on_stdout says otherwise)
    assert_eq!(result.unwrap(), StepOutcome::Success);
}

#[test]
fn test_shell_timeout_returns_fatal_and_records_diagnostic() {
    let executor = ShellExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());
    let params = json!({
        "command": "sleep 5",
        "timeout_seconds": 1
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Fatal);
    assert_eq!(ctx.get("exit_code"), Some(&"124".to_string()));
    assert!(ctx
        .get("diagnostic")
        .is_some_and(|diagnostic| diagnostic.contains("timed out")));
}
