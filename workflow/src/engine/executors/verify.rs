/// @plan:PLAN-20260408-LLXPRT-FIRST.P06
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// Verify executor - runs a configurable sequence of verification checks.
/// @requirement:REQ-LF-VERIFY-001,REQ-LF-VERIFY-002,REQ-LF-VERIFY-003,REQ-LF-VERIFY-004,REQ-LF-VERIFY-005,REQ-LF-VERIFY-006,REQ-LF-VERIFY-007,REQ-LF-VERIFY-008,REQ-LF-VERIFY-009
use crate::engine::executor::{interpolate_string, StepContext, StepExecutor};
use crate::engine::executors::command_manifest::{
    request_from_entry_with_paths, run_manifest_command, ManifestPathContext,
};
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;
use crate::workflow::command_manifest::{CommandEntry, CommandManifest, FailureOutcome};
use parse::{build_summary, parse_check_output};
use std::io::{BufReader, Read};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

mod parse;

/// Result of a single verification check.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P06
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-001,REQ-LF-VERIFY-005
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CheckResult {
    pub check_type: String,
    pub passed: bool,
    pub exit_code: i32,
    pub errors: Vec<ErrorRecord>,
    pub raw_stdout: String,
    pub raw_stderr: String,
    #[serde(default)]
    pub command: Option<CommandEvidence>,
}

/// Structured error record for parsed errors.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P06
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-005,REQ-LF-VERIFY-006
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct ErrorRecord {
    pub file: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub message: String,
    pub severity: Option<String>,
    pub test_name: Option<String>,
    pub assertion_kind: Option<String>,
    pub expected: Option<String>,
    pub actual: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct CommandEvidence {
    pub command_id: String,
    pub argv: Vec<String>,
    pub cwd: String,
    pub expectation_failures: Vec<String>,
    pub artifact_failures: Vec<String>,
    pub failure_classification: String,
}

/// Report containing all verification check results.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P06
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-003,REQ-LF-VERIFY-004,REQ-LF-VERIFY-005
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VerifyReport {
    pub passed: bool,
    pub summary: String,
    pub checks: Vec<CheckResult>,
}

/// Verify executor that runs a configurable sequence of verification checks.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P06
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-001,REQ-LF-VERIFY-002,REQ-LF-VERIFY-003,REQ-LF-VERIFY-004,REQ-LF-VERIFY-007,REQ-LF-VERIFY-008,REQ-LF-VERIFY-009
#[derive(Debug, Clone, Copy)]
pub struct VerifyExecutor;

impl StepExecutor for VerifyExecutor {
    /// Execute the verify step with configurable check sequence.
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P06
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P08
    /// @requirement:REQ-LF-VERIFY-001,REQ-LF-VERIFY-002,REQ-LF-VERIFY-003,REQ-LF-VERIFY-004,REQ-LF-VERIFY-007,REQ-LF-VERIFY-008,REQ-LF-VERIFY-009
    // Pre-existing verify execution flow; split in a dedicated refactor stage.
    #[allow(clippy::too_many_lines)]
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        let check_names = verify_check_sequence(params)?;

        // Validate the verification profile (defaults to npm for backward
        // compatibility). Unknown profiles are a configuration error.
        let profile = resolve_profile(params);
        if !is_valid_profile(profile) {
            return Err(EngineError::StepExecutionError {
                step_id: "verify".to_string(),
                message: format!(
                    "Unknown verification profile '{profile}'. Valid profiles: {}",
                    VALID_PROFILES.join(", ")
                ),
            });
        }

        // If checks array is empty, return success immediately
        if check_names.is_empty() {
            context.set("verify_passed", "true");
            context.set("verify_summary", "No checks configured");
            return Ok(StepOutcome::Success);
        }

        let work_dir = context.work_dir().to_path_buf();
        let artifact_root = resolve_artifact_root(context, params)?;
        let timeout = params
            .get("timeout_seconds")
            .and_then(serde_json::Value::as_u64)
            .map(Duration::from_secs);
        let mut verify_state = VerifyRunState::default();

        // Process each check
        for check_type in &check_names {
            let command_result = run_check(check_type, params, context, &work_dir, timeout)?;
            verify_state.record(command_result);
            if verify_state.should_stop() {
                break;
            }
        }

        // Build summary (REQ-LF-VERIFY-004)
        let summary = build_summary(&verify_state.results);

        // Build report (REQ-LF-VERIFY-005)
        let report = VerifyReport {
            passed: verify_state.all_passed,
            summary: summary.clone(),
            checks: verify_state.results,
        };

        // Write report to file (REQ-LF-VERIFY-003)
        let report_json =
            serde_json::to_string_pretty(&report).map_err(|e| EngineError::StepExecutionError {
                step_id: "verify".to_string(),
                message: format!("Failed to serialize report: {e}"),
            })?;
        let luther_dir = artifact_root;
        std::fs::create_dir_all(&luther_dir).map_err(|e| EngineError::StepExecutionError {
            step_id: "verify".to_string(),
            message: format!("Failed to create .luther directory: {e}"),
        })?;
        let report_path = luther_dir.join("verify-report.json");
        std::fs::write(&report_path, report_json).map_err(|e| EngineError::StepExecutionError {
            step_id: "verify".to_string(),
            message: format!("Failed to write report file: {e}"),
        })?;

        // Set context variables (REQ-LF-VERIFY-002, REQ-LF-VERIFY-004, REQ-LF-VERIFY-009)
        context.set(
            "verify_passed",
            if verify_state.all_passed {
                "true"
            } else {
                "false"
            },
        );
        context.set("verify_summary", &summary);

        // Set per-check-type error context vars
        for result in &report.checks {
            if !result.errors.is_empty() {
                let error_json = serde_json::to_string(&result.errors).map_err(|e| {
                    EngineError::StepExecutionError {
                        step_id: "verify".to_string(),
                        message: format!("Failed to serialize errors: {e}"),
                    }
                })?;
                match result.check_type.as_str() {
                    "test" => context.set("test_failures", &error_json),
                    "build" => context.set("build_errors", &error_json),
                    "typecheck" => context.set("type_errors", &error_json),
                    "lint" => context.set("lint_errors", &error_json),
                    "format" => context.set("format_errors", &error_json),
                    _ => {}
                }
            }
        }

        if verify_state.all_passed {
            Ok(StepOutcome::Success)
        } else if verify_state.fatal_failed {
            Ok(StepOutcome::Fatal)
        } else {
            Ok(StepOutcome::Fixable)
        }
    }
}

