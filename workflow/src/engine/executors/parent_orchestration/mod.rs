//! Parent issue orchestration support.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{Duration, Utc};
use serde_json::{json, Value};

use crate::adapters::github::{GithubError, SystemGithubCommandRunner};
use crate::adapters::github_issues::{
    GithubIssue, GithubIssuePrState, GithubIssueQuery, SystemGithubIssueQuery,
};
use crate::engine::executor::{interpolate_string, StepContext, StepExecutor};
use crate::engine::instance::WorkflowInstance;
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;
use crate::engine::{EngineRunner, RunContext, RunOutcome};
use crate::persistence::leases::{
    get_lease_for_issue, try_claim, update_lease_status, LeaseStatus,
};
use crate::persistence::{
    get_run_with_conn, load_checkpoint_with_conn, upsert_wait_state, write_wait_state_artifact,
    RunMetadata, RunStatus, WaitKind, WaitStateRecord,
};
use crate::workflow::config_loader::{resolve_workflow_config, resolve_workflow_type};
use crate::workflow::schema::WorkflowConfig;
use crate::workflow::target_profile::{apply_target_profile_overrides, TargetProfileOverrides};

pub mod model;

use model::{
    classify_child, missing_ordered_child_states, next_actionable_child, order_subissues,
    ChildIssueState, ChildTerminalState,
};

/// Dispatches every parent orchestration workflow step using the current step id.
pub struct ParentOrchestrationExecutor;

impl StepExecutor for ParentOrchestrationExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        params: &Value,
    ) -> Result<StepOutcome, EngineError> {
        let query = SystemGithubIssueQuery::new(SystemGithubCommandRunner);
        ParentOrchestrationExecutorWithQuery::new(query).execute(context, params)
    }
}

pub trait ChildWorkflowRunner: Send + Sync {
    fn launch_child(
        &self,
        request: &ChildWorkflowLaunchRequest,
    ) -> Result<ChildWorkflowRunResult, String>;

    fn resume_child(
        &self,
        request: &ChildWorkflowLaunchRequest,
    ) -> Result<ChildWorkflowRunResult, String> {
        self.launch_child(request)
    }

    fn run_status(&self, run_id: &str) -> Result<Option<RunStatus>, String> {
        child_run_status_from_registry(run_id)
    }
}

pub struct SystemChildWorkflowRunner;

impl ChildWorkflowRunner for SystemChildWorkflowRunner {
    fn launch_child(
        &self,
        request: &ChildWorkflowLaunchRequest,
    ) -> Result<ChildWorkflowRunResult, String> {
        launch_child_process(request)
    }

    fn resume_child(
        &self,
        request: &ChildWorkflowLaunchRequest,
    ) -> Result<ChildWorkflowRunResult, String> {
        resume_child_process(request)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildWorkflowLaunchRequest {
    pub workflow_type_id: String,
    pub config_id: String,
    pub run_id: String,
    pub repo: String,
    pub issue_number: u64,
    pub work_dir: Option<PathBuf>,
    pub artifact_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChildWorkflowRunResult {
    CompletedSuccess,
    CompletedFailure,
    WaitingExternal,
}

pub struct ParentOrchestrationExecutorWithQuery<Q, R = SystemChildWorkflowRunner> {
    query: Q,
    runner: R,
}

impl<Q> ParentOrchestrationExecutorWithQuery<Q, SystemChildWorkflowRunner> {
    pub fn new(query: Q) -> Self {
        Self::with_runner(query, SystemChildWorkflowRunner)
    }
}

impl<Q, R> ParentOrchestrationExecutorWithQuery<Q, R> {
    pub fn with_runner(query: Q, runner: R) -> Self {
        Self { query, runner }
    }
}

impl<Q, R> StepExecutor for ParentOrchestrationExecutorWithQuery<Q, R>
where
    Q: GithubIssueQuery + Send + Sync,
    R: ChildWorkflowRunner,
{
    fn execute(
        &self,
        context: &mut StepContext,
        params: &Value,
    ) -> Result<StepOutcome, EngineError> {
        let state = OrchestrationState::from_context(context, params)?;
        match state.current_step.as_str() {
            "load_parent_issue" => load_parent_issue(context, &state, &self.query),
            "discover_subissues" | "refresh_parent_and_children" => {
                discover_subissues(context, &state, &self.query)
            }
            "classify_subissues" | "classify_refreshed_subissues" => {
                classify_subissues(context, &state, &self.query)
            }
            "determine_subissue_order" | "determine_refreshed_subissue_order" => {
                determine_subissue_order(context, &state)
            }
            "select_next_child" => select_next_child(context, &state),
            "launch_or_resume_child_workflow" => {
                launch_child_workflow(context, &state, &self.query, &self.runner)
            }
            "wait_for_child_merge" => wait_for_child_merge(context, &state, &self.query),
            "evaluate_parent_completion" => {
                evaluate_parent_completion(context, &state, &self.query)
            }
            "close_or_report_parent" => close_or_report_parent(context, &state, &self.query),
            other => Err(parent_error(format!(
                "unknown parent orchestration step '{other}'"
            ))),
        }
    }
}

struct OrchestrationState {
    current_step: String,
    artifact_root: PathBuf,
    repo: String,
    parent_issue_number: u64,
    luther_label: String,
    child_workflow_type_id: String,
    child_config_id: String,
    merge_poll_interval_seconds: u64,
    max_child_merge_wait_seconds: Option<u64>,
    auto_merge_children: bool,
    wait_for_human_merge: bool,
    work_dir: Option<PathBuf>,
    artifact_dir: Option<PathBuf>,
}

impl OrchestrationState {
    fn from_context(context: &StepContext, params: &Value) -> Result<Self, EngineError> {
        let artifact_root = artifact_root(context, params)?;
        Ok(Self {
            current_step: required_context(context, "current_step_id")?,
            artifact_root: artifact_root.clone(),
            repo: required_context(context, "target_repo")?,
            parent_issue_number: parent_issue_number(context)?,
            luther_label: context
                .get("luther_label")
                .cloned()
                .unwrap_or_else(|| "Luther working".to_string()),
            child_workflow_type_id: context
                .get("parent_orchestration.child_workflow_type_id")
                .or_else(|| context.get("child_workflow_type_id"))
                .cloned()
                .unwrap_or_else(|| "llxprt-issue-fix-v1".to_string()),
            child_config_id: context
                .get("parent_orchestration.child_config_id")
                .or_else(|| context.get("child_config_id"))
                .cloned()
                .unwrap_or_else(|| "llxprt-code".to_string()),
            merge_poll_interval_seconds: context
                .get("parent_orchestration.merge_poll_interval_seconds")
                .or_else(|| context.get("merge_poll_interval_seconds"))
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(300),
            max_child_merge_wait_seconds: context
                .get("parent_orchestration.max_child_merge_wait_seconds")
                .and_then(|value| value.parse::<u64>().ok()),
            auto_merge_children: bool_context(
                context,
                "parent_orchestration.auto_merge_children",
                "auto_merge_children",
            ),
            wait_for_human_merge: bool_context_default(
                context,
                "parent_orchestration.wait_for_human_merge",
                "wait_for_human_merge",
                true,
            ),
            work_dir: context.get("work_dir").map(PathBuf::from),
            artifact_dir: Some(artifact_root.join("children")),
        })
    }
}

include!("core_1.rs");
include!("core_2.rs");
include!("core_3.rs");

#[cfg(test)]
mod tests;
