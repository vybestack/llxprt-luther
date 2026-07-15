//! Durable, engine-owned retry accounting for PR remediation.
//!
//! ## Concurrency model
//!
//! Launch ordinals are allocated atomically under the artifact store's
//! per-binding publication lock during `reserve_launch`. The lock combines an
//! in-process mutex with an advisory file lock on `.publication-lock`, and is
//! held across history recovery, reservation, and immutable-history/canonical
//! publication. A unique `owner_token` (UUID) is minted at reservation time and
//! persisted in the retry state. All subsequent phase transitions
//! (`record_launch_phase`, `record_validation`) acquire the same lock and verify
//! that the persisted state's `owner_token` matches the caller's in-memory
//! token before writing. This CAS-via-lock guarantee ensures exactly one
//! executor can advance a given launch across threads and cooperating
//! processes; a stale owner is rejected by the token mismatch.
//!
//! ## Budget policy
//!
//! The retry budget has one authoritative source: the first durable write
//! establishes the configured budget, and subsequent writes may only tighten
//! (reduce) it. Expansion is rejected. The budget is persisted in the
//! `RetryState` and compared against the configured budget on every write.

mod parameters;
mod persistence;

use self::{parameters::parameter, persistence::launch_transition_id};
pub(super) use persistence::{fnv64, persist};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::engine::executors::pr_followup_artifacts::{
    ArtifactWriteContext, ClockSleeper, JsonArtifactWriteRequest, PrFollowupArtifactStore,
    TerminalArtifactPublication,
};
use crate::engine::executors::pr_followup_types::PrFollowupBinding;
use crate::engine::runner::EngineError;

pub(super) use super::retry_history::load_current_state;
use super::retry_history::{load_matching_state_locked, state_sequence_locked};
use super::retry_lease::{
    is_lease_expired, lease_expiry_from_now, DEFAULT_INVOCATION_TIMEOUT_SECONDS,
};
#[cfg(test)]
pub(super) use super::retry_validation_transitions::record_validation;
pub(super) use super::retry_validation_transitions::{
    causal_exhaustion, project_retry_state, record_validation_and_publish,
    PublishedValidatedResult, RecordValidationContext, ValidatedResultPublication,
    ValidationTransition,
};

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
        if !params.is_object() {
            return Err(EngineError::InvalidState(
                "remediation retry parameters must be a JSON object".to_string(),
            ));
        }
        let budget = Self {
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
        };
        if budget.max_remediation_attempts == 0
            || budget.max_validation_retries == 0
            || budget.max_stale_artifact_retries == 0
        {
            return Err(EngineError::InvalidState(
                "remediation retry budgets must be greater than zero".to_string(),
            ));
        }
        Ok(budget)
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

    fn reconcile(
        self,
        configured: Self,
        counters: RetryCounters,
        prior_reason: Option<RetryExhaustionReason>,
    ) -> Result<(Self, Option<RetryExhaustionReason>), EngineError> {
        self.reject_expansion(configured)?;
        let requested = self.tightened_with(configured);
        let reason = prior_reason.or_else(|| counters.exhaustion_reason_against(requested));
        Ok((
            Self {
                max_remediation_attempts: requested
                    .max_remediation_attempts
                    .max(counters.remediation_attempt_index),
                max_validation_retries: requested
                    .max_validation_retries
                    .max(counters.validation_retry_index),
                max_stale_artifact_retries: requested
                    .max_stale_artifact_retries
                    .max(counters.stale_artifact_retry_index),
            },
            reason,
        ))
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct RetryCounters {
    pub(super) remediation_attempt_index: u64,
    pub(super) validation_retry_index: u64,
    pub(super) stale_artifact_retry_index: u64,
}

impl RetryCounters {
    fn exhaustion_reason_against(self, budget: RetryBudget) -> Option<RetryExhaustionReason> {
        if self.validation_retry_index >= budget.max_validation_retries {
            Some(RetryExhaustionReason::ValidationRetries)
        } else if self.stale_artifact_retry_index >= budget.max_stale_artifact_retries {
            Some(RetryExhaustionReason::StaleArtifactRetries)
        } else {
            None
        }
    }
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
    #[serde(default)]
    pub(super) launch_transition_id: String,
    pub(super) transition_type: String,
    pub(super) launch_phase: LaunchPhase,
    pub(super) launch_ordinal: u64,
    #[serde(default)]
    pub(super) remediation_step_order_index: u64,
    pub(super) predecessor_artifact_sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(super) history_chain_reset: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) validation_source_id: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(super) launch_result_promoted: bool,
    /// Unique owner identity for this launch. Persisted at reservation time
    /// and verified via CAS on every subsequent write.
    #[serde(default)]
    pub(super) owner_token: String,
    /// RFC3339 timestamp after which the active lease is considered stale
    /// (the owning process crashed). Allows a subsequent invocation to
    /// reclaim the ordinal after the lease expires, preventing permanent
    /// deadlock while still serializing concurrent live invocations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) lease_expiry: Option<String>,
    /// Invocation timeout used to derive the lease. A live invocation may not
    /// be reclaimed before its runner timeout plus the lease grace period.
    #[serde(default)]
    pub(super) invocation_timeout_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) exhaustion_reason: Option<RetryExhaustionReason>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RetryExhaustionReason {
    RemediationAttempts,
    ValidationRetries,
    StaleArtifactRetries,
}

