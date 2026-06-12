//! llxprt agent step executor.
//!
//! Spawns the `llxprt` agent CLI to perform a step's work. The binary is
//! configurable so deployments can point at a non-`PATH` install:
//!
//! - Step parameter `binary_path` (highest precedence). Supports `{...}`
//!   interpolation, e.g. `"{work_dir}/bin/llxprt"`.
//! - Workflow variable `llxprt_binary_path` (fallback for all steps).
//! - Default `"llxprt"` (resolved from `PATH`).
//!
//! The same resolution order is shared with the preflight gate in
//! [`crate::adapters::llxprt`] so they never diverge. Spawn failures map to the
//! typed [`EngineError::LlxprtBinaryNotFound`] (missing binary) and runtime
//! failure modes set a `llxprt_failure_reason` context variable
//! (`timeout` / `idle_timeout` / `agent_failure` / `no_diff` / `process_error`)
//! so callers can discriminate the cause.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::engine::executor::{interpolate_string, StepContext, StepExecutor};
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;

#[derive(Debug, Clone, Copy)]
pub struct LlxprtExecutor;

impl StepExecutor for LlxprtExecutor {
    // Pre-existing llxprt process orchestration flow; split in a dedicated refactor stage.
    #[allow(clippy::cognitive_complexity, clippy::too_many_lines)]
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        std::fs::create_dir_all(context.work_dir()).map_err(|e| {
            EngineError::StepExecutionError {
                step_id: "llxprt".to_string(),
                message: format!("Failed to create work_dir: {e}"),
            }
        })?;

        let prompt = params
            .get("prompt")
            .and_then(serde_json::Value::as_str)
            .map_or_else(String::new, |template| {
                interpolate_string(template, context)
            });
        let profile = params
            .get("profile")
            .and_then(serde_json::Value::as_str)
            .map(|template| interpolate_string(template, context));
        let timeout = params
            .get("timeout_seconds")
            .and_then(serde_json::Value::as_u64)
            .map(Duration::from_secs)
            .unwrap_or_else(|| Duration::from_secs(900));
        let min_runtime_before_success = params
            .get("min_runtime_before_success_seconds")
            .and_then(serde_json::Value::as_u64)
            .map(Duration::from_secs)
            .unwrap_or_else(|| Duration::from_secs(120));
        let max_runtime_after_required_diff = params
            .get("max_runtime_after_required_diff_seconds")
            .and_then(serde_json::Value::as_u64)
            .map(Duration::from_secs);
        let idle_timeout = params
            .get("idle_timeout_seconds")
            .and_then(serde_json::Value::as_u64)
            .map(Duration::from_secs);
        let success_file = params
            .get("success_file")
            .and_then(serde_json::Value::as_str)
            .map(|template| interpolate_string(template, context));
        let stdout_file = params
            .get("stdout_file")
            .and_then(serde_json::Value::as_str)
            .map(|template| interpolate_string(template, context));
        let stderr_file = params
            .get("stderr_file")
            .and_then(serde_json::Value::as_str)
            .map(|template| interpolate_string(template, context));
        let success_on_diff = params
            .get("success_on_diff")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let required_changed_paths = string_array_param(params, "required_changed_paths", context);
        let required_changed_path_patterns =
            string_array_param(params, "required_changed_path_patterns", context);
        let early_success_on_diff = params
            .get("early_success_on_diff")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(success_on_diff);
        let continue_on_empty_diff = params
            .get("continue_on_empty_diff")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let success_on_existing_diff = params
            .get("success_on_existing_diff")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let initial_changed_paths = if success_on_existing_diff {
            Vec::new()
        } else {
            changed_paths(context).unwrap_or_default()
        };

        let initial_success_condition_met = !success_on_existing_diff
            && success_condition_met(
                context,
                success_file.as_deref(),
                success_on_diff,
                &required_changed_paths,
                &required_changed_path_patterns,
                &[],
            );

        if let Some(static_content) = params
            .get("static_content")
            .and_then(serde_json::Value::as_str)
        {
            let content = interpolate_string(static_content, context);
            if let Some(path_template) = success_file.as_deref() {
                write_success_file(context, path_template, &content)?;
                return Ok(StepOutcome::Success);
            }
            return Err(EngineError::StepExecutionError {
                step_id: "llxprt".to_string(),
                message: "static_content requires success_file".to_string(),
            });
        }

