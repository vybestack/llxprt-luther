/// @plan:PLAN-20260408-LLXPRT-FIRST.P11
/// Tests for dry-run validation of unresolved interpolation tokens and
/// artifact producer/consumer dependencies (Luther issue #11).
use std::collections::HashSet;

use luther_workflow::workflow::config_loader::{
    parse_workflow_config_toml, parse_workflow_type_toml, validate_artifact_dependencies,
    validate_step_tokens, validate_workflow_tokens,
};
use luther_workflow::workflow::schema::StepDef;

fn available_set(names: &[&str]) -> HashSet<String> {
    names.iter().map(|s| (*s).to_string()).collect()
}

fn shell_step(step_id: &str, command: &str) -> StepDef {
    StepDef {
        step_id: step_id.to_string(),
        step_type: "shell".to_string(),
        description: None,
        parameters: Some(serde_json::json!({ "command": command })),
        produces: None,
        consumes: None,
        terminal: None,
    }
}

#[test]
fn validate_step_tokens_reports_only_unresolvable() {
    let available = available_set(&["artifact_dir", "work_dir"]);
    let step = shell_step(
        "plan",
        "cp {artifact_dir}/in {artifact_root}/out --log {work_dir}/l",
    );

    let unresolved = validate_step_tokens(&step, &available);

    assert_eq!(unresolved.len(), 1);
    assert_eq!(unresolved[0].step_id, "plan");
    assert_eq!(unresolved[0].parameter_path, "parameters.command");
    assert_eq!(unresolved[0].token_name, "artifact_root");
}

#[test]
fn validate_step_tokens_accepts_exact_namespaced_match() {
    // Namespaced token resolves only against the exact `namespace.name` key,
    // mirroring the runtime resolver `StepContext::get`.
    let available = available_set(&["setup_workspace.existing_pr_number"]);
    let step = shell_step("post", "gh pr view {setup_workspace.existing_pr_number}");

    let unresolved = validate_step_tokens(&step, &available);

    assert!(unresolved.is_empty());
}

#[test]
fn validate_step_tokens_flags_namespaced_when_only_bare_available() {
    // Exact-match only: a bare name in the available set must NOT satisfy a
    // namespaced token. Otherwise dry-run would be more permissive than
    // runtime, suppressing genuine unresolved-token errors.
    let available = available_set(&["existing_pr_number"]);
    let step = shell_step("post", "gh pr view {setup_workspace.existing_pr_number}");

    let unresolved = validate_step_tokens(&step, &available);

    assert_eq!(unresolved.len(), 1);
    assert_eq!(unresolved[0].step_id, "post");
    assert_eq!(
        unresolved[0].token_name,
        "setup_workspace.existing_pr_number"
    );
}

#[test]
fn validate_step_tokens_clean_step_reports_nothing() {
    let available = available_set(&["artifact_dir"]);
    let step = shell_step("plan", "echo {artifact_dir}");

    assert!(validate_step_tokens(&step, &available).is_empty());
}

const MINIMAL_WORKFLOW: &str = r#"
workflow_type_id = "tok-test-v1"

[[steps]]
step_id = "first"
step_type = "shell"
[steps.parameters]
command = "echo {artifact_dir}"

[[steps]]
step_id = "second"
step_type = "shell"
[steps.parameters]
command = "echo {artifact_root}"

[[transitions]]
from = "first"
to = "second"
condition = "success"
"#;

const MINIMAL_CONFIG: &str = r#"
config_id = "tok-test-config"
workflow_type_id = "tok-test-v1"

[runtime]
timeout_seconds = 300
max_retries = 3

[repository]
workspace_strategy = "fresh"
branch_template = "luther/{issue_number}"

[guards]

[variables]
artifact_dir = "/tmp/artifacts"
"#;

#[test]
fn validate_workflow_tokens_flags_artifact_root_regression() {
    let wf = parse_workflow_type_toml(MINIMAL_WORKFLOW).expect("parse workflow");
    let config = parse_workflow_config_toml(MINIMAL_CONFIG).expect("parse config");

    let unresolved = validate_workflow_tokens(&wf, &config);

    assert_eq!(unresolved.len(), 1);
    assert_eq!(unresolved[0].step_id, "second");
    assert_eq!(unresolved[0].token_name, "artifact_root");
}

#[test]
fn validate_workflow_tokens_clean_when_all_declared() {
    let workflow = MINIMAL_WORKFLOW.replace("{artifact_root}", "{artifact_dir}");
    let wf = parse_workflow_type_toml(&workflow).expect("parse workflow");
    let config = parse_workflow_config_toml(MINIMAL_CONFIG).expect("parse config");

    assert!(validate_workflow_tokens(&wf, &config).is_empty());
}

