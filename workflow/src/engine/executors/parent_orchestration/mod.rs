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

fn load_parent_issue(
    context: &mut StepContext,
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
) -> Result<StepOutcome, EngineError> {
    query
        .add_label(&state.repo, state.parent_issue_number, &state.luther_label)
        .map_err(github_error)?;
    let issue = query
        .get_issue(&state.repo, state.parent_issue_number)
        .map_err(github_error)?
        .unwrap_or_else(|| fallback_issue(state.parent_issue_number));
    write_json(&state.artifact_root, "parent-issue.json", &issue)?;
    context.set("parent_issue_number", &issue.number.to_string());
    Ok(StepOutcome::Success)
}

fn discover_subissues(
    context: &mut StepContext,
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
) -> Result<StepOutcome, EngineError> {
    let children = query
        .list_sub_issues(&state.repo, state.parent_issue_number)
        .map_err(github_error)?;
    let numbers: Vec<u64> = children.iter().map(|child| child.issue.number).collect();
    write_json(&state.artifact_root, "parent-subissues.json", &children)?;
    write_json(
        &state.artifact_root,
        "parent-refresh-snapshot.json",
        &json!({"parent_issue_number": state.parent_issue_number, "children": numbers}),
    )?;
    context.set("child_issue_numbers", &json!(numbers).to_string());
    if state.current_step == "refresh_parent_and_children" {
        clear_selected_child(&state.artifact_root)?;
    }
    Ok(StepOutcome::Success)
}

fn classify_subissues(
    context: &mut StepContext,
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
) -> Result<StepOutcome, EngineError> {
    let children = read_children(&state.artifact_root)?;
    let states = children
        .iter()
        .map(|child| classify_child_with_run_state(state, query, &child.issue))
        .collect::<Result<Vec<_>, _>>()?;
    write_json(
        &state.artifact_root,
        "subissue-state-snapshot.json",
        &states,
    )?;
    context.set("subissue_state_snapshot", &json!(states).to_string());
    Ok(StepOutcome::Success)
}

fn classify_child_with_run_state(
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    issue: &GithubIssue,
) -> Result<ChildIssueState, EngineError> {
    let pr = query
        .pr_state_for_issue(&state.repo, issue.number)
        .map_err(github_error)?;
    let mut child = classify_child(issue, pr.as_ref());
    apply_child_run_state(state, issue, &mut child)?;
    Ok(child)
}

fn apply_child_run_state(
    state: &OrchestrationState,
    issue: &GithubIssue,
    child: &mut ChildIssueState,
) -> Result<(), EngineError> {
    let conn = daemon_connection()?;
    if let Some(lease) =
        get_lease_for_issue(&conn, &state.repo, child.issue_number).map_err(sql_error)?
    {
        match lease.status {
            LeaseStatus::Failed | LeaseStatus::Abandoned | LeaseStatus::Stale
                if issue.state.eq_ignore_ascii_case("open") =>
            {
                child.terminal_state = ChildTerminalState::FailedRun;
            }
            LeaseStatus::Claimed
            | LeaseStatus::WaitingExternal
            | LeaseStatus::ReadyToResume
            | LeaseStatus::Running => {
                if stale_child_run(&lease, state.merge_poll_interval_seconds)
                    && issue.state.eq_ignore_ascii_case("open")
                {
                    child.terminal_state = ChildTerminalState::StaleRun;
                } else if !child_workflow_completed(&lease)? {
                    child.terminal_state = ChildTerminalState::ActiveRun;
                }
            }
            _ => {}
        }
    }
    apply_child_rollup_state(state, child)
}

fn apply_child_rollup_state(
    state: &OrchestrationState,
    child: &mut ChildIssueState,
) -> Result<(), EngineError> {
    let rollup = read_rollup(&state.artifact_root)?;
    if rollup.children.iter().any(|entry| {
        entry.child_issue_number == child.issue_number
            && unresolved_rollup_outcome_requires_pr(entry)
    }) {
        child.terminal_state = ChildTerminalState::Blocked;
    }
    Ok(())
}

fn stale_child_run(
    lease: &crate::persistence::leases::IssueLease,
    poll_interval_seconds: u64,
) -> bool {
    let grace_seconds = poll_interval_seconds.saturating_mul(3).max(900);
    let grace_i64 = i64::try_from(grace_seconds).unwrap_or(i64::MAX);
    let stale_after = Duration::seconds(grace_i64);
    Utc::now().signed_duration_since(lease.heartbeat_at) > stale_after
}

fn child_workflow_completed(
    lease: &crate::persistence::leases::IssueLease,
) -> Result<bool, EngineError> {
    let Some(run_id) = lease.run_id.as_deref() else {
        return Ok(false);
    };
    let Some(metadata) = get_run_with_conn(&daemon_connection()?, run_id).map_err(sql_error)?
    else {
        return Ok(false);
    };
    Ok(matches!(
        metadata.status,
        RunStatus::Completed | RunStatus::Merged
    ))
}

fn determine_subissue_order(
    context: &mut StepContext,
    state: &OrchestrationState,
) -> Result<StepOutcome, EngineError> {
    let children = read_children(&state.artifact_root)?;
    let order = order_subissues(&children);
    write_json(
        &state.artifact_root,
        "subissue-order-plan.json",
        &json!({"order": order, "strategy": "native_position_then_issue_number"}),
    )?;
    context.set("subissue_order", &json!(order).to_string());
    Ok(StepOutcome::Success)
}

fn select_next_child(
    context: &mut StepContext,
    state: &OrchestrationState,
) -> Result<StepOutcome, EngineError> {
    let states: Vec<ChildIssueState> =
        read_json(&state.artifact_root.join("subissue-state-snapshot.json"))?;
    let order_plan: Value = read_json(&state.artifact_root.join("subissue-order-plan.json"))?;
    let order: Vec<u64> = serde_json::from_value(order_plan["order"].clone())
        .map_err(|err| parent_error(format!("parse subissue order artifact: {err}")))?;
    let missing_states = missing_ordered_child_states(&states, &order);
    if !missing_states.is_empty() {
        write_json(
            &state.artifact_root,
            "selected-child.json",
            &json!({
                "issue_number": null,
                "blocked": true,
                "reason": "order_state_snapshot_mismatch",
                "missing_state_issue_numbers": missing_states
            }),
        )?;
        return Ok(StepOutcome::Fixable);
    }
    let next = next_actionable_child(&states, &order);
    write_json(
        &state.artifact_root,
        "selected-child.json",
        &json!({"issue_number": next}),
    )?;
    if let Some(number) = next {
        context.set("selected_child_issue_number", &number.to_string());
        Ok(StepOutcome::Success)
    } else {
        Ok(StepOutcome::Fixable)
    }
}

fn launch_child_workflow(
    context: &mut StepContext,
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    runner: &dyn ChildWorkflowRunner,
) -> Result<StepOutcome, EngineError> {
    let Some(child) = selected_child(&state.artifact_root)? else {
        write_launch_artifact(
            state,
            json!({"launched": false, "reason": "no_actionable_child"}),
        )?;
        return Ok(StepOutcome::Success);
    };
    let pr = query
        .pr_state_for_issue(&state.repo, child)
        .map_err(github_error)?;
    if let Some(pr) = pr.filter(is_observable_existing_pr) {
        if !has_active_or_recoverable_child_lease(state, child)? {
            return observe_existing_child_pr(context, state, query, child, &pr);
        }
    }
    match prepare_child_lease(state, child)? {
        ChildLeaseAction::Wait { lease, reason } => {
            wait_for_existing_child(state, child, &lease, &reason)
        }
        ChildLeaseAction::Resume(lease) => {
            resume_child_workflow(context, state, query, runner, child, &lease)
        }
        ChildLeaseAction::Launch(lease) => {
            start_child_workflow(context, state, query, runner, child, &lease)
        }
    }
}

fn has_active_or_recoverable_child_lease(
    state: &OrchestrationState,
    child: u64,
) -> Result<bool, EngineError> {
    let conn = daemon_connection()?;
    Ok(get_lease_for_issue(&conn, &state.repo, child)
        .map_err(sql_error)?
        .is_some_and(|lease| {
            matches!(
                lease.status,
                LeaseStatus::ReadyToResume
                    | LeaseStatus::Failed
                    | LeaseStatus::Abandoned
                    | LeaseStatus::Stale
                    | LeaseStatus::WaitingExternal
                    | LeaseStatus::Claimed
                    | LeaseStatus::Running
            )
        }))
}

fn is_observable_existing_pr(pr: &GithubIssuePrState) -> bool {
    !pr.merged
        && !pr.state.eq_ignore_ascii_case("closed")
        && !pr.state.eq_ignore_ascii_case("superseded")
}

fn observe_existing_child_pr(
    context: &mut StepContext,
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    child: u64,
    pr: &GithubIssuePrState,
) -> Result<StepOutcome, EngineError> {
    query
        .add_label(&state.repo, child, &state.luther_label)
        .map_err(github_error)?;
    write_launch_artifact(
        state,
        json!({
            "launched": false,
            "child_issue_number": child,
            "reason": "existing_child_pr",
            "observing_existing_pr": true,
            "pr": pr
        }),
    )?;
    context.set("child_pr_number", &pr.number.to_string());
    update_rollup(state, child, None, "observing_existing_child_pr", Some(pr))?;
    Ok(StepOutcome::Success)
}

fn wait_for_existing_child(
    state: &OrchestrationState,
    child: u64,
    lease: &crate::persistence::leases::IssueLease,
    reason: &str,
) -> Result<StepOutcome, EngineError> {
    let run_status = lease
        .run_id
        .as_deref()
        .map(child_run_status_from_registry)
        .transpose()
        .map_err(parent_error)?
        .flatten();
    if run_status == Some(RunStatus::Merged) {
        write_launch_artifact(
            state,
            json!({
                "launched": false,
                "child_issue_number": child,
                "reason": "child_workflow_completed_waiting_for_pr_merge",
                "existing_run_id": lease.run_id,
                "lease_status": lease.status.to_string(),
                "run_status": run_status.map(|status| status.to_string())
            }),
        )?;
        update_rollup(
            state,
            child,
            lease.run_id.as_deref(),
            "child_workflow_completed_waiting_for_pr_merge",
            None,
        )?;
        return Ok(StepOutcome::Success);
    }
    write_launch_artifact(
        state,
        json!({
            "launched": false,
            "child_issue_number": child,
            "reason": reason,
            "existing_run_id": lease.run_id,
            "lease_status": lease.status.to_string(),
            "run_status": run_status.as_ref().map(ToString::to_string)
        }),
    )?;
    write_child_workflow_wait_artifact(
        state,
        child,
        lease,
        lease.run_id.as_deref(),
        reason,
        run_status.as_ref(),
    )?;
    update_rollup(state, child, lease.run_id.as_deref(), reason, None)?;
    Ok(StepOutcome::Wait)
}

