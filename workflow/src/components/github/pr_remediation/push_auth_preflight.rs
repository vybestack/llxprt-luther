use super::push_stages::{write_push_result_for_exec, PushExecution, PushRemediationCommandResult};
use super::push_support::*;
use super::*;
use crate::adapters::github::parse_token_scopes;
use crate::adapters::workflow_auth_preflight::{
    build_report, classify_remote_url, detect_workflow_paths, WorkflowAuthOutcome,
    WorkflowAuthPreflightConfig,
};
use std::fs;

pub(super) fn workflow_auth_preflight_for_push(
    exec: &PushExecution<'_>,
    commands: &mut Vec<Value>,
    inspection: &PushInspection,
    retry_index: u64,
    config: WorkflowAuthPreflightConfig,
) -> Result<Option<StepOutcome>, EngineError> {
    let detected = detect_workflow_paths(
        inspection.included_paths.iter().map(String::as_str),
        "push_remediation_included_paths",
        &config.workflow_path_patterns,
    );
    if detected.is_empty() {
        return Ok(None);
    }
    let auth_method = push_auth_method(exec, commands)?;
    let (observed_scopes, scope_error) = push_observed_scopes(exec, commands, &auth_method)?;
    let mut report = build_report(auth_method, config, detected, observed_scopes);
    if let Some(scope_error) = scope_error {
        report.outcome = WorkflowAuthOutcome::Fatal;
        report.missing_capability = Some(scope_error);
        report.recommended_operator_action = Some(
            "Install gh or run gh auth login with workflow scope before pushing workflow files."
                .to_string(),
        );
    }
    if report.outcome != WorkflowAuthOutcome::Fatal {
        return Ok(None);
    }
    let reason = report
        .missing_capability
        .clone()
        .unwrap_or_else(|| "workflow auth capability missing".to_string());
    write_fatal_auth_preflight_push_result(
        exec,
        &reason,
        retry_index,
        report,
        std::mem::take(commands),
        inspection,
    )
    .map(Some)
}

fn push_auth_method(
    exec: &PushExecution<'_>,
    commands: &mut Vec<Value>,
) -> Result<String, EngineError> {
    let remote_name = if valid_push_remote_name(&exec.setup.remote_name) {
        exec.setup.remote_name.clone()
    } else {
        commands.push(json!({
            "command_id": "remote-push-url",
            "status": "failed",
            "auth_method": "unknown",
            "spawn_error": format!("invalid remote name: {}", exec.setup.remote_name)
        }));
        return Ok("unknown".to_string());
    };
    let remote = push_runner_command(
        exec.runner,
        "remote-push-url",
        vec![
            "git".to_string(),
            "remote".to_string(),
            "get-url".to_string(),
            "--push".to_string(),
            remote_name,
        ],
        &exec.setup.working_directory,
        &exec.setup.log_dir,
        exec.setup.timeout_seconds,
    );
    let method = if remote.status == "passed" {
        classify_remote_url(remote.bounded_stdout.trim())
    } else {
        "unknown".to_string()
    };
    redact_command_logs(&remote, "remote URL")?;
    commands.push(redacted_remote_url_result(&remote, &method));
    Ok(method)
}

fn valid_push_remote_name(remote_name: &str) -> bool {
    !remote_name.is_empty()
        && remote_name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}

fn redact_command_logs(
    result: &PushRemediationCommandResult,
    label: &str,
) -> Result<(), EngineError> {
    for path in [&result.stdout_log_path, &result.stderr_log_path]
        .into_iter()
        .flatten()
    {
        fs::write(path, format!("<redacted {label}>\n")).map_err(|err| {
            EngineError::InvalidState(format!(
                "failed to redact {label} command log at {}: {err}",
                path.display()
            ))
        })?;
    }
    Ok(())
}

fn redacted_remote_url_result(result: &PushRemediationCommandResult, auth_method: &str) -> Value {
    json!({
        "command_id": result.command_id,
        "argv": result.argv,
        "working_directory": result.working_directory.display().to_string(),
        "status": result.status,
        "exit_code": result.exit_code,
        "signal": result.signal,
        "auth_method": auth_method,
        "stdout_artifact_path": result.stdout_log_path.as_ref().map(|path| path.display().to_string()),
        "stderr_artifact_path": result.stderr_log_path.as_ref().map(|path| path.display().to_string()),
        "spawn_error": result.spawn_error
    })
}

fn push_observed_scopes(
    exec: &PushExecution<'_>,
    commands: &mut Vec<Value>,
    auth_method: &str,
) -> Result<(Vec<String>, Option<String>), EngineError> {
    if auth_method != "https_oauth" {
        return Ok((Vec::new(), None));
    }
    let gh = push_runner_command(
        exec.runner,
        "gh-auth-status",
        vec!["gh".to_string(), "auth".to_string(), "status".to_string()],
        &exec.setup.working_directory,
        &exec.setup.log_dir,
        exec.setup.timeout_seconds,
    );
    let scopes = parse_token_scopes(&format!("{}\n{}", gh.bounded_stdout, gh.bounded_stderr));
    redact_command_logs(&gh, "gh auth status")?;
    commands.push(redacted_gh_auth_status_result(&gh, &scopes));
    if gh.status == "passed" {
        Ok((scopes, None))
    } else {
        Ok((
            scopes,
            Some("unable to determine OAuth scopes with gh auth status".to_string()),
        ))
    }
}

fn redacted_gh_auth_status_result(
    result: &PushRemediationCommandResult,
    scopes: &[String],
) -> Value {
    json!({
        "command_id": result.command_id,
        "argv": result.argv,
        "working_directory": result.working_directory.display().to_string(),
        "status": result.status,
        "exit_code": result.exit_code,
        "signal": result.signal,
        "observed_scopes": scopes,
        "stdout_artifact_path": result.stdout_log_path.as_ref().map(|path| path.display().to_string()),
        "stderr_artifact_path": result.stderr_log_path.as_ref().map(|path| path.display().to_string()),
        "spawn_error": result.spawn_error
    })
}

fn write_fatal_auth_preflight_push_result(
    exec: &PushExecution<'_>,
    reason: &str,
    retry_index: u64,
    report: impl serde::Serialize,
    commands: Vec<Value>,
    inspection: &PushInspection,
) -> Result<StepOutcome, EngineError> {
    let payload = push_payload(
        &exec.setup.binding,
        "fatal",
        retry_index,
        exec.setup.max_push_retries,
        &exec.setup.remote_ref,
        &inspection.pre_push_local_head_sha,
        &inspection.pre_push_remote_head_sha,
        &exec.setup.binding.head_sha,
        None,
        &inspection.pre_push_local_head_sha,
        Some(&inspection.pre_push_remote_head_sha),
        &inspection.pre_push_local_head_sha,
        false,
        inspection.included_paths.clone(),
        inspection.excluded_paths.clone(),
        None,
        Some(reason),
        commands,
        &exec.setup.plan,
        &exec.setup.result,
        exec.test_result,
        exec.clock,
    );
    write_push_result_for_exec(
        exec,
        with_auth_preflight(payload, report),
        Some(("fatal", reason, json!({ "operator_action_required": true }))),
    )?;
    Ok(StepOutcome::Fatal)
}

fn with_auth_preflight(mut payload: Value, report: impl serde::Serialize) -> Value {
    if let Some(object) = payload.as_object_mut() {
        object.insert("auth_preflight".to_string(), json!(report));
    }
    payload
}
