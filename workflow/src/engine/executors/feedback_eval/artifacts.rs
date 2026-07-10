//! Artifact/state persistence and step-parameter helpers.

use super::*;

pub(super) fn empty_artifact(
    state: EvaluationState,
    items_seen: u64,
    max_attempts: u64,
    source_artifacts: Vec<Value>,
) -> FeedbackEvaluationArtifact {
    FeedbackEvaluationArtifact {
        evaluation_state: state,
        items_seen,
        accepted_results: Vec::new(),
        rejected_attempts: Vec::new(),
        unevaluated_items: Vec::new(),
        budget_exhausted_items: Vec::new(),
        max_attempts_per_item: max_attempts,
        reused_results_count: 0,
        source_artifacts,
    }
}

pub(super) fn source_artifact(value: &Value, family: &str) -> Value {
    json!({
        "artifact_family": family,
        "artifact_sequence": value.get("artifact_sequence").cloned().unwrap_or(Value::Null),
        "write_sequence": value.get("write_sequence").cloned().unwrap_or(Value::Null),
        "producer_step_id": value.get("producer_step_id").cloned().unwrap_or(Value::Null)
    })
}

pub(super) fn unevaluated_item(item: &FeedbackItem, reason: &str) -> Value {
    json!({
        "item_id": item.item_id,
        "stable_marker_key": item.stable_marker_key,
        "body_hash": item.body_hash,
        "head_sha": item.head_sha,
        "reason": reason
    })
}

pub(super) fn reject(reason: &str, value: &Value) -> RejectReason {
    RejectReason {
        reason: reason.to_string(),
        parsed_decision: value
            .get("decision")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        observed_head_sha: value
            .get("head_sha")
            .and_then(Value::as_str)
            .map(ToString::to_string),
    }
}

pub(crate) fn required_value_string(value: &Value, field: &str) -> Result<String, RejectReason> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| reject(&format!("missing_{field}"), value))
}

pub(super) fn set_string_field(value: &mut Value, field: &str, text: &str) {
    if let Some(object) = value.as_object_mut() {
        object.insert(field.to_string(), Value::from(text));
    }
}

pub(super) fn write_evaluation_artifact(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    payload: &FeedbackEvaluationArtifact,
    clock: &dyn ClockSleeper,
    failure: Option<(&str, &str, Value)>,
) -> Result<(), EngineError> {
    store.write_json_artifact(
        binding,
        "feedback-evaluations",
        step_id,
        step_order,
        payload,
        failure,
        clock,
    )?;
    Ok(())
}

pub(super) fn write_state_artifact(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    entries: Vec<Value>,
    clock: &dyn ClockSleeper,
) -> Result<(), EngineError> {
    let state_index_hash = stable_json_hash(&Value::Array(entries.clone()));
    store.write_json_artifact(
        binding,
        "coderabbit-feedback-state",
        step_id,
        step_order,
        &json!({
            "state_entries": entries,
            "state_index_hash": state_index_hash,
            "superseded_entries": []
        }),
        None,
        clock,
    )?;
    Ok(())
}

// Pre-existing artifact writer shape shared by follow-up executors.
#[allow(clippy::too_many_arguments)]
pub(super) fn write_raw_response(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,

    item: &FeedbackItem,
    attempt: u64,
    raw: &str,
    clock: &dyn ClockSleeper,
) -> Result<PathBuf, EngineError> {
    let bounded = if raw.len() > RAW_RESPONSE_LIMIT_BYTES {
        &raw[..RAW_RESPONSE_LIMIT_BYTES]
    } else {
        raw
    };
    let record = store.write_raw_text_artifact(
        binding,
        "feedback-evaluator-raw-output",
        step_id,
        step_order,
        &format!(
            "{}-attempt-{}-raw-output",
            sanitize_path_segment(&item.item_id),
            attempt
        ),
        bounded,
        clock,
    )?;
    Ok(record.history_path)
}

pub(super) fn read_or_initialize_state(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    params: &Value,
) -> Result<Value, EngineError> {
    let path = store.canonical_path(binding, "coderabbit-feedback-state");
    if path.exists() {
        return store.read_current_json(binding, "coderabbit-feedback-state");
    }
    Ok(json!({
        "schema_version": PR_FOLLOWUP_SCHEMA_VERSION,
        "run_id": binding.run_id,
        "repository_owner": binding.repository_owner,
        "repository_name": binding.repository_name,
        "pr_number": binding.pr_number,
        "head_ref": binding.head_ref,
        "head_sha": binding.head_sha,
        "base_ref": binding.base_ref,
        "base_sha": binding.base_sha,
        "state_entries": params.get("state_entries").cloned().unwrap_or_else(|| json!([])),
        "state_index_hash": "empty",
        "superseded_entries": []
    }))
}

