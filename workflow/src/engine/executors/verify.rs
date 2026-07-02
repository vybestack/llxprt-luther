/// @plan:PLAN-20260408-LLXPRT-FIRST.P06
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// Verify executor - runs a configurable sequence of verification checks.
/// @requirement:REQ-LF-VERIFY-001,REQ-LF-VERIFY-002,REQ-LF-VERIFY-003,REQ-LF-VERIFY-004,REQ-LF-VERIFY-005,REQ-LF-VERIFY-006,REQ-LF-VERIFY-007,REQ-LF-VERIFY-008,REQ-LF-VERIFY-009
use crate::engine::executor::{interpolate_string, StepContext, StepExecutor};
use crate::engine::executors::command_manifest::{request_from_entry, run_manifest_command};
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;
use crate::workflow::command_manifest::{CommandEntry, CommandManifest, FailureOutcome};
use regex::Regex;
use std::io::{BufReader, Read};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

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
        let mut results: Vec<CheckResult> = Vec::new();
        let mut all_passed = true;
        let mut fatal_failed = false;

        // Process each check
        for check_type in &check_names {
            if let Some(result) = run_manifest_check(check_type, params, context, timeout)? {
                if !result.passed {
                    all_passed = false;
                }
                let is_fatal = result
                    .command
                    .as_ref()
                    .is_some_and(|command| command.failure_classification == "fatal");
                if !result.passed && is_fatal {
                    fatal_failed = true;
                }
                results.push(result);
                if fatal_failed {
                    break;
                }
                continue;
            }

            // Resolve the command for this check type
            let command = resolve_check_command(check_type, params, context)?;

            let output = match run_command_with_timeout(&command, &work_dir, timeout) {
                Ok(WaitResult::Completed(output)) => output,
                Ok(WaitResult::TimedOut { timeout }) => {
                    context.set(
                        "verify_error",
                        &format!("{check_type} timed out after {} seconds", timeout.as_secs()),
                    );
                    all_passed = false;
                    results.push(CheckResult {
                        check_type: check_type.to_string(),
                        passed: false,
                        exit_code: 124,
                        errors: vec![ErrorRecord {
                            message: format!(
                                "{check_type} timed out after {} seconds",
                                timeout.as_secs()
                            ),
                            severity: Some("error".to_string()),
                            ..ErrorRecord::default()
                        }],
                        raw_stdout: String::new(),
                        raw_stderr: format!(
                            "{check_type} timed out after {} seconds",
                            timeout.as_secs()
                        ),
                        command: None,
                    });
                    break;
                }
                Err(WaitError::Spawn(e)) => {
                    context.set("verify_error", &format!("Failed to run {check_type}: {e}"));
                    return Ok(StepOutcome::Fatal);
                }
                Err(WaitError::Wait(e)) => {
                    context.set(
                        "verify_error",
                        &format!("Failed to complete {check_type}: {e}"),
                    );
                    return Ok(StepOutcome::Fatal);
                }
            };

            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let exit_code = output.status.code().unwrap_or(-1);

            // Check if this is a "command not found" error (REQ-LF-VERIFY-008)
            // Exit code 127 typically means command not found in Unix shells
            let is_command_not_found = exit_code == 127
                || stderr.contains("command not found")
                || stderr.contains("No such file or directory");

            if is_command_not_found {
                context.set(
                    "verify_error",
                    &format!("Failed to run {check_type}: command not found"),
                );
                return Ok(StepOutcome::Fatal);
            }

            // Parse output based on check type
            let errors = parse_check_output(check_type, &stdout, &stderr, exit_code);
            let passed = exit_code == 0;

            if !passed {
                all_passed = false;
            }

            results.push(CheckResult {
                check_type: check_type.to_string(),
                passed,
                exit_code,
                errors,
                raw_stdout: cap_output(&stdout),
                raw_stderr: cap_output(&stderr),
                command: None,
            });
        }

        // Build summary (REQ-LF-VERIFY-004)
        let summary = build_summary(&results);

        // Build report (REQ-LF-VERIFY-005)
        let report = VerifyReport {
            passed: all_passed,
            summary: summary.clone(),
            checks: results,
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
        context.set("verify_passed", if all_passed { "true" } else { "false" });
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

        if all_passed {
            Ok(StepOutcome::Success)
        } else if fatal_failed {
            Ok(StepOutcome::Fatal)
        } else {
            Ok(StepOutcome::Fixable)
        }
    }
}
struct CapturedOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
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

