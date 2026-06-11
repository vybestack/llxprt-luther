/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
/// Configuration loading and resolution for workflow types and configs.
use std::collections::HashSet;
use std::path::Path;

use crate::engine::executor::extract_tokens;
use crate::workflow::schema::{StepDef, WorkflowConfig, WorkflowRunRef, WorkflowType};
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

    Ok(())
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
///
/// Existence-only check: the union of all steps' `produces` must cover every
/// step's `consumes`. Absent/empty `produces`/`consumes` are no-ops, keeping
/// existing workflows backward-compatible.
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
