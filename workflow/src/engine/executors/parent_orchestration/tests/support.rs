//! Shared fixtures and mock implementations for parent orchestration tests.

use super::super::*;
pub(super) use crate::adapters::github_issues::{
    GithubIssuePrState, GithubParentIssue, GithubSubIssue, SubIssueSource,
};

pub(super) struct MockQuery {
    pub(super) issue: Option<GithubIssue>,
    pub(super) children: Vec<GithubSubIssue>,
    pub(super) pr: Option<GithubIssuePrState>,
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

/// A mock query that records side-effecting GitHub operations so tests can
/// assert zero side effects on stale child finalization paths.
///
/// The recorded calls are returned via [`RecordingMockQuery::take`] so each
/// test can inspect the operations performed by the code under test. By
/// sharing the recording cell, the mock can be passed by reference (as
/// `&dyn GithubIssueQuery`) while still exposing the recorded operations.
pub(super) struct RecordingMockQuery {
    issue: Option<GithubIssue>,
    children: Vec<GithubSubIssue>,
    pr: Option<GithubIssuePrState>,
    operations: std::sync::Arc<std::sync::Mutex<Vec<RecordedGithubOperation>>>,
}

/// A side-effecting GitHub operation recorded by [`RecordingMockQuery`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RecordedGithubOperation {
    CommentIssue { number: u64, body: String },
    RemoveLabel { number: u64, label: String },
}

