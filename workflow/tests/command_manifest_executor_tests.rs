use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use luther_workflow::engine::executors::command_manifest::{
    request_from_entry, request_from_entry_with_paths, run_manifest_command, ManifestPathContext,
};
use luther_workflow::workflow::command_manifest::{
    ArtifactExpectation, ArtifactExpectations, ArtifactKind, CapturePolicy, CommandEntry,
    FailureOutcome, RetryPolicy, StreamExpectations,
};

#[cfg(unix)]
fn process_is_alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(unix)]
fn wait_until_process_exits(pid: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !process_is_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !process_is_alive(pid)
}

#[cfg(unix)]
fn read_pid_with_retry(path: &Path, timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(pid) = fs::read_to_string(path) {
            return Some(pid);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(unix)]
fn kill_pid_forcefully(pid: &str) {
    let _ = Command::new("kill").args(["-9", pid.trim()]).status();
}

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

fn manifest_group_params(command: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "command_manifest_group": "bootstrap",
        "command_manifest": {
            "commands": [command],
            "groups": { "bootstrap": ["boot"] }
        }
    })
}

fn dispatch_manifest_group(
    work_dir: &Path,
    command: serde_json::Value,
) -> (
    luther_workflow::engine::transition::StepOutcome,
    luther_workflow::engine::executor::StepContext,
) {
    let mut context = luther_workflow::engine::executor::StepContext::new(
        work_dir.to_path_buf(),
        "run".to_string(),
    );
    let registry = luther_workflow::engine::executor::ExecutorRegistry::with_defaults();
    let outcome = registry
        .dispatch(
            "command_manifest_group",
            &mut context,
            &manifest_group_params(command),
        )
        .expect("dispatch command_manifest_group");
    (outcome, context)
}

