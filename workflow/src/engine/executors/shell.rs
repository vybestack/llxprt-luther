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

        // Interpolate command string using context
        let interpolated_command = interpolate_string(command_template, context);

        // --- Stdin setup (pseudocode lines 006-018) ---
        let mut stdin_data: Option<String> = None;

        // Check for stdin param (interpolated string)
        if let Some(stdin_template) = params.get("stdin").and_then(|v| v.as_str()) {
            stdin_data = Some(interpolate_string(stdin_template, context));
        }
        // Check for stdin_file param (read file contents)
        else if let Some(stdin_file) = params.get("stdin_file").and_then(|v| v.as_str()) {
            let file_path = context.work_dir().join(stdin_file);
            match std::fs::read_to_string(&file_path) {
                Ok(contents) => {
                    stdin_data = Some(contents);
                }
                Err(e) => {
                    context.set(
                        "diagnostic",
                        &format!("stdin_file not found or cannot read: {file_path:?}, error: {e}"),
                    );
                    return Ok(StepOutcome::Fatal);
                }
            }
        }

        // --- Spawn command (pseudocode lines 020-035) ---
        // Ensure work_dir exists before running command
        std::fs::create_dir_all(context.work_dir())
            .map_err(|e| EngineError::StepExecutionError {
                step_id: "shell".to_string(),
                message: format!("Failed to create work_dir: {e}"),
            })?;

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&interpolated_command);
        cmd.current_dir(context.work_dir());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // Set up stdin pipe if we have stdin data
        if stdin_data.is_some() {
            cmd.stdin(Stdio::piped());
        }

        // Spawn child process
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                return Err(EngineError::StepExecutionError {
                    step_id: "shell".to_string(),
                    message: format!("Failed to spawn command: {e}"),
                });
            }
        };

        // Write stdin data if present
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

        // Wait for output, enforcing an optional per-step timeout.
        let timeout = params
            .get("timeout_seconds")
            .and_then(serde_json::Value::as_u64)
            .map(Duration::from_secs);
        let output = match wait_with_optional_timeout(&mut child, timeout) {
            Ok(WaitResult::Completed(output)) => output,
            Ok(WaitResult::TimedOut { timeout }) => {
                context.set("exit_code", "124");
                context.set(
                    "diagnostic",
                    &format!("shell command timed out after {} seconds", timeout.as_secs()),
                );
                return Ok(StepOutcome::Fatal);
            }
            Err(e) => {
                return Err(EngineError::StepExecutionError {
                    step_id: "shell".to_string(),
                    message: format!("Failed to wait for command output: {e}"),
                });
            }
        };



        // Capture stdout and stderr into context (lines 037-046)
        let stdout_str = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr_str = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code();

        context.set("stdout", &stdout_str);
        context.set("stderr", &stderr_str);
        if let Some(code) = exit_code {
            context.set("exit_code", &code.to_string());
        }

        // --- Outcome determination (CRITICAL ORDER - per spec) ---
        // 1. If exit_code != 0: check exit_code_map first, else default Fixable
        // 2. If exit_code == 0 AND outcome_on_stdout: scan for patterns, return first match
        // 3. If exit_code == 0 AND output_format == "json": parse JSON, extract context_map
        // 4. Default: Success

        // Non-zero exit: check exit_code_map first (REQ-LF-SHELL-010)
        if exit_code != Some(0) {
            // Check exit_code_map for mapping
            if let Some(exit_code_map) = params.get("exit_code_map") {
                if let Some(map_obj) = exit_code_map.as_object() {
                    if let Some(code_str) = exit_code.and_then(|c| Some(c.to_string())) {
                        if let Some(outcome_value) = map_obj.get(&code_str) {
                            if let Some(outcome_name) = outcome_value.as_str() {
                                let outcome = parse_outcome_name(outcome_name);
                                return Ok(outcome);
                            }
                        }
                    }
                }
            }
            // Unmapped non-zero exit: Fixable (REQ-LF-SHELL-007)
            return Ok(StepOutcome::Fixable);
        }

        // Zero exit: check outcome_on_stdout first (REQ-LF-SHELL-005)
        if let Some(outcome_on_stdout) = params.get("outcome_on_stdout") {
            if let Some(pattern_map) = outcome_on_stdout.as_object() {
                for (pattern, outcome_value) in pattern_map {
                    if stdout_str.contains(pattern) {
                        if let Some(outcome_name) = outcome_value.as_str() {
                            let outcome = parse_outcome_name(outcome_name);
                            return Ok(outcome);
                        }
                    }
                }
            }
        }

        // Zero exit: check output_format == "json" for parsing (REQ-LF-SHELL-001)
        if let Some(format) = params.get("output_format").and_then(|v| v.as_str()) {
            if format == "json" {
                // Parse stdout as JSON
                let parsed_json: serde_json::Value = match serde_json::from_str(&stdout_str) {
                    Ok(json) => json,
                    Err(e) => {
                        context.set("json_parse_error", &e.to_string());
                        return Ok(StepOutcome::Fatal);
                    }
                };

                // Extract values via context_map
                if let Some(context_map) = params.get("context_map") {
                    if let Some(map_obj) = context_map.as_object() {
                        for (var_name, path_value) in map_obj {
                            if let Some(dot_path) = path_value.as_str() {
                                match extract_dot_path(&parsed_json, dot_path) {
                                    Some(value) => {
                                        let str_value = json_value_to_string(value);
                                        context.set(var_name, &str_value);
                                    }
                                    None => {
                                        // Collect top-level keys for error message
                                        let top_keys: Vec<String> = parsed_json
                                            .as_object()
                                            .map(|obj| obj.keys().cloned().collect())
                                            .unwrap_or_default();
                                        context.set(
                                            "json_path_error",
                                            &format!(
                                                "path '{dot_path}' not found, available keys: {:?}",
                                                top_keys
                                            ),
                                        );
                                        return Ok(StepOutcome::Fatal);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Default: Success (REQ-LF-SHELL-006)
        Ok(StepOutcome::Success)
    }
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
    let placeholder = Command::new("sh")
        .arg("-c")
        .arg("exit 0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(std::mem::replace(child, placeholder))
}

fn terminate_process_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pid = child.id().to_string();
        let _ = Command::new("pkill")
            .args(["-TERM", "-P", &pid])
            .status();
    }

    let _ = child.kill();
    thread::sleep(Duration::from_millis(250));

    #[cfg(unix)]
    {
        let pid = child.id().to_string();
        let _ = Command::new("pkill")
            .args(["-KILL", "-P", &pid])
            .status();
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
