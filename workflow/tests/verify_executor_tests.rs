/// @plan:PLAN-20260408-LLXPRT-FIRST.P07
/// `VerifyExecutor` TDD tests - all tests expect contract failures for the Phase 08 implementation contract.
/// These tests verify the `VerifyExecutor` behavior for configurable check sequences,
/// result parsing, report generation, and context variable setting.
use luther_workflow::engine::executor::{StepContext, StepExecutor};
use luther_workflow::engine::executors::verify::{
    profile_default_command, resolve_check_command, ErrorRecord, VerifyExecutor, VerifyReport,
};
use luther_workflow::engine::runner::EngineError;
use luther_workflow::engine::transition::StepOutcome;
use serde_json::json;
use std::fs;

// =============================================================================
// REQ-LF-VERIFY-002: All checks pass returns Success
// =============================================================================

/// @plan:PLAN-20260408-LLXPRT-FIRST.P07
/// @requirement:REQ-LF-VERIFY-002
#[test]
fn test_verify_all_checks_pass_returns_success() {
    let executor = VerifyExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());

    let params = json!({
        "checks": ["typecheck", "test"],
        "check_commands": {
            "typecheck": "echo 'typecheck passed'",
            "test": "echo 'test passed'"
        }
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Success);
    assert_eq!(ctx.get("verify_passed"), Some(&"true".to_string()));
}

// =============================================================================
// REQ-LF-VERIFY-003: Any check fails returns Fixable with report
// =============================================================================

