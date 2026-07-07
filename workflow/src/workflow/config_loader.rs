/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
/// Configuration loading and resolution for workflow types and configs.
use std::collections::HashSet;
use std::path::Path;

use crate::engine::executor::extract_tokens;
use crate::workflow::command_manifest::{CommandEntry, CommandManifest};
use crate::workflow::schema::{
    DaemonSchedulerConfig, DiffPathNormalization, DiscoveryConfig, StepDef, WorkflowConfig,
    WorkflowRunRef, WorkflowType,
};
use crate::workflow::target_profile::{
    resolve_target_profile, target_profile_validation_required, validate_target_profile,
    TargetProfileOverrides,
};
use crate::workflow::validation::validate_workflow_graph;

/// Error type for configuration loading and validation failures.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
/// @requirement:REQ-EARS-WF-005
#[derive(Debug, Clone)]
pub struct ConfigError {
    pub message: String,
    pub source_path: Option<String>,
    pub kind: ConfigErrorKind,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.source_path {
            Some(path) => write!(f, "{} (at {})", self.message, path),
            None => write!(f, "{}", self.message),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Classification of configuration errors.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigErrorKind {
    NotFound,
    ParseError,
    ValidationError,
    MismatchedType,
}

/// Result type for config operations.
pub type Result<T> = std::result::Result<T, ConfigError>;

/// Resolve a workflow type by its ID from the config root.
/// Checks production layout first (flat), then test fixture layout (valid/ subdirectory).
/// Tries .toml first, then falls back to .json for both layouts.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
/// @plan:PLAN-20260408-LLXPRT-FIRST.P20
/// @requirement:REQ-EARS-WF-004
pub fn resolve_workflow_type(id: &str, root: &Path) -> Result<WorkflowType> {
    // Production layout: {root}/workflows/{id}.{ext}
    let prod_dir = root.join("workflows");
    let prod_toml = prod_dir.join(format!("{}.toml", id));
    let prod_json = prod_dir.join(format!("{}.json", id));

    // Test fixture layout: {root}/workflows/valid/{id}.{ext}
    let valid_dir = root.join("workflows/valid");
    let valid_toml = valid_dir.join(format!("{}.toml", id));
    let valid_json = valid_dir.join(format!("{}.json", id));

    // Try production layout TOML first
    if prod_toml.exists() {
        let content = std::fs::read_to_string(&prod_toml).map_err(|e| ConfigError {
            message: format!("Failed to read workflow type file: {}", e),
            source_path: Some(prod_toml.to_string_lossy().to_string()),
            kind: ConfigErrorKind::NotFound,
        })?;
        return load_and_validate_workflow_type_toml(&content, &prod_toml);
    }

    // Try production layout JSON
    if prod_json.exists() {
        let content = std::fs::read_to_string(&prod_json).map_err(|e| ConfigError {
            message: format!("Failed to read workflow type file: {}", e),
            source_path: Some(prod_json.to_string_lossy().to_string()),
            kind: ConfigErrorKind::NotFound,
        })?;
        return load_and_validate_workflow_type_json(&content, &prod_json);
    }

    // Fall back to test fixture layout TOML
    if valid_toml.exists() {
        let content = std::fs::read_to_string(&valid_toml).map_err(|e| ConfigError {
            message: format!("Failed to read workflow type file: {}", e),
            source_path: Some(valid_toml.to_string_lossy().to_string()),
            kind: ConfigErrorKind::NotFound,
        })?;
        return load_and_validate_workflow_type_toml(&content, &valid_toml);
    }

    // Fall back to test fixture layout JSON
    if valid_json.exists() {
        let content = std::fs::read_to_string(&valid_json).map_err(|e| ConfigError {
            message: format!("Failed to read workflow type file: {}", e),
            source_path: Some(valid_json.to_string_lossy().to_string()),
            kind: ConfigErrorKind::NotFound,
        })?;
        return load_and_validate_workflow_type_json(&content, &valid_json);
    }

    // Neither file exists in any location
    Err(ConfigError {
        message: format!("Workflow type '{}' not found (tried .toml and .json)", id),
        source_path: Some(prod_dir.to_string_lossy().to_string()),
        kind: ConfigErrorKind::NotFound,
    })
}

/// Resolve a workflow config by its ID from the config root.
/// Checks production layout first (flat), then test fixture layout (valid/ subdirectory).
/// Tries .toml first, then falls back to .json for both layouts.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
/// @plan:PLAN-20260408-LLXPRT-FIRST.P20
/// @requirement:REQ-EARS-WF-004
pub fn resolve_workflow_config(id: &str, root: &Path) -> Result<WorkflowConfig> {
    // Production layout: {root}/workflow-configs/{id}.{ext}
    let prod_dir = root.join("workflow-configs");
    let prod_toml = prod_dir.join(format!("{}.toml", id));
    let prod_json = prod_dir.join(format!("{}.json", id));

    // Test fixture layout: {root}/workflow-configs/valid/{id}.{ext}
    let valid_dir = root.join("workflow-configs/valid");
    let valid_toml = valid_dir.join(format!("{}.toml", id));
    let valid_json = valid_dir.join(format!("{}.json", id));

    // Try production layout TOML first
    if prod_toml.exists() {
        let content = std::fs::read_to_string(&prod_toml).map_err(|e| ConfigError {
            message: format!("Failed to read workflow config file: {}", e),
            source_path: Some(prod_toml.to_string_lossy().to_string()),
            kind: ConfigErrorKind::NotFound,
        })?;
        return parse_workflow_config_toml(&content);
    }

    // Try production layout JSON
    if prod_json.exists() {
        let content = std::fs::read_to_string(&prod_json).map_err(|e| ConfigError {
            message: format!("Failed to read workflow config file: {}", e),
            source_path: Some(prod_json.to_string_lossy().to_string()),
            kind: ConfigErrorKind::NotFound,
        })?;
        return parse_workflow_config_json(&content);
    }

    // Fall back to test fixture layout TOML
    if valid_toml.exists() {
        let content = std::fs::read_to_string(&valid_toml).map_err(|e| ConfigError {
            message: format!("Failed to read workflow config file: {}", e),
            source_path: Some(valid_toml.to_string_lossy().to_string()),
            kind: ConfigErrorKind::NotFound,
        })?;
        return parse_workflow_config_toml(&content);
    }

    // Fall back to test fixture layout JSON
    if valid_json.exists() {
        let content = std::fs::read_to_string(&valid_json).map_err(|e| ConfigError {
            message: format!("Failed to read workflow config file: {}", e),
            source_path: Some(valid_json.to_string_lossy().to_string()),
            kind: ConfigErrorKind::NotFound,
        })?;
        return parse_workflow_config_json(&content);
    }

    // Neither file exists in any location
    Err(ConfigError {
        message: format!("Workflow config '{}' not found (tried .toml and .json)", id),
        source_path: Some(prod_dir.to_string_lossy().to_string()),
        kind: ConfigErrorKind::NotFound,
    })
}

/// Resolve both workflow type and config, returning a bound WorkflowRunRef.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
/// @plan:PLAN-20260408-LLXPRT-FIRST.P20
/// @requirement:REQ-EARS-WF-004,REQ-EARS-ENG-001
pub fn resolve_workflow(
    workflow_type_id: &str,
    config_id: &str,
    run_id: &str,
    root: &Path,
) -> Result<(WorkflowType, WorkflowConfig, WorkflowRunRef)> {
    // Resolve workflow type first
    let workflow_type = resolve_workflow_type(workflow_type_id, root)?;

    // Then resolve workflow config
    let config = resolve_workflow_config(config_id, root)?;

    // Validate that config matches the workflow type
    validate_config_matches_type(&config, &workflow_type)?;

    // Create the run reference
    let run_ref = WorkflowRunRef::new(workflow_type_id, config_id, run_id);

    Ok((workflow_type, config, run_ref))
}

pub fn parse_daemon_scheduler_config_toml(toml_str: &str) -> Result<DaemonSchedulerConfig> {
    toml::from_str(toml_str).map_err(|e| ConfigError {
        message: format!("Failed to parse daemon scheduler TOML: {}", e),
        source_path: None,
        kind: ConfigErrorKind::ParseError,
    })
}

pub fn load_daemon_scheduler_config(path: &Path) -> Result<DaemonSchedulerConfig> {
    let content = std::fs::read_to_string(path).map_err(|e| ConfigError {
        message: format!("Failed to read daemon scheduler config: {}", e),
        source_path: Some(path.to_string_lossy().to_string()),
        kind: ConfigErrorKind::NotFound,
    })?;
    parse_daemon_scheduler_config_toml(&content).map_err(|e| attach_source_path(e, path))
}

/// Parse workflow type from TOML string.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub fn parse_workflow_type_toml(toml_str: &str) -> Result<WorkflowType> {
    toml::from_str(toml_str).map_err(|e| ConfigError {
        message: format!("Failed to parse workflow type TOML: {}", e),
        source_path: None,
        kind: ConfigErrorKind::ParseError,
    })
}

/// Parse workflow type from JSON string.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub fn parse_workflow_type_json(json_str: &str) -> Result<WorkflowType> {
    serde_json::from_str(json_str).map_err(|e| ConfigError {
        message: format!("Failed to parse workflow type JSON: {}", e),
        source_path: None,
        kind: ConfigErrorKind::ParseError,
    })
}