fn write_child_workflow_wait_artifact(
    state: &OrchestrationState,
    child: u64,
    lease: &crate::persistence::leases::IssueLease,
    child_run_id: Option<&str>,
    reason: &str,
    run_status: Option<&RunStatus>,
) -> Result<(), EngineError> {
    write_json(
        &state.artifact_root,
        "child-workflow-wait.json",
        &json!({
            "waiting": true,
            "state": "child_workflow_in_progress",
            "child_issue_number": child,
            "child_run_id": child_run_id,
            "child_lease_id": lease.lease_id,
            "lease_status": lease.status.to_string(),
            "run_status": run_status.map(ToString::to_string),
            "reason": reason,
            "poll_interval_seconds": state.merge_poll_interval_seconds,
            "max_child_merge_wait_seconds": state.max_child_merge_wait_seconds
        }),
    )
}

fn start_child_workflow(
    context: &mut StepContext,
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    runner: &dyn ChildWorkflowRunner,
    child: u64,
    lease: &crate::persistence::leases::IssueLease,
) -> Result<StepOutcome, EngineError> {
    query
        .add_label(&state.repo, child, &state.luther_label)
        .map_err(github_error)?;
    let request = child_launch_request(state, child);
    update_lease_status(
        &daemon_connection()?,
        &lease.lease_id,
        LeaseStatus::Running,
        Some(&request.run_id),
    )
    .map_err(sql_error)?;
    let result = runner.launch_child(&request).map_err(|err| {
        let _ =
            restore_child_lease_after_runner_error(lease, lease.status, lease.run_id.as_deref());
        parent_error(err)
    })?;
    let run_status = runner.run_status(&request.run_id).map_err(parent_error)?;
    let pr = query
        .pr_state_for_issue(&state.repo, child)
        .map_err(github_error)?;
    finish_child_launch(
        state,
        context,
        query,
        ChildLaunchCompletion {
            child,
            lease,
            request: &request,
            result,
            run_status,
            pr,
        },
    )
}

fn resume_child_workflow(
    context: &mut StepContext,
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    runner: &dyn ChildWorkflowRunner,
    child: u64,
    lease: &crate::persistence::leases::IssueLease,
) -> Result<StepOutcome, EngineError> {
    let Some(run_id) = lease.run_id.clone() else {
        update_lease_status(
            &daemon_connection()?,
            &lease.lease_id,
            LeaseStatus::Failed,
            None,
        )
        .map_err(sql_error)?;
        return Ok(StepOutcome::Fixable);
    };
    let request = child_resume_request(state, child, run_id.clone());
    update_lease_status(
        &daemon_connection()?,
        &lease.lease_id,
        LeaseStatus::Running,
        Some(&request.run_id),
    )
    .map_err(sql_error)?;
    let result = runner.resume_child(&request).map_err(|err| {
        let _ = restore_child_lease_after_runner_error(lease, lease.status, Some(&run_id));
        parent_error(err)
    })?;
    let run_status = runner.run_status(&request.run_id).map_err(parent_error)?;
    let pr = query
        .pr_state_for_issue(&state.repo, child)
        .map_err(github_error)?;
    finish_child_launch(
        state,
        context,
        query,
        ChildLaunchCompletion {
            child,
            lease,
            request: &request,
            result,
            run_status,
            pr,
        },
    )
}

fn restore_child_lease_after_runner_error(
    lease: &crate::persistence::leases::IssueLease,
    status: LeaseStatus,
    run_id: Option<&str>,
) -> Result<(), EngineError> {
    update_lease_status(&daemon_connection()?, &lease.lease_id, status, run_id).map_err(sql_error)
}

fn wait_for_child_merge(
    context: &mut StepContext,
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
) -> Result<StepOutcome, EngineError> {
    let Some(child) = selected_child(&state.artifact_root)? else {
        write_json(
            &state.artifact_root,
            "child-merge-wait.json",
            &json!({"waiting": false, "reason": "no_actionable_child"}),
        )?;
        return Ok(StepOutcome::Success);
    };
    let pr = query
        .pr_state_for_issue(&state.repo, child)
        .map_err(github_error)?;
    match classify_child_pr_wait(pr.as_ref()) {
        ChildPrWait::Merged => finish_merged_child(context, state, query, child, pr.as_ref()),
        ChildPrWait::ReadyForHumanMerge => {
            let run_id = child_run_id_for_wait(state, child)?;
            if child_workflow_ready_for_merge(&run_id)? {
                record_ready_for_human_merge(state, query, child, pr.as_ref())
            } else if run_id.is_none() {
                record_observed_child_pr_merge_wait(state, query, child, pr.as_ref())
            } else {
                record_child_pr_still_in_progress(state, child, pr.as_ref(), run_id.as_deref())
            }
        }
        ChildPrWait::MissingPr => {
            record_blocked_child(state, query, child, pr.as_ref(), "missing_child_pr")
        }
        ChildPrWait::ClosedUnmerged => {
            reevaluate_closed_unmerged_child(state, query, child, pr.as_ref())
        }
        ChildPrWait::Superseded => record_superseded_child(state, query, child, pr.as_ref()),
    }
}

fn record_observed_child_pr_merge_wait(
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    child: u64,
    pr: Option<&GithubIssuePrState>,
) -> Result<StepOutcome, EngineError> {
    write_json(
        &state.artifact_root,
        "child-merge-wait.json",
        &json!({
            "waiting": true,
            "child_issue_number": child,
            "state": "observing_existing_child_pr",
            "child_run_id": null,
            "pr": pr,
            "poll_interval_seconds": state.merge_poll_interval_seconds,
            "max_child_merge_wait_seconds": state.max_child_merge_wait_seconds
        }),
    )?;
    query
        .comment_issue(
            &state.repo,
            state.parent_issue_number,
            &format!(
                "Child issue #{child} already has an active PR. Parent orchestration will observe it and continue after the PR is merged."
            ),
        )
        .map_err(github_error)?;
    update_rollup(state, child, None, "observing_existing_child_pr", pr)?;
    Ok(StepOutcome::Wait)
}

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
struct ParentOrchestrationRollup {
    parent_issue_number: u64,
    children: Vec<ChildRollupEntry>,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
struct ChildRollupEntry {
    child_issue_number: u64,
    child_run_id: Option<String>,
    child_artifact_dir: Option<String>,
    pr_number: Option<u64>,
    pr_state: Option<String>,
    merge_sha: Option<String>,
    outcome: Option<String>,
    non_actionable_reason: Option<String>,
}

fn bool_context(context: &StepContext, primary: &str, fallback: &str) -> bool {
    bool_context_default(context, primary, fallback, false)
}

fn bool_context_default(
    context: &StepContext,
    primary: &str,
    fallback: &str,
    default: bool,
) -> bool {
    context
        .get(primary)
        .or_else(|| context.get(fallback))
        .map_or(default, |value| value == "true")
}

fn daemon_connection() -> Result<rusqlite::Connection, EngineError> {
    let db_path = crate::runtime_paths::get_data_dir().join("checkpoints.db");
    crate::persistence::init_database(&db_path)
        .map_err(|err| parent_error(format!("initialize daemon database: {err}")))?;
    let conn = rusqlite::Connection::open(&db_path)
        .map_err(|err| parent_error(format!("open daemon database: {err}")))?;
    configure_parent_orchestration_connection(&conn)?;
    Ok(conn)
}

fn configure_parent_orchestration_connection(
    conn: &rusqlite::Connection,
) -> Result<(), EngineError> {
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|err| parent_error(format!("set daemon database busy timeout: {err}")))?;
    Ok(())
}

fn open_parent_orchestration_connection(path: &Path) -> Result<rusqlite::Connection, String> {
    let conn = rusqlite::Connection::open(path).map_err(|err| err.to_string())?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|err| err.to_string())?;
    Ok(conn)
}

fn child_run_status_from_registry(run_id: &str) -> Result<Option<RunStatus>, String> {
    let conn = daemon_connection().map_err(|err| err.to_string())?;
    get_run_with_conn(&conn, run_id)
        .map(|metadata| metadata.map(|run| run.status))
        .map_err(|err| err.to_string())
}

enum ChildLeaseAction {
    Launch(crate::persistence::leases::IssueLease),
    Resume(crate::persistence::leases::IssueLease),
    Wait {
        lease: crate::persistence::leases::IssueLease,
        reason: String,
    },
}

fn prepare_child_lease(
    state: &OrchestrationState,
    child: u64,
) -> Result<ChildLeaseAction, EngineError> {
    let conn = daemon_connection()?;
    if let Some(lease) = get_lease_for_issue(&conn, &state.repo, child).map_err(sql_error)? {
        return Ok(match lease.status {
            LeaseStatus::ReadyToResume => {
                if child_workflow_completed(&lease)? {
                    ChildLeaseAction::Wait {
                        lease,
                        reason: "child_workflow_completed_waiting_for_pr_merge".to_string(),
                    }
                } else {
                    ChildLeaseAction::Resume(lease)
                }
            }
            LeaseStatus::Failed | LeaseStatus::Abandoned => {
                prepare_relaunchable_child(&conn, &lease)?
            }
            LeaseStatus::Stale => prepare_relaunchable_child(&conn, &lease)?,
            LeaseStatus::WaitingExternal | LeaseStatus::Claimed | LeaseStatus::Running => {
                ChildLeaseAction::Wait {
                    lease,
                    reason: "active_child_lease".to_string(),
                }
            }
            LeaseStatus::Pending | LeaseStatus::Completed => ChildLeaseAction::Wait {
                lease,
                reason: "non_actionable_child_lease".to_string(),
            },
        });
    }
    let lease = try_claim(&conn, &state.repo, child, &state.child_config_id)
        .map_err(sql_error)?
        .ok_or_else(|| parent_error("child lease claim lost to concurrent worker".to_string()))?;
    Ok(ChildLeaseAction::Launch(lease))
}

fn prepare_relaunchable_child(
    conn: &rusqlite::Connection,
    lease: &crate::persistence::leases::IssueLease,
) -> Result<ChildLeaseAction, EngineError> {
    clear_child_lease_for_relaunch(conn, lease)?;
    let relaunchable = get_lease_for_issue(conn, &lease.issue_repo, lease.issue_number)
        .map_err(sql_error)?
        .ok_or_else(|| {
            parent_error("child lease disappeared while preparing relaunch".to_string())
        })?;
    Ok(ChildLeaseAction::Launch(relaunchable))
}

