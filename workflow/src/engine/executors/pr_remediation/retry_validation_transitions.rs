//! Durable validation transitions within a reserved remediation retry launch.

use serde_json::{json, Value};

use crate::engine::executors::pr_followup_artifacts::{
    ArtifactReplayKey, ArtifactWriteContext, ArtifactWriteRecord, ClockSleeper,
    JsonArtifactWriteRequest, PrFollowupArtifactStore,
};
use crate::engine::executors::pr_followup_types::PrFollowupBinding;
use crate::engine::runner::EngineError;

use super::retry_history::{load_matching_state_locked, state_sequence_locked};
use super::retry_state::{
    exhausted_error, fnv64, persist, reconcile_retry_policy_locked, verify_ownership,
    with_retry_lock, LaunchPhase, RetryBudget, RetryCounters, RetryExhaustionReason, RetryScopeKey,
    RetryState,
};

pub(super) struct ValidationTransition<'a> {
    pub(super) source_id: &'a str,
    pub(super) validation_retry_index: u64,
    pub(super) stale_artifact_retry_index: u64,
    pub(super) transition_type: &'a str,
}

pub(super) struct RecordValidationContext<'a> {
    pub(super) store: &'a PrFollowupArtifactStore,
    pub(super) binding: &'a PrFollowupBinding,
    pub(super) producer_step_id: &'a str,
    pub(super) step_order: u64,
    pub(super) params: &'a Value,
    pub(super) clock: &'a dyn ClockSleeper,
    pub(super) after_lock_hook: Option<&'a dyn Fn()>,
    pub(super) after_transition_hook: Option<&'a dyn Fn()>,
}

pub(super) struct ValidatedResultPublication<'a> {
    pub(super) payload: &'a Value,
    pub(super) failure: Option<(&'a str, &'a str, Value)>,
}

pub(super) struct PublishedValidatedResult {
    pub(super) record: ArtifactWriteRecord,
    pub(super) payload: Value,
}

#[derive(Clone, Copy)]
struct ValidationAdvancement {
    validation_retry: bool,
    stale_artifact_retry: bool,
}

struct RetryOwnership {
    scope: RetryScopeKey,
    token: String,
    phase: LaunchPhase,
    ordinal: u64,
}

impl RetryOwnership {
    fn from_state(state: &RetryState) -> Self {
        Self {
            scope: state.scope.clone(),
            token: state.owner_token.clone(),
            phase: state.launch_phase,
            ordinal: state.launch_ordinal,
        }
    }

    fn verify(&self, state: Option<&RetryState>) -> Result<(), EngineError> {
        let state = state.ok_or_else(|| {
            EngineError::InvalidState(
                "retry state vanished between reserve and validation transition".to_string(),
            )
        })?;
        if state.scope != self.scope {
            return Err(EngineError::InvalidState(
                "retry state scope changed before validation transition".to_string(),
            ));
        }
        verify_ownership(Some(state), &self.token, self.phase, self.ordinal)
    }
}

impl ValidationTransition<'_> {
    fn is_idempotent_replay(&self, state: &RetryState) -> Result<bool, EngineError> {
        if state.validation_source_id.as_deref() != Some(self.source_id) {
            return Ok(false);
        }
        if self.validation_retry_index == state.counters.validation_retry_index
            && self.stale_artifact_retry_index == state.counters.stale_artifact_retry_index
            && self.transition_type == state.transition_type
        {
            return Ok(true);
        }
        Err(EngineError::InvalidState(
            "replayed remediation validation source has divergent counters".to_string(),
        ))
    }

    fn validate_against(&self, state: &RetryState) -> Result<(), EngineError> {
        if self.validation_retry_index < state.counters.validation_retry_index
            || self.stale_artifact_retry_index < state.counters.stale_artifact_retry_index
        {
            return Err(EngineError::InvalidState(
                "remediation validation counters cannot decrease".to_string(),
            ));
        }
        RetryCounters {
            remediation_attempt_index: state.counters.remediation_attempt_index,
            validation_retry_index: self.validation_retry_index,
            stale_artifact_retry_index: self.stale_artifact_retry_index,
        }
        .validate_against_budget(state.budget)
    }

    fn advancement_from(&self, state: &RetryState) -> ValidationAdvancement {
        ValidationAdvancement {
            validation_retry: self.validation_retry_index > state.counters.validation_retry_index,
            stale_artifact_retry: self.stale_artifact_retry_index
                > state.counters.stale_artifact_retry_index,
        }
    }

    fn apply(
        &self,
        state: &mut RetryState,
        advancement: ValidationAdvancement,
    ) -> Result<(), EngineError> {
        state.counters.validation_retry_index = advance_counter(
            state.counters.validation_retry_index,
            advancement.validation_retry,
            "validation retry",
        )?;
        state.counters.stale_artifact_retry_index = advance_counter(
            state.counters.stale_artifact_retry_index,
            advancement.stale_artifact_retry,
            "stale artifact retry",
        )?;
        if state.counters.validation_retry_index != self.validation_retry_index
            || state.counters.stale_artifact_retry_index != self.stale_artifact_retry_index
        {
            return Err(EngineError::InvalidState(
                "remediation validation counter transition must advance by at most one".to_string(),
            ));
        }
        state.counters.validate_against_budget(state.budget)?;
        state.validation_source_id = Some(self.source_id.to_string());
        state.transition_id = format!("fnv64:{:016x}", fnv64(self.source_id.as_bytes()));
        state.transition_type = self.transition_type.to_string();
        if let Some(reason) = exhaustion_reason_for_transition(self.transition_type) {
            state.exhaustion_reason = Some(reason);
        }
        Ok(())
    }
}

