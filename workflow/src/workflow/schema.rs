/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
/// Schema definitions for workflow types, configurations, and runtime references.
use std::collections::HashMap;

/// Declarative topology and transitions for a workflow type.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
/// @requirement:REQ-EARS-WF-001,REQ-EARS-WF-006
#[derive(Debug, Clone, serde::Deserialize)]
pub struct WorkflowType {
    pub workflow_type_id: String,
    pub steps: Vec<StepDef>,
    #[serde(default)]
    pub transitions: Vec<TransitionDef>,
    #[serde(default)]
    pub guards: GuardConfig,
}

/// Runtime profile for a workflow instance.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
/// @requirement:REQ-EARS-WF-007
#[derive(Debug, Clone, serde::Deserialize)]
pub struct WorkflowConfig {
    pub config_id: String,
    pub workflow_type_id: String,
    pub runtime: RuntimeConfig,
    #[serde(rename = "repository", alias = "repo")]
    pub repo: RepoConfig,
    #[serde(rename = "guards", alias = "guard_limits")]
    pub guard_limits: GuardLimits,
    /// Config variables loaded into StepContext at run start.
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
    /// @requirement:REQ-LF-PROF-003
    #[serde(default)]
    pub variables: HashMap<String, String>,
}

/// Bound runtime identity for a workflow run.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
/// @requirement:REQ-EARS-ARCH-004
#[derive(Debug, Clone)]
pub struct WorkflowRunRef {
    pub workflow_type_id: String,
    pub config_id: String,
    pub run_id: String,
}

impl WorkflowRunRef {
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
    /// @requirement:REQ-EARS-ARCH-004
    pub fn new(
        workflow_type_id: impl Into<String>,
        config_id: impl Into<String>,
        run_id: impl Into<String>,
    ) -> Self {
        Self {
            workflow_type_id: workflow_type_id.into(),
            config_id: config_id.into(),
            run_id: run_id.into(),
        }
    }
}

/// Definition of a workflow step.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
#[derive(Debug, Clone, serde::Deserialize)]
pub struct StepDef {
    pub step_id: String,
    pub step_type: String,
    pub description: Option<String>,
    pub parameters: Option<serde_json::Value>,
}

/// Definition of a transition between steps.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
/// @plan:PLAN-20260408-LLXPRT-FIRST.P12
/// @requirement:REQ-LF-LOOP-001
#[derive(Debug, Clone, serde::Deserialize)]
pub struct TransitionDef {
    pub from: String,
    pub to: String,
    pub condition: Option<String>,
    #[serde(default)]
    pub max_iterations: Option<u32>,
}

/// Guard configuration for workflow transitions.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
#[derive(Debug, Clone, serde::Deserialize, Default)]
pub struct GuardConfig {
    #[serde(default)]
    pub max_retries: Option<u32>,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub require_approval: Option<bool>,
}

/// Runtime configuration parameters.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RuntimeConfig {
    pub timeout_seconds: u64,
    pub max_retries: u32,
    pub parallel_steps: Option<u32>,
    pub log_level: Option<String>,
}

/// Repository workspace and branch configuration.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RepoConfig {
    pub workspace_strategy: String,
    pub branch_template: String,
    pub base_branch: Option<String>,
    pub workspace_root: Option<String>,
}

/// Guard limits for workflow execution.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
#[derive(Debug, Clone, serde::Deserialize)]
pub struct GuardLimits {
    pub max_iterations: Option<u32>,
    pub max_file_changes: Option<u32>,
    pub max_tokens: Option<u64>,
    pub max_cost: Option<f64>,
}
