/// @plan:PLAN-20260404-INITIAL-RUNTIME.P11
/// Integration tests for CLI End-to-End behavior.
///
/// These tests verify the behavioral requirements for CLI commands,
/// including run, status, help, and error handling.
use std::process::Command;

/// Helper to get the path to the luther-workflow binary.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P11
fn luther_workflow_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_luther-workflow"))
}

/// Test: CLI run command exists and is recognized.
/// GIVEN: the luther-workflow binary
/// WHEN: running with "run" subcommand
/// THEN: command is recognized (may fail with incomplete implementation but exists)
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P11
/// @requirement:REQ-EARS-QUAL-001
#[test]
fn test_cli_run_command_exists() {
    // GIVEN: the luther-workflow binary
    let mut cmd = luther_workflow_bin();
    cmd.arg("run");

    // WHEN: running with "run" subcommand
    let output = cmd.output().expect("Failed to execute run command");

    // THEN: command should exist (exit code 0 or have "run" in output/error)
    // In RED phase, it may fail with an incomplete implementation but should not be "unknown command"
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // The command should be recognized - either succeed or fail in a known way
    // (not "Unknown command" which indicates the command doesn't exist)
    let is_recognized = !stderr.contains("Unknown command") && !stdout.contains("Unknown command");
    assert!(
        is_recognized,
        "run command should exist and be recognized. stdout: {stdout}, stderr: {stderr}"
    );
}

/// Test: CLI status command exists and is recognized.
/// GIVEN: the luther-workflow binary
/// WHEN: running with "status" subcommand
/// THEN: command is recognized (may fail with incomplete implementation but exists)
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P11
/// @requirement:REQ-EARS-QUAL-001
#[test]
fn test_cli_status_command_exists() {
    // GIVEN: the luther-workflow binary
    let mut cmd = luther_workflow_bin();
    cmd.arg("status");

    // WHEN: running with "status" subcommand
    let output = cmd.output().expect("Failed to execute status command");

    // THEN: command should exist (exit code 0 or have "status" in output/error)
    // In RED phase, it may fail with an incomplete implementation but should not be "unknown command"
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // The command should be recognized - either succeed or fail in a known way
    // (not "Unknown command" which indicates the command doesn't exist)
    let is_recognized = !stderr.contains("Unknown command") && !stdout.contains("Unknown command");
    assert!(
        is_recognized,
        "status command should exist and be recognized. stdout: {stdout}, stderr: {stderr}"
    );
}

/// Test: CLI help flag displays usage information.
/// GIVEN: the luther-workflow binary
/// WHEN: running with "--help" or "help" flag
/// THEN: displays usage message with available commands
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P11
/// @requirement:REQ-EARS-QUAL-001
#[test]
fn test_cli_help_flag() {
    // GIVEN: the luther-workflow binary
    let mut cmd = luther_workflow_bin();
    cmd.arg("--help");

    // WHEN: running with "--help" flag
    let output = cmd.output().expect("Failed to execute help command");

    // THEN: should display usage information
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Check that help output contains expected elements
    let has_usage = stdout.contains("Usage:")
        || stdout.contains("usage:")
        || stderr.contains("Usage:")
        || stderr.contains("usage:");
    let has_commands = stdout.contains("Commands:")
        || stdout.contains("commands:")
        || stderr.contains("Commands:")
        || stderr.contains("commands:");

    assert!(
        output.status.success() || has_usage,
        "Help should display usage information. stdout: {stdout}, stderr: {stderr}"
    );

    // Should list available commands
    assert!(
        has_commands
            || stdout.contains("luther-workflow")
            || stdout.contains("run")
            || stdout.contains("status"),
        "Help should list available commands. stdout: {stdout}"
    );
}

/// Test: CLI run supports explicit run id for durable resume.
/// @requirement:REQ-EARS-ENG-004
#[test]
fn test_cli_run_accepts_explicit_run_id() {
    let mut cmd = luther_workflow_bin();
    cmd.args([
        "run",
        "--workflow-type",
        "hello-world-v1",
        "--config",
        "hello-world-config",
        "--config-dir",
        "tests/fixtures",
        "--run-id",
        "cli-explicit-run-id",
        "--dry-run",
    ]);

    let output = cmd.output().expect("Failed to execute run command");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "dry-run with explicit run id should succeed. stdout: {stdout}, stderr: {stderr}"
    );
    assert!(
        stdout.contains("Starting workflow run: cli-explicit-run-id"),
        "CLI should use the provided run id for resumable runs. stdout: {stdout}"
    );
}

/// Test: CLI invalid command returns error.
/// GIVEN: the luther-workflow binary
/// WHEN: running with an invalid/unknown subcommand
/// THEN: returns non-zero exit code and shows error message
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P11
/// @requirement:REQ-EARS-QUAL-001
#[test]
fn test_cli_invalid_command_error() {
    // GIVEN: the luther-workflow binary
    let mut cmd = luther_workflow_bin();
    cmd.arg("nonexistent-command-xyz123");

    // WHEN: running with an invalid subcommand
    let output = cmd.output().expect("Failed to execute command");

    // THEN: should return non-zero exit code
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        !output.status.success(),
        "Invalid command should return non-zero exit code. stdout: {stdout}, stderr: {stderr}"
    );

    // AND: should show error or unknown command message
    let has_error = stderr.contains("Unknown command")
        || stderr.contains("error")
        || stderr.contains("Error")
        || stderr.contains("invalid")
        || stderr.contains("unrecognized")
        || stdout.contains("Unknown command");
    assert!(
        has_error,
        "Invalid command should show error message. stdout: {stdout}, stderr: {stderr}"
    );
}

/// Test: dry-run reports unresolved tokens and missing artifact producers,
/// exiting non-zero. Guards Luther issue #11 acceptance criteria.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P11
#[test]
fn test_cli_dry_run_reports_validation_errors_and_exits_nonzero() {
    let mut cmd = luther_workflow_bin();
    cmd.args([
        "run",
        "--workflow-type",
        "dry-run-invalid-v1",
        "--config",
        "dry-run-invalid-config",
        "--config-dir",
        "tests/fixtures/dry_run_validation",
        "--run-id",
        "dry-run-validation-errors",
        "--dry-run",
    ]);

    let output = cmd.output().expect("Failed to execute dry-run command");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "dry-run with validation errors must exit non-zero. stdout: {stdout}, stderr: {stderr}"
    );
    assert!(
        stdout.contains("unresolved token:") && stdout.contains("artifact_root"),
        "dry-run should report the unresolved token. stdout: {stdout}"
    );
    assert!(
        stdout.contains("missing artifact producer:") && stdout.contains("plan"),
        "dry-run should report the missing artifact producer. stdout: {stdout}"
    );
}

/// Test: a clean workflow dry-run exits zero and prints no validation errors.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P11
#[test]
fn test_cli_dry_run_clean_workflow_exits_zero() {
    let mut cmd = luther_workflow_bin();
    cmd.args([
        "run",
        "--workflow-type",
        "hello-world-v1",
        "--config",
        "hello-world-config",
        "--config-dir",
        "tests/fixtures",
        "--run-id",
        "dry-run-clean",
        "--dry-run",
    ]);

    let output = cmd.output().expect("Failed to execute dry-run command");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "clean dry-run should exit zero. stdout: {stdout}, stderr: {stderr}"
    );
    assert!(
        !stdout.contains("unresolved token:") && !stdout.contains("missing artifact producer:"),
        "clean dry-run should not report validation errors. stdout: {stdout}"
    );
}