        if let Some(static_stdout) = params
            .get("static_stdout")
            .and_then(serde_json::Value::as_str)
        {
            let stdout = interpolate_string(static_stdout, context);
            if let Some(path_template) = stdout_file.as_deref() {
                write_artifact_file(context, path_template, &stdout)?;
            }
            context.set("stdout", &stdout);
            if let Some(outcome) = match_static_stdout_outcome(params, &stdout) {
                if outcome == StepOutcome::Success && (success_file.is_some() || success_on_diff) {
                    if initial_success_condition_met {
                        return Ok(StepOutcome::Fixable);
                    }
                    if !success_condition_met(
                        context,
                        success_file.as_deref(),
                        success_on_diff,
                        &required_changed_paths,
                        &required_changed_path_patterns,
                        &initial_changed_paths,
                    ) {
                        return Ok(StepOutcome::Fixable);
                    }
                }

                return Ok(outcome);
            }
            if success_file.is_some() || success_on_diff {
                if initial_success_condition_met {
                    return Ok(StepOutcome::Fixable);
                }
                if !success_condition_met(
                    context,
                    success_file.as_deref(),
                    success_on_diff,
                    &required_changed_paths,
                    &required_changed_path_patterns,
                    &initial_changed_paths,
                ) {
                    return Ok(StepOutcome::Fixable);
                }
            }

            return Ok(StepOutcome::Success);
        }

