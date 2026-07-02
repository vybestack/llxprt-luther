use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use regex::Regex;

use crate::workflow::command_manifest::{
    ArtifactExpectation, ArtifactKind, CommandEntry, FailureOutcome,
};

#[derive(Clone, Debug)]
pub struct ManifestCommandRequest {
    pub command_id: String,
    pub argv: Vec<String>,
    pub working_directory: PathBuf,
    pub timeout_seconds: u64,
    pub env: BTreeMap<String, String>,
    pub acceptable_exit_codes: Vec<i32>,
    pub capture_stdout: bool,
    pub capture_stderr: bool,
    pub capture_limit_bytes: usize,
    pub stdout_required_patterns: Vec<String>,
    pub stdout_forbidden_patterns: Vec<String>,
    pub stderr_required_patterns: Vec<String>,
    pub stderr_forbidden_patterns: Vec<String>,
    pub required_artifacts: Vec<ArtifactExpectation>,
    pub forbidden_artifacts: Vec<ArtifactExpectation>,
    pub failure_outcome: FailureOutcome,
    pub retry_max_attempts: u32,
    pub retry_exit_codes: Vec<i32>,
}

#[derive(Clone, Debug)]
pub struct ManifestCommandResult {
    pub command_id: String,
    pub argv: Vec<String>,
    pub working_directory: PathBuf,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub bounded_stdout: String,
    pub bounded_stderr: String,
    pub expectation_failures: Vec<String>,
    pub artifact_failures: Vec<String>,
    pub failure_outcome: FailureOutcome,
    pub spawn_error: Option<String>,
}

impl ManifestCommandResult {
    #[must_use]
    pub fn passed(&self) -> bool {
        self.spawn_error.is_none()
            && !self.timed_out
            && self.expectation_failures.is_empty()
            && self.artifact_failures.is_empty()
    }

    #[must_use]
    pub fn status(&self) -> &'static str {
        if self.passed() {
            "passed"
        } else if self.failure_outcome == FailureOutcome::Fatal || self.spawn_error.is_some() {
            "fatal"
        } else {
            "failed"
        }
    }
}

pub fn request_from_entry(
    entry: &CommandEntry,
    work_dir: &Path,
    default_timeout_seconds: u64,
) -> Result<ManifestCommandRequest, String> {
    let working_directory = manifest_working_directory(entry, work_dir)?;
    Ok(ManifestCommandRequest {
        command_id: entry.id.clone(),
        argv: entry.argv.clone(),
        working_directory,
        timeout_seconds: entry.timeout_seconds.unwrap_or(default_timeout_seconds),
        env: entry.env.clone(),
        acceptable_exit_codes: entry.acceptable_exit_codes.clone(),
        capture_stdout: entry.capture.stdout,
        capture_stderr: entry.capture.stderr,
        capture_limit_bytes: entry.capture.limit_bytes,
        stdout_required_patterns: entry.stdout.required_patterns.clone(),
        stdout_forbidden_patterns: entry.stdout.forbidden_patterns.clone(),
        stderr_required_patterns: entry.stderr.required_patterns.clone(),
        stderr_forbidden_patterns: entry.stderr.forbidden_patterns.clone(),
        required_artifacts: entry.artifacts.required.clone(),
        forbidden_artifacts: entry.artifacts.forbidden.clone(),
        failure_outcome: entry.failure_outcome.clone(),
        retry_max_attempts: entry.retry.max_attempts,
        retry_exit_codes: entry.retry.retry_exit_codes.clone(),
    })
}

pub fn run_manifest_command(request: ManifestCommandRequest) -> ManifestCommandResult {
    let mut attempts_remaining = request.retry_max_attempts.saturating_add(1);
    loop {
        let output = match spawn_manifest_child(&request) {
            Ok(mut child) => wait_for_manifest_child(&mut child, request.timeout_seconds),
            Err(err) => return manifest_spawn_error(request, err),
        };
        let Ok(output) = output else {
            return manifest_spawn_error(request, std::io::Error::last_os_error());
        };
        let should_retry =
            should_retry_manifest_result(&request, output.exit_code, attempts_remaining);
        if !should_retry {
            return manifest_process_result(request, output);
        }
        attempts_remaining -= 1;
    }
}

