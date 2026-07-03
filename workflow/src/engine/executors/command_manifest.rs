use std::collections::BTreeMap;
use std::fs;
use std::io::{BufReader, Read};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use regex::Regex;
use serde_json::{json, Value};

use crate::engine::executor::{interpolate_string, StepContext, StepExecutor};
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;
use crate::workflow::command_manifest::{
    ArtifactExpectation, ArtifactKind, CommandEntry, CommandManifest, FailureOutcome,
};
use crate::workflow::config_loader::validate_command_manifest;

const MANIFEST_GROUP_TIMEOUT_SECONDS: u64 = 900;

pub struct CommandManifestGroupExecutor;

impl StepExecutor for CommandManifestGroupExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        params: &Value,
    ) -> Result<StepOutcome, EngineError> {
        execute_manifest_group(context, params)
    }
}

fn execute_manifest_group(
    context: &mut StepContext,
    params: &Value,
) -> Result<StepOutcome, EngineError> {
    let has_group_param = params.get("command_manifest_group").is_some();
    let group_id = resolve_manifest_group_id(params, context, "").map_err(group_error)?;
    let allow_empty_group = params
        .get("allow_empty_group")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if group_id.trim().is_empty() {
        if allow_empty_group {
            return Ok(StepOutcome::Success);
        }
        return if has_group_param {
            Err(group_error("command_manifest_group must not be empty"))
        } else {
            Err(group_error("command_manifest_group is required"))
        };
    }
    let manifest = parse_command_manifest(params)?;
    let default_timeout_seconds = params
        .get("timeout_seconds")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(MANIFEST_GROUP_TIMEOUT_SECONDS);
    let Some(command_ids) = manifest.groups.get(&group_id) else {
        return Err(group_error(format!(
            "unknown command_manifest group '{group_id}'"
        )));
    };
    let commands_by_id: BTreeMap<&str, &CommandEntry> = manifest
        .commands
        .iter()
        .map(|entry| (entry.id.as_str(), entry))
        .collect();
    let path_context = manifest_path_context_from_step(context);
    for command_id in command_ids {
        let entry = commands_by_id
            .get(command_id.as_str())
            .ok_or_else(|| group_error(format!("unknown command manifest id '{command_id}'")))?;
        let result = run_manifest_entry(entry, &path_context, default_timeout_seconds)
            .map_err(group_error)?;
        let ManifestEntryExecution::Completed(result) = result else {
            continue;
        };
        if !result.passed() {
            context.set("stdout", &manifest_group_failure_stdout(&result));
            context.set("stderr", &result.bounded_stderr);
            return Ok(match result.failure_outcome {
                FailureOutcome::Fatal => StepOutcome::Fatal,
                FailureOutcome::Fixable => StepOutcome::Fixable,
            });
        }
    }
    Ok(StepOutcome::Success)
}

fn context_path(context: &StepContext, key: &str) -> PathBuf {
    context
        .get(key)
        .filter(|value| !value.is_empty())
        .map_or_else(|| context.work_dir().clone(), PathBuf::from)
}

fn manifest_group_failure_stdout(result: &ManifestCommandResult) -> String {
    json!({
        "command_id": result.command_id,
        "argv": result.argv,
        "working_directory": result.working_directory.display().to_string(),
        "exit_code": result.exit_code,
        "timed_out": result.timed_out,
        "status": result.status(),
        "stdout": result.bounded_stdout,
        "stderr": result.bounded_stderr,
        "expectation_failures": result.expectation_failures,
        "artifact_failures": result.artifact_failures,
        "spawn_error": result.spawn_error,
    })
    .to_string()
}

fn parse_command_manifest(params: &Value) -> Result<CommandManifest, EngineError> {
    let value = params
        .get("command_manifest")
        .ok_or_else(|| group_error("command_manifest_group requires command_manifest"))?;
    if value.get("command").is_some() || value.get("shell").is_some() {
        return Err(group_error("shell-string command manifests are forbidden"));
    }
    let manifest: CommandManifest = serde_json::from_value(value.clone())
        .map_err(|err| group_error(format!("invalid command_manifest: {err}")))?;
    validate_command_manifest(&manifest).map_err(|err| group_error(err.message))?;
    Ok(manifest)
}

fn group_error(message: impl Into<String>) -> EngineError {
    EngineError::StepExecutionError {
        step_id: "command_manifest_group".to_string(),
        message: message.into(),
    }
}

