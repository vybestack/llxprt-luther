use super::*;
use crate::workflow::schema::{GuardLimits, RepoConfig, RuntimeConfig, TargetPromptGuidance};

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
            project_subdir: None,
            artifact_path_base: None,
            diff_path_base: None,
            diff_path_normalization: crate::workflow::schema::DiffPathNormalization::RepoRelative,
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
        target_profile: None,
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
fn validation_required_for_partial_repository_identity() {
    let mut config = test_config();
    config.target_profile = None;
    config.variables.remove("target_repo");
    config.variables.remove("repository_name");

    assert!(target_profile_validation_required(
        &config.workflow_type_id,
        &config,
        &TargetProfileOverrides::default()
    ));
    let error = validate_target_profile(&config).unwrap_err();
    assert!(error.message.contains("repository_name"));
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
#[test]
fn profile_resolution_derives_legacy_variables_and_repo_fields() {
    let mut config = test_config();
    config.target_profile = Some(crate::workflow::schema::TargetProfileConfig {
        identity: crate::workflow::schema::TargetIdentityConfig {
            repo: Some("vybestack/llxprt-jefe".to_string()),
            base_branch: Some("develop".to_string()),
            ..Default::default()
        },
        paths: crate::workflow::schema::TargetProfilePathConfig {
            project_subdir: Some("workflow".to_string()),
            work_dir: Some("/tmp/luther-workspaces/jefe".to_string()),
            artifact_dir: Some("/tmp/luther-artifacts/jefe".to_string()),
            ..Default::default()
        },
        diff_policy: crate::workflow::schema::TargetDiffPolicyConfig {
            required_path_regex: Some("^src/".to_string()),
            ..Default::default()
        },
        prompt_guidance: TargetPromptGuidance {
            ecosystem_name: "Rust".to_string(),
            implementation: "Use custom gates".to_string(),
            ..Default::default()
        },
        ..Default::default()
    });

    resolve_target_profile(&mut config).expect("profile resolves");

    assert_eq!(
        config.variables.get("target_repo").map(String::as_str),
        Some("vybestack/llxprt-jefe")
    );
    assert_eq!(
        config.variables.get("repository_name").map(String::as_str),
        Some("llxprt-jefe")
    );
    assert_eq!(
        config.variables.get("base_branch").map(String::as_str),
        Some("develop")
    );
    assert_eq!(config.repo.project_subdir.as_deref(), Some("workflow"));
    assert_eq!(
        config
            .variables
            .get("diff_required_path_regex")
            .map(String::as_str),
        Some("^src/")
    );
    assert_eq!(
        config
            .variables
            .get("target_guidance_implementation")
            .map(String::as_str),
        Some("Use custom gates")
    );
}

#[test]
fn prompt_guidance_seeds_typed_defaults_and_bootstrap_group() {
    let mut config = test_config();
    config.variables.insert(
        "target_guidance_review".to_string(),
        "preserve runtime review guidance".to_string(),
    );
    config.target_profile = Some(crate::workflow::schema::TargetProfileConfig {
        prompt_guidance: TargetPromptGuidance {
            ecosystem_name: "Rust".to_string(),
            implementation: "Use custom gates".to_string(),
            ..Default::default()
        },
        bootstrap: crate::workflow::schema::TargetBootstrapConfig {
            command_group: Some("bootstrap".to_string()),
        },
        ..Default::default()
    });

    resolve_target_profile(&mut config).expect("profile resolves");

    assert_eq!(
        config
            .variables
            .get("target_ecosystem_name")
            .map(String::as_str),
        Some("Rust")
    );
    assert_eq!(
        config
            .variables
            .get("target_guidance_implementation")
            .map(String::as_str),
        Some("Use custom gates")
    );
    assert_eq!(
        config
            .variables
            .get("target_guidance_planning")
            .map(String::as_str),
        Some("")
    );
    assert_eq!(
        config
            .variables
            .get("target_guidance_review")
            .map(String::as_str),
        Some("preserve runtime review guidance")
    );
    assert_eq!(
        config
            .variables
            .get("target_bootstrap_command_group")
            .map(String::as_str),
        Some("bootstrap")
    );
}

#[test]
fn empty_profile_ecosystem_name_does_not_preserve_stale_runtime_value() {
    let mut config = test_config();
    config.variables.insert(
        "target_ecosystem_name".to_string(),
        "stale ecosystem".to_string(),
    );
    config.target_profile = Some(crate::workflow::schema::TargetProfileConfig {
        prompt_guidance: TargetPromptGuidance {
            ecosystem_name: String::new(),
            ..Default::default()
        },
        ..Default::default()
    });

    resolve_target_profile(&mut config).expect("profile resolves");
    let error = validate_target_profile(&config).expect_err("empty ecosystem should fail");

    assert_eq!(
        config
            .variables
            .get("target_ecosystem_name")
            .map(String::as_str),
        Some("")
    );
    assert!(error.message.contains("target_ecosystem_name"));
}

#[test]
fn profile_rejects_unresolved_prompt_variables_and_unsafe_paths() {
    let mut config = test_config();
    config.target_profile = Some(crate::workflow::schema::TargetProfileConfig {
        paths: crate::workflow::schema::TargetProfilePathConfig {
            project_subdir: Some("../outside".to_string()),
            ..Default::default()
        },
        prompt_guidance: TargetPromptGuidance {
            ecosystem_name: "Rust".to_string(),
            planning: "Use {unknown_guidance}".to_string(),
            ..Default::default()
        },
        ..Default::default()
    });

    let path_error = resolve_target_profile(&mut config).unwrap_err();
    assert!(path_error.message.contains("project_subdir"));

    config
        .target_profile
        .as_mut()
        .expect("profile")
        .paths
        .project_subdir = None;

    let prompt_error = resolve_target_profile(&mut config).unwrap_err();
    assert!(prompt_error.message.contains("unknown_guidance"));
}

#[test]
fn ecosystem_name_is_required_for_target_profile_validation() {
    let mut config = test_config();
    config.target_profile = Some(crate::workflow::schema::TargetProfileConfig::default());
    resolve_target_profile(&mut config).expect("profile resolves with empty guidance value");

    let error = validate_target_profile(&config).expect_err("ecosystem name required");

    assert!(error.message.contains("target_ecosystem_name"));
}
