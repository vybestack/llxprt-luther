use super::*;

fn issue132_binding(head_sha: &str) -> PrFollowupBinding {
    PrFollowupBinding {
        schema_version: PR_FOLLOWUP_SCHEMA_VERSION,
        run_id: "issue-132-run".to_string(),
        repository_owner: "example".to_string(),
        repository_name: "workflow".to_string(),
        pr_number: 132,
        head_ref: "issue-132".to_string(),
        head_sha: head_sha.to_string(),
        base_ref: "main".to_string(),
        base_sha: Some("base".to_string()),
    }
}

fn issue132_result_payload() -> Value {
    issue132_result_payload_for("aaa", "bbb", 1)
}

fn issue132_result_payload_for(input_head: &str, output_head: &str, plan_sequence: u64) -> Value {
    json!({
        "validation_state": "valid",
        "overall_status": "success",
        "input_head_sha": input_head,
        "output_head_sha": output_head,
        "plan_artifact_sequence": plan_sequence,
        "results": [{
            "source_type": "coderabbit_feedback",
            "source_id": "item-1",
            "stable_marker_key": "thread:PRRT_1",
            "body_hash": stable_hash("same review feedback"),
            "input_head_sha": input_head,
            "output_head_sha": output_head,
            "status": "fixed",
            "evidence": {"commands": []}
        }],
        "retry_scope": {
            "scope_kind": "remediation_result_validation",
            "run_id": "issue-132-run",
            "repository_owner": "example",
            "repository_name": "workflow",
            "pr_number": 132,
            "input_head_sha": input_head,
            "output_head_sha": output_head,
            "plan_artifact_sequence": plan_sequence,
            "remediation_attempt_index": 0,
            "max_remediation_attempts": 3,
            "validation_retry_index": 0,
            "max_validation_retries": 2,
            "stale_artifact_retry_index": 0,
            "max_stale_artifact_retries": 2
        }
    })
}

fn issue132_legacy_action() -> PendingMarkerAction {
    pending_marker_action_from_value(json!({
        "action_id": "comment_fixed:thread:PRRT_1:item-1:bbb",
        "action_kind": "comment_fixed",
        "item_id": "item-1",
        "stable_marker_key": "thread:PRRT_1",
        "source_head_sha": "aaa",
        "remediation_input_head_sha": "aaa",
        "remediation_output_head": "bbb",
        "remediation_output_head_sha": "bbb",
        "body_hash": stable_hash("same review feedback"),
        "remediation_result_evidence": {"commands": []},
        "status": "pending",
        "resolution_required": true,
        "response_text": "Fixed in this remediation cycle.",
        "thread_id": "PRRT_1",
        "comment_database_id": 7001
    }))
    .expect("legacy pending action should deserialize from fixture JSON")
}

#[derive(Default)]
struct RecordingMarkerRunner {
    calls: Mutex<Vec<Vec<String>>>,
}

impl GithubPrCommandRunner for RecordingMarkerRunner {
    fn run_github_command(&self, argv: &[String]) -> Result<String, EngineError> {
        self.calls.lock().expect("calls").push(argv.to_vec());
        let command = argv.iter().map(String::as_str).collect::<Vec<_>>();
        if matches!(
            command.as_slice(),
            [
                "gh",
                "api",
                "/repos/example/workflow/pulls/132/comments/7001/replies",
                "--method",
                "POST",
                "--field",
                body
            ] if body.starts_with("body=@")
        ) {
            return Ok(json!({
                "id": 9001,
                "html_url": "https://github.test/reply/9001",
                "in_reply_to_id": 7001
            })
            .to_string());
        }
        if command.len() == 11
            && command[..4] == ["gh", "api", "graphql", "-f"]
            && command[4] == format!("query={GRAPHQL_REVIEW_THREADS_QUERY}")
            && command[5..]
                == [
                    "-f",
                    "owner=example",
                    "-f",
                    "name=workflow",
                    "-F",
                    "number=132",
                ]
        {
            return Ok(json!({
                "data": {
                    "repository": {
                        "pullRequest": {
                            "reviewThreads": {"nodes": []}
                        }
                    }
                }
            })
            .to_string());
        }
        if command
            == [
                "gh",
                "api",
                "graphql",
                "-f",
                format!("query={RESOLVE_REVIEW_THREAD_MUTATION}").as_str(),
                "-f",
                "threadId=PRRT_1",
            ]
        {
            return Ok(json!({
                "data": {
                    "resolveReviewThread": {
                        "thread": {"id": "PRRT_1", "isResolved": true}
                    }
                }
            })
            .to_string());
        }
        Err(github_feedback_error(format!(
            "unexpected GitHub command: {argv:?}"
        )))
    }
}

mod cycle_completion;
mod legacy_evidence;
mod validation;
