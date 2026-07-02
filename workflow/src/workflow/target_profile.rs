use std::collections::{BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};

use crate::workflow::config_loader::{ConfigError, ConfigErrorKind, Result};
use crate::workflow::schema::{TargetProfileConfig, WorkflowConfig};

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
    config.target_profile.is_some()
        || workflow_type_id.starts_with("llxprt-")
        || !overrides.is_empty()
        || config.variables.contains_key("target_repo")
        || config.variables.contains_key("repository_owner")
        || config.variables.contains_key("repository_name")
}

pub fn resolve_target_profile(config: &mut WorkflowConfig) -> Result<()> {
    let Some(profile) = config.target_profile.clone() else {
        return Ok(());
    };
    merge_identity(config, &profile)?;
    merge_paths(config, &profile)?;
    merge_conventions(config, &profile);
    merge_diff_policy(config, &profile);
    merge_command_groups(config, &profile);
    merge_list_variables(config, &profile);
    merge_prompt_guidance(config, &profile);
    validate_profile_templates(config, &profile)
}

pub fn apply_target_profile_overrides(
    config: &mut WorkflowConfig,
    overrides: &TargetProfileOverrides,
) -> Result<()> {
    if let Some(repo) = overrides.repo.as_deref() {
        let (owner, name) = split_target_repo(repo)?;
        insert_var(config, "target_repo", repo);
        insert_var(config, "repository_owner", owner);
        insert_var(config, "repository_name", name);
    }

    if let Some(issue) = overrides.issue.as_deref() {
        insert_var(config, "primary_issue_number", issue);
        config.variables.remove("issue_number");
    }

    if let Some(work_dir) = &overrides.work_dir {
        let work_dir_str = utf8_path_override("work_dir", work_dir)?;
        insert_var(config, "work_dir", work_dir_str);
    }

    if let Some(artifact_dir) = &overrides.artifact_dir {
        let artifact_dir_str = utf8_path_override("artifact_dir", artifact_dir)?;
        insert_var(config, "artifact_dir", artifact_dir_str);
    }

    Ok(())
}

