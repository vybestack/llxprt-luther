use super::*;
use std::cell::RefCell;

/// Records argv and returns canned per-call results.
struct MockRunner {
    results: RefCell<Vec<Result<String, GithubError>>>,
    calls: RefCell<Vec<Vec<String>>>,
}

impl MockRunner {
    fn new(results: Vec<Result<String, GithubError>>) -> Self {
        Self {
            results: RefCell::new(results),
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl GithubCommandRunner for MockRunner {
    fn run(&self, argv: &[String]) -> Result<String, GithubError> {
        self.calls.borrow_mut().push(argv.to_vec());
        if self.results.borrow().is_empty() {
            panic!(
                "MockRunner: exhausted canned results on call #{} (argv: {:?})",
                self.calls.borrow().len(),
                argv
            );
        }
        self.results.borrow_mut().remove(0)
    }
}

const SAMPLE: &str = r#"[
    {"number":12,"title":"Fix bug","state":"OPEN",
     "labels":[{"name":"OK for Luther"}],
     "assignees":[{"login":"acoliver"}],
     "milestone":{"title":"v1.2.0"}},
    {"number":5,"title":"No labels","state":"OPEN",
     "labels":[],"assignees":[],"milestone":null}
]"#;

#[test]
fn parse_issue_list_maps_fields() {
    let issues = parse_issue_list(SAMPLE).unwrap();
    assert_eq!(issues.len(), 2);
    assert_eq!(issues[0].number, 12);
    assert_eq!(issues[0].state, "open");
    assert_eq!(issues[0].labels, vec!["OK for Luther"]);
    assert_eq!(issues[0].assignee.as_deref(), Some("acoliver"));
    assert_eq!(issues[0].milestone.as_deref(), Some("v1.2.0"));
    assert_eq!(issues[1].assignee, None);
    assert_eq!(issues[1].milestone, None);
}

#[test]
fn list_issues_builds_correct_argv() {
    let runner = MockRunner::new(vec![Ok(SAMPLE.to_string())]);
    let q = SystemGithubIssueQuery::new(runner);
    let issues = q
        .list_issues("o/r", &["OK for Luther".to_string()], &["open".to_string()])
        .unwrap();
    assert_eq!(issues.len(), 2);
    let calls = q.runner.calls.borrow();
    let argv = &calls[0];
    assert!(argv.contains(&"--repo".to_string()));
    assert!(argv.contains(&"o/r".to_string()));
    assert!(argv.contains(&"--label".to_string()));
    assert!(argv.contains(&"OK for Luther".to_string()));
    assert!(argv.contains(&"--state".to_string()));
    assert!(argv.contains(&"open".to_string()));
}

#[test]
fn has_open_pr_true_and_false() {
    let runner_true = MockRunner::new(vec![Ok(r#"[{"number":99}]"#.to_string())]);
    let q_true = SystemGithubIssueQuery::new(runner_true);
    assert!(q_true.has_open_pr_for_issue("o/r", 12).unwrap());

    let runner_false = MockRunner::new(vec![Ok("[]".to_string())]);
    let q_false = SystemGithubIssueQuery::new(runner_false);
    assert!(!q_false.has_open_pr_for_issue("o/r", 12).unwrap());
}

#[test]
fn has_open_pr_builds_search_argv() {
    let runner = MockRunner::new(vec![Ok("[]".to_string())]);
    let q = SystemGithubIssueQuery::new(runner);
    let _ = q.has_open_pr_for_issue("o/r", 7).unwrap();
    let calls = q.runner.calls.borrow();
    let argv = &calls[0];
    assert!(argv.contains(&"--search".to_string()));
    assert!(argv.contains(&"issue:7".to_string()));
}

#[test]
fn list_milestones_parses_lines() {
    let runner = MockRunner::new(vec![Ok("v1.0.0\nv1.1.0\n\nv2.0.0\n".to_string())]);
    let q = SystemGithubIssueQuery::new(runner);
    let ms = q.list_milestones("o/r").unwrap();
    assert_eq!(ms, vec!["v1.0.0", "v1.1.0", "v2.0.0"]);
}

#[test]
fn repo_owner_name_rejects_invalid_repo_slug() {
    assert!(repo_owner_name("repo-only").is_err());
    assert!(repo_owner_name("owner/").is_err());
    assert!(repo_owner_name("/repo").is_err());
    assert!(repo_owner_name("owner/repo/extra").is_err());
}

#[test]
fn parse_first_sub_issue_page_returns_pagination_cursor() {
    let json = r#"{
        "data": {"repository": {"issue": {
            "number": 1,
            "subIssues": {
                "edges": [],
                "pageInfo": {"hasNextPage": true, "endCursor": "cursor-1"}
            }
        }}}
    }"#;

    let page = parse_first_sub_issue_page(json).unwrap();

    assert!(page.children.is_empty());
    assert_eq!(page.next_cursor.as_deref(), Some("cursor-1"));
}

#[test]
fn parse_parent_issue_response_accepts_parent_only_query_shape() {
    let json = r#"{
        "data": {"repository": {"issue": {
            "parent": {
                "number": 60,
                "title": "Parent coordination issue",
                "state": "OPEN",
                "labels": {"nodes": [{"name": "OK for Luther"}, {"name": "Luther working"}]},
                "assignees": {"nodes": [{"login": "acoliver"}]},
                "milestone": {"title": "v1.0.0"}
            }
        }}}
    }"#;

    let parent = parse_parent_issue_response(json).unwrap().unwrap();

    assert_eq!(parent.issue.number, 60);
    assert_eq!(parent.issue.title, "Parent coordination issue");
    assert_eq!(parent.issue.state, "open");
    assert_eq!(
        parent.issue.labels,
        vec!["OK for Luther".to_string(), "Luther working".to_string()]
    );
    assert_eq!(parent.issue.assignee.as_deref(), Some("acoliver"));
    assert_eq!(parent.issue.milestone.as_deref(), Some("v1.0.0"));
}
#[test]
fn list_sub_issues_empty_native_empty_body_returns_empty_children() {
    let native_empty = r#"{
        "data": {"repository": {"issue": {
            "number": 1,
            "subIssues": {"edges": [], "pageInfo": {"hasNextPage": false}}
        }}}
    }"#;
    let runner = MockRunner::new(vec![
        Ok(native_empty.to_string()),
        Ok(r#"{"body":"No child references here."}"#.to_string()),
    ]);
    let q = SystemGithubIssueQuery::new(runner);

    let children = q.list_sub_issues("o/r", 1).unwrap();

    assert!(children.is_empty());
}

#[test]
fn list_sub_issues_paginates_native_connection() {
    let first_page = r#"{
        "data": {"repository": {"issue": {
            "number": 1,
            "subIssues": {
                "edges": [{"node": {"number": 10, "title": "First", "state": "OPEN"}}],
                "pageInfo": {"hasNextPage": true, "endCursor": "cursor-1"}
            }
        }}}
    }"#;
    let second_page = r#"{
        "data": {"repository": {"issue": {
            "subIssues": {
                "edges": [{"cursor": "cursor-2", "node": {"number": 11, "title": "Second", "state": "OPEN"}}],
                "pageInfo": {"hasNextPage": false, "endCursor": "cursor-2"}
            }
        }}}
    }"#;
    let runner = MockRunner::new(vec![
        Ok(first_page.to_string()),
        Ok(second_page.to_string()),
    ]);
    let q = SystemGithubIssueQuery::new(runner);

    let children = q.list_sub_issues("o/r", 1).unwrap();

    assert_eq!(
        children
            .iter()
            .map(|child| child.issue.number)
            .collect::<Vec<_>>(),
        vec![10, 11]
    );
    let calls = q.runner.calls.borrow();
    assert_eq!(calls.len(), 2);
    assert!(calls[1].contains(&"after=cursor-1".to_string()));
}

#[test]
fn list_sub_issues_uses_body_fallback_when_native_query_fails() {
    let runner = MockRunner::new(vec![
        Err(GithubError::CommandFailed {
            argv: vec!["gh".to_string(), "api".to_string(), "graphql".to_string()],
            exit_code: Some(1),
            stderr: "subIssues unavailable".to_string(),
        }),
        Ok(r#"{"body":"Sub-issue: #42"}"#.to_string()),
        Ok(r#"{"number":42,"title":"Fallback","state":"OPEN","labels":[],"assignees":[],"milestone":null,"body":null}"#.to_string()),
    ]);
    let q = SystemGithubIssueQuery::new(runner);

    let children = q.list_sub_issues("o/r", 1).unwrap();

    assert_eq!(children.len(), 1);
    assert_eq!(children[0].issue.number, 42);
    assert_eq!(children[0].source, SubIssueSource::FallbackChecklist);
}

#[test]
fn list_sub_issues_stops_after_native_page_limit() {
    let first_page = r#"{
        "data": {"repository": {"issue": {
            "number": 1,
            "subIssues": {
                "edges": [],
                "pageInfo": {"hasNextPage": true, "endCursor": "cursor-first"}
            }
        }}}
    }"#;
    let next_page = r#"{
        "data": {"repository": {"issue": {
            "subIssues": {
                "edges": [],
                "pageInfo": {"hasNextPage": true, "endCursor": "cursor-next"}
            }
        }}}
    }"#;
    let mut results = vec![Ok(first_page.to_string())];
    results.extend((1..(MAX_NATIVE_SUB_ISSUE_PAGES + 5)).map(|_| Ok(next_page.to_string())));
    let runner = MockRunner::new(results);
    let q = SystemGithubIssueQuery::new(runner);

    let err = q.list_sub_issues("o/r", 1).unwrap_err();

    assert!(err.to_string().contains("pagination exceeded"));
    assert_eq!(q.runner.calls.borrow().len(), MAX_NATIVE_SUB_ISSUE_PAGES);
}

#[test]
fn enable_pr_auto_merge_uses_allowed_merge_method() {
    assert_auto_merge_method(
        r#"{"mergeCommitAllowed":true,"rebaseMergeAllowed":false,"squashMergeAllowed":false}"#,
        "--merge",
    );
}

#[test]
fn enable_pr_auto_merge_falls_back_to_rebase_when_only_rebase_allowed() {
    assert_auto_merge_method(
        r#"{"mergeCommitAllowed":false,"rebaseMergeAllowed":true,"squashMergeAllowed":false}"#,
        "--rebase",
    );
}

#[test]
fn enable_pr_auto_merge_falls_back_to_squash_when_only_squash_allowed() {
    assert_auto_merge_method(
        r#"{"mergeCommitAllowed":false,"rebaseMergeAllowed":false,"squashMergeAllowed":true}"#,
        "--squash",
    );
}

#[test]
fn enable_pr_auto_merge_prefers_viewer_default_method() {
    assert_auto_merge_method(
        r#"{"mergeCommitAllowed":true,"rebaseMergeAllowed":true,"squashMergeAllowed":true,"viewerDefaultMergeMethod":"REBASE"}"#,
        "--rebase",
    );
}

#[test]
fn enable_pr_auto_merge_ignores_unrecognized_viewer_default() {
    assert_auto_merge_method(
        r#"{"mergeCommitAllowed":true,"rebaseMergeAllowed":false,"squashMergeAllowed":false,"viewerDefaultMergeMethod":"UNKNOWN"}"#,
        "--merge",
    );
}

#[test]
fn enable_pr_auto_merge_errors_when_no_allowed_flags_are_reported() {
    let runner = MockRunner::new(vec![Ok(
        r#"{"mergeCommitAllowed":false,"rebaseMergeAllowed":false,"squashMergeAllowed":false}"#
            .to_string(),
    )]);
    let q = SystemGithubIssueQuery::new(runner);

    let err = q.enable_pr_auto_merge("o/r", 17).unwrap_err();

    assert!(err.to_string().contains("enabled auto-merge method"));
}

fn assert_auto_merge_method(repo_view_json: &str, expected_method: &str) {
    let runner = MockRunner::new(vec![Ok(repo_view_json.to_string()), Ok(String::new())]);
    let q = SystemGithubIssueQuery::new(runner);

    q.enable_pr_auto_merge("o/r", 17).unwrap();

    let calls = q.runner.calls.borrow();
    assert_eq!(calls.len(), 2);
    assert!(calls[1].contains(&expected_method.to_string()));
}

#[test]
fn pr_state_for_issue_prefers_closing_pr_references() {
    let issue_refs = r#"{
        "closedByPullRequestsReferences": [{
            "number": 17,
            "state": "MERGED",
            "merged": true,
            "mergeCommit": {"oid": "abc123"},
            "reviewDecision": "APPROVED",
            "statusCheckRollup": [{"conclusion": "SUCCESS"}]
        }]
    }"#;
    let runner = MockRunner::new(vec![Ok(issue_refs.to_string())]);
    let q = SystemGithubIssueQuery::new(runner);

