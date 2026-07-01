use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::adapters::github::parse_token_scopes;
use crate::adapters::workflow_auth_preflight::{
    artifact_path, build_report, classify_remote_url, detect_workflow_paths,
    extract_workflow_paths_from_text, parse_porcelain_paths, DetectedWorkflowPath,
    WorkflowAuthOutcome, WorkflowAuthPreflightConfig,
};
use crate::engine::executor::{interpolate_string, StepContext, StepExecutor};
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;

#[derive(Debug, Clone, Copy)]
pub struct WorkflowAuthPreflightExecutor;

impl StepExecutor for WorkflowAuthPreflightExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        params: &Value,
    ) -> Result<StepOutcome, EngineError> {
        let config = config_from_params(params)?;
        let artifact_dir = resolve_artifact_dir(context, params);
        let report_path = artifact_path(
            &artifact_dir,
            params
                .get("artifact_path")
                .and_then(Value::as_str)
                .map(|path| interpolate_string(path, context))
                .as_deref(),
        );
        let detected = detect_paths(
            context,
            params,
            &artifact_dir,
            &config.workflow_path_patterns,
        )?;
        let auth_method = if detected.is_empty() {
            "not_required".to_string()
        } else {
            resolve_auth_method(context.work_dir(), params).unwrap_or_else(|err| {
                tracing::warn!(
                    error = %err,
                    "workflow auth method discovery failed; emitting fatal preflight report"
                );
                "unknown".to_string()
            })
        };
        let observed_scopes = if auth_method == "not_required" {
            Vec::new()
        } else {
            observed_scopes_for_auth_method(&auth_method, context.work_dir(), params)
                .unwrap_or_else(|err| {
                    tracing::warn!(
                        error = %err,
                        "workflow auth scope discovery failed; emitting fatal preflight report"
                    );
                    Vec::new()
                })
        };
        let report = build_report(auth_method, config, detected, observed_scopes);
        write_report(&report_path, &report)?;
        context.set(
            "workflow_auth_preflight_artifact",
            &report_path.to_string_lossy(),
        );
        if report.outcome == WorkflowAuthOutcome::Fatal {
            context.set("workflow_auth_preflight_blocked", "true");
            return Ok(StepOutcome::Fatal);
        }
        Ok(StepOutcome::Success)
    }
}

fn config_from_params(params: &Value) -> Result<WorkflowAuthPreflightConfig, EngineError> {
    serde_json::from_value(params.clone()).map_err(|err| EngineError::StepExecutionError {
        step_id: "workflow_auth_preflight".to_string(),
        message: format!("invalid workflow auth preflight parameters: {err}"),
    })
}

fn resolve_artifact_dir(context: &StepContext, params: &Value) -> PathBuf {
    params
        .get("artifact_dir")
        .or_else(|| params.get("artifact_root"))
        .and_then(Value::as_str)
        .map(|path| interpolate_string(path, context))
        .or_else(|| {
            context
                .get("artifact_dir")
                .map(|path| interpolate_string(path, context))
        })
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| context.work_dir().join("artifacts"))
}

fn detect_paths(
    context: &StepContext,
    params: &Value,
    artifact_dir: &Path,
    patterns: &[String],
) -> Result<Vec<DetectedWorkflowPath>, EngineError> {
    let mut detected = detect_artifact_paths(params, artifact_dir, patterns)?;
    if params
        .get("include_git_status")
        .and_then(Value::as_bool)
        .unwrap_or(true)
    {
        let status_paths = git_status_paths(context.work_dir())?;
        detected.extend(detect_workflow_paths(status_paths, "git_status", patterns));
    }
    detected.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.source.cmp(&right.source))
    });
    detected.dedup_by(|left, right| left.path == right.path);
    Ok(detected)
}

fn detect_artifact_paths(
    params: &Value,
    artifact_dir: &Path,
    patterns: &[String],
) -> Result<Vec<DetectedWorkflowPath>, EngineError> {
    let files = params
        .get("text_artifacts")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_else(|| vec!["issue.md", "comments.md", "plan.md"]);
    let mut detected = Vec::new();
    for file in files {
        if file.contains('/') || file.contains('\\') || file.contains("..") {
            return Err(step_error(format!("invalid text artifact name: {file}")));
        }
        let path = artifact_dir.join(file);
        match fs::read_to_string(&path) {
            Ok(text) => detected.extend(extract_workflow_paths_from_text(&text, file, patterns)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(step_error(format!(
                    "failed to read text artifact {}: {err}",
                    path.display()
                )));
            }
        }
    }
    Ok(detected)
}