/// Parse a workflow type from TOML and run full (field + graph) validation,
/// attaching `source_path` to any resulting error. Used by the resolution path
/// so invalid graphs are rejected at load time before engine construction.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
fn load_and_validate_workflow_type_toml(content: &str, source_path: &Path) -> Result<WorkflowType> {
    let workflow_type =
        parse_workflow_type_toml(content).map_err(|e| attach_source_path(e, source_path))?;
    validate_workflow_type(&workflow_type).map_err(|e| attach_source_path(e, source_path))?;
    Ok(workflow_type)
}

/// Parse a workflow type from JSON and run full (field + graph) validation,
/// attaching `source_path` to any resulting error.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
fn load_and_validate_workflow_type_json(content: &str, source_path: &Path) -> Result<WorkflowType> {
    let workflow_type =
        parse_workflow_type_json(content).map_err(|e| attach_source_path(e, source_path))?;
    validate_workflow_type(&workflow_type).map_err(|e| attach_source_path(e, source_path))?;
    Ok(workflow_type)
}

/// Attach a source path to a `ConfigError` if it does not already have one.
fn attach_source_path(mut error: ConfigError, source_path: &Path) -> ConfigError {
    if error.source_path.is_none() {
        error.source_path = Some(source_path.to_string_lossy().to_string());
    }
    error
}