fn is_false(value: &bool) -> bool {
    !*value
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

/// Serializes retry-state reads and mutations with every artifact publication
/// for the binding. One lock domain prevents terminal selection from observing
/// a retry snapshot that changes before idempotency and publication complete.
pub(super) fn with_retry_lock<R>(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    action: impl FnOnce() -> Result<R, EngineError>,
) -> Result<R, EngineError> {
    store.with_binding_publication_lock(binding, action)
}

pub(super) fn reconcile_retry_policy_locked(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    producer_step_id: &str,
    step_order: u64,
    state: &mut RetryState,
    configured: RetryBudget,
    clock: &dyn ClockSleeper,
) -> Result<(), EngineError> {
    let (budget, exhaustion_reason) =
        state
            .budget
            .reconcile(configured, state.counters, state.exhaustion_reason)?;
    if state.budget == budget && state.exhaustion_reason == exhaustion_reason {
        return Ok(());
    }
    state.predecessor_artifact_sequence = state_sequence_locked(store, binding)?;
    state.budget = budget;
    state.exhaustion_reason = exhaustion_reason;
    state.transition_type = if exhaustion_reason.is_some() {
        "policy_tightened_exhausted"
    } else {
        "policy_tightened"
    }
    .to_string();
    persist(store, binding, producer_step_id, step_order, state, clock)
}

pub(super) fn durable_launch_transition_id(state: &RetryState) -> &str {
    if state.launch_transition_id.is_empty() {
        &state.transition_id
    } else {
        &state.launch_transition_id
    }
}

pub(super) fn exhausted_error(state: &RetryState) -> Option<EngineError> {
    state.exhaustion_reason.map(|reason| {
        let exhaustion = RetryExhaustionView::new(reason, state.counters, state.budget);
        EngineError::InvalidState(format!(
            "remediation retry budget exhausted: {exhaustion:?}"
        ))
    })
}

fn result_is_validated(result: &Value) -> bool {
    result.get("validation_source_id").is_some()
        || result
            .get("validation_state")
            .and_then(Value::as_str)
            .is_some_and(|state| state != "unvalidated")
}

fn result_matches_unvalidated_launch(result: &Value, state: &RetryState) -> bool {
    if result_is_validated(result) {
        return false;
    }
    let raw = result.get("history_metadata").is_none() && result.get("producer_step_id").is_none();
    if raw {
        return state.launch_result_promoted;
    }
    let direct_match = result
        .get("retry_launch_transition_id")
        .and_then(Value::as_str)
        == Some(durable_launch_transition_id(state))
        && result.get("retry_launch_ordinal").and_then(Value::as_u64) == Some(state.launch_ordinal);
    let compatible_wrapper_match = result
        .pointer("/retry_scope/remediation_attempt_index")
        .and_then(Value::as_u64)
        == Some(state.launch_ordinal)
        && result.get("validation_state").and_then(Value::as_str) == Some("unvalidated")
        && result.get("producer_step_id").and_then(Value::as_str) == Some("remediate_pr_followup");
    direct_match || compatible_wrapper_match
}

#[derive(Clone, Copy)]
pub(super) struct LaunchReservationRequest<'a> {
    pub(super) store: &'a PrFollowupArtifactStore,
    pub(super) binding: &'a PrFollowupBinding,
    pub(super) plan: &'a Value,
    pub(super) params: &'a Value,
    pub(super) producer_step_id: &'a str,
    pub(super) step_order: u64,
    pub(super) clock: &'a dyn ClockSleeper,
}

