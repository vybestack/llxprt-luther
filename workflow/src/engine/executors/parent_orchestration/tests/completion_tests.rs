//! Parent completion evaluation and acceptance-criteria tests.

use super::super::*;
use super::support::*;

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
fn acceptance_criteria_counts_plus_checklist_markers() {
    let body = "+ [ ] open item\n+ [x] done item\n+ [X] also done";

    assert_eq!(count_acceptance_criteria(body), 3);
    assert_eq!(count_unchecked_acceptance_criteria(body), 1);
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
