//! Durable, engine-owned retry accounting for PR remediation.
//!
//! ## Concurrency model
//!
//! Launch ordinals are allocated atomically under a per-scope SQLite lock
//! during `reserve_launch`. A unique `owner_token` (UUID) is minted at
//! reservation time and persisted in the retry state. All subsequent phase
//! transitions (`record_launch_phase`, `record_validation`) acquire the same
//! lock and verify that the persisted state's `owner_token` matches the
//! caller's in-memory token before writing. This CAS-via-lock guarantee
//! ensures exactly one executor can advance a given launch across processes:
//! a concurrent executor that reads a stale `Reserved` state before the first
//! executor writes `Launched` will be rejected by the token mismatch.
//!
//! ## Budget policy
//!
//! The retry budget has one authoritative source: the first durable write
//! establishes the configured budget, and subsequent writes may only tighten
//! (reduce) it. Expansion is rejected. The budget is persisted in the
//! `RetryState` and compared against the configured budget on every write.

use std::time::Duration;

use rusqlite::{Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::engine::executors::pr_followup_artifacts::{
    ArtifactWriter, ClockSleeper, PrFollowupArtifactStore,
};
use crate::engine::executors::pr_followup_types::PrFollowupBinding;
use crate::engine::runner::EngineError;

pub(super) const RETRY_STATE_FAMILY: &str = "pr-remediation-retry-state";

/// Default retry maxima used when parameters are absent.
const DEFAULT_MAX_REMEDIATION_ATTEMPTS: u64 = 2;
const DEFAULT_MAX_VALIDATION_RETRIES: u64 = 2;
const DEFAULT_MAX_STALE_ARTIFACT_RETRIES: u64 = 2;

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

/// The authoritative retry-budget configuration. Values are resolved from a
/// single typed source: the union of explicitly-present parameters, falling
/// back to documented defaults. Once persisted durably, the budget may only
/// be tightened.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct RetryBudget {
    pub(super) max_remediation_attempts: u64,
    pub(super) max_validation_retries: u64,
    pub(super) max_stale_artifact_retries: u64,
}

impl RetryBudget {
    pub(super) fn from_params(params: &Value) -> Result<Self, EngineError> {
        Ok(Self {
            max_remediation_attempts: parameter(
                params,
                "max_remediation_attempts",
                DEFAULT_MAX_REMEDIATION_ATTEMPTS,
            )?,
            max_validation_retries: parameter(
                params,
                "max_validation_retries",
                DEFAULT_MAX_VALIDATION_RETRIES,
            )?,
            max_stale_artifact_retries: parameter(
                params,
                "max_stale_artifact_retries",
                DEFAULT_MAX_STALE_ARTIFACT_RETRIES,
            )?,
        })
    }

    /// Returns the tightened (minimum) budget of `self` and `configured`.
    /// This is the sole mechanism by which a persisted budget may change:
    /// it can only decrease, never increase.
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

