use super::push_porcelain::parse_porcelain_z;

use super::*;
use std::io::Read as IoRead;
#[cfg(unix)]
use std::os::unix::process::CommandExt;

const COMMAND_STDOUT_READ_LIMIT: u64 = 1024 * 1024;

#[derive(Clone, Debug)]
pub(super) struct PushInspection {
    pub(super) pre_push_local_head_sha: String,
    pub(super) pre_push_remote_head_sha: String,
    pub(super) included_paths: Vec<String>,
    pub(super) excluded_paths: Vec<String>,
}

impl PushInspection {
    pub(super) fn with_retry_head(&self, head_sha: &str) -> Self {
        // Retry-scope matching after commit/push must key off the committed
        // local head, even though the original inspection name records the
        // pre-push snapshot used for the diagnostic artifact shape.
        let mut scoped = self.clone();
        scoped.pre_push_local_head_sha = head_sha.to_string();
        scoped
    }
}

pub(super) fn push_working_directory(
    context: &StepContext,
    params: &Value,
) -> Result<PathBuf, EngineError> {
    let working_directory = params
        .get("working_directory")
        .or_else(|| params.get("work_dir"))
        .and_then(Value::as_str)
        .map(|path| resolve_path(context.work_dir(), path))
        .unwrap_or_else(|| context.work_dir().clone());
    validate_safe_working_directory(context.work_dir(), &working_directory)?;
    Ok(working_directory)
}
pub(super) fn push_remote_ref(
    context: &StepContext,
    params: &Value,
    binding: &PrFollowupBinding,
) -> String {
    string_param(
        context,
        params,
        "remote_ref",
        &format!("refs/heads/{}", binding.head_ref),
    )
}

pub(super) fn push_commit_message(
    params: &Value,
    binding: &PrFollowupBinding,
    plan: &Value,
) -> String {
    params
        .get("commit_message")
        .and_then(Value::as_str)
        .filter(|message| !message.trim().is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| {
            let count = plan
                .get("must_fix")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            format!(
                "Apply PR follow-up remediation for #{} ({count} item{})",
                binding.pr_number,
                if count == 1 { "" } else { "s" }
            )
        })
}

pub(super) fn inspect_push_worktree(
    runner: &dyn PushRemediationCommandRunner,
    working_directory: &Path,
    log_dir: &Path,
    timeout_seconds: u64,
    remote_name: &str,
    remote_ref: &str,
) -> Result<(PushInspection, Vec<Value>), (String, Vec<Value>)> {
    let mut commands = Vec::new();
    let local = local_head_sha(runner, working_directory, log_dir, timeout_seconds)?;
    commands.extend(local.1);
    let remote = remote_head_sha(
        runner,
        working_directory,
        log_dir,
        timeout_seconds,
        remote_name,
        remote_ref,
    )?;
    commands.extend(remote.1);
    let status = push_runner_command(
        runner,
        "status-porcelain",
        vec![
            "git".to_string(),
            "status".to_string(),
            "--porcelain=v1".to_string(),
            "-z".to_string(),
            "--untracked-files=all".to_string(),
        ],
        working_directory,
        log_dir,
        timeout_seconds,
    );
    let status_ok = status.status == "passed";
    let status_stdout =
        read_bounded_command_stdout(&status).unwrap_or_else(|| status.bounded_stdout.clone());
    commands.push(push_command_result_json(&status));
    if !status_ok {
        return Err(("status_failed".to_string(), commands));
    }
    let mut changed_paths = parse_porcelain_z(&status_stdout);
    changed_paths.sort();
    changed_paths.dedup();
    let mut included_paths = Vec::new();
    let mut excluded_paths = Vec::new();
    for path in &changed_paths {
        if push_path_is_excluded(path) {
            excluded_paths.push(path.clone());
        } else {
            included_paths.push(path.clone());
        }
    }
    Ok((
        PushInspection {
            pre_push_local_head_sha: local.0,
            pre_push_remote_head_sha: remote.0,
            included_paths,
            excluded_paths,
        },
        commands,
    ))
}

