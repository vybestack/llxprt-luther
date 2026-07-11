use super::*;
use luther_workflow::cli::RunArgs;
use luther_workflow::workflow::schema::{WorkflowConfig, WorkflowType};

fn run_args() -> RunArgs {
    RunArgs {
        config: None,
        dry_run: false,
        skip_preflight: false,
        workflow_type: None,
        config_dir: None,
        run_id: None,
        repo: None,
        issue: None,
        work_dir: None,
        artifact_dir: None,
    }
}

fn workflow_type_from_json(steps: serde_json::Value) -> WorkflowType {
    serde_json::from_value(serde_json::json!({
        "workflow_type_id": "wf-test",
        "steps": steps,
    }))
    .expect("workflow type deserializes")
}

fn config_with_variables(vars: serde_json::Value) -> WorkflowConfig {
    serde_json::from_value(serde_json::json!({
        "config_id": "cfg-test",
        "workflow_type_id": "wf-test",
        "runtime": {"timeout_seconds": 60, "max_retries": 1},
        "repository": {"workspace_strategy": "reuse", "branch_template": "wf/{run_id}"},
        "guards": {},
        "variables": vars,
    }))
    .expect("workflow config deserializes")
}

#[test]
fn run_config_root_defaults_to_config_dir() {
    let args = run_args();
    assert_eq!(run_config_root(&args), std::path::PathBuf::from("config"));
}

#[test]
fn run_config_root_honors_explicit_config_dir() {
    let mut args = run_args();
    args.config_dir = Some(std::path::PathBuf::from("/custom/config"));
    assert_eq!(
        run_config_root(&args),
        std::path::PathBuf::from("/custom/config")
    );
}

#[test]
fn workflow_requires_github_detects_github_step_type() {
    let wt = workflow_type_from_json(serde_json::json!([
        {"step_id": "s1", "step_type": "github_create_pr"}
    ]));
    assert!(workflow_requires_github(&wt));
}

#[test]
fn workflow_requires_github_detects_gh_command_token() {
    let wt = workflow_type_from_json(serde_json::json!([
        {"step_id": "s1", "step_type": "shell", "parameters": {"command": "gh pr list"}}
    ]));
    assert!(workflow_requires_github(&wt));
}

#[test]
fn workflow_requires_github_false_for_offline_shell() {
    let wt = workflow_type_from_json(serde_json::json!([
        {"step_id": "s1", "step_type": "shell", "parameters": {"command": "echo hi"}}
    ]));
    assert!(!workflow_requires_github(&wt));
}

#[test]
fn workflow_requires_llxprt_true_for_spawning_step() {
    let wt = workflow_type_from_json(serde_json::json!([
        {"step_id": "s1", "step_type": "llxprt", "parameters": {"prompt": "do it"}}
    ]));
    assert!(workflow_requires_llxprt(&wt));
}

#[test]
fn workflow_requires_llxprt_false_for_static_content() {
    let wt = workflow_type_from_json(serde_json::json!([
        {"step_id": "s1", "step_type": "llxprt", "parameters": {"static_content": "hello"}}
    ]));
    assert!(!workflow_requires_llxprt(&wt));
}

#[test]
fn workflow_requires_llxprt_false_when_no_llxprt_steps() {
    let wt = workflow_type_from_json(serde_json::json!([
        {"step_id": "s1", "step_type": "shell", "parameters": {"command": "echo hi"}}
    ]));
    assert!(!workflow_requires_llxprt(&wt));
}

#[test]
fn report_dry_run_validation_clean_workflow_reports_no_errors() {
    let wt = workflow_type_from_json(serde_json::json!([
        {"step_id": "s1", "step_type": "shell", "parameters": {"command": "echo hi"}}
    ]));
    let config = config_with_variables(serde_json::json!({}));
    assert!(!report_dry_run_validation(&wt, &config));
}

#[test]
fn build_run_context_reads_variables() {
    let config = config_with_variables(serde_json::json!({
        "target_repo": "owner/repo",
        "primary_issue_number": "125",
        "work_dir": "/tmp/ws",
        "artifact_dir": "/tmp/art",
    }));
    let ctx = build_run_context(&config, "run-ctx");
    assert_eq!(ctx.repository.as_deref(), Some("owner/repo"));
    assert_eq!(ctx.issue_number, Some(125));
    assert_eq!(ctx.workspace_path.as_deref(), Some("/tmp/ws"));
    assert_eq!(ctx.artifact_root.as_deref(), Some("/tmp/art"));
    assert!(ctx
        .log_path
        .as_deref()
        .is_some_and(|p| p.ends_with("run-ctx.log")));
    assert!(ctx.pr_number.is_none());
    assert!(ctx.head_sha.is_none());
}

#[test]
fn build_run_context_falls_back_to_issue_number_variable() {
    let config = config_with_variables(serde_json::json!({
        "issue_number": "77",
    }));
    let ctx = build_run_context(&config, "run-fallback");
    assert_eq!(ctx.issue_number, Some(77));
    // No explicit work_dir/artifact_dir: defaults are derived, not empty.
    assert!(ctx.workspace_path.is_some());
    assert!(ctx.artifact_root.is_some());
    assert!(ctx.repository.is_none());
}

#[test]
fn build_run_context_ignores_non_numeric_issue() {
    let config = config_with_variables(serde_json::json!({
        "primary_issue_number": "not-a-number",
    }));
    let ctx = build_run_context(&config, "run-bad-issue");
    assert!(ctx.issue_number.is_none());
}

#[test]
fn ensure_daemon_run_dir_none_path_is_ok() {
    assert!(ensure_daemon_run_dir("work", None).is_ok());
}

#[test]
fn ensure_daemon_run_dir_creates_directory() {
    let base = std::env::temp_dir().join(format!(
        "run-ensure-dir-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let nested = base.join("nested/child");
    assert!(ensure_daemon_run_dir("artifact", Some(&nested)).is_ok());
    assert!(nested.is_dir());
    let _ = std::fs::remove_dir_all(&base);
}
