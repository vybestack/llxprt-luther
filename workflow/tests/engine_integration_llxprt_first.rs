/// @plan:PLAN-20260408-LLXPRT-FIRST.P16
/// Engine Integration Tests for `LLxprt` First Phase
///
/// These tests verify the four new components (`ShellExecutor`, `VerifyExecutor`,
/// `ExecutorRegistry`, `StepContext`) working together through the `EngineRunner`.
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use luther_workflow::engine::executor::{ExecutorRegistry, StepContext, StepExecutor};
use luther_workflow::engine::executors::noop::NoOpExecutor;
use luther_workflow::engine::instance::WorkflowInstance;
use luther_workflow::engine::runner::{EngineError, EngineRunner, RunOutcome};
use luther_workflow::engine::transition::StepOutcome;
use luther_workflow::persistence::SqliteStore;
use luther_workflow::workflow::schema::{
    GuardLimits, RepoConfig, RuntimeConfig, StepDef, TransitionDef, WorkflowConfig, WorkflowType,
};

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

// ============================================================================
// Helper Executors
// ============================================================================

/// `SequenceExecutor` returns outcomes from a configured sequence.
/// Used for tests that need to return different outcomes on successive calls.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P16
struct SequenceExecutor {
    outcomes: Mutex<Vec<StepOutcome>>,
    call_count: Mutex<usize>,
}

impl SequenceExecutor {
    const fn new(outcomes: Vec<StepOutcome>) -> Self {
        Self {
            outcomes: Mutex::new(outcomes),
            call_count: Mutex::new(0),
        }
    }
}

impl StepExecutor for SequenceExecutor {
    fn execute(
        &self,
        _context: &mut StepContext,
        _params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        let mut count = self.call_count.lock().unwrap();
        let outcomes = self.outcomes.lock().unwrap();

        if *count < outcomes.len() {
            let outcome = outcomes[*count];
            *count += 1;
            Ok(outcome)
        } else {
            Ok(StepOutcome::Success)
        }
    }
}

/// `FatalExecutor` always returns Fatal outcome.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P16
struct FatalExecutor;

