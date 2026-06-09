use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::workflow::config_loader::{ConfigError, ConfigErrorKind, Result};
use crate::workflow::schema::WorkflowConfig;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TargetProfileOverrides {
    pub repo: Option<String>,
    pub issue: Option<String>,
    pub work_dir: Option<PathBuf>,
    pub artifact_dir: Option<PathBuf>,
}

impl TargetProfileOverrides {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.repo.is_none()
            && self.issue.is_none()
            && self.work_dir.is_none()
            && self.artifact_dir.is_none()
    }
}

#[must_use]
pub fn target_profile_validation_required(
    workflow_type_id: &str,
    config: &WorkflowConfig,
    overrides: &TargetProfileOverrides,
) -> bool {
    workflow_type_id.starts_with("llxprt-")
        || !overrides.is_empty()
        || [
            "target_repo",
            "repository_owner",
            "repository_name",
            "primary_issue_number",
            "issue_number",
            "work_dir",
            "artifact_dir",
        ]
        .iter()
        .any(|key| config.variables.contains_key(*key))
}

pub fn apply_target_profile_overrides(
    config: &mut WorkflowConfig,
    overrides: &TargetProfileOverrides,
) -> Result<()> {
    if let Some(repo) = overrides.repo.as_deref() {
        let (owner, name) = split_target_repo(repo)?;
        config
            .variables
            .insert("target_repo".to_string(), repo.to_string());
        config
            .variables
            .insert("repository_owner".to_string(), owner.to_string());
        config
            .variables
            .insert("repository_name".to_string(), name.to_string());
    }

    if let Some(issue) = overrides.issue.as_deref() {
        config
            .variables
            .insert("primary_issue_number".to_string(), issue.to_string());
        config.variables.remove("issue_number");
    }

    if let Some(work_dir) = &overrides.work_dir {
        let work_dir_str = utf8_path_override("work_dir", work_dir)?;
        config
            .variables
            .insert("work_dir".to_string(), work_dir_str.to_string());
    }

    if let Some(artifact_dir) = &overrides.artifact_dir {
        let artifact_dir_str = utf8_path_override("artifact_dir", artifact_dir)?;
        config
            .variables
            .insert("artifact_dir".to_string(), artifact_dir_str.to_string());
    }

    Ok(())
}

pub fn validate_target_profile(config: &WorkflowConfig) -> Result<()> {
    let variables = &config.variables;
    let mut errors = Vec::new();

    match trimmed_value(config, "target_repo") {
        Some(repo) => {
            if let Err(error) = split_target_repo(repo) {
                errors.push(error.message);
            }
        }
        None => {
            if trimmed_value(config, "repository_owner").is_none() {
                errors.push("missing target profile variable repository_owner".to_string());
            }
            if trimmed_value(config, "repository_name").is_none() {
                errors.push("missing target profile variable repository_name".to_string());
            }
        }
    }

    if trimmed_value(config, "primary_issue_number").is_none()
        && trimmed_value(config, "issue_number").is_none()
    {
        errors.push(
            "missing target profile variable primary_issue_number or issue_number".to_string(),
        );
    }

    for key in ["work_dir", "artifact_dir"] {
        match trimmed_value(config, key) {
            Some(value) => {
                let interpolated = interpolate_variables(value, variables);
                let unresolved = unresolved_tokens(&interpolated);
                if !unresolved.is_empty() {
                    errors.push(format!(
                        "target profile variable {key} contains unresolved template variable(s): {}",
                        unresolved.into_iter().collect::<Vec<_>>().join(", ")
                    ));
                }
            }
            None => errors.push(format!("missing target profile variable {key}")),
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ConfigError {
            message: format!("invalid target profile: {}", errors.join("; ")),
            source_path: None,
            kind: ConfigErrorKind::ValidationError,
        })
    }
}

fn split_target_repo(repo: &str) -> Result<(&str, &str)> {
    let trimmed = repo.trim();
    let Some((owner, name)) = trimmed.split_once('/') else {
        return Err(invalid_repo_error(repo));
    };
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return Err(invalid_repo_error(repo));
    }
    Ok((owner, name))
}

fn invalid_repo_error(repo: &str) -> ConfigError {
    ConfigError {
        message: format!("target_repo must be in OWNER/NAME form, got '{repo}'"),
        source_path: None,
        kind: ConfigErrorKind::ValidationError,
    }
}

fn utf8_path_override<'a>(key: &str, path: &'a Path) -> Result<&'a str> {
    path.as_os_str().to_str().ok_or_else(|| ConfigError {
        message: format!("{key} path is not valid UTF-8: {}", path.display()),
        source_path: None,
        kind: ConfigErrorKind::ValidationError,
    })
}

fn trimmed_value<'a>(config: &'a WorkflowConfig, key: &str) -> Option<&'a str> {
    config
        .variables
        .get(key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
}

fn interpolate_variables(
    template: &str,
    variables: &std::collections::HashMap<String, String>,
) -> String {
    let mut result = template.to_string();
    let mut keys = variables.keys().collect::<Vec<_>>();
    keys.sort_by_key(|key| std::cmp::Reverse(key.len()));
    for key in keys {
        let token = format!("{{{key}}}");
        if let Some(value) = variables.get(key) {
            result = result.replace(&token, value);
        }
    }
    if !variables.contains_key("issue_number") {
        if let Some(value) = variables.get("primary_issue_number") {
            result = result.replace("{issue_number}", value);
        }
    }
    result
}

/// Returns unresolved template tokens whose names match `[A-Za-z_][A-Za-z0-9_]*`.
fn unresolved_tokens(value: &str) -> BTreeSet<String> {
    let mut tokens = BTreeSet::new();
    let mut remaining = value;
    while let Some(start) = remaining.find('{') {
        let after_start = &remaining[start + 1..];
        let Some(end) = after_start.find('}') else {
            break;
        };
        let token = after_start[..end].trim();
        if valid_template_token(token) {
            tokens.insert(token.to_string());
        }
        remaining = &after_start[end + 1..];
    }
    tokens
}

fn valid_template_token(token: &str) -> bool {
    let mut chars = token.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

#[cfg(test)]
mod tests {
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
}
