use super::*;
use crate::workflow::schema::{GuardLimits, RepoConfig, RuntimeConfig};

fn test_config() -> WorkflowConfig {
    WorkflowConfig {
        config_id: "llxprt-code".to_string(),
        workflow_type_id: "llxprt-issue-fix-v1".to_string(),
        runtime: RuntimeConfig {
            timeout_seconds: 1,
            max_retries: 1,
            parallel_steps: None,
            log_level: None,
        },
        repo: RepoConfig {
            workspace_strategy: "temp_clone".to_string(),
            branch_template: "issue{issue_number}".to_string(),
            base_branch: Some("main".to_string()),
            workspace_root: None,
        },
        guard_limits: GuardLimits {
            max_iterations: None,
            max_file_changes: None,
            max_tokens: None,
            max_cost: None,
        },
        variables: [
            ("target_repo", "vybestack/llxprt-code"),
            ("repository_owner", "vybestack"),
            ("repository_name", "llxprt-code"),
            ("primary_issue_number", "1803"),
            ("work_dir", "/tmp/luther-workspaces/llxprt-code"),
            ("artifact_dir", "/tmp/luther-artifacts/llxprt-code"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect(),
        discovery: None,
        command_manifest: None,
    }
}

#[test]
fn overrides_replace_target_profile_values() {
    let mut config = test_config();
    let overrides = TargetProfileOverrides {
        repo: Some("vybestack/llxprt-luther".to_string()),
        issue: Some("3".to_string()),
        work_dir: Some(PathBuf::from("/tmp/luther-workspaces/llxprt-luther")),
        artifact_dir: Some(PathBuf::from("/tmp/luther-artifacts/llxprt-luther")),
    };

    apply_target_profile_overrides(&mut config, &overrides).expect("overrides apply");

    assert_eq!(
        config.variables.get("target_repo").map(String::as_str),
        Some("vybestack/llxprt-luther")
    );
    assert_eq!(
        config.variables.get("repository_owner").map(String::as_str),
        Some("vybestack")
    );
    assert_eq!(
        config.variables.get("repository_name").map(String::as_str),
        Some("llxprt-luther")
    );
    assert_eq!(
        config
            .variables
            .get("primary_issue_number")
            .map(String::as_str),
        Some("3")
    );
    assert_eq!(
        config.variables.get("issue_number").map(String::as_str),
        None
    );
    assert_eq!(
        config.variables.get("work_dir").map(String::as_str),
        Some("/tmp/luther-workspaces/llxprt-luther")
    );
    assert_eq!(
        config.variables.get("artifact_dir").map(String::as_str),
        Some("/tmp/luther-artifacts/llxprt-luther")
    );
}

#[test]
fn invalid_repo_fails_with_repo_variable_name() {
    let mut config = test_config();
    let overrides = TargetProfileOverrides {
        repo: Some("vybestack/llxprt/luther".to_string()),
        ..TargetProfileOverrides::default()
    };

    let error = apply_target_profile_overrides(&mut config, &overrides).unwrap_err();

    assert!(error.message.contains("target_repo"));
    assert!(error.message.contains("OWNER/NAME"));
}

#[test]
fn unresolved_path_templates_fail_before_execution() {
    let mut config = test_config();
    config.variables.insert(
        "work_dir".to_string(),
        "/tmp/luther-workspaces/{missing_repo}".to_string(),
    );

    let error = validate_target_profile(&config).unwrap_err();

    assert!(error.message.contains("work_dir"));
    assert!(error.message.contains("missing_repo"));
}

#[test]
fn unresolved_path_templates_ignore_malformed_tokens() {
    let mut config = test_config();
    config.variables.insert(
        "work_dir".to_string(),
        "/tmp/luther-workspaces/{123}/{variable-with-dashes}/{embedded{nested}tokens}/{missing_repo}".to_string(),
    );

    let error = validate_target_profile(&config).unwrap_err();

    assert!(error.message.contains("missing_repo"));
    assert!(!error.message.contains("123"));
    assert!(!error.message.contains("variable-with-dashes"));
    assert!(!error.message.contains("nested"));
}

#[cfg(unix)]
#[test]
fn non_utf8_path_overrides_fail_explicitly() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let mut config = test_config();
    let overrides = TargetProfileOverrides {
        work_dir: Some(PathBuf::from(OsString::from_vec(vec![b'w', 0x80]))),
        ..TargetProfileOverrides::default()
    };

    let error = apply_target_profile_overrides(&mut config, &overrides).unwrap_err();

    assert!(error.message.contains("work_dir path is not valid UTF-8"));
}
