//! Bridge between remediation-result validation and the durable retry-state
//! store.
//!
//! These helpers translate the ephemeral validation outcome (success, fixable
//! malformed, unsuccessful, stale-scope) into durable retry-counter
//! transitions recorded by `retry_state::record_validation`, and project
//! authoritative engine counters back onto the agent-authored result value
//! so that downstream consumers always see engine-controlled provenance.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::retry_state::{
    durable_launch_transition_id, load_current_state, project_retry_state,
    record_validation_and_publish, PublishedValidatedResult, RecordValidationContext,
    RetryScopeKey, RetryState, ValidatedResultPublication, ValidationTransition,
};
use super::{
    artifact_root, binding_for_context, current_step_id, evaluate_remediation_result,
    read_remediation_result_for_validation, remediation_result_payload, remediation_retry_scope,
    u64_param, write_pending_marker_actions_for_fixed_feedback, FixedFeedbackMarkerContext,
    RemediationResultValidation, RemediationResultValidationArtifact,
};
use crate::engine::executor::StepContext;
use crate::engine::executors::pr_followup_artifacts::{
    ArtifactReplayKey, ArtifactWriteContext, ArtifactWriteRecord, ClockSleeper,
    JsonArtifactWriteRequest, PrFollowupArtifactStore, SystemPrFollowupFilesystem,
};
use crate::engine::executors::pr_followup_types::PrFollowupBinding;
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;

/// Projects authoritative engine retry counters from `state` onto `result`.
pub(super) fn project_engine_retry_state(result: Value, state: &RetryState) -> Value {
    project_retry_state(result, state)
}

/// Returns the collision-resistant identity captured from the exact agent
/// result bytes before any normalization or engine-owned metadata projection.
pub(super) fn remediation_validation_source_id(
    result: &Value,
    retry_state: Option<&RetryState>,
) -> Result<String, EngineError> {
    let source_identity = result
        .get("agent_result_source_identity")
        .and_then(Value::as_str)
        .filter(|identity| identity.len() == 71 && identity.starts_with("sha256:"))
        .ok_or_else(|| {
            EngineError::InvalidState(
                "remediation result is missing exact-payload SHA-256 receipt identity".to_string(),
            )
        })?;
    let Some(state) = retry_state else {
        return Ok(source_identity.to_string());
    };
    let mut digest = Sha256::new();
    digest.update(source_identity.as_bytes());
    digest.update([0]);
    digest.update(durable_launch_transition_id(state).as_bytes());
    digest.update([0]);
    digest.update(state.launch_ordinal.to_be_bytes());
    Ok(format!("sha256:{:x}", digest.finalize()))
}

struct PreparedRemediationValidation {
    store: PrFollowupArtifactStore,
    binding: PrFollowupBinding,
    plan: Value,
    result: Value,
    validation_source_id: String,
    retry_state: Option<RetryState>,
    step_id: String,
    step_order: u64,
    validation: RemediationResultValidation,
}

impl PreparedRemediationValidation {
    fn prepare(
        context: &StepContext,
        params: &Value,
        clock: &dyn ClockSleeper,
    ) -> Result<Self, EngineError> {
        let artifact_root = artifact_root(context, params)?;
        let store =
            PrFollowupArtifactStore::with_filesystem(&artifact_root, &SystemPrFollowupFilesystem)?;
        let binding = binding_for_context(context, params, &store, clock)?;
        let plan = store.read_current_json(&binding, "pr-remediation-plan")?;
        let retry_scope = RetryScopeKey::new(&binding, &plan)?;
        let retry_state = load_current_state(&store, &binding, &retry_scope)?;
        let remediation_step_order = retry_state
            .as_ref()
            .map(|state| state.remediation_step_order_index)
            .filter(|step_order| *step_order > 0)
            .unwrap_or_else(|| u64_param(params, "remediation_step_order_index", 9));
        let read = read_remediation_result_for_validation(
            &store,
            &binding,
            &plan,
            retry_state.as_ref(),
            remediation_step_order,
            clock,
        )?;
        let mut result = read.result;
        let validation_source_id = remediation_validation_source_id(&result, retry_state.as_ref())?;
        let advance_validation = retry_state.as_ref().is_none_or(|state| {
            state.validation_source_id.as_deref() != Some(&validation_source_id)
        });
        if let Some(retry_state) = &retry_state {
            result = project_engine_retry_state(result, retry_state);
        }
        if let (Some(object), Some(error)) = (result.as_object_mut(), read.replay_error) {
            object.insert(
                "engine_validated_replay_error".to_string(),
                Value::String(error),
            );
        }
        let step_id = current_step_id(context, "validate_remediation_result");
        let step_order = u64_param(params, "step_order_index", 9);
        let expected_scope = remediation_retry_scope(&binding, &plan, &result, params);
        let validation = evaluate_remediation_result(
            &binding,
            &plan,
            &result,
            &expected_scope,
            retry_state.is_some(),
            advance_validation,
        )?;
        Ok(Self {
            store,
            binding,
            plan,
            result,
            validation_source_id,
            retry_state,
            step_id,
            step_order,
            validation,
        })
    }

