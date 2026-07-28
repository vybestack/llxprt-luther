use super::*;

fn valid_pending_action_value() -> Value {
    json!({
        "action_id": "comment_invalid:thread:PRRT_1:item-1:none",
        "action_kind": "comment_invalid",
        "item_id": "item-1",
        "stable_marker_key": "thread:PRRT_1",
        "source_head_sha": "aaa",
        "remediation_input_head_sha": "aaa",
        "remediation_output_head": "none",
        "remediation_output_head_sha": null,
        "body_hash": stable_hash("same review feedback"),
        "status": "pending",
        "resolution_required": false,
        "response_text": "This feedback is not valid.",
        "thread_id": "PRRT_1",
        "comment_database_id": 7001
    })
}

#[test]
fn legacy_pending_fixture_is_normalized_when_carried_to_a_resumed_head() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../../../tests/fixtures/github_pr/current/pending-feedback-marker-actions.json"
    ))
    .expect("legacy fixture");
    let source_binding: PrFollowupBinding =
        serde_json::from_value(fixture.clone()).expect("fixture binding");
    let mut resumed_binding = source_binding.clone();
    resumed_binding.head_sha = "2222222222222222222222222222222222222222".to_string();
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    store
        .write_json_artifact(JsonArtifactWriteRequest::new(
            ArtifactWriteContext::new(
                &source_binding,
                PENDING_MARKER_ACTIONS_FAMILY,
                "build_remediation_plan",
                7,
                &SystemClockSleeper,
            ),
            &fixture,
            None,
        ))
        .expect("write legacy pending fixture");

    let carried = read_pending_marker_artifact(&store, &resumed_binding)
        .expect("carry and normalize legacy fixture");
    assert_eq!(
        carried["pending_actions"][0]["remediation_output_head"],
        json!("2222222222222222222222222222222222222222")
    );
    let actions = pending_marker_actions_from_artifact(&store, &resumed_binding, &carried)
        .expect("legacy action remains resumable");
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].action_id, "marker-action-1");
    assert_eq!(actions[0].body_hash, "sha256:body");
}

#[test]
fn legacy_action_id_shape_remains_backward_compatible() {
    let action = pending_marker_action_from_value(valid_pending_action_value())
        .expect("legacy action ID must remain accepted");
    assert_eq!(
        action.action_id,
        "comment_invalid:thread:PRRT_1:item-1:none"
    );
}

#[test]
fn generated_non_fixed_actions_are_unique_across_source_heads() {
    let feedback = json!({
        "thread_id": "PRRT_1",
        "comment_database_id": 7001
    });
    for decision in ["invalid", "out_of_scope"] {
        let actions = ["aaa", "bbb"]
            .into_iter()
            .map(|head| {
                let evaluation = json!({
                    "item_id": "item-1",
                    "stable_marker_key": "thread:PRRT_1",
                    "body_hash": stable_hash("same review feedback"),
                    "head_sha": head,
                    "decision": decision,
                    "reason": decision,
                    "response_text": format!("Recorded {decision} feedback.")
                });
                let value = current_evaluation_marker_action(
                    &issue132_binding(head),
                    &evaluation,
                    Some(&feedback),
                    &json!({}),
                    &SystemClockSleeper,
                )
                .expect("generated action");
                pending_marker_action_from_value(value).expect("valid generated action")
            })
            .collect::<Vec<_>>();

        assert_ne!(actions[0].action_id, actions[1].action_id);
        assert!(actions[0].action_id.contains(":aaa:none"));
        assert!(actions[1].action_id.contains(":bbb:none"));
        assert!(
            validate_marker_actions_before_mutation(&actions).is_none(),
            "same item on distinct heads must process as distinct cycles"
        );
    }
}

#[test]
fn pending_action_routing_error_reports_expected_and_actual_heads() {
    let mut action = valid_pending_action_value();
    action["remediation_output_head"] = json!("bbb");
    let error = pending_marker_action_from_value(action)
        .expect_err("mismatched output routing must fail closed")
        .to_string();

    assert!(error.contains("action_kind=\"comment_invalid\""));
    assert!(error.contains("expected remediation_output_head_sha=\"bbb\""));
    assert!(error.contains("actual remediation_output_head_sha=None"));
}

#[test]
fn pending_action_parser_rejects_malformed_sha256_hashes() {
    for body_hash in ["sha256:", "sha256:bad value", "sha256:bad/value"] {
        let mut action = valid_pending_action_value();
        action["body_hash"] = json!(body_hash);
        assert!(
            pending_marker_action_from_value(action).is_err(),
            "malformed legacy hash must fail closed: {body_hash}"
        );
    }
}

#[test]
fn pending_action_parser_rejects_malformed_identity_status_hash_and_routing() {
    let mut malformed = vec![Value::String("not-an-object".to_string())];
    for (field, replacement) in [
        ("action_kind", json!("unknown_action")),
        ("item_id", json!("")),
        ("stable_marker_key", json!(" ")),
        ("source_head_sha", Value::Null),
        ("body_hash", json!("fnv64:not-canonical")),
        ("status", json!("unknown")),
        ("resolution_required", json!("true")),
        ("thread_id", json!(7)),
        ("comment_database_id", json!(0)),
        ("response_text", json!({"text": "looks present"})),
        ("remediation_output_head", json!("bbb")),
        ("remediation_output_head_sha", json!("bbb")),
    ] {
        let mut action = valid_pending_action_value();
        action[field] = replacement;
        malformed.push(action);
    }

    for action in malformed {
        assert!(
            pending_marker_action_from_value(action.clone()).is_err(),
            "malformed action must fail closed: {action}"
        );
    }
}