        let binary_template = params
            .get(crate::adapters::llxprt::BINARY_PATH_PARAM)
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string)
            .or_else(|| {
                context
                    .get(crate::adapters::llxprt::BINARY_PATH_VARIABLE)
                    .cloned()
            })
            .unwrap_or_else(|| crate::adapters::llxprt::DEFAULT_LLXPRT_BINARY.to_string());
        let binary = interpolate_string(&binary_template, context);

        let mut cmd = Command::new(&binary);
        cmd.arg("--set").arg("reasoning.includeInResponse=false");
        if let Some(profile) = profile.as_deref() {
            cmd.arg("--profile-load").arg(profile);
        }
        cmd.arg("--yolo").arg("-p").arg(&prompt);
        cmd.current_dir(context.work_dir());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.stdin(Stdio::null());

        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                context.set("llxprt_failure_reason", "process_error");
                EngineError::LlxprtBinaryNotFound {
                    path: binary.clone(),
                }
            } else {
                context.set("llxprt_failure_reason", "process_error");
                EngineError::StepExecutionError {
                    step_id: "llxprt".to_string(),
                    message: format!("Failed to spawn llxprt at `{binary}`: {e}"),
                }
            }
        })?;

        let stdout_buffer = Arc::new(Mutex::new(String::new()));
        let stderr_buffer = Arc::new(Mutex::new(String::new()));
        let stdout_reader = child.stdout.take().map(|mut stdout| {
            let buffer = Arc::clone(&stdout_buffer);
            thread::spawn(move || {
                read_stream_into_buffer(&mut stdout, &buffer);
            })
        });
        let stderr_reader = child.stderr.take().map(|mut stderr| {
            let buffer = Arc::clone(&stderr_buffer);
            thread::spawn(move || {
                read_stream_into_buffer(&mut stderr, &buffer);
            })
        });
        let mut stdout_snapshot_len = 0;
        let mut stderr_snapshot_len = 0;

        let start = Instant::now();
        let mut last_progress = Instant::now();
        let mut last_output_change = Instant::now();
        let mut required_diff_seen_at: Option<Instant> = None;
        let mut success_seen = false;
        let mut outcome_seen: Option<StepOutcome> = None;
        while start.elapsed() < timeout {
            if let Some(idle_timeout) = idle_timeout {
                let stdout_len = stdout_buffer.lock().map_or(0, |output| output.len());
                let stderr_len = stderr_buffer.lock().map_or(0, |output| output.len());
                if stdout_len != stdout_snapshot_len || stderr_len != stderr_snapshot_len {
                    last_output_change = Instant::now();
                } else if last_output_change.elapsed() >= idle_timeout {
                    break;
                }
            }

            if let Some(outcome) = match_stdout_outcome(params, &stdout_buffer) {
                outcome_seen = Some(outcome);
                break;
            }

            if child
                .try_wait()
                .map_err(|e| EngineError::StepExecutionError {
                    step_id: "llxprt".to_string(),
                    message: format!("Failed to poll llxprt: {e}"),
                })?
                .is_some()
            {
                break;
            }

            if !initial_success_condition_met
                && success_condition_met(
                    context,
                    success_file.as_deref(),
                    early_success_on_diff,
                    &required_changed_paths,
                    &required_changed_path_patterns,
                    &initial_changed_paths,
                )
            {
                if let Some(seen_at) = required_diff_seen_at {
                    if start.elapsed() >= min_runtime_before_success
                        || max_runtime_after_required_diff
                            .is_some_and(|max_runtime| seen_at.elapsed() >= max_runtime)
                    {
                        success_seen = true;
                        break;
                    }
                } else {
                    required_diff_seen_at = Some(Instant::now());
                    if start.elapsed() >= min_runtime_before_success
                        || max_runtime_after_required_diff
                            .is_some_and(|max_runtime| max_runtime.is_zero())
                    {
                        success_seen = true;
                        break;
                    }
                }
            } else {
                required_diff_seen_at = None;
            }

            if last_progress.elapsed() >= Duration::from_secs(30) {
                let elapsed = start.elapsed().as_secs();
                let stdout_len = stdout_buffer.lock().map_or(0, |output| output.len());
                let stderr_len = stderr_buffer.lock().map_or(0, |output| output.len());
                println!(
                    "[llxprt] running for {elapsed}s (stdout {stdout_len} bytes, stderr {stderr_len} bytes)"
                );
                if stdout_len != stdout_snapshot_len || stderr_len != stderr_snapshot_len {
                    if let Some(path_template) = stdout_file.as_deref() {
                        let stdout = stdout_buffer
                            .lock()
                            .map_or_else(|_| String::new(), |output| output.clone());
                        write_artifact_file(context, path_template, &stdout)?;
                    }
                    if let Some(path_template) = stderr_file.as_deref() {
                        let stderr = stderr_buffer
                            .lock()
                            .map_or_else(|_| String::new(), |output| output.clone());
                        write_artifact_file(context, path_template, &stderr)?;
                    }
                    stdout_snapshot_len = stdout_len;
                    stderr_snapshot_len = stderr_len;
                }
                last_progress = Instant::now();
            }

            thread::sleep(Duration::from_secs(2));
        }

        let timed_out = start.elapsed() >= timeout && !success_seen && outcome_seen.is_none();
        let idle_timed_out = idle_timeout
            .is_some_and(|timeout| last_output_change.elapsed() >= timeout)
            && !success_seen
            && outcome_seen.is_none();
        if success_seen || timed_out || idle_timed_out || outcome_seen.is_some() {
            terminate_process_tree(&mut child);
        }

        let exit_status = child.wait().map_err(|e| EngineError::StepExecutionError {
            step_id: "llxprt".to_string(),
            message: format!("Failed to wait for llxprt: {e}"),
        })?;
        if let Some(reader) = stdout_reader {
            let _ = reader.join();
        }
        if let Some(reader) = stderr_reader {
            let _ = reader.join();
        }

        let stdout = stdout_buffer
            .lock()
            .map_or_else(|_| String::new(), |output| output.clone());
        let stderr = stderr_buffer
            .lock()
            .map_or_else(|_| String::new(), |output| output.clone());
        if let Some(path_template) = stdout_file.as_deref() {
            write_artifact_file(context, path_template, &stdout)?;
        }
        if let Some(path_template) = stderr_file.as_deref() {
            write_artifact_file(context, path_template, &stderr)?;
        }
        if let Some(code) = exit_status.code() {
            context.set("exit_code", &code.to_string());
        }

        context.set("stdout", &stdout);
        context.set("stderr", &stderr);

        if let Some(outcome) = outcome_seen {
            context.set(
                "diagnostic",
                "llxprt stdout outcome marker seen before process exit",
            );
            if outcome == StepOutcome::Success && (success_file.is_some() || success_on_diff) {
                if initial_success_condition_met {
                    return Ok(StepOutcome::Fixable);
                }
                if !success_condition_met(
                    context,
                    success_file.as_deref(),
                    success_on_diff,
                    &required_changed_paths,
                    &required_changed_path_patterns,
                    &initial_changed_paths,
                ) {
                    return Ok(StepOutcome::Fixable);
                }
            }

            return Ok(outcome);
        }

        if success_seen {
            context.set(
                "diagnostic",
                "llxprt success condition met before process exit",
            );
            return Ok(StepOutcome::Success);
        }

        if timed_out || idle_timed_out {
            context.set("exit_code", "124");
            context.set(
                "llxprt_failure_reason",
                if idle_timed_out {
                    "idle_timeout"
                } else {
                    "timeout"
                },
            );
            let diagnostic = if idle_timed_out {
                idle_timeout.map_or_else(
                    || "llxprt timed out after stalled output".to_string(),
                    |timeout| {
                        format!(
                            "llxprt produced no new output for {} seconds",
                            timeout.as_secs()
                        )
                    },
                )
            } else {
                format!("llxprt timed out after {} seconds", timeout.as_secs())
            };
            context.set("diagnostic", &diagnostic);
            return Ok(StepOutcome::Fatal);
        }

        if !exit_status.success() {
            context.set("llxprt_failure_reason", "agent_failure");
            let diagnostic = exit_status.code().map_or_else(
                || "llxprt exited without an exit code".to_string(),
                |code| format!("llxprt exited with status {code}"),
            );
            context.set("diagnostic", &diagnostic);
            return Ok(
                match_exit_code_outcome(params, exit_status.code()).unwrap_or(StepOutcome::Fatal)
            );
        }

        if let Some(outcome) = match_static_stdout_outcome(params, &stdout) {
            if outcome == StepOutcome::Success && (success_file.is_some() || success_on_diff) {
                if initial_success_condition_met {
                    return Ok(StepOutcome::Fixable);
                }
                if !success_condition_met(
                    context,
                    success_file.as_deref(),
                    success_on_diff,
                    &required_changed_paths,
                    &required_changed_path_patterns,
                    &initial_changed_paths,
                ) {
                    return Ok(StepOutcome::Fixable);
                }
            }

            return Ok(outcome);
        }

        if !initial_success_condition_met
            && success_condition_met(
                context,
                success_file.as_deref(),
                success_on_diff,
                &required_changed_paths,
                &required_changed_path_patterns,
                &initial_changed_paths,
            )
        {
            return Ok(StepOutcome::Success);
        }

        if success_file.is_some() || success_on_diff {
            if continue_on_empty_diff && (!stdout.trim().is_empty() || !stderr.trim().is_empty()) {
                context.set(
                    "diagnostic",
                    "llxprt process exited after making no additional required changes",
                );
                return Ok(StepOutcome::Success);
            }
            context.set("llxprt_failure_reason", "no_diff");
            return Ok(StepOutcome::Fixable);
        }

        Ok(StepOutcome::Success)
    }
}

