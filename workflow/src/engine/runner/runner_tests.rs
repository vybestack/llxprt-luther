#[test]
fn public_failure_reason_uses_typed_category_when_safe() {
    let outcome = StepOutcome::Fatal;
    let reason = EngineRunner::public_failure_reason("run_llxprt", &outcome, Some("agent_failure"));
    assert_eq!(reason, "agent_failure (fatal at run_llxprt)");
    assert!(!reason.contains("diagnostic"));
}

#[test]
fn public_failure_reason_falls_back_to_outcome_when_no_category() {
    let reason = EngineRunner::public_failure_reason("run_llxprt", &StepOutcome::Fatal, None);
    assert_eq!(reason, "fatal outcome at run_llxprt");
}

#[test]
fn public_failure_reason_falls_back_to_outcome_for_unsafe_category() {
    // Raw diagnostic-style text (uppercase, spaces, special chars) must never
    // be retained as the public failure reason.
    let reason = EngineRunner::public_failure_reason(
        "run_llxprt",
        &StepOutcome::Fatal,
        Some("Error: token=abc123 access_token=def456"),
    );
    assert_eq!(reason, "fatal outcome at run_llxprt");
}

#[test]
fn public_failure_reason_never_carries_raw_secret_text() {
    // Bearer header in raw diagnostic - must not survive into public reason.
    let reason = EngineRunner::public_failure_reason(
        "run_llxprt",
        &StepOutcome::Fatal,
        Some("Authorization: Bearer s3cr3t_b34r3r_t0k3n"),
    );
    assert_eq!(reason, "fatal outcome at run_llxprt");
    assert!(!reason.contains("s3cr3t"));
}

#[test]
fn safe_failure_category_accepts_known_snake_case_values() {
    for category in [
        "process_error",
        "agent_failure",
        "no_diff",
        "idle_timeout",
        "timeout",
        "push_failure",
        "validation_failure",
    ] {
        assert!(
            EngineRunner::is_safe_failure_category(category),
            "{category} should be a safe category"
        );
    }
}

#[test]
fn safe_failure_category_rejects_uppercase_and_whitespace() {
    assert!(!EngineRunner::is_safe_failure_category("Agent_Failure"));
    assert!(!EngineRunner::is_safe_failure_category("agent failure"));
    assert!(!EngineRunner::is_safe_failure_category(" agent_failure"));
    assert!(!EngineRunner::is_safe_failure_category("agent_failure "));
    assert!(!EngineRunner::is_safe_failure_category("agent	failure"));
}

#[test]
fn safe_failure_category_rejects_special_characters() {
    assert!(!EngineRunner::is_safe_failure_category("token=abc123"));
    assert!(!EngineRunner::is_safe_failure_category("reason: detail"));
    assert!(!EngineRunner::is_safe_failure_category("\"quoted\""));
    assert!(!EngineRunner::is_safe_failure_category("{json: value}"));
    assert!(!EngineRunner::is_safe_failure_category("user@host"));
    assert!(!EngineRunner::is_safe_failure_category("query?param=1"));
}

#[test]
fn safe_failure_category_rejects_url_and_credential_patterns() {
    // URL userinfo credentials.
    assert!(!EngineRunner::is_safe_failure_category(
        "https://user:pass@host/path"
    ));
    // URL query credentials.
    assert!(!EngineRunner::is_safe_failure_category(
        "api_key=secret&token=abc"
    ));
    // JSON credential field.
    assert!(!EngineRunner::is_safe_failure_category(
        "\"password\":\"secret\""
    ));
}

#[test]
fn safe_failure_category_rejects_non_ascii_unicode() {
    assert!(!EngineRunner::is_safe_failure_category("агент_failure"));
    assert!(!EngineRunner::is_safe_failure_category("failure_失敗"));
    assert!(!EngineRunner::is_safe_failure_category("token＝secret"));
    assert!(!EngineRunner::is_safe_failure_category("café_error"));
}

#[test]
fn safe_failure_category_rejects_leading_digits() {
    assert!(!EngineRunner::is_safe_failure_category("1_error"));
    assert!(!EngineRunner::is_safe_failure_category("9fatal"));
}