fn dispatch_manifest_group_err(work_dir: &Path, command: serde_json::Value) -> String {
    let mut context = luther_workflow::engine::executor::StepContext::new(
        work_dir.to_path_buf(),
        "run".to_string(),
    );
    let registry = luther_workflow::engine::executor::ExecutorRegistry::with_defaults();
    registry
        .dispatch(
            "command_manifest_group",
            &mut context,
            &manifest_group_params(command),
        )
        .expect_err("command_manifest_group should fail")
        .to_string()
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
fn manifest_executor_bounds_large_output_capture() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut entry = command_entry(
        "large-output",
        &[
            "python3",
            "-c",
            "import sys; sys.stdout.write('x' * 200_000); sys.stderr.write('y' * 200_000)",
        ],
    );
    entry.capture.limit_bytes = 1024;
    let request = request_from_entry(&entry, temp.path(), 5).expect("request");

    let result = run_manifest_command(request);

    assert!(result.passed(), "{result:?}");
    assert!(result.bounded_stdout.len() < 1200, "stdout was unbounded");
    assert!(result.bounded_stderr.len() < 1200, "stderr was unbounded");
    assert!(result.bounded_stdout.contains("...[truncated]"));
    assert!(result.bounded_stderr.contains("...[truncated]"));
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

#[cfg(unix)]
#[test]
fn manifest_executor_timeout_does_not_wait_for_detached_pipe_holder() {
    let temp = tempfile::tempdir().expect("tempdir");
    let script = r#"
import pathlib
import subprocess
import sys
import time
child = subprocess.Popen(
    [sys.executable, "-c", "import time; time.sleep(30)"],
    stdout=sys.stdout,
    stderr=sys.stderr,
    start_new_session=True,
)
pathlib.Path("hold.pid").write_text(str(child.pid))
time.sleep(30)
"#;
    let mut entry = command_entry("timeout", &["python3", "-c", script]);
    entry.timeout_seconds = Some(1);
    let request = request_from_entry(&entry, temp.path(), 1).expect("request");

    let started = Instant::now();
    let result = run_manifest_command(request);
    let elapsed = started.elapsed();

    let hold_pid = read_pid_with_retry(&temp.path().join("hold.pid"), Duration::from_secs(1));
    if let Some(pid) = &hold_pid {
        kill_pid_forcefully(pid);
    }
    assert!(result.timed_out, "{result:?}");
    assert!(
        result
            .expectation_failures
            .iter()
            .any(|failure| failure.contains("command timed out")),
        "detached pipe holder should be reported as a timeout-like failure: {result:?}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "timeout result waited for detached pipe holder: {elapsed:?}"
    );
}

#[cfg(unix)]
#[test]
fn manifest_executor_success_cleans_up_background_pipe_holder() {
    let temp = tempfile::tempdir().expect("tempdir");
    let script = r#"
import pathlib
import subprocess
import sys
child = subprocess.Popen(
    [sys.executable, "-c", "import time; time.sleep(30)"],
    stdout=sys.stdout,
    stderr=sys.stderr,
)
pathlib.Path("hold.pid").write_text(str(child.pid))
"#;
    let entry = command_entry("background", &["python3", "-c", script]);
    let request = request_from_entry(&entry, temp.path(), 5).expect("request");

    let started = Instant::now();
    let result = run_manifest_command(request);
    let elapsed = started.elapsed();

    let pid = fs::read_to_string(temp.path().join("hold.pid")).expect("background pid");
    let pid = pid.trim();
    let background_exited = wait_until_process_exits(pid, Duration::from_secs(3));
    assert!(result.passed(), "{result:?}");
    assert!(
        elapsed < Duration::from_secs(5),
        "success result waited for background pipe holder: {elapsed:?}"
    );
    assert!(
        background_exited,
        "background pipe holder process {pid} survived manifest cleanup"
    );
    kill_pid_forcefully(pid);
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
    let (outcome, _) = dispatch_manifest_group(
        temp.path(),
        serde_json::json!({
            "id": "boot",
            "argv": ["python3", "-c", "from pathlib import Path; Path('created.txt').write_text('ok')"],
            "run_if_missing_any": ["missing"],
            "run_if_present_all": ["present"],
            "remove_before_run": ["stale.txt"]
        }),
    );

    assert_eq!(
        outcome,
        luther_workflow::engine::transition::StepOutcome::Success
    );
    assert!(temp.path().join("created.txt").exists());
    assert!(!temp.path().join("stale.txt").exists());
}

#[test]
fn command_manifest_group_uses_target_project_and_artifact_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path();
    let project_dir = repo.join("workflow");
    let command_dir = repo.join("tools");
    let artifact_base = repo.join("artifacts");
    fs::create_dir_all(&project_dir).expect("project dir");
    fs::create_dir_all(&command_dir).expect("command dir");
    fs::create_dir_all(&artifact_base).expect("artifact base");

    let mut context =
        luther_workflow::engine::executor::StepContext::new(repo.to_path_buf(), "run".to_string());
    context.set("project_dir", &project_dir.to_string_lossy());
    context.set("default_command_cwd", "tools");
    context.set("artifact_base_dir", &artifact_base.to_string_lossy());
    let registry = luther_workflow::engine::executor::ExecutorRegistry::with_defaults();
    let params = manifest_group_params(serde_json::json!({
        "id": "boot",
        "argv": [
            "python3",
            "-c",
            "import os; from pathlib import Path; Path('../artifacts/marker.txt').write_text(os.getcwd())"
        ],
        "artifacts": {
            "required": [{ "path": "marker.txt", "kind": "file" }]
        }
    }));

    let outcome = registry
        .dispatch("command_manifest_group", &mut context, &params)
        .expect("dispatch command_manifest_group");

    assert_eq!(
        outcome,
        luther_workflow::engine::transition::StepOutcome::Success
    );
    assert_eq!(
        PathBuf::from(fs::read_to_string(artifact_base.join("marker.txt")).expect("marker"))
            .canonicalize()
            .expect("canonical marker cwd"),
        command_dir.canonicalize().expect("canonical command dir")
    );
}