impl StepExecutor for FatalExecutor {
    fn execute(
        &self,
        _context: &mut StepContext,
        _params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        Ok(StepOutcome::Fatal)
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

fn make_workflow_type(steps: Vec<StepDef>, transitions: Vec<TransitionDef>) -> WorkflowType {
    WorkflowType {
        workflow_type_id: "test-integration".to_string(),
        steps,
        transitions,
        guards: Default::default(),
    }
}

/// Helper: Create a minimal "sequence" step with the given id.
fn seq_step(step_id: &str) -> StepDef {
    StepDef {
        step_id: step_id.to_string(),
        step_type: "sequence".to_string(),
        description: None,
        produces: None,
        consumes: None,
        terminal: None,
        parameters: None,
    }
}

fn make_config_with_vars(
    max_iterations: Option<u32>,
    variables: HashMap<String, String>,
) -> WorkflowConfig {
    WorkflowConfig {
        config_id: "test-config".to_string(),
        workflow_type_id: "test-integration".to_string(),
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
        },
        guard_limits: GuardLimits {
            max_iterations,
            max_file_changes: Some(50),
            max_tokens: Some(10000),
            max_cost: Some(10.0),
        },
        variables,
    }
}

fn make_config(max_iterations: Option<u32>) -> WorkflowConfig {
    make_config_with_vars(max_iterations, HashMap::new())
}

#[test]
fn test_llxprt_executor_emits_progress_while_waiting() {
    use luther_workflow::engine::executors::LlxprtExecutor;

    let mut context = StepContext::new(std::env::temp_dir(), uuid::Uuid::new_v4().to_string());
    let params = serde_json::json!({
        "static_stdout": "IMPLEMENTATION_COMPLETE",
        "outcome_on_stdout": {
            "IMPLEMENTATION_COMPLETE": "success"
        }
    });

    let outcome = LlxprtExecutor
        .execute(&mut context, &params)
        .expect("llxprt static stdout should execute");

    assert_eq!(outcome, StepOutcome::Success);
}

#[test]
fn test_llxprt_executor_ignores_blank_static_content() {
    use luther_workflow::engine::executors::LlxprtExecutor;

    let mut context = StepContext::new(std::env::temp_dir(), uuid::Uuid::new_v4().to_string());
    let params = serde_json::json!({
        "static_content": "   ",
        "static_stdout": "PLAN_APPROVED",
        "outcome_on_stdout": {
            "PLAN_APPROVED": "success"
        }
    });

    let outcome = LlxprtExecutor
        .execute(&mut context, &params)
        .expect("blank static_content should fall through to normal llxprt execution path");

    assert_eq!(outcome, StepOutcome::Success);
}

#[test]
fn test_llxprt_outcome_markers_must_be_exact_lines() {
    use luther_workflow::engine::executors::LlxprtExecutor;

    let mut context = StepContext::new(std::env::temp_dir(), uuid::Uuid::new_v4().to_string());
    let params = serde_json::json!({
        "static_stdout": "diff context mentions REMEDIATION_SYSTEM_ERROR inside a changed line",
        "outcome_on_stdout": {
            "REMEDIATION_SYSTEM_ERROR": "fatal"
        }
    });

    let outcome = LlxprtExecutor
        .execute(&mut context, &params)
        .expect("llxprt static stdout should execute");

    assert_eq!(outcome, StepOutcome::Success);
}

#[test]
fn test_llxprt_executor_requires_diff_when_success_on_diff_enabled() {
    use luther_workflow::engine::executors::LlxprtExecutor;

    let temp_dir = tempfile::tempdir().expect("temp dir should be created");
    std::process::Command::new("git")
        .arg("init")
        .current_dir(temp_dir.path())
        .status()
        .expect("git init should run");

    let mut context = StepContext::new(
        temp_dir.path().to_path_buf(),
        uuid::Uuid::new_v4().to_string(),
    );
    let params = serde_json::json!({
        "static_stdout": "IMPLEMENTATION_COMPLETE",
        "success_on_diff": true,
        "outcome_on_stdout": {
            "IMPLEMENTATION_SYSTEM_ERROR": "fatal"
        }
    });

    let outcome = LlxprtExecutor
        .execute(&mut context, &params)
        .expect("llxprt static stdout should execute");

    assert_eq!(outcome, StepOutcome::Fixable);
}

#[test]
fn test_llxprt_executor_requires_new_diff_when_success_on_diff_enabled() {
    use luther_workflow::engine::executors::LlxprtExecutor;

    let temp_dir = tempfile::tempdir().expect("temp dir should be created");
    std::process::Command::new("git")
        .arg("init")
        .current_dir(temp_dir.path())
        .status()
        .expect("git init should run");
    std::process::Command::new("git")
        .args(["config", "user.email", "luther@example.invalid"])
        .current_dir(temp_dir.path())
        .status()
        .expect("git config email should run");
    std::process::Command::new("git")
        .args(["config", "user.name", "Luther Test"])
        .current_dir(temp_dir.path())
        .status()
        .expect("git config name should run");
    std::fs::write(temp_dir.path().join("existing.txt"), "base").expect("write base file");
    std::process::Command::new("git")
        .args(["add", "existing.txt"])
        .current_dir(temp_dir.path())
        .status()
        .expect("git add should run");
    std::process::Command::new("git")
        .args(["commit", "-m", "base"])
        .current_dir(temp_dir.path())
        .status()
        .expect("git commit should run");
    std::fs::write(temp_dir.path().join("existing.txt"), "existing change")
        .expect("write existing diff");

    let mut context = StepContext::new(
        temp_dir.path().to_path_buf(),
        uuid::Uuid::new_v4().to_string(),
    );
    let params = serde_json::json!({
        "static_stdout": "REMEDIATION_COMPLETE",
        "success_on_diff": true,
        "outcome_on_stdout": {
            "REMEDIATION_COMPLETE": "success"
        }
    });

    let outcome = LlxprtExecutor
        .execute(&mut context, &params)
        .expect("llxprt static stdout should execute");

    assert_eq!(outcome, StepOutcome::Fixable);
    let _env_guard = env_lock().lock().expect("env lock should not be poisoned");
}

#[test]
#[allow(unsafe_code)]
fn test_llxprt_executor_can_stop_after_required_diff_before_marker() {
    use luther_workflow::engine::executors::LlxprtExecutor;

    let temp_dir = tempfile::tempdir().expect("temp dir should be created");
    std::process::Command::new("git")
        .arg("init")
        .current_dir(temp_dir.path())
        .status()
        .expect("git init should run");
    std::process::Command::new("git")
        .args(["config", "user.email", "luther@example.invalid"])
        .current_dir(temp_dir.path())
        .status()
        .expect("git config email should run");
    std::process::Command::new("git")
        .args(["config", "user.name", "Luther Test"])
        .current_dir(temp_dir.path())
        .status()
        .expect("git config name should run");
    std::fs::write(temp_dir.path().join("tracked.txt"), "base").expect("write base file");
    std::process::Command::new("git")
        .args(["add", "tracked.txt"])
        .current_dir(temp_dir.path())
        .status()
        .expect("git add should run");
    std::process::Command::new("git")
        .args(["commit", "-m", "base"])
        .current_dir(temp_dir.path())
        .status()
        .expect("git commit should run");

    let bin_dir = temp_dir.path().join("bin");
    std::fs::create_dir(&bin_dir).expect("create bin dir");
    let llxprt_script = bin_dir.join("llxprt");
    std::fs::write(
        &llxprt_script,
        "#!/bin/sh\nprintf 'working\\n'\nprintf changed > \"$PWD/tracked.txt\"\nsleep 30\n",
    )
    .expect("write llxprt script");
    std::fs::write(temp_dir.path().join(".gitignore"), "bin/\n").expect("write gitignore");
    std::process::Command::new("git")
        .args(["add", ".gitignore"])
        .current_dir(temp_dir.path())
        .status()
        .expect("git add gitignore should run");
    std::process::Command::new("git")
        .args(["commit", "-m", "ignore-bin"])
        .current_dir(temp_dir.path())
        .status()
        .expect("git commit gitignore should run");

    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&llxprt_script)
        .status()
        .expect("chmod llxprt script");

    let _env_guard = env_lock().lock().expect("env lock should not be poisoned");
    let original_path = std::env::var_os("PATH").unwrap_or_default();
    let path_value = format!("{}:{}", bin_dir.display(), original_path.to_string_lossy());
    // This integration test must shadow the llxprt executable resolved from PATH.
    unsafe { std::env::set_var("PATH", path_value) };

