//! End-to-end parent orchestration executor step-dispatch tests.

use super::super::*;
use super::support::*;

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
