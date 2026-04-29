/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
/// Configuration loading and resolution for workflow types and configs.

use std::path::Path;

use crate::workflow::schema::{WorkflowConfig, WorkflowRunRef, WorkflowType};

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
        return parse_workflow_type_toml(&content);
    }

    // Try production layout JSON
    if prod_json.exists() {
        let content = std::fs::read_to_string(&prod_json).map_err(|e| ConfigError {
            message: format!("Failed to read workflow type file: {}", e),
            source_path: Some(prod_json.to_string_lossy().to_string()),
            kind: ConfigErrorKind::NotFound,
        })?;
        return parse_workflow_type_json(&content);
    }

    // Fall back to test fixture layout TOML
    if valid_toml.exists() {
        let content = std::fs::read_to_string(&valid_toml).map_err(|e| ConfigError {
            message: format!("Failed to read workflow type file: {}", e),
            source_path: Some(valid_toml.to_string_lossy().to_string()),
            kind: ConfigErrorKind::NotFound,
        })?;
        return parse_workflow_type_toml(&content);
    }

    // Fall back to test fixture layout JSON
    if valid_json.exists() {
        let content = std::fs::read_to_string(&valid_json).map_err(|e| ConfigError {
            message: format!("Failed to read workflow type file: {}", e),
            source_path: Some(valid_json.to_string_lossy().to_string()),
            kind: ConfigErrorKind::NotFound,
        })?;
        return parse_workflow_type_json(&content);
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

/// Parse workflow config from TOML string.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub fn parse_workflow_config_toml(toml_str: &str) -> Result<WorkflowConfig> {
    // TOML uses [repository] and [guards] - the struct handles aliasing
    toml::from_str(toml_str).map_err(|e| ConfigError {
        message: format!("Failed to parse workflow config TOML: {}", e),
        source_path: None,
        kind: ConfigErrorKind::ParseError,
    })
}

/// Parse workflow config from JSON string.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub fn parse_workflow_config_json(json_str: &str) -> Result<WorkflowConfig> {
    // JSON uses "repository" and "guard_limits" - the struct handles aliasing
    serde_json::from_str(json_str).map_err(|e| ConfigError {
        message: format!("Failed to parse workflow config JSON: {}", e),
        source_path: None,
        kind: ConfigErrorKind::ParseError,
    })
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

    Ok(())
}

/// Validate that a workflow config matches its referenced workflow type.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
/// @requirement:REQ-EARS-WF-005
pub fn validate_config_matches_type(config: &WorkflowConfig, workflow_type: &WorkflowType) -> Result<()> {
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