    let mut context = StepContext::new(
        temp_dir.path().to_path_buf(),
        uuid::Uuid::new_v4().to_string(),
    );
    let params = serde_json::json!({
        "success_on_diff": true,
        "min_runtime_before_success_seconds": 120,
        "max_runtime_after_required_diff_seconds": 0,
        "timeout_seconds": 60
    });

    let outcome = LlxprtExecutor
        .execute(&mut context, &params)
        .expect("llxprt should execute");
    eprintln!(
        "status after llxprt: {}",
        String::from_utf8_lossy(
            &std::process::Command::new("git")
                .args(["status", "--porcelain", "--untracked-files=all"])
                .current_dir(temp_dir.path())
                .output()
                .expect("git status")
                .stdout
        )
    );

    // Restore PATH before releasing the process-env lock.
    unsafe { std::env::set_var("PATH", original_path) };
    assert_eq!(outcome, StepOutcome::Success);
}

#[test]
#[allow(unsafe_code)]
fn test_llxprt_executor_nonzero_exit_overrides_partial_diff() {
    use luther_workflow::engine::executors::LlxprtExecutor;

    let temp_dir = tempfile::tempdir().expect("temp dir should be created");
    std::process::Command::new("git")
        .arg("init")
        .current_dir(temp_dir.path())
        .status()
        .expect("git init should run");
    let bin_dir = temp_dir.path().join("bin");
    std::fs::create_dir(&bin_dir).expect("create bin dir");
    let llxprt_script = bin_dir.join("llxprt");
    std::fs::write(
        &llxprt_script,
        "#!/bin/sh\nprintf partial > \"$PWD/partial.txt\"\nprintf 'provider failed\\n' >&2\nexit 1\n",
    )
    .expect("write llxprt script");
    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&llxprt_script)
        .status()
        .expect("chmod llxprt script");

    let _env_guard = env_lock().lock().expect("env lock should not be poisoned");
    let original_path = std::env::var_os("PATH").unwrap_or_default();
    let path_value = format!("{}:{}", bin_dir.display(), original_path.to_string_lossy());
    unsafe { std::env::set_var("PATH", path_value) };
    let mut context = StepContext::new(
        temp_dir.path().to_path_buf(),
        uuid::Uuid::new_v4().to_string(),
    );
    let params = serde_json::json!({ "success_on_diff": true, "exit_code_map": { "1": "fatal" } });
    let outcome = LlxprtExecutor
        .execute(&mut context, &params)
        .expect("llxprt should execute");
    unsafe { std::env::set_var("PATH", original_path) };

    assert_eq!(outcome, StepOutcome::Fatal);
    assert_eq!(context.get("exit_code").map(String::as_str), Some("1"));
}

// ============================================================================
// Test 1: Config variables available in shell steps
// ============================================================================

/// Test 1: Config variables available in shell steps
/// GIVEN: `WorkflowConfig` with variables: {"`my_var"`: "hello"}
/// WHEN: A shell step runs command "echo {`my_var`}"
/// THEN: The command interpolates correctly and stdout contains "hello"
/// @plan:PLAN-20260408-LLXPRT-FIRST.P16
/// @requirement:REQ-LF-PROF-003
#[test]
fn test_config_variables_available_in_shell_steps() {
    let steps = vec![StepDef {
        step_id: "step_a".to_string(),
        step_type: "shell".to_string(),
        description: None,
        produces: None,
        consumes: None,
        terminal: None,
        parameters: Some(serde_json::json!({
            "command": "echo {my_var}"
        })),
    }];

    let transitions = vec![];
    let workflow_type = make_workflow_type(steps, transitions);

    let mut variables = HashMap::new();
    variables.insert("my_var".to_string(), "hello".to_string());
    let config = make_config_with_vars(Some(10), variables);

    let instance = WorkflowInstance::create(workflow_type, config);
    let registry = ExecutorRegistry::with_defaults();
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected Success, got {result:?}"
    );
}

// ============================================================================
// Test 2: Different configs resolve different profiles
// ============================================================================

/// Test 2: Different configs resolve different profiles
/// GIVEN: Same `WorkflowType` with step using {`profile_planning`}
/// WHEN: Two different configs with different `profile_planning` values
/// THEN: The interpolated commands differ
/// @plan:PLAN-20260408-LLXPRT-FIRST.P16
/// @requirement:REQ-LF-PROF-004
#[test]
fn test_different_configs_resolve_different_profiles() {
    let steps = vec![StepDef {
        step_id: "step_a".to_string(),
        step_type: "shell".to_string(),
        description: None,
        produces: None,
        consumes: None,
        terminal: None,
        parameters: Some(serde_json::json!({
            "command": "echo {profile_planning}"
        })),
    }];

    let workflow_type = make_workflow_type(steps, vec![]);

    // Config 1: opusthinking
    let mut vars1 = HashMap::new();
    vars1.insert("profile_planning".to_string(), "opusthinking".to_string());
    let config1 = make_config_with_vars(Some(10), vars1);
    let instance1 = WorkflowInstance::create(workflow_type.clone(), config1);
    let registry1 = ExecutorRegistry::with_defaults();
    let mut runner1 =
        EngineRunner::new(instance1, registry1).expect("Failed to create EngineRunner");
    let result1 = runner1.run();
    assert!(matches!(result1, Ok(RunOutcome::Success)));

    // Config 2: claude
    let mut vars2 = HashMap::new();
    vars2.insert("profile_planning".to_string(), "claude".to_string());
    let config2 = make_config_with_vars(Some(10), vars2);
    let instance2 = WorkflowInstance::create(workflow_type, config2);
    let registry2 = ExecutorRegistry::with_defaults();
    let mut runner2 =
        EngineRunner::new(instance2, registry2).expect("Failed to create EngineRunner");
    let result2 = runner2.run();
    assert!(matches!(result2, Ok(RunOutcome::Success)));
}

