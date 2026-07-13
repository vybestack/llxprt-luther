use super::*;

#[test]
fn legacy_carried_action_recovers_unique_validated_history() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    store
        .write_json_artifact(
            &issue132_binding("aaa"),
            "pr-remediation-result",
            "pr_remediation_result",
            9,
            &issue132_result_payload(),
            None,
            &SystemClockSleeper,
        )
        .expect("write evidence");

    let current = issue132_binding("bbb");
    let ledger = store
        .validate_history_ledger(&current)
        .expect("validated history ledger");
    let action = issue132_legacy_action();
    let outcome = validate_marker_action_evidence(
        &current,
        &store,
        &action,
        "comment-key".to_string(),
        "resolution-key".to_string(),
        Some(&ledger),
        &SystemClockSleeper,
    )
    .expect("legacy recovery must succeed");
    assert!(
        outcome.is_none(),
        "unique validated evidence authorizes the action"
    );
}

#[test]
fn legacy_carried_action_fails_when_history_is_ambiguous() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    for _ in 0..2 {
        store
            .write_json_artifact(
                &issue132_binding("aaa"),
                "pr-remediation-result",
                "pr_remediation_result",
                9,
                &issue132_result_payload(),
                None,
                &SystemClockSleeper,
            )
            .expect("write evidence");
    }

    let current = issue132_binding("bbb");
    let ledger = store
        .validate_history_ledger(&current)
        .expect("validated history ledger");
    let action = issue132_legacy_action();
    let result = validate_marker_action_evidence(
        &current,
        &store,
        &action,
        "comment-key".to_string(),
        "resolution-key".to_string(),
        Some(&ledger),
        &SystemClockSleeper,
    );
    assert!(
        result.is_err(),
        "ambiguous legacy evidence must fail closed"
    );
    assert!(result.unwrap_err().to_string().contains("ambiguous"));
}

#[test]
fn fixed_action_rejects_incomplete_retry_scope_and_producer_provenance() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let record = store
        .write_json_artifact(
            &issue132_binding("aaa"),
            "pr-remediation-result",
            "pr_remediation_result",
            9,
            &issue132_result_payload(),
            None,
            &SystemClockSleeper,
        )
        .expect("write evidence");
    let mut result = store
        .read_history_evidence_by_sequence(
            &issue132_binding("bbb"),
            "pr-remediation-result",
            "aaa",
            Some("bbb"),
            &record.sequence,
        )
        .expect("read evidence")
        .expect("evidence exists");
    let mut action_value = issue132_legacy_action().value;
    action_value["remediation_result_artifact_sequence"] = json!(record.sequence.artifact_sequence);
    action_value["remediation_result_write_sequence"] = json!(record.sequence.write_sequence);
    action_value["remediation_result_producer_step_id"] = json!(record.sequence.producer_step_id);
    action_value["plan_artifact_sequence"] = json!(1);
    action_value["remediation_attempt_index"] = json!(0);
    let action = pending_marker_action_from_value(action_value).expect("fixed action");
    let evidence_ref = carried_evidence_ref(&action)
        .expect("reference parse")
        .expect("complete reference");

    result["retry_scope"]
        .as_object_mut()
        .expect("retry scope")
        .remove("max_validation_retries");
    assert!(!marker_action_has_validator_success(
        &issue132_binding("bbb"),
        &action,
        &result,
        Some(&evidence_ref),
    ));

    result["retry_scope"]["max_validation_retries"] = json!(2);
    result["producer_step_id"] = json!("wrong_producer");
    assert!(!marker_action_has_validator_success(
        &issue132_binding("bbb"),
        &action,
        &result,
        Some(&evidence_ref),
    ));

    result["producer_step_id"] = json!(record.sequence.producer_step_id);
    for (index, max) in [
        ("remediation_attempt_index", "max_remediation_attempts"),
        ("validation_retry_index", "max_validation_retries"),
        ("stale_artifact_retry_index", "max_stale_artifact_retries"),
    ] {
        let mut out_of_bounds = result.clone();
        out_of_bounds["retry_scope"][index] = out_of_bounds["retry_scope"][max].clone();
        assert!(
            !marker_action_has_validator_success(
                &issue132_binding("bbb"),
                &action,
                &out_of_bounds,
                Some(&evidence_ref),
            ),
            "{index} must be strictly less than {max}"
        );
    }
}