fn resolve_auth_method(work_dir: &Path, params: &Value) -> Result<String, EngineError> {
    if let Some(method) = params.get("auth_method").and_then(Value::as_str) {
        return Ok(method.to_string());
    }
    let remote_name = params
        .get("remote_name")
        .and_then(Value::as_str)
        .unwrap_or("origin");
    validate_remote_name(remote_name)?;
    let output = run_command(
        work_dir,
        "git",
        &["remote", "get-url", "--push", remote_name],
    )?;
    Ok(classify_remote_url(output.stdout.trim()))
}

fn validate_remote_name(remote_name: &str) -> Result<(), EngineError> {
    if !remote_name.is_empty()
        && remote_name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
    {
        return Ok(());
    }
    Err(step_error(format!("invalid remote name: {remote_name}")))
}

fn observed_scopes_for_auth_method(
    auth_method: &str,
    work_dir: &Path,
    params: &Value,
) -> Result<Vec<String>, EngineError> {
    if auth_method != "https_oauth" {
        return Ok(Vec::new());
    }
    let gh_program = params
        .get("gh_path")
        .and_then(Value::as_str)
        .unwrap_or("gh");
    validate_gh_program(gh_program)?;
    let output = match run_command_allow_failure(work_dir, gh_program, &["auth", "status"]) {
        Ok(output) => output,
        Err(err) => {
            tracing::warn!(
                error = %err,
                "gh auth status could not be executed during workflow auth preflight"
            );
            return Ok(Vec::new());
        }
    };
    if !output.success {
        tracing::warn!(
            "gh auth status failed during workflow auth preflight; treating observed scopes as empty"
        );
        return Ok(Vec::new());
    }
    let auth_status = format!(
        "{}
{}",
        output.stdout, output.stderr
    );
    Ok(parse_token_scopes(&auth_status))
}

fn validate_gh_program(program: &str) -> Result<(), EngineError> {
    // `gh_path` is operator-controlled workflow configuration: allow the
    // default PATH lookup for `gh` or an explicit absolute binary path, while
    // rejecting worktree-relative paths.
    let path = Path::new(program);
    if program == "gh" || path.is_absolute() {
        return Ok(());
    }
    Err(step_error(format!("invalid gh program path: {program}")))
}

fn git_status_paths(work_dir: &Path) -> Result<Vec<String>, EngineError> {
    let output = run_command_bytes(
        work_dir,
        "git",
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    Ok(parse_porcelain_paths(&output))
}

fn run_command_bytes(
    work_dir: &Path,
    program: &str,
    args: &[&str],
) -> Result<Vec<u8>, EngineError> {
    let output = Command::new(program)
        .args(args)
        .current_dir(work_dir)
        .output()
        .map_err(|err| step_error(format!("failed to run {program}: {err}")))?;
    if !output.status.success() {
        return Err(step_error(format!(
            "{program} {} failed with exit code {}",
            args.join(" "),
            output
                .status
                .code()
                .map_or_else(|| "unknown".to_string(), |code| code.to_string())
        )));
    }
    Ok(output.stdout)
}

struct CommandOutput {
    success: bool,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn run_command(
    work_dir: &Path,
    program: &str,
    args: &[&str],
) -> Result<CommandOutput, EngineError> {
    let output = run_command_allow_failure(work_dir, program, args)?;
    if !output.success {
        return Err(step_error(format!(
            "{program} {} failed with exit code {}",
            args.join(" "),
            output
                .exit_code
                .map_or_else(|| "unknown".to_string(), |code| code.to_string())
        )));
    }
    Ok(output)
}

fn run_command_allow_failure(
    work_dir: &Path,
    program: &str,
    args: &[&str],
) -> Result<CommandOutput, EngineError> {
    let output = Command::new(program)
        .args(args)
        .current_dir(work_dir)
        .output()
        .map_err(|err| step_error(format!("failed to run {program}: {err}")))?;
    Ok(CommandOutput {
        success: output.status.success(),
        exit_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn write_report(path: &Path, report: &impl serde::Serialize) -> Result<(), EngineError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| step_error(format!("failed to create artifact directory: {err}")))?;
    }
    let json = serde_json::to_string_pretty(report)
        .map_err(|err| step_error(format!("failed to serialize preflight report: {err}")))?;
    fs::write(path, json)
        .map_err(|err| step_error(format!("failed to write preflight report: {err}")))
}

fn step_error(message: String) -> EngineError {
    EngineError::StepExecutionError {
        step_id: "workflow_auth_preflight".to_string(),
        message,
    }
}
