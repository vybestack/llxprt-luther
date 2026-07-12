use super::*;
use crate::engine::executors::pr_followup_types::NO_REMEDIATION_OUTPUT_HEAD;

pub(super) fn write_pending_marker_actions_for_invalid_feedback(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    mark_invalid: &[Value],
    clock: &dyn ClockSleeper,
) -> Result<(), EngineError> {
    if mark_invalid.is_empty() {
        return Ok(());
    }

    write_pending_marker_actions(
        store,
        binding,
        step_id,
        step_order,
        mark_invalid,
        None,
        clock,
    )
}

pub(super) fn write_pending_marker_actions(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    items: &[Value],
    remediation_output_head_sha: Option<&str>,
    clock: &dyn ClockSleeper,
) -> Result<(), EngineError> {
    let prior = store
        .read_carried_forward_json(binding, "pending-feedback-marker-actions")?
        .map(normalize_legacy_pending_marker_artifact);
    let carry_forward = prior_artifact_sequence(prior.as_ref());
    let mut pending_actions = PendingActionSet::from_prior(prior.as_ref());
    pending_actions.extend(binding, items, remediation_output_head_sha);
    let payload = PendingMarkerActionsArtifact {
        pending_actions: pending_actions.into_sorted_actions(),
        carry_forward_from_artifact_sequence: carry_forward,
        marker_policy: pending_marker_policy(remediation_output_head_sha),
        updated_at: clock.now_rfc3339(),
    };
    store.write_json_artifact(
        binding,
        "pending-feedback-marker-actions",
        step_id,
        step_order,
        &payload,
        None,
        clock,
    )?;
    Ok(())
}

struct PendingActionSet {
    actions: Vec<Value>,
    seen: BTreeSet<String>,
}

impl PendingActionSet {
    fn from_prior(prior: Option<&Value>) -> Self {
        let mut actions = prior
            .and_then(|value| value.get("pending_actions"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        // Summary markers are informational and must not be carried forward.
        actions.retain(|action| !value_has_summary_marker_key(action));
        let seen = actions
            .iter()
            .filter_map(pending_action_idempotency_key)
            .map(ToString::to_string)
            .collect();
        Self { actions, seen }
    }

    fn extend(
        &mut self,
        binding: &PrFollowupBinding,
        items: &[Value],
        remediation_output_head_sha: Option<&str>,
    ) {
        for item in items
            .iter()
            .filter(|item| !value_has_summary_marker_key(item))
        {
            let action = pending_marker_action(binding, item, remediation_output_head_sha);
            let Some(key) = pending_action_idempotency_key(&action).map(ToString::to_string) else {
                continue;
            };
            if action.get("action_kind").and_then(Value::as_str) == Some("comment_fixed") {
                self.remove_superseded_classifications(&action);
            }
            if self.seen.insert(key) {
                self.actions.push(action);
            }
        }
    }

    fn remove_superseded_classifications(&mut self, fixed: &Value) {
        let mut superseded_keys = Vec::new();
        self.actions.retain(|prior| {
            let retain = prior.get("action_kind").and_then(Value::as_str) == Some("comment_fixed")
                || !same_original_feedback_identity(prior, fixed);
            if !retain {
                superseded_keys
                    .extend(pending_action_idempotency_key(prior).map(ToString::to_string));
            }
            retain
        });
        for key in superseded_keys {
            self.seen.remove(&key);
        }
    }

    fn into_sorted_actions(mut self) -> Vec<Value> {
        self.actions.sort_by(|left, right| {
            pending_action_idempotency_key(left)
                .unwrap_or_default()
                .cmp(pending_action_idempotency_key(right).unwrap_or_default())
        });
        self.actions
    }
}

fn same_original_feedback_identity(left: &Value, right: &Value) -> bool {
    [
        "item_id",
        "stable_marker_key",
        "body_hash",
        "source_head_sha",
    ]
    .iter()
    .all(|field| left.get(*field) == right.get(*field))
}

fn prior_artifact_sequence(prior: Option<&Value>) -> Option<u64> {
    prior
        .and_then(|value| value.get("artifact_sequence"))
        .and_then(Value::as_u64)
}

fn pending_action_idempotency_key(action: &Value) -> Option<&str> {
    action.get("idempotency_key").and_then(Value::as_str)
}

fn pending_marker_policy(remediation_output_head_sha: Option<&str>) -> Value {
    json!({
        "invalid": "comment_invalid",
        "out_of_scope": "comment_out_of_scope",
        "fixed": "comment_fixed",
        "changed": "comment_fixed",
        "remediation_output_head_sha": remediation_output_head_sha
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 7-9
fn pending_marker_action(
    binding: &PrFollowupBinding,
    item: &Value,
    remediation_output_head_sha: Option<&str>,
) -> Value {
    let source_id = string_field(item, "source_id", "unknown-feedback-item");
    let stable_marker_key = item
        .get("stable_marker_key")
        .and_then(Value::as_str)
        .unwrap_or(&source_id)
        .to_string();
    let decision = item
        .get("decision")
        .and_then(Value::as_str)
        .unwrap_or("invalid");
    let action_kind = pending_marker_action_kind(item, decision);
    let body_hash = string_field(item, "body_hash", "no-body-hash");
    let output_head = remediation_output_head_sha.unwrap_or(NO_REMEDIATION_OUTPUT_HEAD);
    let idempotency_key =
        pending_marker_idempotency_key(binding, output_head, &stable_marker_key, action_kind);
    pending_marker_action_value(PendingMarkerActionValue {
        binding,
        item,
        source_id: &source_id,
        stable_marker_key: &stable_marker_key,
        decision,
        action_kind,
        body_hash: &body_hash,
        output_head,
        remediation_output_head_sha,
        idempotency_key,
    })
}

fn pending_marker_action_kind<'a>(item: &Value, decision: &'a str) -> &'a str {
    if item.get("marker_action").and_then(Value::as_str) == Some("comment_fixed") {
        "comment_fixed"
    } else if decision == "out_of_scope" {
        "comment_out_of_scope"
    } else {
        "comment_invalid"
    }
}

fn pending_marker_idempotency_key(
    binding: &PrFollowupBinding,
    output_head: &str,
    stable_marker_key: &str,
    action_kind: &str,
) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}:{}:{}",
        binding.run_id,
        binding.repository_owner,
        binding.repository_name,
        binding.pr_number,
        binding.head_sha,
        output_head,
        stable_marker_key,
        action_kind
    )
}

