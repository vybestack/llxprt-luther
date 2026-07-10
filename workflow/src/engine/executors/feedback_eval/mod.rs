//! Feedback evaluator executor and adapter implementation.
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
//! @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017,REQ-PRFU-020
//! @pseudocode lines 1-23

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::engine::executor::{interpolate_string, StepContext, StepExecutor};
use crate::engine::executors::feedback_eval_policy::{
    apply_low_confidence_accepted_policy, apply_low_confidence_needs_judgment_policy,
    feedback_response_from_value, is_forbidden_response_field, parse_feedback_evaluator_json,
    FeedbackEvaluationAdapter,
};
use crate::engine::executors::pr_followup_artifacts::{
    ArtifactWriter, ClockSleeper, PrFollowupArtifactStore, SystemPrFollowupFilesystem,
};
use crate::engine::executors::pr_followup_types::{
    EvaluationState, PrFollowupBinding, PR_FOLLOWUP_SCHEMA_VERSION, SUMMARY_MARKER_KEY_PREFIX,
};
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;

mod artifacts;
mod command;
mod validate;

pub(crate) use artifacts::required_value_string;
use artifacts::*;
pub use command::{
    CommandFeedbackEvaluationAdapter, FeedbackEvaluatorCommandRunner,
    ProcessFeedbackEvaluatorCommandRunner,
};
use validate::*;

pub const DEFAULT_FEEDBACK_EVALUATOR_ARGV: &[&str] = &[
    "llxprt",
    "--profile-load",
    "gpt55high",
    "--set",
    "reasoning.includeInResponse=false",
    "--set",
    "maxTurnsPerPrompt=1",
    "-p",
    "Evaluate the single PR review feedback request JSON from stdin. Classify it using only the JSON provided; do not use any tools, do not run commands, and do not inspect the repository. Use needs_user_judgment only when the comment asks for a genuine product/scope/design choice that cannot be decided from the current PR. Speculative robustness suggestions, low-value nits, optional future hardening, and comments phrased as consider/if this becomes an issue should be invalid or out_of_scope unless they identify a concrete current defect. Respond with exactly one JSON object containing item_id, stable_marker_key, body_hash, head_sha, decision, reason, recommended_action, and response_text. The response_text must be a non-empty, reviewer-facing message that Luther will post verbatim on the original review thread explaining the decision; do not address the reviewer as yourself or claim to have posted it. Do not return arrays or extra item identities.",
];

#[must_use]
pub fn default_feedback_evaluator_argv() -> Vec<String> {
    DEFAULT_FEEDBACK_EVALUATOR_ARGV
        .iter()
        .map(|arg| (*arg).to_string())
        .collect()
}

const MAX_ATTEMPTS_PER_ITEM: u64 = 3;
const RAW_RESPONSE_LIMIT_BYTES: usize = 16 * 1024;
pub(super) const DEFAULT_FEEDBACK_EVALUATOR_TIMEOUT_SECONDS: u64 = 300;

/// Single-item feedback evaluation request.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017,REQ-PRFU-020
/// @pseudocode lines 8-17
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct FeedbackEvaluationRequest {
    pub item_id: String,
    pub stable_marker_key: String,
    pub body_hash: String,
    pub head_sha: String,
    pub repository_owner: String,
    pub repository_name: String,
    pub pr_number: u64,
    pub author_login: String,
    pub author_kind: Option<String>,
    pub body: String,
    pub path: Option<String>,
    pub url: Option<String>,
    pub allowed_decisions: Vec<String>,
}

/// Single-item feedback evaluation response.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017,REQ-PRFU-020
/// @pseudocode lines 10-17
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct FeedbackEvaluationResponse {
    pub item_id: String,
    pub stable_marker_key: String,
    pub body_hash: String,
    pub head_sha: String,
    pub decision: String,
    pub reason: String,
    pub recommended_action: Option<String>,
    pub response_text: String,
}