/// Parse workflow config from TOML string.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub fn parse_workflow_config_toml(toml_str: &str) -> Result<WorkflowConfig> {
    let config = toml::from_str(toml_str).map_err(|e| ConfigError {
        message: format!("Failed to parse workflow config TOML: {}", e),
        source_path: None,
        kind: ConfigErrorKind::ParseError,
    })?;
    resolve_and_validate_workflow_config(config)
}

/// Parse workflow config from JSON string.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub fn parse_workflow_config_json(json_str: &str) -> Result<WorkflowConfig> {
    let config = serde_json::from_str(json_str).map_err(|e| ConfigError {
        message: format!("Failed to parse workflow config JSON: {}", e),
        source_path: None,
        kind: ConfigErrorKind::ParseError,
    })?;
    resolve_and_validate_workflow_config(config)
}

fn resolve_and_validate_workflow_config(mut config: WorkflowConfig) -> Result<WorkflowConfig> {
    resolve_target_profile(&mut config)?;
    validate_workflow_config(&config)?;
    if target_profile_validation_required(
        &config.workflow_type_id,
        &config,
        &TargetProfileOverrides::default(),
    ) {
        validate_target_profile(&config)?;
    }
    Ok(config)
}

/// Validate a workflow type definition.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
/// @requirement:REQ-EARS-WF-005
pub fn validate_workflow_type(workflow_type: &WorkflowType) -> Result<()> {
    // Check that workflow_type_id is not empty
    if workflow_type.workflow_type_id.is_empty() {
        return Err(ConfigError {
            message: "workflow_type_id cannot be empty".to_string(),
            source_path: None,
            kind: ConfigErrorKind::ValidationError,
        });
    }

    // Check that steps is not empty
    if workflow_type.steps.is_empty() {
        return Err(ConfigError {
            message: "workflow type must have at least one step".to_string(),
            source_path: None,
            kind: ConfigErrorKind::ValidationError,
        });
    }

    // Validate each step has required fields
    for step in &workflow_type.steps {
        if step.step_id.is_empty() {
            return Err(ConfigError {
                message: "step_id cannot be empty".to_string(),
                source_path: None,
                kind: ConfigErrorKind::ValidationError,
            });
        }
        if step.step_type.is_empty() {
            return Err(ConfigError {
                message: format!("step_type cannot be empty for step '{}'", step.step_id),
                source_path: None,
                kind: ConfigErrorKind::ValidationError,
            });
        }
    }

    // Graph-structural validation: reject invalid or unsafe routing (dangling
    // transition targets, duplicate outcome branches, unreachable required
    // steps, and direct fatal routes that bypass required post-PR collectors)
    // before the workflow is ever handed to the engine.
    // @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
    // @requirement:REQ-PRFU-018,REQ-PRFU-020
    if let Err(graph_errors) = validate_workflow_graph(workflow_type) {
        let message = graph_errors
            .iter()
            .map(|error| error.detail.clone())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(ConfigError {
            message: format!("invalid workflow graph: {}", message),
            source_path: None,
            kind: ConfigErrorKind::ValidationError,
        });
    }

    Ok(())
}

