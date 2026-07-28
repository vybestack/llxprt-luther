//! Response validation, reuse checks, and feedback-item parsing.

use super::*;

pub(super) fn validate_response(
    raw: &str,
    request: &FeedbackEvaluationRequest,
) -> Result<FeedbackEvaluationResponse, RejectReason> {
    let value = parse_feedback_evaluator_json(raw).map_err(|err| RejectReason {
        reason: format!("malformed_json: {err}"),
        parsed_decision: None,
        observed_head_sha: None,
    })?;
    reject_batch_response_fields(&value)?;

    let mut response = feedback_response_from_value(&value)?;
    apply_low_confidence_needs_judgment_policy(request, &mut response);
    if response.item_id != request.item_id {
        return Err(reject("wrong_item_id", &value));
    }
    if response.stable_marker_key != request.stable_marker_key {
        return Err(reject("wrong_stable_marker_key", &value));
    }
    if response.body_hash != request.body_hash {
        return Err(reject("wrong_body_hash", &value));
    }
    if response.head_sha != request.head_sha {
        return Err(reject("wrong_head_sha", &value));
    }
    if !matches!(
        response.decision.as_str(),
        "valid" | "invalid" | "out_of_scope" | "needs_user_judgment"
    ) {
        return Err(reject("unknown_decision", &value));
    }
    // Newly generated evaluator responses must carry explicit two-axis
    // classifications (issue 142). Historical persisted artifacts are read
    // via `validate_reusable_accepted`, which tolerates missing axes via
    // legacy projection; this check applies only to fresh adapter output.
    validate_two_axis_fields(&response, &value)?;
    if response.decision != "valid" && response.reason.trim().is_empty() {
        return Err(reject("missing_required_reason", &value));
    }
    if response
        .recommended_action
        .as_deref()
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        return Err(reject("missing_recommended_action", &value));
    }
    if response.response_text.trim().is_empty() {
        return Err(reject("missing_response_text", &value));
    }
    Ok(response)
}

/// Legal correctness enum values accepted from the evaluator (issue 142).
pub(super) const LEGAL_CORRECTNESS_VALUES: &[&str] =
    &["blocker", "high", "medium", "low", "invalid"];

/// Legal delivery_scope enum values accepted from the evaluator (issue 142).
pub(super) const LEGAL_DELIVERY_SCOPE_VALUES: &[&str] = &[
    "required_acceptance_criterion",
    "regression_from_current_patch",
    "small_adjacent_fix",
    "follow_up_issue",
    "user_decision",
];

/// Validate that a **freshly generated** evaluator response carries both
/// required two-axis classifications with legal enum values.
///
/// This applies only to newly generated adapter output. Historical persisted
/// artifacts that predate the two-axis schema are read via
/// [`validate_reusable_accepted`], which tolerates missing axes via legacy
/// projection — ensuring backward compatibility for already-stored state.
fn validate_two_axis_fields(
    response: &FeedbackEvaluationResponse,
    value: &Value,
) -> Result<(), RejectReason> {
    let correctness = response.correctness.as_deref().unwrap_or("");
    if correctness.is_empty() {
        return Err(reject("missing_correctness", value));
    }
    if !LEGAL_CORRECTNESS_VALUES.contains(&correctness) {
        return Err(reject("invalid_correctness", value));
    }
    let delivery_scope = response.delivery_scope.as_deref().unwrap_or("");
    if delivery_scope.is_empty() {
        return Err(reject("missing_delivery_scope", value));
    }
    if !LEGAL_DELIVERY_SCOPE_VALUES.contains(&delivery_scope) {
        return Err(reject("invalid_delivery_scope", value));
    }
    Ok(())
}

pub(super) fn reject_batch_response_fields(value: &Value) -> Result<(), RejectReason> {
    if value.is_array() {
        return Err(reject("response_array_or_batch", value));
    }
    let object = value
        .as_object()
        .ok_or_else(|| reject("response_not_object", value))?;
    for (field, field_value) in object {
        if is_forbidden_response_field(field, field_value) {
            return Err(reject("batch_or_extra_item_ids", value));
        }
    }
    Ok(())
}

