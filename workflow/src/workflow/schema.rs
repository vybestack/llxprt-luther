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
    /// Whether this run requires an observed, verified merge before it may
    /// reach terminal `Completed` semantics. When `true`, the normal runner
    /// completion path writes [`crate::persistence::run_metadata::RunStatus::ReviewReady`]
    /// instead of `Completed`; the run only reaches `Merged` via
    /// [`crate::engine::recovery::typed_merge::complete_typed_merge`], which
    /// atomically commits a typed merge artifact and the status transition.
    /// Defaults to `false` for full backward compatibility with existing
    /// non-merge workflows. [B12/C11]
    /// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
    /// @requirement:REQ-RP-010
    #[serde(default)]
    pub merge_required: bool,
    /// The expected PR merge strategy for a merge-required run. [P17]
    ///
    /// When `merge_required` is `true`, this field is the **authoritative**
    /// strategy evidence that the typed merge verifier cross-checks against
    /// the observed merge structure. The verifier never guesses: if the
    /// observed merge structure is inconsistent with this declared strategy,
    /// it fails closed. If `merge_required` is `true` but this is `None`, the
    /// typed merge completion fails closed (no implicit default).
    /// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
    /// @requirement:REQ-RP-010
    #[serde(default)]
    pub merge_strategy: Option<MergeStrategyConfig>,
    /// Optional argv-only command manifest for repository-specific gates.
    pub command_manifest: Option<CommandManifest>,
    /// Optional config-first target profile used to derive legacy runtime
    /// variables and repository-specific command/check conventions.
    pub target_profile: Option<TargetProfileConfig>,
}

/// @plan:PLAN-20260715-SCOPE-CONTROL
/// Optional scope-control policy attached to a target profile. When present and
/// enabled it constrains the change budget, review caps, subsystems, dependency
/// manifests, mandatory command-manifest groups, and measurement policy for a
/// workflow run.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScopeControlConfig {
    /// Master switch; when `false` every policy below is inert.
    pub enabled: bool,
    /// Numeric change-budget ceiling.
    pub budget: ScopeBudgetConfig,
    /// Bounded review-remediation loop caps.
    pub review_caps: ScopeReviewCapsConfig,
    /// Declared subsystems with their normalized path prefixes.
    #[serde(default)]
    pub subsystems: Vec<ScopeSubsystemConfig>,
    /// Declared dependency manifests and their section paths.
    #[serde(default)]
    pub dependency_manifests: Vec<ScopeDependencyManifestConfig>,
    /// Logical command-manifest group names that must remain present.
    #[serde(default)]
    pub mandatory_command_groups: Vec<String>,
    /// Command used for partial-compile checks after a timeout freeze.
    pub partial_compile_command: Option<String>,
    /// Command-manifest group for partial-compile checks.
    pub partial_compile_group: Option<String>,
    /// Policy controlling how patch growth is measured.
    pub measurement: ScopeMeasurementConfig,
    /// Mandatory PR gates that must not be weakened or removed.
    #[serde(default)]
    pub mandatory_gates: Vec<String>,
}

/// @plan:PLAN-20260715-SCOPE-CONTROL
/// Numeric change-budget ceiling.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScopeBudgetConfig {
    pub max_files_changed: u32,
    pub max_added_lines: u32,
    pub max_new_modules: u32,
    pub max_dependencies_added: u32,
    pub max_public_apis_added: u32,
}

impl Default for ScopeBudgetConfig {
    fn default() -> Self {
        Self {
            max_files_changed: 1,
            max_added_lines: 1,
            max_new_modules: 1,
            max_dependencies_added: 0,
            max_public_apis_added: 1,
        }
    }
}

/// @plan:PLAN-20260715-SCOPE-CONTROL
/// Bounded review-remediation loop caps.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScopeReviewCapsConfig {
    pub initial_full_reviews: u32,
    pub max_delta_reviews: u32,
    pub final_acceptance_reviews: u32,
    pub max_mutating_remediation_rounds: u32,
}

impl Default for ScopeReviewCapsConfig {
    fn default() -> Self {
        Self {
            initial_full_reviews: 1,
            max_delta_reviews: 2,
            final_acceptance_reviews: 1,
            max_mutating_remediation_rounds: 2,
        }
    }
}

/// @plan:PLAN-20260715-SCOPE-CONTROL
/// A declared subsystem with its normalized repository-relative path prefixes.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScopeSubsystemConfig {
    pub id: String,
    #[serde(default)]
    pub paths: Vec<String>,
}

/// @plan:PLAN-20260715-SCOPE-CONTROL
/// A declared dependency manifest and the sections that may receive additions.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScopeDependencyManifestConfig {
    pub path: String,
    #[serde(default)]
    pub sections: Vec<String>,
}

/// @plan:PLAN-20260715-SCOPE-CONTROL
/// Policy controlling how patch growth is measured.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScopeMeasurementConfig {
    /// File extensions treated as source files for module/API counting.
    #[serde(default = "default_source_extensions")]
    pub source_extensions: Vec<String>,
    /// Regex patterns matching public Rust API surface lines.
    #[serde(default)]
    pub public_api_regexes: Vec<String>,
    /// Whether rename inference is disabled during measurement.
    pub disable_rename_inference: bool,
    /// Whether untracked files are enumerated explicitly.
    pub enumerate_untracked: bool,
}

impl Default for ScopeMeasurementConfig {
    fn default() -> Self {
        Self {
            source_extensions: default_source_extensions(),
            public_api_regexes: Vec::new(),
            disable_rename_inference: true,
            enumerate_untracked: true,
        }
    }
}

fn default_source_extensions() -> Vec<String> {
    vec!["rs".to_string()]
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
    /// @plan:PLAN-20260715-SCOPE-CONTROL
    /// Optional scope-control policy. Serde-defaulted so existing configs are
    /// unaffected; validated only when [`ScopeControlConfig::enabled`] is true.
    pub scope_control: ScopeControlConfig,
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
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P06
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
    /// Explicit recovery policy declared on the canonical step definition.
    /// When present, takes precedence over `SAFE_RERUN_STEPS` classification.
    /// Persisted in canonical workflow bytes (canonicalize_workflow_type) so
    /// the capsule envelope digest covers it. [B7]
    /// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P06
    /// @requirement:REQ-RP-005
    #[serde(default)]
    pub recovery_policy: Option<crate::engine::recovery::policy::StepRecoveryPolicy>,
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

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffPathNormalization {
    #[default]
    RepoRelative,
    BaseRelative,
}

/// Config-declared expected merge strategy for a merge-required run. [P17]
///
/// This is the authoritative strategy evidence that the typed merge verifier
/// cross-checks against the observed merge structure (parent count). The
/// verifier never guesses; if `merge_required` is `true` but no strategy is
/// declared, typed merge completion fails closed.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeStrategyConfig {
    /// A standard merge commit (2+ parents).
    MergeCommit,
    /// A squash merge (1 parent).
    Squash,
    /// A rebase merge (1 parent).
    Rebase,
}

impl MergeStrategyConfig {
    /// Convert to the typed-merge [`MergeStrategy`] domain enum. [P17]
    #[must_use]
    pub fn to_merge_strategy(self) -> crate::engine::recovery::typed_merge::MergeStrategy {
        match self {
            Self::MergeCommit => crate::engine::recovery::typed_merge::MergeStrategy::MergeCommit,
            Self::Squash => crate::engine::recovery::typed_merge::MergeStrategy::Squash,
            Self::Rebase => crate::engine::recovery::typed_merge::MergeStrategy::Rebase,
        }
    }
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