struct PendingMarkerActionValue<'a> {
    binding: &'a PrFollowupBinding,
    item: &'a Value,
    source_id: &'a str,
    stable_marker_key: &'a str,
    decision: &'a str,
    action_kind: &'a str,
    body_hash: &'a str,
    output_head: &'a str,
    remediation_output_head_sha: Option<&'a str>,
    idempotency_key: String,
}

fn pending_marker_action_value(action: PendingMarkerActionValue<'_>) -> Value {
    let item = action.item;
    let binding = action.binding;
    let output_head_sha = action
        .remediation_output_head_sha
        .map_or(Value::Null, |head| json!(head));
    let remediation_input_head_sha = item
        .get("remediation_input_head_sha")
        .cloned()
        .unwrap_or_else(|| json!(binding.head_sha));
    json!({
        "action_id": format!("{}:{}:{}:{}:{}", action.action_kind, action.stable_marker_key, action.body_hash, binding.head_sha, action.output_head),
        "action_kind": action.action_kind,
        "item_id": action.source_id,
        "original_feedback_identity": {
            "item_id": action.source_id,
            "stable_marker_key": action.stable_marker_key,
            "body_hash": action.body_hash,
            "source_head_sha": binding.head_sha,
            "thread_id": item.get("thread_id").cloned().unwrap_or(Value::Null),
            "comment_database_id": item.get("comment_database_id").cloned().unwrap_or(Value::Null)
        },
        "thread_id": item.get("thread_id").cloned().unwrap_or(Value::Null),
        "comment_database_id": item.get("comment_database_id").cloned().unwrap_or(Value::Null),
        "stable_marker_key": action.stable_marker_key,
        "source_head_sha": binding.head_sha,
        "remediation_input_head_sha": remediation_input_head_sha,
        "remediation_output_head_sha": output_head_sha,
        "remediation_output_head": action.output_head,
        "body_hash": action.body_hash,
        "idempotency_key": action.idempotency_key,
        "comment_body_template_id": action.action_kind,
        "comment_body_artifact_path": Value::Null,
        "resolution_required": action.action_kind == "comment_fixed",
        "status": "pending",
        "reason": string_field(item, "reason", action.decision),
        "response_text": item.get("response_text").cloned().unwrap_or(Value::Null),
        "remediation_result_status": item.get("remediation_result_status").cloned().unwrap_or(Value::Null),
        "remediation_result_evidence": item.get("remediation_result_evidence").cloned().unwrap_or(Value::Null),
        "evidence": item.get("evidence").cloned().unwrap_or_else(|| item.clone()),
        "source_artifact_sequence": item.get("source_artifact_sequence").cloned().unwrap_or(Value::Null),
        "remediation_result_artifact_sequence": item.get("remediation_result_artifact_sequence").cloned().unwrap_or(Value::Null),
        "remediation_result_write_sequence": item.get("remediation_result_write_sequence").cloned().unwrap_or(Value::Null),
        "remediation_result_producer_step_id": item.get("remediation_result_producer_step_id").cloned().unwrap_or(Value::Null),
        "plan_artifact_sequence": item.get("plan_artifact_sequence").cloned().unwrap_or(Value::Null),
        "remediation_attempt_index": item.get("remediation_attempt_index").cloned().unwrap_or(Value::Null)
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P11
/// @requirement:REQ-PRFU-014
/// @pseudocode lines 24-28
/// Context bundle for `write_pending_marker_actions_for_fixed_feedback` so the
/// fixed-feedback writer stays within the clippy argument-count limit without
/// suppressing the lint.
pub(super) struct FixedFeedbackMarkerContext<'a> {
    pub(super) store: &'a PrFollowupArtifactStore,
    pub(super) binding: &'a PrFollowupBinding,
    pub(super) step_id: &'a str,
    pub(super) step_order: u64,
    pub(super) plan: &'a Value,
    pub(super) validation_payload: &'a RemediationResultValidationArtifact,
    pub(super) result_sequence: &'a ArtifactSequenceMetadata,
    pub(super) clock: &'a dyn ClockSleeper,
}

pub(super) fn write_pending_marker_actions_for_fixed_feedback(
    ctx: &FixedFeedbackMarkerContext<'_>,
) -> Result<(), EngineError> {
    let fixed_items =
        fixed_feedback_marker_items(ctx.plan, ctx.validation_payload, ctx.result_sequence);
    if fixed_items.is_empty() {
        return Ok(());
    }
    write_pending_marker_actions(
        ctx.store,
        ctx.binding,
        ctx.step_id,
        ctx.step_order,
        &fixed_items,
        Some(ctx.validation_payload.output_head_sha.as_str()),
        ctx.clock,
    )
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P11
/// @requirement:REQ-PRFU-014
/// @pseudocode lines 24-28
fn fixed_feedback_marker_items(
    plan: &Value,
    validation_payload: &RemediationResultValidationArtifact,
    result_sequence: &ArtifactSequenceMetadata,
) -> Vec<Value> {
    let mut plan_items = plan_items_by_key(plan);
    let mut items = Vec::new();
    for result in &validation_payload.results {
        let source_type = string_field(result, "source_type", "");
        let source_id = string_field(result, "source_id", "");
        if source_type != "coderabbit_feedback" {
            continue;
        }
        let status = string_field(result, "status", "");
        if !matches!(
            status.as_str(),
            "fixed" | "changed" | "already_satisfied" | "not_reproduced"
        ) {
            continue;
        }
        let key = format!("{source_type}:{source_id}");
        let Some(mut item) = plan_items.remove(&key) else {
            continue;
        };
        if let Some(thread_id) = result.get("thread_id").cloned() {
            item["thread_id"] = thread_id;
        }
        if let Some(comment_database_id) = result.get("comment_database_id").cloned() {
            item["comment_database_id"] = comment_database_id;
        }
        item["decision"] = json!("valid");
        item["marker_action"] = json!("comment_fixed");
        item["remediation_result_status"] = json!(status);
        item["remediation_input_head_sha"] = json!(validation_payload.input_head_sha.clone());
        item["remediation_output_head_sha"] = json!(validation_payload.output_head_sha.clone());
        if let Some(response_text) = result.get("response_text").cloned() {
            item["response_text"] = response_text;
        }
        item["remediation_result_evidence"] = result
            .get("evidence")
            .cloned()
            .unwrap_or_else(|| result.clone());
        item["remediation_result_artifact_sequence"] = json!(result_sequence.artifact_sequence);
        item["remediation_result_write_sequence"] = json!(result_sequence.write_sequence);
        item["remediation_result_producer_step_id"] = json!(result_sequence.producer_step_id);
        item["plan_artifact_sequence"] = validation_payload.plan_artifact_sequence.clone();
        item["remediation_attempt_index"] = json!(validation_payload.remediation_attempt_index);
        items.push(item);
    }
    items
}
