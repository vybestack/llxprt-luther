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
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        std::fs::create_dir_all(context.work_dir()).map_err(|e| EngineError::StepExecutionError {
            step_id: "llxprt".to_string(),
            message: format!("Failed to create work_dir: {e}"),
        })?;

        let prompt = params
            .get("prompt")
            .and_then(serde_json::Value::as_str)
            .map_or_else(String::new, |template| interpolate_string(template, context));
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
        let success_file = params
            .get("success_file")
            .and_then(serde_json::Value::as_str)
            .map(|template| interpolate_string(template, context));
        let success_on_diff = params
            .get("success_on_diff")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let early_success_on_diff = params
            .get("early_success_on_diff")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(success_on_diff);


        if let Some(static_content) = params.get("static_content").and_then(serde_json::Value::as_str) {
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

        if let Some(static_stdout) = params.get("static_stdout").and_then(serde_json::Value::as_str) {
            let stdout = interpolate_string(static_stdout, context);
            context.set("stdout", &stdout);
            if let Some(outcome) = match_static_stdout_outcome(params, &stdout) {
                return Ok(outcome);
            }
            return Ok(StepOutcome::Success);
        }

        let mut cmd = Command::new("llxprt");
        cmd.arg("--set")
            .arg("reasoning.includeInResponse=false");
        if let Some(profile) = profile.as_deref() {
            cmd.arg("--profile-load").arg(profile);
        }
        cmd.arg("--yolo").arg("-p").arg(&prompt);
        cmd.current_dir(context.work_dir());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.stdin(Stdio::null());

        let mut child = cmd.spawn().map_err(|e| EngineError::StepExecutionError {
            step_id: "llxprt".to_string(),
            message: format!("Failed to spawn llxprt: {e}"),
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

        let start = Instant::now();
        let mut success_seen = false;
        let mut outcome_seen: Option<StepOutcome> = None;
        while start.elapsed() < timeout {
            if let Some(outcome) = match_stdout_outcome(params, &stdout_buffer) {
                outcome_seen = Some(outcome);
                break;
            }

            if child.try_wait().map_err(|e| EngineError::StepExecutionError {
                step_id: "llxprt".to_string(),
                message: format!("Failed to poll llxprt: {e}"),
            })?.is_some()
            {
                break;
            }

            if start.elapsed() >= min_runtime_before_success
                && success_condition_met(context, success_file.as_deref(), early_success_on_diff)
            {
                success_seen = true;
                break;
            }

            thread::sleep(Duration::from_secs(2));
        }

        let timed_out = start.elapsed() >= timeout && !success_seen && outcome_seen.is_none();
        if success_seen || timed_out || outcome_seen.is_some() {
            terminate_process_tree(&mut child);
        }

        let _ = child.wait();
        if let Some(reader) = stdout_reader {
            let _ = reader.join();
        }
        if let Some(reader) = stderr_reader {
            let _ = reader.join();
        }

        let stdout = stdout_buffer.lock().map_or_else(|_| String::new(), |output| output.clone());
        let stderr = stderr_buffer.lock().map_or_else(|_| String::new(), |output| output.clone());
        context.set("stdout", &stdout);
        context.set("stderr", &stderr);

        if let Some(outcome) = outcome_seen {
            context.set("diagnostic", "llxprt stdout outcome marker seen before process exit");
            return Ok(outcome);
        }

        if success_seen {
            context.set("diagnostic", "llxprt success condition met before process exit");
            return Ok(StepOutcome::Success);
        }

        if timed_out {
            context.set("exit_code", "124");
            context.set(
                "diagnostic",
                &format!("llxprt timed out after {} seconds", timeout.as_secs()),
            );
            return Ok(StepOutcome::Fatal);
        }

        if let Some(outcome_on_stdout) = params.get("outcome_on_stdout") {
            if let Some(pattern_map) = outcome_on_stdout.as_object() {
                for (pattern, outcome_value) in pattern_map {
                    if stdout.contains(pattern) {
                        if let Some(outcome_name) = outcome_value.as_str() {
                            return Ok(parse_outcome_name(outcome_name));
                        }
                    }
                }
            }
        }

        if success_condition_met(context, success_file.as_deref(), success_on_diff) {
            return Ok(StepOutcome::Success);
        }

        Ok(StepOutcome::Success)
    }
}

fn write_success_file(
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
            message: format!("Failed to create success_file parent: {e}"),
        })?;
    }

    std::fs::write(&path, content).map_err(|e| EngineError::StepExecutionError {
        step_id: "llxprt".to_string(),
        message: format!("Failed to write success_file: {e}"),
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

fn match_static_stdout_outcome(params: &serde_json::Value, stdout: &str) -> Option<StepOutcome> {
    let pattern_map = params.get("outcome_on_stdout")?.as_object()?;
    for (pattern, outcome_value) in pattern_map {
        if stdout.contains(pattern) {
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
        if stdout.contains(pattern) {
            return outcome_value.as_str().map(parse_outcome_name);
        }
    }
    None
}

fn success_condition_met(context: &StepContext, success_file: Option<&str>, success_on_diff: bool) -> bool {
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
        return Command::new("git")
            .args(["diff", "--quiet", "--exit-code"])
            .current_dir(context.work_dir())
            .status()
            .is_ok_and(|status| !status.success());
    }

    false
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