/// Validate a workflow config.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
/// @requirement:REQ-EARS-WF-005
pub fn validate_workflow_config(config: &WorkflowConfig) -> Result<()> {
    // Check that config_id is not empty
    if config.config_id.is_empty() {
        return Err(ConfigError {
            message: "config_id cannot be empty".to_string(),
            source_path: None,
            kind: ConfigErrorKind::ValidationError,
        });
    }

    // Check that workflow_type_id is not empty
    if config.workflow_type_id.is_empty() {
        return Err(ConfigError {
            message: "workflow_type_id cannot be empty".to_string(),
            source_path: None,
            kind: ConfigErrorKind::ValidationError,
        });
    }

    // Validate runtime fields
    if config.runtime.timeout_seconds == 0 {
        return Err(ConfigError {
            message: "runtime.timeout_seconds must be greater than 0".to_string(),
            source_path: None,
            kind: ConfigErrorKind::ValidationError,
        });
    }

    // Validate repo fields
    if config.repo.workspace_strategy.is_empty() {
        return Err(ConfigError {
            message: "repo.workspace_strategy cannot be empty".to_string(),
            source_path: None,
            kind: ConfigErrorKind::ValidationError,
        });
    }
    if config.repo.branch_template.is_empty() {
        return Err(ConfigError {
            message: "repo.branch_template cannot be empty".to_string(),
            source_path: None,
            kind: ConfigErrorKind::ValidationError,
        });
    }

    validate_repository_paths(config)?;
    validate_discovery_config(config)?;
    if let Some(manifest) = &config.command_manifest {
        validate_command_manifest(manifest)?;
    }
    validate_command_variable_shadowing(config)?;

    Ok(())
}
fn validate_repository_paths(config: &WorkflowConfig) -> Result<()> {
    for (field, path) in [
        (
            "repository.project_subdir",
            config.repo.project_subdir.as_deref(),
        ),
        (
            "repository.artifact_path_base",
            config.repo.artifact_path_base.as_deref(),
        ),
        (
            "repository.diff_path_base",
            config.repo.diff_path_base.as_deref(),
        ),
    ]
    .into_iter()
    .filter_map(|(field, value)| value.map(|value| (field, value)))
    {
        validate_repo_relative_path(field, path)?;
    }
    if config.repo.diff_path_normalization == DiffPathNormalization::BaseRelative
        && config.repo.diff_path_base.is_none()
    {
        return Err(ConfigError {
            message: "repository.diff_path_base is required when repository.diff_path_normalization is base_relative".to_string(),
            source_path: None,
            kind: ConfigErrorKind::ValidationError,
        });
    }
    Ok(())
}

fn validate_repo_relative_path(field: &str, path: &str) -> Result<()> {
    if Path::new(path).is_absolute() || path.split('/').any(|part| part == "..") {
        return Err(ConfigError {
            message: format!("{field} must be a relative path under the repository root"),
            source_path: None,
            kind: ConfigErrorKind::ValidationError,
        });
    }
    Ok(())
}