fn clear_child_lease_for_relaunch(
    conn: &rusqlite::Connection,
    lease: &crate::persistence::leases::IssueLease,
) -> Result<(), EngineError> {
    conn.execute(
        "UPDATE issue_leases SET status = ?1, run_id = NULL, updated_at = ?2 WHERE lease_id = ?3",
        rusqlite::params![
            LeaseStatus::Claimed.to_string(),
            Utc::now().to_rfc3339(),
            lease.lease_id
        ],
    )
    .map(|_| ())
    .map_err(sql_error)
}

enum ChildPrWait {
    Merged,
    ReadyForHumanMerge,
    MissingPr,
    ClosedUnmerged,
    Superseded,
}

fn classify_child_pr_wait(pr: Option<&GithubIssuePrState>) -> ChildPrWait {
    let Some(pr) = pr else {
        return ChildPrWait::MissingPr;
    };
    if pr.merged {
        return ChildPrWait::Merged;
    }
    if pr.state.eq_ignore_ascii_case("superseded") {
        return ChildPrWait::Superseded;
    }
    if pr.state.eq_ignore_ascii_case("closed") {
        return ChildPrWait::ClosedUnmerged;
    }
    ChildPrWait::ReadyForHumanMerge
}

fn finish_merged_child(
    context: &mut StepContext,
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    child: u64,
    pr: Option<&GithubIssuePrState>,
) -> Result<StepOutcome, EngineError> {
    let run_id = persisted_child_run_id(context, state, child)?;
    write_json(
        &state.artifact_root,
        "child-merge-wait.json",
        &json!({
            "waiting": false,
            "child_issue_number": child,
            "state": "merged",
            "child_run_id": run_id,
            "pr": pr
        }),
    )?;

    query
        .remove_label(&state.repo, child, &state.luther_label)
        .map_err(github_error)?;
    if let Some(run_id) = run_id.as_deref() {
        mark_child_lease_completed(state, child, run_id)?;
    }
    update_rollup(state, child, run_id.as_deref(), "merged", pr)?;
    Ok(StepOutcome::Success)
}

fn record_ready_for_human_merge(
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    child: u64,
    pr: Option<&GithubIssuePrState>,
) -> Result<StepOutcome, EngineError> {
    let run_id = child_run_id_for_wait(state, child)?;
    let auto_merge = attempt_auto_merge_if_enabled(state, query, pr);
    write_json(
        &state.artifact_root,
        "child-merge-wait.json",
        &json!({
            "waiting": true,
            "child_issue_number": child,
            "state": "ready_for_human_merge",
            "child_run_id": run_id,
            "pr": pr,
            "auto_merge_children": state.auto_merge_children,
            "auto_merge": auto_merge,
            "wait_for_human_merge": state.wait_for_human_merge,
            "poll_interval_seconds": state.merge_poll_interval_seconds,
            "max_child_merge_wait_seconds": state.max_child_merge_wait_seconds
        }),
    )?;
    query
        .comment_issue(
            &state.repo,
            state.parent_issue_number,
            &format!(
                "Child issue #{child} has a PR ready for human merge. Parent orchestration will continue after the PR is merged."
            ),
        )
        .map_err(github_error)?;
    update_rollup(state, child, run_id.as_deref(), "ready_for_human_merge", pr)?;
    Ok(if state.wait_for_human_merge {
        StepOutcome::Wait
    } else {
        StepOutcome::Success
    })
}

fn persisted_child_run_id(
    context: &StepContext,
    state: &OrchestrationState,
    child: u64,
) -> Result<Option<String>, EngineError> {
    if let Some(run_id) = context.get("child_run_id") {
        return Ok(Some(run_id.clone()));
    }
    child_run_id_for_wait(state, child)
}

fn child_run_id_for_wait(
    state: &OrchestrationState,
    child: u64,
) -> Result<Option<String>, EngineError> {
    let conn = daemon_connection()?;
    Ok(get_lease_for_issue(&conn, &state.repo, child)
        .map_err(sql_error)?
        .and_then(|lease| lease.run_id))
}

fn child_workflow_ready_for_merge(run_id: &Option<String>) -> Result<bool, EngineError> {
    let Some(run_id) = run_id.as_deref() else {
        return Ok(false);
    };
    let Some(metadata) = get_run_with_conn(&daemon_connection()?, run_id).map_err(sql_error)?
    else {
        return Ok(false);
    };
    Ok(matches!(
        metadata.status,
        RunStatus::Completed | RunStatus::Merged
    ))
}

fn record_child_pr_still_in_progress(
    state: &OrchestrationState,
    child: u64,
    pr: Option<&GithubIssuePrState>,
    run_id: Option<&str>,
) -> Result<StepOutcome, EngineError> {
    write_json(
        &state.artifact_root,
        "child-merge-wait.json",
        &json!({
            "waiting": true,
            "child_issue_number": child,
            "state": "child_workflow_in_progress",
            "child_run_id": run_id,
            "pr": pr,
            "poll_interval_seconds": state.merge_poll_interval_seconds,
            "max_child_merge_wait_seconds": state.max_child_merge_wait_seconds
        }),
    )?;
    update_rollup(state, child, run_id, "child_workflow_in_progress", pr)?;
    Ok(StepOutcome::Wait)
}

fn reevaluate_closed_unmerged_child(
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    child: u64,
    pr: Option<&GithubIssuePrState>,
) -> Result<StepOutcome, EngineError> {
    let Some(issue) = query.get_issue(&state.repo, child).map_err(github_error)? else {
        return record_blocked_child(state, query, child, pr, "closed_unmerged_pr");
    };
    if issue.state.eq_ignore_ascii_case("open") {
        mark_child_lease_relaunchable(state, child)?;
        update_rollup(state, child, None, "closed_unmerged_pr_relaunchable", pr)?;
        write_json(
            &state.artifact_root,
            "child-merge-wait.json",
            &json!({
                "waiting": false,
                "child_issue_number": child,
                "state": "closed_unmerged_relaunchable",
                "pr": pr
            }),
        )?;
        return Ok(StepOutcome::Success);
    }
    record_blocked_child(state, query, child, pr, "closed_unmerged_pr")
}

fn mark_child_lease_relaunchable(
    state: &OrchestrationState,
    child: u64,
) -> Result<(), EngineError> {
    let conn = daemon_connection()?;
    if let Some(lease) = get_lease_for_issue(&conn, &state.repo, child).map_err(sql_error)? {
        update_lease_status(
            &conn,
            &lease.lease_id,
            LeaseStatus::Failed,
            lease.run_id.as_deref(),
        )
        .map_err(sql_error)?;
    }
    Ok(())
}

fn attempt_auto_merge_if_enabled(
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    pr: Option<&GithubIssuePrState>,
) -> Value {
    if !state.auto_merge_children {
        return json!({"attempted": false, "reason": "disabled"});
    }
    let Some(pr) = pr else {
        return json!({"attempted": false, "reason": "missing_pr"});
    };
    if let Some(reason) = auto_merge_block_reason(pr) {
        return json!({
            "attempted": false,
            "enabled": false,
            "pr_number": pr.number,
            "fallback": "wait_for_human_merge",
            "reason": reason
        });
    }
    match query.enable_pr_auto_merge(&state.repo, pr.number) {
        Ok(()) => json!({"attempted": true, "enabled": true, "pr_number": pr.number}),
        Err(err) => json!({
            "attempted": true,
            "enabled": false,
            "pr_number": pr.number,
            "fallback": "wait_for_human_merge",
            "error": err.to_string()
        }),
    }
}

fn auto_merge_block_reason(pr: &GithubIssuePrState) -> Option<&'static str> {
    if pr.status_check_rollup.as_deref() != Some("passed") {
        return Some("checks_not_passed");
    }
    match pr.review_decision.as_deref() {
        Some("changes_requested" | "review_required") => Some("review_not_approved"),
        _ => None,
    }
}

fn record_superseded_child(
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    child: u64,
    pr: Option<&GithubIssuePrState>,
) -> Result<StepOutcome, EngineError> {
    query
        .comment_issue(
            &state.repo,
            state.parent_issue_number,
            &format!(
                "Parent orchestration paused on superseded child issue #{child}; a replacement PR needs human confirmation."
            ),
        )
        .map_err(github_error)?;
    record_blocked_child(state, query, child, pr, "superseded_child_pr")
}

fn record_blocked_child(
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    child: u64,
    pr: Option<&GithubIssuePrState>,
    reason: &str,
) -> Result<StepOutcome, EngineError> {
    write_json(
        &state.artifact_root,
        "child-merge-wait.json",
        &json!({
            "waiting": false,
            "child_issue_number": child,
            "state": "blocked",
            "reason": reason,
            "pr": pr
        }),
    )?;
    query
        .remove_label(&state.repo, child, &state.luther_label)
        .map_err(github_error)?;
    query
        .comment_issue(
            &state.repo,
            state.parent_issue_number,
            &format!("Parent orchestration is blocked on child issue #{child}: {reason}."),
        )
        .map_err(github_error)?;
    update_rollup(state, child, None, reason, pr)?;
    Ok(StepOutcome::Fixable)
}

fn child_launch_request(state: &OrchestrationState, child: u64) -> ChildWorkflowLaunchRequest {
    let stamp = Utc::now().timestamp_millis();
    child_request_with_run_id(
        state,
        child,
        format!("parent{}-child{}-{stamp}", state.parent_issue_number, child),
    )
}

fn child_resume_request(
    state: &OrchestrationState,
    child: u64,
    run_id: String,
) -> ChildWorkflowLaunchRequest {
    child_request_with_run_id(state, child, run_id)
}

fn child_request_with_run_id(
    state: &OrchestrationState,
    child: u64,
    run_id: String,
) -> ChildWorkflowLaunchRequest {
    let artifact_dir = state
        .artifact_dir
        .as_ref()
        .map(|base| child_artifact_dir(base, child, &run_id));
    ChildWorkflowLaunchRequest {
        workflow_type_id: state.child_workflow_type_id.clone(),
        config_id: state.child_config_id.clone(),
        run_id,
        repo: state.repo.clone(),
        issue_number: child,
        work_dir: state.work_dir.clone(),
        artifact_dir,
    }
}

fn child_artifact_dir(base: &Path, child: u64, run_id: &str) -> PathBuf {
    base.join(format!("issue-{child}")).join(run_id)
}

fn mark_child_lease_completed(
    state: &OrchestrationState,
    child: u64,
    run_id: &str,
) -> Result<(), EngineError> {
    let conn = daemon_connection()?;
    if let Some(lease) = get_lease_for_issue(&conn, &state.repo, child).map_err(sql_error)? {
        update_lease_status(&conn, &lease.lease_id, LeaseStatus::Completed, Some(run_id))
            .map_err(sql_error)?;
    }
    Ok(())
}

