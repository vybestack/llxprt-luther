/// @plan:PLAN-20260408-LLXPRT-FIRST.P21
/// End-to-End Smoke Tests for Luther Workflow
///
/// These tests verify the actual workflow engine against real GitHub state.
/// They require gh authentication and network access.
/// Run with: cargo test --test `smoke_test` -- --ignored
use std::path::PathBuf;
use std::process::Command;

use tempfile::TempDir;

use luther_workflow::engine::executor::{ExecutorRegistry, StepContext, StepExecutor};
use luther_workflow::engine::executors::{
    GitConfigPublishExecutor, ShellExecutor, WorkspaceOwnershipExecutor,
    WorkspaceOwnershipVerifyExecutor,
};
use luther_workflow::engine::instance::WorkflowInstance;
use luther_workflow::engine::runner::{EngineError, EngineRunner, RunContext, RunOutcome};
use luther_workflow::engine::transition::StepOutcome;
use luther_workflow::persistence::trace::{save_trace, SmokeTrace};
use luther_workflow::workflow::config_loader::{resolve_workflow_config, resolve_workflow_type};

// ============================================================================
// Trace-capture helpers (deterministic smoke-replay support, issue #19)
// ============================================================================

/// Persist a captured smoke trace to a stable location and emit a parseable
/// `SMOKE_TRACE` line reporting the saved path and the replay command. This is
/// called *before* any post-run assertion can panic so a failing live smoke
/// always reports how to reproduce it offline (issue #19 criterion #3).
/// @plan:PLAN-LUTHER-ISSUE-19-SMOKE-REPLAY
/// @requirement:REQ-SMOKE-REPLAY-003
fn report_trace(trace: &SmokeTrace, dir: &std::path::Path) -> PathBuf {
    let path = dir.join(format!("trace-{}.json", trace.run_id));
    if let Err(err) = save_trace(trace, &path) {
        eprintln!(
            "SMOKE_TRACE save_failed run_id={} error={err}",
            trace.run_id
        );
        return path;
    }
    let replay_cmd = "cargo test --test smoke_replay_tests";
    eprintln!(
        "SMOKE_TRACE saved={} replay={replay_cmd:?} run_id={}",
        path.display(),
        trace.run_id
    );
    path
}

// ============================================================================
// SmokeTestExecutor - Hybrid executor for smoke tests
// ============================================================================

/// `SmokeTestExecutor` delegates to real `ShellExecutor` for first 3 steps,
/// returns Fatal for all other steps to trigger cleanup via `abandon_and_log`.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P21
struct SmokeTestExecutor {
    /// Real shell executor for steps we want to actually run
    shell_executor: ShellExecutor,
    /// Steps that should execute real commands (delegated to shell)
    real_steps: Vec<String>,
}

impl SmokeTestExecutor {
    /// Create a new `SmokeTestExecutor` with the given steps delegated to real shell.
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P21
    const fn new(real_steps: Vec<String>) -> Self {
        Self {
            shell_executor: ShellExecutor,
            real_steps,
        }
    }
}

impl StepExecutor for SmokeTestExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        // Get current step_id from context
        let step_id = context.get("current_step_id").cloned().unwrap_or_default();

        // If this step is in our real_steps list, delegate to ShellExecutor
        if self.real_steps.contains(&step_id) {
            self.shell_executor.execute(context, params)
        } else {
            // Return Fatal to trigger abandon_and_log transition
            Ok(StepOutcome::Fatal)
        }
    }
}