fn assert_pending_actions_rejected_without_github_calls(actions: Vec<Value>) {
    let temp = tempfile::tempdir().expect("tempdir");
    let binding = issue132_binding("aaa");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    store
        .write_json_artifact(JsonArtifactWriteRequest::new(
            ArtifactWriteContext::new(
                &binding,
                PENDING_MARKER_ACTIONS_FAMILY,
                "validate_remediation_result",
                11,
                &SystemClockSleeper,
            ),
            &json!({"pending_actions": actions}),
            None,
        ))
        .expect("write pending artifact");
    let mut context = StepContext::new(temp.path().to_path_buf(), binding.run_id.clone());
    let params = json!({
        "artifact_root": temp.path().display().to_string(),
        "repository_owner": binding.repository_owner,
        "repository_name": binding.repository_name,
        "pr_number": binding.pr_number,
        "head_ref": binding.head_ref,
        "head_sha": binding.head_sha,
        "base_ref": binding.base_ref,
        "base_sha": binding.base_sha
    });
    let runner = RecordingMarkerRunner::default();

    let result = mark_coderabbit_feedback(&mut context, &params, &runner, &SystemClockSleeper);

    assert!(
        matches!(result, Err(_) | Ok(StepOutcome::Fatal)),
        "invalid pending actions must stop the marker step: {result:?}"
    );
    assert!(
        runner.calls.lock().expect("calls").is_empty(),
        "validation must fail closed before the remote scan or any GitHub mutation"
    );
}

#[test]
fn contradictory_resolution_required_values_fail_before_remote_scan() {
    let mut fixed_without_resolution = valid_pending_action_value();
    fixed_without_resolution["action_id"] = json!("comment_fixed:item-1:aaa:bbb");
    fixed_without_resolution["action_kind"] = json!("comment_fixed");
    fixed_without_resolution["remediation_output_head"] = json!("bbb");
    fixed_without_resolution["remediation_output_head_sha"] = json!("bbb");
    assert_pending_actions_rejected_without_github_calls(vec![fixed_without_resolution]);

    let mut non_fixed_with_resolution = valid_pending_action_value();
    non_fixed_with_resolution["resolution_required"] = json!(true);
    assert_pending_actions_rejected_without_github_calls(vec![non_fixed_with_resolution]);
}

#[test]
fn missing_null_and_empty_action_ids_fail_before_remote_scan() {
    let mut missing = valid_pending_action_value();
    missing
        .as_object_mut()
        .expect("pending action object")
        .remove("action_id");
    assert_pending_actions_rejected_without_github_calls(vec![missing]);

    for invalid_action_id in [Value::Null, json!(""), json!("   ")] {
        let mut action = valid_pending_action_value();
        action["action_id"] = invalid_action_id;
        assert_pending_actions_rejected_without_github_calls(vec![action]);
    }
}

#[test]
fn duplicate_action_id_with_distinct_cycle_provenance_fails_before_remote_scan() {
    let first = valid_pending_action_value();
    let mut second = valid_pending_action_value();
    second["item_id"] = json!("item-2");
    second["stable_marker_key"] = json!("thread:PRRT_2");
    second["source_head_sha"] = json!("bbb");
    second["remediation_input_head_sha"] = json!("bbb");
    second["body_hash"] = json!(stable_hash("different review feedback"));
    second["thread_id"] = json!("PRRT_2");
    second["comment_database_id"] = json!(7002);

    assert_pending_actions_rejected_without_github_calls(vec![first, second]);
}

#[test]
fn duplicate_policy_skip_actions_remain_exempt_from_mutation_uniqueness() {
    let mut first = valid_pending_action_value();
    first["action_kind"] = json!("skip_needs_user_judgment");
    let first = pending_marker_action_from_value(first).expect("first skip action");
    let mut second = first.clone();
    second.item_id = "item-2".to_string();

    assert!(
        validate_marker_actions_before_mutation(&[first, second]).is_none(),
        "policy skips do not mutate GitHub and retain their legacy duplicate exemption"
    );
}

#[test]
fn malformed_pending_action_with_response_text_cannot_mutate_github() {
    let temp = tempfile::tempdir().expect("tempdir");
    let binding = issue132_binding("aaa");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let mut malformed = valid_pending_action_value();
    malformed["item_id"] = json!({"looks": "present"});
    store
        .write_json_artifact(JsonArtifactWriteRequest::new(
            ArtifactWriteContext::new(
                &binding,
                PENDING_MARKER_ACTIONS_FAMILY,
                "validate_remediation_result",
                11,
                &SystemClockSleeper,
            ),
            &json!({"pending_actions": [malformed]}),
            None,
        ))
        .expect("write pending artifact");
    let mut context = StepContext::new(temp.path().to_path_buf(), binding.run_id.clone());
    let params = json!({
        "artifact_root": temp.path().display().to_string(),
        "repository_owner": binding.repository_owner,
        "repository_name": binding.repository_name,
        "pr_number": binding.pr_number,
        "head_ref": binding.head_ref,
        "head_sha": binding.head_sha,
        "base_ref": binding.base_ref,
        "base_sha": binding.base_sha
    });
    let runner = RecordingMarkerRunner::default();

    let result = mark_coderabbit_feedback(&mut context, &params, &runner, &SystemClockSleeper);

    assert!(
        result.is_err(),
        "malformed action must stop the marker step"
    );
    assert!(
        runner.calls.lock().expect("calls").is_empty(),
        "no GitHub command, including mutation, may run before all actions validate"
    );
}