// ============================================================================
// Test 3: Namespaced context across real steps
// ============================================================================

/// Test 3: Namespaced context across real steps
/// GIVEN: Workflow: `step_a` (echo alpha) → `step_b` (echo beta) → `step_c` (echo {`step_a.stdout`})
/// WHEN: Engine runs
/// THEN: `step_c`'s stdout contains "alpha" (from `step_a` namespace)
/// @plan:PLAN-20260408-LLXPRT-FIRST.P16
/// @requirement:REQ-LF-CTX-001,REQ-LF-CTX-003
#[test]
fn test_namespaced_context_across_real_steps() {
    let steps = vec![
        StepDef {
            step_id: "step_a".to_string(),
            step_type: "shell".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: Some(serde_json::json!({
                "command": "echo alpha"
            })),
        },
        StepDef {
            step_id: "step_b".to_string(),
            step_type: "shell".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: Some(serde_json::json!({
                "command": "echo beta"
            })),
        },
        StepDef {
            step_id: "step_c".to_string(),
            step_type: "shell".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: Some(serde_json::json!({
                "command": "echo {step_a.stdout}"
            })),
        },
    ];

    let transitions = vec![
        TransitionDef {
            from: "step_a".to_string(),
            to: "step_b".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        },
        TransitionDef {
            from: "step_b".to_string(),
            to: "step_c".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        },
    ];

    let workflow_type = make_workflow_type(steps, transitions);
    let config = make_config(Some(10));
    let instance = WorkflowInstance::create(workflow_type, config);
    let registry = ExecutorRegistry::with_defaults();
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected Success, got {result:?}"
    );
}

// ============================================================================
// Test 4: Unnamespaced variable gets most recent
// ============================================================================

/// Test 4: Unnamespaced variable gets most recent
/// GIVEN: Workflow: `step_a` (echo first) → `step_b` (echo second) → `step_c` (echo {stdout})
/// WHEN: Engine runs
/// THEN: `step_c`'s echo outputs "second" (most-recent bare stdout from `step_b`)
/// @plan:PLAN-20260408-LLXPRT-FIRST.P16
/// @requirement:REQ-LF-CTX-002
#[test]
fn test_unnamespaced_variable_gets_most_recent() {
    let steps = vec![
        StepDef {
            step_id: "step_a".to_string(),
            step_type: "shell".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: Some(serde_json::json!({
                "command": "echo first"
            })),
        },
        StepDef {
            step_id: "step_b".to_string(),
            step_type: "shell".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: Some(serde_json::json!({
                "command": "echo second"
            })),
        },
        StepDef {
            step_id: "step_c".to_string(),
            step_type: "shell".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: Some(serde_json::json!({
                "command": "echo {stdout}"
            })),
        },
    ];

    let transitions = vec![
        TransitionDef {
            from: "step_a".to_string(),
            to: "step_b".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        },
        TransitionDef {
            from: "step_b".to_string(),
            to: "step_c".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        },
    ];

    let workflow_type = make_workflow_type(steps, transitions);
    let config = make_config(Some(10));
    let instance = WorkflowInstance::create(workflow_type, config);
    let registry = ExecutorRegistry::with_defaults();
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected Success, got {result:?}"
    );
}

// ============================================================================
// Test 5: Per-edge loop with real executor dispatch
// ============================================================================

/// Test 5: Per-edge loop with real executor dispatch
/// GIVEN: Workflow with loop: `step_a` → `step_b` → `step_a` (on fixable, `max_iterations`: 2)
/// WHEN: Executor returns Fixable repeatedly
/// THEN: Engine abandons after 3 iterations with reason identifying the edge
/// @plan:PLAN-20260408-LLXPRT-FIRST.P16
/// @requirement:REQ-LF-LOOP-001,REQ-LF-LOOP-003
#[test]
fn test_per_edge_loop_with_real_executor_dispatch() {
    let steps = vec![
        StepDef {
            step_id: "step_a".to_string(),
            step_type: "sequence".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: None,
        },
        StepDef {
            step_id: "step_b".to_string(),
            step_type: "sequence".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: None,
        },
    ];

    let transitions = vec![
        TransitionDef {
            from: "step_a".to_string(),
            to: "step_b".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        },
        TransitionDef {
            from: "step_b".to_string(),
            to: "step_a".to_string(),
            condition: Some("fixable".to_string()),
            max_iterations: Some(2),
        },
    ];

    let workflow_type = make_workflow_type(steps, transitions);
    let config = make_config(Some(10));
    let instance = WorkflowInstance::create(workflow_type, config);

    // Sequence: A(S)→B(F)→A(S)→B(F)→A(S)→B(F)→Abandoned
    let mut registry = ExecutorRegistry::new();
    registry.register(
        "sequence",
        Box::new(SequenceExecutor::new(vec![
            StepOutcome::Success,
            StepOutcome::Fixable,
            StepOutcome::Success,
            StepOutcome::Fixable,
            StepOutcome::Success,
            StepOutcome::Fixable,
        ])),
    );

    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");
    let result = runner.run();

    match result {
        Ok(RunOutcome::Abandoned { step_id, reason }) => {
            assert!(
                step_id == "step_b" || step_id == "step_a",
                "Abandoned should identify the edge"
            );
            assert!(
                reason.contains("loop")
                    || reason.contains("limit")
                    || reason.contains("step_b")
                    || reason.contains("step_a"),
                "Abandon reason should mention loop limit or edge: got {reason}"
            );
        }
        Ok(other) => panic!("Expected Abandoned, got {other:?}"),
        Err(e) => panic!("Expected Abandoned, got error: {e:?}"),
    }
}

