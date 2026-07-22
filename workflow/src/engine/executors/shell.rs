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
use std::io::Read;
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

    // Drain stdout/stderr on dedicated reader threads BEFORE writing stdin
    // and waiting for the child. Reading only after the process exits can
    // deadlock if either pipe fills while the child is still running: the
    // child blocks writing to a full stdout/stderr pipe while it is still
    // reading stdin, and our `stdin.write_all` blocks because the child never
    // finishes consuming stdin. Spawning the readers first keeps the pipes
    // drained while stdin is written and while the child runs to completion.
    // This mirrors the concurrent-draining pattern used by the feedback
    // evaluator executor.
    let stdout_reader = child.stdout.take().map(spawn_pipe_reader);
    let stderr_reader = child.stderr.take().map(spawn_pipe_reader);

    write_child_stdin(&mut child, stdin_data, &stdout_reader, &stderr_reader)?;

    let timeout = params
        .get("timeout_seconds")
        .and_then(serde_json::Value::as_u64)
        .map(Duration::from_secs);
    finish_child_capture(&mut child, timeout, context, stdout_reader, stderr_reader)
}

fn write_child_stdin(
    child: &mut std::process::Child,
    stdin_data: Option<String>,
    stdout_reader: &Option<thread::JoinHandle<std::io::Result<Vec<u8>>>>,
    stderr_reader: &Option<thread::JoinHandle<std::io::Result<Vec<u8>>>>,
) -> Result<(), EngineError> {
    let Some(data) = stdin_data else {
        return Ok(());
    };
    let Some(mut stdin) = child.stdin.take() else {
        return Ok(());
    };
    stdin
        .write_all(data.as_bytes())
        .map_err(|error| EngineError::StepExecutionError {
            step_id: "shell".to_string(),
            message: format!(
                "Failed to write to stdin: {error} (stdout reader active: {}, stderr reader active: {})",
                stdout_reader.is_some(),
                stderr_reader.is_some()
            ),
        })
}

fn finish_child_capture(
    child: &mut std::process::Child,
    timeout: Option<Duration>,
    context: &mut StepContext,
    stdout_reader: Option<thread::JoinHandle<std::io::Result<Vec<u8>>>>,
    stderr_reader: Option<thread::JoinHandle<std::io::Result<Vec<u8>>>>,
) -> Result<Option<Output>, EngineError> {
    match wait_with_optional_timeout(child, timeout) {
        Ok(WaitResult::Completed(status)) => Ok(Some(Output {
            status,
            stdout: join_pipe_reader(stdout_reader),
            stderr: join_pipe_reader(stderr_reader),
        })),
        Ok(WaitResult::TimedOut { timeout }) => {
            let _ = join_pipe_reader(stdout_reader);
            let _ = join_pipe_reader(stderr_reader);
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
        Err(error) => {
            let _ = join_pipe_reader(stdout_reader);
            let _ = join_pipe_reader(stderr_reader);
            Err(EngineError::StepExecutionError {
                step_id: "shell".to_string(),
                message: format!("Failed to wait for command output: {error}"),
            })
        }
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

/// Result of waiting for the shell child: either it completed (with the exit
/// status; the drained output is joined by the caller) or it timed out.
enum WaitResult {
    Completed(std::process::ExitStatus),
    TimedOut { timeout: Duration },
}

/// Poll the child until it exits or `timeout` elapses. Output pipes must be
/// drained concurrently (by the caller's reader threads) so this function only
/// waits for the process status; it never calls `wait_with_output`, which would
/// re-read already-taken pipes and deadlock.
fn wait_with_optional_timeout(
    child: &mut std::process::Child,
    timeout: Option<Duration>,
) -> std::io::Result<WaitResult> {
    let Some(timeout) = timeout else {
        let status = child.wait()?;
        return Ok(WaitResult::Completed(status));
    };

    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(WaitResult::Completed(status));
        }

        if start.elapsed() >= timeout {
            terminate_process_tree(child);
            return Ok(WaitResult::TimedOut { timeout });
        }

        thread::sleep(Duration::from_millis(100));
    }
}

/// Spawn a thread that reads a child pipe to completion. The pipe is taken
/// from the child before spawning so the reader observes end-of-file as soon
/// as the last write end is closed by the child (and its descendants).
fn spawn_pipe_reader(
    mut pipe: impl Read + Send + 'static,
) -> thread::JoinHandle<std::io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut buffer = Vec::new();
        pipe.read_to_end(&mut buffer)?;
        Ok(buffer)
    })
}