struct ChildLaunchCompletion<'a> {
    child: u64,
    lease: &'a crate::persistence::leases::IssueLease,
    request: &'a ChildWorkflowLaunchRequest,
    result: ChildWorkflowRunResult,
    run_status: Option<RunStatus>,
    pr: Option<GithubIssuePrState>,
}

fn finish_child_launch(
    state: &OrchestrationState,
    context: &mut StepContext,
    query: &dyn GithubIssueQuery,
    completion: ChildLaunchCompletion<'_>,
) -> Result<StepOutcome, EngineError> {
    let effective_result =
        classify_child_run_result(completion.result.clone(), completion.run_status.as_ref());
    let outcome = match effective_result {
        ChildWorkflowRunResult::CompletedSuccess => "completed_success",
        ChildWorkflowRunResult::CompletedFailure => "completed_failure",
        ChildWorkflowRunResult::WaitingExternal => "waiting_external",
    };
    let status = match effective_result {
        ChildWorkflowRunResult::CompletedSuccess => LeaseStatus::ReadyToResume,
        ChildWorkflowRunResult::CompletedFailure => LeaseStatus::Failed,
        ChildWorkflowRunResult::WaitingExternal => LeaseStatus::WaitingExternal,
    };
    update_lease_status(
        &daemon_connection()?,
        &completion.lease.lease_id,
        status,
        Some(&completion.request.run_id),
    )
    .map_err(sql_error)?;
    write_launch_artifact(
        state,
        json!({
            "launched": true,
            "child_issue_number": completion.child,
            "child_workflow_type_id": completion.request.workflow_type_id,
            "child_config_id": completion.request.config_id,
            "run_id": completion.request.run_id,
            "lease_id": completion.lease.lease_id,
            "resumed": completion.lease.run_id.is_some(),
            "outcome": outcome,
            "run_status": completion.run_status.as_ref().map(ToString::to_string),
            "pr": completion.pr
        }),
    )?;
    context.set("child_run_id", &completion.request.run_id);
    if let Some(pr_state) = completion.pr.as_ref() {
        context.set("child_pr_number", &pr_state.number.to_string());
    }
    update_rollup(
        state,
        completion.child,
        Some(&completion.request.run_id),
        outcome,
        completion.pr.as_ref(),
    )?;
    if effective_result == ChildWorkflowRunResult::WaitingExternal {
        write_child_workflow_wait_artifact(
            state,
            completion.child,
            completion.lease,
            Some(&completion.request.run_id),
            "child_workflow_waiting_external",
            completion.run_status.as_ref(),
        )?;
    }
    if effective_result == ChildWorkflowRunResult::CompletedFailure {
        record_terminal_child_failure(state, query, &completion)?;
    }
    Ok(match effective_result {
        ChildWorkflowRunResult::CompletedFailure => StepOutcome::Fixable,
        ChildWorkflowRunResult::CompletedSuccess => StepOutcome::Success,
        ChildWorkflowRunResult::WaitingExternal => StepOutcome::Wait,
    })
}

fn record_terminal_child_failure(
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
    completion: &ChildLaunchCompletion<'_>,
) -> Result<(), EngineError> {
    write_json(
        &state.artifact_root,
        "child-terminal-state.json",
        &json!({
            "child_issue_number": completion.child,
            "state": "failed_child_run",
            "child_run_id": completion.request.run_id,
            "lease_id": completion.lease.lease_id,
            "run_status": completion.run_status.as_ref().map(ToString::to_string),
            "pr": completion.pr
        }),
    )?;
    query
        .remove_label(&state.repo, completion.child, &state.luther_label)
        .map_err(github_error)?;
    query
        .comment_issue(
            &state.repo,
            state.parent_issue_number,
            &format!(
                "Parent orchestration is paused because child issue #{} reached a terminal failed workflow state.",
                completion.child
            ),
        )
        .map_err(github_error)?;
    update_rollup(
        state,
        completion.child,
        Some(&completion.request.run_id),
        "failed_child_run",
        completion.pr.as_ref(),
    )
}

fn classify_child_run_result(
    process_result: ChildWorkflowRunResult,
    run_status: Option<&RunStatus>,
) -> ChildWorkflowRunResult {
    match run_status {
        Some(
            RunStatus::WaitingForChecks | RunStatus::WaitingExternal | RunStatus::ReadyToResume,
        ) => ChildWorkflowRunResult::WaitingExternal,
        Some(RunStatus::Completed | RunStatus::Merged) => ChildWorkflowRunResult::CompletedSuccess,
        Some(RunStatus::Failed | RunStatus::Abandoned | RunStatus::Cancelled) => {
            ChildWorkflowRunResult::CompletedFailure
        }
        Some(_) | None => process_result,
    }
}

fn write_launch_artifact(state: &OrchestrationState, value: Value) -> Result<(), EngineError> {
    write_json(&state.artifact_root, "child-run-launch.json", &value)
}

fn update_rollup(
    state: &OrchestrationState,
    child: u64,
    run_id: Option<&str>,
    outcome: &str,
    pr: Option<&GithubIssuePrState>,
) -> Result<(), EngineError> {
    let mut rollup = read_rollup(&state.artifact_root)?;
    rollup.parent_issue_number = state.parent_issue_number;
    rollup
        .children
        .retain(|entry| entry.child_issue_number != child);
    rollup.children.push(ChildRollupEntry {
        child_issue_number: child,
        child_run_id: run_id.map(str::to_string),
        child_artifact_dir: run_id.and_then(|run_id| {
            state.artifact_dir.as_ref().map(|base| {
                child_artifact_dir(base, child, run_id)
                    .to_string_lossy()
                    .to_string()
            })
        }),
        pr_number: pr.map(|state| state.number),
        pr_state: pr.map(|state| state.state.clone()),
        merge_sha: pr.and_then(|state| state.merge_commit_sha.clone()),
        outcome: Some(outcome.to_string()),
        non_actionable_reason: non_actionable_reason_for_outcome(outcome),
    });
    rollup
        .children
        .sort_by_key(|entry| entry.child_issue_number);
    write_json(
        &state.artifact_root,
        "parent-orchestration-rollup.json",
        &rollup,
    )
}

fn non_actionable_reason_for_outcome(outcome: &str) -> Option<String> {
    match outcome {
        "non_actionable_child" => Some("child issue is explicitly non-actionable".to_string()),
        "non_actionable_child_lease" => {
            Some("child lease is already terminal outside the parent orchestrator".to_string())
        }
        _ => None,
    }
}

fn read_rollup(artifact_root: &Path) -> Result<ParentOrchestrationRollup, EngineError> {
    let path = artifact_root.join("parent-orchestration-rollup.json");

    if path.exists() {
        read_json(&path)
    } else {
        Ok(ParentOrchestrationRollup::default())
    }
}

fn child_is_complete(child: &ChildIssueState) -> bool {
    matches!(child.terminal_state, ChildTerminalState::Merged)
}

fn child_is_blocked(child: &ChildIssueState) -> bool {
    matches!(
        child.terminal_state,
        ChildTerminalState::Blocked
            | ChildTerminalState::MergedIssueOpen
            | ChildTerminalState::Superseded
            | ChildTerminalState::ClosedUnmerged
    )
}

fn parent_summary_comment(complete: bool, evaluation: &Value) -> String {
    if complete {
        format!(
            "Parent orchestration complete. Evidence:\n{}",
            serde_json::to_string_pretty(evaluation).unwrap_or_else(|_| "{}".to_string())
        )
    } else {
        format!(
            "Parent orchestration is incomplete or blocked. Current state:\n{}",
            serde_json::to_string_pretty(evaluation).unwrap_or_else(|_| "{}".to_string())
        )
    }
}

fn resume_child_process(
    request: &ChildWorkflowLaunchRequest,
) -> Result<ChildWorkflowRunResult, String> {
    run_child_workflow(request, ChildRunMode::Resume)
}

fn launch_child_process(
    request: &ChildWorkflowLaunchRequest,
) -> Result<ChildWorkflowRunResult, String> {
    run_child_workflow(request, ChildRunMode::Launch)
}

enum ChildRunMode {
    Launch,
    Resume,
}

fn run_child_workflow(
    request: &ChildWorkflowLaunchRequest,
    mode: ChildRunMode,
) -> Result<ChildWorkflowRunResult, String> {
    let config_root = PathBuf::from("config");
    let config_id = validated_child_id(&request.config_id, "config id")?;
    let workflow_type_id = validated_child_id(&request.workflow_type_id, "type id")?;
    let mut config = resolve_workflow_config(config_id, &config_root)
        .map_err(|err| format!("resolve child config '{config_id}': {err}"))?;
    let workflow_type = resolve_workflow_type(workflow_type_id, &config_root)
        .map_err(|err| format!("resolve child workflow type: {err}"))?;
    apply_child_overrides(&mut config, request)?;
    let db_path = crate::runtime_paths::get_data_dir().join("checkpoints.db");
    if matches!(mode, ChildRunMode::Resume) {
        prepare_child_resume(&db_path, request)?;
    }
    let run_context = child_run_context(&config, request);
    let instance =
        WorkflowInstance::create_with_run_id(workflow_type, config.clone(), &request.run_id);
    let mut runner = EngineRunner::with_db_path_and_context(
        instance,
        crate::engine::executor::ExecutorRegistry::with_defaults(),
        &db_path,
        run_context,
    )
    .map_err(|err| err.to_string())?;
    let outcome = runner.run().map_err(|err| err.to_string())?;
    child_result_from_run_outcome(outcome, request, &config, &db_path)
}

fn validated_child_id<'a>(value: &'a str, label: &str) -> Result<&'a str, String> {
    if value.is_empty()
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(format!("unsafe child workflow {label} '{value}'"));
    }
    Ok(value)
}

fn apply_child_overrides(
    config: &mut WorkflowConfig,
    request: &ChildWorkflowLaunchRequest,
) -> Result<(), String> {
    let overrides = TargetProfileOverrides {
        repo: Some(request.repo.clone()),
        issue: Some(request.issue_number.to_string()),
        work_dir: request.work_dir.clone(),
        artifact_dir: request.artifact_dir.clone(),
    };
    apply_target_profile_overrides(config, &overrides)
        .map_err(|err| format!("apply child target overrides: {err}"))
}

fn prepare_child_resume(
    db_path: &Path,
    request: &ChildWorkflowLaunchRequest,
) -> Result<(), String> {
    let conn = open_parent_orchestration_connection(db_path)?;
    let metadata = get_run_with_conn(&conn, &request.run_id)
        .map_err(|err| err.to_string())?
        .ok_or_else(|| format!("missing child run metadata for {}", request.run_id))?;
    let step = metadata
        .current_step
        .as_deref()
        .filter(|step| !step.is_empty())
        .ok_or_else(|| format!("missing current_step for child resume {}", request.run_id))?;
    crate::engine::commit_continuation(
        &conn,
        &crate::engine::ContinuationRequest {
            run_id: request.run_id.clone(),
            kind: crate::engine::ContinuationKind::Resume,
            force: true,
        },
        step,
    )
    .map(|_| ())
    .map_err(|err| format!("commit child resume: {err}"))
}