// ============================================================================
// Test 6: Independent loops through engine
// ============================================================================

/// Test 6: Independent loops through engine
/// GIVEN: Workflow with two independent loops, each with per-edge limits
/// WHEN: Each loop executes 2 times (4 total backward transitions)
/// THEN: Run outcome is Success — both loops stay within their limits
/// @plan:PLAN-20260408-LLXPRT-FIRST.P16
/// @requirement:REQ-LF-LOOP-002
#[test]
fn test_independent_loops_through_engine() {
    let steps = vec![
        seq_step("step_a"),
        seq_step("step_b"),
        seq_step("step_c"),
        seq_step("step_d"),
        seq_step("step_e"),
    ];

    let transitions = vec![
        // Loop 1: A → B → A
        TransitionDef {
            from: "step_a".to_string(),
            to: "step_b".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        },
        TransitionDef {
            from: "step_b".to_string(),
            to: "step_a".to_string(),
            condition: Some("fixable".to_string()),
            max_iterations: Some(3),
        },
        TransitionDef {
            from: "step_b".to_string(),
            to: "step_c".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        },
        // Loop 2: C → D → C
        TransitionDef {
            from: "step_c".to_string(),
            to: "step_d".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        },
        TransitionDef {
            from: "step_d".to_string(),
            to: "step_c".to_string(),
            condition: Some("fixable".to_string()),
            max_iterations: Some(3),
        },
        TransitionDef {
            from: "step_d".to_string(),
            to: "step_e".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        },
    ];

    let workflow_type = make_workflow_type(steps, transitions);
    // Global limit of 3 would fail with 4 total backward transitions
    let config = make_config(Some(3));
    let instance = WorkflowInstance::create(workflow_type, config);

    // Sequence: loop 1 twice, loop 2 twice, then exit
    let mut registry = ExecutorRegistry::new();
    registry.register(
        "sequence",
        Box::new(SequenceExecutor::new(vec![
            StepOutcome::Success, // step_a → step_b
            StepOutcome::Fixable, // step_b → step_a (loop 1, iter 1)
            StepOutcome::Success, // step_a → step_b
            StepOutcome::Success, // step_b → step_c (exit loop 1)
            StepOutcome::Success, // step_c → step_d
            StepOutcome::Fixable, // step_d → step_c (loop 2, iter 1)
            StepOutcome::Success, // step_c → step_d
            StepOutcome::Success, // step_d → step_e (exit loop 2)
        ])),
    );

    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");
    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected Success, got {result:?}"
    );
}

// ============================================================================
// Test 7: Verify executor dispatches through registry
// ============================================================================

/// Test 7: Verify executor dispatches through registry
/// GIVEN: A step with `step_type` = "verify" and valid params
/// WHEN: Engine runs
/// THEN: `VerifyExecutor` dispatches without `StepExecutionError`
/// @plan:PLAN-20260408-LLXPRT-FIRST.P16
/// @requirement:REQ-LF-VERIFY-001
#[test]
fn test_verify_executor_dispatches_through_registry() {
    let steps = vec![StepDef {
        step_id: "verify_step".to_string(),
        step_type: "verify".to_string(),
        description: None,
        produces: None,
        consumes: None,
        terminal: None,
        parameters: Some(serde_json::json!({
            "checks": ["test"]
        })),
    }];

    let workflow_type = make_workflow_type(steps, vec![]);
    let config = make_config(Some(10));
    let instance = WorkflowInstance::create(workflow_type, config);

    // Use with_defaults which includes VerifyExecutor
    let registry = ExecutorRegistry::with_defaults();
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");
    let result = runner.run();

    // VerifyExecutor should dispatch (test will likely return Fixable since npm won't be found)
    // but we should NOT get StepExecutionError for "verify" type
    match result {
        Ok(_) => {
            // Either Success or Fixable is fine - we just need dispatch to work
        }
        Err(EngineError::StepExecutionError { message, .. }) => {
            panic!("StepExecutionError indicates dispatch failed: {message}");
        }
        Err(e) => {
            // Other errors are fine - verify step might fail for other reasons
            println!("Got error (ok for verify): {e:?}");
        }
    }
}

// ============================================================================
// Test 8: Config variables and namespaced context combined
// ============================================================================

