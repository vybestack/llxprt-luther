use super::post_pr_stages::{PostPrTestCommandRequest, PostPrTestCommandResult};
use super::*;
use crate::engine::executors::command_manifest::{
    run_manifest_entry, ManifestCommandResult, ManifestEntryExecution, ManifestPathContext,
};
use crate::workflow::command_manifest::CommandManifest;
use crate::workflow::config_loader::validate_command_manifest;
#[cfg(unix)]
use std::os::unix::process::CommandExt;

pub(super) fn reject_shell_string_manifest(value: &Value) -> Result<(), EngineError> {
    if value.get("command").is_some() || value.get("shell").is_some() {
        return Err(pr_remediation_error(
            "shell-string command manifests are forbidden",
        ));
    }
    Ok(())
}

pub(super) fn validated_command_manifest(value: &Value) -> Result<CommandManifest, EngineError> {
    reject_shell_string_manifest(value)?;
    let manifest = serde_json::from_value(value.clone())
        .map_err(|err| pr_remediation_error(format!("invalid command_manifest: {err}")))?;
    validate_command_manifest(&manifest)
        .map_err(|err| pr_remediation_error(format!("invalid command_manifest: {err}")))?;
    Ok(manifest)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-017
/// @pseudocode lines 29-30
pub(super) fn run_manifest_post_pr_test_process(
    request: PostPrTestCommandRequest,
) -> PostPrTestCommandResult {
    let Some(entry) = &request.manifest_entry else {
        return run_post_pr_test_process(request);
    };
    let mut resolved_entry = entry.clone();
    resolved_entry.working_directory = None;
    resolved_entry.project_subdirectory = None;
    let paths = ManifestPathContext {
        repo_root: request.repo_root_directory.clone(),
        default_working_directory: request.working_directory.clone(),
        artifact_base_directory: request.artifact_base_directory.clone(),
    };
    let result = match run_manifest_entry(&resolved_entry, &paths, request.timeout_seconds) {
        Ok(ManifestEntryExecution::Completed(result)) => result,
        Ok(ManifestEntryExecution::Skipped) => return skipped_manifest_post_pr_result(request),
        Err(err) => return manifest_post_pr_error(request, err),
    };
    write_optional_log(&request.stdout_log_path, &result.bounded_stdout);
    write_optional_log(&request.stderr_log_path, &result.bounded_stderr);
    post_pr_result_from_manifest(request, *result)
}

fn skipped_manifest_post_pr_result(request: PostPrTestCommandRequest) -> PostPrTestCommandResult {
    let stdout = "skipped by command manifest conditions".to_string();
    write_optional_log(&request.stdout_log_path, &stdout);
    write_optional_log(&request.stderr_log_path, "");
    PostPrTestCommandResult {
        command_id: request.command_id,
        argv: request.argv,
        working_directory: request.working_directory,
        exit_code: Some(0),
        signal: None,
        status: "passed".to_string(),
        bounded_stdout: stdout,
        bounded_stderr: String::new(),
        stdout_log_path: Some(request.stdout_log_path),
        stderr_log_path: Some(request.stderr_log_path),
        spawn_error: None,
        expectation_failures: Vec::new(),
        artifact_failures: Vec::new(),
        failure_classification: None,
    }
}

fn manifest_post_pr_error(
    request: PostPrTestCommandRequest,
    err: String,
) -> PostPrTestCommandResult {
    write_optional_log(&request.stdout_log_path, "");
    write_optional_log(&request.stderr_log_path, &err);
    PostPrTestCommandResult {
        command_id: request.command_id,
        argv: request.argv,
        working_directory: request.working_directory,
        exit_code: None,
        signal: None,
        status: "fatal".to_string(),
        bounded_stdout: String::new(),
        bounded_stderr: bounded_excerpt(&err, 4096),
        stdout_log_path: Some(request.stdout_log_path),
        stderr_log_path: Some(request.stderr_log_path),
        spawn_error: Some(err),
        expectation_failures: Vec::new(),
        artifact_failures: Vec::new(),
        failure_classification: Some("fatal".to_string()),
    }
}

fn post_pr_result_from_manifest(
    request: PostPrTestCommandRequest,
    result: ManifestCommandResult,
) -> PostPrTestCommandResult {
    let status = result.status().to_string();
    PostPrTestCommandResult {
        command_id: request.command_id,
        argv: result.argv,
        working_directory: result.working_directory,
        exit_code: result.exit_code,
        signal: None,
        status: status.clone(),
        bounded_stdout: result.bounded_stdout,
        bounded_stderr: result.bounded_stderr,
        stdout_log_path: Some(request.stdout_log_path),
        stderr_log_path: Some(request.stderr_log_path),
        spawn_error: result.spawn_error,
        expectation_failures: result.expectation_failures,
        artifact_failures: result.artifact_failures,
        failure_classification: Some(status),
    }
}

pub(super) fn run_post_pr_test_process(
    request: PostPrTestCommandRequest,
) -> PostPrTestCommandResult {
    let mut child = match spawn_post_pr_test_child(&request) {
        Ok(child) => child,
        Err(err) => return post_pr_test_spawn_error(request, err),
    };
    let output = match wait_for_post_pr_test_child(&mut child, request.timeout_seconds) {
        Ok(output) => output,
        Err(err) => return post_pr_test_spawn_error(request, err),
    };
    let stdout = output.stdout_text();
    let stderr = output.stderr_text();
    write_optional_log(&request.stdout_log_path, &stdout);
    write_optional_log(&request.stderr_log_path, &stderr);
    post_pr_test_process_result(request, output.exit_code, output.timed_out, stdout, stderr)
}

fn spawn_post_pr_test_child(
    request: &PostPrTestCommandRequest,
) -> std::io::Result<std::process::Child> {
    let program = request.argv.first().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "post-PR test command argv must not be empty",
        )
    })?;
    let mut command = Command::new(program);
    command.args(&request.argv[1..]);
    command.current_dir(&request.working_directory);
    command.env_clear();
    apply_allowed_command_environment(&mut command);
    command.env("PWD", &request.working_directory);
    tracing::debug!(
        command_id = %request.command_id,
        "spawned post-PR test child with cleared environment and whitelisted variables"
    );
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.stdin(Stdio::null());
    #[cfg(unix)]
    command.process_group(0);
    command.spawn()
}