#[test]
fn command_manifest_group_records_failure_evidence() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (outcome, context) = dispatch_manifest_group(
        temp.path(),
        serde_json::json!({
            "id": "boot",
            "argv": ["python3", "-c", "import sys; print('visible out'); print('visible err', file=sys.stderr); raise SystemExit(7)"],
            "failure_outcome": "fixable"
        }),
    );

    assert_eq!(
        outcome,
        luther_workflow::engine::transition::StepOutcome::Fixable
    );
    let stdout = context.get("stdout").expect("failure stdout evidence");
    assert!(stdout.contains("boot"));
    assert!(stdout.contains("visible out"));
    assert!(stdout.contains("visible err"));
    assert!(stdout.contains("7"));
    assert_eq!(
        context.get("stderr").map(String::as_str),
        Some("visible err\n")
    );
}

#[test]
fn command_manifest_group_rejects_broad_remove_before_run_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    let err = dispatch_manifest_group_err(
        temp.path(),
        serde_json::json!({
            "id": "boot",
            "argv": ["python3", "-c", "raise SystemExit('must not run')"],
            "remove_before_run": ["."]
        }),
    );

    assert!(err.contains("removal path") && err.contains("must not target work_dir itself"));
    assert!(temp.path().exists());
}

#[cfg(unix)]
#[test]
fn command_manifest_group_removes_symlink_without_following_it() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    fs::write(outside.path().join("keep.txt"), "keep").expect("outside file");
    symlink(outside.path(), temp.path().join("link")).expect("symlink");

    let (outcome, _) = dispatch_manifest_group(
        temp.path(),
        serde_json::json!({
            "id": "boot",
            "argv": ["python3", "-c", "print('ok')"],
            "remove_before_run": ["link"]
        }),
    );

    assert_eq!(
        outcome,
        luther_workflow::engine::transition::StepOutcome::Success
    );
    assert!(!temp.path().join("link").exists());
    assert!(outside.path().join("keep.txt").exists());
}

#[cfg(unix)]
#[test]
fn command_manifest_group_rejects_removal_through_symlinked_parent() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    fs::write(outside.path().join("victim"), "keep").expect("outside file");
    symlink(outside.path(), temp.path().join("link")).expect("symlink");

    let err = dispatch_manifest_group_err(
        temp.path(),
        serde_json::json!({
            "id": "boot",
            "argv": ["true"],
            "remove_before_run": ["link/victim"]
        }),
    );

    assert!(
        err.contains("must stay under work_dir"),
        "unexpected error: {err}"
    );
    assert!(outside.path().join("victim").exists());
}

#[test]
fn command_manifest_group_rejects_duplicate_command_ids_at_runtime() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut context = luther_workflow::engine::executor::StepContext::new(
        temp.path().to_path_buf(),
        "run".to_string(),
    );
    let registry = luther_workflow::engine::executor::ExecutorRegistry::with_defaults();
    let params = serde_json::json!({
        "command_manifest_group": "bootstrap",
        "command_manifest": {
            "commands": [
                { "id": "boot", "argv": ["true"] },
                { "id": "boot", "argv": ["false"] }
            ],
            "groups": { "bootstrap": ["boot"] }
        }
    });
    let err = registry
        .dispatch("command_manifest_group", &mut context, &params)
        .expect_err("duplicate runtime manifest command ids should fail")
        .to_string();
    assert!(err.contains("duplicate"));
}

#[test]
fn command_manifest_group_rejects_invalid_condition_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    for field in [
        "run_if_missing_any",
        "run_if_present_all",
        "remove_before_run",
    ] {
        let mut command = serde_json::json!({
            "id": "boot",
            "argv": ["python3", "-c", "print('should not run')"]
        });
        command[field] = serde_json::json!(["../outside"]);

        let err = dispatch_manifest_group_err(temp.path(), command);
        assert!(
            err.contains("must stay under work_dir"),
            "{field} should reject traversal paths: {err}"
        );
    }
}