pub(super) fn local_head_sha(
    runner: &dyn PushRemediationCommandRunner,
    working_directory: &Path,
    log_dir: &Path,
    timeout_seconds: u64,
) -> Result<(String, Vec<Value>), (String, Vec<Value>)> {
    let command = push_runner_command(
        runner,
        "local-head",
        vec![
            "git".to_string(),
            "rev-parse".to_string(),
            "HEAD".to_string(),
        ],
        working_directory,
        log_dir,
        timeout_seconds,
    );
    let ok = command.status == "passed";
    let head = command.bounded_stdout.trim().to_string();
    let value = push_command_result_json(&command);
    if ok && !head.is_empty() {
        Ok((head, vec![value]))
    } else {
        Err(("local_head_unavailable".to_string(), vec![value]))
    }
}

pub(super) fn remote_head_sha(
    runner: &dyn PushRemediationCommandRunner,
    working_directory: &Path,
    log_dir: &Path,
    timeout_seconds: u64,
    remote_name: &str,
    remote_ref: &str,
) -> Result<(String, Vec<Value>), (String, Vec<Value>)> {
    let command = push_runner_command(
        runner,
        "remote-head",
        vec![
            "git".to_string(),
            "ls-remote".to_string(),
            "--heads".to_string(),
            remote_name.to_string(),
            remote_ref.to_string(),
        ],
        working_directory,
        log_dir,
        timeout_seconds,
    );
    let ok = command.status == "passed";
    let head = command
        .bounded_stdout
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string();
    let value = push_command_result_json(&command);
    if ok && !head.is_empty() {
        Ok((head, vec![value]))
    } else {
        Err(("remote_head_unavailable".to_string(), vec![value]))
    }
}

pub(super) fn push_runner_command(
    runner: &dyn PushRemediationCommandRunner,
    command_id: &str,
    argv: Vec<String>,
    working_directory: &Path,
    log_dir: &Path,
    timeout_seconds: u64,
) -> PushRemediationCommandResult {
    let log_name = unique_command_log_name(command_id);
    runner.run(PushRemediationCommandRequest {
        command_id: command_id.to_string(),
        argv,
        working_directory: working_directory.to_path_buf(),
        timeout_seconds,
        stdout_log_path: log_dir.join(format!("{log_name}-stdout.log")),
        stderr_log_path: log_dir.join(format!("{log_name}-stderr.log")),
    })
}

pub(super) fn unique_command_log_name(command_id: &str) -> String {
    format!(
        "{}-{}",
        sanitize_command_id(command_id),
        uuid::Uuid::new_v4()
    )
}

pub(super) fn push_path_is_excluded(path: &str) -> bool {
    let normalized = path.trim_start_matches("./");
    normalized == ".llxprt"
        || normalized.starts_with(".llxprt/")
        || path_file_name_matches(normalized, "LLXPRT.md")
        || path_file_name_matches(normalized, ".generated-notice")
        || path_file_name_matches(normalized, ".generated-notice.md")
        || path_file_name_matches(normalized, "GENERATED_NOTICE.md")
        || path_component_matches(normalized, "generated-notice")
}

fn path_file_name_matches(path: &str, file_name: &str) -> bool {
    path.rsplit('/').next() == Some(file_name)
}

fn path_component_matches(path: &str, component: &str) -> bool {
    path.split('/').any(|part| part == component)
}

pub(super) fn must_fix_success_evidence_is_acceptable(plan: &Value, result: &Value) -> bool {
    let must_fix = plan
        .get("must_fix")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let results = result
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if must_fix.is_empty() {
        return true;
    }
    must_fix.iter().all(|item| {
        let source_type = string_field(item, "source_type", "");
        let source_id = string_field(item, "source_id", "");
        results.iter().any(|entry| {
            string_field(entry, "source_type", "") == source_type
                && string_field(entry, "source_id", "") == source_id
                && matches!(
                    string_field(entry, "status", "").as_str(),
                    "fixed" | "changed" | "already_satisfied" | "not_reproduced"
                )
                && entry.get("evidence").is_some_and(evidence_is_non_empty)
        })
    })
}