pub fn validate_target_profile(config: &WorkflowConfig) -> Result<()> {
    let mut errors = Vec::new();
    validate_repo_identity(config, &mut errors);
    validate_required_issue(config, &mut errors);
    validate_runtime_path_variables(config, &mut errors);
    validate_profile_command_groups(config, &mut errors);

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

fn merge_identity(config: &mut WorkflowConfig, profile: &TargetProfileConfig) -> Result<()> {
    let repo = profile
        .identity
        .repo
        .clone()
        .or_else(|| config.variables.get("target_repo").cloned());
    if let Some(repo) = repo.as_deref() {
        let (owner, name) = split_target_repo(repo)?;
        let owner = owner.to_string();
        let name = name.to_string();
        insert_var(config, "target_repo", repo);
        insert_var(config, "repository_owner", &owner);
        insert_var(config, "repository_name", &name);
    } else if let (Some(owner), Some(name)) = (
        profile.identity.owner.as_deref(),
        profile.identity.name.as_deref(),
    ) {
        let repo = format!("{owner}/{name}");
        split_target_repo(&repo)?;
        insert_var(config, "target_repo", &repo);
        insert_var(config, "repository_owner", owner);
        insert_var(config, "repository_name", name);
    } else {
        insert_optional(
            config,
            "repository_owner",
            profile.identity.owner.as_deref(),
        );
        insert_optional(config, "repository_name", profile.identity.name.as_deref());
    }

    if let Some(base_branch) = profile.identity.base_branch.as_deref() {
        config.repo.base_branch = Some(base_branch.to_string());
        insert_var(config, "base_branch", base_branch);
    }
    Ok(())
}

fn merge_paths(config: &mut WorkflowConfig, profile: &TargetProfileConfig) -> Result<()> {
    validate_profile_paths(profile)?;
    if let Some(value) = profile.paths.project_subdir.as_deref() {
        config.repo.project_subdir = Some(value.to_string());
    }
    if let Some(value) = profile.paths.artifact_path_base.as_deref() {
        config.repo.artifact_path_base = Some(value.to_string());
    }
    if let Some(value) = profile.paths.diff_path_base.as_deref() {
        config.repo.diff_path_base = Some(value.to_string());
    }
    insert_optional(
        config,
        "project_subdir",
        profile.paths.project_subdir.as_deref(),
    );
    insert_optional(
        config,
        "default_command_cwd",
        profile.paths.default_command_cwd.as_deref(),
    );
    insert_optional(config, "work_dir", profile.paths.work_dir.as_deref());
    insert_optional(
        config,
        "artifact_dir",
        profile.paths.artifact_dir.as_deref(),
    );
    Ok(())
}

fn merge_conventions(config: &mut WorkflowConfig, profile: &TargetProfileConfig) {
    insert_optional(
        config,
        "assignee",
        profile.issue_conventions.assignee.as_deref(),
    );
    insert_optional(
        config,
        "ok_label",
        profile.issue_conventions.ok_label.as_deref(),
    );
    insert_optional(
        config,
        "luther_label",
        profile.issue_conventions.luther_label.as_deref(),
    );
    insert_optional(
        config,
        "pr_title_prefix",
        profile.pr_conventions.title_prefix.as_deref(),
    );
    insert_optional(
        config,
        "pr_body_guidance",
        profile.pr_conventions.body_guidance.as_deref(),
    );
    if let Some(branch) = profile.templates.branch.as_deref() {
        config.repo.branch_template = branch.to_string();
        insert_var(config, "branch_template", branch);
    }
    insert_optional(
        config,
        "pr_title_template",
        profile.templates.pr_title.as_deref(),
    );
    insert_optional(
        config,
        "pr_body_template",
        profile.templates.pr_body.as_deref(),
    );
}

fn merge_diff_policy(config: &mut WorkflowConfig, profile: &TargetProfileConfig) {
    insert_optional(
        config,
        "required_changed_path_pattern",
        profile.diff_policy.required_changed_path_pattern.as_deref(),
    );
    insert_optional(
        config,
        "diff_required_path_regex",
        profile.diff_policy.required_path_regex.as_deref(),
    );
    insert_optional(
        config,
        "diff_failure_message",
        profile.diff_policy.failure_message.as_deref(),
    );
    insert_joined(
        config,
        "diff_allowed_path_patterns",
        &profile.diff_policy.allowed_path_patterns,
    );
    insert_joined(
        config,
        "diff_required_path_patterns",
        &profile.diff_policy.required_path_patterns,
    );
}

fn merge_command_groups(config: &mut WorkflowConfig, profile: &TargetProfileConfig) {
    for (logical_name, manifest_group) in &profile.command_groups {
        insert_var(
            config,
            &format!("command_manifest_group_{logical_name}"),
            manifest_group,
        );
    }
}

fn merge_list_variables(config: &mut WorkflowConfig, profile: &TargetProfileConfig) {
    insert_joined(config, "required_pr_checks", &profile.pr_checks.required);
    insert_joined(config, "optional_pr_checks", &profile.pr_checks.optional);
    insert_joined(config, "ignored_pr_checks", &profile.pr_checks.ignored);
    insert_joined(config, "auth_requirements", &profile.auth.requirements);
    insert_joined(
        config,
        "preflight_expectations",
        &profile.preflight.expectations,
    );
}

fn merge_prompt_guidance(config: &mut WorkflowConfig, profile: &TargetProfileConfig) {
    for key in ["planning", "implementation", "review"] {
        config
            .variables
            .entry(format!("target_guidance_{key}"))
            .or_default();
    }
    for (key, value) in &profile.prompt_guidance {
        insert_var(config, &format!("target_guidance_{key}"), value);
    }
}

fn validate_profile_templates(
    config: &WorkflowConfig,
    profile: &TargetProfileConfig,
) -> Result<()> {
    let templates = profile_templates(profile);
    let template_variables = template_variables(config);
    let errors = templates
        .iter()
        .filter_map(|(field, value)| unresolved_template_error(field, value, &template_variables))
        .collect::<Vec<_>>();
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

fn profile_templates(profile: &TargetProfileConfig) -> Vec<(&'static str, &str)> {
    let mut templates = Vec::new();
    push_optional(
        &mut templates,
        "target_profile.templates.branch",
        profile.templates.branch.as_deref(),
    );
    push_optional(
        &mut templates,
        "target_profile.templates.pr_title",
        profile.templates.pr_title.as_deref(),
    );
    push_optional(
        &mut templates,
        "target_profile.templates.pr_body",
        profile.templates.pr_body.as_deref(),
    );
    push_optional(
        &mut templates,
        "target_profile.diff_policy.required_changed_path_pattern",
        profile.diff_policy.required_changed_path_pattern.as_deref(),
    );
    push_optional(
        &mut templates,
        "target_profile.diff_policy.required_path_regex",
        profile.diff_policy.required_path_regex.as_deref(),
    );
    push_optional(
        &mut templates,
        "target_profile.diff_policy.failure_message",
        profile.diff_policy.failure_message.as_deref(),
    );
    for (key, value) in &profile.prompt_guidance {
        templates.push(("target_profile.prompt_guidance", value.as_str()));
        templates.push(("target_profile.prompt_guidance_key", key.as_str()));
    }
    templates
}

fn validate_profile_paths(profile: &TargetProfileConfig) -> Result<()> {
    for (field, value) in profile_path_values(profile) {
        if unsafe_relative_path(value) {
            return Err(ConfigError {
                message: format!("{field} must stay within the repository, got '{value}'"),
                source_path: None,
                kind: ConfigErrorKind::ValidationError,
            });
        }
    }
    Ok(())
}

fn profile_path_values(profile: &TargetProfileConfig) -> Vec<(&'static str, &str)> {
    let mut values = Vec::new();
    push_optional(
        &mut values,
        "target_profile.paths.project_subdir",
        profile.paths.project_subdir.as_deref(),
    );
    push_optional(
        &mut values,
        "target_profile.paths.default_command_cwd",
        profile.paths.default_command_cwd.as_deref(),
    );
    push_optional(
        &mut values,
        "target_profile.paths.artifact_path_base",
        profile.paths.artifact_path_base.as_deref(),
    );
    push_optional(
        &mut values,
        "target_profile.paths.diff_path_base",
        profile.paths.diff_path_base.as_deref(),
    );
    values
}

fn unsafe_relative_path(value: &str) -> bool {
    let path = Path::new(value);
    path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
}

fn validate_repo_identity(config: &WorkflowConfig, errors: &mut Vec<String>) {
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
}

fn validate_required_issue(config: &WorkflowConfig, errors: &mut Vec<String>) {
    if trimmed_value(config, "primary_issue_number").is_none()
        && trimmed_value(config, "issue_number").is_none()
    {
        errors.push(
            "missing target profile variable primary_issue_number or issue_number".to_string(),
        );
    }
}

fn validate_runtime_path_variables(config: &WorkflowConfig, errors: &mut Vec<String>) {
    for key in ["work_dir", "artifact_dir"] {
        match trimmed_value(config, key) {
            Some(value) => collect_unresolved_errors(config, key, value, errors),
            None => errors.push(format!("missing target profile variable {key}")),
        }
    }
}

fn validate_profile_command_groups(config: &WorkflowConfig, errors: &mut Vec<String>) {
    let Some(profile) = &config.target_profile else {
        return;
    };
    let Some(manifest) = &config.command_manifest else {
        if !profile.command_groups.is_empty() {
            errors.push("target profile command groups require command_manifest".to_string());
        }
        return;
    };
    for (logical_name, manifest_group) in &profile.command_groups {
        if !manifest.groups.contains_key(manifest_group) {
            errors.push(format!(
                "target profile command group '{logical_name}' references unknown manifest group '{manifest_group}'"
            ));
        }
    }
}

fn collect_unresolved_errors(
    config: &WorkflowConfig,
    key: &str,
    value: &str,
    errors: &mut Vec<String>,
) {
    let interpolated = interpolate_variables(value, &config.variables);
    let unresolved = unresolved_tokens(&interpolated);
    if !unresolved.is_empty() {
        errors.push(format!(
            "target profile variable {key} contains unresolved template variable(s): {}",
            unresolved.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
}

fn unresolved_template_error(
    field: &str,
    value: &str,
    variables: &HashMap<String, String>,
) -> Option<String> {
    let interpolated = interpolate_variables(value, variables);
    let unresolved = unresolved_tokens(&interpolated)
        .into_iter()
        .filter(|token| !variables.contains_key(token))
        .collect::<BTreeSet<_>>();
    (!unresolved.is_empty()).then(|| {
        format!(
            "{field} contains unresolved template variable(s): {}",
            unresolved.into_iter().collect::<Vec<_>>().join(", ")
        )
    })
}

fn template_variables(config: &WorkflowConfig) -> HashMap<String, String> {
    let mut variables = config.variables.clone();
    for runtime_token in ["issue_title", "issue_url", "pr_body_guidance"] {
        variables
            .entry(runtime_token.to_string())
            .or_insert_with(|| format!("{{{runtime_token}}}"));
    }
    variables
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

fn interpolate_variables(template: &str, variables: &HashMap<String, String>) -> String {
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

fn unresolved_tokens(value: &str) -> BTreeSet<String> {
    let mut tokens = BTreeSet::new();
    let mut remaining = value;
    while let Some(start) = remaining.find('{') {
        let after_start = &remaining[start + 1..];
        let Some(end) = after_start.find('}') else {
            break;
        };
        let token = after_start[..end].trim();
        if let Some(token) = unresolved_variable_token(token) {
            tokens.insert(token);
        }
        remaining = &after_start[end + 1..];
    }
    tokens
}

fn unresolved_variable_token(token: &str) -> Option<String> {
    token
        .split_at_checked(1)
        .filter(|(first, rest)| valid_token_head(first) && valid_token_tail(rest))
        .map(|_| token.to_string())
}

fn valid_token_head(first: &str) -> bool {
    first == "_" || first.as_bytes()[0].is_ascii_alphabetic()
}

fn valid_token_tail(rest: &str) -> bool {
    rest.bytes().all(valid_token_tail_byte)
}

fn valid_token_tail_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn insert_var(config: &mut WorkflowConfig, key: &str, value: &str) {
    config.variables.insert(key.to_string(), value.to_string());
}

fn insert_optional(config: &mut WorkflowConfig, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        insert_var(config, key, value);
    }
}

fn insert_joined(config: &mut WorkflowConfig, key: &str, values: &[String]) {
    if !values.is_empty() {
        insert_var(config, key, &values.join(","));
    }
}

fn push_optional<'a>(
    values: &mut Vec<(&'static str, &'a str)>,
    field: &'static str,
    value: Option<&'a str>,
) {
    if let Some(value) = value {
        values.push((field, value));
    }
}

#[cfg(test)]
mod target_profile_tests;