#[test]
fn command_manifest_group_skips_command_when_present_all_not_satisfied() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::write(temp.path().join("stale.txt"), "stale").expect("stale");
    let (outcome, _) = dispatch_manifest_group(
        temp.path(),
        serde_json::json!({
            "id": "boot",
            "argv": ["python3", "-c", "from pathlib import Path; Path('created.txt').write_text('ok')"],
            "run_if_present_all": ["does_not_exist"],
            "remove_before_run": ["stale.txt"]
        }),
    );

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
    let (outcome, _) = dispatch_manifest_group(
        temp.path(),
        serde_json::json!({
            "id": "boot",
            "argv": ["python3", "-c", "from pathlib import Path; Path('created.txt').write_text('ok')"],
            "run_if_missing_any": ["already.txt"],
            "remove_before_run": ["stale.txt"]
        }),
    );

    assert_eq!(
        outcome,
        luther_workflow::engine::transition::StepOutcome::Success
    );
    assert!(!temp.path().join("created.txt").exists());
    assert!(temp.path().join("stale.txt").exists());
}

#[test]
fn command_manifest_group_requires_group_parameter() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut context = luther_workflow::engine::executor::StepContext::new(
        temp.path().to_path_buf(),
        "run".to_string(),
    );
    let registry = luther_workflow::engine::executor::ExecutorRegistry::with_defaults();
    let err = registry
        .dispatch(
            "command_manifest_group",
            &mut context,
            &serde_json::json!({}),
        )
        .expect_err("missing command_manifest_group should fail")
        .to_string();

    assert!(err.contains("command_manifest_group is required"));
}

#[test]
fn command_manifest_group_uses_step_timeout_as_default() {
    let temp = tempfile::tempdir().expect("tempdir");
    let params = serde_json::json!({
        "command_manifest_group": "bootstrap",
        "timeout_seconds": 1,
        "command_manifest": {
            "commands": [{
                "id": "boot",
                "argv": ["python3", "-c", "import time; time.sleep(30)"],
            }],
            "groups": { "bootstrap": ["boot"] }
        }
    });
    let mut context = luther_workflow::engine::executor::StepContext::new(
        temp.path().to_path_buf(),
        "run".to_string(),
    );
    let registry = luther_workflow::engine::executor::ExecutorRegistry::with_defaults();

    let started = Instant::now();
    let outcome = registry
        .dispatch("command_manifest_group", &mut context, &params)
        .expect("manifest group dispatch");

    assert!(matches!(
        outcome,
        luther_workflow::engine::transition::StepOutcome::Fatal
    ));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "step timeout_seconds was not used as the default manifest command timeout"
    );
}

#[test]
fn command_manifest_group_empty_group_is_rejected() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut context = luther_workflow::engine::executor::StepContext::new(
        temp.path().to_path_buf(),
        "run".to_string(),
    );
    let params = serde_json::json!({ "command_manifest_group": "" });
    let registry = luther_workflow::engine::executor::ExecutorRegistry::with_defaults();
    let err = registry
        .dispatch("command_manifest_group", &mut context, &params)
        .expect_err("empty group should fail")
        .to_string();
    assert!(err.contains("command_manifest_group must not be empty"));
}

#[test]
fn command_manifest_group_can_skip_empty_optional_group() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut context = luther_workflow::engine::executor::StepContext::new(
        temp.path().to_path_buf(),
        "run".to_string(),
    );
    let params = serde_json::json!({
        "command_manifest_group": "",
        "allow_empty_group": true,
    });
    let registry = luther_workflow::engine::executor::ExecutorRegistry::with_defaults();
    let outcome = registry
        .dispatch("command_manifest_group", &mut context, &params)
        .expect("empty optional group should succeed");
    assert!(matches!(
        outcome,
        luther_workflow::engine::transition::StepOutcome::Success
    ));
}

#[test]
fn command_manifest_group_rejects_backslash_runtime_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    let err = dispatch_manifest_group_err(
        temp.path(),
        serde_json::json!({
            "id": "boot",
            "argv": ["true"],
            "remove_before_run": ["node_modules\\.bin"],
        }),
    );
    assert!(
        err.contains("must stay under work_dir"),
        "unexpected error: {err}"
    );
}