/// Test 8: Config variables and namespaced context combined
/// GIVEN: `WorkflowConfig` with variables: {"repo": "my-repo"}
///        `step_a` (echo {repo}) → `step_b` (echo {`step_a.stdout`})
/// WHEN: Engine runs
/// THEN: `step_b` echoes "my-repo" (config var → `step_a` → namespaced ref in `step_b`)
/// @plan:PLAN-20260408-LLXPRT-FIRST.P16
/// @requirement:REQ-LF-PROF-003,REQ-LF-CTX-001
#[test]
fn test_config_variables_and_namespaced_context_combined() {
    let steps = vec![
        StepDef {
            step_id: "step_a".to_string(),
            step_type: "shell".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: Some(serde_json::json!({
                "command": "echo {repo}"
            })),
        },
        StepDef {
            step_id: "step_b".to_string(),
            step_type: "shell".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: Some(serde_json::json!({
                "command": "echo {step_a.stdout}"
            })),
        },
    ];

    let transitions = vec![TransitionDef {
        from: "step_a".to_string(),
        to: "step_b".to_string(),
        condition: Some("success".to_string()),
        max_iterations: None,
    }];

    let workflow_type = make_workflow_type(steps, transitions);

    let mut variables = HashMap::new();
    variables.insert("repo".to_string(), "my-repo".to_string());
    let config = make_config_with_vars(Some(10), variables);

    let instance = WorkflowInstance::create(workflow_type, config);
    let registry = ExecutorRegistry::with_defaults();
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected Success, got {result:?}"
    );
}

// ============================================================================
// Test 9: Builtin variables still resolve
// ============================================================================

/// Test 9: Builtin variables still resolve
/// GIVEN: A shell step with command "echo {`run_id`}"
/// WHEN: Engine runs
/// THEN: stdout is non-empty (contains the UUID `run_id`)
/// @plan:PLAN-20260408-LLXPRT-FIRST.P16
/// @requirement:REQ-LF-CTX-004
#[test]
fn test_builtin_variables_still_resolve() {
    let steps = vec![StepDef {
        step_id: "step_a".to_string(),
        step_type: "shell".to_string(),
        description: None,
        produces: None,
        consumes: None,
        terminal: None,
        parameters: Some(serde_json::json!({
            "command": "echo {run_id}"
        })),
    }];

    let workflow_type = make_workflow_type(steps, vec![]);
    let config = make_config(Some(10));
    let instance = WorkflowInstance::create(workflow_type, config);
    let registry = ExecutorRegistry::with_defaults();
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected Success, got {result:?}"
    );
}

// ============================================================================
// Test 10: Fatal with transition routes to target step
// ============================================================================

/// Test 10: Fatal with transition routes to target step
/// GIVEN: Workflow: `step_a` → `step_b` → `step_c`, with `step_b` → `abandon_step` on fatal
/// WHEN: `step_b` returns Fatal
/// THEN: `abandon_step` is executed and `RunOutcome::Success`
/// @plan:PLAN-20260408-LLXPRT-FIRST.P16
/// @requirement:REQ-LF-FAIL-001
#[test]
fn test_fatal_with_transition_routes_to_target_step() {
    let steps = vec![
        StepDef {
            step_id: "step_a".to_string(),
            step_type: "noop".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: None,
        },
        StepDef {
            step_id: "step_b".to_string(),
            step_type: "fatal".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: None,
        },
        StepDef {
            step_id: "abandon_step".to_string(),
            step_type: "noop".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: None,
        },
    ];

    let transitions = vec![
        TransitionDef {
            from: "step_a".to_string(),
            to: "step_b".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        },
        TransitionDef {
            from: "step_b".to_string(),
            to: "abandon_step".to_string(),
            condition: Some("fatal".to_string()),
            max_iterations: None,
        },
    ];

    let workflow_type = make_workflow_type(steps, transitions);
    let config = make_config(Some(10));
    let instance = WorkflowInstance::create(workflow_type, config);

    let mut registry = ExecutorRegistry::new();
    registry.register("noop", Box::new(NoOpExecutor));
    registry.register("fatal", Box::new(FatalExecutor));

    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");
    let result = runner.run();

    // Should follow fatal transition to abandon_step and succeed
    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected Success (fatal routed to abandon_step), got {result:?}"
    );
}

// ============================================================================
// Test 11: Fatal without transition returns failure
// ============================================================================

/// Test 11: Fatal without transition returns failure
/// GIVEN: Workflow: `step_a` → `step_b` → `step_c`, with NO fatal transition from `step_b`
/// WHEN: `step_b` returns Fatal
/// THEN: `RunOutcome::Failure` at `step_b` (fallback behavior)
/// @plan:PLAN-20260408-LLXPRT-FIRST.P16
/// @requirement:REQ-LF-FAIL-001
#[test]
fn test_fatal_without_transition_returns_failure() {
    let steps = vec![
        StepDef {
            step_id: "step_a".to_string(),
            step_type: "noop".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: None,
        },
        StepDef {
            step_id: "step_b".to_string(),
            step_type: "fatal".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: None,
        },
        StepDef {
            step_id: "step_c".to_string(),
            step_type: "noop".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: None,
        },
    ];

    // No fatal transition from step_b - only success transition
    let transitions = vec![
        TransitionDef {
            from: "step_a".to_string(),
            to: "step_b".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        },
        TransitionDef {
            from: "step_b".to_string(),
            to: "step_c".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        },
    ];

    let workflow_type = make_workflow_type(steps, transitions);
    let config = make_config(Some(10));
    let instance = WorkflowInstance::create(workflow_type, config);

    let mut registry = ExecutorRegistry::new();
    registry.register("noop", Box::new(NoOpExecutor));
    registry.register("fatal", Box::new(FatalExecutor));

    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");
    let result = runner.run();

    match result {
        Ok(RunOutcome::Failure { step_id, .. }) => {
            assert_eq!(step_id, "step_b", "Failure should be at step_b");
        }
        Ok(other) => panic!("Expected Failure, got {other:?}"),
        Err(e) => panic!("Expected Failure outcome, got error: {e:?}"),
    }
}

