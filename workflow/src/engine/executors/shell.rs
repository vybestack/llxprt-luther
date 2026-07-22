/// @plan:PLAN-20260408-STEP-EXEC.P03
/// @plan:PLAN-20260408-STEP-EXEC.P05
/// @plan:PLAN-20260408-LLXPRT-FIRST.P03
/// @plan:PLAN-20260408-LLXPRT-FIRST.P05
/// Shell executor - executes shell commands with enhanced features:
/// - JSON output parsing with dot-path extraction
/// - Stdin piping from string or file
/// - Outcome pattern matching on stdout
/// - Exit code mapping
#[allow(unused_imports)]
use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::engine::executor::{interpolate_string, StepContext, StepExecutor};
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;

/// Shell executor that runs shell commands.
#[derive(Debug, Clone, Copy)]
pub struct ShellExecutor;

impl StepExecutor for ShellExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        // Extract "command" from params JSON
        let command_template = params
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EngineError::StepExecutionError {
                step_id: "shell".to_string(),
                message: "Missing 'command' parameter for shell executor".to_string(),
            })?;

        // Bind every dynamic value as a positional shell parameter. Shell parsing
        // sees only the static template and parameter expansions; metacharacters
        // in GitHub/config values are never reparsed as shell syntax.
        let (interpolated_command, shell_args) = bind_shell_template(command_template, context);

        // --- Stdin setup (pseudocode lines 006-018) ---
        let stdin_data = match resolve_shell_stdin(params, context) {
            StdinResolution::Data(data) => data,
            StdinResolution::Fatal => return Ok(StepOutcome::Fatal),
        };

        // --- Spawn + run + capture (pseudocode lines 020-046) ---
        let Some(output) = spawn_and_capture(
            params,
            context,
            &interpolated_command,
            &shell_args,
            stdin_data,
        )?
        else {
            // Timed out: the diagnostic/exit_code were recorded by the helper.
            return Ok(StepOutcome::Fatal);
        };

        // Capture stdout/stderr/exit_code into context (lines 037-046).
        let stdout_str = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr_str = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code();
        context.set("stdout", &stdout_str);
        context.set("stderr", &stderr_str);
        if let Some(code) = exit_code {
            context.set("exit_code", &code.to_string());
        }

        // --- Outcome determination (CRITICAL ORDER - per spec) ---
        resolve_shell_outcome(params, context, exit_code, &stdout_str)
    }
}

/// Spawn the shell child, pipe stdin, wait with an optional timeout, and return
/// the captured output. Returns `Ok(None)` on timeout after recording the
/// diagnostic/exit_code on `context`, so the caller returns `Fatal` without
/// further outcome evaluation.
///
/// Issue 158 finding 6: when a WorkspaceAuthorization exists (produced by the
/// `workspace_ownership_verify` step), the workspace was already verified and
/// must NOT be created or mutated via `create_dir_all` before the descriptor is
/// opened and validated. A TOCTOU swap or a missing path must never be silently
/// created; the descriptor open fails closed instead. Creation only happens when
/// no authorization is present (legacy/test compatibility for non-daemon
/// workspaces).
fn bind_shell_template(template: &str, context: &StepContext) -> (String, Vec<String>) {
    let mut command = String::with_capacity(template.len());
    let mut args = Vec::new();
    let mut rest = template;
    let mut single_quoted = false;
    while let Some(open) = rest.find('{') {
        let prefix = &rest[..open];
        command.push_str(prefix);
        single_quoted = update_single_quote_state(prefix, single_quoted);
        let token = &rest[open + 1..];
        let Some(close) = token.find('}') else {
            command.push_str(&rest[open..]);
            return (command, args);
        };
        let key = &token[..close];
        if let Some(value) = context.get(key) {
            args.push(value.clone());
            let parameter = format!("${{{}}}", args.len());
            if single_quoted {
                command.push_str("'\"");
                command.push_str(&parameter);
                command.push_str("\"'");
            } else {
                command.push_str(&parameter);
            }
        } else {
            command.push('{');
            command.push_str(key);
            command.push('}');
        }
        rest = &token[close + 1..];
    }
    command.push_str(rest);
    (command, args)
}