struct VerifyRunState {
    results: Vec<CheckResult>,
    all_passed: bool,
    fatal_failed: bool,
    stop_after_current: bool,
}

impl Default for VerifyRunState {
    fn default() -> Self {
        Self {
            results: Vec::new(),
            all_passed: true,
            fatal_failed: false,
            stop_after_current: false,
        }
    }
}

impl VerifyRunState {
    fn record(&mut self, outcome: CheckExecutionOutcome) {
        match outcome {
            CheckExecutionOutcome::Completed(result) => self.record_result(result),
            CheckExecutionOutcome::TimedOut(result) => {
                self.record_result(result);
                self.stop_after_current = true;
            }
        }
    }

    fn record_result(&mut self, result: CheckResult) {
        if !result.passed {
            self.all_passed = false;
            self.fatal_failed |= check_result_is_fatal(&result);
        }
        self.results.push(result);
    }

    fn should_stop(&self) -> bool {
        self.stop_after_current || self.fatal_failed
    }
}

struct CapturedOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

enum CheckExecutionOutcome {
    Completed(CheckResult),
    TimedOut(CheckResult),
}

fn run_check(
    check_type: &str,
    params: &serde_json::Value,
    context: &mut StepContext,
    work_dir: &std::path::Path,
    timeout: Option<Duration>,
) -> Result<CheckExecutionOutcome, EngineError> {
    if check_type == "diff_or_existing_pr" {
        return run_diff_or_existing_pr_check(params, context);
    }
    if let Some(result) = run_manifest_check(check_type, params, context, timeout)? {
        return Ok(CheckExecutionOutcome::Completed(result));
    }

    let command = resolve_check_command(check_type, params, context)?;
    run_legacy_check(check_type, &command, context, work_dir, timeout)
}

