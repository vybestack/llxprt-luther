use super::*;

const VALIDATED_HISTORY_LEDGER_REQUIRED: &str =
    "cross-head evidence lookup requires a validated history ledger";

pub(crate) fn carried_evidence_ref(
    action: &PendingMarkerAction,
) -> Result<Option<FixedActionEvidenceRef>, EngineError> {
    if action.body_hash.is_empty() {
        return Err(github_feedback_error(
            "fixed marker action carries empty body_hash",
        ));
    }
    if action
        .value
        .get("remediation_result_evidence")
        .is_none_or(|evidence| evidence.is_null())
    {
        return Ok(None);
    }
    let tuple_fields = [
        "remediation_result_artifact_sequence",
        "remediation_result_write_sequence",
        "remediation_result_producer_step_id",
    ];
    let present = tuple_fields
        .iter()
        .filter(|field| {
            action
                .value
                .get(**field)
                .is_some_and(|value| !value.is_null())
        })
        .count();
    if present == 0 {
        return Ok(None);
    }
    if present != tuple_fields.len() {
        return Err(github_feedback_error(
            "fixed marker action carries a partial remediation-result evidence tuple",
        ));
    }
    FixedActionEvidenceRef::from_action_value(&action.value)
        .map(Some)
        .map_err(github_feedback_error)
}

pub(crate) fn validate_marker_action_evidence(
    binding: &PrFollowupBinding,
    store: &PrFollowupArtifactStore,
    action: &PendingMarkerAction,
    comment_key: String,
    resolution_key: String,
    history_ledger: Option<&ValidatedHistoryLedger>,
    clock: &dyn ClockSleeper,
) -> Result<Option<MarkerActionOutcome>, EngineError> {
    if !matches!(
        action.action_kind.as_str(),
        "comment_fixed" | "resolve_thread"
    ) {
        return Ok(None);
    }
    let evidence_ref = carried_evidence_ref(action).map_err(|err| {
        github_feedback_error(format!(
            "invalid evidence for action {} ({}, item {}): {err}",
            action.action_id, action.stable_marker_key, action.item_id
        ))
    })?;
    let result = read_marker_action_evidence(
        binding,
        store,
        action,
        evidence_ref.as_ref(),
        history_ledger,
    )?;
    if result.as_ref().is_some_and(|payload| {
        marker_action_has_validator_success(binding, action, payload, evidence_ref.as_ref())
    }) {
        return Ok(None);
    }
    Ok(Some(missing_validator_evidence_outcome(
        action,
        comment_key,
        resolution_key,
        clock,
    )))
}

fn read_marker_action_evidence(
    binding: &PrFollowupBinding,
    store: &PrFollowupArtifactStore,
    action: &PendingMarkerAction,
    evidence_ref: Option<&FixedActionEvidenceRef>,
    history_ledger: Option<&ValidatedHistoryLedger>,
) -> Result<Option<Value>, EngineError> {
    // Current-head actions use the canonical envelope. Cross-head actions use
    // exact immutable sequence identity, while legacy actions may recover only
    // a unique, fully validated source/output-head candidate.
    if action.source_head_sha == binding.head_sha {
        store.read_optional_current_json_for_head(binding, "pr-remediation-result")
    } else if let Some(reference) = evidence_ref {
        let ledger = history_ledger
            .ok_or_else(|| github_feedback_error(VALIDATED_HISTORY_LEDGER_REQUIRED))?;
        store.read_validated_history_evidence_by_sequence(
            binding,
            ledger,
            "pr-remediation-result",
            &reference.source_head_sha,
            Some(reference.output_head_sha.as_str()),
            &reference.result_sequence,
        )
    } else {
        let ledger = history_ledger
            .ok_or_else(|| github_feedback_error(VALIDATED_HISTORY_LEDGER_REQUIRED))?;
        store.read_validated_history_json_by_head(
            binding,
            ledger,
            "pr-remediation-result",
            &action.source_head_sha,
            Some(action.remediation_output_head.as_str()),
        )
    }
}