fn cap_error_message(message: &str) -> String {
    const MAX_ERROR_MESSAGE_BYTES: usize = 4_000;
    cap_text(message, MAX_ERROR_MESSAGE_BYTES, "verifier error message")
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
    let request =
        request_from_entry(&entry, context.work_dir(), default_timeout).map_err(|message| {
            EngineError::StepExecutionError {
                step_id: "verify".to_string(),
                message,
            }
        })?;
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

/// Parse the output of a check and extract errors.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P06
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-005
fn parse_check_output(
    check_type: &str,
    stdout: &str,
    stderr: &str,
    exit_code: i32,
) -> Vec<ErrorRecord> {
    if exit_code == 0 {
        return vec![];
    }

    match check_type {
        "typecheck" => parse_typescript_errors(stdout, stderr),
        "test" => parse_test_results(stdout, stderr),
        "lint" => parse_lint_errors(stdout, stderr),
        "format" => parse_format_errors(stdout, stderr),
        "build" => parse_build_errors(stdout, stderr),
        "diff" => parse_diff_errors(stdout, stderr),
        _ => {
            // Unknown check type - wrap raw output in ErrorRecord
            let combined = format!("{stdout}{stderr}").trim().to_string();
            vec![ErrorRecord {
                file: None,
                line: None,
                column: None,
                message: if combined.is_empty() {
                    format!("Check failed with exit code {exit_code}")
                } else {
                    combined
                },
                severity: Some("error".to_string()),
                test_name: None,
                assertion_kind: None,
                expected: None,
                actual: None,
            }]
        }
    }
}

/// Parse TypeScript compiler errors from output.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P06
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-005
fn parse_typescript_errors(stdout: &str, stderr: &str) -> Vec<ErrorRecord> {
    let mut errors = Vec::new();
    let combined = format!("{stdout}{stderr}");

    // Regex pattern: file(line,col): error TSxxxx: message
    // Example: src/foo.ts(10,5): error TS2322: Type X is not assignable to Type Y
    let ts_regex = Regex::new(r"^(.+)\((\d+),(\d+)\): error (TS\d+): (.+)$").unwrap();

    for line in combined.lines() {
        if let Some(caps) = ts_regex.captures(line) {
            let file = caps
                .get(1)
                .map(|m: regex::Match<'_>| m.as_str().to_string());
            let line_num = caps
                .get(2)
                .and_then(|m: regex::Match<'_>| m.as_str().parse::<u32>().ok());
            let col_num = caps
                .get(3)
                .and_then(|m: regex::Match<'_>| m.as_str().parse::<u32>().ok());
            let error_code = caps
                .get(4)
                .map(|m: regex::Match<'_>| m.as_str().to_string());
            let message = caps
                .get(5)
                .map(|m: regex::Match<'_>| m.as_str().to_string());

            let full_message = if let Some(code) = error_code {
                format!("{}: {}", code, message.unwrap_or_default())
            } else {
                message.unwrap_or_default()
            };

            errors.push(ErrorRecord {
                file,
                line: line_num,
                column: col_num,
                message: full_message,
                severity: Some("error".to_string()),
                test_name: None,
                assertion_kind: None,
                expected: None,
                actual: None,
            });
        }
    }

    // Fallback: if no errors parsed but there was output, wrap raw output
    if errors.is_empty() && !combined.trim().is_empty() {
        errors.push(ErrorRecord {
            file: None,
            line: None,
            column: None,
            message: combined.trim().to_string(),
            severity: Some("error".to_string()),
            test_name: None,
            assertion_kind: None,
            expected: None,
            actual: None,
        });
    }

    errors
}

/// Unescape a string that may have shell-escaped quotes.
/// Converts \\\" back to " for JSON parsing.
fn unescape_shell_json(s: &str) -> String {
    s.replace("\\\"", "\"")
}

/// Escape helper: converts escaped JSON from test commands
/// Parse test results from test runner output.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P06
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-006
fn parse_test_results(stdout: &str, stderr: &str) -> Vec<ErrorRecord> {
    let mut errors = Vec::new();

    // Try JSON parse first (vitest --reporter=json)
    // Also try with unescaped quotes in case shell escaped them
    let json_result = serde_json::from_str::<serde_json::Value>(stdout)
        .or_else(|_| serde_json::from_str::<serde_json::Value>(&unescape_shell_json(stdout)));

    if let Ok(json) = json_result {
        if let Some(test_results) = json.get("testResults").and_then(|v| v.as_array()) {
            for test_file in test_results {
                let file_path = test_file
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(String::from);

                if let Some(assertion_results) =
                    test_file.get("assertionResults").and_then(|v| v.as_array())
                {
                    for test in assertion_results {
                        if let Some(status) = test.get("status").and_then(|v| v.as_str()) {
                            if status == "failed" {
                                let test_name = test
                                    .get("fullName")
                                    .and_then(|v| v.as_str())
                                    .map(String::from);

                                let message = test
                                    .get("failureMessages")
                                    .and_then(|v| v.as_array())
                                    .map(|arr| {
                                        arr.iter()
                                            .filter_map(|v| v.as_str())
                                            .collect::<Vec<_>>()
                                            .join("\n")
                                    })
                                    .unwrap_or_default();

                                errors.push(ErrorRecord {
                                    file: file_path.clone(),
                                    line: None,
                                    column: None,
                                    message,
                                    severity: Some("error".to_string()),
                                    test_name,
                                    assertion_kind: Some("assertion".to_string()),
                                    expected: None,
                                    actual: None,
                                });
                            }
                        }
                    }
                }
            }
        }

        if !errors.is_empty() {
            return errors;
        }
    }

    // Fallback: just return raw output as a single error
    let combined = format!("{stdout}{stderr}").trim().to_string();
    if !combined.is_empty() {
        errors.push(ErrorRecord {
            file: None,
            line: None,
            column: None,
            message: combined,
            severity: Some("error".to_string()),
            test_name: None,
            assertion_kind: None,
            expected: None,
            actual: None,
        });
    }

    errors
}

/// Parse lint errors from linter output.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P06
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-005
fn parse_lint_errors(stdout: &str, stderr: &str) -> Vec<ErrorRecord> {
    let mut errors = Vec::new();

    // Try JSON parse (eslint --format json)
    // Also try with unescaped quotes in case shell escaped them
    let json_result = serde_json::from_str::<serde_json::Value>(stdout)
        .or_else(|_| serde_json::from_str::<serde_json::Value>(&unescape_shell_json(stdout)));

    if let Ok(json_array) = json_result {
        if let Some(results) = json_array.as_array() {
            for file_result in results {
                let file_path = file_result
                    .get("filePath")
                    .and_then(|v| v.as_str())
                    .map(String::from);

                if let Some(messages) = file_result.get("messages").and_then(|v| v.as_array()) {
                    for msg in messages {
                        let line = msg.get("line").and_then(|v| v.as_u64()).map(|v| v as u32);
                        let column = msg.get("column").and_then(|v| v.as_u64()).map(|v| v as u32);
                        let message = msg
                            .get("message")
                            .and_then(|v| v.as_str())
                            .map(String::from)
                            .unwrap_or_default();
                        let severity = msg.get("severity").and_then(|v| v.as_u64()).map(|v| {
                            if v == 2 {
                                "error".to_string()
                            } else {
                                "warning".to_string()
                            }
                        });

                        errors.push(ErrorRecord {
                            file: file_path.clone(),
                            line,
                            column,
                            message,
                            severity,
                            test_name: None,
                            assertion_kind: None,
                            expected: None,
                            actual: None,
                        });
                    }
                }
            }

            if !errors.is_empty() {
                return errors;
            }
        }
    }

    let combined = format!("{stdout}{stderr}");
    let stylish_errors = parse_eslint_stylish_errors(&combined);
    if !stylish_errors.is_empty() {
        return stylish_errors;
    }

    let combined = combined.trim().to_string();
    if !combined.is_empty() {
        errors.push(ErrorRecord {
            file: None,
            line: None,
            column: None,
            message: cap_error_message(&combined),
            severity: Some("error".to_string()),
            test_name: None,
            assertion_kind: None,
            expected: None,
            actual: None,
        });
    }

    errors
}

fn parse_eslint_stylish_errors(output: &str) -> Vec<ErrorRecord> {
    let diagnostic_regex =
        Regex::new(r"^\s*(\d+):(\d+)\s+(error|warning)\s+(.+?)(?:\s{2,}([^\s].*?))?\s*$").unwrap();
    let mut current_file: Option<String> = None;
    let mut errors = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('/') || trimmed.starts_with("./") || trimmed.starts_with("../") {
            current_file = Some(trimmed.to_string());
            continue;
        }

        let Some(caps) = diagnostic_regex.captures(line) else {
            continue;
        };
        let severity = caps
            .get(3)
            .map(|m| m.as_str().to_string())
            .unwrap_or_else(|| "error".to_string());
        if severity != "error" {
            continue;
        }

        errors.push(ErrorRecord {
            file: current_file.clone(),
            line: caps.get(1).and_then(|m| m.as_str().parse::<u32>().ok()),
            column: caps.get(2).and_then(|m| m.as_str().parse::<u32>().ok()),
            message: caps
                .get(4)
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_default(),
            severity: Some(severity),
            test_name: None,
            assertion_kind: None,
            expected: None,
            actual: None,
        });
    }

    errors
}

/// Parse format errors from format check output.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P06
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-005
fn parse_format_errors(stdout: &str, stderr: &str) -> Vec<ErrorRecord> {
    let mut errors = Vec::new();
    let combined = format!("{stdout}{stderr}");

    for line in combined.lines() {
        let trimmed = line.trim();

        // Prettier --check outputs unformatted filenames
        // Example lines: "[warn] src/foo.ts" or just "src/foo.ts"
        if trimmed.starts_with("[warn]") {
            let file_path = trimmed
                .strip_prefix("[warn]")
                .map(|s| s.trim())
                .unwrap_or(trimmed);
            if !file_path.is_empty() && file_path.contains('.') {
                errors.push(ErrorRecord {
                    file: Some(file_path.to_string()),
                    line: None,
                    column: None,
                    message: "File is not formatted".to_string(),
                    severity: Some("warning".to_string()),
                    test_name: None,
                    assertion_kind: None,
                    expected: None,
                    actual: None,
                });
            }
        } else if trimmed.ends_with(".ts")
            || trimmed.ends_with(".tsx")
            || trimmed.ends_with(".js")
            || trimmed.ends_with(".jsx")
            || trimmed.ends_with(".json")
            || trimmed.ends_with(".md")
            || trimmed.ends_with(".css")
            || trimmed.ends_with(".scss")
            || trimmed.ends_with(".html")
        {
            // Likely a file path from prettier output
            errors.push(ErrorRecord {
                file: Some(trimmed.to_string()),
                line: None,
                column: None,
                message: "File is not formatted".to_string(),
                severity: Some("warning".to_string()),
                test_name: None,
                assertion_kind: None,
                expected: None,
                actual: None,
            });
        }
    }

    // Fallback: if no errors parsed but there was output, wrap raw output
    if errors.is_empty() && !combined.trim().is_empty() {
        errors.push(ErrorRecord {
            file: None,
            line: None,
            column: None,
            message: combined.trim().to_string(),
            severity: Some("error".to_string()),
            test_name: None,
            assertion_kind: None,
            expected: None,
            actual: None,
        });
    }

    errors
}
fn parse_diff_errors(stdout: &str, stderr: &str) -> Vec<ErrorRecord> {
    let combined = format!("{stdout}{stderr}").trim().to_string();
    vec![ErrorRecord {
        file: None,
        line: None,
        column: None,
        message: if combined.is_empty() {
            "No repository changes were produced".to_string()
        } else {
            combined
        },
        severity: Some("error".to_string()),
        test_name: None,
        assertion_kind: None,
        expected: None,
        actual: None,
    }]
}

/// Parse build errors from build output.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P06
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-005
fn parse_build_errors(stdout: &str, stderr: &str) -> Vec<ErrorRecord> {
    // Try to extract TypeScript-style errors first
    let errors = parse_typescript_errors(stdout, stderr);

    // Fallback: if no errors parsed but there was output, wrap raw output
    if errors.is_empty() {
        let combined = format!("{stdout}{stderr}").trim().to_string();
        if !combined.is_empty() {
            return vec![ErrorRecord {
                file: None,
                line: None,
                column: None,
                message: combined,
                severity: Some("error".to_string()),
                test_name: None,
                assertion_kind: None,
                expected: None,
                actual: None,
            }];
        }
    }

    errors
}

/// Build a summary string from check results.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P06
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-004
fn build_summary(checks: &[CheckResult]) -> String {
    let mut parts = Vec::new();

    for check in checks {
        let status = if check.passed {
            "pass".to_string()
        } else {
            format!("{} errors", check.errors.len())
        };
        parts.push(format!("{}: {}", check.check_type, status));
    }

    if parts.is_empty() {
        "No checks ran".to_string()
    } else {
        parts.join(", ")
    }
}