fn run_diff_or_existing_pr_check(
    params: &serde_json::Value,
    context: &StepContext,
) -> Result<CheckExecutionOutcome, EngineError> {
    let config = params
        .get("diff_or_existing_pr")
        .ok_or_else(|| diff_gate_error("missing diff_or_existing_pr parameters"))?;
    let required_path_regex =
        interpolate_string(&required_string(config, "required_path_regex")?, context);
    let regex = regex::Regex::new(&required_path_regex)
        .map_err(|err| diff_gate_error(format!("invalid required_path_regex: {err}")))?;
    let existing_pr =
        optional_string(config, "existing_pr_number", context).unwrap_or_else(|| "0".to_string());
    let has_existing_pr = valid_existing_pr_number(&existing_pr);
    let changed_paths = git_changed_paths(context.work_dir())?;
    let matched_path = changed_paths
        .iter()
        .filter_map(|path| normalize_diff_path(context, path))
        .find(|path| regex.is_match(path));
    let passed = matched_path.is_some() || has_existing_pr;
    let message = optional_string(config, "failure_message", context)
        .unwrap_or_else(|| "required changed path not found".to_string());
    Ok(CheckExecutionOutcome::Completed(diff_gate_result(
        passed,
        matched_path,
        changed_paths,
        message,
    )))
}

fn valid_existing_pr_number(existing_pr: &str) -> bool {
    existing_pr
        .trim()
        .parse::<u64>()
        .is_ok_and(|number| number != 0)
}

fn git_changed_paths(work_dir: &std::path::Path) -> Result<Vec<String>, EngineError> {
    let output = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(work_dir)
        .output()
        .map_err(|err| diff_gate_error(format!("failed to run git status: {err}")))?;
    if !output.status.success() {
        return Err(diff_gate_error(format!(
            "git status failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_git_status_path)
        .collect())
}

fn parse_git_status_path(line: &str) -> Option<String> {
    line.get(3..)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(|path| path.split(" -> ").last().unwrap_or(path).to_string())
}

fn normalize_diff_path(context: &StepContext, path: &str) -> Option<String> {
    if context.get("diff_path_normalization").map(String::as_str) != Some("base_relative") {
        return Some(path.to_string());
    }
    let base = context.get("diff_path_base")?;
    if base.is_empty() {
        return Some(path.to_string());
    }
    path.strip_prefix(&format!("{base}/"))
        .map(ToString::to_string)
}

fn diff_gate_result(
    passed: bool,
    matched_path: Option<String>,
    changed_paths: Vec<String>,
    failure_message: String,
) -> CheckResult {
    let stdout = matched_path.unwrap_or_default();
    let errors = if passed {
        Vec::new()
    } else {
        vec![ErrorRecord {
            message: failure_message.clone(),
            severity: Some("error".to_string()),
            ..ErrorRecord::default()
        }]
    };
    CheckResult {
        check_type: "diff_or_existing_pr".to_string(),
        passed,
        exit_code: if passed { 0 } else { 1 },
        errors,
        raw_stdout: stdout,
        raw_stderr: if passed {
            String::new()
        } else {
            failure_message
        },
        command: Some(CommandEvidence {
            command_id: "diff_or_existing_pr".to_string(),
            argv: changed_paths,
            failure_classification: "fatal".to_string(),
            ..CommandEvidence::default()
        }),
    }
}

fn required_string(value: &serde_json::Value, key: &str) -> Result<String, EngineError> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| diff_gate_error(format!("diff_or_existing_pr.{key} is required")))
}

fn optional_string(value: &serde_json::Value, key: &str, context: &StepContext) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(|value| interpolate_string(value, context))
}

fn diff_gate_error(message: impl Into<String>) -> EngineError {
    EngineError::StepExecutionError {
        step_id: "verify".to_string(),
        message: message.into(),
    }
}