fn child_run_context(config: &WorkflowConfig, request: &ChildWorkflowLaunchRequest) -> RunContext {
    RunContext {
        log_path: None,
        artifact_root: request
            .artifact_dir
            .as_ref()
            .map(|path| path.to_string_lossy().to_string())
            .or_else(|| config.variables.get("artifact_dir").cloned()),
        workspace_path: request
            .work_dir
            .as_ref()
            .map(|path| path.to_string_lossy().to_string())
            .or_else(|| config.variables.get("work_dir").cloned()),
        repository: Some(request.repo.clone()),
        issue_number: i64::try_from(request.issue_number).ok(),
        pr_number: None,
        head_sha: None,
    }
}

fn child_result_from_run_outcome(
    outcome: RunOutcome,
    request: &ChildWorkflowLaunchRequest,
    config: &WorkflowConfig,
    db_path: &Path,
) -> Result<ChildWorkflowRunResult, String> {
    match outcome {
        RunOutcome::Success => Ok(ChildWorkflowRunResult::CompletedSuccess),
        RunOutcome::WaitingExternal { step_id, reason } => {
            persist_child_external_wait_state(request, config, db_path, &step_id, &reason)?;
            Ok(ChildWorkflowRunResult::WaitingExternal)
        }
        RunOutcome::Interrupted { step_id } => {
            persist_child_interrupted_state(request, config, db_path, &step_id)?;
            Ok(ChildWorkflowRunResult::WaitingExternal)
        }
        RunOutcome::Failure { .. } | RunOutcome::Abandoned { .. } => {
            Ok(ChildWorkflowRunResult::CompletedFailure)
        }
    }
}

fn persist_child_interrupted_state(
    request: &ChildWorkflowLaunchRequest,
    config: &WorkflowConfig,
    db_path: &Path,
    step_id: &str,
) -> Result<(), String> {
    let conn = open_parent_orchestration_connection(db_path)?;
    let checkpoint = load_checkpoint_with_conn(&conn, &request.run_id)
        .map_err(|err| err.to_string())?
        .ok_or_else(|| {
            format!(
                "missing child interrupted checkpoint for {}",
                request.run_id
            )
        })?;
    let previous = crate::persistence::get_wait_state(&conn, &request.run_id)
        .map_err(|err| err.to_string())?;
    let mut record =
        previous.unwrap_or_else(|| WaitStateRecord::new(&request.run_id, &config.config_id));
    record.lease_id = child_lease_id(&conn, request)?;
    record.workflow_type = config.workflow_type_id.clone();
    record.config_id = config.config_id.clone();
    record.repository = request.repo.clone();
    record.issue_number = request.issue_number;
    record.wait_kind = WaitKind::HumanReview;
    record.wait_condition = json!({
        "step_id": step_id,
        "reason": "child_workflow_interrupted",
        "repository": request.repo,
        "issue_number": request.issue_number,
    });
    record.last_observed_state = json!({
        "classification": "interrupted",
        "step_id": step_id,
        "reason": "child_workflow_interrupted"
    });
    record.poll_interval_seconds = child_wait_poll_interval(config);
    record.max_wait_seconds = None;
    let interval_seconds = i64::try_from(record.poll_interval_seconds).unwrap_or(i64::MAX);
    record.next_poll_at = Utc::now() + Duration::seconds(interval_seconds);
    record.resume_step = checkpoint.step_id.clone();
    record.checkpoint_id = crate::engine::continuation::checkpoint_identity(&checkpoint);
    upsert_wait_state(&conn, &record).map_err(|err| err.to_string())?;
    write_wait_state_artifact(&request.run_id, &record)
        .map(|_| ())
        .map_err(|err| err.to_string())
}

fn persist_child_external_wait_state(
    request: &ChildWorkflowLaunchRequest,
    config: &WorkflowConfig,
    db_path: &Path,
    step_id: &str,
    reason: &str,
) -> Result<(), String> {
    let conn = open_parent_orchestration_connection(db_path)?;
    let checkpoint = load_checkpoint_with_conn(&conn, &request.run_id)
        .map_err(|err| err.to_string())?
        .ok_or_else(|| format!("missing child waiting checkpoint for {}", request.run_id))?;
    let metadata = get_run_with_conn(&conn, &request.run_id).map_err(|err| err.to_string())?;
    let wait_kind = child_wait_kind_for_step(step_id);
    let identity = child_wait_poll_identity(metadata.as_ref(), wait_kind)?;
    let previous = crate::persistence::get_wait_state(&conn, &request.run_id)
        .map_err(|err| err.to_string())?;
    let mut record =
        previous.unwrap_or_else(|| WaitStateRecord::new(&request.run_id, &config.config_id));
    record.lease_id = child_lease_id(&conn, request)?;
    record.workflow_type = config.workflow_type_id.clone();
    record.config_id = config.config_id.clone();
    record.repository = request.repo.clone();
    record.issue_number = request.issue_number;
    record.pr_number = identity.pr_number;
    record.head_sha = identity.head_sha;
    record.wait_kind = wait_kind;
    record.wait_condition = json!({
        "step_id": step_id,
        "reason": reason,
        "repository": request.repo,
        "issue_number": request.issue_number,
    });
    record.last_observed_state = json!({
        "classification": "suspended",
        "step_id": step_id,
        "reason": reason
    });
    record.poll_interval_seconds = child_wait_poll_interval(config);
    record.max_wait_seconds = None;
    let interval_seconds = i64::try_from(record.poll_interval_seconds).unwrap_or(i64::MAX);
    record.next_poll_at = Utc::now() + Duration::seconds(interval_seconds);
    record.resume_step = checkpoint.step_id.clone();
    record.checkpoint_id = crate::engine::continuation::checkpoint_identity(&checkpoint);
    upsert_wait_state(&conn, &record).map_err(|err| err.to_string())?;
    write_wait_state_artifact(&request.run_id, &record)
        .map(|_| ())
        .map_err(|err| err.to_string())
}

