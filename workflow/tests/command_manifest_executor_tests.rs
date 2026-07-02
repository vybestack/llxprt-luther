use std::fs;

use luther_workflow::engine::executors::command_manifest::{
    request_from_entry, request_from_entry_with_paths, run_manifest_command, ManifestPathContext,
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
        run_if_missing_any: Vec::new(),
        run_if_present_all: Vec::new(),
        remove_before_run: Vec::new(),
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
fn manifest_executor_uses_default_project_cwd_and_repo_relative_override() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path();
    fs::create_dir_all(repo.join("workflow")).expect("workflow dir");
    fs::create_dir_all(repo.join("other")).expect("other dir");
    let paths = ManifestPathContext {
        repo_root: repo.to_path_buf(),
        default_working_directory: repo.join("workflow"),
        artifact_base_directory: repo.to_path_buf(),
    };
    let default_entry = command_entry("default", &["pwd"]);
    let request = request_from_entry_with_paths(&default_entry, &paths, 5).expect("request");
    assert_eq!(request.working_directory, repo.join("workflow"));

    let mut override_entry = command_entry("override", &["pwd"]);
    override_entry.working_directory = Some("other".to_string());
    let request = request_from_entry_with_paths(&override_entry, &paths, 5).expect("request");
    assert_eq!(request.working_directory, repo.join("other"));
}

#[test]
fn manifest_executor_checks_artifacts_against_artifact_base() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path();
    fs::create_dir_all(repo.join("workflow")).expect("workflow dir");
    fs::write(repo.join("artifact.txt"), "ok").expect("artifact");
    let paths = ManifestPathContext {
        repo_root: repo.to_path_buf(),
        default_working_directory: repo.join("workflow"),
        artifact_base_directory: repo.to_path_buf(),
    };
    let mut entry = command_entry("artifact-base", &["true"]);
    entry.artifacts.required.push(ArtifactExpectation {
        path: "artifact.txt".to_string(),
        kind: ArtifactKind::File,
    });
    let request = request_from_entry_with_paths(&entry, &paths, 5).expect("request");
    let result = run_manifest_command(request);
    assert!(result.passed(), "{result:?}");
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
fn manifest_executor_rejects_artifact_paths_that_escape_base() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::write(temp.path().join("outside.txt"), "outside").expect("outside artifact");
    fs::write(temp.path().join("bad.log"), "bad").expect("bad artifact");
    let mut entry = command_entry("artifact-containment", &["true"]);
    entry.artifacts = ArtifactExpectations {
        required: vec![
            ArtifactExpectation {
                path: "../outside.txt".to_string(),
                kind: ArtifactKind::File,
            },
            ArtifactExpectation {
                path: temp.path().join("outside.txt").display().to_string(),
                kind: ArtifactKind::File,
            },
        ],
        forbidden: vec![ArtifactExpectation {
            path: "../bad.log".to_string(),
            kind: ArtifactKind::File,
        }],
    };
    let artifact_base = temp.path().join("artifacts");
    fs::create_dir(&artifact_base).expect("artifact base");
    let paths = ManifestPathContext {
        repo_root: temp.path().to_path_buf(),
        default_working_directory: temp.path().to_path_buf(),
        artifact_base_directory: artifact_base,
    };
    let request = request_from_entry_with_paths(&entry, &paths, 5).expect("request");
    let result = run_manifest_command(request);
    assert!(!result.passed());
    assert_eq!(result.artifact_failures.len(), 2);
}

#[cfg(unix)]
#[test]
fn manifest_executor_rejects_artifact_symlinks_that_escape_base() {
    let temp = tempfile::tempdir().expect("tempdir");
    let artifact_base = temp.path().join("artifacts");
    let outside = temp.path().join("outside");
    fs::create_dir(&artifact_base).expect("artifact base");
    fs::create_dir(&outside).expect("outside dir");
    fs::write(outside.join("outside.txt"), "outside").expect("outside file");
    std::os::unix::fs::symlink(outside.join("outside.txt"), artifact_base.join("file-link"))
        .expect("file symlink");
    std::os::unix::fs::symlink(&outside, artifact_base.join("dir-link")).expect("dir symlink");

    let mut entry = command_entry("artifact-symlink-containment", &["true"]);
    entry.artifacts.required = vec![
        ArtifactExpectation {
            path: "file-link".to_string(),
            kind: ArtifactKind::Any,
        },
        ArtifactExpectation {
            path: "file-link".to_string(),
            kind: ArtifactKind::File,
        },
        ArtifactExpectation {
            path: "dir-link".to_string(),
            kind: ArtifactKind::Directory,
        },
    ];
    let paths = ManifestPathContext {
        repo_root: temp.path().to_path_buf(),
        default_working_directory: temp.path().to_path_buf(),
        artifact_base_directory: artifact_base,
    };
    let request = request_from_entry_with_paths(&entry, &paths, 5).expect("request");
    let result = run_manifest_command(request);
    assert!(!result.passed());
    assert_eq!(result.artifact_failures.len(), 3);
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

#[test]
fn command_manifest_group_honors_conditions_and_removal_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::write(temp.path().join("stale.txt"), "stale").expect("stale");
    fs::create_dir(temp.path().join("present")).expect("present dir");
    let mut context = luther_workflow::engine::executor::StepContext::new(
        temp.path().to_path_buf(),
        "run".to_string(),
    );
    let params = serde_json::json!({
        "command_manifest_group": "bootstrap",
        "command_manifest": {
            "commands": [{
                "id": "boot",
                "argv": ["python3", "-c", "from pathlib import Path; Path('created.txt').write_text('ok')"],
                "run_if_missing_any": ["missing"],
                "run_if_present_all": ["present"],
                "remove_before_run": ["stale.txt"]
            }],
            "groups": { "bootstrap": ["boot"] }
        }
    });
    let registry = luther_workflow::engine::executor::ExecutorRegistry::with_defaults();
    let outcome = registry
        .dispatch("command_manifest_group", &mut context, &params)
        .expect("dispatch command_manifest_group");
    assert_eq!(
        outcome,
        luther_workflow::engine::transition::StepOutcome::Success
    );
    assert!(temp.path().join("created.txt").exists());
    assert!(!temp.path().join("stale.txt").exists());
}