    /// Returns `Err` if `configured` expands any dimension of `self`. Used to
    /// detect and reject budget expansion across restarts when the persisted
    /// budget was established from a non-default configured value and the new
    /// configured value is higher. Tightening (configured lower) is always
    /// allowed.
    fn reject_expansion(self, configured: Self) -> Result<(), EngineError> {
        if configured.max_remediation_attempts > self.max_remediation_attempts {
            return Err(EngineError::InvalidState(format!(
                "remediation budget expansion rejected: max_remediation_attempts \
                 persisted={} configured={}",
                self.max_remediation_attempts, configured.max_remediation_attempts
            )));
        }
        if configured.max_validation_retries > self.max_validation_retries {
            return Err(EngineError::InvalidState(format!(
                "remediation budget expansion rejected: max_validation_retries \
                 persisted={} configured={}",
                self.max_validation_retries, configured.max_validation_retries
            )));
        }
        if configured.max_stale_artifact_retries > self.max_stale_artifact_retries {
            return Err(EngineError::InvalidState(format!(
                "remediation budget expansion rejected: max_stale_artifact_retries \
                 persisted={} configured={}",
                self.max_stale_artifact_retries, configured.max_stale_artifact_retries
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct RetryCounters {
    pub(super) remediation_attempt_index: u64,
    pub(super) validation_retry_index: u64,
    pub(super) stale_artifact_retry_index: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum LaunchPhase {
    Reserved,
    Launched,
    Completed,
}

/// Unique ownership token for a launch lifecycle. Minted atomically during
/// `reserve_launch`, persisted in the retry state, and verified on every
/// subsequent transition. Prevents two concurrent executors from advancing
/// the same launch ordinal.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct RetryState {
    pub(super) scope: RetryScopeKey,
    pub(super) budget: RetryBudget,
    pub(super) counters: RetryCounters,
    pub(super) transition_id: String,
    pub(super) transition_type: String,
    pub(super) launch_phase: LaunchPhase,
    pub(super) launch_ordinal: u64,
    pub(super) predecessor_artifact_sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) validation_source_id: Option<String>,
    /// Unique owner identity for this launch. Persisted at reservation time
    /// and verified via CAS on every subsequent write.
    #[serde(default)]
    pub(super) owner_token: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RetryExhaustionReason {
    RemediationAttempts,
    ValidationRetries,
    StaleArtifactRetries,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct RetryExhaustionView {
    reason: RetryExhaustionReason,
    remediation_attempt_index: u64,
    validation_retry_index: u64,
    stale_artifact_retry_index: u64,
    max_remediation_attempts: u64,
    max_validation_retries: u64,
    max_stale_artifact_retries: u64,
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

/// Opens a per-scope SQLite lock at the canonical retry-state path with
/// `.lock.sqlite3` suffix. The lock provides cross-process mutual exclusion
/// for all retry-state mutations.
fn with_retry_lock<R>(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    action: impl FnOnce() -> Result<R, EngineError>,
) -> Result<R, EngineError> {
    let lock_path = store
        .canonical_path(binding, RETRY_STATE_FAMILY)
        .with_extension("lock.sqlite3");
    let mut connection = Connection::open(&lock_path)
        .map_err(|error| EngineError::InvalidState(format!("open retry lock: {error}")))?;
    connection
        .busy_timeout(Duration::from_secs(30))
        .map_err(|error| EngineError::InvalidState(format!("configure retry lock: {error}")))?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| EngineError::InvalidState(format!("acquire retry lock: {error}")))?;
    let result = action();
    if result.is_ok() {
        transaction
            .commit()
            .map_err(|error| EngineError::InvalidState(format!("release retry lock: {error}")))?;
    }
    result
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
    with_retry_lock(store, binding, || {
        reserve_launch_locked(
            store,
            binding,
            plan,
            params,
            producer_step_id,
            step_order,
            clock,
        )
    })
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
        // Reuse the existing Reserved state. The owner_token persisted at
        // reservation time identifies the executor that holds this ordinal.
        // A concurrent executor calling reserve_launch will obtain the same
        // Reserved state and reuse the same owner_token, but the subsequent
        // record_launch_phase call acquires the lock and verifies ownership
        // via CAS — only the first to advance survives.
        return Ok(state.clone());
    }
    let budget = match &previous {
        None => configured_budget,
        Some(state) => {
            // Reject budget expansion: a persisted budget that was established
            // from non-default configured values must not be silently expanded
            // by a restart with higher configured values. Tightening is allowed.
            state.budget.reject_expansion(configured_budget)?;
            state.budget.tightened_with(configured_budget)
        }
    };
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
        owner_token: uuid::Uuid::new_v4().to_string(),
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
    let expected_token = state.owner_token.clone();
    let expected_phase = state.launch_phase;
    let expected_ordinal = state.launch_ordinal;
    with_retry_lock(store, binding, || {
        // CAS: verify the persisted state still belongs to this owner and is
        // at the expected phase. A concurrent executor that stole the ordinal
        // will have a different owner_token and this write is rejected.
        let persisted = load_matching_state(store, binding, &state.scope)?;
        verify_ownership(
            persisted.as_ref(),
            &expected_token,
            expected_phase,
            expected_ordinal,
        )?;
        state.launch_phase = phase;
        state.transition_type = match phase {
            LaunchPhase::Reserved => "launch_reserved",
            LaunchPhase::Launched => "launch_launched",
            LaunchPhase::Completed => "launch_completed",
        }
        .to_string();
        persist(store, binding, producer_step_id, step_order, state, clock)
    })
}

/// Verifies that the persisted state belongs to the same owner (CAS guard)
/// and is at the expected phase/ordinal. Returns an error if another process
/// has advanced or replaced the state.
fn verify_ownership(
    persisted: Option<&RetryState>,
    expected_token: &str,
    expected_phase: LaunchPhase,
    expected_ordinal: u64,
) -> Result<(), EngineError> {
    match persisted {
        None => Err(EngineError::InvalidState(
            "retry state vanished between reserve and phase transition".to_string(),
        )),
        Some(state) => {
            if state.owner_token != expected_token {
                return Err(EngineError::InvalidState(format!(
                    "retry state ownership changed: expected token {expected_token}, \
                     persisted token {}",
                    state.owner_token
                )));
            }
            if state.launch_phase != expected_phase {
                return Err(EngineError::InvalidState(format!(
                    "retry state phase diverged: expected {expected_phase:?}, \
                     persisted {:?}",
                    state.launch_phase
                )));
            }
            if state.launch_ordinal != expected_ordinal {
                return Err(EngineError::InvalidState(format!(
                    "retry state ordinal diverged: expected {expected_ordinal}, \
                     persisted {}",
                    state.launch_ordinal
                )));
            }
            Ok(())
        }
    }
}

pub(super) fn load_current_state(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    scope: &RetryScopeKey,
) -> Result<Option<RetryState>, EngineError> {
    load_matching_state(store, binding, scope)
}

/// Writes a durable tombstone retry state that exhausts the remediation budget,
/// preventing continuation from gaining new launches. Written directly to the
/// canonical path without store sequence recovery, because the store's
/// sequence-recovery scan would fail on the corrupt history that triggered the
/// tombstone. The tombstone is a terminal marker, not a normal sequence-
/// participating artifact.
pub(super) fn write_terminal_tombstone(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    scope: &RetryScopeKey,
    _clock: &dyn ClockSleeper,
) -> Result<(), EngineError> {
    let budget = RetryBudget {
        max_remediation_attempts: 1,
        max_validation_retries: 0,
        max_stale_artifact_retries: 0,
    };
    let tombstone = RetryState {
        scope: scope.clone(),
        budget,
        counters: RetryCounters {
            remediation_attempt_index: 1,
            validation_retry_index: 0,
            stale_artifact_retry_index: 0,
        },
        transition_id: format!("fnv64:tombstone:{}", binding.head_sha),
        transition_type: "terminal_tombstone".to_string(),
        launch_phase: LaunchPhase::Completed,
        launch_ordinal: 1,
        predecessor_artifact_sequence: None,
        validation_source_id: None,
        owner_token: uuid::Uuid::new_v4().to_string(),
    };
    with_retry_lock(store, binding, || {
        let path = store.canonical_path(binding, RETRY_STATE_FAMILY);
        let bytes = serde_json::to_vec_pretty(&tombstone)
            .map_err(|err| EngineError::InvalidState(format!("serialize tombstone: {err}")))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                EngineError::InvalidState(format!("create tombstone parent: {err}"))
            })?;
        }
        std::fs::write(&path, &bytes)
            .map_err(|err| EngineError::InvalidState(format!("write tombstone: {err}")))?;
        Ok(())
    })
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
    pub(super) source_id: &'a str,
    pub(super) validation_retry_index: u64,
    pub(super) stale_artifact_retry_index: u64,
    pub(super) transition_type: &'a str,
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
    // Idempotent replay of the same validation source with matching counters.
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
    let expected_token = state.owner_token.clone();
    let expected_phase = state.launch_phase;
    let expected_ordinal = state.launch_ordinal;
    let expected_validation_source = state.validation_source_id.clone();
    with_retry_lock(store, binding, || {
        let persisted = load_matching_state(store, binding, &state.scope)?;
        verify_ownership(
            persisted.as_ref(),
            &expected_token,
            expected_phase,
            expected_ordinal,
        )?;
        // Also verify the validation source hasn't been advanced by a concurrent
        // write between the initial read and this CAS-protected write.
        if let Some(persisted) = &persisted {
            if persisted.validation_source_id != expected_validation_source {
                return Err(EngineError::InvalidState(
                    "retry state validation source changed under concurrent write".to_string(),
                ));
            }
        }
        state.counters.validation_retry_index = transition.validation_retry_index;
        state.counters.stale_artifact_retry_index = transition.stale_artifact_retry_index;
        state.validation_source_id = Some(transition.source_id.to_string());
        state.transition_id = format!("fnv64:{:016x}", fnv64(transition.source_id.as_bytes()));
        state.transition_type = transition.transition_type.to_string();
        persist(store, binding, producer_step_id, step_order, state, clock)
    })
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

pub(super) fn fnv64(bytes: &[u8]) -> u64 {
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

    #[test]
    fn reject_expansion_errors_on_increase() {
        let persisted = RetryBudget::from_params(&json!({
            "max_remediation_attempts": 2,
            "max_validation_retries": 2,
            "max_stale_artifact_retries": 2
        }))
        .expect("valid persisted budget");
        let expanded = RetryBudget::from_params(&json!({
            "max_remediation_attempts": 3,
            "max_validation_retries": 2,
            "max_stale_artifact_retries": 2
        }))
        .expect("valid expanded budget");
        assert!(persisted.reject_expansion(expanded).is_err());
    }

    #[test]
    fn reject_expansion_allows_tightening() {
        let persisted = RetryBudget::from_params(&json!({
            "max_remediation_attempts": 3,
            "max_validation_retries": 3,
            "max_stale_artifact_retries": 3
        }))
        .expect("valid persisted budget");
        let tightened = RetryBudget::from_params(&json!({
            "max_remediation_attempts": 2,
            "max_validation_retries": 2,
            "max_stale_artifact_retries": 2
        }))
        .expect("valid tightened budget");
        assert!(persisted.reject_expansion(tightened).is_ok());
    }

    #[test]
    fn reject_expansion_allows_equal() {
        let persisted = RetryBudget::from_params(&json!({
            "max_remediation_attempts": 2,
            "max_validation_retries": 2,
            "max_stale_artifact_retries": 2
        }))
        .expect("valid persisted budget");
        let equal = RetryBudget::from_params(&json!({
            "max_remediation_attempts": 2,
            "max_validation_retries": 2,
            "max_stale_artifact_retries": 2
        }))
        .expect("valid equal budget");
        assert!(persisted.reject_expansion(equal).is_ok());
    }
}