fn update_single_quote_state(text: &str, mut single_quoted: bool) -> bool {
    let mut escaped = false;
    for byte in text.bytes() {
        if byte == b'\\' && !single_quoted {
            escaped = !escaped;
            continue;
        }
        if byte == b'\'' && !escaped {
            single_quoted = !single_quoted;
        }
        escaped = false;
    }
    single_quoted
}
/// Issue 158 finding 1 + inode authorization: the child's working directory is
/// anchored to a verified workspace descriptor via `fchdir` in `pre_exec`. When
/// an authorization is present, the opened descriptor must match it exactly.
/// There is no path fallback: a TOCTOU swap of the workspace path between the
/// verify step and this shell step cannot redirect the shell.
fn spawn_and_capture(
    params: &serde_json::Value,
    context: &mut StepContext,
    interpolated_command: &str,
    shell_args: &[String],
    stdin_data: Option<String>,
) -> Result<Option<Output>, EngineError> {
    let authorization = context.workspace_authorization().copied();
    if authorization.is_none() {
        std::fs::create_dir_all(context.work_dir()).map_err(|e| {
            EngineError::StepExecutionError {
                step_id: "shell".to_string(),
                message: format!("Failed to create work_dir: {e}"),
            }
        })?;
    }

    let work_dir = context.work_dir().to_path_buf();
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(interpolated_command)
        .arg("luther-shell")
        .args(shell_args);
    let _child_cwd_fd =
        configure_command_cwd(&mut cmd, &work_dir, context.run_id(), authorization)?;
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    if stdin_data.is_some() {
        cmd.stdin(Stdio::piped());
    }

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            return Err(EngineError::StepExecutionError {
                step_id: "shell".to_string(),
                message: format!("Failed to spawn command: {e}"),
            });
        }
    };

    if let Some(data) = stdin_data {
        if let Some(mut stdin) = child.stdin.take() {
            if let Err(e) = stdin.write_all(data.as_bytes()) {
                return Err(EngineError::StepExecutionError {
                    step_id: "shell".to_string(),
                    message: format!("Failed to write to stdin: {e}"),
                });
            }
            // stdin is dropped here, which closes the pipe
        }
    }

    let timeout = params
        .get("timeout_seconds")
        .and_then(serde_json::Value::as_u64)
        .map(Duration::from_secs);
    match wait_with_optional_timeout(&mut child, timeout) {
        Ok(WaitResult::Completed(output)) => Ok(Some(output)),
        Ok(WaitResult::TimedOut { timeout }) => {
            context.set("exit_code", "124");
            context.set(
                "diagnostic",
                &format!(
                    "shell command timed out after {} seconds",
                    timeout.as_secs()
                ),
            );
            Ok(None)
        }
        Err(e) => Err(EngineError::StepExecutionError {
            step_id: "shell".to_string(),
            message: format!("Failed to wait for command output: {e}"),
        }),
    }
}

/// Resolve the step outcome from a completed (non-timed-out) shell run,
/// following the spec's critical evaluation order:
/// 1. Non-zero exit: consult `exit_code_map`, else default `Fixable`.
/// 2. Zero exit + `outcome_on_stdout`: first matching pattern wins.
/// 3. Zero exit + `output_format == "json"`: parse and extract `context_map`.
/// 4. Default: `Success`.
fn resolve_shell_outcome(
    params: &serde_json::Value,
    context: &mut StepContext,
    exit_code: Option<i32>,
    stdout_str: &str,
) -> Result<StepOutcome, EngineError> {
    if exit_code != Some(0) {
        return Ok(mapped_nonzero_outcome(params, exit_code));
    }
    if let Some(outcome) = match_outcome_on_stdout(params, stdout_str) {
        return Ok(outcome);
    }
    if let Some(outcome) = extract_json_context(params, context, stdout_str) {
        return Ok(outcome);
    }
    Ok(StepOutcome::Success)
}

/// Map a non-zero exit code via `exit_code_map`, defaulting to `Fixable`
/// (REQ-LF-SHELL-007/010).
fn mapped_nonzero_outcome(params: &serde_json::Value, exit_code: Option<i32>) -> StepOutcome {
    if let Some(outcome_name) = exit_code.and_then(|c| {
        params
            .get("exit_code_map")
            .and_then(|m| m.as_object())
            .and_then(|map| map.get(&c.to_string()))
            .and_then(|v| v.as_str())
    }) {
        return parse_outcome_name(outcome_name);
    }
    StepOutcome::Fixable
}

/// First `outcome_on_stdout` pattern contained in `stdout` wins
/// (REQ-LF-SHELL-005).
fn match_outcome_on_stdout(params: &serde_json::Value, stdout_str: &str) -> Option<StepOutcome> {
    params
        .get("outcome_on_stdout")
        .and_then(|m| m.as_object())?
        .iter()
        .find_map(|(pattern, outcome_value)| {
            stdout_str.contains(pattern).then(|| {
                outcome_value
                    .as_str()
                    .map(parse_outcome_name)
                    .unwrap_or(StepOutcome::Success)
            })
        })
}