#[test]
fn command_manifest_group_rejects_invalid_condition_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut context = luther_workflow::engine::executor::StepContext::new(
        temp.path().to_path_buf(),
        "run".to_string(),
    );
    let params = serde_json::json!({
        "command_manifest_group": "bootstrap",
        "command_manifest": {
            "commands": [{
                "id": "boot",
                "argv": ["python3", "-c", "print('should not run')"],
                "run_if_missing_any": ["../outside"]
            }],
            "groups": { "bootstrap": ["boot"] }
        }
    });
    let registry = luther_workflow::engine::executor::ExecutorRegistry::with_defaults();
    let err = registry
        .dispatch("command_manifest_group", &mut context, &params)
        .expect_err("invalid condition path rejected");
    assert!(err
        .to_string()
        .contains("manifest path must stay under work_dir"));
}

#[test]
fn command_manifest_group_skips_command_when_present_all_not_satisfied() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::write(temp.path().join("stale.txt"), "stale").expect("stale");
    let mut context = luther_workflow::engine::executor::StepContext::new(
        temp.path().to_path_buf(),
        "run".to_string(),
    );
    let params = serde_json::json!({
        "command_manifest_group": "bootstrap",
        "command_manifest": {
            "commands": [{
                "id": "boot",
                "argv": ["python3", "-c", "from pathlib import Path; Path('created.txt').write_text('ok')"],
                "run_if_present_all": ["does_not_exist"],
                "remove_before_run": ["stale.txt"]
            }],
            "groups": { "bootstrap": ["boot"] }
        }
    });
    let registry = luther_workflow::engine::executor::ExecutorRegistry::with_defaults();
    let outcome = registry
        .dispatch("command_manifest_group", &mut context, &params)
        .expect("dispatch command_manifest_group");
    assert_eq!(
        outcome,
        luther_workflow::engine::transition::StepOutcome::Success
    );
    assert!(!temp.path().join("created.txt").exists());
    assert!(temp.path().join("stale.txt").exists());
}

#[test]
fn command_manifest_group_skips_command_when_missing_any_path_exists() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::write(temp.path().join("already.txt"), "present").expect("present");
    fs::write(temp.path().join("stale.txt"), "stale").expect("stale");
    let mut context = luther_workflow::engine::executor::StepContext::new(
        temp.path().to_path_buf(),
        "run".to_string(),
    );
    let params = serde_json::json!({
        "command_manifest_group": "bootstrap",
        "command_manifest": {
            "commands": [{
                "id": "boot",
                "argv": ["python3", "-c", "from pathlib import Path; Path('created.txt').write_text('ok')"],
                "run_if_missing_any": ["already.txt"],
                "remove_before_run": ["stale.txt"]
            }],
            "groups": { "bootstrap": ["boot"] }
        }
    });
    let registry = luther_workflow::engine::executor::ExecutorRegistry::with_defaults();
    let outcome = registry
        .dispatch("command_manifest_group", &mut context, &params)
        .expect("dispatch command_manifest_group");
    assert_eq!(
        outcome,
        luther_workflow::engine::transition::StepOutcome::Success
    );
    assert!(!temp.path().join("created.txt").exists());
    assert!(temp.path().join("stale.txt").exists());
}

#[test]
fn command_manifest_group_empty_group_is_noop() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut context = luther_workflow::engine::executor::StepContext::new(
        temp.path().to_path_buf(),
        "run".to_string(),
    );
    let params = serde_json::json!({ "command_manifest_group": "" });
    let registry = luther_workflow::engine::executor::ExecutorRegistry::with_defaults();
    let outcome = registry
        .dispatch("command_manifest_group", &mut context, &params)
        .expect("empty group succeeds");
    assert_eq!(
        outcome,
        luther_workflow::engine::transition::StepOutcome::Success
    );
}