fn missing_validator_evidence_outcome(
    action: &PendingMarkerAction,
    comment_key: String,
    resolution_key: String,
    clock: &dyn ClockSleeper,
) -> MarkerActionOutcome {
    let failed = json!({
        "idempotency_key": comment_key,
        "resolution_idempotency_key": resolution_key,
        "action_id": action.value.get("action_id").cloned().unwrap_or(Value::Null),
        "reason": "missing_validator_success_evidence",
        "failure_state": "failed_fatal"
    });
    let mut updated_action = action.value.clone();
    if let Some(object) = updated_action.as_object_mut() {
        object.insert("status".to_string(), json!("failed"));
        object.insert("comment_idempotency_key".to_string(), json!(comment_key));
        object.insert(
            "resolution_idempotency_key".to_string(),
            json!(resolution_key),
        );
        object.insert(
            "failure_reason".to_string(),
            json!("missing_validator_success_evidence"),
        );
        object.insert("updated_at".to_string(), json!(clock.now_rfc3339()));
    }
    let audit = marker_action_audit(
        action,
        "failed",
        &comment_key,
        None,
        &ResolveAudit {
            resolve_attempted: false,
            resolve_succeeded: false,
            resolve_error: None,
            final_thread_resolved_state: None,
        },
    );
    MarkerActionOutcome {
        action: action.clone(),
        status: "failed".to_string(),
        comment_key,
        resolution_key,
        posted_comment: None,
        resolved_thread: None,
        skipped: Vec::new(),
        partial: None,
        retryable: None,
        failed: Some(failed),
        audit,
        updated_action,
    }
}

struct ValidatorResultHeads<'a> {
    input: &'a str,
    output: Option<&'a str>,
}

pub(crate) fn marker_action_has_validator_success(
    binding: &PrFollowupBinding,
    action: &PendingMarkerAction,
    result: &Value,
    evidence_ref: Option<&FixedActionEvidenceRef>,
) -> bool {
    let Some(heads) = validator_result_heads(binding, action, result) else {
        return false;
    };
    if !validator_provenance_matches(binding, result, &heads, evidence_ref) {
        return false;
    }
    result
        .get("results")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                validator_result_item_matches(action, item, &heads, evidence_ref.is_some())
            })
        })
}

fn validator_result_heads<'a>(
    binding: &'a PrFollowupBinding,
    action: &PendingMarkerAction,
    result: &'a Value,
) -> Option<ValidatorResultHeads<'a>> {
    if result.get("validation_state").and_then(Value::as_str) != Some("valid")
        || action
            .value
            .get("remediation_result_evidence")
            .is_none_or(|evidence| evidence.is_null())
    {
        return None;
    }
    let input = result
        .get("input_head_sha")
        .and_then(Value::as_str)
        .unwrap_or(&binding.head_sha);
    let output = result.get("output_head_sha").and_then(Value::as_str);
    if input != action.source_head_sha
        || (action.remediation_output_head != NO_REMEDIATION_OUTPUT_HEAD
            && output != Some(action.remediation_output_head.as_str()))
    {
        return None;
    }
    Some(ValidatorResultHeads { input, output })
}

fn validator_provenance_matches(
    binding: &PrFollowupBinding,
    result: &Value,
    heads: &ValidatorResultHeads<'_>,
    evidence_ref: Option<&FixedActionEvidenceRef>,
) -> bool {
    let Some(reference) = evidence_ref else {
        return result
            .get("retry_scope")
            .and_then(|scope| scope.get("run_id"))
            .and_then(Value::as_str)
            == Some(binding.run_id.as_str());
    };
    let Some(plan_sequence) = result.get("plan_artifact_sequence").and_then(Value::as_u64) else {
        return false;
    };
    let Some(result_producer) = result.get("producer_step_id").and_then(Value::as_str) else {
        return false;
    };
    !result_producer.is_empty()
        && result.get("artifact_sequence").and_then(Value::as_u64)
            == Some(reference.result_sequence.artifact_sequence)
        && result.get("write_sequence").and_then(Value::as_u64)
            == Some(reference.result_sequence.write_sequence)
        && result_producer == reference.result_sequence.producer_step_id
        && plan_sequence == reference.plan_artifact_sequence
        && retry_scope_matches_fixed_action(
            binding,
            result,
            plan_sequence,
            Some(heads.input),
            heads.output,
            Some(reference),
        )
}