pub(super) fn deterministic_feedback_evaluation(
    item: &FeedbackItem,
    accepted_at: String,
) -> Option<Value> {
    if !is_coderabbit_summary_item(item) {
        return None;
    }
    Some(json!({
        "item_id": item.item_id,
        "stable_marker_key": item.stable_marker_key,
        "body_hash": item.body_hash,
        "head_sha": item.head_sha,
        "decision": "invalid",
        "correctness": "invalid",
        "delivery_scope": "user_decision",
        "reason": "CodeRabbit summary/walkthrough comments are informational and do not identify a specific actionable feedback item.",
        "recommended_action": "No code changes or review-thread response are required for the summary comment.",
        "response_text": "This is an informational CodeRabbit summary/walkthrough comment rather than an actionable review item, so no code change is required.",
        "accepted_at": accepted_at,
        "attempt_count": 0,
        "source": "deterministic",
        "reuse_state": "not_reused"
    }))
}

pub(super) fn is_coderabbit_summary_item(item: &FeedbackItem) -> bool {
    let key = item.stable_marker_key.to_ascii_lowercase();
    let body = item.body.to_ascii_lowercase();
    key.starts_with(SUMMARY_MARKER_KEY_PREFIX)
        || body.contains("summary by coderabbit")
        || body.contains("summarize by coderabbit")
        || (body.contains("walkthrough") && body.contains("coderabbit"))
        || body.contains("coderabbit finished reviewing this pull request")
        || body.contains("rate limited by coderabbit")
        || body.contains("review limit reached")
        || (body.contains("coderabbit") && body.contains("run out of usage credits"))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012
/// @pseudocode lines 4,8
pub(super) fn feedback_items(feedback: &Value) -> Result<Vec<FeedbackItem>, EngineError> {
    let array = feedback
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| feedback_eval_error("coderabbit-feedback missing items array"))?;
    array
        .iter()
        .map(|item| {
            Ok(FeedbackItem {
                item_id: require_string(item, "item_id")?,
                stable_marker_key: require_string(item, "stable_marker_key")?,
                body_hash: require_string(item, "body_hash")?,
                head_sha: item
                    .get("commit_sha")
                    .and_then(Value::as_str)
                    .or_else(|| item.get("head_sha").and_then(Value::as_str))
                    .ok_or_else(|| {
                        feedback_eval_error("feedback item missing commit_sha/head_sha")
                    })?
                    .to_string(),
                author_login: item
                    .get("author_login")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                author_kind: item
                    .get("author_kind")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                body: require_string(item, "body")?,
                path: item
                    .get("path")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                url: item
                    .get("url")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
            })
        })
        .collect()
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011
/// @pseudocode lines 8-9
pub(super) fn build_request(
    binding: &PrFollowupBinding,
    item: &FeedbackItem,
) -> FeedbackEvaluationRequest {
    FeedbackEvaluationRequest {
        item_id: item.item_id.clone(),
        stable_marker_key: item.stable_marker_key.clone(),
        body_hash: item.body_hash.clone(),
        head_sha: item.head_sha.clone(),
        repository_owner: binding.repository_owner.clone(),
        repository_name: binding.repository_name.clone(),
        author_kind: item.author_kind.clone(),
        pr_number: binding.pr_number,
        author_login: item.author_login.clone(),
        body: item.body.clone(),
        path: item.path.clone(),
        url: item.url.clone(),
        allowed_decisions: vec![
            "valid".to_string(),
            "invalid".to_string(),
            "out_of_scope".to_string(),
            "needs_user_judgment".to_string(),
        ],
    }
}

pub(super) fn accepted_result(
    response: &FeedbackEvaluationResponse,
    accepted_at: String,
    attempt_count: u64,
    source: &str,
    reuse_state: &str,
) -> Value {
    let mut result = json!({
        "item_id": response.item_id,
        "stable_marker_key": response.stable_marker_key,
        "body_hash": response.body_hash,
        "head_sha": response.head_sha,
        "decision": response.decision,
        "reason": response.reason,
        "recommended_action": response.recommended_action.clone().unwrap_or_default(),
        "response_text": response.response_text,
        "accepted_at": accepted_at,
        "attempt_count": attempt_count,
        "source": source,
        "reuse_state": reuse_state
    });
    if let Some(object) = result.as_object_mut() {
        if let Some(ref correctness) = response.correctness {
            object.insert(
                "correctness".to_string(),
                Value::String(correctness.clone()),
            );
        }
        if let Some(ref delivery_scope) = response.delivery_scope {
            object.insert(
                "delivery_scope".to_string(),
                Value::String(delivery_scope.clone()),
            );
        }
    }
    result
}

pub(super) fn validate_reusable_accepted(
    binding: &PrFollowupBinding,
    item: &FeedbackItem,
    value: &Value,
) -> Result<(), EngineError> {
    let decision = require_string(value, "decision")?;
    if !matches!(
        decision.as_str(),
        "valid" | "invalid" | "out_of_scope" | "needs_user_judgment"
    ) {
        return Err(feedback_eval_error("unknown reusable decision"));
    }
    if require_string(value, "item_id")? != item.item_id
        || require_string(value, "stable_marker_key")? != item.stable_marker_key
        || require_string(value, "body_hash")? != item.body_hash
        || require_string(value, "head_sha")? != item.head_sha
    {
        return Err(feedback_eval_error("reusable evaluation identity mismatch"));
    }
    if require_string(value, "repository_owner")? != binding.repository_owner
        || require_string(value, "repository_name")? != binding.repository_name
        || require_u64(value, "pr_number")? != binding.pr_number
    {
        return Err(feedback_eval_error("reusable evaluation binding mismatch"));
    }

    if decision != "valid"
        && value
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .is_empty()
    {
        return Err(feedback_eval_error("missing reusable reason"));
    }
    if value
        .get("recommended_action")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        return Err(feedback_eval_error("missing reusable recommended_action"));
    }
    if value
        .get("response_text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        return Err(feedback_eval_error("missing reusable response_text"));
    }
    Ok(())
}