pub(super) fn apply_allowed_command_environment(command: &mut Command) {
    for key in ALLOWED_COMMAND_ENVIRONMENT {
        if let Ok(value) = std::env::var(key) {
            command.env(key, value);
        }
    }
}

const ALLOWED_COMMAND_ENVIRONMENT: &[&str] = &[
    "CARGO_HOME",
    "GEM_HOME",
    "GEM_PATH",
    "GOCACHE",
    "GOMODCACHE",
    "GOPATH",
    "HOME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "NODE_PATH",
    "NPM_CONFIG_CACHE",
    "PATH",
    "PYENV_ROOT",
    "RUSTUP_HOME",
    "SSL_CERT_FILE",
    "TMPDIR",
    "USER",
];

fn post_pr_test_spawn_error(
    request: PostPrTestCommandRequest,
    err: std::io::Error,
) -> PostPrTestCommandResult {
    write_optional_log(&request.stdout_log_path, "");
    write_optional_log(&request.stderr_log_path, &err.to_string());
    PostPrTestCommandResult {
        command_id: request.command_id,
        argv: request.argv,
        working_directory: request.working_directory,
        exit_code: None,
        signal: None,
        status: "fatal".to_string(),
        bounded_stdout: String::new(),
        bounded_stderr: bounded_excerpt(&err.to_string(), 4096),
        stdout_log_path: Some(request.stdout_log_path),
        stderr_log_path: Some(request.stderr_log_path),
        spawn_error: Some(err.to_string()),
        expectation_failures: Vec::new(),
        artifact_failures: Vec::new(),
        failure_classification: Some("fatal".to_string()),
    }
}

fn wait_for_post_pr_test_child(
    child: &mut std::process::Child,
    timeout_seconds: u64,
) -> std::io::Result<ProcessOutputCapture> {
    let stdout_buffer = Arc::new(Mutex::new(String::new()));
    let stderr_buffer = Arc::new(Mutex::new(String::new()));
    let stdout_reader = spawn_reader(child.stdout.take(), &stdout_buffer);
    let stderr_reader = spawn_reader(child.stderr.take(), &stderr_buffer);
    let wait_result = wait_for_child_exit(child, timeout_seconds);
    join_reader(stdout_reader);
    join_reader(stderr_reader);
    let (exit_code, timed_out) = wait_result?;
    Ok(ProcessOutputCapture {
        stdout_buffer,
        stderr_buffer,
        exit_code,
        timed_out,
    })
}

fn post_pr_test_process_result(
    request: PostPrTestCommandRequest,
    exit_code: Option<i32>,
    timed_out: bool,
    stdout: String,
    stderr: String,
) -> PostPrTestCommandResult {
    PostPrTestCommandResult {
        command_id: request.command_id,
        argv: request.argv,
        working_directory: request.working_directory,
        exit_code,
        signal: None,
        status: process_status(timed_out, exit_code).to_string(),
        bounded_stdout: bounded_excerpt(&stdout, 4096),
        bounded_stderr: bounded_excerpt(&stderr, 4096),
        stdout_log_path: Some(request.stdout_log_path),
        stderr_log_path: Some(request.stderr_log_path),
        spawn_error: timed_out.then(|| "post-PR test command timed out".to_string()),
        expectation_failures: timed_out
            .then(|| "post-PR test command timed out".to_string())
            .into_iter()
            .collect(),
        artifact_failures: Vec::new(),
        failure_classification: None,
    }
}