/// Feedback evaluator executor for `feedback_evaluator`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017,REQ-PRFU-020
/// @pseudocode lines 1-23
#[derive(Clone)]
pub struct FeedbackEvaluatorExecutor {
    adapter: Arc<dyn FeedbackEvaluationAdapter>,
    clock: Arc<dyn ClockSleeper>,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 1-23
impl std::fmt::Debug for FeedbackEvaluatorExecutor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("FeedbackEvaluatorExecutor").finish()
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 1-23
impl FeedbackEvaluatorExecutor {
    #[must_use]
    pub fn new(
        adapter: impl FeedbackEvaluationAdapter + 'static,
        clock: impl ClockSleeper + 'static,
    ) -> Self {
        Self {
            adapter: Arc::new(adapter),
            clock: Arc::new(clock),
        }
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017,REQ-PRFU-020
/// @pseudocode lines 1-23
impl StepExecutor for FeedbackEvaluatorExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        evaluate_coderabbit_feedback(context, params, self.adapter.as_ref(), self.clock.as_ref())
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 1-23
#[derive(Clone, Debug)]
struct FeedbackItem {
    item_id: String,
    stable_marker_key: String,
    body_hash: String,
    head_sha: String,
    author_login: String,
    author_kind: Option<String>,
    body: String,
    path: Option<String>,
    url: Option<String>,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 6,14-21
#[derive(Clone, Debug, serde::Serialize)]
struct FeedbackEvaluationArtifact {
    evaluation_state: EvaluationState,
    items_seen: u64,
    accepted_results: Vec<Value>,
    rejected_attempts: Vec<Value>,
    unevaluated_items: Vec<Value>,
    budget_exhausted_items: Vec<Value>,
    max_attempts_per_item: u64,
    reused_results_count: u64,
    source_artifacts: Vec<Value>,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 1-23
// Pre-existing orchestration flow; split in a dedicated refactor stage.
#[allow(clippy::too_many_lines)]
fn evaluate_coderabbit_feedback(
    context: &StepContext,
    params: &Value,
    adapter: &dyn FeedbackEvaluationAdapter,
    clock: &dyn ClockSleeper,
) -> Result<StepOutcome, EngineError> {
    let artifact_root = artifact_root(context, params)?;
    let store =
        PrFollowupArtifactStore::with_filesystem(&artifact_root, &SystemPrFollowupFilesystem)?;
    let binding = read_or_build_binding(context, params, &store)?;
    let step_id = current_step_id(context, "evaluate_coderabbit_feedback");
    let step_order = u64_param(params, "step_order_index", 6);
    let max_attempts = MAX_ATTEMPTS_PER_ITEM;

    let feedback = match store.read_current_json(&binding, "coderabbit-feedback") {
        Ok(value) => value,
        Err(err) => {
            let payload = empty_artifact(
                EvaluationState::Fatal,
                0,
                max_attempts,
                vec![json!({
                    "artifact_family": "coderabbit-feedback",
                    "error": err.to_string()
                })],
            );
            write_evaluation_artifact(
                &store,
                &binding,
                &step_id,
                step_order,
                &payload,
                clock,
                Some((
                    "fatal",
                    "missing_or_unbindable_feedback",
                    json!({ "error": err.to_string() }),
                )),
            )?;
            return Ok(StepOutcome::Fatal);
        }
    };

    if feedback.get("readiness_state").and_then(Value::as_str) != Some("ready") {
        let payload = empty_artifact(
            EvaluationState::Fatal,
            0,
            max_attempts,
            vec![source_artifact(&feedback, "coderabbit-feedback")],
        );
        write_evaluation_artifact(
            &store,
            &binding,
            &step_id,
            step_order,
            &payload,
            clock,
            Some((
                "fatal",
                "feedback_not_ready",
                json!({ "readiness_state": feedback.get("readiness_state").cloned().unwrap_or(Value::Null) }),
            )),
        )?;
        return Ok(StepOutcome::Fatal);
    }

    let mut items = feedback_items(&feedback)?;
    items.sort_by(|left, right| {
        (
            left.stable_marker_key.as_str(),
            left.body_hash.as_str(),
            left.item_id.as_str(),
        )
            .cmp(&(
                right.stable_marker_key.as_str(),
                right.body_hash.as_str(),
                right.item_id.as_str(),
            ))
    });

    let state_value = read_or_initialize_state(&store, &binding, params)?;
    let state_entries = state_value
        .get("state_entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut accepted_results = Vec::new();
    let mut rejected_attempts = Vec::new();
    let mut unevaluated_items = Vec::new();
    let mut budget_exhausted_items = Vec::new();
    let mut fatal_reuse_errors = Vec::new();
    let mut new_state_entries = state_entries.clone();
    let mut reused_results_count = 0;

    for item in &items {
        match reusable_evaluation(&binding, item, &state_entries) {
            ReuseLookup::Reuse(value) => {
                let mut reused = value;
                set_string_field(&mut reused, "source", "reused");
                set_string_field(&mut reused, "reuse_state", "reused_from_state");
                accepted_results.push(reused);
                reused_results_count += 1;
            }
            ReuseLookup::Fatal(reason) => {
                fatal_reuse_errors.push(json!({
                    "item_id": item.item_id,
                    "stable_marker_key": item.stable_marker_key,
                    "body_hash": item.body_hash,
                    "head_sha": item.head_sha,
                    "reason": reason
                }));
                unevaluated_items.push(unevaluated_item(item, "fatal_prior_state"));
            }
            ReuseLookup::NoMatch => {
                let mut accepted: Option<Value> =
                    deterministic_feedback_evaluation(item, clock.now_rfc3339());
                if accepted.is_some() {
                    if let Some(accepted_value) = accepted.as_ref() {
                        upsert_state_entry(
                            &mut new_state_entries,
                            &binding,
                            item,
                            accepted_value,
                            clock.now_rfc3339(),
                        );
                    }
                }
                for attempt in 1..=max_attempts {
                    if accepted.is_some() {
                        break;
                    }
                    let request = build_request(&binding, item);
                    let raw = match adapter.evaluate(&request) {
                        Ok(raw) => raw,
                        Err(err) => {
                            let raw = format!("feedback evaluator command error: {err}");
                            let raw_response_artifact_path = write_raw_response(
                                &store, &binding, &step_id, step_order, item, attempt, &raw, clock,
                            )?;
                            rejected_attempts.push(json!({
                                "attempt_number": attempt,
                                "item_id": item.item_id,
                                "stable_marker_key": item.stable_marker_key,
                                "body_hash": item.body_hash,
                                "raw_response_artifact_path": raw_response_artifact_path.display().to_string(),
                                "reject_reason": "command_error",
                                "command_error": err.to_string(),
                                "parsed_decision": null,
                                "observed_head_sha": null
                            }));
                            continue;
                        }
                    };
                    let raw_response_artifact_path = write_raw_response(
                        &store, &binding, &step_id, step_order, item, attempt, &raw, clock,
                    )?;

                    match validate_response(&raw, &request) {
                        Ok(response) => {
                            let accepted_value = accepted_result(
                                &response,
                                clock.now_rfc3339(),
                                attempt,
                                "new",
                                "not_reused",
                            );
                            accepted = Some(accepted_value.clone());
                            upsert_state_entry(
                                &mut new_state_entries,
                                &binding,
                                item,
                                &accepted_value,
                                clock.now_rfc3339(),
                            );
                            break;
                        }
                        Err(reject) => rejected_attempts.push(json!({
                            "attempt_number": attempt,
                            "item_id": item.item_id,
                            "stable_marker_key": item.stable_marker_key,
                            "body_hash": item.body_hash,
                            "raw_response_artifact_path": raw_response_artifact_path.display().to_string(),
                            "reject_reason": reject.reason,
                            "parsed_decision": reject.parsed_decision,
                            "observed_head_sha": reject.observed_head_sha
                        })),
                    }
                }
                if let Some(value) = accepted {
                    accepted_results.push(value);
                } else {
                    budget_exhausted_items.push(json!({
                        "item_id": item.item_id,
                        "stable_marker_key": item.stable_marker_key,
                        "body_hash": item.body_hash,
                        "head_sha": item.head_sha,
                        "attempts": max_attempts
                    }));
                }
            }
        }
    }

    let complete = fatal_reuse_errors.is_empty()
        && budget_exhausted_items.is_empty()
        && unevaluated_items.is_empty()
        && exactly_one_accepted_per_item(&items, &accepted_results);
    let evaluation_state = if complete {
        EvaluationState::Complete
    } else if !budget_exhausted_items.is_empty() {
        EvaluationState::BudgetExhausted
    } else if !fatal_reuse_errors.is_empty() {
        EvaluationState::Fatal
    } else {
        EvaluationState::Incomplete
    };

    let payload = FeedbackEvaluationArtifact {
        evaluation_state,
        items_seen: items.len() as u64,
        accepted_results,
        rejected_attempts,
        unevaluated_items,
        budget_exhausted_items,
        max_attempts_per_item: max_attempts,
        reused_results_count,
        source_artifacts: vec![source_artifact(&feedback, "coderabbit-feedback")],
    };

    if complete {
        write_state_artifact(
            &store,
            &binding,
            &step_id,
            step_order,
            new_state_entries,
            clock,
        )?;
    }

    let failure = if complete {
        None
    } else {
        Some((
            evaluation_state.as_str(),
            match evaluation_state {
                EvaluationState::BudgetExhausted => "evaluation_budget_exhausted",
                _ => "evaluation_incomplete_or_fatal",
            },
            json!({
                "fatal_reuse_errors": fatal_reuse_errors,
                "items_seen": items.len()
            }),
        ))
    };
    write_evaluation_artifact(
        &store, &binding, &step_id, step_order, &payload, clock, failure,
    )?;

    Ok(if complete {
        StepOutcome::Success
    } else {
        StepOutcome::Fatal
    })
}

#[derive(Debug)]
pub(crate) struct RejectReason {
    reason: String,
    parsed_decision: Option<String>,
    observed_head_sha: Option<String>,
}

#[derive(Debug)]
enum ReuseLookup {
    Reuse(Value),
    NoMatch,
    Fatal(String),
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012
/// @pseudocode lines 5-7
fn reusable_evaluation(
    binding: &PrFollowupBinding,
    item: &FeedbackItem,
    state_entries: &[Value],
) -> ReuseLookup {
    let mut matching = Vec::new();
    let mut stale_or_malformed = Vec::new();
    for entry in state_entries {
        let Some(object) = entry.as_object() else {
            stale_or_malformed.push("non_object_state_entry".to_string());
            continue;
        };
        let key_matches = object.get("stable_marker_key").and_then(Value::as_str)
            == Some(item.stable_marker_key.as_str());
        if !key_matches {
            continue;
        }
        let body_matches =
            object.get("body_hash").and_then(Value::as_str) == Some(item.body_hash.as_str());
        let head_matches =
            object.get("head_sha").and_then(Value::as_str) == Some(item.head_sha.as_str());
        if !(body_matches && head_matches) {
            stale_or_malformed.push("stale_state_for_current_marker_key".to_string());
            continue;
        }
        matching.push(entry.clone());
    }
    if matching.len() > 1 {
        return ReuseLookup::Fatal("duplicate_accepted_evaluation_state".to_string());
    }
    if let Some(entry) = matching.pop() {
        if entry.get("reuse_eligible").and_then(Value::as_bool) != Some(true) {
            return ReuseLookup::NoMatch;
        }
        let Some(mut accepted) = entry.get("accepted_evaluation").cloned() else {
            return ReuseLookup::Fatal("missing_accepted_evaluation".to_string());
        };
        if validate_reusable_accepted(binding, item, &accepted).is_err() {
            return ReuseLookup::Fatal("malformed_or_unbindable_accepted_evaluation".to_string());
        }
        apply_low_confidence_accepted_policy(
            &item.body,
            &item.author_login,
            item.author_kind.as_deref(),
            &mut accepted,
        );
        return ReuseLookup::Reuse(accepted);
    }
    ReuseLookup::NoMatch
}

#[cfg(test)]
mod tests;
