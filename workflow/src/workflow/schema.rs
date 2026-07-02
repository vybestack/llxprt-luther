/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
/// Schema definitions for workflow types, configurations, and runtime references.
use std::collections::HashMap;

use crate::workflow::command_manifest::CommandManifest;

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
    /// Optional daemon issue-discovery rules for this workflow config.
    /// @plan:PLAN-20260415-DAEMON-DISCOVERY.P01
    /// @requirement:REQ-DAEMON-DISCOVERY-001
    #[serde(default)]
    pub discovery: Option<DiscoveryConfig>,
    /// Optional argv-only command manifest for repository-specific gates.
    #[serde(default)]
    pub command_manifest: Option<CommandManifest>,
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
    /// Logical artifact names this step produces (e.g. `"plan"`, `"verify_report"`).
    /// Used by dry-run artifact-dependency validation. Not file paths.
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P11
    #[serde(default)]
    pub produces: Option<Vec<String>>,
    /// Logical artifact names this step consumes. Each must be produced by some step.
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P11
    #[serde(default)]
    pub consumes: Option<Vec<String>>,
    /// Explicit terminal marker. When `Some(true)` the step is a terminal step
    /// and must not declare any outgoing transitions. Steps with
    /// `step_type == "post_pr_failure_terminal"` are also treated as terminal
    /// for back-compat even when this is `None`.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
    #[serde(default)]
    pub terminal: Option<bool>,
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
    #[serde(default)]
    pub project_subdir: Option<String>,
    #[serde(default)]
    pub artifact_path_base: Option<String>,
    #[serde(default)]
    pub diff_path_base: Option<String>,
    #[serde(default)]
    pub diff_path_normalization: Option<DiffPathNormalization>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffPathNormalization {
    #[default]
    RepoRelative,
    BaseRelative,
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

/// Daemon issue-discovery rules describing how to find eligible issues for a
/// repository and how the daemon should claim/launch work for them.
///
/// All fields are optional in the on-disk config; unset fields are filled by
/// [`crate::workflow::config_loader::resolve_discovery_config`] from the
/// config's `[variables]` table and built-in defaults so existing configs keep
/// working without changes.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P01
/// @requirement:REQ-DAEMON-DISCOVERY-001
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, Default, PartialEq, Eq)]
pub struct DiscoveryConfig {
    /// When false (default) the daemon keeps heartbeat-only behavior and does
    /// not discover/launch work for this config.
    #[serde(default)]
    pub enabled: bool,
    /// `owner/name` repository slug. Defaults from `variables.target_repo`.
    #[serde(default)]
    pub repo: Option<String>,
    /// Labels an issue must have to be eligible. Defaults to
    /// `[variables.ok_label]` when unset.
    #[serde(default)]
    pub include_labels: Vec<String>,
    /// Labels that make an issue ineligible. Defaults to
    /// `[variables.luther_label]` when unset.
    #[serde(default)]
    pub exclude_labels: Vec<String>,
    /// Issue states to query. Defaults to `["open"]` when unset.
    #[serde(default)]
    pub issue_states: Vec<String>,
    /// Required assignee filter. `Some("")` means unassigned (matching the
    /// legacy `select_issue` behavior). Defaults from `variables.assignee`.
    #[serde(default)]
    pub assignee_filter: Option<String>,
    /// Milestone ordering strategy: `"semver"` or `"none"`. Defaults to
    /// `"semver"` when unset.
    #[serde(default)]
    pub milestone_order: Option<String>,
    /// Maximum simultaneous active runs for this config. Defaults to 1.
    #[serde(default)]
    pub max_concurrent_runs: Option<u32>,
    /// Discovery poll interval in seconds. Defaults to 300.
    #[serde(default)]
    pub poll_interval_secs: Option<u64>,
    /// Global active run ceiling used by supervisor schedulers.
    #[serde(default)]
    pub max_concurrent_active_runs: Option<u32>,
    /// Per-repository active run ceiling for multi-target daemons.
    #[serde(default)]
    pub max_concurrent_runs_per_repository: Option<u32>,
    /// Per-config active run ceiling for supervisor schedulers; when set, it
    /// takes precedence over legacy `max_concurrent_runs`.
    #[serde(default)]
    pub max_concurrent_runs_per_config: Option<u32>,
}

/// Supervisor-level configuration for the multi-target daemon scheduler.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, Default, PartialEq, Eq)]
pub struct DaemonSchedulerConfig {
    /// Global ceiling for active, non-waiting workflow runs.
    #[serde(default)]
    pub max_concurrent_active_runs: Option<u32>,
    /// Per-workflow-config ceiling for active, non-waiting runs.
    #[serde(default)]
    pub max_concurrent_runs_per_config: Option<u32>,
    /// Per-repository ceiling for active, non-waiting runs.
    #[serde(default)]
    pub max_concurrent_runs_per_repository: Option<u32>,
    /// Daemon scheduler poll interval in seconds. Defaults to 300.
    #[serde(default, alias = "poll_interval_secs")]
    pub poll_interval_seconds: Option<u64>,
    /// Workflow config targets supervised by this scheduler.
    #[serde(default)]
    pub targets: Vec<DaemonTargetConfig>,
}

/// A single workflow-config target inside [`DaemonSchedulerConfig::targets`].
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, Default, PartialEq, Eq)]
pub struct DaemonTargetConfig {
    /// The workflow config id to supervise.
    pub config_id: String,
}