// ============================================================================
// Tests
// ============================================================================
fn smoke_registry(real_steps: Vec<String>) -> ExecutorRegistry {
    let mut registry = ExecutorRegistry::new();
    registry.register("shell", Box::new(SmokeTestExecutor::new(real_steps)));
    registry.register(
        "failure_cleanup",
        Box::new(SmokeTestExecutor::new(vec!["abandon_and_log".to_string()])),
    );
    registry.register(
        "workspace_ownership_verify",
        Box::new(WorkspaceOwnershipVerifyExecutor),
    );
    registry.register("git_config_publish", Box::new(GitConfigPublishExecutor));
    registry.register("workspace_ownership", Box::new(WorkspaceOwnershipExecutor));
    registry.register(
        "command_manifest_group",
        Box::new(luther_workflow::engine::executors::NoOpExecutor),
    );
    registry.register("llxprt", Box::new(SmokeTestExecutor::new(Vec::new())));
    registry.register(
        "write_file",
        Box::new(luther_workflow::engine::executors::WriteFileExecutor),
    );
    registry.register(
        "verify",
        Box::new(luther_workflow::engine::executors::VerifyExecutor),
    );
    registry.register(
        "noop",
        Box::new(luther_workflow::engine::executors::NoOpExecutor),
    );
    registry
}

/// Smoke test that runs the real selection and split setup path against GitHub.
/// Typed ownership/config executors cover the descriptor-sensitive steps; shell
/// setup and fetch run normally before `create_plan` triggers cleanup.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P21
#[test]
#[ignore = "Requires gh auth, network access, modifies real GitHub state"]
fn test_smoke_select_and_fetch() {
    // Set up config root path
    let config_root = PathBuf::from("config");

    // Load workflow type from TOML via resolution system
    let workflow_type = resolve_workflow_type("llxprt-issue-fix-v1", &config_root)
        .expect("Failed to load workflow type");

    // Load workflow config from TOML via resolution system
    let mut config = resolve_workflow_config("llxprt-code", &config_root)
        .expect("Failed to load workflow config");

    // Create a temp directory and override work_dir in config variables
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let temp_path = temp_dir.path().to_string_lossy().to_string();
    config.variables.insert("work_dir".to_string(), temp_path);

    // Create a known run identity so the ignored live smoke can provision the
    // same ownership evidence required by the typed setup executors.
    let run_id = uuid::Uuid::new_v4().to_string();
    luther_workflow::engine::workspace_ownership::provision_workspace_owner_marker(
        temp_dir.path(),
        &run_id,
    )
    .expect("provision smoke workspace owner marker");
    let instance = WorkflowInstance::create_with_run_id(workflow_type, config, run_id);

    let registry = smoke_registry(vec![
        "select_issue".to_string(),
        "setup_workspace_init".to_string(),
        "setup_workspace".to_string(),
        "fetch_issue".to_string(),
        "abandon_and_log".to_string(),
    ]);

    // Create engine runner with an immutable workspace path resolved before
    // StepContext construction (issue 158 slice 4), replacing the removed
    // post-construction `set_work_dir` mutation.
    let temp_path = temp_dir.path().to_path_buf();
    let mut runner = EngineRunner::with_context(
        instance,
        registry,
        RunContext {
            workspace_path: Some(temp_path.to_string_lossy().to_string()),
            ..Default::default()
        },
    )
    .expect("Failed to create EngineRunner");

    // Run the engine
    let outcome = runner.run();

    // Capture a normalized smoke trace of the engine's step/outcome sequence so
    // the run can be replayed offline. Report the trace path + replay command
    // BEFORE any assertion can panic (issue #19 criterion #3). On clean success
    // the trace is removed unless LUTHER_SAVE_TRACES=1 is set.
    let run_outcome = match &outcome {
        Ok(o) => o.clone(),
        Err(_) => RunOutcome::Failure {
            step_id: "<engine-error>".to_string(),
            reason: "engine run returned Err".to_string(),
        },
    };
    if let Ok(trace) = runner.export_trace(&run_outcome) {
        let trace_path = report_trace(&trace, temp_dir.path());
        let keep = std::env::var("LUTHER_SAVE_TRACES").is_ok()
            || !matches!(outcome, Ok(RunOutcome::Success));
        if !keep {
            let _ = std::fs::remove_file(&trace_path);
        }
    } else {
        eprintln!("SMOKE_TRACE export_failed");
    }

    // The engine should complete via the abandon_and_log path
    // The exact outcome depends on how the engine handles terminal steps
    // We mainly care that the files were created

    // Verify workspace was set up: {work_dir}/.git should exist
    let git_dir = temp_dir.path().join(".git");
    assert!(
        git_dir.exists(),
        "Expected .git directory to exist in work_dir: {git_dir:?}"
    );

    // Verify issue.md was fetched and written: {work_dir}/.luther/issue.md
    let issue_md = temp_dir.path().join(".luther/issue.md");
    assert!(
        issue_md.exists(),
        "Expected issue.md to exist: {issue_md:?}"
    );
    let issue_content = std::fs::read_to_string(&issue_md).expect("Failed to read issue.md");
    assert!(
        !issue_content.is_empty(),
        "Expected issue.md to have non-empty content"
    );

    // Verify issue-raw.json was written: {work_dir}/.luther/issue-raw.json
    let issue_raw_json = temp_dir.path().join(".luther/issue-raw.json");
    assert!(
        issue_raw_json.exists(),
        "Expected issue-raw.json to exist: {issue_raw_json:?}"
    );
    let raw_content =
        std::fs::read_to_string(&issue_raw_json).expect("Failed to read issue-raw.json");
    assert!(
        !raw_content.is_empty(),
        "Expected issue-raw.json to have non-empty content"
    );

    // Verify it's valid JSON
    let json_parsed: serde_json::Value =
        serde_json::from_str(&raw_content).expect("issue-raw.json should be valid JSON");
    assert!(
        json_parsed.get("title").is_some(),
        "Expected 'title' field in issue-raw.json"
    );

    // Note: The outcome may be Success or Failure depending on exact engine behavior.
    // The critical assertions above verify the real steps executed correctly.
    // The issue should have been unassigned and unlabeled by abandon_and_log.
    match outcome {
        Ok(RunOutcome::Success | RunOutcome::Failure { .. }) => {
            // Both outcomes are acceptable for this smoke test
        }
        Ok(other) => {
            println!("Note: Unexpected outcome: {other:?}");
        }
        Err(e) => {
            panic!("Engine run failed with error: {e:?}");
        }
    }

    // TempDir cleanup happens automatically when dropped
}