fn write_success_file(
    context: &StepContext,
    path_template: &str,
    content: &str,
) -> Result<(), EngineError> {
    write_artifact_file(context, path_template, content)
}

fn write_artifact_file(
    context: &StepContext,
    path_template: &str,
    content: &str,
) -> Result<(), EngineError> {
    let path = Path::new(path_template);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        context.work_dir().join(path)
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| EngineError::StepExecutionError {
            step_id: "llxprt".to_string(),
            message: format!("Failed to create artifact file parent: {e}"),
        })?;
    }

    std::fs::write(&path, content).map_err(|e| EngineError::StepExecutionError {
        step_id: "llxprt".to_string(),
        message: format!("Failed to write artifact file: {e}"),
    })
}

fn read_stream_into_buffer<R: Read>(reader: &mut R, buffer: &Arc<Mutex<String>>) {
    let mut bytes = [0_u8; 4096];
    loop {
        match reader.read(&mut bytes) {
            Ok(0) => break,
            Ok(n) => {
                if let Ok(mut output) = buffer.lock() {
                    output.push_str(&String::from_utf8_lossy(&bytes[..n]));
                }
            }
            Err(_) => break,
        }
    }
}

fn match_exit_code_outcome(
    params: &serde_json::Value,
    exit_code: Option<i32>,
) -> Option<StepOutcome> {
    let code = exit_code?.to_string();
    let outcome_name = params
        .get("exit_code_map")?
        .as_object()?
        .get(&code)?
        .as_str()?;
    Some(parse_outcome_name(outcome_name))
}