pub(super) enum LaunchReservationOutcome<R> {
    Recovered,
    Reserved(Box<RetryState>, R),
}

pub(super) fn reserve_launch_with_baseline<R>(
    request: LaunchReservationRequest<'_>,
    capture_baseline: impl FnOnce(&RetryState) -> Result<R, EngineError>,
) -> Result<LaunchReservationOutcome<R>, EngineError> {
    with_retry_lock(request.store, request.binding, || {
        let mut reservation = LaunchReservation::new(request)?;
        reservation.prepare()?;
        if reservation.recover_agent_result_after_crash()? {
            return Ok(LaunchReservationOutcome::Recovered);
        }
        let state = reservation.reserve_prepared()?;
        let baseline = capture_baseline(&state)?;
        Ok(LaunchReservationOutcome::Reserved(
            Box::new(state),
            baseline,
        ))
    })
}

#[cfg(test)]
pub(super) fn reserve_launch(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    plan: &Value,
    params: &Value,
    producer_step_id: &str,
    step_order: u64,
    clock: &dyn ClockSleeper,
) -> Result<RetryState, EngineError> {
    let request = LaunchReservationRequest {
        store,
        binding,
        plan,
        params,
        producer_step_id,
        step_order,
        clock,
    };
    with_retry_lock(store, binding, || {
        let mut reservation = LaunchReservation::new(request)?;
        reservation.prepare()?;
        reservation.reserve_prepared()
    })
}

/// Context for reserving a remediation launch ordinal under the per-scope
/// lock. Holds all inputs and the resolved scope/budget/previous state so
/// the orchestration logic reads cleanly without re-deriving intermediate
/// values.
struct LaunchReservation<'a> {
    store: &'a PrFollowupArtifactStore,
    binding: &'a PrFollowupBinding,
    plan: &'a Value,
    producer_step_id: &'a str,
    step_order: u64,
    clock: &'a dyn ClockSleeper,
    scope: RetryScopeKey,
    configured_budget: RetryBudget,
    invocation_timeout_seconds: u64,
    previous: Option<RetryState>,
}

impl<'a> LaunchReservation<'a> {
    fn new(request: LaunchReservationRequest<'a>) -> Result<Self, EngineError> {
        let LaunchReservationRequest {
            store,
            binding,
            plan,
            params,
            producer_step_id,
            step_order,
            clock,
        } = request;
        let scope = RetryScopeKey::new(binding, plan)?;
        let configured_budget = RetryBudget::from_params(params)?;
        let invocation_timeout_seconds = parameter(
            params,
            "timeout_seconds",
            DEFAULT_INVOCATION_TIMEOUT_SECONDS,
        )?;
        if invocation_timeout_seconds == 0 {
            return Err(EngineError::InvalidState(
                "remediation timeout_seconds must be greater than zero".to_string(),
            ));
        }
        let previous = load_matching_state_locked(store, binding, &scope)?;
        Ok(Self {
            store,
            binding,
            plan,
            producer_step_id,
            step_order,
            clock,
            scope,
            configured_budget,
            invocation_timeout_seconds,
            previous,
        })
    }

    fn reconcile_previous(&mut self) -> Result<(), EngineError> {
        let Some(previous) = self.previous.as_mut() else {
            return Ok(());
        };
        reconcile_retry_policy_locked(
            self.store,
            self.binding,
            self.producer_step_id,
            self.step_order,
            previous,
            self.configured_budget,
            self.clock,
        )
    }

    fn resolve_budget(&self) -> RetryBudget {
        self.previous
            .as_ref()
            .map_or(self.configured_budget, |state| state.budget)
    }

    /// If a previous state is active (Reserved/Launched) and its lease has not
    /// expired, returns an error to serialize concurrent executors. A stale
    /// lease (crashed process) or Completed phase falls through to allow the
    /// next ordinal.
    fn guard_active_launch(&self) -> Result<(), EngineError> {
        let Some(state) = self.previous.as_ref() else {
            return Ok(());
        };
        match state.launch_phase {
            LaunchPhase::Reserved | LaunchPhase::Launched => {
                if !is_lease_expired(state, self.clock)? {
                    return Err(EngineError::InvalidState(format!(
                        "remediation launch already in progress for scope \
                         (run_id={}, pr={}, head_sha={}): ordinal={}, \
                         phase={:?}, owner_token={}",
                        self.scope.run_id,
                        self.scope.pr_number,
                        self.scope.input_head_sha,
                        state.launch_ordinal,
                        state.launch_phase,
                        state.owner_token
                    )));
                }
                // Lease expired — the previous process crashed. Fall through
                // to allocate the next ordinal from the last persisted
                // counters. The crashed ordinal is already counted in
                // remediation_attempt_index, so the next ordinal is
                // counters.remediation_attempt_index + 1.
                Ok(())
            }
            LaunchPhase::Completed => Ok(()),
        }
    }