fn run_legacy_check(
    check_type: &str,
    command: &str,
    context: &mut StepContext,
    work_dir: &std::path::Path,
    timeout: Option<Duration>,
) -> Result<CheckExecutionOutcome, EngineError> {
    let output = match run_command_with_timeout(command, work_dir, timeout) {
        Ok(WaitResult::Completed(output)) => output,
        Ok(WaitResult::TimedOut { timeout }) => {
            return Ok(CheckExecutionOutcome::TimedOut(timeout_result(
                check_type, context, timeout,
            )));
        }
        Err(WaitError::Spawn(e)) => {
            context.set("verify_error", &format!("Failed to run {check_type}: {e}"));
            return Ok(CheckExecutionOutcome::Completed(fatal_error_result(
                check_type,
                format!("Failed to run {check_type}: {e}"),
            )));
        }
        Err(WaitError::Wait(e)) => {
            context.set(
                "verify_error",
                &format!("Failed to complete {check_type}: {e}"),
            );
            return Ok(CheckExecutionOutcome::Completed(fatal_error_result(
                check_type,
                format!("Failed to complete {check_type}: {e}"),
            )));
        }
    };
    completed_legacy_result(check_type, output, context)
}

fn timeout_result(check_type: &str, context: &mut StepContext, timeout: Duration) -> CheckResult {
    let message = format!("{check_type} timed out after {} seconds", timeout.as_secs());
    context.set("verify_error", &message);
    CheckResult {
        check_type: check_type.to_string(),
        passed: false,
        exit_code: 124,
        errors: vec![ErrorRecord {
            message: message.clone(),
            severity: Some("error".to_string()),
            ..ErrorRecord::default()
        }],
        raw_stdout: String::new(),
        raw_stderr: message,
        command: None,
    }
}

fn fatal_error_result(check_type: &str, message: String) -> CheckResult {
    CheckResult {
        check_type: check_type.to_string(),
        passed: false,
        exit_code: -1,
        errors: vec![ErrorRecord {
            message,
            severity: Some("error".to_string()),
            ..ErrorRecord::default()
        }],
        raw_stdout: String::new(),
        raw_stderr: String::new(),
        command: Some(CommandEvidence {
            failure_classification: "fatal".to_string(),
            ..CommandEvidence::default()
        }),
    }
}

fn completed_legacy_result(
    check_type: &str,
    output: CapturedOutput,
    context: &mut StepContext,
) -> Result<CheckExecutionOutcome, EngineError> {
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code().unwrap_or(-1);

    if is_command_not_found(exit_code, &stderr) {
        context.set(
            "verify_error",
            &format!("Failed to run {check_type}: command not found"),
        );
        return Ok(CheckExecutionOutcome::Completed(fatal_error_result(
            check_type,
            format!("Failed to run {check_type}: command not found"),
        )));
    }

    Ok(CheckExecutionOutcome::Completed(CheckResult {
        check_type: check_type.to_string(),
        passed: exit_code == 0,
        exit_code,
        errors: parse_check_output(check_type, &stdout, &stderr, exit_code),
        raw_stdout: cap_output(&stdout),
        raw_stderr: cap_output(&stderr),
        command: None,
    }))
}

fn is_command_not_found(exit_code: i32, stderr: &str) -> bool {
    exit_code == 127
        || stderr.contains("command not found")
        || stderr.contains("No such file or directory")
}

fn check_result_is_fatal(result: &CheckResult) -> bool {
    result
        .command
        .as_ref()
        .is_some_and(|command| command.failure_classification == "fatal")
}

enum WaitResult {
    Completed(CapturedOutput),
    TimedOut { timeout: Duration },
}

enum WaitError {
    Spawn(std::io::Error),
    Wait(std::io::Error),
}