fn validator_result_item_matches(
    action: &PendingMarkerAction,
    item: &Value,
    heads: &ValidatorResultHeads<'_>,
    exact_evidence: bool,
) -> bool {
    item.get("source_type").and_then(Value::as_str) == Some("coderabbit_feedback")
        && item.get("source_id").and_then(Value::as_str) == Some(action.item_id.as_str())
        && matches!(
            item.get("status").and_then(Value::as_str),
            Some("fixed" | "changed" | "already_satisfied" | "not_reproduced")
        )
        && item.get("input_head_sha").and_then(Value::as_str) == Some(heads.input)
        && (!exact_evidence || item.get("output_head_sha").and_then(Value::as_str) == heads.output)
        && item.get("body_hash").and_then(Value::as_str) == Some(action.body_hash.as_str())
        && item.get("stable_marker_key").and_then(Value::as_str)
            == Some(action.stable_marker_key.as_str())
        && validator_item_evidence_matches(action, item, exact_evidence)
}

fn validator_item_evidence_matches(
    action: &PendingMarkerAction,
    item: &Value,
    exact_evidence: bool,
) -> bool {
    if exact_evidence {
        item.get("evidence") == action.value.get("remediation_result_evidence")
    } else {
        item.get("evidence")
            .is_some_and(|evidence| !evidence.is_null())
    }
}

fn retry_scope_matches_fixed_action(
    binding: &PrFollowupBinding,
    result: &Value,
    plan_sequence: u64,
    input_head: Option<&str>,
    output_head: Option<&str>,
    evidence_ref: Option<&FixedActionEvidenceRef>,
) -> bool {
    let Some(scope) = result.get("retry_scope").and_then(Value::as_object) else {
        return false;
    };
    let required_numbers = [
        "remediation_attempt_index",
        "max_remediation_attempts",
        "validation_retry_index",
        "max_validation_retries",
        "stale_artifact_retry_index",
        "max_stale_artifact_retries",
    ];
    scope.get("scope_kind").and_then(Value::as_str) == Some("remediation_result_validation")
        && scope.get("run_id").and_then(Value::as_str) == Some(binding.run_id.as_str())
        && scope.get("repository_owner").and_then(Value::as_str)
            == Some(binding.repository_owner.as_str())
        && scope.get("repository_name").and_then(Value::as_str)
            == Some(binding.repository_name.as_str())
        && scope.get("pr_number").and_then(Value::as_u64) == Some(binding.pr_number)
        && scope.get("input_head_sha").and_then(Value::as_str) == input_head
        && scope.get("output_head_sha").and_then(Value::as_str) == output_head
        && scope.get("plan_artifact_sequence").and_then(Value::as_u64) == Some(plan_sequence)
        && required_numbers
            .iter()
            .all(|field| scope.get(*field).and_then(Value::as_u64).is_some())
        && retry_index_within_bound(
            scope,
            "remediation_attempt_index",
            "max_remediation_attempts",
        )
        && retry_index_within_bound(scope, "validation_retry_index", "max_validation_retries")
        && retry_index_within_bound(
            scope,
            "stale_artifact_retry_index",
            "max_stale_artifact_retries",
        )
        && evidence_ref.is_none_or(|reference| {
            scope
                .get("remediation_attempt_index")
                .and_then(Value::as_u64)
                == Some(reference.remediation_attempt_index)
        })
}

fn retry_index_within_bound(
    scope: &serde_json::Map<String, Value>,
    index_field: &str,
    max_field: &str,
) -> bool {
    let Some(index) = scope.get(index_field).and_then(Value::as_u64) else {
        return false;
    };
    scope
        .get(max_field)
        .and_then(Value::as_u64)
        .is_some_and(|max| max > 0 && index < max)
}