#[test]
fn safe_failure_category_rejects_empty_and_unlisted_snake_case() {
    // The allowlist is explicit: a structurally valid snake_case value that is
    // not in the allowlist must be rejected. This prevents a secret like
    // `bearer_abc123` (which would pass a structural snake_case check) from
    // being persisted as a durable failure category.
    assert!(!EngineRunner::is_safe_failure_category(""));
    assert!(!EngineRunner::is_safe_failure_category("bearer_abc123"));
    assert!(!EngineRunner::is_safe_failure_category("some_new_category"));
    assert!(
        !EngineRunner::is_safe_failure_category(&"a".repeat(32)),
        "even a 32-char snake_case value not in the allowlist must be rejected"
    );
}

#[test]
fn public_failure_reason_rejects_secret_shaped_snake_case_category() {
    // The explicit allowlist must reject a secret that happens to be valid
    // snake_case (lowercase + underscores), which the prior structural check
    // would have accepted. The public failure reason must fall back to the
    // generic outcome label, never the secret-shaped category.
    let secret_category = "bearer_s3cr3t_t0k3n_value";
    // Sanity: this would have passed the old structural check.
    assert!(secret_category
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'));
    // But the explicit allowlist rejects it.
    assert!(!EngineRunner::is_safe_failure_category(secret_category));

    let reason = EngineRunner::public_failure_reason(
        "run_llxprt",
        &StepOutcome::Fatal,
        Some(secret_category),
    );
    assert_eq!(reason, "fatal outcome at run_llxprt");
    assert!(!reason.contains("bearer"));
    assert!(!reason.contains("s3cr3t"));
    assert!(!reason.contains("t0k3n"));
}

use super::support::preview_for_log;
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
            diff_path_normalization: crate::workflow::schema::DiffPathNormalization::RepoRelative,
        },
        guard_limits: crate::workflow::schema::GuardLimits {
            max_iterations: Some(3),
            max_file_changes: Some(50),
            max_tokens: Some(10000),
            max_cost: Some(10.0),
        },
        variables: std::collections::HashMap::new(),
        discovery: None,
        parent_orchestration: Default::default(),
        command_manifest: None,
        target_profile: None,
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
            diff_path_normalization: crate::workflow::schema::DiffPathNormalization::RepoRelative,
        },
        guard_limits: GuardLimits {
            max_iterations: Some(3),
            max_file_changes: Some(50),
            max_tokens: Some(10000),
            max_cost: Some(10.0),
        },
        variables: std::collections::HashMap::new(),
        discovery: None,
        parent_orchestration: Default::default(),
        command_manifest: None,
        target_profile: None,
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
            diff_path_normalization: crate::workflow::schema::DiffPathNormalization::RepoRelative,
        },
        guard_limits: GuardLimits {
            max_iterations: Some(3),
            max_file_changes: Some(50),
            max_tokens: Some(10000),
            max_cost: Some(10.0),
        },
        variables: std::collections::HashMap::new(),
        discovery: None,
        parent_orchestration: Default::default(),
        command_manifest: None,
        target_profile: None,
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

// ---------------------------------------------------------------------------
// Issue 158 finding 1/2: failure_cleanup lease operations must require the
// immutable daemon_managed_claim authority, and verify_failure_cleanup_workspace
// must use the immutable StepContext.work_dir() accessor, never the mutable
// context.get("work_dir") that a shell step can overwrite.
// ---------------------------------------------------------------------------

use crate::engine::transition::StepOutcome;
use crate::persistence::leases::{
    get_lease_for_issue, init_leases_table, try_claim, update_lease_status, LeaseStatus,
};
use crate::persistence::{RunMetadata, RunStatus};

