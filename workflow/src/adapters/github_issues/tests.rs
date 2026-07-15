use super::subissues::parse_sub_issue_response;
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
    assert_eq!(issues[0].assignees, vec!["acoliver"]);
    assert_eq!(issues[0].milestone.as_deref(), Some("v1.2.0"));
    assert_eq!(issues[1].assignees, Vec::<String>::new());
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
    let runner_true = MockRunner::new(vec![Ok(
        r#"[{"number":99,"state":"OPEN","merged":false}]"#.to_string()
    )]);
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

    let argv = vec!["gh".to_string(), "api".to_string(), "graphql".to_string()];
    let page = parse_first_sub_issue_page(json, &argv).unwrap();

    assert!(page.children.is_empty());
    assert_eq!(page.next_cursor.as_deref(), Some("cursor-1"));
}

#[test]
fn parse_sub_issue_response_skips_missing_edge_nodes() {
    let json = r#"{
        "data": {"repository": {"issue": {
            "number": 1,
            "subIssues": {
                "edges": [
                    {"node": null},
                    {"node": {"number": 2, "title": "Child", "state": "OPEN"}},
                    {}
                ],
                "pageInfo": {"hasNextPage": false, "endCursor": null}
            }
        }}}
    }"#;

    let children = parse_sub_issue_response(json).unwrap();

    assert_eq!(children.len(), 1);
    assert_eq!(children[0].issue.number, 2);
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
    assert_eq!(parent.issue.assignees, vec!["acoliver"]);
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
fn pr_state_for_issue_selects_most_relevant_closing_pr() {
    let issue_refs = r#"{
        "closedByPullRequestsReferences": [
            {"number": 17, "state": "CLOSED", "merged": false, "updatedAt": "2026-01-02T00:00:00Z"},
            {"number": 18, "state": "OPEN", "merged": false, "updatedAt": "2026-01-01T00:00:00Z"}
        ]
    }"#;
    let runner = MockRunner::new(vec![Ok(issue_refs.to_string())]);
    let q = SystemGithubIssueQuery::new(runner);

    let pr = q.pr_state_for_issue("o/r", 7).unwrap().unwrap();

    assert_eq!(pr.number, 18);
    assert_eq!(pr.state, "open");
}

#[test]
fn pr_state_for_issue_falls_back_for_closed_unmerged_references() {
    let issue_refs = r#"{
        "closedByPullRequestsReferences": [
            {"number": 17, "state": "CLOSED", "merged": false, "updatedAt": "2026-01-02T00:00:00Z"}
        ]
    }"#;
    let search_hit =
        r#"[{"number":20,"state":"OPEN","merged":false,"updatedAt":"2026-01-03T00:00:00Z"}]"#;
    let runner = MockRunner::new(vec![Ok(issue_refs.to_string()), Ok(search_hit.to_string())]);
    let q = SystemGithubIssueQuery::new(runner);

    let pr = q.pr_state_for_issue("o/r", 7).unwrap().unwrap();

    assert_eq!(pr.number, 20);
    let calls = q.runner.calls.borrow();
    assert_eq!(calls.len(), 2);
    assert!(calls[1].contains(&"--search".to_string()));
}

