use std::fs;

use luther_workflow::engine::executors::command_manifest::{
    request_from_entry, run_manifest_command,
};
use luther_workflow::workflow::command_manifest::{
    ArtifactExpectation, ArtifactExpectations, ArtifactKind, CapturePolicy, CommandEntry,
    FailureOutcome, RetryPolicy, StreamExpectations,
};

fn command_entry(id: &str, argv: &[&str]) -> CommandEntry {
    CommandEntry {
        id: id.to_string(),
        argv: argv.iter().map(|arg| (*arg).to_string()).collect(),
        working_directory: None,
        project_subdirectory: None,
        env: Default::default(),
        timeout_seconds: Some(5),
        acceptable_exit_codes: vec![0],
        capture: Default::default(),
        stdout: Default::default(),
        stderr: Default::default(),
        artifacts: Default::default(),
        failure_outcome: FailureOutcome::Fatal,
        retry: Default::default(),
    }
}

#[test]
fn manifest_executor_uses_argv_cwd_and_env_without_shell() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::create_dir(temp.path().join("sub")).expect("subdir");
    let mut entry = command_entry(
        "env-cwd",
        &[
            "python3",
            "-c",
            "import os; print(os.environ['CUSTOM_ENV']); print(os.getcwd().endswith('/sub'))",
        ],
    );
    entry.working_directory = Some("sub".to_string());
    entry
        .env
        .insert("CUSTOM_ENV".to_string(), "manifest".to_string());
    let request = request_from_entry(&entry, temp.path(), 5).expect("request");
    let result = run_manifest_command(request);
    assert!(result.passed(), "{result:?}");
    assert!(result.bounded_stdout.contains("manifest"));
    assert!(result.bounded_stdout.contains("True"));
}

#[test]
fn manifest_executor_applies_output_expectations() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut entry = command_entry("patterns", &["python3", "-c", "print('ok forbidden')"]);
    entry.stdout = StreamExpectations {
        required_patterns: vec!["ok".to_string()],
        forbidden_patterns: vec!["forbidden".to_string()],
    };
    entry.failure_outcome = FailureOutcome::Fixable;
    let request = request_from_entry(&entry, temp.path(), 5).expect("request");
    let result = run_manifest_command(request);
    assert!(!result.passed());
    assert_eq!(result.status(), "failed");
    assert!(result
        .expectation_failures
        .iter()
        .any(|failure| failure.contains("forbidden")));
}

#[test]
fn manifest_executor_checks_artifacts_even_on_zero_exit() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::write(temp.path().join("bad.log"), "bad").expect("bad artifact");
    let mut entry = command_entry("artifacts", &["python3", "-c", "print('done')"]);
    entry.artifacts = ArtifactExpectations {
        required: vec![ArtifactExpectation {
            path: "missing.txt".to_string(),
            kind: ArtifactKind::File,
        }],
        forbidden: vec![ArtifactExpectation {
            path: "bad.log".to_string(),
            kind: ArtifactKind::File,
        }],
    };
    let request = request_from_entry(&entry, temp.path(), 5).expect("request");
    let result = run_manifest_command(request);
    assert!(!result.passed());
    assert_eq!(result.artifact_failures.len(), 2);
}

#[test]
fn manifest_executor_honors_acceptable_exit_codes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut entry = command_entry("exit", &["python3", "-c", "raise SystemExit(7)"]);
    entry.acceptable_exit_codes = vec![7];
    let request = request_from_entry(&entry, temp.path(), 5).expect("request");
    let result = run_manifest_command(request);
    assert!(result.passed(), "{result:?}");
    assert_eq!(result.exit_code, Some(7));
}

#[test]
fn manifest_executor_honors_capture_policy() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut entry = command_entry(
        "capture",
        &[
            "python3",
            "-c",
            "import sys; print('visible'); print('hidden', file=sys.stderr)",
        ],
    );
    entry.capture = CapturePolicy {
        stdout: true,
        stderr: false,
        limit_bytes: 64 * 1024,
    };
    let request = request_from_entry(&entry, temp.path(), 5).expect("request");
    let result = run_manifest_command(request);
    assert!(result.passed(), "{result:?}");
    assert!(result.bounded_stdout.contains("visible"));
    assert!(result.bounded_stderr.is_empty());
}

#[test]
fn manifest_executor_retries_configured_exit_codes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let marker = temp.path().join("attempt");
    let script = format!(
        "from pathlib import Path\np=Path({:?})\nif not p.exists():\n    p.write_text('1')\n    raise SystemExit(7)\nprint('retried')\n",
        marker
    );
    let mut entry = command_entry("retry", &["python3", "-c", &script]);
    entry.retry = RetryPolicy {
        max_attempts: 1,
        retry_exit_codes: vec![7],
    };
    let request = request_from_entry(&entry, temp.path(), 5).expect("request");
    let result = run_manifest_command(request);
    assert!(result.passed(), "{result:?}");
    assert_eq!(result.exit_code, Some(0));
    assert!(result.bounded_stdout.contains("retried"));
}