fn entry_conditions_match(entry: &CommandEntry, work_dir: &Path) -> Result<bool, String> {
    if !entry.run_if_missing_any.is_empty() {
        let mut any_missing = false;
        for path in &entry.run_if_missing_any {
            if !manifest_relative_path(work_dir, path)?.exists() {
                any_missing = true;
                break;
            }
        }
        if !any_missing {
            return Ok(false);
        }
    }
    for path in &entry.run_if_present_all {
        if !manifest_relative_path(work_dir, path)?.exists() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn remove_manifest_paths(entry: &CommandEntry, work_dir: &Path) -> Result<(), String> {
    for (index, path) in entry.remove_before_run.iter().enumerate() {
        let path = manifest_removal_path(work_dir, path)?;
        remove_manifest_path(entry, work_dir, &path, index)?;
    }
    Ok(())
}

fn remove_manifest_path(
    entry: &CommandEntry,
    work_dir: &Path,
    path: &Path,
    index: usize,
) -> Result<(), String> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    validate_removal_target(entry, work_dir, path, &metadata)?;
    let tombstone = tombstone_path(path, index);
    fs::rename(path, &tombstone).map_err(|err| {
        format!(
            "rename command '{}' path '{}' for removal: {err}",
            entry.id,
            path.display()
        )
    })?;
    match remove_tombstone(entry, &tombstone) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = fs::rename(&tombstone, path);
            Err(err)
        }
    }
}

fn remove_tombstone(entry: &CommandEntry, tombstone: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(tombstone).map_err(|err| {
        format!(
            "inspect command '{}' removal path '{}': {err}",
            entry.id,
            tombstone.display()
        )
    })?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(tombstone).map_err(|err| {
            format!(
                "remove command '{}' path '{}': {err}",
                entry.id,
                tombstone.display()
            )
        })
    } else {
        fs::remove_file(tombstone).map_err(|err| {
            format!(
                "remove command '{}' path '{}': {err}",
                entry.id,
                tombstone.display()
            )
        })
    }
}

fn validate_removal_target(
    entry: &CommandEntry,
    work_dir: &Path,
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), String> {
    let root = removal_root(work_dir)?;
    let parent = path.parent().unwrap_or(&root);
    let canonical_parent = parent.canonicalize().map_err(|err| {
        format!(
            "inspect command '{}' removal parent '{}': {err}",
            entry.id,
            parent.display()
        )
    })?;
    if !canonical_parent.starts_with(&root) {
        return Err(format!(
            "remove command '{}' path '{}' must stay under work_dir",
            entry.id,
            path.display()
        ));
    }
    if !metadata.file_type().is_symlink() {
        let canonical_path = path.canonicalize().map_err(|err| {
            format!(
                "inspect command '{}' removal path '{}': {err}",
                entry.id,
                path.display()
            )
        })?;
        if !canonical_path.starts_with(&root) {
            return Err(format!(
                "remove command '{}' path '{}' must stay under work_dir",
                entry.id,
                path.display()
            ));
        }
    }
    Ok(())
}

fn removal_root(work_dir: &Path) -> Result<PathBuf, String> {
    work_dir
        .canonicalize()
        .map_err(|err| format!("inspect removal root '{}': {err}", work_dir.display()))
}

fn tombstone_path(path: &Path, index: usize) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("manifest-path");
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    parent.join(format!(
        ".luther-removing-{}-{index}-{stamp}-{file_name}",
        std::process::id()
    ))
}

fn manifest_removal_path(work_dir: &Path, relative: &str) -> Result<PathBuf, String> {
    let path = manifest_relative_path(work_dir, relative)?;
    if !has_named_path_component(Path::new(relative)) {
        return Err(format!(
            "remove_before_run path must not target work_dir itself: {relative}"
        ));
    }
    Ok(path)
}

fn has_named_path_component(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, std::path::Component::Normal(_)))
}

fn manifest_relative_path(work_dir: &Path, relative: &str) -> Result<PathBuf, String> {
    let path = Path::new(relative);
    if relative.is_empty()
        || relative.contains('\\')
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::Prefix(_)
                    | std::path::Component::RootDir
                    | std::path::Component::ParentDir
            )
        })
    {
        return Err(format!(
            "manifest path must stay under work_dir: {relative}"
        ));
    }
    Ok(work_dir.join(path))
}

#[derive(Clone, Debug)]
pub struct ManifestPathContext {
    pub repo_root: PathBuf,
    pub default_working_directory: PathBuf,
    pub artifact_base_directory: PathBuf,
}