#[test]
fn pr_state_for_issue_reports_none_when_issue_has_no_closing_pr() {
    let issue_refs = r#"{"closedByPullRequestsReferences": []}"#;
    let runner = MockRunner::new(vec![Ok(issue_refs.to_string()), Ok("[]".to_string())]);
    let q = SystemGithubIssueQuery::new(runner);

    assert!(q.pr_state_for_issue("o/r", 7).unwrap().is_none());

    let calls = q.runner.calls.borrow();
    assert_eq!(calls.len(), 2);
    assert!(calls[0].contains(&"closedByPullRequestsReferences".to_string()));
    assert!(calls[1].contains(&"--search".to_string()));
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
fn status_check_rollup_ignores_skipped_and_neutral_checks() {
    let argv = vec!["gh".to_string()];
    let pr = parse_pr_state(
        r#"[{"number":20,"state":"OPEN","merged":false,"statusCheckRollup":[{"conclusion":"SKIPPED"},{"conclusion":"NEUTRAL"}]}]"#,
        &argv,
    )
    .unwrap()
    .unwrap();

    assert_eq!(pr.status_check_rollup.as_deref(), None);
}

#[test]
fn status_check_rollup_treats_terminal_failure_conclusions_as_failed() {
    let argv = vec!["gh".to_string()];
    for conclusion in [
        "STARTUP_FAILURE",
        "CANCELLED",
        "STALE",
        "ACTION_REQUIRED",
        "FAILURE",
        "TIMED_OUT",
    ] {
        let pr = parse_pr_state(
            &format!(
                r#"[{{"number":20,"state":"OPEN","merged":false,"statusCheckRollup":[{{"conclusion":"SUCCESS"}},{{"conclusion":"{conclusion}"}}]}}]"#
            ),
            &argv,
        )
        .unwrap()
        .unwrap();

        assert_eq!(pr.status_check_rollup.as_deref(), Some("failed"));
    }
}

#[test]
fn pr_state_for_issue_uses_open_pr_search_when_closing_refs_are_empty() {
    let issue_refs = r#"{"closedByPullRequestsReferences": []}"#;
    let search_hit =
        r#"[{"number":20,"state":"OPEN","merged":false,"updatedAt":"2026-01-01T00:00:00Z"}]"#;
    let runner = MockRunner::new(vec![Ok(issue_refs.to_string()), Ok(search_hit.to_string())]);
    let q = SystemGithubIssueQuery::new(runner);

    let pr = q.pr_state_for_issue("o/r", 7).unwrap().unwrap();

    assert_eq!(pr.number, 20);
    assert_eq!(pr.state, "open");
    let calls = q.runner.calls.borrow();
    assert_eq!(calls.len(), 2);
    assert!(calls[1].contains(&"--search".to_string()));
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
fn fallback_child_lookup_failure_is_reported() {
    let result = parse_body_reference_children("o/r", "- [ ] #13", |_| {
        Err(GithubError::CommandFailed {
            argv: vec!["gh".to_string(), "issue".to_string(), "view".to_string()],
            exit_code: Some(1),
            stderr: "lookup failed".to_string(),
        })
    });

    assert!(result.is_err());
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

#[test]
fn default_merge_method_flag_maps_known_methods() {
    assert_eq!(default_merge_method_flag("MERGE"), Some("--merge"));
    assert_eq!(default_merge_method_flag("merge"), Some("--merge"));
    assert_eq!(default_merge_method_flag("REBASE"), Some("--rebase"));
    assert_eq!(default_merge_method_flag("squash"), Some("--squash"));
    assert_eq!(default_merge_method_flag("unknown"), None);
}

#[test]
fn merge_method_allowed_reads_repository_flags() {
    let value = serde_json::json!({
        "mergeCommitAllowed": true,
        "rebaseMergeAllowed": false,
        "squashMergeAllowed": true,
    });
    assert!(merge_method_allowed(&value, "--merge"));
    assert!(!merge_method_allowed(&value, "--rebase"));
    assert!(merge_method_allowed(&value, "--squash"));
    assert!(!merge_method_allowed(&value, "--bogus"));
    // Missing key defaults to false.
    assert!(!merge_method_allowed(&serde_json::json!({}), "--merge"));
}

#[test]
fn issue_not_found_stderr_recognizes_known_messages() {
    assert!(issue_not_found_stderr(
        "GraphQL: Could not resolve to an Issue with the number of 5"
    ));
    assert!(issue_not_found_stderr(
        "  could not resolve to issue or pull request 9  "
    ));
    assert!(issue_not_found_stderr("no issue found for 12"));
    assert!(!issue_not_found_stderr("some other error"));
}

#[test]
fn normalize_state_lowercases() {
    assert_eq!(normalize_state("OPEN"), "open");
    assert_eq!(normalize_state("Closed"), "closed");
}

#[test]
fn parse_body_issue_references_extracts_checklist_children() {
    let body = "\
Intro paragraph mentioning #999 that should be ignored.
- [ ] #101 first child
- [x] #102 done child
* [ ] #103 another
- [ ] #104 and #105 too many refs ignored
sub-issue: #106
random line #700";
    let refs = parse_body_issue_references(body);
    assert_eq!(refs, vec![101, 102, 103, 106]);
}

#[test]
fn parse_body_issue_references_dedupes() {
    let body = "sub-issue: #55\nchild: #55\n- [ ] #55";
    assert_eq!(parse_body_issue_references(body), vec![55]);
}

#[test]
fn is_subissue_reference_line_detects_prefixes_and_single_ref_checklist() {
    assert!(is_subissue_reference_line("- [ ] #10 title"));
    assert!(!is_subissue_reference_line("- [ ] #10 and #11"));
    assert!(is_subissue_reference_line("sub-issue: #10"));
    assert!(is_subissue_reference_line("Child Issue #10"));
    assert!(is_subissue_reference_line("children: #10"));
    assert!(!is_subissue_reference_line("just some text"));
}

#[test]
fn checklist_item_strips_supported_markers() {
    assert_eq!(checklist_item("- [ ] #10"), Some("#10"));
    assert_eq!(checklist_item("* [X] done"), Some("done"));
    assert_eq!(checklist_item("- plain"), None);
}

#[test]
fn issue_reference_tokens_counts_hash_tokens() {
    assert_eq!(issue_reference_tokens("#10 title"), 1);
    assert_eq!(issue_reference_tokens("#10 and #11"), 2);
    assert_eq!(issue_reference_tokens("no refs here"), 0);
}

#[test]
fn parse_issue_list_maps_state_and_labels() {
    let json = r#"[
        {"number":7,"title":"T","state":"OPEN","labels":[{"name":"bug"}],
         "assignees":[{"login":"me"}],"milestone":{"title":"m1"}}
    ]"#;
    let issues = parse_issue_list(json).unwrap();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].state, "open");
    assert_eq!(issues[0].labels, vec!["bug".to_string()]);
    assert_eq!(issues[0].assignees, vec!["me"]);
    assert_eq!(issues[0].milestone.as_deref(), Some("m1"));
}

#[test]
fn parse_issue_list_rejects_malformed_json() {
    let err = parse_issue_list("not json").unwrap_err();
    match err {
        GithubError::CommandFailed { stderr, .. } => {
            assert!(stderr.contains("failed to parse gh issue list JSON"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn build_issue_list_argv_single_open_state_uses_open() {
    let argv = build_issue_list_argv("o/r", &[], &["open".into()]);
    let idx = argv.iter().position(|a| a == "--state").unwrap();
    assert_eq!(argv[idx + 1], "open");
    assert!(argv.contains(&"--repo".to_string()));
    assert!(argv.contains(&"o/r".to_string()));
}

#[test]
fn build_issue_list_argv_includes_label_filters() {
    let argv = build_issue_list_argv("o/r", &["ready".into()], &["open".into()]);
    let idx = argv.iter().position(|a| a == "--label").unwrap();
    assert_eq!(argv[idx + 1], "ready");
}
