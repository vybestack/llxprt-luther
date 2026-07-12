use super::*;
use crate::engine::executors::pr_followup_artifacts::{
    ArtifactWriter, ClockSleeper, PrFollowupArtifactStore,
};
use crate::engine::executors::pr_followup_types::{
    value_has_summary_marker_key, PrFollowupBinding, NO_REMEDIATION_OUTPUT_HEAD,
};
use crate::engine::runner::EngineError;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Thread identifier map keyed by pending-action collision key.
/// Each value carries the optional GraphQL thread id and numeric comment id.
type ThreadIdentifierMap = BTreeMap<String, (Option<String>, Option<i64>)>;

pub(super) fn read_pending_marker_artifact(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
) -> Result<Value, EngineError> {
    // Pending marker actions must survive a remediation head change. A prior-
    // head artifact for the **same PR** is carried forward because each
    // pending action inside carries its own `source_head_sha` binding: the
    // marker gate uses that identity to locate immutable remediation evidence
    // from the history directory. Only an absent artifact degrades to empty;
    // a genuinely different-PR artifact remains a fatal binding mismatch.
    match store.read_carried_forward_json(binding, PENDING_MARKER_ACTIONS_FAMILY)? {
        Some(value) => Ok(normalize_legacy_pending_marker_artifact(value)),
        None => Ok(empty_pending_marker_artifact()),
    }
}

/// Normalizes legacy pending-action routing fields while preserving stored
/// action IDs and hashes so retries retain their original idempotency identity.
pub(crate) fn normalize_legacy_pending_marker_artifact(mut artifact: Value) -> Value {
    let Some(actions) = artifact
        .get_mut("pending_actions")
        .and_then(Value::as_array_mut)
    else {
        return artifact;
    };
    for action in actions {
        let Some(object) = action.as_object_mut() else {
            continue;
        };
        if object.contains_key("remediation_output_head") {
            continue;
        }
        match object.get("remediation_output_head_sha") {
            Some(Value::String(head)) if !head.trim().is_empty() => {
                object.insert("remediation_output_head".to_string(), json!(head));
            }
            None | Some(Value::Null) => {
                object.insert(
                    "remediation_output_head".to_string(),
                    json!(NO_REMEDIATION_OUTPUT_HEAD),
                );
            }
            _ => {}
        }
    }
    artifact
}

pub(super) fn empty_pending_marker_artifact() -> Value {
    json!({
        "pending_actions": [],
        "carry_forward_from_artifact_sequence": null,
        "marker_policy": {},
        "updated_at": null
    })
}

pub(super) fn refresh_pending_marker_actions_from_current_artifacts(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    pending_artifact: &mut Value,
    params: &Value,
    clock: &dyn ClockSleeper,
) -> Result<(), EngineError> {
    let Some(feedback) = read_optional_current_json(store, binding, "coderabbit-feedback")? else {
        return Ok(());
    };
    let Some(evaluations) = read_optional_current_json(store, binding, "feedback-evaluations")?
    else {
        pending_artifact["refreshed_from_current_artifacts_at"] = json!(clock.now_rfc3339());
        pending_artifact["refresh_incomplete_reason"] = json!("missing_feedback_evaluations");
        return Ok(());
    };
    let feedback_items = feedback_items_by_identity(&feedback);
    let mut actions = pending_artifact
        .get("pending_actions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // Carry-forward pruning: drop any stale summary-keyed action loaded from a
    // pre-fix pending-feedback-marker-actions.json so reruns never re-persist or
    // post an informational summary marker.
    actions.retain(|action| !value_has_summary_marker_key(action));
    let mut seen = actions
        .iter()
        .map(pending_action_collision_key)
        .collect::<BTreeSet<_>>();
    for evaluation in evaluations
        .get("accepted_results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(action) = current_evaluation_marker_action(
            binding,
            evaluation,
            feedback_items.get(&evaluation_identity_key(evaluation)),
            params,
            clock,
        ) else {
            continue;
        };
        let key = pending_action_collision_key(&action);
        if seen.insert(key) {
            actions.push(action);
        }
    }
    pending_artifact["pending_actions"] = json!(actions);
    pending_artifact["refreshed_from_current_artifacts_at"] = json!(clock.now_rfc3339());
    pending_artifact["refresh_incomplete_reason"] = json!(null);
    Ok(())
}

pub(super) fn read_optional_current_json(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    artifact_family: &str,
) -> Result<Option<Value>, EngineError> {
    let path = store.canonical_path(binding, artifact_family);
    if !path.exists() {
        return Ok(None);
    }
    // A present-but-corrupt or binding-mismatched artifact is a fatal error:
    // pending-action producers must never silently swallow artifact read
    // failures because that would produce incomplete pending actions that
    // skip valid remediation evidence. Only a genuine NotFound degrades to
    // None.
    store.read_current_json(binding, artifact_family).map(Some)
}

