//! Bridge between remediation-result validation and the durable retry-state
//! store.
//!
//! These helpers translate the ephemeral validation outcome (success, fixable
//! malformed, unsuccessful, stale-scope) into durable retry-counter
//! transitions recorded by `retry_state::record_validation`, and project
//! authoritative engine counters back onto the agent-authored result value
//! so that downstream consumers always see engine-controlled provenance.

use serde_json::{json, Value};

use super::retry_state::{fnv64, record_validation, RetryState, ValidationTransition};
use super::RemediationResultValidation;
use crate::engine::executors::pr_followup_artifacts::{ClockSleeper, PrFollowupArtifactStore};
use crate::engine::executors::pr_followup_types::PrFollowupBinding;
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;

/// Projects authoritative engine retry counters from `state` onto `result`,
/// overwriting any agent-authored values at both the top level and inside the
/// nested `retry_scope` object. This guarantees downstream validation always
/// reads engine-controlled provenance.
pub(super) fn project_engine_retry_state(mut result: Value, state: &RetryState) -> Value {
    if !result.is_object() {
        // An agent-authored result that is not a JSON object cannot carry
        // authoritative counters; replace it with a fresh object so the
        // engine's durable counters are the only source of truth.
        result = json!({});
    }
    let object = result
        .as_object_mut()
        .expect("engine retry projection must be a JSON object");
    object.insert(
        "remediation_attempt_index".to_string(),
        json!(state.counters.remediation_attempt_index),
    );
    object.insert(
        "max_remediation_attempts".to_string(),
        json!(state.budget.max_remediation_attempts),
    );
    object.insert(
        "validation_retry_index".to_string(),
        json!(state.counters.validation_retry_index),
    );
    object.insert(
        "max_validation_retries".to_string(),
        json!(state.budget.max_validation_retries),
    );
    object.insert(
        "stale_artifact_retry_index".to_string(),
        json!(state.counters.stale_artifact_retry_index),
    );
    object.insert(
        "max_stale_artifact_retries".to_string(),
        json!(state.budget.max_stale_artifact_retries),
    );
    let scope = object
        .entry("retry_scope".to_string())
        .or_insert_with(|| json!({}));
    if let Some(scope) = scope.as_object_mut() {
        scope.insert(
            "remediation_attempt_index".to_string(),
            json!(state.counters.remediation_attempt_index),
        );
        scope.insert(
            "max_remediation_attempts".to_string(),
            json!(state.budget.max_remediation_attempts),
        );
        scope.insert(
            "validation_retry_index".to_string(),
            json!(state.counters.validation_retry_index),
        );
        scope.insert(
            "max_validation_retries".to_string(),
            json!(state.budget.max_validation_retries),
        );
        scope.insert(
            "stale_artifact_retry_index".to_string(),
            json!(state.counters.stale_artifact_retry_index),
        );
        scope.insert(
            "max_stale_artifact_retries".to_string(),
            json!(state.budget.max_stale_artifact_retries),
        );
    }
    result
}

/// Derives a deterministic, unforgeable validation source ID from engine-
/// controlled provenance fields. The identity is derived solely from the
/// store-injected immutable sequence fields so that a forged or copied result
/// payload cannot carry a previously-valid source ID to bypass counter
/// advancement.
pub(super) fn remediation_validation_source_id(result: &Value) -> Result<String, EngineError> {
    // Agent-authored `validation_source_id` fields are deliberately ignored:
    // a forged or copied result payload could carry a previously-valid source
    // ID to bypass counter advancement. By deriving solely from the store-
    // injected immutable sequence fields, the identity is deterministic,
    // unforgeable, and stable across re-reads.
    let artifact_sequence = result
        .get("artifact_sequence")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let write_sequence = result
        .get("write_sequence")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let producer_step_id = result
        .get("producer_step_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let identity = format!("engine:{artifact_sequence}:{write_sequence}:{producer_step_id}");
    Ok(format!("fnv64:{:016x}", fnv64(identity.as_bytes())))
}

/// Groups the durable retry-state recording inputs to keep
/// `validate_remediation_result` under the function-size guardrail.
pub(super) struct ValidationRetryContext<'a> {
    pub(super) store: &'a PrFollowupArtifactStore,
    pub(super) binding: &'a PrFollowupBinding,
    pub(super) step_id: &'a str,
    pub(super) step_order: u64,
    pub(super) state: &'a mut RetryState,
    pub(super) validation: &'a RemediationResultValidation,
    pub(super) validation_source_id: &'a str,
    pub(super) clock: &'a dyn ClockSleeper,
}

/// Records the validation transition into the durable retry state. Resolves
/// the effective stale-artifact index from the validation outcome (using the
/// scope's value when stale-scope classification is present, otherwise the
/// state's current value) and delegates to `retry_state::record_validation`.
pub(super) fn record_validation_transition(
    ctx: ValidationRetryContext<'_>,
) -> Result<(), EngineError> {
    let stale_index = ctx
        .validation
        .stale_scope
        .as_ref()
        .map_or(ctx.state.counters.stale_artifact_retry_index, |scope| {
            scope.stale_artifact_retry_index
        });
    let transition = ValidationTransition {
        source_id: ctx.validation_source_id,
        validation_retry_index: ctx.validation.validation_retry_index,
        stale_artifact_retry_index: stale_index,
        transition_type: ctx.validation.state.as_str(),
    };
    record_validation(
        ctx.store,
        ctx.binding,
        ctx.step_id,
        ctx.step_order,
        ctx.state,
        &transition,
        ctx.clock,
    )
}

/// Builds the optional failure tuple for `write_json_artifact` when the
/// validation outcome is fatal, carrying the semantic state, reason, and a
/// diagnostic JSON object with validation details.
pub(super) fn fatal_validation_failure(
    validation: &RemediationResultValidation,
) -> Option<(&str, &str, Value)> {
    (validation.outcome == StepOutcome::Fatal).then(|| {
        (
            validation.state.as_str(),
            validation.failure_reason.as_str(),
            json!({
                "validation_errors": validation.errors,
                "unsuccessful_statuses": validation.unsuccessful_statuses,
                "no_change_after_remediation": validation.no_change_after_remediation,
                "remediation_attempt_index": validation.remediation_attempt_index,
                "max_remediation_attempts": validation.max_remediation_attempts,
                "validation_retry_index": validation.validation_retry_index,
                "max_validation_retries": validation.max_validation_retries
            }),
        )
    })
}
