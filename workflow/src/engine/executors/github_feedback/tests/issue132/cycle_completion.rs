use super::*;

fn exact_cycle_action(
    record: &crate::engine::executors::pr_followup_artifacts::ArtifactWriteRecord,
    input_head: &str,
    output_head: &str,
    plan_sequence: u64,
) -> PendingMarkerAction {
    pending_marker_action_from_value(json!({
        "action_id": format!("comment_fixed:item-1:{input_head}:{output_head}"),
        "action_kind": "comment_fixed",
        "item_id": "item-1",
        "stable_marker_key": "thread:PRRT_1",
        "source_head_sha": input_head,
        "remediation_input_head_sha": input_head,
        "remediation_output_head": output_head,
        "remediation_output_head_sha": output_head,
        "body_hash": stable_hash("same review feedback"),
        "status": "pending",
        "resolution_required": true,
        "reason": "fixed",
        "response_text": format!("Fixed during remediation {input_head}->{output_head}."),
        "thread_id": "PRRT_1",
        "comment_database_id": 7001,
        "remediation_result_status": "fixed",
        "remediation_result_evidence": {"commands": []},
        "remediation_result_artifact_sequence": record.sequence.artifact_sequence,
        "remediation_result_write_sequence": record.sequence.write_sequence,
        "remediation_result_producer_step_id": record.sequence.producer_step_id,
        "plan_artifact_sequence": plan_sequence,
        "remediation_attempt_index": 0
    }))
    .expect("valid exact-cycle action")
}

fn assert_two_cycle_marker_completion(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    runner: &RecordingMarkerRunner,
    first_artifact_sequence: u64,
    second_artifact_sequence: u64,
) {
    let calls = runner.calls.lock().expect("calls");
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.iter().any(|arg| arg.contains("/replies")))
            .count(),
        2,
        "both remediation cycles must post an in-thread reply"
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.iter().any(|arg| arg == "graphql"))
            .count(),
        2,
        "both remediation cycles must resolve the review thread"
    );
    drop(calls);

    let report = store
        .read_current_json(binding, MARKER_ARTIFACT_FAMILY)
        .expect("read marker report");
    assert_eq!(
        report.get("marker_state").and_then(Value::as_str),
        Some("complete")
    );
    assert_eq!(report["posted_comments"].as_array().map(Vec::len), Some(2));
    assert_eq!(report["resolved_threads"].as_array().map(Vec::len), Some(2));
    let audit = report["action_audit"].as_array().expect("action audit");
    assert_eq!(audit.len(), 2);
    assert_eq!(audit[0]["source_head_sha"], json!("aaa"));
    assert_eq!(audit[0]["remediation_output_head"], json!("bbb"));
    assert_eq!(
        audit[0]["remediation_result_artifact_sequence"],
        json!(first_artifact_sequence)
    );
    assert_eq!(audit[1]["source_head_sha"], json!("bbb"));
    assert_eq!(audit[1]["remediation_output_head"], json!("ccc"));
    assert_eq!(
        audit[1]["remediation_result_artifact_sequence"],
        json!(second_artifact_sequence)
    );
}

struct TwoCycleMarkerFixture {
    _temp: tempfile::TempDir,
    store: PrFollowupArtifactStore,
    binding: PrFollowupBinding,
    actions: Vec<PendingMarkerAction>,
    first_artifact_sequence: u64,
    second_artifact_sequence: u64,
}

fn two_cycle_marker_fixture() -> TwoCycleMarkerFixture {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let first = store
        .write_json_artifact(JsonArtifactWriteRequest::new(
            ArtifactWriteContext::new(
                &issue132_binding("aaa"),
                "pr-remediation-result",
                "pr_remediation_result",
                9,
                &SystemClockSleeper,
            ),
            &issue132_result_payload_for("aaa", "bbb", 1),
            None,
        ))
        .expect("write A->B evidence");
    let second = store
        .write_json_artifact(JsonArtifactWriteRequest::new(
            ArtifactWriteContext::new(
                &issue132_binding("bbb"),
                "pr-remediation-result",
                "pr_remediation_result",
                9,
                &SystemClockSleeper,
            ),
            &issue132_result_payload_for("bbb", "ccc", 2),
            None,
        ))
        .expect("write B->C evidence");
    let actions = vec![
        exact_cycle_action(&first, "aaa", "bbb", 1),
        exact_cycle_action(&second, "bbb", "ccc", 2),
    ];
    TwoCycleMarkerFixture {
        _temp: temp,
        store,
        binding: issue132_binding("ccc"),
        actions,
        first_artifact_sequence: first.sequence.artifact_sequence,
        second_artifact_sequence: second.sequence.artifact_sequence,
    }
}

fn execute_two_cycle_marker_fixture(fixture: &TwoCycleMarkerFixture) -> RecordingMarkerRunner {
    let runner = RecordingMarkerRunner::default();
    let local_completed = BTreeSet::new();
    let remote_completed = BTreeSet::new();
    let params = json!({});
    let processor = MarkerActionProcessor {
        binding: &fixture.binding,
        store: &fixture.store,
        step_id: "mark_coderabbit_feedback",
        step_order: 15,
        runner: &runner,
        clock: &SystemClockSleeper,
        local_completed: &local_completed,
        remote_completed: &remote_completed,
        params: &params,
    };
    let pending_artifact = json!({
        "pending_actions": fixture.actions.iter().map(|action| action.value.clone()).collect::<Vec<_>>(),
        "marker_policy": {}
    });
    let outcomes = process_pending_marker_actions(&processor, fixture.actions.clone())
        .expect("execute both cycles");
    assert_eq!(outcomes.len(), 2);
    assert!(outcomes.iter().all(|outcome| outcome.status == "completed"));
    write_updated_pending_actions(
        &fixture.store,
        &fixture.binding,
        "mark_coderabbit_feedback",
        15,
        &pending_artifact,
        &outcomes,
        &SystemClockSleeper,
    )
    .expect("complete pending actions");
    assert_eq!(
        write_marker_report(
            &fixture.store,
            &fixture.binding,
            "mark_coderabbit_feedback",
            15,
            &outcomes,
            Vec::new(),
            &SystemClockSleeper,
        )
        .expect("write complete report"),
        StepOutcome::Success
    );
    runner
}

#[test]
fn same_item_across_a_b_and_b_c_cycles_replies_resolves_and_reports_both() {
    let fixture = two_cycle_marker_fixture();
    assert!(
        validate_marker_actions_before_mutation(&fixture.actions).is_none(),
        "the same item is valid when each action has distinct cycle provenance"
    );
    let runner = execute_two_cycle_marker_fixture(&fixture);
    assert_two_cycle_marker_completion(
        &fixture.store,
        &fixture.binding,
        &runner,
        fixture.first_artifact_sequence,
        fixture.second_artifact_sequence,
    );
}
