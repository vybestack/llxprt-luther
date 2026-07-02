use super::*;

#[test]
fn preview_for_log_truncates_on_utf8_boundary() {
    let text = format!("{}○tail", "a".repeat(499));

    let preview = preview_for_log(&text, 500);

    assert_eq!(preview.len(), 499);
    assert!(preview.ends_with('a'));
}

#[test]
fn preview_for_log_preserves_short_utf8_text() {
    let text = "status ○ complete";

    assert_eq!(preview_for_log(text, 500), text);
}

#[test]
fn preview_for_log_keeps_ascii_boundary_at_limit() {
    let text = format!("{}○tail", "a".repeat(500));

    let preview = preview_for_log(&text, 500);

    assert_eq!(preview.len(), 500);
    assert!(preview.chars().all(|ch| ch == 'a'));
}

#[test]
fn preview_for_log_handles_zero_max_bytes() {
    assert_eq!(preview_for_log("some output", 0), "");
    assert_eq!(preview_for_log("○", 0), "");
}

struct FailingUtf8Executor;

impl crate::engine::executor::StepExecutor for FailingUtf8Executor {
    fn execute(
        &self,
        context: &mut StepContext,
        _params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        let boundary_split_text = format!("{}○tail", "a".repeat(499));
        context.set("stdout", &boundary_split_text);
        context.set("stderr", &boundary_split_text);
        Ok(StepOutcome::Fixable)
    }
}

#[test]
fn run_failure_logging_handles_multibyte_stdout_and_stderr() {
    let workflow_type = crate::workflow::schema::WorkflowType {
        workflow_type_id: "utf8-failure-log".to_string(),
        steps: vec![crate::workflow::schema::StepDef {
            step_id: "fail".to_string(),
            step_type: "failing_utf8".to_string(),
            description: None,
            parameters: None,
            produces: None,
            consumes: None,
            terminal: None,
        }],
        transitions: vec![],
        guards: Default::default(),
    };
    let config = test_workflow_config("utf8-failure-log");
    let instance = WorkflowInstance::create(workflow_type, config);
    let mut registry = crate::engine::executor::ExecutorRegistry::new();
    registry.register("failing_utf8", Box::new(FailingUtf8Executor));
    let mut runner = EngineRunner::new(instance, registry).expect("runner");

    let outcome = runner
        .run()
        .expect("run should not panic while logging output");

    assert!(
        matches!(outcome, RunOutcome::Failure { step_id, reason } if step_id == "fail" && reason == "Fixable error with no recovery transition")
    );
}

fn test_workflow_config(workflow_type_id: &str) -> crate::workflow::schema::WorkflowConfig {
    crate::workflow::schema::WorkflowConfig {
        config_id: format!("{workflow_type_id}-config"),
        workflow_type_id: workflow_type_id.to_string(),
        runtime: crate::workflow::schema::RuntimeConfig {
            timeout_seconds: 3600,
            max_retries: 3,
            parallel_steps: None,
            log_level: None,
        },
        repo: crate::workflow::schema::RepoConfig {
            workspace_strategy: "temp".to_string(),
            branch_template: "test-{run_id}".to_string(),
            base_branch: Some("main".to_string()),
            workspace_root: None,
            project_subdir: None,
            artifact_path_base: None,
            diff_path_base: None,
            diff_path_normalization: None,
        },
        guard_limits: crate::workflow::schema::GuardLimits {
            max_iterations: Some(3),
            max_file_changes: Some(50),
            max_tokens: Some(10000),
            max_cost: Some(10.0),
        },
        variables: std::collections::HashMap::new(),
        discovery: None,
        command_manifest: None,
    }
}

#[test]
fn llxprt_engine_error_variants_render() {
    let not_found = EngineError::LlxprtBinaryNotFound {
        path: "/opt/llxprt".to_string(),
    };
    assert_eq!(
        not_found.to_string(),
        "llxprt binary not found at `/opt/llxprt`"
    );
    let version = EngineError::LlxprtVersionError {
        path: "llxprt".to_string(),
        message: "boom".to_string(),
    };
    assert_eq!(
        version.to_string(),
        "llxprt binary at `llxprt` failed version check: boom"
    );
    let profile = EngineError::LlxprtProfileError {
        profile: "fast".to_string(),
        message: "missing".to_string(),
    };
    assert_eq!(
        profile.to_string(),
        "llxprt profile `fast` could not be resolved: missing"
    );
}

#[test]
fn llxprt_error_maps_to_engine_error() {
    use crate::adapters::llxprt::LlxprtError;
    let mapped: EngineError = LlxprtError::BinaryNotFound {
        path: "llxprt".to_string(),
    }
    .into();
    assert!(matches!(mapped, EngineError::LlxprtBinaryNotFound { .. }));
    let mapped: EngineError = LlxprtError::VersionCheckFailed {
        path: "llxprt".to_string(),
        message: "x".to_string(),
    }
    .into();
    assert!(matches!(mapped, EngineError::LlxprtVersionError { .. }));
    let mapped: EngineError = LlxprtError::NotExecutable {
        path: "llxprt".to_string(),
        message: "x".to_string(),
    }
    .into();
    assert!(matches!(mapped, EngineError::LlxprtVersionError { .. }));
}