/// @plan:PLAN-20260408-LLXPRT-FIRST.P07
/// @requirement:REQ-LF-VERIFY-003
#[test]
fn test_verify_any_check_fails_returns_fixable() {
    let executor = VerifyExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());

    let params = json!({
        "checks": ["typecheck", "test"],
        "check_commands": {
            "typecheck": "echo 'typecheck passed'",
            "test": "echo 'test failed' && exit 1"
        }
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Fixable);
    assert_eq!(ctx.get("verify_passed"), Some(&"false".to_string()));
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P07
/// @requirement:REQ-LF-VERIFY-003
#[test]
fn test_verify_writes_report_file_on_failure() {
    let executor = VerifyExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let work_dir = temp_dir.path();
    let mut ctx = StepContext::new(work_dir.to_path_buf(), "run-1".to_string());

    let params = json!({
        "checks": ["lint"],
        "check_commands": {
            "lint": "echo 'lint error' && exit 1"
        }
    });

    let result = executor.execute(&mut ctx, &params);
    assert!(result.is_ok());

    // Verify report file exists
    let report_path = work_dir.join(".luther").join("verify-report.json");
    assert!(
        report_path.exists(),
        "Report file should exist at {report_path:?}"
    );

    // Verify it's valid JSON with passed: false
    let report_content = fs::read_to_string(&report_path).unwrap();
    let report: VerifyReport = serde_json::from_str(&report_content).unwrap();
    assert!(!report.passed);
}

// =============================================================================
// REQ-LF-VERIFY-005: Structured error details
// =============================================================================

/// @plan:PLAN-20260408-LLXPRT-FIRST.P07
/// @requirement:REQ-LF-VERIFY-005
#[test]
fn test_verify_report_contains_per_check_results() {
    let executor = VerifyExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let work_dir = temp_dir.path();
    let mut ctx = StepContext::new(work_dir.to_path_buf(), "run-1".to_string());

    let params = json!({
        "checks": ["lint", "typecheck"],
        "check_commands": {
            "lint": "echo 'lint passed'",
            "typecheck": "echo 'typecheck failed' && exit 1"
        }
    });

    let _result = executor.execute(&mut ctx, &params);

    // Parse report and verify 2 check entries
    let report_path = work_dir.join(".luther").join("verify-report.json");
    let report_content = fs::read_to_string(&report_path).unwrap();
    let report: VerifyReport = serde_json::from_str(&report_content).unwrap();

    assert_eq!(report.checks.len(), 2);

    // Find lint check (should be passed)
    let lint_check = report
        .checks
        .iter()
        .find(|c| c.check_type == "lint")
        .expect("lint check should exist");
    assert!(lint_check.passed);

    // Find typecheck check (should be failed)
    let typecheck_check = report
        .checks
        .iter()
        .find(|c| c.check_type == "typecheck")
        .expect("typecheck check should exist");
    assert!(!typecheck_check.passed);
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P07
/// @requirement:REQ-LF-VERIFY-004
#[test]
fn test_verify_summary_contains_all_check_statuses() {
    let executor = VerifyExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());

    let params = json!({
        "checks": ["lint", "typecheck"],
        "check_commands": {
            "lint": "echo 'lint passed'",
            "typecheck": "printf 'src/foo.ts(10,5): error TS2322: Type X is not assignable to Type Y' && exit 1"
        }
    });

    let _result = executor.execute(&mut ctx, &params);

    let summary = ctx
        .get("verify_summary")
        .expect("verify_summary should be set");
    assert!(
        summary.contains("lint"),
        "Summary should contain lint status"
    );
    assert!(
        summary.contains("typecheck"),
        "Summary should contain typecheck status"
    );
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P07
/// @requirement:REQ-LF-VERIFY-005
#[test]
fn test_verify_typescript_error_parser_extracts_file_and_line() {
    let executor = VerifyExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let work_dir = temp_dir.path();
    let mut ctx = StepContext::new(work_dir.to_path_buf(), "run-1".to_string());

    let params = json!({
        "checks": ["typecheck"],
        "check_commands": {
            "typecheck": "printf 'src/foo.ts(10,5): error TS2322: Type X is not assignable to Type Y' && exit 1"
        }
    });

    let _result = executor.execute(&mut ctx, &params);

    // Parse report to verify error extraction
    let report_path = work_dir.join(".luther").join("verify-report.json");
    let report_content = fs::read_to_string(&report_path).unwrap();
    let report: VerifyReport = serde_json::from_str(&report_content).unwrap();

    let typecheck = report
        .checks
        .iter()
        .find(|c| c.check_type == "typecheck")
        .expect("typecheck check should exist");
    assert!(!typecheck.passed);
    assert!(!typecheck.errors.is_empty(), "Should have parsed errors");

    let error = &typecheck.errors[0];
    assert_eq!(error.file, Some("src/foo.ts".to_string()));
    assert_eq!(error.line, Some(10));
    assert!(error.message.contains("TS2322"));
}

// =============================================================================
// REQ-LF-VERIFY-006: Test failure details
// =============================================================================

/// @plan:PLAN-20260408-LLXPRT-FIRST.P07
/// @requirement:REQ-LF-VERIFY-006
#[test]
fn test_verify_test_parser_extracts_test_name_and_failure() {
    let executor = VerifyExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let work_dir = temp_dir.path();
    let mut ctx = StepContext::new(work_dir.to_path_buf(), "run-1".to_string());

    // Simulate vitest JSON output
    let vitest_json = json!({
        "testResults": [
            {
                "name": "/test/unit.spec.ts",
                "assertionResults": [
                    {
                        "fullName": "test unit should work",
                        "status": "failed",
                        "failureMessages": ["Expected 5, received 3"]
                    }
                ]
            }
        ]
    });

    let params = json!({
        "checks": ["test"],
        "check_commands": {
            "test": format!("echo '{}' && exit 1", serde_json::to_string(&vitest_json).unwrap().replace('"', "\\\""))
        }
    });

    let _result = executor.execute(&mut ctx, &params);

    // Check test_failures context variable
    let test_failures_json = ctx
        .get("test_failures")
        .expect("test_failures should be set");
    let test_failures: Vec<ErrorRecord> = serde_json::from_str(test_failures_json).unwrap();

    assert!(!test_failures.is_empty(), "Should have test failures");
    let error = &test_failures[0];
    assert_eq!(error.test_name, Some("test unit should work".to_string()));
    assert!(error.message.contains("Expected 5"));
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P07
/// @requirement:REQ-LF-VERIFY-005
#[test]
fn test_verify_lint_parser_extracts_eslint_json() {
    let executor = VerifyExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let work_dir = temp_dir.path();
    let mut ctx = StepContext::new(work_dir.to_path_buf(), "run-1".to_string());

    // Simulate eslint JSON output
    let eslint_json = json!([
        {
            "filePath": "/src/app.ts",
            "messages": [
                {
                    "line": 15,
                    "column": 3,
                    "message": "Unexpected console statement",
                    "severity": 2
                }
            ]
        }
    ]);

    let params = json!({
        "checks": ["lint"],
        "check_commands": {
            "lint": format!("echo '{}' && exit 1", serde_json::to_string(&eslint_json).unwrap().replace('"', "\\\""))
        }
    });

    let _result = executor.execute(&mut ctx, &params);

    // Parse report to verify eslint error extraction
    let report_path = work_dir.join(".luther").join("verify-report.json");
    let report_content = fs::read_to_string(&report_path).unwrap();
    let report: VerifyReport = serde_json::from_str(&report_content).unwrap();

    let lint_check = report
        .checks
        .iter()
        .find(|c| c.check_type == "lint")
        .expect("lint check should exist");
    assert!(!lint_check.passed);
    assert!(
        !lint_check.errors.is_empty(),
        "Should have parsed lint errors"
    );

    let error = &lint_check.errors[0];
    assert_eq!(error.file, Some("/src/app.ts".to_string()));
    assert_eq!(error.line, Some(15));
    assert_eq!(error.column, Some(3));
    assert!(error.message.contains("console statement"));
}

// =============================================================================
// REQ-LF-VERIFY-007: Parameterized check suite
// =============================================================================

/// @plan:PLAN-20260408-LLXPRT-FIRST.P07
/// @requirement:REQ-LF-VERIFY-007
#[test]
fn test_verify_custom_check_commands_override_defaults() {
    let executor = VerifyExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let work_dir = temp_dir.path();
    let mut ctx = StepContext::new(work_dir.to_path_buf(), "run-1".to_string());

    let params = json!({
        "checks": ["lint"],
        "check_commands": {
            "lint": "echo 'custom_lint_output'"
        }
    });

    let _result = executor.execute(&mut ctx, &params);

    // Parse report to verify custom command was used
    let report_path = work_dir.join(".luther").join("verify-report.json");
    let report_content = fs::read_to_string(&report_path).unwrap();
    let report: VerifyReport = serde_json::from_str(&report_content).unwrap();

    let lint_check = report
        .checks
        .iter()
        .find(|c| c.check_type == "lint")
        .expect("lint check should exist");
    assert!(lint_check.raw_stdout.contains("custom_lint_output"));
}

// =============================================================================
// REQ-LF-VERIFY-008: Unspawnable command returns Fatal
// =============================================================================

/// @plan:PLAN-20260408-LLXPRT-FIRST.P07
/// @requirement:REQ-LF-VERIFY-008
#[test]
fn test_verify_unspawnable_command_returns_fatal() {
    let executor = VerifyExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());

    let params = json!({
        "checks": ["lint"],
        "check_commands": {
            "lint": "/nonexistent/binary/that/cannot/spawn"
        }
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Fatal);
    assert!(ctx.get("verify_error").is_some());
}

// =============================================================================
// REQ-LF-VERIFY-009: Per-check-type context variables
// =============================================================================

/// @plan:PLAN-20260408-LLXPRT-FIRST.P07
/// @requirement:REQ-LF-VERIFY-009
#[test]
fn test_verify_sets_per_check_type_context_variables() {
    let executor = VerifyExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let work_dir = temp_dir.path();
    let mut ctx = StepContext::new(work_dir.to_path_buf(), "run-1".to_string());

    let params = json!({
        "checks": ["test"],
        "check_commands": {
            "test": "echo 'test failure' && exit 1"
        }
    });

    let _result = executor.execute(&mut ctx, &params);

    // Verify test_failures context variable is a valid JSON array
    let test_failures_json = ctx
        .get("test_failures")
        .expect("test_failures should be set");
    let test_failures: Vec<ErrorRecord> =
        serde_json::from_str(test_failures_json).expect("Should be valid JSON array");
    assert!(!test_failures.is_empty());
}

// =============================================================================
// REQ-LF-VERIFY-001: Configurable check sequence
// =============================================================================

/// @plan:PLAN-20260408-LLXPRT-FIRST.P07
/// @requirement:REQ-LF-VERIFY-001
#[test]
fn test_verify_runs_only_configured_checks() {
    let executor = VerifyExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let work_dir = temp_dir.path();
    let mut ctx = StepContext::new(work_dir.to_path_buf(), "run-1".to_string());

    let params = json!({
        "checks": ["lint"],
        "check_commands": {
            "lint": "echo 'only lint'",
            "test": "echo 'this should not run'"
        }
    });

    let _result = executor.execute(&mut ctx, &params);

    // Parse report and verify only 1 check ran
    let report_path = work_dir.join(".luther").join("verify-report.json");
    let report_content = fs::read_to_string(&report_path).unwrap();
    let report: VerifyReport = serde_json::from_str(&report_content).unwrap();

    assert_eq!(report.checks.len(), 1);
    assert_eq!(report.checks[0].check_type, "lint");
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P07
/// @requirement:REQ-LF-VERIFY-001
#[test]
fn test_verify_empty_checks_array_returns_success() {
    let executor = VerifyExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());

    let params = json!({
        "checks": []
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Success);
    assert_eq!(ctx.get("verify_passed"), Some(&"true".to_string()));
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P07
/// @requirement:REQ-LF-VERIFY-005,REQ-LF-VERIFY-006
#[test]
fn test_verify_unparseable_output_produces_raw_error_record() {
    let executor = VerifyExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let work_dir = temp_dir.path();
    let mut ctx = StepContext::new(work_dir.to_path_buf(), "run-1".to_string());

    let params = json!({
        "checks": ["lint"],
        "check_commands": {
            "lint": "echo 'garbage garbage garbage' && exit 1"
        }
    });

    let _result = executor.execute(&mut ctx, &params);

    // Parse report to verify raw error handling
    let report_path = work_dir.join(".luther").join("verify-report.json");
    let report_content = fs::read_to_string(&report_path).unwrap();
    let report: VerifyReport = serde_json::from_str(&report_content).unwrap();

    let lint_check = report
        .checks
        .iter()
        .find(|c| c.check_type == "lint")
        .expect("lint check should exist");
    assert!(!lint_check.passed);
    assert_ne!(lint_check.exit_code, 0);
    assert!(lint_check.raw_stdout.contains("garbage") || lint_check.raw_stderr.contains("garbage"));
    assert!(
        !lint_check.errors.is_empty(),
        "Should have at least one error record"
    );

    let error = &lint_check.errors[0];
    assert!(
        error.message.contains("garbage"),
        "Error message should contain raw output"
    );
    assert_eq!(error.severity, Some("error".to_string()));
}

#[test]
fn test_verify_diff_check_fails_when_no_changes_exist() {
    let executor = VerifyExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let work_dir = temp_dir.path();
    let mut ctx = StepContext::new(work_dir.to_path_buf(), "run-1".to_string());

    let params = json!({
        "checks": ["diff"],
        "check_commands": {
            "diff": "exit 1"
        }
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Fixable);
    assert_eq!(ctx.get("verify_passed"), Some(&"false".to_string()));

    let report_path = work_dir.join(".luther").join("verify-report.json");
    let report_content = fs::read_to_string(&report_path).unwrap();
    let report: VerifyReport = serde_json::from_str(&report_content).unwrap();
    let diff_check = report
        .checks
        .iter()
        .find(|check| check.check_type == "diff")
        .expect("diff check should exist");
    assert!(!diff_check.passed);
    assert_eq!(
        diff_check.errors[0].message,
        "No repository changes were produced"
    );
}

#[test]
fn test_verify_timeout_returns_fixable_and_records_report() {
    let executor = VerifyExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let work_dir = temp_dir.path();
    let mut ctx = StepContext::new(work_dir.to_path_buf(), "run-1".to_string());

    let params = json!({
        "checks": ["test"],
        "timeout_seconds": 1,
        "check_commands": {
            "test": "sleep 5"
        }
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Fixable);
    assert_eq!(ctx.get("verify_passed"), Some(&"false".to_string()));
    assert!(ctx.get("verify_error").unwrap().contains("timed out"));

    let report_path = work_dir.join(".luther").join("verify-report.json");
    let report_content = fs::read_to_string(&report_path).unwrap();
    let report: VerifyReport = serde_json::from_str(&report_content).unwrap();
    assert!(!report.passed);
    assert_eq!(report.checks[0].exit_code, 124);
}

#[test]
fn test_verify_drains_large_command_output_without_blocking() {
    let executor = VerifyExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let work_dir = temp_dir.path();
    let mut ctx = StepContext::new(work_dir.to_path_buf(), "run-1".to_string());

    let params = json!({
        "checks": ["lint"],
        "timeout_seconds": 5,
        "check_commands": {
            "lint": r#"python3 -c 'import sys; sys.stdout.write("x" * 200000)'"#
        }
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Success);
    assert_eq!(ctx.get("verify_passed"), Some(&"true".to_string()));

    let report_path = work_dir.join(".luther").join("verify-report.json");
    let report_content = fs::read_to_string(&report_path).unwrap();
    let report: VerifyReport = serde_json::from_str(&report_content).unwrap();
    assert!(report.passed);
    assert_eq!(report.checks[0].check_type, "lint");
    assert!(report.checks[0].raw_stdout.len() <= 20100);
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P17
/// @requirement:REQ-LF-VERIFY-007
#[test]
fn test_verify_interpolates_context_in_custom_check_commands() {
    let executor = VerifyExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let work_dir = temp_dir.path();
    let mut ctx = StepContext::new(work_dir.to_path_buf(), "run-1".to_string());
    ctx.set_current_step_id("setup_workspace");
    ctx.set("existing_pr_number", "0");
    ctx.set_current_step_id("run_tests");

    let params = json!({
        "checks": ["diff_or_existing_pr"],
        "check_commands": {
            "diff_or_existing_pr": "test \"{setup_workspace.existing_pr_number}\" != \"0\""
        }
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Fixable);

    let report_path = work_dir.join(".luther").join("verify-report.json");
    let report_content = fs::read_to_string(&report_path).unwrap();
    let report: VerifyReport = serde_json::from_str(&report_content).unwrap();
    let check = report
        .checks
        .iter()
        .find(|check| check.check_type == "diff_or_existing_pr")
        .expect("diff_or_existing_pr check should exist");
    assert!(!check.passed);
    assert_eq!(check.exit_code, 1);
}

#[test]
fn test_verify_parses_eslint_stylish_errors_without_storing_all_warnings_as_error() {
    let executor = VerifyExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let work_dir = temp_dir.path();
    let mut ctx = StepContext::new(work_dir.to_path_buf(), "run-1".to_string());

    let params = json!({
        "checks": ["lint"],
        "check_commands": {
            "lint": r#"cat <<'EOF'
/path/to/src/example.ts
  7:13  error    'unusedValue' is assigned a value but never used  @typescript-eslint/no-unused-vars
  8:5   warning  Unexpected any value in conditional              @typescript-eslint/strict-boolean-expressions

✖ 10002 problems (1 error, 10001 warnings)
EOF
exit 1"#
        }
    });

    let result = executor.execute(&mut ctx, &params);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Fixable);

    let report_path = work_dir.join(".luther").join("verify-report.json");
    let report_content = fs::read_to_string(&report_path).unwrap();
    let report: VerifyReport = serde_json::from_str(&report_content).unwrap();
    let lint_check = report
        .checks
        .iter()
        .find(|check| check.check_type == "lint")
        .expect("lint check should exist");
    assert!(!lint_check.passed);
    assert_eq!(lint_check.errors.len(), 1);
    assert_eq!(
        lint_check.errors[0].file,
        Some("/path/to/src/example.ts".to_string())
    );
    assert_eq!(lint_check.errors[0].line, Some(7));
    assert_eq!(lint_check.errors[0].column, Some(13));
    assert!(lint_check.errors[0].message.contains("unusedValue"));
    assert!(!lint_check.errors[0].message.contains("10001 warnings"));
}

// =============================================================================
// REQ-LF-VERIFY-007: Configurable verification profiles
// =============================================================================

/// Default profile (no `profile` param) resolves to the npm defaults,
/// preserving backward compatibility.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-007
#[test]
fn test_verify_default_profile_is_npm() {
    let temp_dir = tempfile::tempdir().unwrap();
    let ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());

    let params = json!({ "checks": ["lint"] });

    let command = resolve_check_command("lint", &params, &ctx).unwrap();
    assert_eq!(command, "npm run lint 2>&1");
}

/// Explicitly selecting the npm profile resolves identically to the default.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-007
#[test]
fn test_verify_explicit_npm_profile_matches_default() {
    let temp_dir = tempfile::tempdir().unwrap();
    let ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());

    let default_params = json!({ "checks": ["test"] });
    let npm_params = json!({ "checks": ["test"], "profile": "npm" });

    let default_cmd = resolve_check_command("test", &default_params, &ctx).unwrap();
    let npm_cmd = resolve_check_command("test", &npm_params, &ctx).unwrap();
    assert_eq!(default_cmd, npm_cmd);
    assert_eq!(npm_cmd, "npm run test 2>&1");
}

/// The cargo profile resolves to cargo commands rather than npm.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-007
#[test]
fn test_verify_cargo_profile_resolves_cargo_commands() {
    let temp_dir = tempfile::tempdir().unwrap();
    let ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());

    let params = json!({
        "checks": ["lint", "test", "build", "format"],
        "profile": "cargo"
    });

    assert_eq!(
        resolve_check_command("lint", &params, &ctx).unwrap(),
        "cargo clippy 2>&1"
    );
    assert_eq!(
        resolve_check_command("test", &params, &ctx).unwrap(),
        "cargo test 2>&1"
    );
    assert_eq!(
        resolve_check_command("build", &params, &ctx).unwrap(),
        "cargo build 2>&1"
    );
    assert_eq!(
        resolve_check_command("format", &params, &ctx).unwrap(),
        "cargo fmt --check 2>&1"
    );

    // cargo has no separate typecheck default.
    assert!(profile_default_command("cargo", "typecheck").is_none());
}

/// The custom profile, combined with explicit check_commands, succeeds.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-007
#[test]
fn test_verify_custom_profile_with_check_commands_succeeds() {
    let executor = VerifyExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());

    let params = json!({
        "checks": ["lint", "test"],
        "profile": "custom",
        "check_commands": {
            "lint": "echo 'lint passed'",
            "test": "echo 'test passed'"
        }
    });

    let result = executor.execute(&mut ctx, &params);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), StepOutcome::Success);
}

/// The custom profile without an override for a check type errors, since it
/// defines no defaults.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-007
#[test]
fn test_verify_custom_profile_missing_command_errors() {
    let temp_dir = tempfile::tempdir().unwrap();
    let ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());

    let params = json!({ "checks": ["lint"], "profile": "custom" });

    let result = resolve_check_command("lint", &params, &ctx);
    let Err(EngineError::StepExecutionError { message, .. }) = result else {
        panic!("expected StepExecutionError for custom profile without override");
    };
    assert!(message.contains("lint"));
    assert!(message.contains("custom"));
}

/// An explicit check_commands override beats the profile default, while
/// non-overridden check types still use the profile default.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-007
#[test]
fn test_verify_check_commands_override_profile_default() {
    let temp_dir = tempfile::tempdir().unwrap();
    let ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());

    let params = json!({
        "checks": ["lint", "test"],
        "profile": "cargo",
        "check_commands": {
            "lint": "echo 'override lint'"
        }
    });

    // Override wins for lint.
    assert_eq!(
        resolve_check_command("lint", &params, &ctx).unwrap(),
        "echo 'override lint'"
    );
    // Non-overridden type falls back to the cargo default.
    assert_eq!(
        resolve_check_command("test", &params, &ctx).unwrap(),
        "cargo test 2>&1"
    );
}

#[test]
fn test_verify_expands_manifest_group_placeholder() {
    let executor = VerifyExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());

    let params = json!({
        "checks": ["command_manifest"],
        "command_manifest_group": "local",
        "command_manifest": {
            "commands": [
                { "id": "alpha", "argv": ["python3", "-c", "print('alpha')"] },
                { "id": "beta", "argv": ["python3", "-c", "print('beta')"] }
            ],
            "groups": { "local": ["alpha", "beta"] }
        }
    });

    let result = executor
        .execute(&mut ctx, &params)
        .expect("verify executes");
    assert_eq!(result, StepOutcome::Success);

    let report_content =
        fs::read_to_string(temp_dir.path().join(".luther/verify-report.json")).unwrap();
    let report: VerifyReport = serde_json::from_str(&report_content).unwrap();
    let command_ids = report
        .checks
        .iter()
        .filter_map(|check| check.command.as_ref())
        .map(|command| command.command_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(command_ids, ["alpha", "beta"]);
}

#[test]
fn test_verify_manifest_group_runs_without_checks_array() {
    let executor = VerifyExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());

    let params = json!({
        "command_manifest": {
            "commands": [
                { "id": "coverage", "argv": ["python3", "-c", "print('covered')"] }
            ],
            "groups": { "local": ["coverage"] }
        }
    });

    let result = executor
        .execute(&mut ctx, &params)
        .expect("verify executes");
    assert_eq!(result, StepOutcome::Success);

    let report_content =
        fs::read_to_string(temp_dir.path().join(".luther/verify-report.json")).unwrap();
    let report: VerifyReport = serde_json::from_str(&report_content).unwrap();
    assert_eq!(report.checks[0].check_type, "coverage");
}

/// An unknown profile name is rejected with a StepExecutionError listing the
/// valid profiles.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-007
#[test]
fn test_verify_invalid_profile_returns_error() {
    let executor = VerifyExecutor;
    let temp_dir = tempfile::tempdir().unwrap();
    let mut ctx = StepContext::new(temp_dir.path().to_path_buf(), "run-1".to_string());

    let params = json!({ "checks": ["lint"], "profile": "ruby" });

    let result = executor.execute(&mut ctx, &params);
    let Err(EngineError::StepExecutionError { message, .. }) = result else {
        panic!("expected StepExecutionError for invalid profile");
    };
    assert!(message.contains("ruby"));
    assert!(message.contains("npm"));
    assert!(message.contains("cargo"));
    assert!(message.contains("custom"));
}