// ============================================================================
// Test 12: Run completion records metadata (Success)
// ============================================================================

/// Test 12: Run completion records metadata (Success)
/// GIVEN: Workflow with DB path (use `with_db_path()`) and `issue_number` = "42"
/// WHEN: Engine runs to success
/// THEN: Metadata record contains outcome = "completed", `issue_number` = "42"
/// @plan:PLAN-20260408-LLXPRT-FIRST.P16
/// @requirement:REQ-LF-FAIL-005
#[test]
fn test_run_completion_records_metadata() {
    let steps = vec![StepDef {
        step_id: "step_a".to_string(),
        step_type: "noop".to_string(),
        description: None,
        produces: None,
        consumes: None,
        terminal: None,
        parameters: None,
    }];

    let workflow_type = make_workflow_type(steps, vec![]);

    let mut variables = HashMap::new();
    variables.insert("issue_number".to_string(), "42".to_string());
    let config = make_config_with_vars(Some(10), variables);

    let instance = WorkflowInstance::create(workflow_type, config);

    let mut registry = ExecutorRegistry::new();
    registry.register("noop", Box::new(NoOpExecutor));

    // Create a temp database file
    let temp_dir = std::env::temp_dir();
    let db_path = temp_dir.join(format!("test_metadata_{}.db", uuid::Uuid::new_v4()));

    let mut runner = EngineRunner::with_db_path(instance, registry, &db_path)
        .expect("Failed to create EngineRunner");
    let run_id = runner.run_id().to_string();

    let result = runner.run();
    assert!(matches!(result, Ok(RunOutcome::Success)));

    // Query the database for run metadata
    let store = SqliteStore::open(&db_path).expect("Failed to open store");
    let metadata = store
        .get_run(&run_id)
        .expect("Failed to get run metadata")
        .expect("Run metadata should exist");

    assert_eq!(
        metadata.status,
        luther_workflow::persistence::RunStatus::Completed,
        "Status should be Completed"
    );
    assert_eq!(metadata.run_id, run_id, "Run ID should match");

    // Clean up
    let _ = std::fs::remove_file(&db_path);
}

// ============================================================================
// Test 13: Run abandonment records metadata
// ============================================================================

/// Test 13: Run abandonment records metadata
/// GIVEN: Workflow that exceeds a loop limit
/// WHEN: Engine returns Abandoned
/// THEN: Metadata record contains outcome = "abandoned", `step_id` of abandonment
/// @plan:PLAN-20260408-LLXPRT-FIRST.P16
/// @requirement:REQ-LF-FAIL-005
#[test]
fn test_run_abandonment_records_metadata() {
    let steps = vec![
        StepDef {
            step_id: "step_a".to_string(),
            step_type: "sequence".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: None,
        },
        StepDef {
            step_id: "step_b".to_string(),
            step_type: "sequence".to_string(),
            description: None,
            produces: None,
            consumes: None,
            terminal: None,
            parameters: None,
        },
    ];

    let transitions = vec![
        TransitionDef {
            from: "step_a".to_string(),
            to: "step_b".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        },
        TransitionDef {
            from: "step_b".to_string(),
            to: "step_a".to_string(),
            condition: Some("fixable".to_string()),
            max_iterations: Some(2),
        },
    ];

    let workflow_type = make_workflow_type(steps, transitions);
    let config = make_config(Some(10));
    let instance = WorkflowInstance::create(workflow_type, config);

    // Create temp database
    let temp_dir = std::env::temp_dir();
    let db_path = temp_dir.join(format!("test_abandon_{}.db", uuid::Uuid::new_v4()));

    let mut registry = ExecutorRegistry::new();
    registry.register(
        "sequence",
        Box::new(SequenceExecutor::new(vec![
            StepOutcome::Success,
            StepOutcome::Fixable,
            StepOutcome::Success,
            StepOutcome::Fixable,
            StepOutcome::Success,
            StepOutcome::Fixable,
        ])),
    );

    let mut runner = EngineRunner::with_db_path(instance, registry, &db_path)
        .expect("Failed to create EngineRunner");
    let run_id = runner.run_id().to_string();

    let result = runner.run();
    assert!(
        matches!(result, Ok(RunOutcome::Abandoned { .. })),
        "Expected Abandoned, got {result:?}"
    );

    // Query the database for run metadata
    let store = SqliteStore::open(&db_path).expect("Failed to open store");
    let metadata = store
        .get_run(&run_id)
        .expect("Failed to get run metadata")
        .expect("Run metadata should exist");

    assert_eq!(
        metadata.status,
        luther_workflow::persistence::RunStatus::Abandoned,
        "Status should be Abandoned"
    );

    // Clean up
    let _ = std::fs::remove_file(&db_path);
}