#[test]
fn engine_error_display_formats_correctly() {
    // @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    let err = EngineError::StepExecutionError {
        step_id: "test_step".to_string(),
        message: "something failed".to_string(),
    };
    assert!(err.to_string().contains("test_step"));
}

#[test]
fn run_outcome_variants_exist() {
    // @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    let _success = RunOutcome::Success;
    let _failure = RunOutcome::Failure {
        step_id: "s1".to_string(),
        reason: "test".to_string(),
    };
    let _abandoned = RunOutcome::Abandoned {
        step_id: "s2".to_string(),
        reason: "loop".to_string(),
    };
    let _interrupted = RunOutcome::Interrupted {
        step_id: "s3".to_string(),
    };
    let _waiting = RunOutcome::WaitingExternal {
        step_id: "s4".to_string(),
        reason: "checks pending".to_string(),
    };
}

#[test]
fn engine_runner_can_be_created() {
    // @plan:PLAN-20260404-INITIAL-RUNTIME.P08
    // @plan:PLAN-20260408-STEP-EXEC.P06
    use crate::workflow::schema::{
        GuardLimits, RepoConfig, RuntimeConfig, WorkflowConfig, WorkflowType,
    };

    let workflow_type = WorkflowType {
        workflow_type_id: "test".to_string(),
        steps: vec![],
        transitions: vec![],
        guards: Default::default(),
    };

    let config = WorkflowConfig {
        config_id: "test-config".to_string(),
        workflow_type_id: "test".to_string(),
        runtime: RuntimeConfig {
            timeout_seconds: 3600,
            max_retries: 3,
            parallel_steps: None,
            log_level: None,
        },
        repo: RepoConfig {
            workspace_strategy: "temp".to_string(),
            branch_template: "test-{run_id}".to_string(),
            base_branch: Some("main".to_string()),
            workspace_root: None,
            project_subdir: None,
            artifact_path_base: None,
            diff_path_base: None,
            diff_path_normalization: None,
        },
        guard_limits: GuardLimits {
            max_iterations: Some(3),
            max_file_changes: Some(50),
            max_tokens: Some(10000),
            max_cost: Some(10.0),
        },
        variables: std::collections::HashMap::new(),
        discovery: None,
        command_manifest: None,
    };

    let instance = WorkflowInstance::create(workflow_type, config);
    let registry = crate::engine::executor::ExecutorRegistry::with_defaults();
    let runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");
    assert!(!runner.run_id().is_empty());
}

/// Build a minimal two-step noop workflow (step1 -> step2, both terminal-ish)
/// for exercising the trace export seam without network or `gh`.
/// @plan:PLAN-LUTHER-ISSUE-19-SMOKE-REPLAY
fn seam_test_instance() -> WorkflowInstance {
    use crate::workflow::schema::{
        GuardLimits, RepoConfig, RuntimeConfig, StepDef, TransitionDef, WorkflowConfig,
        WorkflowType,
    };
    let step = |id: &str| StepDef {
        step_id: id.to_string(),
        step_type: "noop".to_string(),
        description: None,
        parameters: None,
        produces: None,
        consumes: None,
        terminal: None,
    };
    let workflow_type = WorkflowType {
        workflow_type_id: "seam-test".to_string(),
        steps: vec![step("step1"), step("step2")],
        transitions: vec![TransitionDef {
            from: "step1".to_string(),
            to: "step2".to_string(),
            condition: None,
            max_iterations: None,
        }],
        guards: Default::default(),
    };
    let config = WorkflowConfig {
        config_id: "seam-config".to_string(),
        workflow_type_id: "seam-test".to_string(),
        runtime: RuntimeConfig {
            timeout_seconds: 3600,
            max_retries: 3,
            parallel_steps: None,
            log_level: None,
        },
        repo: RepoConfig {
            workspace_strategy: "temp".to_string(),
            branch_template: "test-{run_id}".to_string(),
            base_branch: Some("main".to_string()),
            workspace_root: None,
            project_subdir: None,
            artifact_path_base: None,
            diff_path_base: None,
            diff_path_normalization: None,
        },
        guard_limits: GuardLimits {
            max_iterations: Some(3),
            max_file_changes: Some(50),
            max_tokens: Some(10000),
            max_cost: Some(10.0),
        },
        variables: std::collections::HashMap::new(),
        discovery: None,
        command_manifest: None,
    };
    WorkflowInstance::create(workflow_type, config)
}

#[test]
fn export_trace_matches_executed_sequence_and_outcome() {
    // @plan:PLAN-LUTHER-ISSUE-19-SMOKE-REPLAY
    // @requirement:REQ-SMOKE-REPLAY-001
    let instance = seam_test_instance();
    let registry = crate::engine::executor::ExecutorRegistry::with_defaults();
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let outcome = runner.run().expect("run should succeed");
    assert!(matches!(outcome, RunOutcome::Success));

    let trace = runner
        .export_trace(&outcome)
        .expect("export_trace should succeed");
    assert_eq!(trace.workflow_type_id, "seam-test");
    assert_eq!(trace.config_id, "seam-config");
    assert_eq!(trace.run_id, runner.run_id());
    let steps: Vec<&str> = trace.events.iter().map(|e| e.step_id.as_str()).collect();
    assert_eq!(steps, vec!["step1", "step2"]);
    assert!(trace.events.iter().all(|e| e.outcome == "success"));
    assert!(trace.final_outcome.matches_run_outcome(&outcome));
}
