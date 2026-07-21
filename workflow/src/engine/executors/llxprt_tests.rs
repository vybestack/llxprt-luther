//! Tests for [`super::llxprt`].

use super::*;
use serde_json::json;
use std::path::PathBuf;

#[test]
fn parse_outcome_name_maps_known_names() {
    assert!(matches!(
        parse_outcome_name("success"),
        StepOutcome::Success
    ));
    assert!(matches!(
        parse_outcome_name("fixable"),
        StepOutcome::Fixable
    ));
    assert!(matches!(parse_outcome_name("fatal"), StepOutcome::Fatal));
    assert!(matches!(
        parse_outcome_name("retryable"),
        StepOutcome::Retryable
    ));
    assert!(matches!(
        parse_outcome_name("abandon"),
        StepOutcome::Abandon
    ));
}

#[test]
fn parse_outcome_name_unknown_defaults_to_fatal() {
    assert!(matches!(parse_outcome_name("nonsense"), StepOutcome::Fatal));
    assert!(matches!(parse_outcome_name(""), StepOutcome::Fatal));
}

#[test]
fn contains_outcome_marker_line_matches_trimmed_line() {
    let stdout = "noise\n   MARKER_DONE   \nmore";
    assert!(contains_outcome_marker_line(stdout, "MARKER_DONE"));
}

#[test]
fn contains_outcome_marker_line_requires_full_line_match() {
    let stdout = "prefix MARKER suffix";
    assert!(!contains_outcome_marker_line(stdout, "MARKER"));
}

#[test]
fn match_exit_code_outcome_maps_configured_code() {
    let params = json!({"exit_code_map": {"2": "fixable", "3": "abandon"}});
    assert!(matches!(
        match_exit_code_outcome(&params, Some(2)),
        Some(StepOutcome::Fixable)
    ));
    assert!(matches!(
        match_exit_code_outcome(&params, Some(3)),
        Some(StepOutcome::Abandon)
    ));
}

#[test]
fn match_exit_code_outcome_none_when_unmapped_or_missing() {
    let params = json!({"exit_code_map": {"2": "fixable"}});
    assert!(match_exit_code_outcome(&params, Some(9)).is_none());
    assert!(match_exit_code_outcome(&params, None).is_none());
    assert!(match_exit_code_outcome(&json!({}), Some(2)).is_none());
}

#[test]
fn match_static_stdout_outcome_matches_marker_line() {
    let params = json!({"outcome_on_stdout": {"ALL_DONE": "success"}});
    let outcome = match_static_stdout_outcome(&params, "log\nALL_DONE\n");
    assert!(matches!(outcome, Some(StepOutcome::Success)));
}

#[test]
fn match_static_stdout_outcome_none_without_match() {
    let params = json!({"outcome_on_stdout": {"ALL_DONE": "success"}});
    assert!(match_static_stdout_outcome(&params, "nothing here").is_none());
    assert!(match_static_stdout_outcome(&json!({}), "ALL_DONE").is_none());
}

#[test]
fn match_stdout_outcome_reads_shared_buffer() {
    let params = json!({"outcome_on_stdout": {"READY": "retryable"}});
    let buffer = Arc::new(Mutex::new("prelude\nREADY\n".to_string()));
    let outcome = match_stdout_outcome(&params, &buffer);
    assert!(matches!(outcome, Some(StepOutcome::Retryable)));
}

#[test]
fn string_array_param_interpolates_and_defaults() {
    let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-1".to_string());
    context.set("name", "world");
    let params = json!({"args": ["hello-{name}", "static"]});
    let out = string_array_param(&params, "args", &context);
    assert_eq!(out, vec!["hello-world".to_string(), "static".to_string()]);
    // Missing key yields empty vec.
    assert!(string_array_param(&params, "missing", &context).is_empty());
}

#[test]
fn owned_daemon_workspace_reaches_implementation_after_scope_barrier() {
    let workspace = tempfile::tempdir().expect("workspace");
    let artifacts = tempfile::tempdir().expect("artifacts");
    let mut context = owned_daemon_implementation_context(workspace.path(), artifacts.path());
    let outcome = LlxprtExecutor
        .execute(
            &mut context,
            &json!({
                "static_content": "agent reached",
                "success_file": "agent-reached.txt"
            }),
        )
        .expect("execute implementation");

    assert_eq!(outcome, StepOutcome::Success);
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("agent-reached.txt")).expect("output"),
        "agent reached"
    );
}

fn owned_daemon_implementation_context(
    workspace: &std::path::Path,
    artifacts: &std::path::Path,
) -> StepContext {
    use crate::engine::continuation::write_workspace_owner_marker;
    use crate::engine::executors::scope_control::{
        normalize_charter, persist_charter_and_status, test_measurement_config, DraftBudget,
        DraftReviewCaps, DraftSubsystem, TaskCharterDraft,
    };
    use crate::workflow::schema::ScopeControlConfig;

    initialize_llxprt_test_repo(workspace);
    std::fs::write(workspace.join("README.md"), "base\n").expect("base file");
    run_llxprt_test_git(workspace, &["add", "README.md"]);
    run_llxprt_test_git(workspace, &["commit", "-q", "-m", "base"]);
    write_workspace_owner_marker(workspace, "run-owned").expect("owner marker");
    let policy = ScopeControlConfig {
        enabled: true,
        measurement: test_measurement_config(&["rs"], &[]),
        ..ScopeControlConfig::default()
    };
    let charter = normalize_charter(&TaskCharterDraft {
        charter_id: "issue-154".into(),
        issue_number: 154,
        run_id: "run-owned".into(),
        merge_base: llxprt_test_head(workspace),
        acceptance_criteria: vec!["implementation executes".into()],
        non_goals: vec!["exclude arbitrary .luther files".into()],
        subsystems: vec![DraftSubsystem {
            id: "source".into(),
            paths: vec!["src".into()],
        }],
        budget: DraftBudget {
            max_files_changed: 1,
            max_added_lines: 10,
            max_new_modules: 1,
            max_dependencies_added: 0,
            max_public_apis_added: 0,
        },
        review_caps: DraftReviewCaps {
            initial_full_reviews: 1,
            max_delta_reviews: 1,
            final_acceptance_reviews: 1,
            max_mutating_remediation_rounds: 1,
        },
        mandatory_gates: vec!["test".into()],
    });
    persist_charter_and_status(artifacts, &charter).expect("persist charter");
    let run_context = crate::engine::runner::RunContext {
        daemon_managed: true,
        ..crate::engine::runner::RunContext::default()
    };
    let mut context =
        StepContext::from_run_context(workspace.to_path_buf(), "run-owned".into(), &run_context);
    context.set_current_step_id("implement");
    context.set("artifact_dir", &artifacts.to_string_lossy());
    context.set(
        "scope_control_policy",
        &serde_json::to_string(&policy).expect("serialize policy"),
    );
    context
}

fn initialize_llxprt_test_repo(workspace: &std::path::Path) {
    run_llxprt_test_git(workspace, &["init", "-q"]);
    run_llxprt_test_git(workspace, &["config", "user.email", "test@example.com"]);
    run_llxprt_test_git(workspace, &["config", "user.name", "Test User"]);
}

fn run_llxprt_test_git(workspace: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .expect("git command must be available for repository integration tests");
    assert!(
        output.status.success(),
        "git command failed: {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn llxprt_test_head(workspace: &std::path::Path) -> String {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(workspace)
        .output()
        .expect("git command must be available for repository integration tests");
    assert!(
        output.status.success(),
        "git rev-parse HEAD failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git head must be UTF-8")
        .trim()
        .to_string()
}