fn validate_command_variable_shadowing(config: &WorkflowConfig) -> Result<()> {
    if config.command_manifest.is_some()
        && config.variables.keys().any(|key| key.ends_with("_command"))
    {
        return command_manifest_error(
            "command manifests must not be combined with legacy *_command shell variables",
        );
    }
    Ok(())
}

pub fn command_manifest_entry<'a>(
    manifest: &'a CommandManifest,
    id: &str,
) -> Result<&'a CommandEntry> {
    manifest
        .commands
        .iter()
        .find(|entry| entry.id == id)
        .ok_or_else(|| ConfigError {
            message: format!("unknown command manifest id '{id}'"),
            source_path: None,
            kind: ConfigErrorKind::ValidationError,
        })
}

pub fn validate_command_manifest(manifest: &CommandManifest) -> Result<()> {
    let mut ids = HashSet::new();
    for entry in &manifest.commands {
        validate_command_entry(entry, &mut ids)?;
    }
    for (group_id, command_ids) in &manifest.groups {
        if group_id.is_empty() || command_ids.is_empty() {
            return command_manifest_error("command manifest groups require ids and command ids");
        }
        for command_id in command_ids {
            command_manifest_entry(manifest, command_id)?;
        }
    }
    Ok(())
}

fn validate_command_entry(entry: &CommandEntry, ids: &mut HashSet<String>) -> Result<()> {
    if entry.id.is_empty() || !ids.insert(entry.id.clone()) {
        return command_manifest_error(format!("duplicate or empty command id '{}'", entry.id));
    }
    if entry.argv.is_empty() || entry.argv.iter().any(|arg| arg.is_empty()) {
        return command_manifest_error(format!("command '{}' argv must not be empty", entry.id));
    }
    validate_command_paths(entry)?;
    validate_command_env(entry)?;
    validate_command_patterns(entry)?;
    validate_command_numbers(entry)
}

fn validate_command_paths(entry: &CommandEntry) -> Result<()> {
    for path in [
        entry.working_directory.as_deref(),
        entry.project_subdirectory.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        validate_command_relative_path(entry, path, "working directory")?;
    }
    for path in entry
        .run_if_missing_any
        .iter()
        .chain(entry.run_if_present_all.iter())
    {
        validate_command_relative_path(entry, path, "conditional path")?;
    }
    for path in &entry.remove_before_run {
        validate_command_relative_path(entry, path, "removal path")?;
        validate_command_removal_path(entry, path)?;
    }
    Ok(())
}

fn validate_command_relative_path(entry: &CommandEntry, path: &str, label: &str) -> Result<()> {
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || path.to_string_lossy().contains('\\')
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return command_manifest_error(format!(
            "command '{}' {label} '{}' must stay under work_dir",
            entry.id,
            path.display()
        ));
    }
    Ok(())
}

fn validate_command_removal_path(entry: &CommandEntry, path: &str) -> Result<()> {
    if !Path::new(path)
        .components()
        .any(|component| matches!(component, std::path::Component::Normal(_)))
    {
        return command_manifest_error(format!(
            "command '{}' removal path '{}' must not target work_dir itself",
            entry.id, path
        ));
    }
    Ok(())
}

fn validate_command_env(entry: &CommandEntry) -> Result<()> {
    for key in entry.env.keys() {
        if key.is_empty()
            || !key
                .chars()
                .all(|ch| ch == '_' || ch.is_ascii_uppercase() || ch.is_ascii_digit())
            || key.chars().next().is_some_and(|ch| ch.is_ascii_digit())
        {
            return command_manifest_error(format!(
                "command '{}' env key '{}' is invalid",
                entry.id, key
            ));
        }
    }
    Ok(())
}

fn validate_command_patterns(entry: &CommandEntry) -> Result<()> {
    for pattern in entry
        .stdout
        .required_patterns
        .iter()
        .chain(entry.stdout.forbidden_patterns.iter())
        .chain(entry.stderr.required_patterns.iter())
        .chain(entry.stderr.forbidden_patterns.iter())
    {
        regex::Regex::new(pattern).map_err(|err| ConfigError {
            message: format!(
                "command '{}' has invalid regex '{}': {err}",
                entry.id, pattern
            ),
            source_path: None,
            kind: ConfigErrorKind::ValidationError,
        })?;
    }
    Ok(())
}