/// Parse stdout as JSON and extract the `context_map` dot-paths
/// (REQ-LF-SHELL-001). Returns `Some(Fatal)` on a parse or extraction failure,
/// `None` when JSON output was not requested.
fn extract_json_context(
    params: &serde_json::Value,
    context: &mut StepContext,
    stdout_str: &str,
) -> Option<StepOutcome> {
    let format = params.get("output_format").and_then(|v| v.as_str())?;
    if format != "json" {
        return None;
    }
    let parsed_json: serde_json::Value = match serde_json::from_str(stdout_str) {
        Ok(json) => json,
        Err(e) => {
            context.set("json_parse_error", &e.to_string());
            return Some(StepOutcome::Fatal);
        }
    };
    let context_map = params.get("context_map").and_then(|v| v.as_object());
    if let Some(map_obj) = context_map {
        for (var_name, path_value) in map_obj {
            if let Some(dot_path) = path_value.as_str() {
                match extract_dot_path(&parsed_json, dot_path) {
                    Some(value) => {
                        context.set(var_name, &json_value_to_string(value));
                    }
                    None => {
                        let top_keys: Vec<String> = parsed_json
                            .as_object()
                            .map(|obj| obj.keys().cloned().collect())
                            .unwrap_or_default();
                        context.set(
                            "json_path_error",
                            &format!("path '{dot_path}' not found, available keys: {top_keys:?}"),
                        );
                        return Some(StepOutcome::Fatal);
                    }
                }
            }
        }
    }
    Some(StepOutcome::Success)
}

enum WaitResult {
    Completed(Output),
    TimedOut { timeout: Duration },
}

fn wait_with_optional_timeout(
    child: &mut std::process::Child,
    timeout: Option<Duration>,
) -> std::io::Result<WaitResult> {
    let Some(timeout) = timeout else {
        let owned_child = take_child(child)?;
        return owned_child.wait_with_output().map(WaitResult::Completed);
    };

    let start = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            let owned_child = take_child(child)?;
            return owned_child.wait_with_output().map(WaitResult::Completed);
        }

        if start.elapsed() >= timeout {
            terminate_process_tree(child);
            return Ok(WaitResult::TimedOut { timeout });
        }

        thread::sleep(Duration::from_millis(100));
    }
}

fn take_child(child: &mut std::process::Child) -> std::io::Result<std::process::Child> {
    let replacement_child = Command::new("sh")
        .arg("-c")
        .arg("exit 0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(std::mem::replace(child, replacement_child))
}

fn terminate_process_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pid = child.id().to_string();
        let _ = Command::new("pkill").args(["-TERM", "-P", &pid]).status();
    }

    let _ = child.kill();
    thread::sleep(Duration::from_millis(250));

    #[cfg(unix)]
    {
        let pid = child.id().to_string();
        let _ = Command::new("pkill").args(["-KILL", "-P", &pid]).status();
    }

    let _ = child.kill();
    let _ = child.wait();
}

/// Extract a value from a JSON object using dot-path notation.
/// Pseudocode lines 088-097
/// @plan:PLAN-20260408-LLXPRT-FIRST.P03
/// @plan:PLAN-20260408-LLXPRT-FIRST.P05
/// @requirement:REQ-LF-SHELL-001,REQ-LF-SHELL-009
fn extract_dot_path<'a>(json: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = json;

    for part in parts {
        // Skip empty parts (from leading dots)
        if part.is_empty() {
            continue;
        }

        current = current.get(part)?;
    }

    Some(current)
}

/// Convert a JSON value to a string representation.
/// Pseudocode lines 099-107
/// @plan:PLAN-20260408-LLXPRT-FIRST.P03
/// @plan:PLAN-20260408-LLXPRT-FIRST.P05
/// @requirement:REQ-LF-SHELL-001
fn json_value_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            serde_json::to_string(value).unwrap_or_default()
        }
        serde_json::Value::Null => String::new(),
    }
}

/// Parse an outcome name string into a StepOutcome variant.
/// Pseudocode lines 109-118
/// @plan:PLAN-20260408-LLXPRT-FIRST.P03
/// @plan:PLAN-20260408-LLXPRT-FIRST.P05
/// @requirement:REQ-LF-SHELL-005,REQ-LF-SHELL-010
fn parse_outcome_name(name: &str) -> StepOutcome {
    match name.to_lowercase().as_str() {
        "success" => StepOutcome::Success,
        "fixable" => StepOutcome::Fixable,
        "fatal" => StepOutcome::Fatal,
        "retryable" => StepOutcome::Retryable,
        "abandon" => StepOutcome::Abandon,
        _ => StepOutcome::Success, // unknown name defaults to success
    }
}

