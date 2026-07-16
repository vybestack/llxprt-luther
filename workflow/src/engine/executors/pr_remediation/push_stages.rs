use super::push_auth_preflight::workflow_auth_preflight_for_push;
use super::push_support::*;
use super::*;
use crate::adapters::workflow_auth_preflight::WorkflowAuthPreflightConfig;

/// Dedicated remediation push executor for `push_remediation_changes`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017,REQ-PRFU-020
/// @pseudocode lines 34-40
#[derive(Debug, Default)]
pub struct PushRemediationChangesExecutor;

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 34-40
impl StepExecutor for PushRemediationChangesExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        push_remediation_changes(
            context,
            params,
            &SystemClockSleeper,
            &SystemPushRemediationCommandRunner,
        )
    }
}

/// Safe argv-only command runner used by remediation push.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 35-39
pub trait PushRemediationCommandRunner: Send + Sync {
    fn run(&self, request: PushRemediationCommandRequest) -> PushRemediationCommandResult;
}

/// Owned remediation push command request.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 35-39
#[derive(Clone, Debug)]
pub struct PushRemediationCommandRequest {
    pub command_id: String,
    pub argv: Vec<String>,
    pub working_directory: PathBuf,
    pub timeout_seconds: u64,
    pub stdout_log_path: PathBuf,
    pub stderr_log_path: PathBuf,
}

/// Owned remediation push command result.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 35-40
#[derive(Clone, Debug, Default)]
pub struct PushRemediationCommandResult {
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
}

/// Production remediation push command runner.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 35-39
#[derive(Debug, Default)]
pub struct SystemPushRemediationCommandRunner;

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 35-39
impl PushRemediationCommandRunner for SystemPushRemediationCommandRunner {
    fn run(&self, request: PushRemediationCommandRequest) -> PushRemediationCommandResult {
        run_push_remediation_process(request)
    }
}