fn child_wait_kind_for_step(step_id: &str) -> WaitKind {
    match step_id {
        "watch_pr_checks" => WaitKind::PrChecks,
        "collect_coderabbit_feedback" => WaitKind::CoderabbitReview,
        "merge_pr" | "wait_for_merge" => WaitKind::PrMerge,
        "launch_or_resume_child_workflow" | "dependency_child_workflow" => {
            WaitKind::DependencyChildWorkflow
        }
        "wait_for_child_merge" | "dependency_child_merge" => WaitKind::DependencyChildMerge,
        "rate_limit_backoff" | "github_rate_limit_backoff" => WaitKind::RateLimitBackoff,
        _ => WaitKind::HumanReview,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChildWaitIdentity {
    pr_number: Option<u64>,
    head_sha: Option<String>,
}

fn child_wait_poll_identity(
    metadata: Option<&RunMetadata>,
    wait_kind: WaitKind,
) -> Result<ChildWaitIdentity, String> {
    let identity = ChildWaitIdentity {
        pr_number: metadata
            .and_then(|md| md.pr_number)
            .and_then(|number| u64::try_from(number).ok()),
        head_sha: metadata.and_then(|md| md.head_sha.clone()),
    };
    match wait_kind {
        WaitKind::PrChecks if identity.pr_number.is_none() || identity.head_sha.is_none() => {
            Err("missing child PR number or head SHA for PR checks wait state".to_string())
        }
        WaitKind::CoderabbitReview
        | WaitKind::HumanReview
        | WaitKind::PrMerge
        | WaitKind::DependencyChildMerge
            if identity.pr_number.is_none() =>
        {
            Err(format!(
                "missing child PR number for {wait_kind} wait state"
            ))
        }
        _ => Ok(identity),
    }
}

fn child_wait_poll_interval(config: &WorkflowConfig) -> u64 {
    config
        .discovery
        .as_ref()
        .and_then(|discovery| discovery.poll_interval_secs)
        .unwrap_or(300)
}

fn child_lease_id(
    conn: &rusqlite::Connection,
    request: &ChildWorkflowLaunchRequest,
) -> Result<Option<String>, String> {
    get_lease_for_issue(conn, &request.repo, request.issue_number)
        .map(|lease| lease.map(|lease| lease.lease_id))
        .map_err(|err| err.to_string())
}

fn sql_error(err: rusqlite::Error) -> EngineError {
    parent_error(format!("lease database error: {err}"))
}

fn evaluate_parent_completion(
    context: &mut StepContext,
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
) -> Result<StepOutcome, EngineError> {
    refresh_parent_completion_evidence(state, query)?;
    let states: Vec<ChildIssueState> =
        read_json(&state.artifact_root.join("subissue-state-snapshot.json"))?;
    let rollup = read_rollup(&state.artifact_root)?;
    let parent: GithubIssue = read_json(&state.artifact_root.join("parent-issue.json"))?;
    let active_children = incomplete_child_numbers(&states, &rollup);
    let blocked_children = blocked_child_numbers(&states);
    let active_runs = active_child_leases(state, &states)?;
    let merged_pr_children: Vec<_> = rollup
        .children
        .iter()
        .filter(|child| child.outcome.as_deref() == Some("merged"))
        .cloned()
        .collect();
    let closed_without_completion_evidence: Vec<u64> = states
        .iter()
        .filter(|child| child.terminal_state == ChildTerminalState::Closed)
        .map(|child| child.issue_number)
        .collect();
    let acceptance = evaluate_acceptance_criteria(parent.body.as_deref(), &states, &rollup);
    let child_completion_evidence = child_completion_evidence(&states, &rollup);
    let native_subissues_closed_or_non_actionable = active_children.is_empty();
    let required_prs_merged_or_superseded = required_prs_satisfied(&states, &rollup);
    let no_active_child_runs = active_runs.is_empty();
    let no_parent_followup_remaining = acceptance.remaining_work.is_empty();
    let complete = native_subissues_closed_or_non_actionable
        && required_prs_merged_or_superseded
        && no_active_child_runs
        && acceptance.satisfied
        && no_parent_followup_remaining
        && blocked_children.is_empty();
    write_json(
        &state.artifact_root,
        "parent-completion-evaluation.json",
        &json!({
            "complete": complete,
            "native_subissues_closed_or_non_actionable": native_subissues_closed_or_non_actionable,
            "required_child_prs_merged_or_superseded": required_prs_merged_or_superseded,
            "no_active_child_workflow_runs": no_active_child_runs,
            "parent_acceptance_criteria_satisfied": acceptance.satisfied,
            "no_parent_followup_remaining": no_parent_followup_remaining,
            "acceptance_criteria_satisfied": acceptance.satisfied,
            "acceptance_criteria_evidence": acceptance.evidence,
            "active_child_issues": active_children,
            "blocked_child_issues": blocked_children,
            "active_child_runs": active_runs,
            "merged_child_prs": merged_pr_children,
            "closed_without_completion_evidence": closed_without_completion_evidence,
            "child_completion_evidence": child_completion_evidence,
            "children": states,
            "rollup": rollup,
            "remaining_work": acceptance.remaining_work
        }),
    )?;
    context.set("parent_complete", if complete { "true" } else { "false" });
    Ok(if complete {
        StepOutcome::Success
    } else {
        StepOutcome::Fixable
    })
}

fn incomplete_child_numbers(
    states: &[ChildIssueState],
    rollup: &ParentOrchestrationRollup,
) -> Vec<u64> {
    states
        .iter()
        .filter(|child| !child_completion_satisfied(child, rollup))
        .map(|child| child.issue_number)
        .collect()
}

fn blocked_child_numbers(states: &[ChildIssueState]) -> Vec<u64> {
    states
        .iter()
        .filter(|child| child_is_blocked(child))
        .map(|child| child.issue_number)
        .collect()
}

fn required_prs_satisfied(states: &[ChildIssueState], rollup: &ParentOrchestrationRollup) -> bool {
    states.iter().all(|child| match child.terminal_state {
        ChildTerminalState::Merged => true,
        ChildTerminalState::Closed => child_has_explicit_non_actionable_reason(child, rollup),
        _ => false,
    }) && !rollup
        .children
        .iter()
        .any(unresolved_rollup_outcome_requires_pr)
}

fn child_completion_satisfied(child: &ChildIssueState, rollup: &ParentOrchestrationRollup) -> bool {
    child_is_complete(child) || child_has_explicit_non_actionable_reason(child, rollup)
}

fn child_has_explicit_non_actionable_reason(
    child: &ChildIssueState,
    rollup: &ParentOrchestrationRollup,
) -> bool {
    child.terminal_state == ChildTerminalState::Closed
        && rollup.children.iter().any(|entry| {
            entry.child_issue_number == child.issue_number
                && matches!(
                    entry.outcome.as_deref(),
                    Some("non_actionable_child" | "non_actionable_child_lease")
                )
                && entry
                    .non_actionable_reason
                    .as_deref()
                    .is_some_and(|reason| !reason.trim().is_empty())
        })
}

fn child_completion_evidence(
    states: &[ChildIssueState],
    rollup: &ParentOrchestrationRollup,
) -> Vec<Value> {
    states
        .iter()
        .map(|child| {
            let rollup_entry = rollup
                .children
                .iter()
                .find(|entry| entry.child_issue_number == child.issue_number);
            json!({
                "child_issue_number": child.issue_number,
                "terminal_state": child.terminal_state,
                "pr_number": child.pr_number,
                "completion_satisfied": child_completion_satisfied(child, rollup),
                "non_actionable_reason": rollup_entry.and_then(|entry| entry.non_actionable_reason.clone()),
                "merge_sha": rollup_entry.and_then(|entry| entry.merge_sha.clone()),
                "child_artifact_dir": rollup_entry.and_then(|entry| entry.child_artifact_dir.clone())
            })
        })
        .collect()
}

fn unresolved_rollup_outcome_requires_pr(child: &ChildRollupEntry) -> bool {
    matches!(
        child.outcome.as_deref(),
        Some(
            "missing_child_pr"
                | "superseded_child_pr"
                | "closed_unmerged_pr"
                | "stale_child_run"
                | "failed_child_run"
                | "active_child_lease"
                | "completed_failure",
        )
    )
}

struct AcceptanceEvaluation {
    satisfied: bool,
    evidence: Vec<String>,
    remaining_work: Vec<String>,
}

fn evaluate_acceptance_criteria(
    parent_body: Option<&str>,
    states: &[ChildIssueState],
    rollup: &ParentOrchestrationRollup,
) -> AcceptanceEvaluation {
    let mut evidence = Vec::new();
    let mut remaining_work = Vec::new();
    evidence.push(format!("{} child issue(s) classified", states.len()));
    evidence.push(format!(
        "{} child rollup entry(s) recorded",
        rollup.children.len()
    ));
    let criteria = parent_body.map_or(0, count_acceptance_criteria);
    if criteria == 0 {
        remaining_work.push(
            "parent acceptance criteria require deterministic verification; no explicit checked acceptance checklist was found"
                .to_string(),
        );
    } else {
        let unchecked = parent_body.map_or(0, count_unchecked_acceptance_criteria);
        evidence.push(format!(
            "{criteria} parent acceptance checklist item(s) found; {unchecked} unchecked"
        ));
        if unchecked > 0 {
            remaining_work.push(format!(
                "{unchecked} parent acceptance checklist item(s) remain unchecked"
            ));
        }
    }
    if states
        .iter()
        .any(|child| !child_completion_satisfied(child, rollup))
    {
        remaining_work.push("one or more child issues are not complete".to_string());
    }
    for child in states {
        match child.terminal_state {
            ChildTerminalState::Closed if child_has_explicit_non_actionable_reason(child, rollup) => {
                evidence.push(format!(
                    "child issue #{} is closed with explicit non-actionable evidence",
                    child.issue_number
                ));
            }
            ChildTerminalState::Closed => remaining_work.push(format!(
                "child issue #{} is closed without merged PR evidence or an explicit non-actionable reason",
                child.issue_number
            )),
            ChildTerminalState::Merged => evidence.push(format!(
                "child issue #{} is closed with merged PR evidence",
                child.issue_number
            )),
            ChildTerminalState::MergedIssueOpen => remaining_work.push(format!(
                "child issue #{} has merged PR evidence but is still open",
                child.issue_number
            )),
            _ => remaining_work.push(format!(
                "child issue #{} lacks terminal completion evidence",
                child.issue_number
            )),
        }
    }
    if rollup
        .children
        .iter()
        .any(unresolved_rollup_outcome_requires_pr)
    {
        remaining_work.push("one or more child runs lack merged PR evidence".to_string());
    }
    AcceptanceEvaluation {
        satisfied: remaining_work.is_empty(),
        evidence,
        remaining_work,
    }
}

fn count_unchecked_acceptance_criteria(body: &str) -> usize {
    body.lines()
        .filter(|line| line.trim_start().starts_with("- [ ]"))
        .count()
}

fn refresh_parent_completion_evidence(
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
) -> Result<(), EngineError> {
    let issue = query
        .get_issue(&state.repo, state.parent_issue_number)
        .map_err(github_error)?
        .unwrap_or_else(|| fallback_issue(state.parent_issue_number));
    write_json(&state.artifact_root, "parent-issue.json", &issue)
}

fn count_acceptance_criteria(body: &str) -> usize {
    body.lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("- [x]")
                || trimmed.starts_with("- [X]")
                || trimmed.starts_with("- [ ]")
        })
        .count()
}

fn active_child_leases(
    state: &OrchestrationState,
    children: &[ChildIssueState],
) -> Result<Vec<Value>, EngineError> {
    let conn = daemon_connection()?;
    let mut active = Vec::new();
    for child in children {
        let lease =
            get_lease_for_issue(&conn, &state.repo, child.issue_number).map_err(sql_error)?;
        if let Some(lease) = lease.filter(active_child_lease_blocks_parent) {
            active.push(json!({
                "issue_number": child.issue_number,
                "run_id": lease.run_id,
                "status": lease.status.to_string()
            }));
        }
    }
    Ok(active)
}

fn active_child_lease_blocks_parent(lease: &crate::persistence::leases::IssueLease) -> bool {
    matches!(
        lease.status,
        LeaseStatus::WaitingExternal
            | LeaseStatus::ReadyToResume
            | LeaseStatus::Claimed
            | LeaseStatus::Running
    )
}

fn close_or_report_parent(
    _context: &mut StepContext,
    state: &OrchestrationState,
    query: &dyn GithubIssueQuery,
) -> Result<StepOutcome, EngineError> {
    refresh_parent_completion_evidence(state, query)?;
    let evaluation: Value = read_json(
        &state
            .artifact_root
            .join("parent-completion-evaluation.json"),
    )?;
    let complete = evaluation
        .get("complete")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let body = parent_summary_comment(complete, &evaluation);
    query
        .comment_issue(&state.repo, state.parent_issue_number, &body)
        .map_err(github_error)?;
    if complete {
        query
            .close_issue(&state.repo, state.parent_issue_number)
            .map_err(github_error)?;
    }
    if complete || evaluation_reports_terminal_blocker(&evaluation) {
        query
            .remove_label(&state.repo, state.parent_issue_number, &state.luther_label)
            .map_err(github_error)?;
    }
    write_json(
        &state.artifact_root,
        "parent-close-result.json",
        &json!({
            "closed": complete,
            "commented": true,
            "parent_issue_number": state.parent_issue_number
        }),
    )?;
    Ok(StepOutcome::Success)
}

fn evaluation_reports_terminal_blocker(evaluation: &Value) -> bool {
    evaluation
        .get("blocked_child_issues")
        .and_then(Value::as_array)
        .is_some_and(|blocked| !blocked.is_empty())
}

fn required_context(context: &StepContext, key: &str) -> Result<String, EngineError> {
    context
        .get(key)
        .cloned()
        .ok_or_else(|| parent_error(format!("missing context value '{key}'")))
}

fn parent_issue_number(context: &StepContext) -> Result<u64, EngineError> {
    context
        .get("primary_issue_number")
        .or_else(|| context.get("issue_number"))
        .ok_or_else(|| parent_error("missing context value 'primary_issue_number'".to_string()))?
        .parse::<u64>()
        .map_err(|err| parent_error(format!("invalid numeric parent issue context value: {err}")))
}

fn artifact_root(context: &StepContext, params: &Value) -> Result<PathBuf, EngineError> {
    let template = params
        .get("artifact_root")
        .and_then(Value::as_str)
        .or_else(|| context.get("artifact_root").map(String::as_str))
        .or_else(|| context.get("artifact_dir").map(String::as_str))
        .unwrap_or("{work_dir}/.luther-parent-orchestration");
    let interpolated = interpolate_string(template, context);
    if interpolated.contains('{') {
        return Err(parent_error(format!(
            "artifact_root contains unresolved template token: {interpolated}"
        )));
    }
    Ok(PathBuf::from(interpolated))
}