fn evidence_is_non_empty(evidence: &Value) -> bool {
    match evidence {
        Value::Object(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        _ => false,
    }
}

pub(super) fn validate_push_local_verification_result(
    binding: &PrFollowupBinding,
    plan: &Value,
    result: &Value,
    test_result: &Value,
) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    validate_post_pr_test_result_binding(binding, test_result, &mut errors);
    validate_post_pr_test_result_sequences(plan, result, test_result, &mut errors);
    validate_post_pr_retry_scope(binding, plan, result, test_result, &mut errors);
    let commands = test_result.get("commands").and_then(Value::as_array);
    if commands.is_none_or(Vec::is_empty) {
        errors.push("commands must contain local verification evidence".to_string());
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn validate_post_pr_test_result_binding(
    binding: &PrFollowupBinding,
    test_result: &Value,
    errors: &mut Vec<String>,
) {
    if test_result.get("test_state").and_then(Value::as_str) != Some("passed") {
        errors.push("test_state must be passed".to_string());
    }
    if !binding_from_value(test_result)
        .map(|test_binding| &test_binding == binding)
        .unwrap_or(false)
    {
        errors.push("post-pr-test-result binding mismatch".to_string());
    }
}

fn validate_post_pr_test_result_sequences(
    plan: &Value,
    result: &Value,
    test_result: &Value,
    errors: &mut Vec<String>,
) {
    if test_result.get("plan_artifact_sequence") != plan.get("artifact_sequence") {
        errors.push("plan_artifact_sequence mismatch".to_string());
    }
    if test_result.get("remediation_result_artifact_sequence") != result.get("artifact_sequence") {
        errors.push("remediation_result_artifact_sequence mismatch".to_string());
    }
}

fn validate_post_pr_retry_scope(
    binding: &PrFollowupBinding,
    plan: &Value,
    result: &Value,
    test_result: &Value,
    errors: &mut Vec<String>,
) {
    let scope = test_result.get("retry_scope").and_then(Value::as_object);
    require_scope_str(scope, "run_id", binding.run_id.as_str(), errors);
    require_scope_str(
        scope,
        "repository_owner",
        binding.repository_owner.as_str(),
        errors,
    );
    require_scope_str(
        scope,
        "repository_name",
        binding.repository_name.as_str(),
        errors,
    );
    require_scope_u64(scope, "pr_number", Some(binding.pr_number), errors);
    require_scope_str(scope, "head_sha", binding.head_sha.as_str(), errors);
    require_scope_u64(
        scope,
        "plan_artifact_sequence",
        plan.get("artifact_sequence").and_then(Value::as_u64),
        errors,
    );
    require_scope_u64(
        scope,
        "remediation_result_artifact_sequence",
        result.get("artifact_sequence").and_then(Value::as_u64),
        errors,
    );
}

fn require_scope_str(
    scope: Option<&serde_json::Map<String, Value>>,
    field: &str,
    expected: &str,
    errors: &mut Vec<String>,
) {
    if scope
        .and_then(|scope| scope.get(field))
        .and_then(Value::as_str)
        != Some(expected)
    {
        errors.push(format!("retry_scope.{field} mismatch"));
    }
}

fn require_scope_u64(
    scope: Option<&serde_json::Map<String, Value>>,
    field: &str,
    expected: Option<u64>,
    errors: &mut Vec<String>,
) {
    let observed = scope
        .and_then(|scope| scope.get(field))
        .and_then(Value::as_u64);
    if expected.is_none() {
        errors.push(format!("missing source value for retry_scope.{field}"));
    } else if observed != expected {
        errors.push(format!("retry_scope.{field} mismatch"));
    }
}

pub(super) fn current_push_retry_index(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    plan: &Value,
    result: &Value,
    local_head: &str,
    remote_ref: &str,
) -> Result<u64, EngineError> {
    let path = store.canonical_path(binding, "push-remediation-result");
    if !path.exists() {
        return Ok(0);
    }
    let value = store.read_current_raw_json(binding, "push-remediation-result")?;
    let same_scope = value
        .get("retry_scope")
        .and_then(Value::as_object)
        .is_some_and(|scope| {
            push_retry_scope_matches(scope, binding, plan, result, local_head, remote_ref)
        });
    Ok(if same_scope {
        value
            .get("push_retry_index")
            .and_then(Value::as_u64)
            .unwrap_or_default()
            + 1
    } else {
        0
    })
}

fn push_retry_scope_matches(
    scope: &serde_json::Map<String, Value>,
    binding: &PrFollowupBinding,
    plan: &Value,
    result: &Value,
    local_head: &str,
    remote_ref: &str,
) -> bool {
    scope.get("run_id").and_then(Value::as_str) == Some(binding.run_id.as_str())
        && scope.get("repository_owner").and_then(Value::as_str)
            == Some(binding.repository_owner.as_str())
        && scope.get("repository_name").and_then(Value::as_str)
            == Some(binding.repository_name.as_str())
        && scope.get("pr_number").and_then(Value::as_u64) == Some(binding.pr_number)
        && scope.get("head_sha").and_then(Value::as_str) == Some(local_head)
        && scope.get("remote_ref").and_then(Value::as_str) == Some(remote_ref)
        && scope.get("plan_artifact_sequence") == plan.get("artifact_sequence")
        && scope.get("remediation_result_artifact_sequence") == result.get("artifact_sequence")
}

#[allow(clippy::too_many_arguments)]
pub(super) fn write_push_config_fatal(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    max_push_retries: u64,
    remote_ref: &str,
    reason: &str,
    details: Value,
    plan: &Value,
    result: &Value,
    test_result: Value,
    clock: &dyn ClockSleeper,
) -> Result<StepOutcome, EngineError> {
    let payload = push_payload(
        binding,
        "fatal",
        0,
        max_push_retries,
        remote_ref,
        "unknown",
        "unknown",
        &binding.head_sha,
        None,
        "unknown",
        None,
        "unknown",
        false,
        Vec::new(),
        Vec::new(),
        None,
        Some(reason),
        Vec::new(),
        plan,
        result,
        &test_result,
        clock,
    );
    write_push_result(
        store,
        binding,
        step_id,
        step_order,
        payload,
        Some(("fatal", reason, details)),
        clock,
    )?;
    Ok(StepOutcome::Fatal)
}

// Pre-existing artifact writer shape shared by push remediation.
#[allow(clippy::too_many_arguments)]
pub(super) fn write_retryable_push_failure(
    store: &PrFollowupArtifactStore,

    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    retry_index: u64,
    max_push_retries: u64,
    remote_ref: &str,
    reason: &str,
    commands: Vec<Value>,
    inspection: &PushInspection,
    plan: &Value,
    result: &Value,
    test_result: &Value,
    clock: &dyn ClockSleeper,
) -> Result<StepOutcome, EngineError> {
    let exhausted = retry_index >= max_push_retries;
    let state = if exhausted {
        "retry_exhausted"
    } else {
        "retryable_failed"
    };
    let payload = push_payload(
        binding,
        state,
        retry_index,
        max_push_retries,
        remote_ref,
        &inspection.pre_push_local_head_sha,
        &inspection.pre_push_remote_head_sha,
        &binding.head_sha,
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
        plan,
        result,
        test_result,
        clock,
    );
    write_push_result(
        store,
        binding,
        step_id,
        step_order,
        payload,
        Some((state, reason, json!({ "push_retry_index": retry_index }))),
        clock,
    )?;
    Ok(if exhausted {
        StepOutcome::Fatal
    } else {
        StepOutcome::Retryable
    })
}

// Pre-existing artifact writer shape shared by push remediation.
#[allow(clippy::too_many_arguments)]
pub(super) fn write_push_failure_from_observation(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    max_push_retries: u64,
    remote_ref: &str,
    fatal: bool,
    reason: &str,
    commands: Vec<Value>,
    plan: &Value,
    result: &Value,
    test_result: &Value,
    clock: &dyn ClockSleeper,
) -> Result<StepOutcome, EngineError> {
    let retry_head = binding.head_sha.as_str();
    let retry_index = if fatal {
        0
    } else {
        current_push_retry_index(store, binding, plan, result, retry_head, remote_ref)?
    };
    // max_push_retries is the maximum retry index; index 0 is the first
    // retryable failure artifact, so equality means the configured cap is hit.
    let exhausted = !fatal && retry_index >= max_push_retries;
    let resolved_state = if fatal {
        "fatal"
    } else if exhausted {
        "retry_exhausted"
    } else {
        "retryable_failed"
    };
    let payload = push_payload(
        binding,
        resolved_state,
        retry_index,
        max_push_retries,
        remote_ref,
        retry_head,
        "unknown",
        &binding.head_sha,
        None,
        retry_head,
        None,
        retry_head,
        false,
        Vec::new(),
        Vec::new(),
        None,
        Some(reason),
        commands,
        plan,
        result,
        test_result,
        clock,
    );
    write_push_result(
        store,
        binding,
        step_id,
        step_order,
        payload,
        Some((
            resolved_state,
            reason,
            json!({ "push_retry_index": retry_index }),
        )),
        clock,
    )?;
    Ok(if fatal || exhausted {
        StepOutcome::Fatal
    } else {
        StepOutcome::Retryable
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn push_payload(
    binding: &PrFollowupBinding,
    push_state: &str,
    retry_index: u64,
    max_push_retries: u64,
    remote_ref: &str,
    pre_push_local_head_sha: &str,
    pre_push_remote_head_sha: &str,
    pre_push_pr_head_sha: &str,
    committed_head_sha: Option<&str>,
    post_push_local_head_sha: &str,
    post_push_remote_head_sha: Option<&str>,
    expected_head_sha: &str,
    verified_remote_matches_expected: bool,
    staged_paths: Vec<String>,
    excluded_paths: Vec<String>,
    commit_message: Option<&str>,
    push_error_class: Option<&str>,
    commands: Vec<Value>,
    plan: &Value,
    result: &Value,
    test_result: &Value,
    clock: &dyn ClockSleeper,
) -> Value {
    json!({
        "push_state": push_state,
        "push_retry_index": retry_index,
        "max_push_retries": max_push_retries,
        "retry_scope": {
            "run_id": binding.run_id,
            "repository_owner": binding.repository_owner,
            "repository_name": binding.repository_name,
            "pr_number": binding.pr_number,
            "head_sha": committed_head_sha.unwrap_or(pre_push_local_head_sha),
            "remote_ref": remote_ref,
            "plan_artifact_sequence": plan.get("artifact_sequence"),
            "remediation_result_artifact_sequence": result.get("artifact_sequence")
        },
        "remote_ref": remote_ref,
        "pre_push_local_head_sha": pre_push_local_head_sha,
        "pre_push_remote_head_sha": pre_push_remote_head_sha,
        "pre_push_pr_head_sha": pre_push_pr_head_sha,
        "committed_head_sha": committed_head_sha,
        "post_push_local_head_sha": post_push_local_head_sha,
        "post_push_remote_head_sha": post_push_remote_head_sha,
        "expected_head_sha": expected_head_sha,
        "verified_remote_matches_expected": verified_remote_matches_expected,
        "staged_paths": staged_paths,
        "excluded_paths": excluded_paths,
        "commit_message": commit_message,
        "push_error_class": push_error_class,
        "commands": commands,
        "stdout_artifact_path": Value::Null,
        "stderr_artifact_path": Value::Null,
        "source_artifacts": [
            source_artifact(plan, "pr-remediation-plan"),
            source_artifact(result, "pr-remediation-result"),
            source_artifact(test_result, "post-pr-test-result")
        ],
        "pushed_at": clock.now_rfc3339()
    })
}

pub(super) fn write_push_result(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    payload: Value,
    failure: Option<(&str, &str, Value)>,
    clock: &dyn ClockSleeper,
) -> Result<(), EngineError> {
    store.write_json_artifact(
        binding,
        "push-remediation-result",
        step_id,
        step_order,
        &payload,
        failure,
        clock,
    )?;
    Ok(())
}

fn read_bounded_command_stdout(result: &PushRemediationCommandResult) -> Option<String> {
    let path = result.stdout_log_path.as_ref()?;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) => {
            tracing::warn!(
                command_id = %result.command_id,
                path = %path.display(),
                error = %err,
                "failed to open stdout log"
            );
            return None;
        }
    };
    let mut output = Vec::new();
    if let Err(err) = file
        .take(COMMAND_STDOUT_READ_LIMIT)
        .read_to_end(&mut output)
    {
        tracing::warn!(
            command_id = %result.command_id,
            path = %path.display(),
            error = %err,
            "failed to read stdout log"
        );
        return None;
    }
    let valid_len = trailing_utf8_boundary(&output);
    output.truncate(valid_len);
    Some(String::from_utf8_lossy(&output).into_owned())
}

fn trailing_utf8_boundary(bytes: &[u8]) -> usize {
    let mut index = 0;
    let mut last_safe = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii() {
            index += 1;
            last_safe = index;
            continue;
        }
        if is_utf8_continuation(byte) {
            break;
        }
        let expected_len = match utf8_sequence_len(byte) {
            Some(len) => len,
            None => break,
        };
        let end = index + expected_len;
        if end > bytes.len()
            || !bytes[index + 1..end]
                .iter()
                .all(|byte| is_utf8_continuation(*byte))
        {
            break;
        }
        index = end;
        last_safe = index;
    }
    last_safe
}

fn is_utf8_continuation(byte: u8) -> bool {
    (byte & 0b1100_0000) == 0b1000_0000
}

fn utf8_sequence_len(byte: u8) -> Option<usize> {
    match byte {
        0xC2..=0xDF => Some(2),
        0xE0..=0xEF => Some(3),
        0xF0..=0xF4 => Some(4),
        _ => None,
    }
}

pub(super) fn push_command_result_json(result: &PushRemediationCommandResult) -> Value {
    json!({
        "command_id": result.command_id,
        "argv": result.argv,
        "working_directory": result.working_directory.display().to_string(),
        "status": result.status,
        "exit_code": result.exit_code,
        "signal": result.signal,
        "bounded_stdout": result.bounded_stdout,
        "bounded_stderr": result.bounded_stderr,
        "stdout_artifact_path": result.stdout_log_path.as_ref().map(|path| path.display().to_string()),
        "stderr_artifact_path": result.stderr_log_path.as_ref().map(|path| path.display().to_string()),
        "spawn_error": result.spawn_error
    })
}

pub(super) fn run_push_remediation_process(
    request: PushRemediationCommandRequest,
) -> PushRemediationCommandResult {
    let mut child = match spawn_push_remediation_child(&request) {
        Ok(child) => child,
        Err(err) => return push_remediation_spawn_error(request, err),
    };
    let output = match wait_for_push_remediation_child(&mut child, request.timeout_seconds) {
        Ok(output) => output,
        Err(err) => return push_remediation_spawn_error(request, err),
    };
    let stdout = output.stdout_text();
    let stderr = output.stderr_text();
    write_optional_log(&request.stdout_log_path, &stdout);
    write_optional_log(&request.stderr_log_path, &stderr);
    push_remediation_process_result(request, output.exit_code, output.timed_out, stdout, stderr)
}

fn spawn_push_remediation_child(
    request: &PushRemediationCommandRequest,
) -> std::io::Result<std::process::Child> {
    let program = request.argv.first().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "push remediation command argv must not be empty",
        )
    })?;
    let mut command = Command::new(program);
    command.args(&request.argv[1..]);
    command.current_dir(&request.working_directory);
    command.env_clear();
    apply_allowed_command_environment(&mut command);
    command.env("PWD", &request.working_directory);
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.stdin(Stdio::null());
    #[cfg(unix)]
    command.process_group(0);
    command.spawn()
}