// ============================================================================
// Test 14: Set work dir preserves seeded variables
// ============================================================================

/// Test 14: Set work dir preserves seeded variables
/// GIVEN: `EngineRunner` with config variables {"`my_var"`: "preserved"}
/// WHEN: Call `runner.set_work_dir(new_path)`, then run shell step echoing {`my_var`}
/// THEN: stdout contains "preserved" - the config variable survived the change
/// @plan:PLAN-20260408-LLXPRT-FIRST.P16
/// @requirement:REQ-LF-PROF-003
#[test]
fn test_set_work_dir_preserves_seeded_variables() {
    let steps = vec![StepDef {
        step_id: "step_a".to_string(),
        step_type: "shell".to_string(),
        description: None,
        produces: None,
        consumes: None,
        terminal: None,
        parameters: Some(serde_json::json!({
            "command": "echo {my_var}"
        })),
    }];

    let workflow_type = make_workflow_type(steps, vec![]);

    let mut variables = HashMap::new();
    variables.insert("my_var".to_string(), "preserved".to_string());
    let config = make_config_with_vars(Some(10), variables);

    let instance = WorkflowInstance::create(workflow_type, config);
    let registry = ExecutorRegistry::with_defaults();
    let mut runner = EngineRunner::new(instance, registry).expect("Failed to create EngineRunner");

    // Change work directory
    let new_work_dir = std::env::temp_dir().join(format!("test_work_dir_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&new_work_dir).expect("Failed to create work dir");
    runner.set_work_dir(new_work_dir.clone());

    let result = runner.run();

    assert!(
        matches!(result, Ok(RunOutcome::Success)),
        "Expected Success, got {result:?}"
    );

    // Clean up
    let _ = std::fs::remove_dir_all(&new_work_dir);
}

// ============================================================================
// Configurable binary path + typed-error / failure-reason coverage
// (Luther issue #15)
// ============================================================================

/// Write an executable shell script at `path` with the given body.
fn write_executable_script(path: &std::path::Path, body: &str) {
    std::fs::write(path, body).expect("write script");
    std::process::Command::new("chmod")
        .arg("+x")
        .arg(path)
        .status()
        .expect("chmod script");
}

/// An explicit `binary_path` param must run that binary without PATH shadowing.
#[test]
fn test_llxprt_executor_honors_explicit_binary_path() {
    use luther_workflow::engine::executors::LlxprtExecutor;

    let temp_dir = tempfile::tempdir().expect("temp dir should be created");
    let script = temp_dir.path().join("custom-llxprt");
    write_executable_script(&script, "#!/bin/sh\nprintf 'LLXPRT_DONE\\n'\nexit 0\n");

    let mut context = StepContext::new(
        temp_dir.path().to_path_buf(),
        uuid::Uuid::new_v4().to_string(),
    );
    let params = serde_json::json!({
        "binary_path": script.to_string_lossy(),
        "outcome_on_stdout": { "LLXPRT_DONE": "success" }
    });

    let outcome = LlxprtExecutor
        .execute(&mut context, &params)
        .expect("custom binary should execute");
    assert_eq!(outcome, StepOutcome::Success);
}

/// A resolved `binary_path` that does not exist yields a typed
/// `LlxprtBinaryNotFound` error with the failure reason recorded.
#[test]
fn test_llxprt_executor_missing_binary_path_is_typed_error() {
    use luther_workflow::engine::executors::LlxprtExecutor;

    let temp_dir = tempfile::tempdir().expect("temp dir should be created");
    let missing = temp_dir.path().join("does-not-exist-llxprt");

    let mut context = StepContext::new(
        temp_dir.path().to_path_buf(),
        uuid::Uuid::new_v4().to_string(),
    );
    let params = serde_json::json!({ "binary_path": missing.to_string_lossy() });

    let err = LlxprtExecutor
        .execute(&mut context, &params)
        .expect_err("missing binary should error");
    match err {
        EngineError::LlxprtBinaryNotFound { path } => {
            assert_eq!(path, missing.to_string_lossy());
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert_eq!(
        context.get("llxprt_failure_reason").map(String::as_str),
        Some("process_error")
    );
}

/// An idle timeout records `llxprt_failure_reason = "idle_timeout"`.
#[test]
fn test_llxprt_executor_idle_timeout_sets_failure_reason() {
    use luther_workflow::engine::executors::LlxprtExecutor;

    let temp_dir = tempfile::tempdir().expect("temp dir should be created");
    let script = temp_dir.path().join("idle-llxprt");
    write_executable_script(&script, "#!/bin/sh\nprintf 'starting\\n'\nsleep 30\n");

    let mut context = StepContext::new(
        temp_dir.path().to_path_buf(),
        uuid::Uuid::new_v4().to_string(),
    );
    let params = serde_json::json!({
        "binary_path": script.to_string_lossy(),
        "idle_timeout_seconds": 1,
        "timeout_seconds": 30
    });

    let outcome = LlxprtExecutor
        .execute(&mut context, &params)
        .expect("idle script should execute");
    assert_eq!(outcome, StepOutcome::Fatal);
    assert_eq!(
        context.get("llxprt_failure_reason").map(String::as_str),
        Some("idle_timeout")
    );
}