fn run_command_with_timeout(
    command: &str,
    work_dir: &std::path::Path,
    timeout: Option<Duration>,
) -> Result<WaitResult, WaitError> {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(work_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = cmd.spawn().map_err(WaitError::Spawn)?;
    let stdout_reader = child.stdout.take().map(spawn_output_reader);
    let stderr_reader = child.stderr.take().map(spawn_output_reader);

    let status = if let Some(timeout) = timeout {
        let start = Instant::now();
        loop {
            if let Some(status) = child.try_wait().map_err(WaitError::Wait)? {
                break Some(status);
            }

            if start.elapsed() >= timeout {
                terminate_command(command, child.id());
                let _ = child.kill();
                let _ = child.wait();
                let _ = join_output_reader(stdout_reader);
                let _ = join_output_reader(stderr_reader);
                return Ok(WaitResult::TimedOut { timeout });
            }

            thread::sleep(Duration::from_millis(100));
        }
    } else {
        Some(child.wait().map_err(WaitError::Wait)?)
    };

    let stdout = join_output_reader(stdout_reader).unwrap_or_default();
    let stderr = join_output_reader(stderr_reader).unwrap_or_default();

    Ok(WaitResult::Completed(CapturedOutput {
        status: status.expect("wait status is set before output capture"),
        stdout,
        stderr,
    }))
}

fn spawn_output_reader<R>(reader: R) -> thread::JoinHandle<Vec<u8>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut output = Vec::new();
        let _ = reader.read_to_end(&mut output);
        output
    })
}

fn join_output_reader(reader: Option<thread::JoinHandle<Vec<u8>>>) -> Option<Vec<u8>> {
    reader.and_then(|handle| handle.join().ok())
}

fn terminate_command(_command: &str, shell_pid: u32) {
    #[cfg(unix)]
    {
        let pgid = format!("-{}", shell_pid);
        let _ = Command::new("kill").args(["-TERM", &pgid]).status();
        thread::sleep(Duration::from_millis(250));
        let _ = Command::new("kill").args(["-KILL", &pgid]).status();
    }

    #[cfg(not(unix))]
    {
        let _ = shell_pid;
    }
}

fn cap_output(output: &str) -> String {
    const MAX_OUTPUT_BYTES: usize = 20_000;
    cap_text(output, MAX_OUTPUT_BYTES, "verifier output")
}

fn cap_text(output: &str, max_bytes: usize, label: &str) -> String {
    if output.len() <= max_bytes {
        return output.to_string();
    }

    let head_len = max_bytes / 2;
    let tail_len = max_bytes - head_len;
    let mut head_end = head_len.min(output.len());
    while !output.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = output.len().saturating_sub(tail_len);
    while !output.is_char_boundary(tail_start) {
        tail_start += 1;
    }

    format!(
        "{}\n\n[... {label} truncated: {} bytes omitted ...]\n\n{}",
        &output[..head_end],
        tail_start.saturating_sub(head_end),
        &output[tail_start..],
    )
}

fn resolve_artifact_root(
    context: &StepContext,
    params: &serde_json::Value,
) -> Result<PathBuf, EngineError> {
    let root = params
        .get("artifact_root")
        .and_then(serde_json::Value::as_str)
        .map_or_else(
            || context.work_dir().join(".luther"),
            |template| PathBuf::from(interpolate_string(template, context)),
        );

    Ok(if root.is_absolute() {
        root
    } else {
        context.work_dir().join(root)
    })
}

/// Valid verification profile names.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P06
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-007
const VALID_PROFILES: &[&str] = &["npm", "pnpm", "yarn", "cargo", "python", "go", "custom"];

/// Default verification profile used when none is specified.
const DEFAULT_PROFILE: &str = "npm";

/// Return whether a profile name is recognized.
/// @requirement:REQ-LF-VERIFY-007
fn is_valid_profile(profile: &str) -> bool {
    VALID_PROFILES.contains(&profile)
}

/// Read the configured verification profile, defaulting to npm.
/// @requirement:REQ-LF-VERIFY-007
fn resolve_profile(params: &serde_json::Value) -> &str {
    params
        .get("profile")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(DEFAULT_PROFILE)
}