    fn reject_exhausted(&self) -> Result<(), EngineError> {
        if let Some(error) = self.previous.as_ref().and_then(exhausted_error) {
            return Err(error);
        }
        Ok(())
    }

    fn prepare(&mut self) -> Result<(), EngineError> {
        self.reconcile_previous()?;
        self.reject_exhausted()
    }

    fn next_launch_ordinal(&self) -> Result<u64, EngineError> {
        self.previous
            .as_ref()
            .map_or_else(RetryCounters::default, |state| state.counters)
            .remediation_attempt_index
            .checked_add(1)
            .ok_or_else(|| {
                EngineError::InvalidState(
                    "remediation launch ordinal overflowed durable retry state".to_string(),
                )
            })
    }

    fn reject_launch_beyond_budget(
        &mut self,
        launch_ordinal: u64,
        budget: RetryBudget,
    ) -> Result<(), EngineError> {
        if launch_ordinal <= budget.max_remediation_attempts {
            return Ok(());
        }
        let previous = self.previous.as_mut().ok_or_else(|| {
            EngineError::InvalidState("first remediation launch exceeds its budget".to_string())
        })?;
        previous.predecessor_artifact_sequence = state_sequence_locked(self.store, self.binding)?;
        previous.transition_type = "next_launch_cap_exhausted".to_string();
        previous.exhaustion_reason = Some(RetryExhaustionReason::RemediationAttempts);
        persist(
            self.store,
            self.binding,
            self.producer_step_id,
            self.step_order,
            previous,
            self.clock,
        )?;
        Err(exhausted_error(previous).expect("exhaustion reason was just persisted"))
    }

    fn recoverable_crash_phase(&mut self) -> Result<Option<LaunchPhase>, EngineError> {
        match self.previous.as_ref() {
            Some(state) if state.launch_phase == LaunchPhase::Completed => {
                if !matches!(
                    state.transition_type.as_str(),
                    "launch_completed" | "policy_tightened"
                ) {
                    return Ok(None);
                }
                let launch_ordinal = self.next_launch_ordinal()?;
                self.reject_launch_beyond_budget(launch_ordinal, self.resolve_budget())?;
                Ok(Some(LaunchPhase::Completed))
            }
            Some(state) if state.launch_phase == LaunchPhase::Launched => {
                if !is_lease_expired(state, self.clock)? {
                    return Ok(None);
                }
                Ok(Some(LaunchPhase::Launched))
            }
            _ => Ok(None),
        }
    }