fn advance_counter(current: u64, advances: bool, name: &str) -> Result<u64, EngineError> {
    current
        .checked_add(u64::from(advances))
        .ok_or_else(|| EngineError::InvalidState(format!("{name} counter overflowed")))
}

fn advance_validation_locked(
    context: &RecordValidationContext<'_>,
    ownership: &RetryOwnership,
    state: &mut RetryState,
    transition: &ValidationTransition<'_>,
) -> Result<(), EngineError> {
    let persisted = load_matching_state_locked(context.store, context.binding, &ownership.scope)?;
    ownership.verify(persisted.as_ref())?;
    let mut next = persisted.ok_or_else(|| {
        EngineError::InvalidState("retry state vanished during validation".to_string())
    })?;
    let idempotent_replay = transition.is_idempotent_replay(&next)?;
    reconcile_retry_policy_locked(
        context.store,
        context.binding,
        context.producer_step_id,
        context.step_order,
        &mut next,
        RetryBudget::from_params(context.params)?,
        context.clock,
    )?;
    if !idempotent_replay {
        if let Some(error) = exhausted_error(&next) {
            *state = next;
            return Err(error);
        }
        transition.validate_against(&next)?;
        let advancement = transition.advancement_from(&next);
        next.predecessor_artifact_sequence = state_sequence_locked(context.store, context.binding)?;
        transition.apply(&mut next, advancement)?;
        persist(
            context.store,
            context.binding,
            context.producer_step_id,
            context.step_order,
            &next,
            context.clock,
        )?;
    }
    *state = next;
    Ok(())
}

#[cfg(test)]
pub(super) fn record_validation(
    context: RecordValidationContext<'_>,
    state: &mut RetryState,
    transition: &ValidationTransition<'_>,
) -> Result<(), EngineError> {
    let ownership = RetryOwnership::from_state(state);
    with_retry_lock(context.store, context.binding, || {
        if let Some(hook) = context.after_lock_hook {
            hook();
        }
        advance_validation_locked(&context, &ownership, state, transition)
    })
}

pub(super) fn record_validation_and_publish(
    context: RecordValidationContext<'_>,
    state: &mut RetryState,
    transition: &ValidationTransition<'_>,
    publication: ValidatedResultPublication<'_>,
) -> Result<PublishedValidatedResult, EngineError> {
    let ownership = RetryOwnership::from_state(state);
    with_retry_lock(context.store, context.binding, || {
        if let Some(hook) = context.after_lock_hook {
            hook();
        }
        advance_validation_locked(&context, &ownership, state, transition)?;
        if let Some(hook) = context.after_transition_hook {
            hook();
        }
        let payload = project_retry_state(publication.payload.clone(), state);
        let record = context.store.write_json_artifact_once_locked(
            JsonArtifactWriteRequest::new(
                ArtifactWriteContext::new(
                    context.binding,
                    "pr-remediation-result",
                    context.producer_step_id,
                    context.step_order,
                    context.clock,
                ),
                &payload,
                publication.failure,
            ),
            ArtifactReplayKey::superseding("validation_source_id", transition.source_id),
        )?;
        Ok(PublishedValidatedResult { record, payload })
    })
}

pub(super) fn project_retry_state(mut result: Value, state: &RetryState) -> Value {
    if !result.is_object() {
        result = json!({});
    }
    let Some(object) = result.as_object_mut() else {
        return result;
    };
    for (field, value) in retry_counter_fields(state) {
        object.insert(field.to_string(), json!(value));
    }
    object.insert(
        "retry_launch_transition_id".to_string(),
        json!(super::retry_state::durable_launch_transition_id(state)),
    );
    object.insert(
        "retry_launch_ordinal".to_string(),
        json!(state.launch_ordinal),
    );
    let scope = object
        .entry("retry_scope".to_string())
        .or_insert_with(|| json!({}));
    if !scope.is_object() {
        *scope = json!({});
    }
    if let Some(scope) = scope.as_object_mut() {
        for (field, value) in retry_counter_fields(state) {
            scope.insert(field.to_string(), json!(value));
        }
    }
    result
}

fn retry_counter_fields(state: &RetryState) -> [(&'static str, u64); 6] {
    [
        (
            "remediation_attempt_index",
            state.counters.remediation_attempt_index,
        ),
        (
            "max_remediation_attempts",
            state.budget.max_remediation_attempts,
        ),
        (
            "validation_retry_index",
            state.counters.validation_retry_index,
        ),
        (
            "max_validation_retries",
            state.budget.max_validation_retries,
        ),
        (
            "stale_artifact_retry_index",
            state.counters.stale_artifact_retry_index,
        ),
        (
            "max_stale_artifact_retries",
            state.budget.max_stale_artifact_retries,
        ),
    ]
}

fn exhaustion_reason_for_transition(transition_type: &str) -> Option<RetryExhaustionReason> {
    match transition_type {
        "malformed_cap_exhausted" => Some(RetryExhaustionReason::ValidationRetries),
        "unsuccessful_remediation_cap_exhausted" => {
            Some(RetryExhaustionReason::RemediationAttempts)
        }
        "stale_artifact_cap_exhausted" => Some(RetryExhaustionReason::StaleArtifactRetries),
        _ => None,
    }
}

pub(super) fn causal_exhaustion(state: &RetryState) -> Option<&'static str> {
    match state.exhaustion_reason {
        Some(RetryExhaustionReason::RemediationAttempts) => Some("remediation_attempts_exhausted"),
        Some(RetryExhaustionReason::ValidationRetries) => Some("validation_retries_exhausted"),
        Some(RetryExhaustionReason::StaleArtifactRetries) => {
            Some("stale_artifact_retries_exhausted")
        }
        None => None,
    }
}
