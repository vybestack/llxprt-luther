use std::collections::HashSet;

use super::config_loader::{ConfigError, ConfigErrorKind, Result};
use super::schema::{DiscoveryConfig, WorkflowConfig};

pub(super) fn validate_discovery_config(config: &WorkflowConfig) -> Result<()> {
    let discovery = resolve_discovery_config(config);
    validate_label_sets(&discovery)?;

    if discovery.enabled {
        validate_required(&discovery, "repo", discovery.repo.as_deref())?;
        validate_required(
            &discovery,
            "approval_label",
            discovery.approval_label.as_deref(),
        )?;
        validate_required(
            &discovery,
            "approval_actor",
            discovery.approval_actor.as_deref(),
        )?;
        validate_required(
            &discovery,
            "claim_assignee",
            discovery.claim_assignee.as_deref(),
        )?;
        validate_required(&discovery, "claim_label", discovery.claim_label.as_deref())?;
    }
    Ok(())
}

fn validate_label_sets(discovery: &DiscoveryConfig) -> Result<()> {
    let include: HashSet<&str> = discovery
        .include_labels
        .iter()
        .map(String::as_str)
        .collect();
    if let Some(label) = discovery
        .exclude_labels
        .iter()
        .find(|label| include.contains(label.as_str()))
    {
        return validation_error(format!(
            "discovery include_labels and exclude_labels both contain '{label}'"
        ));
    }
    Ok(())
}

fn validate_required(_discovery: &DiscoveryConfig, field: &str, value: Option<&str>) -> Result<()> {
    if value.is_none_or(str::is_empty) {
        return validation_error(format!(
            "discovery.enabled is true but discovery.{field} could not be resolved"
        ));
    }
    Ok(())
}

fn validation_error(message: String) -> Result<()> {
    Err(ConfigError {
        message,
        source_path: None,
        kind: ConfigErrorKind::ValidationError,
    })
}

#[must_use]
pub fn resolve_discovery_config(config: &WorkflowConfig) -> DiscoveryConfig {
    let raw = config.discovery.clone().unwrap_or_default();
    let var = |key: &str| config.variables.get(key).cloned();

    let repo = raw
        .repo
        .filter(|value| !value.is_empty())
        .or_else(|| var("target_repo"));
    let include_labels = resolved_list(raw.include_labels, var("ok_label"));
    let exclude_labels = resolved_list(raw.exclude_labels, var("luther_label"));
    let issue_states = if raw.issue_states.is_empty() {
        vec!["open".to_owned()]
    } else {
        raw.issue_states
    };

    DiscoveryConfig {
        enabled: raw.enabled,
        repo,
        include_labels,
        exclude_labels,
        active_parent_label: raw.active_parent_label.or_else(|| var("luther_label")),
        issue_states,
        approval_label: raw.approval_label.or_else(|| var("ok_label")),
        approval_actor: raw.approval_actor.or_else(|| var("assignee")),
        claim_assignee: raw.claim_assignee.or_else(|| var("assignee")),
        claim_label: raw.claim_label.or_else(|| var("luther_label")),
        milestone_order: Some(raw.milestone_order.unwrap_or_else(|| "semver".to_owned())),
        max_concurrent_runs: Some(raw.max_concurrent_runs.unwrap_or(1)),
        poll_interval_secs: Some(raw.poll_interval_secs.unwrap_or(300)),
        max_concurrent_active_runs: raw.max_concurrent_active_runs,
        max_concurrent_runs_per_repository: raw.max_concurrent_runs_per_repository,
        max_concurrent_runs_per_config: raw.max_concurrent_runs_per_config,
        route_parent_issues: raw.route_parent_issues,
        parent_workflow_type_id: Some(
            raw.parent_workflow_type_id
                .unwrap_or_else(|| "parent-issue-orchestrator-v1".to_owned()),
        ),
        parent_config_id: raw.parent_config_id,
        skip_children_of_active_parents: raw.skip_children_of_active_parents,
    }
}

fn resolved_list(explicit: Vec<String>, fallback: Option<String>) -> Vec<String> {
    if explicit.is_empty() {
        fallback.into_iter().collect()
    } else {
        explicit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_config() -> WorkflowConfig {
        toml::from_str(
            r#"
config_id = "test"
workflow_type_id = "test"
[runtime]
timeout_seconds = 60
max_retries = 1
[repository]
workspace_strategy = "temp_clone"
branch_template = "issue{issue_number}"
base_branch = "main"
[guards]
max_iterations = 1
max_file_changes = 10
max_tokens = 1000
max_cost = 1.0
[variables]
target_repo = "owner/repo"
ok_label = "OK for Luther"
assignee = "acoliver"
luther_label = "Luther working"
[discovery]
enabled = true
"#,
        )
        .expect("config parses")
    }

    #[test]
    fn resolves_approval_and_claim_defaults_from_profile_variables() {
        let resolved = resolve_discovery_config(&enabled_config());
        assert_eq!(resolved.approval_label.as_deref(), Some("OK for Luther"));
        assert_eq!(resolved.approval_actor.as_deref(), Some("acoliver"));
        assert_eq!(resolved.claim_assignee.as_deref(), Some("acoliver"));
        assert_eq!(resolved.claim_label.as_deref(), Some("Luther working"));
    }

    #[test]
    fn enabled_discovery_rejects_empty_claim_policy() {
        let mut config = enabled_config();
        config
            .variables
            .insert("assignee".to_owned(), String::new());
        let error = validate_discovery_config(&config).unwrap_err();
        assert!(error.message.contains("approval_actor"));
    }
}