    fn recover_launch_result(
        &self,
        recoverable_phase: LaunchPhase,
        canonical: Option<&Value>,
        state: &mut RetryState,
    ) -> Result<bool, EngineError> {
        let canonical_launch_matches =
            canonical.is_some_and(|result| result_matches_unvalidated_launch(result, state));
        let launch_result = self.store.read_untrusted_remediation_launch_result(
            self.binding,
            state.launch_ordinal,
            &state.owner_token,
        )?;
        if launch_result.is_some()
            && recoverable_phase == LaunchPhase::Completed
            && !canonical_launch_matches
        {
            return Ok(false);
        }
        if launch_result.is_some() {
            self.store.promote_remediation_launch_result_locked(
                self.binding,
                state.launch_ordinal,
                &state.owner_token,
            )?;
            state.launch_result_promoted = true;
        } else if !canonical_launch_matches
            || (recoverable_phase == LaunchPhase::Launched && !state.launch_result_promoted)
        {
            return Ok(false);
        } else if recoverable_phase == LaunchPhase::Completed {
            let canonical_owner = canonical
                .and_then(|result| result.get("retry_launch_owner_token"))
                .and_then(Value::as_str);
            if canonical_owner != Some(state.owner_token.as_str()) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn persist_recovered_launch(
        &mut self,
        recoverable_phase: LaunchPhase,
        mut state: RetryState,
    ) -> Result<(), EngineError> {
        state.predecessor_artifact_sequence = state_sequence_locked(self.store, self.binding)?;
        state.launch_phase = LaunchPhase::Completed;
        state.transition_type = match recoverable_phase {
            LaunchPhase::Launched => "launch_recovered_from_agent_result",
            LaunchPhase::Completed => "launch_completed_result_recovered",
            LaunchPhase::Reserved => {
                return Err(EngineError::InvalidState(
                    "reserved remediation launch cannot recover an agent result".to_string(),
                ))
            }
        }
        .to_string();
        persist(
            self.store,
            self.binding,
            self.producer_step_id,
            self.step_order,
            &state,
            self.clock,
        )?;
        self.previous = Some(state);
        Ok(())
    }

    fn recover_agent_result_after_crash(&mut self) -> Result<bool, EngineError> {
        let Some(recoverable_phase) = self.recoverable_crash_phase()? else {
            return Ok(false);
        };
        let canonical = self
            .store
            .read_untrusted_current_json(self.binding, "pr-remediation-result")?;
        if canonical.as_ref().is_some_and(result_is_validated) {
            return Ok(false);
        }
        let mut state = self.previous.clone().ok_or_else(|| {
            EngineError::InvalidState("recoverable retry state disappeared".to_string())
        })?;
        if !self.recover_launch_result(recoverable_phase, canonical.as_ref(), &mut state)? {
            return Ok(false);
        }
        self.persist_recovered_launch(recoverable_phase, state)?;
        Ok(true)
    }

    /// Builds and persists the reserved `RetryState` for the next ordinal.
    fn reserve_prepared(mut self) -> Result<RetryState, EngineError> {
        self.guard_active_launch()?;
        let budget = self.resolve_budget();
        let counters = self
            .previous
            .as_ref()
            .map_or_else(RetryCounters::default, |state| state.counters);
        let launch_ordinal = self.next_launch_ordinal()?;
        self.reject_launch_beyond_budget(launch_ordinal, budget)?;
        let predecessor_artifact_sequence = if self.previous.is_some() {
            state_sequence_locked(self.store, self.binding)?
        } else {
            None
        };
        let owner_token = uuid::Uuid::new_v4().to_string();
        let launch_transition_id = launch_transition_id(self.binding, self.plan, launch_ordinal);
        let state = RetryState {
            scope: self.scope,
            budget,
            counters: RetryCounters {
                remediation_attempt_index: launch_ordinal,
                ..counters
            },
            transition_id: launch_transition_id.clone(),
            launch_transition_id,
            transition_type: "launch_reserved".to_string(),
            launch_phase: LaunchPhase::Reserved,
            launch_ordinal,
            remediation_step_order_index: self.step_order,
            predecessor_artifact_sequence,
            history_chain_reset: false,
            validation_source_id: None,
            launch_result_promoted: false,
            owner_token,
            lease_expiry: Some(lease_expiry_from_now(
                self.clock,
                self.invocation_timeout_seconds,
            )?),
            invocation_timeout_seconds: self.invocation_timeout_seconds,
            exhaustion_reason: None,
        };
        persist(
            self.store,
            self.binding,
            self.producer_step_id,
            self.step_order,
            &state,
            self.clock,
        )?;
        Ok(state)
    }
}

pub(super) fn promote_launch_result(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    producer_step_id: &str,
    step_order: u64,
    state: &mut RetryState,
    clock: &dyn ClockSleeper,
) -> Result<bool, EngineError> {
    let expected_token = state.owner_token.clone();
    let expected_ordinal = state.launch_ordinal;
    with_retry_lock(store, binding, || {
        let persisted = load_matching_state_locked(store, binding, &state.scope)?;
        verify_ownership(
            persisted.as_ref(),
            &expected_token,
            LaunchPhase::Launched,
            expected_ordinal,
        )?;
        if !store.promote_remediation_launch_result_locked(
            binding,
            expected_ordinal,
            &expected_token,
        )? {
            return Ok(false);
        }
        state.predecessor_artifact_sequence = state_sequence_locked(store, binding)?;
        state.launch_result_promoted = true;
        state.transition_type = "launch_result_promoted".to_string();
        persist(store, binding, producer_step_id, step_order, state, clock)?;
        Ok(true)
    })
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
        let persisted = load_matching_state_locked(store, binding, &state.scope)?;
        verify_ownership(
            persisted.as_ref(),
            &expected_token,
            expected_phase,
            expected_ordinal,
        )?;
        state.predecessor_artifact_sequence = state_sequence_locked(store, binding)?;
        state.launch_phase = phase;
        // The valid_transition guard above only allows Reserved→Launched and
        // Launched→Completed, so Reserved can never be the target phase here.
        state.transition_type = match phase {
            LaunchPhase::Launched => "launch_launched",
            LaunchPhase::Completed => "launch_completed",
            LaunchPhase::Reserved => {
                return Err(EngineError::InvalidState(
                    "reserved cannot be a launch phase transition target".to_string(),
                ))
            }
        }
        .to_string();
        // Refresh the lease on transition to Launched so a long-running
        // remediation does not have its lease expire prematurely relative
        // to the actual invocation start.
        if phase == LaunchPhase::Launched {
            state.lease_expiry = Some(lease_expiry_from_now(
                clock,
                state.invocation_timeout_seconds,
            )?);
        }
        persist(store, binding, producer_step_id, step_order, state, clock)
    })
}