/// Map a check type to its default command for the given profile.
///
/// Returns `None` when the profile defines no default for that check type
/// (for example `cargo` has no `typecheck`, and `custom` has no defaults at
/// all). The `custom` profile intentionally returns `None` for every check
/// type so that an explicit `check_commands` override is required.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P06
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-007
pub fn profile_default_command(profile: &str, check_type: &str) -> Option<&'static str> {
    match profile {
        "npm" => match check_type {
            "lint" => Some("npm run lint 2>&1"),
            "typecheck" => Some("npm run typecheck 2>&1"),
            "test" => Some("npm run test 2>&1"),
            "format" => Some("npm run format:check 2>&1"),
            "build" => Some("npm run build 2>&1"),
            _ => None,
        },
        "pnpm" => match check_type {
            "lint" => Some("pnpm run lint 2>&1"),
            "typecheck" => Some("pnpm run typecheck 2>&1"),
            "test" => Some("pnpm run test 2>&1"),
            "format" => Some("pnpm run format:check 2>&1"),
            "build" => Some("pnpm run build 2>&1"),
            _ => None,
        },
        "yarn" => match check_type {
            "lint" => Some("yarn lint 2>&1"),
            "typecheck" => Some("yarn typecheck 2>&1"),
            "test" => Some("yarn test 2>&1"),
            "format" => Some("yarn format:check 2>&1"),
            "build" => Some("yarn build 2>&1"),
            _ => None,
        },
        "cargo" => match check_type {
            "lint" => Some("cargo clippy 2>&1"),
            "test" => Some("cargo test 2>&1"),
            "format" => Some("cargo fmt --check 2>&1"),
            "build" => Some("cargo build 2>&1"),
            _ => None,
        },
        "python" => match check_type {
            "lint" => Some("ruff check . 2>&1"),
            "typecheck" => Some("mypy . 2>&1"),
            "test" => Some("pytest 2>&1"),
            "format" => Some("ruff format --check . 2>&1"),
            _ => None,
        },
        "go" => match check_type {
            "lint" => Some("golangci-lint run 2>&1"),
            "test" => Some("go test ./... 2>&1"),
            "format" => Some("gofmt -l . 2>&1"),
            "build" => Some("go build ./... 2>&1"),
            _ => None,
        },
        // "custom" and any other profile define no defaults.
        _ => None,
    }
}

/// Resolve the command for a specific check type.
///
/// Precedence (highest first): explicit `check_commands[check_type]` override,
/// then the selected profile's default command. An error is returned when the
/// active profile defines no default for the check type and no override was
/// provided.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P06
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-007
fn verify_check_sequence(params: &serde_json::Value) -> Result<Vec<String>, EngineError> {
    let mut check_names = explicit_check_sequence(params)?;
    if let Some(mut manifest_names) = manifest_group_check_sequence(params)? {
        if let Some(position) = check_names
            .iter()
            .position(|check_name| check_name == "command_manifest")
        {
            check_names.splice(position..=position, manifest_names);
        } else if check_names.is_empty() {
            check_names.append(&mut manifest_names);
        }
    }
    if check_names.is_empty() && params.get("checks").is_none() {
        return Err(EngineError::StepExecutionError {
            step_id: "verify".to_string(),
            message: "Missing 'checks' parameter".to_string(),
        });
    }
    Ok(check_names)
}

fn explicit_check_sequence(params: &serde_json::Value) -> Result<Vec<String>, EngineError> {
    let Some(checks) = params.get("checks") else {
        return Ok(Vec::new());
    };
    checks
        .as_array()
        .ok_or_else(|| EngineError::StepExecutionError {
            step_id: "verify".to_string(),
            message: "'checks' parameter must be an array".to_string(),
        })?
        .iter()
        .map(|check| {
            check
                .as_str()
                .map(ToString::to_string)
                .ok_or_else(|| EngineError::StepExecutionError {
                    step_id: "verify".to_string(),
                    message: "Check type must be a string".to_string(),
                })
        })
        .collect()
}

fn manifest_group_check_sequence(
    params: &serde_json::Value,
) -> Result<Option<Vec<String>>, EngineError> {
    let Some(manifest_value) = params.get("command_manifest") else {
        return Ok(None);
    };
    reject_shell_manifest(manifest_value)?;
    let manifest = parse_manifest_value(manifest_value)?;
    let group_id = params
        .get("command_manifest_group")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("local");
    Ok(manifest.groups.get(group_id).cloned())
}

fn parse_manifest_value(
    manifest_value: &serde_json::Value,
) -> Result<CommandManifest, EngineError> {
    serde_json::from_value(manifest_value.clone()).map_err(|err| EngineError::StepExecutionError {
        step_id: "verify".to_string(),
        message: format!("invalid command_manifest: {err}"),
    })
}