fn should_retry_manifest_result(
    request: &ManifestCommandRequest,
    exit_code: Option<i32>,
    attempts_remaining: u32,
) -> bool {
    attempts_remaining > 1 && exit_code.is_some_and(|code| request.retry_exit_codes.contains(&code))
}

fn manifest_working_directory(entry: &CommandEntry, work_dir: &Path) -> Result<PathBuf, String> {
    let relative = entry
        .working_directory
        .as_deref()
        .or(entry.project_subdirectory.as_deref())
        .unwrap_or("");
    let candidate = if relative.is_empty() {
        work_dir.to_path_buf()
    } else {
        work_dir.join(relative)
    };
    validate_manifest_working_directory(work_dir, &candidate)?;
    Ok(candidate)
}

pub fn validate_manifest_working_directory(
    work_dir: &Path,
    candidate: &Path,
) -> Result<(), String> {
    let base = work_dir
        .canonicalize()
        .map_err(|err| format!("canonicalize work_dir: {err}"))?;
    let candidate = candidate
        .canonicalize()
        .map_err(|err| format!("canonicalize manifest working_directory: {err}"))?;
    if candidate.starts_with(&base) {
        Ok(())
    } else {
        Err("manifest working_directory must stay under workflow work_dir".to_string())
    }
}

fn spawn_manifest_child(request: &ManifestCommandRequest) -> std::io::Result<std::process::Child> {
    let program = request.argv.first().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "manifest argv must not be empty",
        )
    })?;
    let mut command = Command::new(program);
    command.args(&request.argv[1..]);
    command.current_dir(&request.working_directory);
    command.env_clear();
    apply_allowed_command_environment(&mut command);
    command.env("PWD", &request.working_directory);
    for (key, value) in &request.env {
        command.env(key, value);
    }
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.stdin(Stdio::null());
    #[cfg(unix)]
    command.process_group(0);
    command.spawn()
}