/// Verifies that the persisted state belongs to the same owner (CAS guard)
/// and is at the expected phase/ordinal. Returns an error if another process
/// has advanced or replaced the state.
pub(super) fn verify_ownership(
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

/// Writes a durable tombstone retry state that exhausts the remediation budget,
/// preventing continuation from gaining new launches. The marker uses the same
/// immutable history-first publication protocol as terminal artifacts, with a
/// resilient sequence high-water scan that cannot reuse corrupt history names.
///
/// The tombstone carries the configured effective budget (not a hardcoded
/// sentinel budget) so that the budget provenance is preserved across the
/// tombstone. The counters are set to the budget maxima so that any
/// subsequent `reserve_launch` correctly detects exhaustion and rejects.
pub(super) fn write_terminal_tombstone_locked(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    scope: &RetryScopeKey,
    params: &Value,
    clock: &dyn ClockSleeper,
) -> Result<RetryState, EngineError> {
    let budget = RetryBudget::from_params(params)?;
    let tombstone_transition_id = format!(
        "fnv64:tombstone:{}:{}",
        binding.head_sha,
        clock.now_rfc3339()
    );
    let tombstone = RetryState {
        scope: scope.clone(),
        budget,
        counters: RetryCounters {
            remediation_attempt_index: budget.max_remediation_attempts,
            validation_retry_index: budget.max_validation_retries,
            stale_artifact_retry_index: budget.max_stale_artifact_retries,
        },
        transition_id: tombstone_transition_id.clone(),
        launch_transition_id: tombstone_transition_id,
        transition_type: "terminal_tombstone".to_string(),
        launch_phase: LaunchPhase::Completed,
        launch_ordinal: budget.max_remediation_attempts,
        remediation_step_order_index: parameter(params, "remediation_step_order_index", 9)?,
        predecessor_artifact_sequence: None,
        history_chain_reset: true,
        validation_source_id: None,
        launch_result_promoted: false,
        owner_token: uuid::Uuid::new_v4().to_string(),
        lease_expiry: None,
        invocation_timeout_seconds: parameter(
            params,
            "timeout_seconds",
            DEFAULT_INVOCATION_TIMEOUT_SECONDS,
        )?,
        exhaustion_reason: Some(RetryExhaustionReason::RemediationAttempts),
    };
    let idempotency_key = format!(
        "retry-tombstone:{}:{}:{}:{}",
        binding.run_id, binding.pr_number, binding.head_sha, scope.remediation_plan_sequence
    );
    let mut payload = serde_json::to_value(&tombstone)
        .map_err(|error| EngineError::InvalidState(format!("serialize tombstone: {error}")))?;
    payload["idempotency_key"] = Value::from(idempotency_key.clone());
    store.publish_terminal_once_locked(&TerminalArtifactPublication {
        binding,
        artifact_family: RETRY_STATE_FAMILY,
        producer_step_id: "post_pr_failure_terminal",
        step_order_index: parameter(params, "step_order_index", 13)?,
        payload: &payload,
        failure_reason: "retry_state_corrupt",
        idempotency_key: &idempotency_key,
        clock,
        allow_distinct_idempotency_keys: true,
    })?;
    load_matching_state_locked(store, binding, scope)?.ok_or_else(|| {
        EngineError::InvalidState(
            "persisted retry tombstone could not be reconstructed".to_string(),
        )
    })
}

#[cfg(test)]
#[path = "retry_state_tests.rs"]
mod tests;