    let pr = q.pr_state_for_issue("o/r", 7).unwrap().unwrap();

    assert_eq!(pr.number, 17);
    assert!(pr.merged);
    assert_eq!(pr.merge_commit_sha.as_deref(), Some("abc123"));
    assert_eq!(pr.review_decision.as_deref(), Some("approved"));
    assert_eq!(pr.status_check_rollup.as_deref(), Some("passed"));
    let calls = q.runner.calls.borrow();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].contains(&"closedByPullRequestsReferences".to_string()));
}

#[test]
fn pr_state_for_issue_falls_back_to_search_when_issue_has_no_closing_pr() {
    let issue_refs = r#"{"closedByPullRequestsReferences": []}"#;
    let pr_search = r#"[{
        "number": 19,
        "state": "OPEN",
        "merged": false,
        "mergeCommit": null,
        "reviewDecision": null,
        "statusCheckRollup": [{"conclusion": "FAILURE"}]
    }]"#;
    let runner = MockRunner::new(vec![Ok(issue_refs.to_string()), Ok(pr_search.to_string())]);
    let q = SystemGithubIssueQuery::new(runner);

    let pr = q.pr_state_for_issue("o/r", 7).unwrap().unwrap();

    assert_eq!(pr.number, 19);
    assert_eq!(pr.state, "open");
    assert!(!pr.merged);
    assert_eq!(pr.status_check_rollup.as_deref(), Some("failed"));
    let calls = q.runner.calls.borrow();
    assert_eq!(calls.len(), 2);
    assert!(calls[1].contains(&"issue:7".to_string()));
}