impl RecordingMockQuery {
    /// Create a new recording query with no issue/PR data and an empty
    /// recording cell.
    pub(super) fn new() -> Self {
        Self {
            issue: None,
            children: Vec::new(),
            pr: None,
            operations: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// Take ownership of the recorded operations, leaving the cell empty.
    pub(super) fn take(&self) -> Vec<RecordedGithubOperation> {
        std::mem::take(&mut *self.operations.lock().unwrap())
    }
}

impl Default for RecordingMockQuery {
    fn default() -> Self {
        Self::new()
    }
}

impl GithubIssueQuery for RecordingMockQuery {
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

    fn remove_label(&self, _repo: &str, number: u64, label: &str) -> Result<(), GithubError> {
        self.operations
            .lock()
            .unwrap()
            .push(RecordedGithubOperation::RemoveLabel {
                number,
                label: label.to_string(),
            });
        Ok(())
    }

    fn pr_state_for_issue(
        &self,
        _repo: &str,
        _number: u64,
    ) -> Result<Option<GithubIssuePrState>, GithubError> {
        Ok(self.pr.clone())
    }

    fn comment_issue(&self, _repo: &str, number: u64, body: &str) -> Result<(), GithubError> {
        self.operations
            .lock()
            .unwrap()
            .push(RecordedGithubOperation::CommentIssue {
                number,
                body: body.to_string(),
            });
        Ok(())
    }

    fn close_issue(&self, _repo: &str, _number: u64) -> Result<(), GithubError> {
        Ok(())
    }

    fn enable_pr_auto_merge(&self, _repo: &str, _pr_number: u64) -> Result<(), GithubError> {
        Ok(())
    }
}

pub(super) fn issue(number: u64, state: &str) -> GithubIssue {
    GithubIssue {
        number,
        title: format!("Issue {number}"),
        state: state.to_string(),
        labels: Vec::new(),
        assignees: vec![],
        milestone: None,
        body: None,
    }
}

pub(super) fn context(root: &Path) -> StepContext {
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

pub(super) fn context_with_primary_issue_only(root: &Path) -> StepContext {
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

pub(super) fn unique_child_issue_number() -> u64 {
    static NEXT_CHILD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    let counter = NEXT_CHILD.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    nanos.saturating_add(counter)
}

pub(super) struct MockChildRunner;

pub(super) struct WaitingChildRunner;

impl ChildWorkflowRunner for WaitingChildRunner {
    fn launch_child(
        &self,
        request: &ChildWorkflowLaunchRequest,
    ) -> Result<ChildWorkflowRunResult, ChildWorkflowRunnerError> {
        assert_eq!(request.workflow_type_id, "llxprt-issue-fix-v1");
        Ok(ChildWorkflowRunResult::WaitingExternal)
    }

    fn run_status(&self, _run_id: &str) -> Result<Option<RunStatus>, ChildWorkflowRunnerError> {
        Ok(Some(RunStatus::WaitingExternal))
    }
}

impl ChildWorkflowRunner for MockChildRunner {
    fn launch_child(
        &self,
        request: &ChildWorkflowLaunchRequest,
    ) -> Result<ChildWorkflowRunResult, ChildWorkflowRunnerError> {
        assert_eq!(request.workflow_type_id, "llxprt-issue-fix-v1");
        Ok(ChildWorkflowRunResult::CompletedSuccess)
    }
}

pub(super) struct NoLaunchRunner;

impl ChildWorkflowRunner for NoLaunchRunner {
    fn launch_child(
        &self,
        _request: &ChildWorkflowLaunchRequest,
    ) -> Result<ChildWorkflowRunResult, ChildWorkflowRunnerError> {
        panic!("parent orchestrator must not duplicate a child with an existing PR");
    }
}

pub(super) fn unordered_children() -> Vec<GithubSubIssue> {
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

pub(super) fn child_run_metadata(pr_number: Option<i64>, head_sha: Option<&str>) -> RunMetadata {
    let mut metadata = RunMetadata::new("child-run", "llxprt-issue-fix-v1", "llxprt-code");
    metadata.pr_number = pr_number;
    metadata.head_sha = head_sha.map(str::to_string);
    metadata
}

pub(super) fn open_pr(number: u64) -> GithubIssuePrState {
    GithubIssuePrState {
        number,
        state: "open".to_string(),
        merged: false,
        merge_commit_sha: None,
        review_decision: None,
        status_check_rollup: Some("pending".to_string()),
    }
}

pub(super) fn assert_observed_pr_artifacts(artifact_root: &Path) {
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

pub(super) fn workflow_config(request: &ChildWorkflowLaunchRequest) -> WorkflowConfig {
    WorkflowConfig {
        config_id: request.config_id.clone(),
        workflow_type_id: request.workflow_type_id.clone(),
        runtime: crate::workflow::schema::RuntimeConfig {
            timeout_seconds: 3600,
            max_retries: 3,
            parallel_steps: None,
            log_level: None,
        },
        repo: crate::workflow::schema::RepoConfig {
            workspace_strategy: "temp".to_string(),
            branch_template: "test-{run_id}".to_string(),
            base_branch: Some("main".to_string()),
            workspace_root: None,
            project_subdir: None,
            artifact_path_base: None,
            diff_path_base: None,
            diff_path_normalization: crate::workflow::schema::DiffPathNormalization::RepoRelative,
        },
        guard_limits: crate::workflow::schema::GuardLimits {
            max_iterations: Some(3),
            max_file_changes: Some(50),
            max_tokens: Some(10_000),
            max_cost: Some(10.0),
        },
        variables: std::collections::HashMap::new(),
        discovery: None,
        parent_orchestration: Default::default(),
        merge_required: false,
        merge_strategy: None,
        command_manifest: None,
        target_profile: None,
    }
}

pub(super) fn ready_pr(number: u64) -> GithubIssuePrState {
    pr_with_checks(number, Some("passed"), Some("approved"))
}

pub(super) fn pr_with_checks(
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

pub(super) fn child_state(number: u64, status: ChildIssueStatus) -> ChildIssueState {
    ChildIssueState {
        issue_number: number,
        terminal_state: status,
        pr_number: None,
    }
}

pub(super) fn merged_pr(number: u64) -> GithubIssuePrState {
    GithubIssuePrState {
        number,
        state: "closed".to_string(),
        merged: true,
        merge_commit_sha: Some("abc123".to_string()),
        review_decision: Some("approved".to_string()),
        status_check_rollup: Some("passed".to_string()),
    }
}

pub(super) fn rollup_state(
    artifact_root: PathBuf,
    artifact_dir: Option<PathBuf>,
) -> OrchestrationState {
    OrchestrationState {
        current_step: "step".to_string(),
        artifact_root,
        repo: "o/r".to_string(),
        parent_issue_number: 100,
        luther_label: "Luther working".to_string(),
        child_workflow_type_id: "wf".to_string(),
        child_config_id: "cfg".to_string(),
        merge_poll_interval_seconds: 300,
        max_child_merge_wait_seconds: None,
        auto_merge_children: false,
        wait_for_human_merge: true,
        work_dir: None,
        artifact_dir,
        config_root: PathBuf::from("/config"),
    }
}

/// A mock query whose `add_label` always fails with a given `GithubError`. Used
/// to verify that a stranded `Running` lease is compensated back to the observed
/// state when `add_label` fails after the CAS in `start_child_workflow`.
pub(super) struct FailingAddLabelQuery {
    pub(super) error: GithubError,
}

impl FailingAddLabelQuery {
    pub(super) fn new(error: GithubError) -> Self {
        Self { error }
    }
}

impl GithubIssueQuery for FailingAddLabelQuery {
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
        Ok(None)
    }

    fn list_sub_issues(
        &self,
        _repo: &str,
        _number: u64,
    ) -> Result<Vec<GithubSubIssue>, GithubError> {
        Ok(Vec::new())
    }

    fn get_parent_issue(
        &self,
        _repo: &str,
        _number: u64,
    ) -> Result<Option<GithubParentIssue>, GithubError> {
        Ok(None)
    }

    fn add_label(&self, _repo: &str, _number: u64, _label: &str) -> Result<(), GithubError> {
        Err(self.error.clone())
    }

    fn remove_label(&self, _repo: &str, _number: u64, _label: &str) -> Result<(), GithubError> {
        Ok(())
    }

    fn pr_state_for_issue(
        &self,
        _repo: &str,
        _number: u64,
    ) -> Result<Option<GithubIssuePrState>, GithubError> {
        Ok(None)
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

/// A runner that records whether `launch_child` and `resume_child` were each
/// invoked, tracked separately so a test can assert that neither dispatch path
/// fires. Each is expected *never* to be called when `add_label` fails before
/// dispatch.
pub(super) struct NoLaunchTrackingRunner {
    pub(super) launched: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(super) resumed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl NoLaunchTrackingRunner {
    pub(super) fn new() -> Self {
        Self {
            launched: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            resumed: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
}

impl ChildWorkflowRunner for NoLaunchTrackingRunner {
    fn launch_child(
        &self,
        _request: &ChildWorkflowLaunchRequest,
    ) -> Result<ChildWorkflowRunResult, ChildWorkflowRunnerError> {
        self.launched
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(ChildWorkflowRunResult::CompletedSuccess)
    }

    fn resume_child(
        &self,
        _request: &ChildWorkflowLaunchRequest,
    ) -> Result<ChildWorkflowRunResult, ChildWorkflowRunnerError> {
        self.resumed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(ChildWorkflowRunResult::CompletedSuccess)
    }

    fn run_status(&self, _run_id: &str) -> Result<Option<RunStatus>, ChildWorkflowRunnerError> {
        Ok(Some(RunStatus::Completed))
    }
}