pub(super) fn read_or_build_binding(
    context: &StepContext,
    params: &Value,
    store: &PrFollowupArtifactStore,
) -> Result<PrFollowupBinding, EngineError> {
    let requested = PrFollowupBinding {
        schema_version: PR_FOLLOWUP_SCHEMA_VERSION,
        run_id: context.run_id().to_string(),
        repository_owner: string_param(context, params, "repository_owner", "example"),
        repository_name: string_param(context, params, "repository_name", "workflow"),
        pr_number: string_param(context, params, "pr_number", "1910")
            .parse()
            .unwrap_or(1910),
        head_ref: string_param(context, params, "head_ref", "feature"),
        head_sha: string_param(
            context,
            params,
            "head_sha",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
        base_ref: string_param(context, params, "base_ref", "main"),
        base_sha: Some(string_param(context, params, "base_sha", "base-a")),
    };

    if let Some(value) = store.find_current_pr_artifact_for_run(context.run_id(), &requested)? {
        return binding_from_value(&value);
    }
    Ok(requested)
}

pub(super) fn artifact_root(context: &StepContext, params: &Value) -> Result<PathBuf, EngineError> {
    let raw = params
        .get("artifact_root")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| feedback_eval_error("missing artifact_root"))?;
    let interpolated = interpolate_string(raw, context);
    if has_unresolved_template(&interpolated) {
        return Err(feedback_eval_error(format!(
            "artifact_root contains unresolved template: {interpolated}"
        )));
    }
    let path = PathBuf::from(interpolated);
    Ok(if path.is_absolute() {
        path
    } else {
        context.work_dir().join(path)
    })
}

pub(super) fn binding_from_value(value: &Value) -> Result<PrFollowupBinding, EngineError> {
    Ok(PrFollowupBinding {
        schema_version: u32::try_from(require_u64(value, "schema_version")?)
            .map_err(|err| feedback_eval_error(format!("schema_version out of range: {err}")))?,
        run_id: require_string(value, "run_id")?,
        repository_owner: require_string(value, "repository_owner")?,
        repository_name: require_string(value, "repository_name")?,
        pr_number: require_u64(value, "pr_number")?,
        head_ref: require_string(value, "head_ref")?,
        head_sha: require_string(value, "head_sha")?,
        base_ref: require_string(value, "base_ref")?,
        base_sha: value
            .get("base_sha")
            .and_then(Value::as_str)
            .map(ToString::to_string),
    })
}

pub(super) fn require_string(value: &Value, field: &str) -> Result<String, EngineError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| feedback_eval_error(format!("missing string field {field}")))
}

pub(super) fn require_u64(value: &Value, field: &str) -> Result<u64, EngineError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| feedback_eval_error(format!("missing integer field {field}")))
}

fn string_param(context: &StepContext, params: &Value, key: &str, default: &str) -> String {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(|value| interpolate_string(value, context))
        .filter(|value| !value.is_empty() && !has_unresolved_template(value))
        .or_else(|| {
            context
                .get(key)
                .filter(|value| !value.is_empty() && !has_unresolved_template(value))
                .cloned()
        })
        .unwrap_or_else(|| default.to_string())
}

pub(super) fn has_unresolved_template(value: &str) -> bool {
    value.contains('{') || value.contains('}')
}

pub(super) fn u64_param(params: &Value, key: &str, default: u64) -> u64 {
    params.get(key).and_then(Value::as_u64).unwrap_or(default)
}

pub(super) fn current_step_id(context: &StepContext, default: &str) -> String {
    context
        .get("current_step_id")
        .cloned()
        .unwrap_or_else(|| default.to_string())
}

pub(super) fn sanitize_path_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

pub(super) fn stable_json_hash(value: &Value) -> String {
    let text = serde_json::to_string(value).unwrap_or_default();
    let mut hash = 14_695_981_039_346_656_037_u64;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    format!("fnv64:{hash:016x}")
}

pub(super) fn feedback_eval_error(message: impl Into<String>) -> EngineError {
    EngineError::InvalidState(format!("feedback evaluator: {}", message.into()))
}