/// Build a minimal workflow instance with a single `failure_cleanup` terminal
/// step and a transition into it from a non-terminal step.
fn failure_cleanup_test_instance() -> WorkflowInstance {
    use crate::workflow::schema::*;
    let workflow_type = WorkflowType {
        workflow_type_id: "fc-test".to_string(),
        steps: vec![
            StepDef {
                step_id: "work_step".to_string(),
                step_type: "noop".to_string(),
                description: None,
                parameters: None,
                produces: None,
                consumes: None,
                terminal: None,
            },
            StepDef {
                step_id: "abandon_and_log".to_string(),
                step_type: "failure_cleanup".to_string(),
                description: None,
                parameters: None,
                produces: None,
                consumes: None,
                terminal: Some(true),
            },
        ],
        transitions: vec![TransitionDef {
            from: "work_step".to_string(),
            to: "abandon_and_log".to_string(),
            condition: Some("fixable".to_string()),
            max_iterations: None,
        }],
        guards: Default::default(),
    };
    let config = test_workflow_config("fc-test");
    WorkflowInstance::create(workflow_type, config)
}

/// Initialize leases + runs schema on a file-based connection.
fn init_failure_cleanup_schema(conn: &Connection) {
    init_leases_table(conn).unwrap();
    crate::persistence::sqlite::init_runs_schema_serialized(conn).unwrap();
}

#[test]
fn protect_failure_cleanup_lease_is_noop_for_non_daemon_run() {
    // Issue 158 finding 1: a non-daemon (one-shot CLI) run has no
    // daemon_managed_claim authority and must never touch a lease. Even if a
    // matching lease exists for the issue, the non-daemon run must not
    // advance it. The daemon_managed flag is read from the immutable runtime
    // provenance accessor.
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    // Seed schema + a Running lease in the SAME db the runner will use.
    let conn = Connection::open(&db_path).unwrap();
    init_failure_cleanup_schema(&conn);
    let lease = try_claim(&conn, "o/r", 42, "cfg").unwrap().unwrap();
    update_lease_status(
        &conn,
        &lease.lease_id,
        LeaseStatus::Running,
        Some("run-non-daemon"),
    )
    .unwrap();
    drop(conn);

    let instance = failure_cleanup_test_instance();
    let registry = crate::engine::executor::ExecutorRegistry::with_defaults();
    // Non-daemon RunContext (daemon_managed = false).
    let run_context = RunContext {
        daemon_managed: false,
        repository: Some("o/r".to_string()),
        issue_number: Some(42),
        ..Default::default()
    };
    let runner = EngineRunner::with_db_path_and_context(instance, registry, &db_path, run_context)
        .expect("runner");

    // Build metadata with a matching repository/issue so the lease WOULD be
    // found if the guard were absent.
    let mut metadata = RunMetadata::new(&runner.instance.run_id, "fc-test", "fc-test-config");
    metadata.repository = Some("o/r".to_string());
    metadata.issue_number = Some(42);
    metadata.status = RunStatus::Failed;

    // Call protect_failure_cleanup_lease on the same DB.
    let conn2 = Connection::open(&db_path).unwrap();
    let result = runner.protect_failure_cleanup_lease(&conn2, &metadata);
    assert!(
        result.is_ok(),
        "non-daemon run must not error on protect_failure_cleanup_lease"
    );
    // The lease must remain Running (untouched by the non-daemon run).
    let lease_after = get_lease_for_issue(&conn2, "o/r", 42).unwrap().unwrap();
    assert_eq!(
        lease_after.status,
        LeaseStatus::Running,
        "non-daemon run must not advance the lease"
    );
}