/// Configure the child's working directory via an anchored, verified
/// workspace descriptor and `fchdir` in `pre_exec`. There is no path-based
/// `current_dir` fallback: a workspace that cannot be anchored (symlink root,
/// missing directory) fails closed.
///
/// When `authorization` is `Some` (produced by the `workspace_ownership_verify`
/// step), the opened workspace descriptor must match it exactly,
/// descriptor-relative. A TOCTOU swap of the workspace path between the verify
/// step and this shell step produces a different inode and fails closed.
fn configure_command_cwd(
    cmd: &mut Command,
    work_dir: &std::path::Path,
    _run_id: &str,
    authorization: Option<crate::engine::workspace_ownership::WorkspaceAuthorization>,
) -> Result<Option<std::os::fd::OwnedFd>, EngineError> {
    use crate::engine::workspace_ownership::{configure_fchdir_pre_exec, WorkspaceAnchor};

    let anchor =
        WorkspaceAnchor::open(work_dir).map_err(|error| EngineError::StepExecutionError {
            step_id: "shell".to_string(),
            message: format!("Failed to anchor work_dir: {error}"),
        })?;
    // Issue 158 inode authorization: require the opened descriptor to match
    // the exact authorized identity captured by the verify step. There is no
    // path fallback and no silent acceptance: a mismatch fails closed so a
    // TOCTOU swap of the workspace path between the verify step and this shell
    // step cannot redirect the shell.
    if let Some(ref expected) = authorization {
        verify_descriptor_matches_authorization(anchor.as_fd(), expected)?;
    }
    let child_fd = anchor
        .prepare_child_fd()
        .map_err(|error| EngineError::StepExecutionError {
            step_id: "shell".to_string(),
            message: format!("Failed to prepare anchored work_dir: {error}"),
        })?;
    configure_fchdir_pre_exec(cmd, &child_fd).map_err(|error| EngineError::StepExecutionError {
        step_id: "shell".to_string(),
        message: format!("Failed to configure anchored work_dir: {error}"),
    })?;
    Ok(Some(child_fd))
}

/// Require an open workspace descriptor to match `expected` exactly,
/// descriptor-relative (no path re-resolution). Returns an `EngineError` on
/// mismatch or inspection failure so a TOCTOU swap of the workspace path
/// between the verify step and the shell step cannot redirect the shell.
fn verify_descriptor_matches_authorization(
    fd: std::os::fd::BorrowedFd<'_>,
    expected: &crate::engine::workspace_ownership::WorkspaceAuthorization,
) -> Result<(), EngineError> {
    let matches =
        crate::engine::workspace_ownership::descriptor_matches_authorization(fd, expected)
            .map_err(|error| EngineError::StepExecutionError {
                step_id: "shell".to_string(),
                message: format!("Failed to verify workspace authorization: {error}"),
            })?;
    if !matches {
        return Err(EngineError::StepExecutionError {
            step_id: "shell".to_string(),
            message: "workspace identity does not match the authorization from workspace_ownership_verify".to_string(),
        });
    }
    Ok(())
}

/// Result of resolving shell stdin from params.
enum StdinResolution {
    /// Stdin data is present (or absent as `None`).
    Data(Option<String>),
    /// A `stdin_file` could not be read; the step must fail fatally.
    Fatal,
}

/// Resolve optional stdin data from `stdin` (interpolated string) or
/// `stdin_file` (file path relative to work_dir) params.
fn resolve_shell_stdin(params: &serde_json::Value, context: &mut StepContext) -> StdinResolution {
    if let Some(stdin_template) = params.get("stdin").and_then(|v| v.as_str()) {
        return StdinResolution::Data(Some(interpolate_string(stdin_template, context)));
    }
    if let Some(stdin_file) = params.get("stdin_file").and_then(|v| v.as_str()) {
        let relative = std::path::Path::new(stdin_file);
        if relative.is_absolute()
            || relative.components().any(|component| {
                !matches!(
                    component,
                    std::path::Component::Normal(_) | std::path::Component::CurDir
                )
            })
        {
            context.set(
                "diagnostic",
                "stdin_file must be a relative path without traversal",
            );
            return StdinResolution::Fatal;
        }
        let file_path = context.work_dir().join(relative);
        return match std::fs::read_to_string(&file_path) {
            Ok(contents) => StdinResolution::Data(Some(contents)),
            Err(e) => {
                context.set(
                    "diagnostic",
                    &format!("stdin_file not found or cannot read: {file_path:?}, error: {e}"),
                );
                StdinResolution::Fatal
            }
        };
    }
    StdinResolution::Data(None)
}
