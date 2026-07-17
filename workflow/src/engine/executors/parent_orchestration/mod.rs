//! Parent issue orchestration support.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{Duration, Utc};
use serde_json::{json, Value};
use tracing::warn;

use crate::adapters::github::{GithubError, SystemGithubCommandRunner};
use crate::adapters::github_issues::{
    GithubIssue, GithubIssuePrState, GithubIssueQuery, SystemGithubIssueQuery,
};
use crate::engine::executor::{interpolate_string, StepContext, StepExecutor};
use crate::engine::instance::WorkflowInstance;
static ARTIFACT_WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;
use crate::engine::{EngineRunner, RunContext, RunOutcome};
use crate::persistence::leases::{
    get_lease_for_issue, get_leases_for_issues, try_claim, update_lease_status,
    update_lease_status_conditional, LeaseStatus,
};
use crate::persistence::{
    get_run_with_conn, load_checkpoint_with_conn, upsert_wait_state, write_wait_state_artifact,
    RunMetadata, RunStatus, WaitKind, WaitStateRecord,
};
use crate::workflow::schema::WorkflowConfig;
use crate::workflow::target_profile::{apply_target_profile_overrides, TargetProfileOverrides};
pub mod model;

use model::{
    classify_child, next_actionable_child, order_subissues, ChildIssueState, ChildIssueStatus,
};

pub use model::missing_ordered_child_states;

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChildWorkflowRunnerError {
    LaunchFailed(String),
    ResumeFailed(String),
    StatusFailed(String),
    Unsupported(String),
}

impl std::fmt::Display for ChildWorkflowRunnerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChildWorkflowRunnerError::LaunchFailed(message) => {
                write!(f, "child workflow launch failed: {message}")
            }
            ChildWorkflowRunnerError::ResumeFailed(message) => {
                write!(f, "child workflow resume failed: {message}")
            }
            ChildWorkflowRunnerError::StatusFailed(message) => {
                write!(f, "child workflow status read failed: {message}")
            }
            ChildWorkflowRunnerError::Unsupported(message) => {
                write!(f, "unsupported child workflow operation: {message}")
            }
        }
    }
}

impl std::error::Error for ChildWorkflowRunnerError {}

pub trait ChildWorkflowRunner: Send + Sync {
    fn launch_child(
        &self,
        request: &ChildWorkflowLaunchRequest,
    ) -> Result<ChildWorkflowRunResult, ChildWorkflowRunnerError>;

    fn resume_child(
        &self,
        _request: &ChildWorkflowLaunchRequest,
    ) -> Result<ChildWorkflowRunResult, ChildWorkflowRunnerError> {
        Err(ChildWorkflowRunnerError::Unsupported(
            "child workflow runner does not support resume_child".to_string(),
        ))
    }

    fn run_status(&self, _run_id: &str) -> Result<Option<RunStatus>, ChildWorkflowRunnerError> {
        Err(ChildWorkflowRunnerError::Unsupported(
            "child workflow runner does not support run_status".to_string(),
        ))
    }
}

pub struct SystemChildWorkflowRunner;

impl ChildWorkflowRunner for SystemChildWorkflowRunner {
    fn launch_child(
        &self,
        request: &ChildWorkflowLaunchRequest,
    ) -> Result<ChildWorkflowRunResult, ChildWorkflowRunnerError> {
        launch_child_process(request).map_err(ChildWorkflowRunnerError::LaunchFailed)
    }

    fn resume_child(
        &self,
        request: &ChildWorkflowLaunchRequest,
    ) -> Result<ChildWorkflowRunResult, ChildWorkflowRunnerError> {
        resume_child_process(request).map_err(ChildWorkflowRunnerError::ResumeFailed)
    }

    fn run_status(&self, run_id: &str) -> Result<Option<RunStatus>, ChildWorkflowRunnerError> {
        child_run_status_from_registry(run_id)
            .map_err(|err| ChildWorkflowRunnerError::StatusFailed(err.to_string()))
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
    pub config_root: PathBuf,
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
    config_root: PathBuf,
}

impl OrchestrationState {
    fn from_context(context: &StepContext, params: &Value) -> Result<Self, EngineError> {
        let artifact_root = artifact_root(context, params)?;
        let artifact_dir = artifact_root.join("children");
        Ok(Self {
            current_step: required_context(context, "current_step_id")?,
            artifact_root,
            repo: required_context(context, "target_repo")?,
            parent_issue_number: parent_issue_number(context)?,
            luther_label: context_value_with_warned_default(
                context,
                "luther_label",
                "parent_orchestration.active_parent_label",
                "Luther working",
            ),
            child_workflow_type_id: context_value_with_warned_default(
                context,
                "parent_orchestration.child_workflow_type_id",
                "child_workflow_type_id",
                "llxprt-issue-fix-v1",
            ),
            child_config_id: context_value_with_warned_default(
                context,
                "parent_orchestration.child_config_id",
                "child_config_id",
                "llxprt-code",
            ),
            merge_poll_interval_seconds: optional_u64_context(
                context,
                "parent_orchestration.merge_poll_interval_seconds",
                "merge_poll_interval_seconds",
            )?
            .unwrap_or(300),
            max_child_merge_wait_seconds: optional_u64_context(
                context,
                "parent_orchestration.max_child_merge_wait_seconds",
                "max_child_merge_wait_seconds",
            )?,
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
            artifact_dir: Some(artifact_dir),
            config_root: parent_config_root(context)?,
        })
    }
}

mod child_run;
mod child_wait;
mod child_workflow;
mod completion;
mod context;
mod discovery;
mod lease;
mod rollup;

use child_run::*;
use child_wait::*;
use child_workflow::*;
use completion::*;
use context::*;
use discovery::*;
use lease::*;
use rollup::*;

#[cfg(test)]
mod tests;