fn wait_for_manifest_child(
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

fn manifest_process_result(
    request: ManifestCommandRequest,
    output: ProcessOutputCapture,
) -> ManifestCommandResult {
    let stdout = output.stdout_text();
    let stderr = output.stderr_text();
    let expectation_failures = expectation_failures(&request, output.exit_code, &stdout, &stderr);
    let artifact_failures = artifact_failures(&request);
    let mut result = ManifestCommandResult {
        command_id: request.command_id,
        argv: request.argv,
        working_directory: request.working_directory,
        exit_code: output.exit_code,
        timed_out: output.timed_out,
        bounded_stdout: captured_excerpt(
            request.capture_stdout,
            &stdout,
            request.capture_limit_bytes,
        ),
        bounded_stderr: captured_excerpt(
            request.capture_stderr,
            &stderr,
            request.capture_limit_bytes,
        ),
        expectation_failures,
        artifact_failures,
        failure_outcome: request.failure_outcome,
        spawn_error: None,
    };
    if output.timed_out {
        result
            .expectation_failures
            .push("command timed out".to_string());
    }
    result
}

fn expectation_failures(
    request: &ManifestCommandRequest,
    exit_code: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> Vec<String> {
    let mut failures = Vec::new();
    if !exit_code.is_some_and(|code| request.acceptable_exit_codes.contains(&code)) {
        failures.push(format!("exit code {:?} was not acceptable", exit_code));
    }
    failures.extend(pattern_failures(
        "stdout",
        stdout,
        &request.stdout_required_patterns,
        &request.stdout_forbidden_patterns,
    ));
    failures.extend(pattern_failures(
        "stderr",
        stderr,
        &request.stderr_required_patterns,
        &request.stderr_forbidden_patterns,
    ));
    failures
}

fn pattern_failures(
    stream: &str,
    text: &str,
    required: &[String],
    forbidden: &[String],
) -> Vec<String> {
    let mut failures = Vec::new();
    for pattern in required {
        if Regex::new(pattern).map_or(true, |regex| !regex.is_match(text)) {
            failures.push(format!("{stream} missing required pattern {pattern}"));
        }
    }
    for pattern in forbidden {
        if Regex::new(pattern).is_ok_and(|regex| regex.is_match(text)) {
            failures.push(format!("{stream} matched forbidden pattern {pattern}"));
        }
    }
    failures
}

fn artifact_failures(request: &ManifestCommandRequest) -> Vec<String> {
    let mut failures = Vec::new();
    for artifact in &request.required_artifacts {
        if !artifact_matches(&request.working_directory, artifact) {
            failures.push(format!("required artifact missing: {}", artifact.path));
        }
    }
    for artifact in &request.forbidden_artifacts {
        if artifact_matches(&request.working_directory, artifact) {
            failures.push(format!("forbidden artifact present: {}", artifact.path));
        }
    }
    failures
}

fn artifact_matches(root: &Path, artifact: &ArtifactExpectation) -> bool {
    let path = root.join(&artifact.path);
    match artifact.kind {
        ArtifactKind::Any => path.exists(),
        ArtifactKind::File => path.is_file(),
        ArtifactKind::Directory => path.is_dir(),
    }
}

fn manifest_spawn_error(
    request: ManifestCommandRequest,
    err: std::io::Error,
) -> ManifestCommandResult {
    ManifestCommandResult {
        command_id: request.command_id,
        argv: request.argv,
        working_directory: request.working_directory,
        exit_code: None,
        timed_out: false,
        bounded_stdout: String::new(),
        bounded_stderr: bounded_excerpt(&err.to_string(), request.capture_limit_bytes),
        expectation_failures: Vec::new(),
        artifact_failures: Vec::new(),
        failure_outcome: FailureOutcome::Fatal,
        spawn_error: Some(err.to_string()),
    }
}

struct ProcessOutputCapture {
    stdout_buffer: Arc<Mutex<String>>,
    stderr_buffer: Arc<Mutex<String>>,
    exit_code: Option<i32>,
    timed_out: bool,
}

impl ProcessOutputCapture {
    fn stdout_text(&self) -> String {
        self.stdout_buffer
            .lock()
            .map_or_else(|_| String::new(), |v| v.clone())
    }

    fn stderr_text(&self) -> String {
        self.stderr_buffer
            .lock()
            .map_or_else(|_| String::new(), |v| v.clone())
    }
}

fn spawn_reader(
    pipe: Option<impl Read + Send + 'static>,
    buffer: &Arc<Mutex<String>>,
) -> thread::JoinHandle<()> {
    let buffer = Arc::clone(buffer);
    thread::spawn(move || {
        if let Some(mut pipe) = pipe {
            let mut text = String::new();
            let _ = pipe.read_to_string(&mut text);
            if let Ok(mut guard) = buffer.lock() {
                *guard = text;
            }
        }
    })
}

fn wait_for_child_exit(
    child: &mut std::process::Child,
    timeout_seconds: u64,
) -> std::io::Result<(Option<i32>, bool)> {
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok((status.code(), false));
        }
        if Instant::now() >= deadline {
            terminate_child(child);
            let status = child.wait()?;
            return Ok((status.code(), true));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn terminate_child(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pid = child.id().to_string();
        let _ = Command::new("kill").args(["-TERM", "-", &pid]).status();
        thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
}

fn join_reader(reader: thread::JoinHandle<()>) {
    let _ = reader.join();
}

fn captured_excerpt(enabled: bool, text: &str, limit_bytes: usize) -> String {
    if enabled {
        bounded_excerpt(text, limit_bytes)
    } else {
        String::new()
    }
}

pub fn bounded_excerpt(text: &str, limit_bytes: usize) -> String {
    if text.len() <= limit_bytes {
        return text.to_string();
    }
    let mut end = limit_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n...[truncated]", &text[..end])
}

pub fn apply_allowed_command_environment(command: &mut Command) {
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

pub fn remove_if_file(path: &Path) {
    if path.is_file() {
        let _ = fs::remove_file(path);
    }
}