#[test]
fn protect_failure_cleanup_lease_advances_for_daemon_run() {
    // Issue 158 finding 1: a daemon-managed run DOES have lease authority.
    // protect_failure_cleanup_lease must advance the matching lease to
    // CleanupAbandoned.
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let conn = Connection::open(&db_path).unwrap();
    init_failure_cleanup_schema(&conn);
    let lease = try_claim(&conn, "o/r", 43, "cfg").unwrap().unwrap();
    drop(conn);

    let instance = failure_cleanup_test_instance();
    let registry = crate::engine::executor::ExecutorRegistry::with_defaults();
    let run_id = instance.run_id.clone();
    let run_context = RunContext {
        daemon_managed: true,
        repository: Some("o/r".to_string()),
        issue_number: Some(43),
        ..Default::default()
    };
    let runner = EngineRunner::with_db_path_and_context(instance, registry, &db_path, run_context)
        .expect("runner");

    // Set the lease to Running for this run_id.
    let conn2 = Connection::open(&db_path).unwrap();
    update_lease_status(&conn2, &lease.lease_id, LeaseStatus::Running, Some(&run_id)).unwrap();

    let mut metadata = RunMetadata::new(&run_id, "fc-test", "fc-test-config");
    metadata.repository = Some("o/r".to_string());
    metadata.issue_number = Some(43);
    metadata.status = RunStatus::Failed;

    let result = runner.protect_failure_cleanup_lease(&conn2, &metadata);
    assert!(result.is_ok(), "daemon run must protect lease: {result:?}");
    let lease_after = get_lease_for_issue(&conn2, "o/r", 43).unwrap().unwrap();
    assert_eq!(
        lease_after.status,
        LeaseStatus::CleanupAbandoned,
        "daemon run must advance lease to CleanupAbandoned"
    );
}

#[test]
fn verify_failure_cleanup_workspace_uses_immutable_work_dir_not_context_variable() {
    // Issue 158 finding 1: verify_failure_cleanup_workspace must read the
    // workspace from the immutable StepContext.work_dir() accessor, NOT from
    // context.get("work_dir"). A shell step can overwrite the mutable
    // "work_dir" context variable to redirect cleanup verification to a
    // different workspace. The immutable accessor prevents this shadowing.
    //
    // We simulate a shell step overwriting "work_dir" in the context
    // variables, then verify that verify_failure_cleanup_workspace consults
    // the immutable work_dir (the real workspace), not the shadowed variable.
    let real_workspace = tempfile::tempdir().unwrap();
    let shadowed_workspace = tempfile::tempdir().unwrap();
    let db_path = real_workspace.path().join("test.db");

    let instance = failure_cleanup_test_instance();
    let registry = crate::engine::executor::ExecutorRegistry::with_defaults();
    let run_context = RunContext {
        daemon_managed: true,
        workspace_path: Some(real_workspace.path().to_string_lossy().to_string()),
        ..Default::default()
    };
    let mut runner =
        EngineRunner::with_db_path_and_context(instance, registry, &db_path, run_context)
            .expect("runner");

    // Simulate a shell step overwriting the mutable "work_dir" variable to
    // point at a different (shadowed) workspace.
    runner
        .context
        .set("work_dir", shadowed_workspace.path().to_str().unwrap());

    // The immutable work_dir() must still point at the real workspace.
    assert_eq!(
        runner.context.work_dir(),
        real_workspace.path(),
        "immutable work_dir() must not be affected by context variable shadowing"
    );
    // The mutable variable IS shadowed (this is the vulnerability that the
    // immutable accessor closes).
    assert_eq!(
        runner.context.get("work_dir").map(String::as_str),
        Some(shadowed_workspace.path().to_str().unwrap()),
        "the mutable variable is shadowed; verify uses the immutable accessor"
    );

    // verify_failure_cleanup_workspace with a Fixable outcome routing into
    // the failure_cleanup step should consult the immutable work_dir (the
    // real workspace, which has no ownership evidence). For a daemon-managed
    // run, this must NOT return NotApplicable just because the shadowed
    // variable points elsewhere — it must use the real workspace.
    let outcome = StepOutcome::Fixable;
    let result = runner.verify_failure_cleanup_workspace(&outcome, Some("abandon_and_log"));
    // The real workspace has no ownership evidence. For a daemon-managed run,
    // ownership IS required, so this should fail (OwnershipFailure), proving
    // the immutable accessor was used. If the shadowed variable were used
    // instead, the result would differ based on the shadowed path.
    assert!(
        matches!(result, Err(EngineError::OwnershipFailure(_))),
        "verify_failure_cleanup_workspace must use the immutable work_dir; \
         a daemon-managed run with no ownership evidence at the real workspace \
         must fail, got: {result:?}"
    );
}