fn match_static_stdout_outcome(params: &serde_json::Value, stdout: &str) -> Option<StepOutcome> {
    let pattern_map = params.get("outcome_on_stdout")?.as_object()?;
    for (pattern, outcome_value) in pattern_map {
        if contains_outcome_marker_line(stdout, pattern) {
            return outcome_value.as_str().map(parse_outcome_name);
        }
    }
    None
}

fn match_stdout_outcome(
    params: &serde_json::Value,
    stdout_buffer: &Arc<Mutex<String>>,
) -> Option<StepOutcome> {
    let stdout = stdout_buffer.lock().ok()?;
    let pattern_map = params.get("outcome_on_stdout")?.as_object()?;
    for (pattern, outcome_value) in pattern_map {
        if contains_outcome_marker_line(&stdout, pattern) {
            return outcome_value.as_str().map(parse_outcome_name);
        }
    }
    None
}

fn contains_outcome_marker_line(stdout: &str, marker: &str) -> bool {
    stdout.lines().any(|line| line.trim() == marker)
}

fn success_condition_met(
    context: &StepContext,
    success_file: Option<&str>,
    success_on_diff: bool,
    required_changed_paths: &[String],
    required_changed_path_patterns: &[String],
    initial_changed_paths: &[String],
) -> bool {
    if let Some(path) = success_file {
        let path = std::path::Path::new(path);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            context.work_dir().join(path)
        };
        if path.metadata().is_ok_and(|m| m.len() > 0) {
            return true;
        }
    }

    if success_on_diff {
        return changed_paths(context).is_some_and(|paths| {
            diff_requirements_met(
                &new_changed_paths(&paths, initial_changed_paths),
                required_changed_paths,
                required_changed_path_patterns,
            )
        });
    }

    false
}

fn string_array_param(
    params: &serde_json::Value,
    name: &str,
    context: &StepContext,
) -> Vec<String> {
    params
        .get(name)
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(|template| interpolate_string(template, context))
                .collect()
        })
        .unwrap_or_default()
}

fn changed_paths(context: &StepContext) -> Option<Vec<String>> {
    let output = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(context.work_dir())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let paths = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_status_path)
        .collect::<Vec<_>>();
    Some(paths)
}

fn parse_status_path(line: &str) -> Option<String> {
    let path = line.get(3..)?.trim();
    if path.is_empty() {
        return None;
    }
    let path = path.split(" -> ").last().unwrap_or(path);
    Some(path.to_string())
}

fn new_changed_paths(paths: &[String], initial_changed_paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter(|path| !initial_changed_paths.contains(path))
        .cloned()
        .collect()
}

fn diff_requirements_met(
    paths: &[String],
    required_changed_paths: &[String],
    required_changed_path_patterns: &[String],
) -> bool {
    if paths.is_empty() {
        return false;
    }

    required_changed_paths
        .iter()
        .all(|required| paths.iter().any(|path| path == required))
        && required_changed_path_patterns
            .iter()
            .all(|pattern| paths.iter().any(|path| path.contains(pattern)))
}

fn terminate_process_tree(child: &mut std::process::Child) {
    let _ = child.kill();
    thread::sleep(Duration::from_millis(250));

    let _ = child.kill();
}

fn parse_outcome_name(name: &str) -> StepOutcome {
    match name {
        "success" => StepOutcome::Success,
        "fixable" => StepOutcome::Fixable,
        "fatal" => StepOutcome::Fatal,
        "retryable" => StepOutcome::Retryable,
        "abandon" => StepOutcome::Abandon,
        _ => StepOutcome::Fatal,
    }
}