fn write_json<T: serde::Serialize>(
    artifact_root: &Path,
    name: &str,
    value: &T,
) -> Result<(), EngineError> {
    fs::create_dir_all(artifact_root)
        .map_err(|err| parent_error(format!("create artifact root: {err}")))?;
    let path = artifact_root.join(name);
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|err| parent_error(format!("serialize {name}: {err}")))?;
    let temp_path = path.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(&temp_path, bytes)
        .map_err(|err| parent_error(format!("write {}: {err}", temp_path.display())))?;
    fs::rename(&temp_path, &path).map_err(|err| {
        parent_error(format!(
            "rename {} to {}: {err}",
            temp_path.display(),
            path.display()
        ))
    })
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, EngineError> {
    let bytes =
        fs::read(path).map_err(|err| parent_error(format!("read {}: {err}", path.display())))?;
    serde_json::from_slice(&bytes)
        .map_err(|err| parent_error(format!("parse {}: {err}", path.display())))
}

fn clear_selected_child(artifact_root: &Path) -> Result<(), EngineError> {
    let path = artifact_root.join("selected-child.json");
    if path.exists() {
        fs::remove_file(&path)
            .map_err(|err| parent_error(format!("remove {}: {err}", path.display())))?;
    }
    Ok(())
}

fn read_children(
    artifact_root: &Path,
) -> Result<Vec<crate::adapters::github_issues::GithubSubIssue>, EngineError> {
    read_json(&artifact_root.join("parent-subissues.json"))
}

fn selected_child(artifact_root: &Path) -> Result<Option<u64>, EngineError> {
    let selected: Value = read_json(&artifact_root.join("selected-child.json"))?;
    Ok(selected.get("issue_number").and_then(Value::as_u64))
}

fn fallback_issue(number: u64) -> GithubIssue {
    GithubIssue {
        number,
        title: String::new(),
        state: "open".to_string(),
        labels: Vec::new(),
        assignee: None,
        milestone: None,
        body: None,
    }
}

fn github_error(err: GithubError) -> EngineError {
    parent_error(err.to_string())
}

