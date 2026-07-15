/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
/// Schema definitions for workflow types, configurations, and runtime references.
use std::collections::{BTreeMap, HashMap};

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
    pub discovery: Option<DiscoveryConfig>,
    /// Parent/sub-issue orchestration policy (built-in defaults when unset).
    #[serde(default)]
    pub parent_orchestration: ParentOrchestrationConfig,
    /// Optional argv-only command manifest for repository-specific gates.
    pub command_manifest: Option<CommandManifest>,
    /// Optional config-first target profile used to derive legacy runtime
    /// variables and repository-specific command/check conventions.
    pub target_profile: Option<TargetProfileConfig>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TargetProfileConfig {
    pub identity: TargetIdentityConfig,
    pub paths: TargetProfilePathConfig,
    pub issue_conventions: TargetIssueConventions,
    pub pr_conventions: TargetPrConventions,
    pub templates: TargetTemplateConfig,
    pub diff_policy: TargetDiffPolicyConfig,
    pub command_groups: BTreeMap<String, String>,
    pub pr_checks: TargetPrCheckPolicy,
    pub auth: TargetAuthConfig,
    pub preflight: TargetPreflightConfig,
    pub prompt_guidance: TargetPromptGuidance,
    pub bootstrap: TargetBootstrapConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TargetIdentityConfig {
    pub repo: Option<String>,
    pub owner: Option<String>,
    pub name: Option<String>,
    pub base_branch: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TargetProfilePathConfig {
    pub project_subdir: Option<String>,
    pub default_command_cwd: Option<String>,
    pub work_dir: Option<String>,
    pub artifact_dir: Option<String>,
    pub artifact_path_base: Option<String>,
    pub diff_path_base: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TargetIssueConventions {
    pub assignee: Option<String>,
    pub ok_label: Option<String>,
    pub luther_label: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TargetPrConventions {
    pub title_prefix: Option<String>,
    pub body_guidance: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TargetTemplateConfig {
    pub branch: Option<String>,
    pub pr_title: Option<String>,
    pub pr_body: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TargetDiffPolicyConfig {
    pub required_changed_path_pattern: Option<String>,
    pub required_path_regex: Option<String>,
    pub failure_message: Option<String>,
    pub allowed_path_patterns: Vec<String>,
    pub required_path_patterns: Vec<String>,
    pub commit_exclude_pathspecs: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TargetPrCheckPolicy {
    pub required: Vec<String>,
    pub optional: Vec<String>,
    pub ignored: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TargetAuthConfig {
    pub requirements: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TargetPreflightConfig {
    pub expectations: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TargetPromptGuidance {
    pub ecosystem_name: String,
    pub planning: String,
    pub implementation: String,
    pub review: String,
    pub verification: String,
    pub style: String,
    pub fixture_parity: String,
    pub forbidden_actions: String,
    pub remediation_scope: String,
    pub command_manifest_summary: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TargetBootstrapConfig {
    pub command_group: Option<String>,
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
    pub produces: Option<Vec<String>>,
    /// Logical artifact names this step consumes. Each must be produced by some step.
    /// @plan:PLAN-20260408-LLXPRT-FIRST.P11
    pub consumes: Option<Vec<String>>,
    /// Explicit terminal marker. When `Some(true)` the step is a terminal step
    /// and must not declare any outgoing transitions. Steps with
    /// `step_type == "post_pr_failure_terminal"` are also treated as terminal
    /// for back-compat even when this is `None`.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
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
    pub max_iterations: Option<u32>,
}

/// Guard configuration for workflow transitions.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
#[derive(Debug, Clone, serde::Deserialize, Default)]
pub struct GuardConfig {
    pub max_retries: Option<u32>,
    pub timeout_seconds: Option<u64>,
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
    pub project_subdir: Option<String>,
    pub artifact_path_base: Option<String>,
    pub diff_path_base: Option<String>,
    #[serde(default)]
    pub diff_path_normalization: DiffPathNormalization,
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

/// Default upper bound for waiting on child workflow/merge progress.
pub const DEFAULT_MAX_CHILD_MERGE_WAIT_SECONDS: u64 = 86_400;

/// Parent/sub-issue orchestration policy for parent issue workflows.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ParentOrchestrationConfig {
    pub auto_merge_children: bool,
    pub wait_for_human_merge: bool,
    pub merge_poll_interval_seconds: u64,
    pub max_child_merge_wait_seconds: Option<u64>,
    pub child_workflow_type_id: String,
    pub child_config_id: String,
}

impl Default for ParentOrchestrationConfig {
    fn default() -> Self {
        Self {
            auto_merge_children: false,
            wait_for_human_merge: true,
            merge_poll_interval_seconds: 300,
            max_child_merge_wait_seconds: None,
            child_workflow_type_id: "llxprt-issue-fix-v1".to_string(),
            child_config_id: "llxprt-code".to_string(),
        }
    }
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
    pub repo: Option<String>,
    /// Labels an issue must have to be eligible. Defaults to
    /// `[variables.ok_label]` when unset.
    #[serde(default)]
    pub include_labels: Vec<String>,
    /// Labels that make an issue ineligible. Defaults to
    /// `[variables.luther_label]` when unset.
    #[serde(default)]
    pub exclude_labels: Vec<String>,
    /// Label that marks a parent issue as actively being orchestrated.
    /// Defaults to `variables.luther_label`.
    pub active_parent_label: Option<String>,
    /// Issue states to query. Defaults to `["open"]` when unset.
    #[serde(default)]
    pub issue_states: Vec<String>,
    /// Label whose application authorizes issue execution.
    pub approval_label: Option<String>,
    /// GitHub actor permitted to apply the approval label.
    pub approval_actor: Option<String>,
    /// Operating user assigned after the daemon wins the issue lease.
    pub claim_assignee: Option<String>,
    /// Working label applied after the daemon wins the issue lease.
    pub claim_label: Option<String>,
    /// Milestone ordering strategy: `"semver"` or `"none"`. Defaults to
    /// `"semver"` when unset.
    pub milestone_order: Option<String>,
    /// Maximum simultaneous active runs for this config. Defaults to 1.
    pub max_concurrent_runs: Option<u32>,
    /// Discovery poll interval in seconds. Defaults to 300.
    pub poll_interval_secs: Option<u64>,
    /// Global active run ceiling used by supervisor schedulers.
    pub max_concurrent_active_runs: Option<u32>,
    /// Per-repository active run ceiling for multi-target daemons.
    pub max_concurrent_runs_per_repository: Option<u32>,
    /// Per-config active run ceiling for supervisor schedulers; when set, it
    /// takes precedence over legacy `max_concurrent_runs`.
    pub max_concurrent_runs_per_config: Option<u32>,
    /// Route parent issues with native sub-issues to the parent orchestrator.
    #[serde(default)]
    pub route_parent_issues: bool,
    /// Workflow type used when a parent issue is discovered.
    pub parent_workflow_type_id: Option<String>,
    /// Workflow config used when a parent issue is discovered.
    pub parent_config_id: Option<String>,
    /// Skip child issues when their parent already has the Luther working label.
    #[serde(default)]
    pub skip_children_of_active_parents: bool,
}

/// Supervisor-level configuration for the multi-target daemon scheduler.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, Default, PartialEq, Eq)]
pub struct DaemonSchedulerConfig {
    /// Global ceiling for active, non-waiting workflow runs.
    pub max_concurrent_active_runs: Option<u32>,
    /// Per-workflow-config ceiling for active, non-waiting runs.
    pub max_concurrent_runs_per_config: Option<u32>,
    /// Per-repository ceiling for active, non-waiting runs.
    pub max_concurrent_runs_per_repository: Option<u32>,
    /// Daemon scheduler poll interval in seconds. Defaults to 300.
    #[serde(alias = "poll_interval_secs")]
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