#[test]
fn status_check_rollup_accepts_commit_status_states() {
    let argv = vec!["gh".to_string()];
    let passed = parse_pr_state(
        r#"[{"number":20,"state":"OPEN","merged":false,"statusCheckRollup":[{"state":"SUCCESS"}]}]"#,
        &argv,
    )
    .unwrap()
    .unwrap();
    assert_eq!(passed.status_check_rollup.as_deref(), Some("passed"));

    let failed = parse_pr_state(
        r#"[{"number":20,"state":"OPEN","merged":false,"statusCheckRollup":[{"state":"ERROR"}]}]"#,
        &argv,
    )
    .unwrap()
    .unwrap();
    assert_eq!(failed.status_check_rollup.as_deref(), Some("failed"));
}

#[test]
fn pr_state_for_issue_reports_absent_pr_when_no_reference_or_search_hit() {
    let runner = MockRunner::new(vec![
        Ok(r#"{"closedByPullRequestsReferences": []}"#.to_string()),
        Ok("[]".to_string()),
    ]);
    let q = SystemGithubIssueQuery::new(runner);

    assert!(q.pr_state_for_issue("o/r", 7).unwrap().is_none());
}

#[test]
fn pr_state_parser_distinguishes_closed_unmerged_pr() {
    let argv = vec!["gh".to_string()];
    let pr = parse_pr_state(
        r#"[{"number":20,"state":"CLOSED","merged":false,"statusCheckRollup":[]}]"#,
        &argv,
    )
    .unwrap()
    .unwrap();

    assert_eq!(pr.state, "closed");
    assert!(!pr.merged);
}

#[test]
fn body_reference_fallback_requires_checklist_or_subissue_context() {
    let body = "This ordinary issue mentions #12 in discussion.\n- [ ] #13 child work\nSub-issue: #14\nchild issue #15";

    assert_eq!(parse_body_issue_references(body), vec![13, 14, 15]);
}

#[test]
fn sub_issues_query_uses_only_issue_edge_schema_fields() {
    assert!(SUB_ISSUES_QUERY.contains("subIssues(first:100){edges{node{"));
    assert!(!SUB_ISSUES_QUERY.contains("subIssues(first:100){nodes"));
    assert!(!SUB_ISSUES_QUERY.contains("edges{position"));
    assert!(!SUB_ISSUES_QUERY.contains(" position"));
}

#[test]
fn parse_sub_issue_response_derives_positions_from_connection_order() {
    let json = r#"{
        "data": {"repository": {"issue": {
            "number": 1,
            "subIssues": {
                "edges": [
                    {"node": {"number": 42, "title": "Second", "state": "OPEN"}},
                    {"node": {"number": 7, "title": "First", "state": "OPEN"}}
                ],
                "pageInfo": {"hasNextPage": false}
            }
        }}}
    }"#;

    let children = parse_sub_issue_response(json).unwrap();
    assert_eq!(children.len(), 2);
    assert_eq!(children[0].issue.number, 42);
    assert_eq!(children[0].position, Some(0));
    assert_eq!(children[1].issue.number, 7);
    assert_eq!(children[1].position, Some(1));
}

#[test]
fn multi_state_uses_all() {
    let argv = build_issue_list_argv("o/r", &[], &["open".into(), "closed".into()]);
    let idx = argv.iter().position(|a| a == "--state").unwrap();
    assert_eq!(argv[idx + 1], "all");
}