fn push_remediation_spawn_error(
    request: PushRemediationCommandRequest,
    err: std::io::Error,
) -> PushRemediationCommandResult {
    write_optional_log(&request.stdout_log_path, "");
    write_optional_log(&request.stderr_log_path, &err.to_string());
    PushRemediationCommandResult {
        command_id: request.command_id,
        argv: request.argv,
        working_directory: request.working_directory,
        exit_code: None,
        signal: None,
        status: "fatal".to_string(),
        bounded_stdout: String::new(),
        bounded_stderr: bounded_excerpt(&err.to_string(), 4096),
        stdout_log_path: Some(request.stdout_log_path),
        stderr_log_path: Some(request.stderr_log_path),
        spawn_error: Some(err.to_string()),
    }
}

fn wait_for_push_remediation_child(
    child: &mut std::process::Child,
    timeout_seconds: u64,
) -> std::io::Result<ProcessOutputCapture> {
    let stdout_buffer = Arc::new(Mutex::new(String::new()));
    let stderr_buffer = Arc::new(Mutex::new(String::new()));
    let stdout_reader = spawn_reader(child.stdout.take(), &stdout_buffer);
    let stderr_reader = spawn_reader(child.stderr.take(), &stderr_buffer);
    let wait_result = wait_for_child_exit(child, timeout_seconds);
    join_reader(stdout_reader);
    join_reader(stderr_reader);
    let (exit_code, timed_out) = wait_result?;
    Ok(ProcessOutputCapture {
        stdout_buffer,
        stderr_buffer,
        exit_code,
        timed_out,
    })
}

