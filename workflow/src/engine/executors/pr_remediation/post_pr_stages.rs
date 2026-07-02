use super::post_pr_test_process::{
    run_manifest_post_pr_test_process, run_post_pr_test_process, validated_command_manifest,
};
use super::*;
use crate::engine::executors::command_manifest::{
    manifest_default_working_directory, resolve_manifest_group_id,
};
use crate::workflow::command_manifest::CommandEntry;

/// Dedicated post-PR local verification executor for `run_post_pr_tests`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017,REQ-PRFU-020
/// @pseudocode lines 29-33
#[derive(Debug, Default)]
pub struct RunPostPrTestsExecutor;
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-33
impl StepExecutor for RunPostPrTestsExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        run_post_pr_tests(
            context,
            params,
            &SystemClockSleeper,
            &SystemPostPrTestCommandRunner,
        )
    }
}
/// Safe argv-only command runner used by post-PR local verification.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-30
pub trait PostPrTestCommandRunner: Send + Sync {
    fn run(&self, request: PostPrTestCommandRequest) -> PostPrTestCommandResult;
}
/// Owned post-PR test command request.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-30
#[derive(Clone, Debug)]
pub struct PostPrTestCommandRequest {
    pub command_id: String,
    pub argv: Vec<String>,
    pub repo_root_directory: PathBuf,
    pub working_directory: PathBuf,
    pub artifact_base_directory: PathBuf,
    pub timeout_seconds: u64,
    pub stdout_log_path: PathBuf,
    pub stderr_log_path: PathBuf,
    pub manifest_entry: Option<CommandEntry>,
}
/// Owned post-PR test command result.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 30-33
#[derive(Clone, Debug, Default)]
pub struct PostPrTestCommandResult {
    pub command_id: String,
    pub argv: Vec<String>,
    pub working_directory: PathBuf,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub status: String,
    pub bounded_stdout: String,
    pub bounded_stderr: String,
    pub stdout_log_path: Option<PathBuf>,
    pub stderr_log_path: Option<PathBuf>,
    pub spawn_error: Option<String>,
    pub expectation_failures: Vec<String>,
    pub artifact_failures: Vec<String>,
    pub failure_classification: Option<String>,
}
/// Production post-PR test command runner.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-30
#[derive(Debug, Default)]
pub struct SystemPostPrTestCommandRunner;
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-30
impl PostPrTestCommandRunner for SystemPostPrTestCommandRunner {
    fn run(&self, request: PostPrTestCommandRequest) -> PostPrTestCommandResult {
        if request.manifest_entry.is_some() {
            return run_manifest_post_pr_test_process(request);
        }
        run_post_pr_test_process(request)
    }
}
/// Testable post-PR verification executor with injected runner and clock.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-33
pub struct RunPostPrTestsExecutorWithRunner<R, C> {
    runner: R,
    clock: C,
}
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-33
impl<R, C> RunPostPrTestsExecutorWithRunner<R, C> {
    #[must_use]
    pub fn new(runner: R, clock: C) -> Self {
        Self { runner, clock }
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-33
impl<R, C> StepExecutor for RunPostPrTestsExecutorWithRunner<R, C>
where
    R: PostPrTestCommandRunner,
    C: ClockSleeper,
{
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        run_post_pr_tests(context, params, &self.clock, &self.runner)
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-33
fn run_post_pr_tests(
    context: &StepContext,
    params: &Value,
    clock: &dyn ClockSleeper,
    runner: &dyn PostPrTestCommandRunner,
) -> Result<StepOutcome, EngineError> {
    let setup = load_post_pr_test_setup(context, params, clock)?;
    let max_retries = match u64_required_param(params, "max_verification_retries", 2) {
        Ok(value) => value,
        Err(err) => {
            return write_post_pr_test_configuration_fatal(
                &setup,
                0,
                0,
                "malformed_retry_cap",
                vec![err.to_string()],
                clock,
            );
        }
    };
    let commands = match post_pr_test_commands(context, params) {
        Ok(commands) => commands,
        Err(err) => {
            return write_post_pr_test_configuration_fatal(
                &setup,
                max_retries,
                0,
                "invalid_command_configuration",
                vec![err.to_string()],
                clock,
            );
        }
    };

    let log_dir = setup
        .store
        .canonical_path(&setup.binding, "post-pr-test-result")
        .with_file_name("post-pr-test-logs");
    let summary = run_post_pr_test_commands(runner, commands, &log_dir);

    if !summary.infrastructure_errors.is_empty() {
        return write_post_pr_test_configuration_fatal(
            &setup,
            max_retries,
            0,
            "infrastructure_failure",
            summary.infrastructure_errors,
            clock,
        );
    }

    let retry_index =
        current_verification_retry_index(&setup.store, &setup.binding, &setup.plan, &setup.result)?;
    write_post_pr_test_completion(&setup, summary, retry_index, max_retries, clock)
}

struct PostPrTestSetup {
    store: PrFollowupArtifactStore,
    binding: PrFollowupBinding,
    plan: Value,
    result: Value,
    step_id: String,
    step_order: u64,
}

struct PostPrTestCommandSummary {
    command_results: Vec<Value>,
    infrastructure_errors: Vec<String>,
    any_failed: bool,
}

fn load_post_pr_test_setup(
    context: &StepContext,
    params: &Value,
    clock: &dyn ClockSleeper,
) -> Result<PostPrTestSetup, EngineError> {
    let artifact_root = artifact_root(context, params)?;
    let store =
        PrFollowupArtifactStore::with_filesystem(&artifact_root, &SystemPrFollowupFilesystem)?;
    let binding = binding_for_context(context, params, &store, clock)?;
    let plan = store.read_current_json(&binding, "pr-remediation-plan")?;
    let result = store.read_current_json(&binding, "pr-remediation-result")?;
    Ok(PostPrTestSetup {
        store,
        binding,
        plan,
        result,
        step_id: current_step_id(context, "run_post_pr_tests"),
        step_order: u64_param(params, "step_order_index", 10),
    })
}

fn write_post_pr_test_configuration_fatal(
    setup: &PostPrTestSetup,
    max_retries: u64,
    retry_index: u64,
    reason: &str,
    errors: Vec<String>,
    clock: &dyn ClockSleeper,
) -> Result<StepOutcome, EngineError> {
    write_post_pr_test_fatal(
        &setup.store,
        &setup.binding,
        &setup.step_id,
        setup.step_order,
        &setup.plan,
        &setup.result,
        retry_index,
        max_retries,
        reason,
        errors,
        clock,
    )
}

fn run_post_pr_test_commands(
    runner: &dyn PostPrTestCommandRunner,
    commands: Vec<PostPrTestCommandConfig>,
    log_dir: &Path,
) -> PostPrTestCommandSummary {
    let mut summary = PostPrTestCommandSummary {
        command_results: Vec::new(),
        infrastructure_errors: Vec::new(),
        any_failed: false,
    };
    for command in commands {
        let result = runner.run(post_pr_test_request(&command, log_dir));
        record_post_pr_test_command_result(&mut summary, &result);
    }
    summary
}

fn post_pr_test_request(
    command: &PostPrTestCommandConfig,
    log_dir: &Path,
) -> PostPrTestCommandRequest {
    let log_name = super::push_support::unique_command_log_name(&command.command_id);
    PostPrTestCommandRequest {
        command_id: command.command_id.clone(),
        argv: command.argv.clone(),
        repo_root_directory: command.repo_root_directory.clone(),
        working_directory: command.working_directory.clone(),
        artifact_base_directory: command.artifact_base_directory.clone(),
        timeout_seconds: command.timeout_seconds,
        stdout_log_path: log_dir.join(format!("{log_name}-stdout.log")),
        stderr_log_path: log_dir.join(format!("{log_name}-stderr.log")),
        manifest_entry: command.manifest_entry.clone(),
    }
}

fn record_post_pr_test_command_result(
    summary: &mut PostPrTestCommandSummary,
    result: &PostPrTestCommandResult,
) {
    match result.status.as_str() {
        "fatal" => summary
            .infrastructure_errors
            .push(format!("command {} reported fatal", result.command_id)),
        "passed" => {}
        _ => summary.any_failed = true,
    }
    summary.command_results.push(command_result_json(result));
}

fn write_post_pr_test_completion(
    setup: &PostPrTestSetup,
    summary: PostPrTestCommandSummary,
    retry_index: u64,
    max_retries: u64,
    clock: &dyn ClockSleeper,
) -> Result<StepOutcome, EngineError> {
    let exhausted = summary.any_failed && retry_index >= max_retries;
    let test_state = if summary.any_failed {
        "failed"
    } else {
        "passed"
    };
    let payload = post_pr_test_payload(
        &setup.binding,
        test_state,
        summary.command_results,
        retry_index,
        max_retries,
        exhausted,
        &setup.plan,
        &setup.result,
        Vec::new(),
        clock,
    )?;
    let failure = if exhausted {
        Some((
            "failed",
            "verification_retry_cap_exhausted",
            json!({ "verification_retry_index": retry_index, "max_verification_retries": max_retries }),
        ))
    } else {
        None
    };
    setup.store.write_json_artifact(
        &setup.binding,
        "post-pr-test-result",
        &setup.step_id,
        setup.step_order,
        &payload,
        failure,
        clock,
    )?;

    Ok(if !summary.any_failed {
        StepOutcome::Success
    } else if exhausted {
        StepOutcome::Fatal
    } else {
        StepOutcome::Fixable
    })
}

#[derive(Clone, Debug)]
struct PostPrTestCommandConfig {
    command_id: String,
    argv: Vec<String>,
    repo_root_directory: PathBuf,
    working_directory: PathBuf,
    artifact_base_directory: PathBuf,
    timeout_seconds: u64,
    manifest_entry: Option<CommandEntry>,
}
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-30
fn post_pr_test_commands(
    context: &StepContext,
    params: &Value,
) -> Result<Vec<PostPrTestCommandConfig>, EngineError> {
    let manifest_commands = manifest_group_post_pr_commands(context, params)?;
    let commands_value = params
        .get("commands")
        .or_else(|| params.get("post_pr_test_commands"));
    let owned_commands;
    let commands = if let Some(commands_value) = commands_value {
        commands_value
            .as_array()
            .ok_or_else(|| pr_remediation_error("post-PR test commands must be an array"))?
    } else if let Some(commands) = manifest_commands {
        owned_commands = commands;
        &owned_commands
    } else {
        return Err(pr_remediation_error("missing post-PR test commands"));
    };
    if commands.is_empty() {
        return Err(pr_remediation_error(
            "post-PR test commands must not be empty",
        ));
    }
    let mut configured = Vec::new();
    for (index, value) in commands.iter().enumerate() {
        configured.push(post_pr_test_command(context, params, value, index)?);
    }
    Ok(configured)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-30
fn post_pr_test_command(
    context: &StepContext,
    params: &Value,
    value: &Value,
    index: usize,
) -> Result<PostPrTestCommandConfig, EngineError> {
    let object = post_pr_command_object(value)?;
    let command_id = post_pr_command_id(object, index);
    let manifest_entry = post_pr_manifest_entry(params, object)?;
    let argv = post_pr_command_argv(params, object, manifest_entry.as_ref())?;
    let repo_root_directory = context.work_dir().clone();
    let working_directory =
        post_pr_command_working_directory(context, object, manifest_entry.as_ref())?;
    let artifact_base_directory = post_pr_command_artifact_base_directory(context);
    let timeout_seconds = post_pr_command_timeout(params, object, manifest_entry.as_ref())?;

    Ok(PostPrTestCommandConfig {
        command_id,
        argv,
        repo_root_directory,
        working_directory,
        artifact_base_directory,
        timeout_seconds,
        manifest_entry,
    })
}

fn post_pr_command_object(value: &Value) -> Result<&serde_json::Map<String, Value>, EngineError> {
    if value.as_str().is_some() {
        return Err(pr_remediation_error(
            "scalar shell-string post-PR commands are forbidden",
        ));
    }
    let object = value
        .as_object()
        .ok_or_else(|| pr_remediation_error("post-PR command entries must be objects"))?;
    if object.contains_key("command") || object.contains_key("shell") {
        return Err(pr_remediation_error(
            "shell-string post-PR commands are forbidden",
        ));
    }
    Ok(object)
}

fn post_pr_command_id(object: &serde_json::Map<String, Value>, index: usize) -> String {
    object
        .get("id")
        .or_else(|| object.get("command_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("post-pr-test-{index}"))
}

fn post_pr_manifest_entry(
    params: &Value,
    object: &serde_json::Map<String, Value>,
) -> Result<Option<CommandEntry>, EngineError> {
    object
        .get("command_id")
        .and_then(Value::as_str)
        .map_or(Ok(None), |id| manifest_command_entry(params, id))
}

fn post_pr_command_argv(
    params: &Value,
    object: &serde_json::Map<String, Value>,
    manifest_entry: Option<&CommandEntry>,
) -> Result<Vec<String>, EngineError> {
    let argv = if let Some(entry) = manifest_entry {
        entry.argv.clone()
    } else if let Some(argv) = object.get("argv") {
        string_array(argv, "argv")?
    } else if let Some(id) = object.get("command_id").and_then(Value::as_str) {
        configured_command_argv(params, id)?
    } else {
        return Err(pr_remediation_error(
            "post-PR command requires argv or command_id",
        ));
    };
    validate_post_pr_argv(argv)
}

fn validate_post_pr_argv(argv: Vec<String>) -> Result<Vec<String>, EngineError> {
    if argv.is_empty() || argv.iter().any(|arg| arg.is_empty()) {
        return Err(pr_remediation_error(
            "post-PR command argv must not be empty",
        ));
    }
    Ok(argv)
}

fn post_pr_command_working_directory(
    context: &StepContext,
    object: &serde_json::Map<String, Value>,
    manifest_entry: Option<&CommandEntry>,
) -> Result<PathBuf, EngineError> {
    let working_directory = object
        .get("working_directory")
        .or_else(|| object.get("work_dir"))
        .and_then(Value::as_str)
        .or_else(|| manifest_entry.and_then(explicit_manifest_working_directory))
        .map(|path| resolve_path(context.work_dir(), path))
        .unwrap_or_else(|| default_command_working_directory(context));
    validate_safe_working_directory(context.work_dir(), &working_directory)?;
    Ok(working_directory)
}

fn explicit_manifest_working_directory(entry: &CommandEntry) -> Option<&str> {
    entry
        .working_directory
        .as_deref()
        .or(entry.project_subdirectory.as_deref())
}

fn default_command_working_directory(context: &StepContext) -> PathBuf {
    manifest_default_working_directory(context)
}

fn post_pr_command_artifact_base_directory(context: &StepContext) -> PathBuf {
    context
        .get("artifact_base_dir")
        .filter(|value| !value.is_empty())
        .map_or_else(|| context.work_dir().clone(), PathBuf::from)
}

fn post_pr_command_timeout(
    params: &Value,
    object: &serde_json::Map<String, Value>,
    manifest_entry: Option<&CommandEntry>,
) -> Result<u64, EngineError> {
    let timeout_seconds = manifest_entry
        .and_then(|entry| entry.timeout_seconds)
        .or_else(|| object.get("timeout_seconds").and_then(Value::as_u64))
        .unwrap_or_else(|| u64_param(params, "test_timeout_seconds", 900));
    if timeout_seconds == 0 {
        return Err(pr_remediation_error(
            "post-PR command timeout_seconds must be positive",
        ));
    }
    Ok(timeout_seconds)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-30
fn configured_command_argv(params: &Value, id: &str) -> Result<Vec<String>, EngineError> {
    let registry = params
        .get("command_registry")
        .or_else(|| params.get("commands_by_id"))
        .and_then(Value::as_object)
        .ok_or_else(|| pr_remediation_error("command_id used without command registry"))?;
    let Some(value) = registry.get(id) else {
        return Err(pr_remediation_error(format!(
            "unrecognized command_id {id}"
        )));
    };
    string_array(value, "command_registry entry")
}

fn manifest_group_post_pr_commands(
    context: &StepContext,
    params: &Value,
) -> Result<Option<Vec<Value>>, EngineError> {
    let Some(value) = params.get("command_manifest") else {
        return Ok(None);
    };
    let manifest = validated_command_manifest(value)?;
    let group_id =
        resolve_manifest_group_id(params, context, "post_pr").map_err(pr_remediation_error)?;
    let Some(command_ids) = manifest.groups.get(group_id.as_str()) else {
        return Err(pr_remediation_error(format!(
            "unknown command_manifest group '{group_id}'"
        )));
    };
    Ok(Some(
        command_ids
            .iter()
            .map(|id| json!({ "command_id": id }))
            .collect(),
    ))
}

fn manifest_command_entry(params: &Value, id: &str) -> Result<Option<CommandEntry>, EngineError> {
    let Some(value) = params.get("command_manifest") else {
        return Ok(None);
    };
    let manifest = validated_command_manifest(value)?;
    Ok(manifest.commands.into_iter().find(|entry| entry.id == id))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-30
fn string_array(value: &Value, field: &str) -> Result<Vec<String>, EngineError> {
    value
        .as_array()
        .ok_or_else(|| pr_remediation_error(format!("{field} must be an argv array")))?
        .iter()
        .map(|arg| {
            arg.as_str()
                .filter(|text| !text.is_empty())
                .map(ToString::to_string)
                .ok_or_else(|| {
                    pr_remediation_error(format!("{field} contains a non-string or empty arg"))
                })
        })
        .collect()
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-30
pub(super) fn validate_safe_working_directory(
    work_dir: &Path,
    candidate: &Path,
) -> Result<(), EngineError> {
    let base = work_dir
        .canonicalize()
        .map_err(|err| pr_remediation_error(format!("canonicalize work_dir: {err}")))?;
    let candidate = candidate.canonicalize().map_err(|err| {
        pr_remediation_error(format!(
            "canonicalize post-PR test working_directory: {err}"
        ))
    })?;
    if candidate.starts_with(&base) {
        Ok(())
    } else {
        Err(pr_remediation_error(
            "post-PR test working_directory must stay under workflow work_dir",
        ))
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014
/// @pseudocode lines 32-33
fn current_verification_retry_index(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    plan: &Value,
    result: &Value,
) -> Result<u64, EngineError> {
    let (plan_sequence, result_sequence) = verification_source_artifact_sequences(plan, result)?;
    match store.read_current_json(binding, "post-pr-test-result") {
        Ok(value)
            if same_verification_retry_scope_for_sequences(
                &value,
                plan_sequence,
                result_sequence,
            ) =>
        {
            Ok(value
                .get("verification_retry_index")
                .and_then(Value::as_u64)
                .unwrap_or_default()
                + 1)
        }
        Ok(_) | Err(_) => Ok(0),
    }
}

fn same_verification_retry_scope_for_sequences(
    value: &Value,
    plan_sequence: u64,
    result_sequence: u64,
) -> bool {
    retry_scope_sequence(value, "/retry_scope/plan_artifact_sequence") == Some(plan_sequence)
        && retry_scope_sequence(value, "/retry_scope/remediation_result_artifact_sequence")
            == Some(result_sequence)
}

fn verification_source_artifact_sequences(
    plan: &Value,
    result: &Value,
) -> Result<(u64, u64), EngineError> {
    Ok((
        artifact_sequence(plan, "pr-remediation-plan")?,
        artifact_sequence(result, "pr-remediation-result")?,
    ))
}

fn artifact_sequence(value: &Value, artifact_family: &str) -> Result<u64, EngineError> {
    value
        .get("artifact_sequence")
        .and_then(Value::as_u64)
        .ok_or_else(|| pr_remediation_error(format!("{artifact_family} missing artifact_sequence")))
}

fn retry_scope_sequence(value: &Value, pointer: &str) -> Option<u64> {
    value.pointer(pointer).and_then(Value::as_u64)
}

#[allow(clippy::too_many_arguments)]
fn write_post_pr_test_fatal(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    plan: &Value,
    result: &Value,
    retry_index: u64,
    max_retries: u64,
    reason: &str,
    errors: Vec<String>,
    clock: &dyn ClockSleeper,
) -> Result<StepOutcome, EngineError> {
    let payload = match post_pr_test_payload(
        binding,
        "fatal",
        Vec::new(),
        retry_index,
        max_retries,
        true,
        plan,
        result,
        errors.clone(),
        clock,
    ) {
        Ok(payload) => payload,
        Err(err) => post_pr_test_fatal_payload(
            binding,
            retry_index,
            max_retries,
            errors.clone(),
            err,
            clock,
        ),
    };
    store.write_json_artifact(
        binding,
        "post-pr-test-result",
        step_id,
        step_order,
        &payload,
        Some(("fatal", reason, json!({ "errors": errors }))),
        clock,
    )?;
    Ok(StepOutcome::Fatal)
}

#[allow(clippy::too_many_arguments)]
fn post_pr_test_payload(
    binding: &PrFollowupBinding,
    test_state: &str,
    commands: Vec<Value>,
    retry_index: u64,
    max_retries: u64,
    exhausted: bool,
    plan: &Value,
    result: &Value,
    errors: Vec<String>,
    clock: &dyn ClockSleeper,
) -> Result<Value, EngineError> {
    let plan_sequence = artifact_sequence(plan, "pr-remediation-plan")?;
    let result_sequence = artifact_sequence(result, "pr-remediation-result")?;
    Ok(json!({
        "test_state": test_state,
        "commands": commands,
        "verification_retry_index": retry_index,
        "max_verification_retries": max_retries,
        "retry_scope": {
            "run_id": binding.run_id,
            "repository_owner": binding.repository_owner,
            "repository_name": binding.repository_name,
            "pr_number": binding.pr_number,
            "head_sha": binding.head_sha,
            "plan_artifact_sequence": plan_sequence,
            "remediation_result_artifact_sequence": result_sequence,
        },
        "plan_artifact_sequence": plan_sequence,
        "remediation_result_artifact_sequence": result_sequence,
        "verification_retry_exhausted": exhausted,
        "configuration_errors": errors,
        "verified_at": clock.now_rfc3339()
    }))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 30-31
fn post_pr_test_fatal_payload(
    binding: &PrFollowupBinding,
    retry_index: u64,
    max_retries: u64,
    errors: Vec<String>,
    payload_error: EngineError,
    clock: &dyn ClockSleeper,
) -> Value {
    json!({
        "test_state": "fatal",
        "commands": [],
        "verification_retry_index": retry_index,
        "max_verification_retries": max_retries,
        "retry_scope": {
            "run_id": binding.run_id,
            "repository_owner": binding.repository_owner,
            "repository_name": binding.repository_name,
            "pr_number": binding.pr_number,
            "head_sha": binding.head_sha,
            "plan_artifact_sequence": Value::Null,
            "remediation_result_artifact_sequence": Value::Null,
        },
        "plan_artifact_sequence": Value::Null,
        "remediation_result_artifact_sequence": Value::Null,
        "verification_retry_exhausted": true,
        "configuration_errors": errors,
        "payload_error": payload_error.to_string(),
        "verified_at": clock.now_rfc3339()
    })
}

fn command_result_json(result: &PostPrTestCommandResult) -> Value {
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
        "spawn_error": result.spawn_error,
        "expectation_failures": result.expectation_failures,
        "artifact_failures": result.artifact_failures,
        "failure_classification": result.failure_classification
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-017
/// @pseudocode lines 29-30
pub(super) fn sanitize_command_id(command_id: &str) -> String {
    command_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

/// Post-PR iteration guard executor for `post_pr_iteration_guard`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-020
/// @pseudocode lines 8-15
#[derive(Debug, Default)]
pub struct PostPrIterationGuardExecutor;

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-020
/// @pseudocode lines 8-15
impl StepExecutor for PostPrIterationGuardExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        let artifact_root = artifact_root(context, params)?;
        let store = PrFollowupArtifactStore::new(artifact_root);
        let binding = binding_for_context(context, params, &store, &SystemClockSleeper)?;
        let max_iterations = u64_param(params, "max_post_pr_remediation_iterations", 3);
        let previous = latest_guard_for_current_run(&store, &binding)?;
        let (iteration_index, previous_head_sha, reason) = match previous.as_ref() {
            None => (0, Value::Null, "initial_entry"),
            Some(guard)
                if guard.get("head_sha").and_then(Value::as_str)
                    == Some(binding.head_sha.as_str()) =>
            {
                (
                    guard
                        .get("iteration_index")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    Value::String(binding.head_sha.clone()),
                    "same_head_reentry",
                )
            }
            Some(guard) => (
                guard
                    .get("iteration_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    + 1,
                guard.get("head_sha").cloned().unwrap_or(Value::Null),
                "head_sha_changed_after_remediation_push",
            ),
        };
        let exceeded = iteration_index > max_iterations;
        let payload = json!({
            "guard_state": if exceeded { "max_iterations_exceeded" } else { "proceed" },
            "iteration_index": iteration_index,
            "max_post_pr_remediation_iterations": max_iterations,
            "previous_head_sha": previous_head_sha,
            "reason": if exceeded { "max_iterations_exceeded" } else { reason },
            "ignored_stale_artifacts": [],
            "updated_at": SystemClockSleeper.now_rfc3339()
        });
        let failure = exceeded.then(|| {
            (
                "fatal",
                "max_iterations_exceeded",
                json!({
                    "iteration_index": iteration_index,
                    "max_post_pr_remediation_iterations": max_iterations
                }),
            )
        });
        store.write_json_artifact(
            &binding,
            "post-pr-iteration-guard",
            "post_pr_iteration_guard",
            u64_param(params, "step_order_index", 2),
            &payload,
            failure,
            &SystemClockSleeper,
        )?;
        if exceeded {
            Ok(StepOutcome::Fatal)
        } else {
            Ok(StepOutcome::Success)
        }
    }
}

fn latest_guard_for_current_run(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
) -> Result<Option<Value>, EngineError> {
    let root = store
        .root()
        .join("pr-followup")
        .join("history")
        .join(&binding.run_id)
        .join(&binding.repository_owner)
        .join(&binding.repository_name)
        .join(binding.pr_number.to_string())
        .join("post-pr-iteration-guard");
    if !root.exists() {
        return Ok(None);
    }
    let mut values = Vec::new();
    for entry in std::fs::read_dir(&root)
        .map_err(|err| pr_remediation_error(format!("read guard history: {err}")))?
    {
        let path = entry
            .map_err(|err| pr_remediation_error(format!("read guard history entry: {err}")))?
            .path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let content = std::fs::read_to_string(&path).map_err(|err| {
            pr_remediation_error(format!("read guard artifact {}: {err}", path.display()))
        })?;
        let value: Value = serde_json::from_str(&content).map_err(|err| {
            pr_remediation_error(format!("parse guard artifact {}: {err}", path.display()))
        })?;
        if binding_from_value(&value).is_ok_and(|actual| {
            actual.run_id == binding.run_id
                && actual.repository_owner == binding.repository_owner
                && actual.repository_name == binding.repository_name
                && actual.pr_number == binding.pr_number
        }) {
            values.push(value);
        }
    }
    values.sort_by_key(|value| {
        value
            .get("artifact_sequence")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    });
    Ok(values.pop())
}

/// Post-PR failure terminal executor contract for `post_pr_failure_terminal`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-020
/// @pseudocode lines 50-53
#[derive(Debug, Default)]
pub struct PostPrFailureTerminalExecutor;

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-020
/// @pseudocode lines 50-53
impl StepExecutor for PostPrFailureTerminalExecutor {
    fn execute(
        &self,
        _context: &mut StepContext,
        _params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        Ok(StepOutcome::Fatal)
    }
}
