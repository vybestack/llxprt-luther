use super::*;
use crate::adapters::github_issues::{
    GithubIssuePrState, GithubParentIssue, GithubSubIssue, SubIssueSource,
};

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
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    let counter = NEXT_CHILD.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    nanos.saturating_add(counter)
}

#[test]
fn child_run_registry_status_overrides_zero_exit_waiting_child() {
    assert_eq!(
        classify_child_run_result(
            &ChildWorkflowRunResult::CompletedSuccess,
            Some(&RunStatus::WaitingExternal)
        ),
        ChildWorkflowRunResult::WaitingExternal
    );
}

#[test]
fn closed_child_without_required_pr_is_not_complete_by_default() {
    let state = ChildIssueState {
        issue_number: 7,
        terminal_state: ChildIssueStatus::Closed,
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
    let db_path = temp.path().join("checkpoints.db");
    crate::persistence::init_database(&db_path).unwrap();
    let mut conn = open_parent_orchestration_connection(&db_path).unwrap();
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

    let action = prepare_child_lease_with_conn(&state, child, &mut conn).unwrap();

    match action {
        ChildLeaseAction::Launch(lease) => {
            assert_eq!(lease.status, LeaseStatus::Claimed);
            assert_eq!(lease.run_id, None);
        }
        _ => panic!("failed child lease should launch fresh workflow"),
    }
}

#[test]
fn child_lease_claim_contention_waits_without_error() {
    let temp = tempfile::tempdir().unwrap();
    let mut context = context(temp.path());
    context.set("current_step_id", "launch_or_resume_child_workflow");
    let state = OrchestrationState::from_context(&context, &json!({})).unwrap();
    let db_path = temp.path().join("checkpoints.db");
    crate::persistence::init_database(&db_path).unwrap();
    let conn = open_parent_orchestration_connection(&db_path).unwrap();

    let action = claim_child_lease(&state, unique_child_issue_number(), &conn).unwrap();

    match action {
        ChildLeaseAction::Launch(lease) => {
            let contended = claim_child_lease(&state, lease.issue_number, &conn).unwrap();
            match contended {
                ChildLeaseAction::Wait { lease, reason } => {
                    assert!(lease.is_none());
                    assert_eq!(reason, "child_lease_claim_contended");
                }
                _ => panic!("lost child lease claim should wait"),
            }
        }
        _ => panic!("first child lease claim should launch"),
    }
}

#[test]
fn parent_completion_rejects_closed_child_without_explicit_non_actionable_reason() {
    let states = vec![ChildIssueState {
        issue_number: 7,
        terminal_state: ChildIssueStatus::Closed,
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
        terminal_state: ChildIssueStatus::Closed,
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
        terminal_state: ChildIssueStatus::Closed,
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
        terminal_state: ChildIssueStatus::Superseded,
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
        Some("changes_requested")
    );
    assert_eq!(
        auto_merge_block_reason(&pr_with_checks(17, Some("passed"), Some("review_required"))),
        Some("review_required")
    );
}

#[test]
fn failed_child_run_is_recoverable_but_unsatisfied() {
    let states = vec![ChildIssueState {
        issue_number: 7,
        terminal_state: ChildIssueStatus::FailedRun,
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
        terminal_state: ChildIssueStatus::MergedIssueOpen,
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

struct NoLaunchRunner;

impl ChildWorkflowRunner for NoLaunchRunner {
    fn launch_child(
        &self,
        _request: &ChildWorkflowLaunchRequest,
    ) -> Result<ChildWorkflowRunResult, ChildWorkflowRunnerError> {
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

#[test]
fn child_wait_kind_mapping_covers_known_steps() {
    for (step, wait_kind) in [
        ("watch_pr_checks", WaitKind::PrChecks),
        ("collect_coderabbit_feedback", WaitKind::CoderabbitReview),
        ("merge_pr", WaitKind::PrMerge),
        (
            "dependency_child_workflow",
            WaitKind::DependencyChildWorkflow,
        ),
        ("dependency_child_merge", WaitKind::DependencyChildMerge),
        ("github_rate_limit_backoff", WaitKind::RateLimitBackoff),
        ("other", WaitKind::HumanReview),
    ] {
        assert_eq!(child_wait_kind_for_step(step), wait_kind);
    }
}

#[test]
fn child_wait_identity_accepts_required_metadata() {
    let mut metadata = child_run_metadata(Some(17), Some("abc123"));

    let identity = child_wait_poll_identity(Some(&metadata), WaitKind::PrChecks).unwrap();

    assert_eq!(identity.pr_number, Some(17));
    assert_eq!(identity.head_sha.as_deref(), Some("abc123"));

    assert!(child_wait_poll_identity(Some(&metadata), WaitKind::DependencyChildWorkflow).is_ok());
    metadata.head_sha = None;
    assert!(child_wait_poll_identity(Some(&metadata), WaitKind::DependencyChildWorkflow).is_err());
}

#[test]
fn child_wait_identity_rejects_missing_pr_context() {
    let metadata = child_run_metadata(None, Some("abc123"));

    assert!(child_wait_poll_identity(Some(&metadata), WaitKind::CoderabbitReview).is_err());
    assert!(child_wait_poll_identity(None, WaitKind::HumanReview).is_err());
    assert!(child_wait_poll_identity(None, WaitKind::RateLimitBackoff).is_ok());
}

#[test]
fn child_wait_identity_rejects_missing_check_head_sha() {
    let metadata = child_run_metadata(Some(17), None);

    assert!(child_wait_poll_identity(Some(&metadata), WaitKind::PrChecks).is_err());
}

#[test]
fn acceptance_criteria_counts_plus_checklist_markers() {
    let body = "+ [ ] open item\n+ [x] done item\n+ [X] also done";

    assert_eq!(count_acceptance_criteria(body), 3);
    assert_eq!(count_unchecked_acceptance_criteria(body), 1);
}

fn child_run_metadata(pr_number: Option<i64>, head_sha: Option<&str>) -> RunMetadata {
    let mut metadata = RunMetadata::new("child-run", "llxprt-issue-fix-v1", "llxprt-code");
    metadata.pr_number = pr_number;
    metadata.head_sha = head_sha.map(str::to_string);
    metadata
}

#[test]
fn parent_completion_executor_writes_complete_evaluation() {
    let temp = tempfile::tempdir().unwrap();
    let mut context = context(temp.path());
    context.set_current_step_id("evaluate_parent_completion");
    let state = OrchestrationState::from_context(&context, &json!({})).unwrap();
    let child = unique_child_issue_number();
    write_json(
        &state.artifact_root,
        "subissue-state-snapshot.json",
        &vec![ChildIssueState {
            issue_number: child,
            terminal_state: ChildIssueStatus::Merged,
            pr_number: Some(17),
        }],
    )
    .unwrap();
    write_json(
        &state.artifact_root,
        "parent-orchestration-rollup.json",
        &ParentOrchestrationRollup {
            parent_issue_number: 42,
            children: vec![ChildRollupEntry {
                child_issue_number: child,
                child_run_id: Some("child-run".to_string()),
                child_artifact_dir: Some("/tmp/child-run".to_string()),
                pr_number: Some(17),
                pr_state: Some("merged".to_string()),
                merge_sha: Some("abc123".to_string()),
                outcome: Some("merged".to_string()),
                non_actionable_reason: None,
            }],
        },
    )
    .unwrap();
    let query = MockQuery {
        issue: Some(GithubIssue {
            body: Some("- [x] Parent acceptance".to_string()),
            ..issue(42, "open")
        }),
        children: Vec::new(),
        pr: None,
    };

    let outcome = evaluate_parent_completion(&mut context, &state, &query).unwrap();

    assert_eq!(outcome, StepOutcome::Success);
    assert_eq!(context.get("parent_complete"), Some(&"true".to_string()));
    let evaluation: Value = read_json(
        &state
            .artifact_root
            .join("parent-completion-evaluation.json"),
    )
    .unwrap();
    assert_eq!(
        evaluation.get("complete").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        evaluation
            .get("required_child_prs_merged_or_superseded")
            .and_then(Value::as_bool),
        Some(true)
    );
}

#[test]
fn parent_completion_executor_reports_remaining_work() {
    let temp = tempfile::tempdir().unwrap();
    let mut context = context(temp.path());
    context.set_current_step_id("evaluate_parent_completion");
    let state = OrchestrationState::from_context(&context, &json!({})).unwrap();
    let child = unique_child_issue_number();
    write_json(
        &state.artifact_root,
        "subissue-state-snapshot.json",
        &vec![ChildIssueState {
            issue_number: child,
            terminal_state: ChildIssueStatus::Closed,
            pr_number: None,
        }],
    )
    .unwrap();
    write_json(
        &state.artifact_root,
        "parent-orchestration-rollup.json",
        &ParentOrchestrationRollup {
            parent_issue_number: 42,
            children: Vec::new(),
        },
    )
    .unwrap();
    let query = MockQuery {
        issue: Some(GithubIssue {
            body: Some("- [ ] Parent acceptance".to_string()),
            ..issue(42, "open")
        }),
        children: Vec::new(),
        pr: None,
    };

    let outcome = evaluate_parent_completion(&mut context, &state, &query).unwrap();

    assert_eq!(outcome, StepOutcome::Fixable);
    let evaluation: Value = read_json(
        &state
            .artifact_root
            .join("parent-completion-evaluation.json"),
    )
    .unwrap();
    let remaining = evaluation
        .get("remaining_work")
        .and_then(Value::as_array)
        .unwrap();
    assert!(remaining.iter().any(|work| work
        .as_str()
        .is_some_and(|work| work.contains("remain unchecked"))));
    assert!(remaining.iter().any(|work| work
        .as_str()
        .is_some_and(|work| work.contains("explicit non-actionable reason"))));
}

#[test]
fn close_or_report_parent_records_completion_result() {
    let temp = tempfile::tempdir().unwrap();
    let mut context = context(temp.path());

    context.set_current_step_id("close_or_report_parent");
    let state = OrchestrationState::from_context(&context, &json!({})).unwrap();
    write_json(
        &state.artifact_root,
        "parent-completion-evaluation.json",
        &json!({
            "complete": true,
            "blocked_child_issues": [],
            "remaining_work": []
        }),
    )
    .unwrap();
    let query = MockQuery {
        issue: Some(issue(42, "open")),
        children: Vec::new(),
        pr: None,
    };

    let outcome = close_or_report_parent(&mut context, &state, &query).unwrap();

    assert_eq!(outcome, StepOutcome::Success);
    let result: Value = read_json(&state.artifact_root.join("parent-close-result.json")).unwrap();
    assert_eq!(result.get("closed").and_then(Value::as_bool), Some(true));
    assert_eq!(
        result.get("parent_issue_number").and_then(Value::as_u64),
        Some(42)
    );
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
fn interrupted_child_workflow_uses_step_wait_kind() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("checkpoints.db");
    crate::persistence::init_database(&db_path).unwrap();
    let conn = open_parent_orchestration_connection(&db_path).unwrap();
    let request = ChildWorkflowLaunchRequest {
        workflow_type_id: "llxprt-issue-fix-v1".to_string(),
        config_id: "llxprt-code".to_string(),
        run_id: "child-interrupted-run".to_string(),
        repo: "owner/repo".to_string(),
        issue_number: unique_child_issue_number(),
        work_dir: None,
        artifact_dir: None,
        config_root: PathBuf::from("config"),
    };
    let config = workflow_config(&request);
    crate::persistence::checkpoint::save_checkpoint_with_conn(
        &conn,
        &crate::persistence::Checkpoint::new(&request.run_id, "launch_or_resume_child_workflow"),
    )
    .unwrap();

    persist_child_interrupted_state(
        &request,
        &config,
        &db_path,
        "launch_or_resume_child_workflow",
    )
    .unwrap();

    let record = crate::persistence::get_wait_state(&conn, &request.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(record.wait_kind, WaitKind::DependencyChildWorkflow);
}

fn workflow_config(request: &ChildWorkflowLaunchRequest) -> WorkflowConfig {
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
        command_manifest: None,
        target_profile: None,
    }
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

fn child_state(number: u64, status: ChildIssueStatus) -> ChildIssueState {
    ChildIssueState {
        issue_number: number,
        terminal_state: status,
        pr_number: None,
    }
}

fn merged_pr(number: u64) -> GithubIssuePrState {
    GithubIssuePrState {
        number,
        state: "closed".to_string(),
        merged: true,
        merge_commit_sha: Some("abc123".to_string()),
        review_decision: Some("approved".to_string()),
        status_check_rollup: Some("passed".to_string()),
    }
}

#[test]
fn classify_child_pr_wait_covers_all_states() {
    assert!(matches!(
        classify_child_pr_wait(None),
        ChildPrWait::MissingPr
    ));
    assert!(matches!(
        classify_child_pr_wait(Some(&merged_pr(1))),
        ChildPrWait::Merged
    ));
    let mut superseded = pr_with_checks(2, Some("passed"), Some("approved"));
    superseded.state = "SUPERSEDED".to_string();
    assert!(matches!(
        classify_child_pr_wait(Some(&superseded)),
        ChildPrWait::Superseded
    ));
    let mut closed = pr_with_checks(3, Some("passed"), Some("approved"));
    closed.state = "CLOSED".to_string();
    assert!(matches!(
        classify_child_pr_wait(Some(&closed)),
        ChildPrWait::ClosedUnmerged
    ));
    assert!(matches!(
        classify_child_pr_wait(Some(&ready_pr(4))),
        ChildPrWait::ReadyForHumanMerge
    ));
}

#[test]
fn auto_merge_block_reason_flags_unpassed_checks_and_reviews() {
    assert_eq!(
        auto_merge_block_reason(&pr_with_checks(1, Some("pending"), Some("approved"))),
        Some("checks_not_passed")
    );
    assert_eq!(
        auto_merge_block_reason(&pr_with_checks(
            1,
            Some("passed"),
            Some("changes_requested")
        )),
        Some("changes_requested")
    );
    assert_eq!(
        auto_merge_block_reason(&pr_with_checks(1, Some("passed"), Some("review_required"))),
        Some("review_required")
    );
    assert_eq!(
        auto_merge_block_reason(&pr_with_checks(1, Some("passed"), Some("approved"))),
        None
    );
}

#[test]
fn classify_child_run_result_maps_run_status() {
    assert_eq!(
        classify_child_run_result(
            &ChildWorkflowRunResult::CompletedSuccess,
            Some(&RunStatus::Running)
        ),
        ChildWorkflowRunResult::WaitingExternal
    );
    assert_eq!(
        classify_child_run_result(
            &ChildWorkflowRunResult::WaitingExternal,
            Some(&RunStatus::Completed)
        ),
        ChildWorkflowRunResult::CompletedSuccess
    );
    assert_eq!(
        classify_child_run_result(
            &ChildWorkflowRunResult::CompletedSuccess,
            Some(&RunStatus::Failed)
        ),
        ChildWorkflowRunResult::CompletedFailure
    );
    assert_eq!(
        classify_child_run_result(
            &ChildWorkflowRunResult::CompletedFailure,
            Some(&RunStatus::Merged)
        ),
        ChildWorkflowRunResult::CompletedSuccess
    );
    // With no run status, the process result passes through unchanged.
    assert_eq!(
        classify_child_run_result(&ChildWorkflowRunResult::WaitingExternal, None),
        ChildWorkflowRunResult::WaitingExternal
    );
}

#[test]
fn non_actionable_reason_for_outcome_maps_known_outcomes() {
    assert!(non_actionable_reason_for_outcome("non_actionable_child").is_some());
    assert!(non_actionable_reason_for_outcome("non_actionable_child_lease").is_some());
    assert!(non_actionable_reason_for_outcome("merged").is_none());
}

#[test]
fn child_is_complete_and_blocked_predicates() {
    assert!(child_is_complete(&child_state(1, ChildIssueStatus::Merged)));
    assert!(!child_is_complete(&child_state(
        1,
        ChildIssueStatus::Blocked
    )));
    assert!(child_is_blocked(&child_state(1, ChildIssueStatus::Blocked)));
    assert!(child_is_blocked(&child_state(
        1,
        ChildIssueStatus::Superseded
    )));
    assert!(child_is_blocked(&child_state(
        1,
        ChildIssueStatus::ClosedUnmerged
    )));
    assert!(child_is_blocked(&child_state(
        1,
        ChildIssueStatus::MergedIssueOpen
    )));
    assert!(!child_is_blocked(&child_state(1, ChildIssueStatus::Merged)));
}

#[test]
fn parent_summary_comment_reflects_completion() {
    let evaluation = json!({"complete": true});
    let complete = parent_summary_comment(true, &evaluation);
    assert!(complete.contains("complete"));
    let incomplete = parent_summary_comment(false, &evaluation);
    assert!(incomplete.contains("incomplete") || incomplete.contains("blocked"));
}

#[test]
fn child_artifact_dir_layout() {
    let dir = child_artifact_dir(Path::new("/base"), 12, "run-xyz");
    assert_eq!(dir, Path::new("/base/issue-12/run-xyz"));
}

#[test]
fn required_prs_satisfied_empty_states_is_vacuously_true() {
    let rollup = ParentOrchestrationRollup::default();
    assert!(required_prs_satisfied(&[], &rollup));
}

#[test]
fn required_prs_satisfied_rejects_pending_child() {
    let states = vec![child_state(1, ChildIssueStatus::Open)];
    let rollup = ParentOrchestrationRollup::default();
    assert!(!required_prs_satisfied(&states, &rollup));
}

#[test]
fn required_prs_satisfied_accepts_all_merged() {
    let states = vec![
        child_state(1, ChildIssueStatus::Merged),
        child_state(2, ChildIssueStatus::Merged),
    ];
    let rollup = ParentOrchestrationRollup::default();
    assert!(required_prs_satisfied(&states, &rollup));
}

#[test]
fn incomplete_and_blocked_child_number_collection() {
    let states = vec![
        child_state(1, ChildIssueStatus::Merged),
        child_state(2, ChildIssueStatus::Blocked),
        child_state(3, ChildIssueStatus::Open),
    ];
    let rollup = ParentOrchestrationRollup::default();
    let incomplete = incomplete_child_numbers(&states, &rollup);
    assert!(incomplete.contains(&2));
    assert!(incomplete.contains(&3));
    assert!(!incomplete.contains(&1));
    let blocked = blocked_child_numbers(&states);
    assert_eq!(blocked, vec![2]);
}

#[test]
fn unresolved_rollup_outcome_requires_pr_detects_pending_outcomes() {
    let mut entry = ChildRollupEntry {
        child_issue_number: 1,
        child_run_id: None,
        child_artifact_dir: None,
        pr_number: None,
        pr_state: None,
        merge_sha: None,
        outcome: Some("missing_child_pr".to_string()),
        non_actionable_reason: None,
    };
    assert!(unresolved_rollup_outcome_requires_pr(&entry));
    entry.outcome = Some("merged".to_string());
    assert!(!unresolved_rollup_outcome_requires_pr(&entry));
}

#[test]
fn count_acceptance_criteria_counts_checklist_items() {
    let body = "- [x] done\n- [ ] todo\n* [X] also done\nplain line";
    assert_eq!(count_acceptance_criteria(body), 3);
    assert_eq!(count_unchecked_acceptance_criteria(body), 1);
}

#[test]
fn checklist_marker_recognizes_bullets() {
    assert!(checklist_marker("- [ ] item", &["[ ]"]));
    assert!(checklist_marker("* [x] item", &["[x]"]));
    assert!(checklist_marker("+ [X] item", &["[X]"]));
    assert!(!checklist_marker("no marker", &["[ ]"]));
}

#[test]
fn active_child_lease_blocks_parent_for_active_statuses() {
    use crate::persistence::leases::{IssueLease, LeaseStatus};
    fn lease(status: LeaseStatus) -> IssueLease {
        IssueLease {
            lease_id: "l".to_string(),
            issue_repo: "o/r".to_string(),
            issue_number: 1,
            config_id: "cfg".to_string(),
            run_id: Some("run".to_string()),
            status,
            claimed_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            heartbeat_at: chrono::Utc::now(),
        }
    }
    assert!(active_child_lease_blocks_parent(&lease(
        LeaseStatus::Running
    )));
    assert!(active_child_lease_blocks_parent(&lease(
        LeaseStatus::Claimed
    )));
    assert!(active_child_lease_blocks_parent(&lease(
        LeaseStatus::WaitingExternal
    )));
    assert!(active_child_lease_blocks_parent(&lease(
        LeaseStatus::ReadyToResume
    )));
    assert!(!active_child_lease_blocks_parent(&lease(
        LeaseStatus::Completed
    )));
    assert!(!active_child_lease_blocks_parent(&lease(
        LeaseStatus::Failed
    )));
}

fn rollup_state(artifact_root: PathBuf, artifact_dir: Option<PathBuf>) -> OrchestrationState {
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

#[test]
fn read_rollup_defaults_when_missing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let rollup = read_rollup(temp.path()).expect("read rollup");
    assert_eq!(rollup.parent_issue_number, 0);
    assert!(rollup.children.is_empty());
}

#[test]
fn update_rollup_persists_and_replaces_child_entries() {
    let temp = tempfile::tempdir().expect("tempdir");
    let artifact_dir = temp.path().join("children");
    let state = rollup_state(temp.path().to_path_buf(), Some(artifact_dir));

    let pr = merged_pr(7);
    update_rollup(&state, 5, Some("run-5"), "merged", Some(&pr)).expect("update rollup");

    let rollup = read_rollup(temp.path()).expect("read rollup");
    assert_eq!(rollup.parent_issue_number, 100);
    assert_eq!(rollup.children.len(), 1);
    let entry = &rollup.children[0];
    assert_eq!(entry.child_issue_number, 5);
    assert_eq!(entry.child_run_id.as_deref(), Some("run-5"));
    assert_eq!(entry.pr_number, Some(7));
    assert_eq!(entry.outcome.as_deref(), Some("merged"));
    assert!(entry.child_artifact_dir.is_some());

    // Re-updating the same child replaces (does not duplicate) its entry.
    update_rollup(&state, 5, Some("run-5b"), "non_actionable_child", None).expect("second update");
    let rollup = read_rollup(temp.path()).expect("read rollup again");
    assert_eq!(rollup.children.len(), 1);
    assert_eq!(rollup.children[0].child_run_id.as_deref(), Some("run-5b"));
    assert_eq!(
        rollup.children[0].non_actionable_reason.as_deref(),
        Some("child issue is explicitly non-actionable")
    );
}

#[test]
fn rollup_has_outcome_matches_recorded_outcome() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state = rollup_state(temp.path().to_path_buf(), None);
    update_rollup(&state, 9, None, "blocked", None).expect("update rollup");

    assert!(rollup_has_outcome(&state, 9, "blocked").expect("has outcome"));
    assert!(!rollup_has_outcome(&state, 9, "merged").expect("no outcome"));
    assert!(!rollup_has_outcome(&state, 10, "blocked").expect("other child"));
}

#[test]
fn write_launch_artifact_writes_child_run_launch_json() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state = rollup_state(temp.path().to_path_buf(), None);
    write_launch_artifact(&state, serde_json::json!({"launched": true}))
        .expect("write launch artifact");
    let path = temp.path().join("child-run-launch.json");
    assert!(path.exists());
    let contents = std::fs::read_to_string(&path).expect("read artifact");
    assert!(contents.contains("launched"));
}

#[test]
fn classify_child_run_result_prefers_run_status_over_process_result() {
    use crate::persistence::RunStatus;
    assert!(matches!(
        classify_child_run_result(
            &ChildWorkflowRunResult::CompletedSuccess,
            Some(&RunStatus::Running)
        ),
        ChildWorkflowRunResult::WaitingExternal
    ));
    assert!(matches!(
        classify_child_run_result(
            &ChildWorkflowRunResult::WaitingExternal,
            Some(&RunStatus::Completed)
        ),
        ChildWorkflowRunResult::CompletedSuccess
    ));
    assert!(matches!(
        classify_child_run_result(
            &ChildWorkflowRunResult::WaitingExternal,
            Some(&RunStatus::Failed)
        ),
        ChildWorkflowRunResult::CompletedFailure
    ));
    // With no run status, the process result is passed through unchanged.
    assert!(matches!(
        classify_child_run_result(&ChildWorkflowRunResult::CompletedFailure, None),
        ChildWorkflowRunResult::CompletedFailure
    ));
}

#[test]
fn record_blocked_child_writes_artifact_and_rollup() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state = rollup_state(temp.path().to_path_buf(), None);
    let query = MockQuery {
        issue: None,
        children: Vec::new(),
        pr: None,
    };
    let pr = ready_pr(11);
    let outcome =
        record_blocked_child(&state, &query, 11, Some(&pr), "blocked_reason").expect("blocked");
    assert!(matches!(outcome, StepOutcome::Fixable));

    // The blocking wait artifact is written.
    let wait_path = temp.path().join("child-merge-wait.json");
    assert!(wait_path.exists());
    let contents = std::fs::read_to_string(&wait_path).expect("read wait");
    assert!(contents.contains("blocked_reason"));

    // And the rollup records the block outcome for the child.
    assert!(rollup_has_outcome(&state, 11, "blocked_reason").expect("rollup outcome"));
}

#[test]
fn record_superseded_child_comments_and_blocks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state = rollup_state(temp.path().to_path_buf(), None);
    let query = MockQuery {
        issue: None,
        children: Vec::new(),
        pr: None,
    };
    let pr = ready_pr(12);
    let outcome = record_superseded_child(&state, &query, 12, Some(&pr)).expect("superseded");
    assert!(matches!(outcome, StepOutcome::Fixable));
    assert!(rollup_has_outcome(&state, 12, "superseded_child_pr").expect("rollup outcome"));
}

#[test]
fn attempt_auto_merge_disabled_returns_disabled_reason() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut state = rollup_state(temp.path().to_path_buf(), None);
    state.auto_merge_children = false;
    let query = MockQuery {
        issue: None,
        children: Vec::new(),
        pr: None,
    };
    let result = attempt_auto_merge_if_enabled(&state, &query, Some(&ready_pr(1)));
    assert_eq!(result["attempted"], serde_json::json!(false));
    assert_eq!(result["reason"], serde_json::json!("disabled"));
}

#[test]
fn attempt_auto_merge_enabled_without_pr_reports_missing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut state = rollup_state(temp.path().to_path_buf(), None);
    state.auto_merge_children = true;
    let query = MockQuery {
        issue: None,
        children: Vec::new(),
        pr: None,
    };
    let result = attempt_auto_merge_if_enabled(&state, &query, None);
    assert_eq!(result["reason"], serde_json::json!("missing_pr"));
}

#[test]
fn attempt_auto_merge_blocked_by_failing_checks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut state = rollup_state(temp.path().to_path_buf(), None);
    state.auto_merge_children = true;
    let query = MockQuery {
        issue: None,
        children: Vec::new(),
        pr: None,
    };
    let pr = pr_with_checks(2, Some("pending"), Some("approved"));
    let result = attempt_auto_merge_if_enabled(&state, &query, Some(&pr));
    assert_eq!(result["attempted"], serde_json::json!(false));
    assert_eq!(result["reason"], serde_json::json!("checks_not_passed"));
    assert_eq!(
        result["fallback"],
        serde_json::json!("wait_for_human_merge")
    );
}

#[test]
fn attempt_auto_merge_enabled_succeeds_when_ready() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut state = rollup_state(temp.path().to_path_buf(), None);
    state.auto_merge_children = true;
    let query = MockQuery {
        issue: None,
        children: Vec::new(),
        pr: None,
    };
    let result = attempt_auto_merge_if_enabled(&state, &query, Some(&ready_pr(3)));
    assert_eq!(result["attempted"], serde_json::json!(true));
    assert_eq!(result["enabled"], serde_json::json!(true));
    assert_eq!(result["pr_number"], serde_json::json!(3));
}