/// Join a pipe-reader thread and return the drained bytes. A `None` handle
/// means the pipe was not piped; an empty buffer is returned in that case.
fn join_pipe_reader(reader: Option<thread::JoinHandle<std::io::Result<Vec<u8>>>>) -> Vec<u8> {
    match reader {
        Some(handle) => match handle.join() {
            Ok(Ok(bytes)) => bytes,
            // A reader failure or panic is not fatal to the step outcome; the
            // caller still receives whatever (possibly empty) bytes we have.
            Ok(Err(_)) | Err(_) => Vec::new(),
        },
        None => Vec::new(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::executor::{StepContext, StepExecutor};
    use crate::engine::transition::StepOutcome;
    use std::time::Instant;
    use tempfile::tempdir;

    /// Regression: piping a large stdin payload while the child writes a large
    /// stdout/stderr payload must not deadlock. Before the concurrent-drain
    /// fix, the reader threads were spawned only after stdin was written, so a
    /// child that filled its stdout pipe while still reading stdin blocked our
    /// `write_all`, and the child blocked on a full stdout pipe it could not
    /// drain because it had not finished consuming stdin. This test exercises
    /// that path with payloads exceeding typical pipe capacities (64 KiB) and
    /// enforces a generous-but-finite timeout so a regression deadlocks the
    /// test instead of silently passing.
    #[test]
    fn large_stdin_and_stdout_do_not_deadlock() {
        let work_dir = tempdir().expect("create temp work dir");
        let mut context = StepContext::new(
            work_dir.path().to_path_buf(),
            "deadlock-regression".to_string(),
        );
        // Write enough bytes to both directions to exceed a typical pipe
        // buffer (64 KiB) on each stream independently.
        let stdin_bytes = "x".repeat(256 * 1024);
        let params = serde_json::json!({
            "command": "cat; dd if=/dev/zero bs=262144 count=1 2>/dev/null | tr '\\000' A 1>&2; dd if=/dev/zero bs=262144 count=1 2>/dev/null | tr '\\000' B",
            "stdin": stdin_bytes,
            "timeout_seconds": 60u64,
        });

        let started = Instant::now();
        let outcome = ShellExecutor.execute(&mut context, &params);
        let elapsed = started.elapsed();

        let outcome = outcome.expect("shell execution should not error");
        assert!(
            matches!(outcome, StepOutcome::Success),
            "expected Success, got {outcome:?}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(60),
            "execution should not deadlock; elapsed {elapsed:?}"
        );

        let stdout = context
            .get("stdout")
            .map(String::as_str)
            .unwrap_or_default();
        let stderr = context
            .get("stderr")
            .map(String::as_str)
            .unwrap_or_default();
        // `cat` writes the 256 KiB stdin to stdout, then `printf 'B...'` adds
        // another 256 KiB of 'B' bytes, for a total of 512 KiB.
        assert_eq!(stdout.len(), 512 * 1024, "stdout should be fully drained");
        let (cat_part, b_part) = stdout.split_at(256 * 1024);
        assert!(
            cat_part.chars().all(|c| c == 'x'),
            "first half of stdout should be the echoed stdin"
        );
        assert!(
            b_part.chars().all(|c| c == 'B'),
            "second half of stdout should be 'B' bytes"
        );
        assert_eq!(stderr.len(), 256 * 1024, "stderr should be fully drained");
        assert!(
            stderr.chars().all(|c| c == 'A'),
            "stderr should contain only 'A' bytes"
        );
    }
}