fn parent_error(message: String) -> EngineError {
    EngineError::StepExecutionError {
        step_id: "parent_orchestration".to_string(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::github_issues::{
        GithubIssuePrState, GithubParentIssue, GithubSubIssue, SubIssueSource,
    };

    #[derive(Default)]
    struct MockQuery {
        issue: Option<GithubIssue>,
        children: Vec<GithubSubIssue>,
        pr: Option<GithubIssuePrState>,
    }

    impl GithubIssueQuery for MockQuery {
        fn list_issues(
            &self,
            _repo: &str,
            _include_labels: &[String],
            _states: &[String],
        ) -> Result<Vec<GithubIssue>, GithubError> {
            Ok(Vec::new())
        }

        fn has_open_pr_for_issue(&self, _repo: &str, _number: u64) -> Result<bool, GithubError> {
            Ok(false)
        }

        fn list_milestones(&self, _repo: &str) -> Result<Vec<String>, GithubError> {
            Ok(Vec::new())
        }

        fn get_issue(&self, _repo: &str, _number: u64) -> Result<Option<GithubIssue>, GithubError> {
            Ok(self.issue.clone())
        }

        fn list_sub_issues(
            &self,
            _repo: &str,
            _number: u64,
        ) -> Result<Vec<GithubSubIssue>, GithubError> {
            Ok(self.children.clone())
        }

        fn get_parent_issue(
            &self,
            _repo: &str,
            _number: u64,
        ) -> Result<Option<GithubParentIssue>, GithubError> {
            Ok(None)
        }

        fn add_label(&self, _repo: &str, _number: u64, _label: &str) -> Result<(), GithubError> {
            Ok(())
        }

        fn remove_label(&self, _repo: &str, _number: u64, _label: &str) -> Result<(), GithubError> {
            Ok(())
        }

        fn pr_state_for_issue(
            &self,
            _repo: &str,
            _number: u64,
        ) -> Result<Option<GithubIssuePrState>, GithubError> {
            Ok(self.pr.clone())
        }

        fn comment_issue(&self, _repo: &str, _number: u64, _body: &str) -> Result<(), GithubError> {
            Ok(())
        }

        fn close_issue(&self, _repo: &str, _number: u64) -> Result<(), GithubError> {
            Ok(())
        }

        fn enable_pr_auto_merge(&self, _repo: &str, _pr_number: u64) -> Result<(), GithubError> {
            Ok(())
        }
    }

    fn issue(number: u64, state: &str) -> GithubIssue {
        GithubIssue {
            number,
            title: format!("Issue {number}"),
            state: state.to_string(),
            labels: Vec::new(),
            assignee: None,
            milestone: None,
            body: None,
        }
    }

    fn context(root: &Path) -> StepContext {
        let mut context = StepContext::new(root.join("work"), "run-parent".to_string());
        context.set("target_repo", "owner/repo");
        context.set("issue_number", "42");
        context.set("artifact_root", &root.join("artifacts").to_string_lossy());
        context.set(
            "parent_orchestration.child_workflow_type_id",
            "llxprt-issue-fix-v1",
        );
        context.set("parent_orchestration.child_config_id", "llxprt-code");
        context
    }

    fn context_with_primary_issue_only(root: &Path) -> StepContext {
        let mut context = StepContext::new(root.join("work"), "run-parent".to_string());
        context.set("target_repo", "owner/repo");
        context.set("primary_issue_number", "42");
        context.set("artifact_root", &root.join("artifacts").to_string_lossy());
        context.set(
            "parent_orchestration.child_workflow_type_id",
            "llxprt-issue-fix-v1",
        );
        context.set("parent_orchestration.child_config_id", "llxprt-code");
        context
    }

    fn unique_child_issue_number() -> u64 {
        static NEXT_CHILD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let micros = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as u64;
        let counter = NEXT_CHILD.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        micros.saturating_add(counter)
    }

    #[test]
    fn child_run_registry_status_overrides_zero_exit_waiting_child() {
        assert_eq!(
            classify_child_run_result(
                ChildWorkflowRunResult::CompletedSuccess,
                Some(&RunStatus::WaitingExternal)
            ),
            ChildWorkflowRunResult::WaitingExternal
        );
    }

    #[test]
    fn closed_child_without_required_pr_is_not_complete_by_default() {
        let state = ChildIssueState {
            issue_number: 7,
            terminal_state: ChildTerminalState::Closed,
            pr_number: None,
        };
        assert!(!child_is_complete(&state));
    }

    #[test]
    fn failed_child_lease_relaunches_fresh_workflow() {
        let temp = tempfile::tempdir().unwrap();
        let mut context = context(temp.path());
        context.set("current_step_id", "launch_or_resume_child_workflow");
        let state = OrchestrationState::from_context(&context, &json!({})).unwrap();
        let conn = daemon_connection().unwrap();
        let child = unique_child_issue_number();
        let lease = try_claim(&conn, &state.repo, child, &state.child_config_id)
            .unwrap()
            .unwrap();
        update_lease_status(
            &conn,
            &lease.lease_id,
            LeaseStatus::Failed,
            Some("old-child-run"),
        )
        .unwrap();

        let action = prepare_child_lease(&state, child).unwrap();

        match action {
            ChildLeaseAction::Launch(lease) => {
                assert_eq!(lease.status, LeaseStatus::Claimed);
                assert_eq!(lease.run_id, None);
            }
            _ => panic!("failed child lease should launch fresh workflow"),
        }
    }

    #[test]
    fn parent_completion_rejects_closed_child_without_explicit_non_actionable_reason() {
        let states = vec![ChildIssueState {
            issue_number: 7,
            terminal_state: ChildTerminalState::Closed,
            pr_number: None,
        }];
        let rollup = ParentOrchestrationRollup {
            parent_issue_number: 42,
            children: vec![],
        };

        let evaluation =
            evaluate_acceptance_criteria(Some("- [x] Parent acceptance"), &states, &rollup);

        assert!(!required_prs_satisfied(&states, &rollup));
        assert!(!evaluation.satisfied);
        assert!(evaluation
            .remaining_work
            .iter()
            .any(|work| work.contains("explicit non-actionable reason")));
    }

    #[test]
    fn parent_completion_accepts_closed_child_with_explicit_non_actionable_reason() {
        let states = vec![ChildIssueState {
            issue_number: 7,
            terminal_state: ChildTerminalState::Closed,
            pr_number: None,
        }];
        let rollup = ParentOrchestrationRollup {
            parent_issue_number: 42,
            children: vec![ChildRollupEntry {
                child_issue_number: 7,
                child_run_id: None,
                child_artifact_dir: None,
                pr_number: None,
                pr_state: None,
                merge_sha: None,
                outcome: Some("non_actionable_child".to_string()),
                non_actionable_reason: Some("closed before orchestration as duplicate".to_string()),
            }],
        };

        let evaluation =
            evaluate_acceptance_criteria(Some("- [x] Parent acceptance"), &states, &rollup);

        assert!(required_prs_satisfied(&states, &rollup));
        assert!(evaluation.satisfied);
        assert!(evaluation
            .evidence
            .iter()
            .any(|evidence| evidence.contains("explicit non-actionable evidence")));
    }

    #[test]
    fn parent_completion_accepts_closed_child_with_non_actionable_lease_reason() {
        let states = vec![ChildIssueState {
            issue_number: 7,
            terminal_state: ChildTerminalState::Closed,
            pr_number: None,
        }];
        let rollup = ParentOrchestrationRollup {
            parent_issue_number: 42,
            children: vec![ChildRollupEntry {
                child_issue_number: 7,
                child_run_id: None,
                child_artifact_dir: None,
                pr_number: None,
                pr_state: None,
                merge_sha: None,
                outcome: Some("non_actionable_child_lease".to_string()),
                non_actionable_reason: Some(
                    "child lease already terminal before parent run".to_string(),
                ),
            }],
        };

        let evaluation =
            evaluate_acceptance_criteria(Some("- [x] Parent acceptance"), &states, &rollup);

        assert!(required_prs_satisfied(&states, &rollup));
        assert!(evaluation.satisfied);
    }

    #[test]
    fn parent_completion_rejects_unresolved_superseded_child() {
        let states = vec![ChildIssueState {
            issue_number: 7,
            terminal_state: ChildTerminalState::Superseded,
            pr_number: Some(17),
        }];
        let rollup = ParentOrchestrationRollup {
            parent_issue_number: 42,
            children: vec![ChildRollupEntry {
                child_issue_number: 7,
                child_run_id: Some("child-run".to_string()),
                child_artifact_dir: Some("/tmp/parent/children/issue-7/child-run".to_string()),
                pr_number: Some(17),
                pr_state: Some("superseded".to_string()),
                merge_sha: None,
                outcome: Some("superseded_child_pr".to_string()),
                non_actionable_reason: None,
            }],
        };

        let evaluation = evaluate_acceptance_criteria(None, &states, &rollup);

        assert!(!required_prs_satisfied(&states, &rollup));
        assert!(!evaluation.satisfied);
        assert!(evaluation
            .remaining_work
            .iter()
            .any(|work| work.contains("lack merged PR evidence")));
    }

    #[test]
    fn auto_merge_is_gated_on_green_checks_and_review_state() {
        assert_eq!(auto_merge_block_reason(&ready_pr(17)), None);
        assert_eq!(
            auto_merge_block_reason(&pr_with_checks(17, Some("pending"), None)),
            Some("checks_not_passed")
        );
        assert_eq!(
            auto_merge_block_reason(&pr_with_checks(
                17,
                Some("passed"),
                Some("changes_requested")
            )),
            Some("review_not_approved")
        );
    }

    #[test]
    fn failed_child_run_is_recoverable_but_unsatisfied() {
        let states = vec![ChildIssueState {
            issue_number: 7,
            terminal_state: ChildTerminalState::FailedRun,
            pr_number: None,
        }];
        let rollup = ParentOrchestrationRollup {
            parent_issue_number: 42,
            children: vec![ChildRollupEntry {
                child_issue_number: 7,
                child_run_id: Some("child-run".to_string()),
                child_artifact_dir: Some("/tmp/parent/children/issue-7/child-run".to_string()),
                pr_number: None,
                pr_state: None,
                merge_sha: None,
                outcome: Some("failed_child_run".to_string()),
                non_actionable_reason: None,
            }],
        };

        assert!(!child_is_blocked(&states[0]));
        assert!(!required_prs_satisfied(&states, &rollup));
    }

    #[test]
    fn parent_completion_rejects_merged_pr_when_child_issue_is_open() {
        let states = vec![ChildIssueState {
            issue_number: 7,
            terminal_state: ChildTerminalState::MergedIssueOpen,
            pr_number: Some(17),
        }];
        let rollup = ParentOrchestrationRollup {
            parent_issue_number: 42,
            children: vec![ChildRollupEntry {
                child_issue_number: 7,
                child_run_id: Some("child-run".to_string()),
                child_artifact_dir: Some("/tmp/parent/children/issue-7/child-run".to_string()),
                pr_number: Some(17),
                pr_state: Some("merged".to_string()),
                merge_sha: Some("abc123".to_string()),
                outcome: Some("merged".to_string()),
                non_actionable_reason: None,
            }],
        };

        let evaluation =
            evaluate_acceptance_criteria(Some("- [x] Parent acceptance"), &states, &rollup);

        assert!(child_is_blocked(&states[0]));
        assert!(!child_is_complete(&states[0]));
        assert!(!required_prs_satisfied(&states, &rollup));
        assert!(evaluation
            .remaining_work
            .iter()
            .any(|work| work.contains("still open")));
    }

    #[test]
    fn child_config_and_workflow_ids_reject_path_traversal() {
        assert!(validated_child_id("parent-orchestrator-luther", "config id").is_ok());
        assert!(validated_child_id("llxprt-issue-fix-v1", "type id").is_ok());
        assert!(validated_child_id("../llxprt-code", "config id").is_err());
        assert!(validated_child_id("../../workflows/llxprt-issue-fix-v1", "type id").is_err());
        assert!(validated_child_id("llxprt/code", "config id").is_err());
    }

    #[test]
    fn parent_executor_discovers_orders_and_selects_child() {
        let temp = tempfile::tempdir().unwrap();
        let mut context = context(temp.path());
        let children = unordered_children();
        let expected_child = children
            .iter()
            .min_by_key(|child| child.position)
            .unwrap()
            .issue
            .number
            .to_string();
        let query = MockQuery {
            issue: Some(issue(42, "open")),
            children,
            pr: None,
        };
        let executor = ParentOrchestrationExecutorWithQuery::with_runner(query, MockChildRunner);
        for step in [
            "load_parent_issue",
            "discover_subissues",
            "classify_subissues",
            "determine_subissue_order",
            "select_next_child",
        ] {
            context.set_current_step_id(step);
            let outcome = executor.execute(&mut context, &json!({})).unwrap();
            assert_eq!(outcome, StepOutcome::Success);
        }
        assert_eq!(
            context.get("selected_child_issue_number"),
            Some(&expected_child)
        );
        assert!(temp.path().join("artifacts/selected-child.json").exists());
    }

    #[test]
    fn existing_child_pr_is_observed_without_duplicate_launch() {
        let temp = tempfile::tempdir().unwrap();
        let mut context = context(temp.path());
        let artifact_root = temp.path().join("artifacts");
        let child = unique_child_issue_number();
        write_json(
            &artifact_root,
            "selected-child.json",
            &json!({"issue_number": child}),
        )
        .unwrap();
        let query = MockQuery {
            issue: Some(issue(42, "open")),
            children: vec![GithubSubIssue {
                issue: issue(child, "open"),
                position: Some(1),
                source: SubIssueSource::Native,
            }],
            pr: Some(open_pr(17)),
        };
        let executor = ParentOrchestrationExecutorWithQuery::with_runner(query, NoLaunchRunner);

        context.set_current_step_id("launch_or_resume_child_workflow");
        let outcome = executor.execute(&mut context, &json!({})).unwrap();

        assert_eq!(outcome, StepOutcome::Success);
        assert_eq!(context.get("child_pr_number"), Some(&"17".to_string()));
        assert_observed_pr_artifacts(&artifact_root);
    }

    struct MockChildRunner;

    struct WaitingChildRunner;

    impl ChildWorkflowRunner for WaitingChildRunner {
        fn launch_child(
            &self,
            request: &ChildWorkflowLaunchRequest,
        ) -> Result<ChildWorkflowRunResult, String> {
            assert_eq!(request.workflow_type_id, "llxprt-issue-fix-v1");
            Ok(ChildWorkflowRunResult::WaitingExternal)
        }

        fn run_status(&self, _run_id: &str) -> Result<Option<RunStatus>, String> {
            Ok(Some(RunStatus::WaitingExternal))
        }
    }
    impl ChildWorkflowRunner for MockChildRunner {
        fn launch_child(
            &self,
            request: &ChildWorkflowLaunchRequest,
        ) -> Result<ChildWorkflowRunResult, String> {
            assert_eq!(request.workflow_type_id, "llxprt-issue-fix-v1");
            Ok(ChildWorkflowRunResult::CompletedSuccess)
        }
    }

    struct NoLaunchRunner;

    impl ChildWorkflowRunner for NoLaunchRunner {
        fn launch_child(
            &self,
            _request: &ChildWorkflowLaunchRequest,
        ) -> Result<ChildWorkflowRunResult, String> {
            panic!("parent orchestrator must not duplicate a child with an existing PR");
        }
    }

    #[test]
    fn fresh_waiting_child_launch_records_child_run_id_in_wait_artifact() {
        let temp = tempfile::tempdir().unwrap();
        let mut context = context(temp.path());
        let artifact_root = temp.path().join("artifacts");
        let child = unique_child_issue_number();
        write_json(
            &artifact_root,
            "selected-child.json",
            &json!({"issue_number": child}),
        )
        .unwrap();
        let query = MockQuery {
            issue: Some(issue(42, "open")),
            children: vec![GithubSubIssue {
                issue: issue(child, "open"),
                position: Some(1),
                source: SubIssueSource::Native,
            }],
            pr: None,
        };
        let executor = ParentOrchestrationExecutorWithQuery::with_runner(query, WaitingChildRunner);

        context.set_current_step_id("launch_or_resume_child_workflow");
        let outcome = executor.execute(&mut context, &json!({})).unwrap();

        assert_eq!(outcome, StepOutcome::Wait);
        let launched_run_id = context.get("child_run_id").unwrap();
        let wait: Value = read_json(&artifact_root.join("child-workflow-wait.json")).unwrap();
        assert_eq!(
            wait.get("child_run_id").and_then(Value::as_str),
            Some(launched_run_id.as_str())
        );
        assert!(wait.get("child_run_id").and_then(Value::as_str).is_some());
    }

    fn unordered_children() -> Vec<GithubSubIssue> {
        let first = unique_child_issue_number();
        let second = unique_child_issue_number();
        vec![
            GithubSubIssue {
                issue: issue(second, "open"),
                position: Some(2),
                source: SubIssueSource::Native,
            },
            GithubSubIssue {
                issue: issue(first, "open"),
                position: Some(1),
                source: SubIssueSource::Native,
            },
        ]
    }

    fn open_pr(number: u64) -> GithubIssuePrState {
        GithubIssuePrState {
            number,
            state: "open".to_string(),
            merged: false,
            merge_commit_sha: None,
            review_decision: None,
            status_check_rollup: Some("pending".to_string()),
        }
    }

    fn assert_observed_pr_artifacts(artifact_root: &Path) {
        let launch: Value = read_json(&artifact_root.join("child-run-launch.json")).unwrap();
        assert_eq!(launch.get("launched").and_then(Value::as_bool), Some(false));
        assert_eq!(
            launch.get("reason").and_then(Value::as_str),
            Some("existing_child_pr")
        );
        assert_eq!(
            launch
                .get("pr")
                .and_then(|pr| pr.get("number"))
                .and_then(Value::as_u64),
            Some(17)
        );
        let rollup: ParentOrchestrationRollup =
            read_json(&artifact_root.join("parent-orchestration-rollup.json")).unwrap();

        assert_eq!(rollup.children.len(), 1);
        assert_eq!(
            rollup.children[0].outcome.as_deref(),
            Some("observing_existing_child_pr")
        );
    }
    #[test]
    fn load_parent_issue_accepts_daemon_primary_issue_number() {
        let temp = tempfile::tempdir().unwrap();
        let mut context = context_with_primary_issue_only(temp.path());
        let query = MockQuery {
            issue: Some(issue(42, "open")),
            children: Vec::new(),
            pr: None,
        };
        let executor = ParentOrchestrationExecutorWithQuery::with_runner(query, MockChildRunner);

        context.set_current_step_id("load_parent_issue");
        let outcome = executor.execute(&mut context, &json!({})).unwrap();

        assert_eq!(outcome, StepOutcome::Success);
        assert_eq!(context.get("parent_issue_number"), Some(&"42".to_string()));
        assert!(temp.path().join("artifacts/parent-issue.json").exists());
    }

    fn ready_pr(number: u64) -> GithubIssuePrState {
        pr_with_checks(number, Some("passed"), Some("approved"))
    }

    fn pr_with_checks(
        number: u64,
        status_check_rollup: Option<&str>,
        review_decision: Option<&str>,
    ) -> GithubIssuePrState {
        GithubIssuePrState {
            number,
            state: "open".to_string(),
            merged: false,
            merge_commit_sha: None,
            review_decision: review_decision.map(str::to_string),
            status_check_rollup: status_check_rollup.map(str::to_string),
        }
    }
}