fn push_remediation_process_result(
    request: PushRemediationCommandRequest,
    exit_code: Option<i32>,
    timed_out: bool,
    stdout: String,
    stderr: String,
) -> PushRemediationCommandResult {
    PushRemediationCommandResult {
        command_id: request.command_id,
        argv: request.argv,
        working_directory: request.working_directory,
        exit_code,
        signal: None,
        status: process_status(timed_out, exit_code).to_string(),
        bounded_stdout: bounded_excerpt(&stdout, 4096),
        bounded_stderr: bounded_excerpt(&stderr, 4096),
        stdout_log_path: Some(request.stdout_log_path),
        stderr_log_path: Some(request.stderr_log_path),
        spawn_error: timed_out.then(|| "push remediation command timed out".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_utf8_boundary_rejects_all_continuation_bytes() {
        assert_eq!(trailing_utf8_boundary(&[0x80, 0xBF]), 0);
    }

    #[test]
    fn trailing_utf8_boundary_trims_incomplete_trailing_sequence() {
        assert_eq!(trailing_utf8_boundary(&[b'a', 0xE2, 0x82]), 1);
    }

    #[test]
    fn trailing_utf8_boundary_keeps_complete_and_trims_invalid_bytes() {
        assert_eq!(trailing_utf8_boundary(&[b'a', 0xE2, 0x82, 0xAC]), 4);
        assert_eq!(trailing_utf8_boundary(&[b'a', 0xFF]), 1);
    }

    #[test]
    fn trailing_utf8_boundary_trims_after_complete_multibyte() {
        assert_eq!(trailing_utf8_boundary(&[0xE2, 0x82, 0xAC, 0xE2, 0x82]), 3);
    }
}