fn validate_command_numbers(entry: &CommandEntry) -> Result<()> {
    if entry.timeout_seconds == Some(0) || entry.capture.limit_bytes == 0 {
        return command_manifest_error(format!(
            "command '{}' timeout and capture limits must be positive",
            entry.id
        ));
    }
    if entry.retry.max_attempts > 0 && entry.retry.retry_exit_codes.is_empty() {
        return command_manifest_error(format!(
            "command '{}' retry policy requires retry_exit_codes",
            entry.id
        ));
    }
    Ok(())
}

fn command_manifest_error(message: impl Into<String>) -> Result<()> {
    Err(ConfigError {
        message: message.into(),
        source_path: None,
        kind: ConfigErrorKind::ValidationError,
    })
}

/// Validate the resolved discovery rules for a workflow config.
///
/// Rejects configs where include/exclude label sets intersect (an issue could
/// never be both required and forbidden) and configs where discovery is enabled
/// but no repository can be resolved from `[discovery]` or `[variables]`.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P01
/// @requirement:REQ-DAEMON-DISCOVERY-001
fn validate_discovery_config(config: &WorkflowConfig) -> Result<()> {
    let discovery = resolve_discovery_config(config);

    let include: HashSet<&str> = discovery
        .include_labels
        .iter()
        .map(String::as_str)
        .collect();
    for label in &discovery.exclude_labels {
        if include.contains(label.as_str()) {
            return Err(ConfigError {
                message: format!(
                    "discovery include_labels and exclude_labels both contain '{}'",
                    label
                ),
                source_path: None,
                kind: ConfigErrorKind::ValidationError,
            });
        }
    }

    if discovery.enabled && discovery.repo.as_deref().unwrap_or("").is_empty() {
        return Err(ConfigError {
            message:
                "discovery.enabled is true but no repository could be resolved (set discovery.repo or variables.target_repo)"
                    .to_string(),
            source_path: None,
            kind: ConfigErrorKind::ValidationError,
        });
    }

    Ok(())
}

/// Resolve effective discovery rules for a config, filling unset fields from
/// the config's `[variables]` table and built-in defaults.
///
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P01
/// @requirement:REQ-DAEMON-DISCOVERY-001
#[must_use]
pub fn resolve_discovery_config(config: &WorkflowConfig) -> DiscoveryConfig {
    let raw = config.discovery.clone().unwrap_or_default();
    let var = |key: &str| config.variables.get(key).cloned();

    let repo = raw
        .repo
        .filter(|s| !s.is_empty())
        .or_else(|| var("target_repo"));

    let include_labels = if raw.include_labels.is_empty() {
        var("ok_label").into_iter().collect()
    } else {
        raw.include_labels
    };

    let exclude_labels = if raw.exclude_labels.is_empty() {
        var("luther_label").into_iter().collect()
    } else {
        raw.exclude_labels
    };

    let issue_states = if raw.issue_states.is_empty() {
        vec!["open".to_string()]
    } else {
        raw.issue_states
    };

    let assignee_filter = raw.assignee_filter.or_else(|| var("assignee"));

    let milestone_order = Some(raw.milestone_order.unwrap_or_else(|| "semver".to_string()));
    let max_concurrent_runs = Some(raw.max_concurrent_runs.unwrap_or(1));
    let poll_interval_secs = Some(raw.poll_interval_secs.unwrap_or(300));

    DiscoveryConfig {
        enabled: raw.enabled,
        repo,
        include_labels,
        exclude_labels,
        // Default active parent detection to the configured Luther working
        // label, matching the DiscoveryConfig default contract.
        active_parent_label: raw.active_parent_label.or_else(|| var("luther_label")),
        issue_states,
        assignee_filter,
        milestone_order,
        max_concurrent_runs,
        poll_interval_secs,
        max_concurrent_active_runs: raw.max_concurrent_active_runs,
        max_concurrent_runs_per_repository: raw.max_concurrent_runs_per_repository,
        max_concurrent_runs_per_config: raw.max_concurrent_runs_per_config,
        route_parent_issues: raw.route_parent_issues,
        parent_workflow_type_id: Some(
            raw.parent_workflow_type_id
                .unwrap_or_else(|| "parent-issue-orchestrator-v1".to_string()),
        ),
        parent_config_id: raw.parent_config_id,
        skip_children_of_active_parents: raw.skip_children_of_active_parents,
    }
}