/// Testable remediation push executor with injected runner and clock.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 34-40
pub struct PushRemediationChangesExecutorWithRunner<R, C> {
    runner: R,
    clock: C,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 34-40
impl<R, C> PushRemediationChangesExecutorWithRunner<R, C> {
    #[must_use]
    pub fn new(runner: R, clock: C) -> Self {
        Self { runner, clock }
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 34-40
impl<R, C> StepExecutor for PushRemediationChangesExecutorWithRunner<R, C>
where
    R: PushRemediationCommandRunner,
    C: ClockSleeper,
{
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        push_remediation_changes(context, params, &self.clock, &self.runner)
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 34-40
fn push_remediation_changes(
    context: &mut StepContext,
    params: &Value,
    clock: &dyn ClockSleeper,
    runner: &dyn PushRemediationCommandRunner,
) -> Result<StepOutcome, EngineError> {
    if let Some(outcome) = crate::engine::executors::scope_control_barrier_pub(context) {
        return Ok(outcome);
    }
    let setup = load_push_run_setup(context, params, clock)?;
    let test_result = match read_verified_post_pr_test_result(&setup, clock) {
        Ok(test_result) => test_result,
        Err(outcome) => return outcome,
    };
    if !must_fix_success_evidence_is_acceptable(&setup.plan, &setup.result) {
        return write_missing_validator_success_evidence(&setup, &test_result, clock);
    }

    let mut commands = Vec::new();
    let inspection = match inspect_push_worktree(
        runner,
        &setup.working_directory,
        &setup.log_dir,
        setup.timeout_seconds,
        &setup.remote_name,
        &setup.remote_ref,
    ) {
        Ok((inspection, observed)) => {
            commands.extend(observed);
            inspection
        }
        Err((reason, observed)) => {
            commands.extend(observed);
            let exec = PushExecution::new(&setup, &test_result, runner, clock);
            return write_push_failure_from_exec(&exec, true, reason.as_str(), commands);
        }
    };

    let retry_index = current_push_retry_index(
        &setup.store,
        &setup.binding,
        &setup.plan,
        &setup.result,
        &inspection.pre_push_local_head_sha,
        &setup.remote_ref,
    )?;
    let exec = PushExecution::new(&setup, &test_result, runner, clock);
    let auth_config = push_workflow_auth_preflight_config(params)?;
    if let Some(outcome) = workflow_auth_preflight_for_push(
        &exec,
        &mut commands,
        &inspection,
        retry_index,
        auth_config,
    )? {
        return Ok(outcome);
    }
    if inspection.included_paths.is_empty() {
        handle_no_included_push_paths(&exec, commands, inspection, retry_index)
    } else {
        handle_included_push_paths(&exec, params, commands, inspection, retry_index)
    }
}

fn push_workflow_auth_preflight_config(
    params: &serde_json::Value,
) -> Result<WorkflowAuthPreflightConfig, EngineError> {
    serde_json::from_value(params.clone()).map_err(|err| EngineError::StepExecutionError {
        step_id: "push_remediation_changes".to_string(),
        message: format!("invalid workflow auth preflight parameters: {err}"),
    })
}

pub(super) struct PushRunSetup {
    pub(super) store: PrFollowupArtifactStore,
    pub(super) binding: PrFollowupBinding,
    pub(super) plan: Value,
    pub(super) result: Value,
    pub(super) step_id: String,
    pub(super) step_order: u64,
    pub(super) max_push_retries: u64,
    pub(super) timeout_seconds: u64,
    pub(super) working_directory: PathBuf,
    pub(super) remote_ref: String,
    pub(super) remote_name: String,
    pub(super) log_dir: PathBuf,
}

pub(super) struct PushExecution<'a> {
    pub(super) setup: &'a PushRunSetup,
    pub(super) test_result: &'a Value,
    pub(super) runner: &'a dyn PushRemediationCommandRunner,
    pub(super) clock: &'a dyn ClockSleeper,
}

impl<'a> PushExecution<'a> {
    fn new(
        setup: &'a PushRunSetup,
        test_result: &'a Value,
        runner: &'a dyn PushRemediationCommandRunner,
        clock: &'a dyn ClockSleeper,
    ) -> Self {
        Self {
            setup,
            test_result,
            runner,
            clock,
        }
    }
}

fn load_push_run_setup(
    context: &StepContext,
    params: &Value,
    clock: &dyn ClockSleeper,
) -> Result<PushRunSetup, EngineError> {
    let artifact_root = artifact_root(context, params)?;
    let store =
        PrFollowupArtifactStore::with_filesystem(&artifact_root, &SystemPrFollowupFilesystem)?;
    let binding = binding_for_context(context, params, &store, clock)?;
    let plan = store.read_current_json(&binding, "pr-remediation-plan")?;
    let result = store.read_current_json(&binding, "pr-remediation-result")?;
    let working_directory = push_working_directory(context, params)?;
    let remote_ref = push_remote_ref(context, params, &binding);
    let log_dir = store
        .canonical_path(&binding, "push-remediation-result")
        .with_file_name("push-remediation-logs");
    Ok(PushRunSetup {
        store,
        binding,
        plan,
        result,
        step_id: current_step_id(context, "push_remediation_changes"),
        step_order: u64_param(params, "step_order_index", 11),
        max_push_retries: u64_required_param(params, "max_push_retries", 1)?,
        timeout_seconds: u64_required_param(params, "push_timeout_seconds", 900)?,
        working_directory,
        remote_ref,
        remote_name: string_param(context, params, "remote_name", "origin"),
        log_dir,
    })
}

fn read_verified_post_pr_test_result(
    setup: &PushRunSetup,
    clock: &dyn ClockSleeper,
) -> Result<Value, Result<StepOutcome, EngineError>> {
    let test_result = setup
        .store
        .read_current_json(&setup.binding, "post-pr-test-result")
        .map_err(|err| {
            write_push_config_fatal_for_setup(
                setup,
                "missing_or_unreadable_post_pr_test_result",
                json!({ "error": err.to_string() }),
                Value::Null,
                clock,
            )
        })?;
    validate_push_local_verification_result(
        &setup.binding,
        &setup.plan,
        &setup.result,
        &test_result,
    )
    .map_err(|errors| {
        write_push_config_fatal_for_setup(
            setup,
            "post_pr_local_verification_not_passed",
            json!({ "errors": errors }),
            test_result.clone(),
            clock,
        )
    })?;
    Ok(test_result)
}

fn write_push_config_fatal_for_setup(
    setup: &PushRunSetup,
    reason: &str,
    details: Value,
    test_result: Value,
    clock: &dyn ClockSleeper,
) -> Result<StepOutcome, EngineError> {
    write_push_config_fatal(
        &setup.store,
        &setup.binding,
        &setup.step_id,
        setup.step_order,
        setup.max_push_retries,
        &setup.remote_ref,
        reason,
        details,
        &setup.plan,
        &setup.result,
        test_result,
        clock,
    )
}

fn write_missing_validator_success_evidence(
    setup: &PushRunSetup,
    test_result: &Value,
    clock: &dyn ClockSleeper,
) -> Result<StepOutcome, EngineError> {
    let payload = push_payload(
        &setup.binding,
        "fatal",
        0,
        setup.max_push_retries,
        &setup.remote_ref,
        "unknown",
        "unknown",
        &setup.binding.head_sha,
        None,
        "unknown",
        None,
        "unknown",
        false,
        Vec::new(),
        Vec::new(),
        None,
        Some("missing_validator_success_evidence"),
        Vec::new(),
        &setup.plan,
        &setup.result,
        test_result,
        clock,
    );
    write_push_result(
        &setup.store,
        &setup.binding,
        &setup.step_id,
        setup.step_order,
        payload,
        Some(("fatal", "missing_validator_success_evidence", json!({}))),
        clock,
    )?;
    Ok(StepOutcome::Fatal)
}

fn handle_no_included_push_paths(
    exec: &PushExecution<'_>,
    commands: Vec<Value>,
    inspection: PushInspection,
    retry_index: u64,
) -> Result<StepOutcome, EngineError> {
    if inspection.pre_push_remote_head_sha != inspection.pre_push_local_head_sha {
        push_existing_local_head(exec, commands, inspection, retry_index)
    } else {
        write_no_change_push_result(exec, commands, inspection, retry_index)
    }
}

fn push_existing_local_head(
    exec: &PushExecution<'_>,
    mut commands: Vec<Value>,
    inspection: PushInspection,
    retry_index: u64,
) -> Result<StepOutcome, EngineError> {
    let push = run_git_push(exec);
    let push_ok = push.status == "passed";
    commands.push(push_command_result_json(&push));
    if !push_ok {
        return write_retryable_push_failure_for_exec(
            exec,
            retry_index,
            "push_failed",
            commands,
            &inspection,
        );
    }
    let remote_after = match remote_head_for_exec(exec) {
        Ok((head, observed)) => {
            commands.extend(observed);
            head
        }
        Err((reason, observed)) => {
            commands.extend(observed);
            return write_retryable_push_failure_for_exec(
                exec,
                retry_index,
                reason.as_str(),
                commands,
                &inspection,
            );
        }
    };
    write_existing_head_push_result(exec, commands, inspection, retry_index, remote_after)
}

fn write_existing_head_push_result(
    exec: &PushExecution<'_>,
    commands: Vec<Value>,
    inspection: PushInspection,
    retry_index: u64,
    remote_after: String,
) -> Result<StepOutcome, EngineError> {
    let verified = remote_after == inspection.pre_push_local_head_sha;
    let payload = push_payload(
        &exec.setup.binding,
        if verified {
            "pushed_existing_head"
        } else {
            "retryable_failed"
        },
        retry_index,
        exec.setup.max_push_retries,
        &exec.setup.remote_ref,
        &inspection.pre_push_local_head_sha,
        &inspection.pre_push_remote_head_sha,
        &exec.setup.binding.head_sha,
        Some(&inspection.pre_push_local_head_sha),
        &inspection.pre_push_local_head_sha,
        Some(&remote_after),
        &inspection.pre_push_local_head_sha,
        verified,
        Vec::new(),
        inspection.excluded_paths,
        None,
        (!verified).then_some("remote_head_mismatch_after_push"),
        commands,
        &exec.setup.plan,
        &exec.setup.result,
        exec.test_result,
        exec.clock,
    );
    let failure = (!verified).then(|| {
        (
            "retryable_failed",
            "remote_head_mismatch_after_push",
            json!({ "committed_head": inspection.pre_push_local_head_sha, "remote_head": remote_after }),
        )
    });
    write_push_result_for_exec(exec, payload, failure)?;
    Ok(push_outcome_for_verification(
        verified,
        retry_index,
        exec.setup.max_push_retries,
    ))
}

fn write_no_change_push_result(
    exec: &PushExecution<'_>,
    commands: Vec<Value>,
    inspection: PushInspection,
    retry_index: u64,
) -> Result<StepOutcome, EngineError> {
    let state = if inspection.excluded_paths.is_empty() {
        "no_change"
    } else {
        "no_change_excluded_only"
    };
    let verified = inspection.pre_push_remote_head_sha == inspection.pre_push_local_head_sha;
    let payload = push_payload(
        &exec.setup.binding,
        state,
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
        verified,
        Vec::new(),
        inspection.excluded_paths,
        None,
        (!verified).then_some("remote_head_mismatch"),
        commands,
        &exec.setup.plan,
        &exec.setup.result,
        exec.test_result,
        exec.clock,
    );
    let failure = (!verified).then(|| {
        (
            "fatal",
            "remote_head_mismatch",
            json!({ "local_head": inspection.pre_push_local_head_sha, "remote_head": inspection.pre_push_remote_head_sha }),
        )
    });
    write_push_result_for_exec(exec, payload, failure)?;

    Ok(if verified {
        StepOutcome::Fixable
    } else {
        StepOutcome::Fatal
    })
}

fn handle_included_push_paths(
    exec: &PushExecution<'_>,
    params: &Value,
    mut commands: Vec<Value>,
    inspection: PushInspection,
    retry_index: u64,
) -> Result<StepOutcome, EngineError> {
    if let Some(outcome) = stage_remediation_changes(exec, &mut commands, &inspection, retry_index)?
    {
        return Ok(outcome);
    }
    let commit_message = push_commit_message(params, &exec.setup.binding, &exec.setup.plan);
    if let Some(outcome) = commit_remediation_changes(
        exec,
        &mut commands,
        &inspection,
        retry_index,
        &commit_message,
    )? {
        return Ok(outcome);
    }
    let committed_head = match local_head_for_exec(exec) {
        Ok((head, observed)) => {
            commands.extend(observed);
            head
        }
        Err((reason, observed)) => {
            commands.extend(observed);
            return write_retryable_push_failure_for_exec(
                exec,
                retry_index,
                reason.as_str(),
                commands,
                &inspection,
            );
        }
    };
    let committed_retry_index = current_push_retry_index(
        &exec.setup.store,
        &exec.setup.binding,
        &exec.setup.plan,
        &exec.setup.result,
        &committed_head,
        &exec.setup.remote_ref,
    )?;
    push_committed_head(
        exec,
        commands,
        inspection,
        committed_retry_index,
        commit_message,
        committed_head,
    )
}

fn stage_remediation_changes(
    exec: &PushExecution<'_>,
    commands: &mut Vec<Value>,
    inspection: &PushInspection,
    retry_index: u64,
) -> Result<Option<StepOutcome>, EngineError> {
    let stage = push_runner_command(
        exec.runner,
        "stage",
        vec!["git".to_string(), "add".to_string(), "--".to_string()]
            .into_iter()
            .chain(inspection.included_paths.iter().cloned())
            .collect(),
        &exec.setup.working_directory,
        &exec.setup.log_dir,
        exec.setup.timeout_seconds,
    );
    let stage_ok = stage.status == "passed";
    commands.push(push_command_result_json(&stage));
    if stage_ok {
        Ok(None)
    } else {
        write_retryable_push_failure_for_exec(
            exec,
            retry_index,
            "stage_failed",
            std::mem::take(commands),
            inspection,
        )
        .map(Some)
    }
}

fn commit_remediation_changes(
    exec: &PushExecution<'_>,
    commands: &mut Vec<Value>,
    inspection: &PushInspection,
    retry_index: u64,
    commit_message: &str,
) -> Result<Option<StepOutcome>, EngineError> {
    let commit = push_runner_command(
        exec.runner,
        "commit",
        vec![
            "git".to_string(),
            "commit".to_string(),
            "-m".to_string(),
            commit_message.to_string(),
            "--".to_string(),
        ]
        .into_iter()
        .chain(inspection.included_paths.iter().cloned())
        .collect(),
        &exec.setup.working_directory,
        &exec.setup.log_dir,
        exec.setup.timeout_seconds,
    );
    let commit_ok = commit.status == "passed";
    commands.push(push_command_result_json(&commit));
    if commit_ok {
        Ok(None)
    } else {
        write_retryable_push_failure_for_exec(
            exec,
            retry_index,
            "commit_failed",
            std::mem::take(commands),
            inspection,
        )
        .map(Some)
    }
}

fn push_committed_head(
    exec: &PushExecution<'_>,
    mut commands: Vec<Value>,
    inspection: PushInspection,
    retry_index: u64,
    commit_message: String,
    committed_head: String,
) -> Result<StepOutcome, EngineError> {
    let push = run_git_push(exec);
    let push_ok = push.status == "passed";
    commands.push(push_command_result_json(&push));
    if !push_ok {
        return write_retryable_push_failure_for_exec(
            exec,
            retry_index,
            "push_failed",
            commands,
            &inspection.with_retry_head(&committed_head),
        );
    }
    let remote_after = match remote_head_for_exec(exec) {
        Ok((head, observed)) => {
            commands.extend(observed);
            head
        }
        Err((reason, observed)) => {
            commands.extend(observed);
            return write_retryable_push_failure_for_exec(
                exec,
                retry_index,
                reason.as_str(),
                commands,
                &inspection.with_retry_head(&committed_head),
            );
        }
    };
    write_committed_head_push_result(
        exec,
        commands,
        inspection,
        retry_index,
        commit_message,
        committed_head,
        remote_after,
    )
}

fn write_committed_head_push_result(
    exec: &PushExecution<'_>,
    commands: Vec<Value>,
    inspection: PushInspection,
    retry_index: u64,
    commit_message: String,
    committed_head: String,
    remote_after: String,
) -> Result<StepOutcome, EngineError> {
    let verified = remote_after == committed_head;
    let payload = push_payload(
        &exec.setup.binding,
        if verified {
            "pushed"
        } else {
            "retryable_failed"
        },
        retry_index,
        exec.setup.max_push_retries,
        &exec.setup.remote_ref,
        &inspection.pre_push_local_head_sha,
        &inspection.pre_push_remote_head_sha,
        &exec.setup.binding.head_sha,
        Some(&committed_head),
        &committed_head,
        Some(&remote_after),
        &committed_head,
        verified,
        inspection.included_paths,
        inspection.excluded_paths,
        Some(&commit_message),
        (!verified).then_some("remote_head_mismatch_after_push"),
        commands,
        &exec.setup.plan,
        &exec.setup.result,
        exec.test_result,
        exec.clock,
    );
    let failure = (!verified).then(|| {
        (
            "retryable_failed",
            "remote_head_mismatch_after_push",
            json!({ "committed_head": committed_head, "remote_head": remote_after }),
        )
    });
    write_push_result_for_exec(exec, payload, failure)?;
    Ok(push_outcome_for_verification(
        verified,
        retry_index,
        exec.setup.max_push_retries,
    ))
}

fn run_git_push(exec: &PushExecution<'_>) -> PushRemediationCommandResult {
    push_runner_command(
        exec.runner,
        "push",
        vec![
            "git".to_string(),
            "push".to_string(),
            exec.setup.remote_name.clone(),
            format!("HEAD:{}", exec.setup.remote_ref),
        ],
        &exec.setup.working_directory,
        &exec.setup.log_dir,
        exec.setup.timeout_seconds,
    )
}

fn local_head_for_exec(
    exec: &PushExecution<'_>,
) -> Result<(String, Vec<Value>), (String, Vec<Value>)> {
    local_head_sha(
        exec.runner,
        &exec.setup.working_directory,
        &exec.setup.log_dir,
        exec.setup.timeout_seconds,
    )
}

fn remote_head_for_exec(
    exec: &PushExecution<'_>,
) -> Result<(String, Vec<Value>), (String, Vec<Value>)> {
    remote_head_sha(
        exec.runner,
        &exec.setup.working_directory,
        &exec.setup.log_dir,
        exec.setup.timeout_seconds,
        &exec.setup.remote_name,
        &exec.setup.remote_ref,
    )
}

fn write_retryable_push_failure_for_exec(
    exec: &PushExecution<'_>,
    retry_index: u64,
    reason: &str,
    commands: Vec<Value>,
    inspection: &PushInspection,
) -> Result<StepOutcome, EngineError> {
    write_retryable_push_failure(
        &exec.setup.store,
        &exec.setup.binding,
        &exec.setup.step_id,
        exec.setup.step_order,
        retry_index,
        exec.setup.max_push_retries,
        &exec.setup.remote_ref,
        reason,
        commands,
        inspection,
        &exec.setup.plan,
        &exec.setup.result,
        exec.test_result,
        exec.clock,
    )
}

fn write_push_failure_from_exec(
    exec: &PushExecution<'_>,
    fatal: bool,
    reason: &str,
    commands: Vec<Value>,
) -> Result<StepOutcome, EngineError> {
    write_push_failure_from_observation(
        &exec.setup.store,
        &exec.setup.binding,
        &exec.setup.step_id,
        exec.setup.step_order,
        exec.setup.max_push_retries,
        &exec.setup.remote_ref,
        fatal,
        reason,
        commands,
        &exec.setup.plan,
        &exec.setup.result,
        exec.test_result,
        exec.clock,
    )
}

pub(super) fn write_push_result_for_exec(
    exec: &PushExecution<'_>,
    payload: Value,
    failure: Option<(&str, &str, Value)>,
) -> Result<(), EngineError> {
    write_push_result(
        &exec.setup.store,
        &exec.setup.binding,
        &exec.setup.step_id,
        exec.setup.step_order,
        payload,
        failure,
        exec.clock,
    )
}

fn push_outcome_for_verification(
    verified: bool,
    retry_index: u64,
    max_push_retries: u64,
) -> StepOutcome {
    if verified {
        StepOutcome::Success
    } else if retry_index >= max_push_retries {
        StepOutcome::Fatal
    } else {
        StepOutcome::Retryable
    }
}