fn reject_shell_manifest(manifest_value: &serde_json::Value) -> Result<(), EngineError> {
    if manifest_value.get("command").is_some() || manifest_value.get("shell").is_some() {
        return Err(EngineError::StepExecutionError {
            step_id: "verify".to_string(),
            message: "shell-string command manifests are forbidden".to_string(),
        });
    }
    Ok(())
}

fn run_manifest_check(
    check_type: &str,
    params: &serde_json::Value,
    context: &StepContext,
    timeout: Option<Duration>,
) -> Result<Option<CheckResult>, EngineError> {
    let Some(entry) = manifest_entry_for_check(check_type, params)? else {
        return Ok(None);
    };
    let default_timeout = timeout.map_or(900, |duration| duration.as_secs());
    let path_context = manifest_path_context(context);
    let request = request_from_entry_with_paths(&entry, &path_context, default_timeout).map_err(
        |message| EngineError::StepExecutionError {
            step_id: "verify".to_string(),
            message,
        },
    )?;
    let result = run_manifest_command(request);
    let mut errors = parse_check_output(
        check_type,
        &result.bounded_stdout,
        &result.bounded_stderr,
        result.exit_code.unwrap_or(-1),
    );
    errors.extend(
        result
            .expectation_failures
            .iter()
            .chain(result.artifact_failures.iter())
            .map(|message| ErrorRecord {
                message: message.clone(),
                severity: Some("error".to_string()),
                ..ErrorRecord::default()
            }),
    );
    Ok(Some(CheckResult {
        check_type: check_type.to_string(),
        passed: result.passed(),
        exit_code: result.exit_code.unwrap_or(-1),
        errors,
        raw_stdout: result.bounded_stdout,
        raw_stderr: result.bounded_stderr,
        command: Some(CommandEvidence {
            command_id: result.command_id,
            argv: result.argv,
            cwd: result.working_directory.to_string_lossy().to_string(),
            expectation_failures: result.expectation_failures,
            artifact_failures: result.artifact_failures,
            failure_classification: failure_classification(&result.failure_outcome),
        }),
    }))
}
fn manifest_path_context(context: &StepContext) -> ManifestPathContext {
    ManifestPathContext {
        repo_root: context.work_dir().clone(),
        default_working_directory: context_path(context, "project_dir"),
        artifact_base_directory: context_path(context, "artifact_base_dir"),
    }
}

fn context_path(context: &StepContext, key: &str) -> PathBuf {
    context
        .get(key)
        .filter(|value| !value.is_empty())
        .map_or_else(|| context.work_dir().clone(), PathBuf::from)
}

fn manifest_entry_for_check(
    check_type: &str,
    params: &serde_json::Value,
) -> Result<Option<CommandEntry>, EngineError> {
    let Some(manifest_value) = params.get("command_manifest") else {
        return Ok(None);
    };
    reject_shell_manifest(manifest_value)?;
    let manifest = parse_manifest_value(manifest_value)?;
    Ok(manifest
        .commands
        .into_iter()
        .find(|entry| entry.id == check_type))
}

fn failure_classification(outcome: &FailureOutcome) -> String {
    match outcome {
        FailureOutcome::Fatal => "fatal".to_string(),
        FailureOutcome::Fixable => "fixable".to_string(),
    }
}

pub fn resolve_check_command(
    check_type: &str,
    params: &serde_json::Value,
    context: &StepContext,
) -> Result<String, EngineError> {
    // Check custom commands first - explicit overrides always win.
    if let Some(custom_commands) = params.get("check_commands") {
        if let Some(custom_cmd) = custom_commands.get(check_type).and_then(|v| v.as_str()) {
            return Ok(interpolate_string(custom_cmd, context));
        }
    }

    // Fall back to the selected profile's ecosystem-appropriate default.
    let profile = resolve_profile(params);
    if let Some(command) = profile_default_command(profile, check_type) {
        return Ok(command.to_string());
    }

    Err(EngineError::StepExecutionError {
        step_id: "verify".to_string(),
        message: format!(
            "Check type '{check_type}' is not defined in profile '{profile}' and no check_commands override was provided"
        ),
    })
}