pub(super) fn feedback_items_by_identity(feedback: &Value) -> BTreeMap<String, Value> {
    feedback
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|item| (evaluation_identity_key(item), item.clone()))
        .collect()
}

/// Index collected review-thread identifiers (thread id + numeric comment id)
/// by the most specific stable item identity available. Item-level keys avoid
/// collisions when several comments share the same GraphQL review thread marker.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016
pub(super) fn collect_thread_identifiers_by_action_key(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
) -> Result<ThreadIdentifierMap, EngineError> {
    let mut identifiers: ThreadIdentifierMap = BTreeMap::new();
    let Some(feedback) = read_optional_current_json(store, binding, "coderabbit-feedback")? else {
        return Ok(identifiers);
    };
    let items = feedback
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut stable_marker_key_counts: BTreeMap<String, usize> = BTreeMap::new();
    for item in &items {
        if let Some(stable_marker_key) = item.get("stable_marker_key").and_then(Value::as_str) {
            *stable_marker_key_counts
                .entry(stable_marker_key.to_string())
                .or_default() += 1;
        }
    }
    for item in &items {
        let thread_id = item
            .get("thread_id")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let comment_database_id = item.get("comment_database_id").and_then(Value::as_i64);
        if thread_id.is_none() && comment_database_id.is_none() {
            continue;
        }
        let key = thread_identifier_action_key(item, &stable_marker_key_counts);
        if !key.is_empty() {
            identifiers.insert(key, (thread_id, comment_database_id));
        }
    }
    Ok(identifiers)
}

/// Fill in missing `thread_id`/`comment_database_id` on a pending marker action
/// from the collected review-thread index, without overwriting present values.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016
pub(super) fn backfill_thread_identifiers(
    mut value: Value,
    identifiers: &BTreeMap<String, (Option<String>, Option<i64>)>,
) -> Value {
    let key = pending_action_thread_identifier_key(&value);
    if key.is_empty() {
        return value;
    }
    let Some((thread_id, comment_database_id)) = identifiers.get(&key) else {
        return value;
    };
    if let Some(object) = value.as_object_mut() {
        if object.get("thread_id").is_none_or(Value::is_null) {
            if let Some(thread_id) = thread_id {
                object.insert("thread_id".to_string(), json!(thread_id));
            }
        }
        if object.get("comment_database_id").is_none_or(Value::is_null) {
            if let Some(comment_database_id) = comment_database_id {
                object.insert(
                    "comment_database_id".to_string(),
                    json!(comment_database_id),
                );
            }
        }
    }
    value
}

pub(super) fn thread_identifier_action_key(
    item: &Value,
    stable_marker_key_counts: &BTreeMap<String, usize>,
) -> String {
    let item_id = string_field(item, "item_id");
    if !item_id.is_empty() {
        return format!("item_id:{item_id}");
    }
    let body_hash = string_field(item, "body_hash");
    if !body_hash.is_empty() {
        return format!("body_hash:{body_hash}");
    }
    let stable_marker_key = string_field(item, "stable_marker_key");
    if stable_marker_key_counts
        .get(&stable_marker_key)
        .copied()
        .unwrap_or_default()
        == 1
    {
        return format!("stable_marker_key:{stable_marker_key}");
    }
    String::new()
}

pub(super) fn pending_action_thread_identifier_key(value: &Value) -> String {
    let item_id = string_field(value, "item_id");
    if !item_id.is_empty() {
        return format!("item_id:{item_id}");
    }
    let body_hash = string_field(value, "body_hash");
    if !body_hash.is_empty() {
        return format!("body_hash:{body_hash}");
    }
    let stable_marker_key = string_field(value, "stable_marker_key");
    if !stable_marker_key.is_empty() {
        // The feedback-item index only stores unique stable-marker keys; shared
        // keys intentionally remain unbackfilled rather than guessing a thread.
        return format!("stable_marker_key:{stable_marker_key}");
    }
    String::new()
}

pub(super) fn evaluation_identity_key(value: &Value) -> String {
    format!(
        "{}:{}:{}",
        string_field(value, "item_id"),
        string_field(value, "stable_marker_key"),
        string_field(value, "body_hash")
    )
}

pub(super) fn pending_action_collision_key(action: &Value) -> String {
    action
        .get("idempotency_key")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| {
            format!(
                "{}:{}:{}:{}:{}",
                string_field(action, "source_head_sha"),
                action
                    .get("remediation_output_head")
                    .and_then(Value::as_str)
                    .or_else(|| action
                        .get("remediation_output_head_sha")
                        .and_then(Value::as_str))
                    .unwrap_or(NO_REMEDIATION_OUTPUT_HEAD),
                string_field(action, "body_hash"),
                string_field(action, "action_kind"),
                string_field(action, "stable_marker_key")
            )
        })
}