/// Validate that a workflow config matches its referenced workflow type.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
/// @requirement:REQ-EARS-WF-005
pub fn validate_config_matches_type(
    config: &WorkflowConfig,
    workflow_type: &WorkflowType,
) -> Result<()> {
    if config.workflow_type_id != workflow_type.workflow_type_id {
        return Err(ConfigError {
            message: format!(
                "Config workflow_type_id '{}' does not match workflow type '{}'",
                config.workflow_type_id, workflow_type.workflow_type_id
            ),
            source_path: None,
            kind: ConfigErrorKind::MismatchedType,
        });
    }

    Ok(())
}

/// A template token that could not be resolved against the available variable set.
///
/// Reported by dry-run validation so unresolved interpolation tokens surface
/// before a step executes, rather than only when the broken value is used.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P11
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedToken {
    pub step_id: String,
    /// Dotted path to the offending string leaf, e.g. `parameters.command`.
    pub parameter_path: String,
    pub token_name: String,
}

/// A consumed artifact that has no producing step.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P11
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingArtifactProducer {
    pub consumer_step_id: String,
    pub artifact_name: String,
}

/// Recursively walk a JSON value, collecting unresolved tokens from string leaves.
///
/// The `context_map` sub-object is skipped as a token source: its values are
/// `jq` dot-paths/keys, not interpolation templates.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P11
fn collect_unresolved_in_value(
    step_id: &str,
    path: &str,
    value: &serde_json::Value,
    available: &HashSet<String>,
    out: &mut Vec<UnresolvedToken>,
) {
    match value {
        serde_json::Value::String(s) => {
            for token in extract_tokens(s) {
                if token_is_resolvable(&token, available) {
                    continue;
                }
                out.push(UnresolvedToken {
                    step_id: step_id.to_string(),
                    parameter_path: path.to_string(),
                    token_name: token,
                });
            }
        }
        serde_json::Value::Array(items) => {
            for (idx, item) in items.iter().enumerate() {
                let child = format!("{path}[{idx}]");
                collect_unresolved_in_value(step_id, &child, item, available, out);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, item) in map {
                // context_map values are jq paths, not interpolation templates.
                if key == "context_map" {
                    continue;
                }
                let child = format!("{path}.{key}");
                collect_unresolved_in_value(step_id, &child, item, available, out);
            }
        }
        _ => {}
    }
}

/// Whether a token name resolves against the available set.
///
/// Resolution is exact-match only, mirroring the runtime resolver
/// `StepContext::get`: namespaced tokens (`a.b`) are looked up as a strict
/// `namespace.name` key, never via a bare-name (`b`) fallback. The available
/// set already registers every statically-declarable output in both bare and
/// namespaced forms (see `register_context_map_outputs` /
/// `register_known_executor_outputs`), so a bare-name fallback here would make
/// dry-run validation more permissive than runtime and suppress genuine
/// unresolved-token errors.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P11
fn token_is_resolvable(token: &str, available: &HashSet<String>) -> bool {
    available.contains(token)
}

/// Validate a single step's parameters for unresolved interpolation tokens.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P11
#[must_use]
pub fn validate_step_tokens(step: &StepDef, available: &HashSet<String>) -> Vec<UnresolvedToken> {
    let mut out = Vec::new();
    if let Some(params) = &step.parameters {
        collect_unresolved_in_value(&step.step_id, "parameters", params, available, &mut out);
    }
    out
}

/// Statically-known context variables produced by a given executor `step_type`.
///
/// These executors set context values at runtime via `context.set(...)` rather
/// than through a declarative `context_map`, so they must be enumerated here to
/// avoid false positives when later steps interpolate them. Keep in lockstep
/// with the `context.set(...)` calls in `src/engine/executors/`.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P11
fn known_executor_outputs(step_type: &str) -> &'static [&'static str] {
    match step_type {
        "github_pr_identity" => &[
            "repository_owner",
            "repository_name",
            "pr_number",
            "head_ref",
            "head_sha",
            "base_ref",
            "base_sha",
        ],
        _ => &[],
    }
}