    fn publish_result(
        &mut self,
        params: &Value,
        clock: &dyn ClockSleeper,
    ) -> Result<(RemediationResultValidationArtifact, ArtifactWriteRecord), EngineError> {
        let payload = remediation_result_payload(
            &self.binding,
            &self.result,
            &self.validation,
            &self.validation_source_id,
            self.retry_state.as_ref(),
            clock,
        );
        let payload_value = serde_json::to_value(&payload).map_err(|error| {
            EngineError::InvalidState(format!("serialize remediation validation result: {error}"))
        })?;
        let failure = fatal_validation_failure(&self.validation);
        let (payload, result_write_record) = if let Some(state) = &mut self.retry_state {
            let published = record_validation_transition(ValidationRetryContext {
                store: &self.store,
                binding: &self.binding,
                step_id: &self.step_id,
                step_order: self.step_order,
                state,
                validation: &self.validation,
                validation_source_id: &self.validation_source_id,
                params,
                payload: &payload_value,
                failure,
                clock,
            })?;
            let payload = serde_json::from_value(published.payload).map_err(|error| {
                EngineError::InvalidState(format!(
                    "deserialize projected remediation validation result: {error}"
                ))
            })?;
            (payload, published.record)
        } else {
            let record = self.store.write_json_artifact_once(
                JsonArtifactWriteRequest::new(
                    ArtifactWriteContext::new(
                        &self.binding,
                        "pr-remediation-result",
                        &self.step_id,
                        self.step_order,
                        clock,
                    ),
                    &payload,
                    failure,
                ),
                ArtifactReplayKey::new("validation_source_id", &self.validation_source_id),
            )?;
            (payload, record)
        };
        Ok((payload, result_write_record))
    }
}

pub(super) fn validate_remediation_result(
    context: &StepContext,
    params: &Value,
    clock: &dyn ClockSleeper,
) -> Result<StepOutcome, EngineError> {
    let mut prepared = PreparedRemediationValidation::prepare(context, params, clock)?;
    let outcome = prepared.validation.outcome;
    let (payload, result_write_record) = prepared.publish_result(params, clock)?;
    if outcome == StepOutcome::Success {
        write_pending_marker_actions_for_fixed_feedback(&FixedFeedbackMarkerContext {
            store: &prepared.store,
            binding: &prepared.binding,
            step_id: &prepared.step_id,
            step_order: prepared.step_order,
            plan: &prepared.plan,
            validation_payload: &payload,
            result_sequence: &result_write_record.sequence,
            clock,
        })?;
    }

    Ok(outcome)
}

/// Groups the durable retry-state recording inputs used while publishing a
/// validated remediation result.
struct ValidationRetryContext<'a> {
    pub(super) store: &'a PrFollowupArtifactStore,
    pub(super) binding: &'a PrFollowupBinding,
    pub(super) step_id: &'a str,
    pub(super) step_order: u64,
    pub(super) state: &'a mut RetryState,
    pub(super) validation: &'a RemediationResultValidation,
    pub(super) validation_source_id: &'a str,
    pub(super) params: &'a Value,
    pub(super) payload: &'a Value,
    pub(super) failure: Option<(&'a str, &'a str, Value)>,
    pub(super) clock: &'a dyn ClockSleeper,
}

/// Records the validation transition into the durable retry state. Resolves
/// the effective stale-artifact index from the validation outcome (using the
/// scope's value when stale-scope classification is present, otherwise the
/// state's current value) and delegates to `retry_state::record_validation`.
fn record_validation_transition(
    ctx: ValidationRetryContext<'_>,
) -> Result<PublishedValidatedResult, EngineError> {
    let stale_index = ctx
        .validation
        .stale_scope
        .as_ref()
        .map_or(ctx.state.counters.stale_artifact_retry_index, |scope| {
            scope.stale_artifact_retry_index
        });
    let advances_validation =
        ctx.validation.validation_retry_index > ctx.state.counters.validation_retry_index;
    let validation_retry_index = ctx
        .state
        .counters
        .validation_retry_index
        .checked_add(u64::from(advances_validation))
        .ok_or_else(|| {
            EngineError::InvalidState("validation retry counter overflowed".to_string())
        })?;
    let transition = ValidationTransition {
        source_id: ctx.validation_source_id,
        validation_retry_index,
        stale_artifact_retry_index: stale_index,
        transition_type: ctx.validation.state.as_str(),
    };
    record_validation_and_publish(
        RecordValidationContext {
            store: ctx.store,
            binding: ctx.binding,
            producer_step_id: ctx.step_id,
            step_order: ctx.step_order,
            params: ctx.params,
            clock: ctx.clock,
            after_lock_hook: None,
            after_transition_hook: None,
        },
        ctx.state,
        &transition,
        ValidatedResultPublication {
            payload: ctx.payload,
            failure: ctx.failure,
        },
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
                "no_change_after_remediation": validation.no_change_after_remediation
            }),
        )
    })
}
