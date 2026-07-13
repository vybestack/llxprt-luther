//! Durable, engine-owned retry accounting for PR remediation.

use std::time::Duration;

use rusqlite::{Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::engine::executors::pr_followup_artifacts::{
    ArtifactWriter, ClockSleeper, PrFollowupArtifactStore,
};
use crate::engine::executors::pr_followup_types::PrFollowupBinding;
use crate::engine::runner::EngineError;

const RETRY_STATE_FAMILY: &str = "pr-remediation-retry-state";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct RetryScopeKey {
    run_id: String,
    repository_owner: String,
    repository_name: String,
    pr_number: u64,
    input_head_sha: String,
    remediation_plan_sequence: u64,
}

impl RetryScopeKey {
    pub(super) fn new(binding: &PrFollowupBinding, plan: &Value) -> Result<Self, EngineError> {
        let remediation_plan_sequence = plan
            .get("artifact_sequence")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                EngineError::InvalidState(
                    "remediation plan is missing a valid artifact_sequence".to_string(),
                )
            })?;
        Ok(Self {
            run_id: binding.run_id.clone(),
            repository_owner: binding.repository_owner.clone(),
            repository_name: binding.repository_name.clone(),
            pr_number: binding.pr_number,
            input_head_sha: binding.head_sha.clone(),
            remediation_plan_sequence,
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct RetryBudget {
    pub max_remediation_attempts: u64,
    pub max_validation_retries: u64,
    pub max_stale_artifact_retries: u64,
}

impl RetryBudget {
    pub(super) fn from_params(params: &Value) -> Result<Self, EngineError> {
        Ok(Self {
            max_remediation_attempts: parameter(params, "max_remediation_attempts", 2)?,
            max_validation_retries: parameter(params, "max_validation_retries", 2)?,
            max_stale_artifact_retries: parameter(params, "max_stale_artifact_retries", 2)?,
        })
    }

    fn tightened_with(self, configured: Self) -> Self {
        Self {
            max_remediation_attempts: self
                .max_remediation_attempts
                .min(configured.max_remediation_attempts),
            max_validation_retries: self
                .max_validation_retries
                .min(configured.max_validation_retries),
            max_stale_artifact_retries: self
                .max_stale_artifact_retries
                .min(configured.max_stale_artifact_retries),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct RetryCounters {
    pub remediation_attempt_index: u64,
    pub validation_retry_index: u64,
    pub stale_artifact_retry_index: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum LaunchPhase {
    Reserved,
    Launched,
    Completed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct RetryState {
    pub scope: RetryScopeKey,
    pub budget: RetryBudget,
    pub counters: RetryCounters,
    pub transition_id: String,
    pub transition_type: String,
    pub launch_phase: LaunchPhase,
    pub launch_ordinal: u64,
    pub predecessor_artifact_sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_source_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RetryExhaustionReason {
    RemediationAttempts,
    ValidationRetries,
    StaleArtifactRetries,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct RetryExhaustionView {
    pub reason: RetryExhaustionReason,
    pub remediation_attempt_index: u64,
    pub validation_retry_index: u64,
    pub stale_artifact_retry_index: u64,
    pub max_remediation_attempts: u64,
    pub max_validation_retries: u64,
    pub max_stale_artifact_retries: u64,
}

impl RetryExhaustionView {
    fn new(reason: RetryExhaustionReason, counters: RetryCounters, budget: RetryBudget) -> Self {
        Self {
            reason,
            remediation_attempt_index: counters.remediation_attempt_index,
            validation_retry_index: counters.validation_retry_index,
            stale_artifact_retry_index: counters.stale_artifact_retry_index,
            max_remediation_attempts: budget.max_remediation_attempts,
            max_validation_retries: budget.max_validation_retries,
            max_stale_artifact_retries: budget.max_stale_artifact_retries,
        }
    }
}

pub(super) fn reserve_launch(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    plan: &Value,
    params: &Value,
    producer_step_id: &str,
    step_order: u64,
    clock: &dyn ClockSleeper,
) -> Result<RetryState, EngineError> {
    let lock_path = store
        .canonical_path(binding, RETRY_STATE_FAMILY)
        .with_extension("lock.sqlite3");
    let mut connection = Connection::open(lock_path)
        .map_err(|error| EngineError::InvalidState(format!("open retry lock: {error}")))?;
    connection
        .busy_timeout(Duration::from_secs(30))
        .map_err(|error| EngineError::InvalidState(format!("configure retry lock: {error}")))?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| EngineError::InvalidState(format!("acquire retry lock: {error}")))?;
    let state = reserve_launch_locked(
        store,
        binding,
        plan,
        params,
        producer_step_id,
        step_order,
        clock,
    )?;
    transaction
        .commit()
        .map_err(|error| EngineError::InvalidState(format!("release retry lock: {error}")))?;
    Ok(state)
}

fn reserve_launch_locked(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    plan: &Value,
    params: &Value,
    producer_step_id: &str,
    step_order: u64,
    clock: &dyn ClockSleeper,
) -> Result<RetryState, EngineError> {
    let scope = RetryScopeKey::new(binding, plan)?;
    let configured_budget = RetryBudget::from_params(params)?;
    let previous = load_matching_state(store, binding, &scope)?;
    if let Some(state) = previous
        .as_ref()
        .filter(|state| state.launch_phase == LaunchPhase::Reserved)
    {
        return Ok(state.clone());
    }
    let budget = previous.as_ref().map_or(configured_budget, |state| {
        state.budget.tightened_with(configured_budget)
    });
    let counters = previous
        .as_ref()
        .map_or_else(RetryCounters::default, |state| state.counters);
    if counters.remediation_attempt_index >= budget.max_remediation_attempts {
        let exhaustion =
            RetryExhaustionView::new(RetryExhaustionReason::RemediationAttempts, counters, budget);
        return Err(EngineError::InvalidState(format!(
            "remediation retry budget exhausted: {exhaustion:?}"
        )));
    }
    let launch_ordinal = counters.remediation_attempt_index + 1;
    let predecessor_artifact_sequence = if previous.is_some() {
        state_sequence(store, binding)?
    } else {
        None
    };
    let state = RetryState {
        scope,
        budget,
        counters: RetryCounters {
            remediation_attempt_index: launch_ordinal,
            ..counters
        },
        transition_id: launch_transition_id(binding, plan, launch_ordinal),
        transition_type: "launch_reserved".to_string(),
        launch_phase: LaunchPhase::Reserved,
        launch_ordinal,
        predecessor_artifact_sequence,
        validation_source_id: None,
    };
    persist(store, binding, producer_step_id, step_order, &state, clock)?;
    Ok(state)
}

pub(super) fn record_launch_phase(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    producer_step_id: &str,
    step_order: u64,
    state: &mut RetryState,
    phase: LaunchPhase,
    clock: &dyn ClockSleeper,
) -> Result<(), EngineError> {
    let valid_transition = matches!(
        (state.launch_phase, phase),
        (LaunchPhase::Reserved, LaunchPhase::Launched)
            | (LaunchPhase::Launched, LaunchPhase::Completed)
    );
    if !valid_transition {
        return Err(EngineError::InvalidState(format!(
            "invalid remediation launch transition: {:?} -> {:?}",
            state.launch_phase, phase
        )));
    }
    state.launch_phase = phase;
    state.transition_type = match phase {
        LaunchPhase::Reserved => "launch_reserved",
        LaunchPhase::Launched => "launch_launched",
        LaunchPhase::Completed => "launch_completed",
    }
    .to_string();
    persist(store, binding, producer_step_id, step_order, state, clock)?;
    Ok(())
}

pub(super) fn load_current_state(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    scope: &RetryScopeKey,
) -> Result<Option<RetryState>, EngineError> {
    load_matching_state(store, binding, scope)
}

fn load_matching_state(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    scope: &RetryScopeKey,
) -> Result<Option<RetryState>, EngineError> {
    let path = store.canonical_path(binding, RETRY_STATE_FAMILY);
    if !path.exists() {
        return Ok(None);
    }
    let value = store.read_current_raw_json(binding, RETRY_STATE_FAMILY)?;
    let state: RetryState = serde_json::from_value(value).map_err(|error| {
        EngineError::InvalidState(format!("invalid remediation retry state: {error}"))
    })?;
    Ok((state.scope == *scope).then_some(state))
}

pub(super) struct ValidationTransition<'a> {
    pub source_id: &'a str,
    pub validation_retry_index: u64,
    pub stale_artifact_retry_index: u64,
    pub transition_type: &'a str,
}

pub(super) fn record_validation(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    producer_step_id: &str,
    step_order: u64,
    state: &mut RetryState,
    transition: &ValidationTransition<'_>,
    clock: &dyn ClockSleeper,
) -> Result<(), EngineError> {
    if state.validation_source_id.as_deref() == Some(transition.source_id) {
        if transition.validation_retry_index == state.counters.validation_retry_index
            && transition.stale_artifact_retry_index == state.counters.stale_artifact_retry_index
        {
            return Ok(());
        }
        return Err(EngineError::InvalidState(
            "replayed remediation validation source has divergent counters".to_string(),
        ));
    }
    if transition.validation_retry_index < state.counters.validation_retry_index
        || transition.stale_artifact_retry_index < state.counters.stale_artifact_retry_index
    {
        return Err(EngineError::InvalidState(
            "remediation validation counters cannot decrease".to_string(),
        ));
    }
    state.counters.validation_retry_index = transition.validation_retry_index;
    state.counters.stale_artifact_retry_index = transition.stale_artifact_retry_index;
    state.validation_source_id = Some(transition.source_id.to_string());
    state.transition_id = format!("fnv64:{:016x}", fnv64(transition.source_id.as_bytes()));
    state.transition_type = transition.transition_type.to_string();
    persist(store, binding, producer_step_id, step_order, state, clock)
}

fn persist(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    producer_step_id: &str,
    step_order: u64,
    state: &RetryState,
    clock: &dyn ClockSleeper,
) -> Result<(), EngineError> {
    store.write_json_artifact(
        binding,
        RETRY_STATE_FAMILY,
        producer_step_id,
        step_order,
        state,
        None,
        clock,
    )?;
    Ok(())
}

fn state_sequence(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
) -> Result<Option<u64>, EngineError> {
    let value = store.read_current_raw_json(binding, RETRY_STATE_FAMILY)?;
    Ok(value.get("artifact_sequence").and_then(Value::as_u64))
}

fn launch_transition_id(binding: &PrFollowupBinding, plan: &Value, ordinal: u64) -> String {
    let identity = format!(
        "{}:{}/{}:{}:{}:{}:launch:{ordinal}",
        binding.run_id,
        binding.repository_owner,
        binding.repository_name,
        binding.pr_number,
        binding.head_sha,
        plan.get("artifact_sequence")
            .and_then(Value::as_u64)
            .unwrap_or_default()
    );
    format!("fnv64:{:016x}", fnv64(identity.as_bytes()))
}

fn fnv64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn parameter(params: &Value, name: &str, default: u64) -> Result<u64, EngineError> {
    match params.get(name) {
        None => Ok(default),
        Some(value) => value.as_u64().ok_or_else(|| {
            EngineError::InvalidState(format!(
                "remediation retry parameter {name} must be an unsigned integer"
            ))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::{fnv64, RetryBudget};
    use serde_json::json;

    #[test]
    fn configured_budget_can_only_tighten() {
        let persisted = RetryBudget::from_params(&json!({
            "max_remediation_attempts": 3,
            "max_validation_retries": 4,
            "max_stale_artifact_retries": 5
        }))
        .expect("valid persisted budget");
        let expanded = RetryBudget::from_params(&json!({
            "max_remediation_attempts": 9,
            "max_validation_retries": 9,
            "max_stale_artifact_retries": 9
        }))
        .expect("valid expanded budget");
        assert_eq!(persisted.tightened_with(expanded), persisted);
    }

    #[test]
    fn transition_hash_is_stable() {
        assert_eq!(fnv64(b"retry"), 0x163c_a1f2_c427_ff19);
    }
}