/// Smoke test that verifies dry-run mode prints the key workflow `step_ids`.
/// This exercises the CLI -> config resolution -> workflow loading path.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P21
#[test]
#[ignore = "Integration test that runs cargo build and executes CLI"]
fn test_smoke_dry_run_prints_all_steps() {
    // Build the project first to ensure binary is up to date
    let build_output = Command::new("cargo")
        .args(["build"])
        .output()
        .expect("Failed to run cargo build");

    assert!(
        build_output.status.success(),
        "Cargo build failed: {}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    // Run the CLI with dry-run flag
    let output = Command::new("cargo")
        .args([
            "run",
            "--",
            "run",
            "--workflow-type",
            "llxprt-issue-fix-v1",
            "--config",
            "llxprt-code",
            "--dry-run",
        ])
        .output()
        .expect("Failed to run cargo run with dry-run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout} {stderr}");

    // Key step_ids, including the complete split setup chain, should be present.
    let expected_steps = [
        "workspace_ownership_verify",
        "select_issue",
        "setup_workspace_init",
        "git_config_publish",
        "workspace_ownership",
        "setup_workspace",
        "fetch_issue",
        "create_plan",
        "evaluate_plan",
        "implement",
        "evaluate_impl",
        "run_tests",
        "remediate",
        "push_changes",
        "generate_pr_description",
        "create_pr",
        "abandon_and_log",
        "log_completion",
    ];

    for step_id in &expected_steps {
        assert!(
            combined.contains(step_id),
            "Expected output to contain step_id '{step_id}', but got:\n{combined}"
        );
    }

    // Verify "Dry run complete" message is present
    assert!(
        combined.contains("Dry run complete") || combined.contains("dry run complete"),
        "Expected 'Dry run complete' message in output, but got:\n{combined}"
    );

    // The command should succeed (exit code 0)
    assert!(
        output.status.success(),
        "Dry run command failed with exit code: {:?}\nstderr: {}",
        output.status.code(),
        stderr
    );
}