pub(super) fn upsert_state_entry(
    entries: &mut Vec<Value>,
    binding: &PrFollowupBinding,
    item: &FeedbackItem,
    accepted: &Value,
    timestamp: String,
) {
    entries.retain(|entry| {
        !(entry.get("stable_marker_key").and_then(Value::as_str)
            == Some(item.stable_marker_key.as_str())
            && entry.get("body_hash").and_then(Value::as_str) == Some(item.body_hash.as_str())
            && entry.get("head_sha").and_then(Value::as_str) == Some(item.head_sha.as_str()))
    });
    let mut accepted_with_binding = accepted.clone();
    if let Some(object) = accepted_with_binding.as_object_mut() {
        object.insert(
            "repository_owner".to_string(),
            Value::from(binding.repository_owner.clone()),
        );
        object.insert(
            "repository_name".to_string(),
            Value::from(binding.repository_name.clone()),
        );
        object.insert("pr_number".to_string(), Value::from(binding.pr_number));
    }
    entries.push(json!({
        "item_id": item.item_id,
        "stable_marker_key": item.stable_marker_key,
        "body_hash": item.body_hash,
        "head_sha": item.head_sha,
        "first_seen_at": timestamp,
        "last_seen_at": timestamp,
        "evaluation_status": "accepted",
        "accepted_evaluation": accepted_with_binding,
        "remediation_status": "pending",
        "marker_status": "pending",
        "resolution_status": "pending",
        "superseded": false,
        "stale": false,
        "reuse_eligible": true
    }));
}

pub(super) fn exactly_one_accepted_per_item(items: &[FeedbackItem], accepted: &[Value]) -> bool {
    let mut counts: BTreeMap<(String, String, String), u64> = BTreeMap::new();
    for value in accepted {
        let key = (
            value
                .get("item_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            value
                .get("body_hash")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            value
                .get("head_sha")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        );
        *counts.entry(key).or_default() += 1;
    }
    items.iter().all(|item| {
        counts.get(&(
            item.item_id.clone(),
            item.body_hash.clone(),
            item.head_sha.clone(),
        )) == Some(&1)
    }) && counts.len() == items.len()
}