/// Register an executor's statically-known outputs (by `step_type`).
///
/// Registered both bare and namespaced (`<step_id>.<name>`).
/// @plan:PLAN-20260408-LLXPRT-FIRST.P11
fn register_known_executor_outputs(step: &StepDef, available: &mut HashSet<String>) {
    for name in known_executor_outputs(&step.step_type) {
        available.insert((*name).to_string());
        available.insert(format!("{}.{}", step.step_id, name));
    }
}

/// Register a `context_map` declaration's keys as statically-known step outputs.
///
/// Each key is registered both bare (`pr_number`) and namespaced
/// (`<step_id>.pr_number`) so legitimate cross-step references are not flagged.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P11
fn register_context_map_outputs(step: &StepDef, available: &mut HashSet<String>) {
    let Some(params) = &step.parameters else {
        return;
    };
    let Some(context_map) = params.get("context_map").and_then(|v| v.as_object()) else {
        return;
    };
    for key in context_map.keys() {
        available.insert(key.clone());
        available.insert(format!("{}.{}", step.step_id, key));
    }
}

/// Build the set of variable names that runtime interpolation can resolve.
///
/// Mirrors runtime seeding to avoid false positives: config variables, the
/// always-present built-ins, the `issue_number` fallback, and every
/// statically-declarable `context_map` output (bare and namespaced).
///
/// Known limitation: tokens produced at runtime by non-`context_map` executors
/// cannot be proven statically and would be reported if referenced.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P11
#[must_use]
pub fn build_available_variables(wf: &WorkflowType, config: &WorkflowConfig) -> HashSet<String> {
    let mut available: HashSet<String> = config.variables.keys().cloned().collect();
    // Built-ins always seeded into StepContext.
    available.insert("work_dir".to_string());
    available.insert("repo_root".to_string());
    available.insert("project_subdir".to_string());
    available.insert("project_dir".to_string());
    available.insert("artifact_base_dir".to_string());
    available.insert("diff_path_base".to_string());
    available.insert("diff_path_base_dir".to_string());
    available.insert("diff_path_normalization".to_string());
    available.insert("run_id".to_string());
    available.insert("current_step_id".to_string());
    // Statically-declarable step outputs from context_map declarations and
    // from executors that set known context variables at runtime.
    for step in &wf.steps {
        register_context_map_outputs(step, &mut available);
        register_known_executor_outputs(step, &mut available);
    }
    // Documented issue_number -> primary_issue_number fallback. Evaluated after
    // step outputs are registered so a `primary_issue_number` produced by a
    // step's context_map (not just config variables) also seeds the alias,
    // matching runtime resolution and avoiding dry-run false positives.
    if available.contains("primary_issue_number") {
        available.insert("issue_number".to_string());
    }
    available
}

/// Validate every step in a workflow for unresolved interpolation tokens.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P11
#[must_use]
pub fn validate_workflow_tokens(
    wf: &WorkflowType,
    config: &WorkflowConfig,
) -> Vec<UnresolvedToken> {
    let available = build_available_variables(wf, config);
    let mut out = Vec::new();
    for step in &wf.steps {
        out.extend(validate_step_tokens(step, &available));
    }
    out
}

/// Validate that every consumed artifact has a producing step.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P11
#[must_use]
pub fn validate_artifact_dependencies(wf: &WorkflowType) -> Vec<MissingArtifactProducer> {
    let mut produced: HashSet<&str> = HashSet::new();
    for step in &wf.steps {
        if let Some(names) = &step.produces {
            for name in names {
                produced.insert(name.as_str());
            }
        }
    }
    let mut out = Vec::new();
    for step in &wf.steps {
        if let Some(names) = &step.consumes {
            for name in names {
                if !produced.contains(name.as_str()) {
                    out.push(MissingArtifactProducer {
                        consumer_step_id: step.step_id.clone(),
                        artifact_name: name.clone(),
                    });
                }
            }
        }
    }
    out
}