pub fn manifest_path_context_from_step(context: &StepContext) -> ManifestPathContext {
    ManifestPathContext {
        repo_root: context.work_dir().clone(),
        default_working_directory: manifest_default_working_directory(context),
        artifact_base_directory: context_path(context, "artifact_base_dir"),
    }
}

pub fn manifest_default_working_directory(context: &StepContext) -> PathBuf {
    context
        .get("default_command_cwd")
        .filter(|value| !value.is_empty())
        .map_or_else(
            || context_path(context, "project_dir"),
            |value| {
                let path = PathBuf::from(value);
                if path.is_absolute() {
                    path
                } else {
                    context.work_dir().join(path)
                }
            },
        )
}

#[derive(Clone, Debug)]
pub struct ManifestCommandRequest {
    pub command_id: String,
    pub argv: Vec<String>,
    pub working_directory: PathBuf,
    pub artifact_base_directory: PathBuf,
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
pub enum ManifestEntryExecution {
    Skipped,
    Completed(Box<ManifestCommandResult>),
}

pub fn run_manifest_entry(
    entry: &CommandEntry,
    paths: &ManifestPathContext,
    default_timeout_seconds: u64,
) -> Result<ManifestEntryExecution, String> {
    // Manifest condition and removal paths are repository-root-relative, matching
    // their validation as target repository paths rather than command cwd paths.
    if !entry_conditions_match(entry, &paths.repo_root)? {
        return Ok(ManifestEntryExecution::Skipped);
    }
    remove_manifest_paths(entry, &paths.repo_root)?;
    let request = request_from_entry_with_paths(entry, paths, default_timeout_seconds)?;
    Ok(ManifestEntryExecution::Completed(Box::new(
        run_manifest_command(request),
    )))
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

pub fn resolve_manifest_group_id(
    params: &Value,
    context: &StepContext,
    default: &str,
) -> Result<String, String> {
    let raw_group_id = params
        .get("command_manifest_group")
        .and_then(Value::as_str)
        .unwrap_or(default);
    let group_id = interpolate_string(raw_group_id, context);
    if group_id.contains('{') || group_id.contains('}') {
        Err(format!(
            "command_manifest_group contains unresolved template token: {group_id}"
        ))
    } else {
        Ok(group_id)
    }
}

pub fn request_from_entry(
    entry: &CommandEntry,
    work_dir: &Path,
    default_timeout_seconds: u64,
) -> Result<ManifestCommandRequest, String> {
    let path_context = ManifestPathContext {
        repo_root: work_dir.to_path_buf(),
        default_working_directory: work_dir.to_path_buf(),
        artifact_base_directory: work_dir.to_path_buf(),
    };
    request_from_entry_with_paths(entry, &path_context, default_timeout_seconds)
}

pub fn request_from_entry_with_paths(
    entry: &CommandEntry,
    paths: &ManifestPathContext,
    default_timeout_seconds: u64,
) -> Result<ManifestCommandRequest, String> {
    let working_directory = manifest_working_directory(entry, paths)?;
    Ok(ManifestCommandRequest {
        command_id: entry.id.clone(),
        argv: entry.argv.clone(),
        working_directory,
        artifact_base_directory: paths.artifact_base_directory.clone(),
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
            Ok(mut child) => wait_for_manifest_child(
                &mut child,
                request.timeout_seconds,
                request.capture_limit_bytes,
            ),
            Err(err) => return manifest_spawn_error(request, err),
        };
        let output = match output {
            Ok(output) => output,
            Err(err) => return manifest_spawn_error(request, err),
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

fn manifest_working_directory(
    entry: &CommandEntry,
    paths: &ManifestPathContext,
) -> Result<PathBuf, String> {
    let relative = entry
        .working_directory
        .as_deref()
        .or(entry.project_subdirectory.as_deref());
    let candidate = relative.map_or_else(
        || paths.default_working_directory.clone(),
        |relative| paths.repo_root.join(relative),
    );
    validate_manifest_working_directory(&paths.repo_root, &candidate)?;
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
    capture_limit_bytes: usize,
) -> std::io::Result<ProcessOutputCapture> {
    let stdout_buffer = Arc::new(Mutex::new(String::new()));
    let stderr_buffer = Arc::new(Mutex::new(String::new()));
    let stdout_reader = spawn_reader(child.stdout.take(), &stdout_buffer, capture_limit_bytes);
    let stderr_reader = spawn_reader(child.stderr.take(), &stderr_buffer, capture_limit_bytes);
    let wait_result = wait_for_child_exit(child, timeout_seconds);
    let (exit_code, timed_out) = wait_result?;
    let stdout_done = wait_for_reader_after_process_exit(&stdout_reader);
    let stderr_done = wait_for_reader_after_process_exit(&stderr_reader);
    if !stdout_done || !stderr_done {
        terminate_child(child);
        if !stdout_done {
            let _ = wait_for_reader_after_cleanup(&stdout_reader);
        }
        if !stderr_done {
            let _ = wait_for_reader_after_cleanup(&stderr_reader);
        }
    }
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
        if !artifact_matches(&request.artifact_base_directory, artifact) {
            failures.push(format!("required artifact missing: {}", artifact.path));
        }
    }
    for artifact in &request.forbidden_artifacts {
        if artifact_matches(&request.artifact_base_directory, artifact) {
            failures.push(format!("forbidden artifact present: {}", artifact.path));
        }
    }
    failures
}

fn artifact_matches(root: &Path, artifact: &ArtifactExpectation) -> bool {
    let Some(path) = artifact_path_under_root(root, &artifact.path) else {
        return false;
    };
    let Some(path) = resolved_artifact_path_under_root(root, &path) else {
        return false;
    };
    match artifact.kind {
        ArtifactKind::Any => path.exists(),
        ArtifactKind::File => path.is_file(),
        ArtifactKind::Directory => path.is_dir(),
    }
}

fn artifact_path_under_root(root: &Path, artifact_path: &str) -> Option<PathBuf> {
    let path = Path::new(artifact_path);
    let escapes_root = path.components().any(|component| {
        matches!(
            component,
            std::path::Component::Prefix(_)
                | std::path::Component::RootDir
                | std::path::Component::ParentDir
        )
    });
    (!escapes_root).then(|| root.join(path))
}

fn resolved_artifact_path_under_root(root: &Path, path: &Path) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    let path = path.canonicalize().ok()?;
    path.starts_with(&root).then_some(path)
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
    capture_limit_bytes: usize,
) -> mpsc::Receiver<()> {
    let buffer = Arc::clone(buffer);
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        if let Some(pipe) = pipe {
            let text = read_bounded_text(pipe, capture_limit_bytes);
            if let Ok(mut guard) = buffer.lock() {
                *guard = text;
            }
        }
        let _ = sender.send(());
    });
    receiver
}

fn read_bounded_text(pipe: impl Read, capture_limit_bytes: usize) -> String {
    let mut reader = BufReader::new(pipe);
    let capture_bound = capture_limit_bytes.saturating_add(1);
    let mut bytes = Vec::with_capacity(capture_bound.min(8192));
    let mut scratch = [0_u8; 8192];
    loop {
        match reader.read(&mut scratch) {
            Ok(0) => break,
            Ok(count) => {
                let remaining = capture_bound.saturating_sub(bytes.len());
                if remaining > 0 {
                    bytes.extend_from_slice(&scratch[..count.min(remaining)]);
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
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
        let process_group = format!("-{}", child.id());
        let _ = run_process_group_kill("TERM", &process_group);
        thread::sleep(Duration::from_millis(250));
        let _ = run_process_group_kill("KILL", &process_group);
        thread::sleep(Duration::from_millis(25));
    }
    let _ = child.kill();
}

#[cfg(unix)]
fn run_process_group_kill(
    signal: &str,
    process_group: &str,
) -> std::io::Result<std::process::ExitStatus> {
    let mut command = Command::new("/bin/kill");
    command.env_clear();
    apply_allowed_command_environment(&mut command);
    let signal_arg = format!("-{signal}");
    command.args([signal_arg.as_str(), process_group]);
    command.stdin(Stdio::null());
    command.stdout(Stdio::null());
    command.stderr(Stdio::null());
    command.status()
}

fn wait_for_reader_after_process_exit(reader: &mpsc::Receiver<()>) -> bool {
    wait_for_reader_for(reader, Duration::from_millis(100))
}

fn wait_for_reader_after_cleanup(reader: &mpsc::Receiver<()>) -> bool {
    wait_for_reader_for(reader, Duration::from_secs(3))
}

fn wait_for_reader_for(reader: &mpsc::Receiver<()>, timeout: Duration) -> bool {
    reader.recv_timeout(timeout).is_ok()
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