const STEP_PRODUCED_ISSUE_NUMBER_WORKFLOW: &str = r#"
workflow_type_id = "tok-alias-v1"

[[steps]]
step_id = "identity"
step_type = "shell"
[steps.parameters]
command = "echo identity"
[steps.parameters.context_map]
primary_issue_number = ".number"

[[steps]]
step_id = "consume"
step_type = "shell"
[steps.parameters]
command = "echo issue #{issue_number}"

[[transitions]]
from = "identity"
to = "consume"
condition = "success"
"#;

const ALIAS_CONFIG: &str = r#"
config_id = "tok-alias-config"
workflow_type_id = "tok-alias-v1"

[runtime]
timeout_seconds = 300
max_retries = 3

[repository]
workspace_strategy = "fresh"
branch_template = "luther/{issue_number}"

[guards]

[variables]
"#;

#[test]
fn issue_number_alias_resolves_from_step_produced_primary_issue_number() {
    // `primary_issue_number` is produced by a step's context_map (not a config
    // variable). The documented `issue_number` alias must still be seeded, so
    // a later `{issue_number}` reference is not a dry-run false positive. This
    // guards the ordering fix: the alias is evaluated AFTER step outputs are
    // registered.
    let wf = parse_workflow_type_toml(STEP_PRODUCED_ISSUE_NUMBER_WORKFLOW).expect("parse workflow");
    let config = parse_workflow_config_toml(ALIAS_CONFIG).expect("parse config");

    let unresolved = validate_workflow_tokens(&wf, &config);

    assert!(
        unresolved.is_empty(),
        "expected issue_number alias to resolve from step-produced primary_issue_number, got: {unresolved:?}"
    );
}

#[test]
fn real_production_workflow_has_no_false_positive_tokens() {
    let wf_text = std::fs::read_to_string("config/workflows/llxprt-issue-fix-v1.toml")
        .expect("read production workflow");
    let wf = parse_workflow_type_toml(&wf_text).expect("parse production workflow");

    for config_id in ["llxprt-code", "llxprt-jefe", "codepuppy", "llxprt-luther"] {
        let cfg_text = std::fs::read_to_string(format!("config/workflow-configs/{config_id}.toml"))
            .expect("read production config");
        let config = parse_workflow_config_toml(&cfg_text).expect("parse production config");
        let unresolved = validate_workflow_tokens(&wf, &config);

        assert!(
            unresolved.is_empty(),
            "expected no unresolved tokens in production workflow for {config_id}, got: {unresolved:?}"
        );
    }
}

fn artifact_step(step_id: &str, produces: &[&str], consumes: &[&str]) -> StepDef {
    StepDef {
        step_id: step_id.to_string(),
        step_type: "shell".to_string(),
        description: None,
        parameters: None,
        produces: (!produces.is_empty())
            .then(|| produces.iter().map(|s| (*s).to_string()).collect()),
        consumes: (!consumes.is_empty())
            .then(|| consumes.iter().map(|s| (*s).to_string()).collect()),
        terminal: None,
    }
}

fn artifact_workflow(steps: Vec<StepDef>) -> luther_workflow::workflow::schema::WorkflowType {
    luther_workflow::workflow::schema::WorkflowType {
        workflow_type_id: "artifact-test-v1".to_string(),
        steps,
        transitions: Vec::new(),
        guards: Default::default(),
    }
}

#[test]
fn artifact_dependencies_ok_when_producer_exists() {
    let wf = artifact_workflow(vec![
        artifact_step("plan_step", &["plan"], &[]),
        artifact_step("impl_step", &[], &["plan"]),
    ]);

    assert!(validate_artifact_dependencies(&wf).is_empty());
}

#[test]
fn artifact_dependencies_flag_missing_producer() {
    let wf = artifact_workflow(vec![
        artifact_step("plan_step", &["plan"], &[]),
        artifact_step("impl_step", &[], &["verify_report"]),
    ]);

    let missing = validate_artifact_dependencies(&wf);

    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].consumer_step_id, "impl_step");
    assert_eq!(missing[0].artifact_name, "verify_report");
}

#[test]
fn artifact_dependencies_backward_compatible_without_declarations() {
    let wf = artifact_workflow(vec![
        artifact_step("a", &[], &[]),
        artifact_step("b", &[], &[]),
    ]);

    assert!(validate_artifact_dependencies(&wf).is_empty());
}

#[test]
fn artifact_dependencies_allow_self_production() {
    let wf = artifact_workflow(vec![artifact_step("solo", &["data"], &["data"])]);

    assert!(validate_artifact_dependencies(&wf).is_empty());
}

#[test]
fn artifact_dependencies_empty_produces_list_is_noop() {
    let wf = artifact_workflow(vec![artifact_step("only", &[], &[])]);

    assert!(validate_artifact_dependencies(&wf).is_empty());
}
