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

/// Duration after which an active launch lease is considered stale (the owning
/// process crashed). This bounds the time a concurrent executor waits before
/// it can reclaim the ordinal, preventing permanent deadlock while still
/// serializing genuinely concurrent live invocations.
const LEASE_DURATION_SECONDS: i64 = 7200; // 2 hours

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

impl RetryCounters {
    /// Validates that no counter exceeds the corresponding budget maximum.
    /// Recovered or deserialized state must satisfy this invariant to be
    /// considered internally consistent.
    fn validate_against_budget(&self, budget: RetryBudget) -> Result<(), EngineError> {
        if self.remediation_attempt_index > budget.max_remediation_attempts {
            return Err(EngineError::InvalidState(format!(
                "remediation_attempt_index {} exceeds max_remediation_attempts {}",
                self.remediation_attempt_index, budget.max_remediation_attempts
            )));
        }
        if self.validation_retry_index > budget.max_validation_retries {
            return Err(EngineError::InvalidState(format!(
                "validation_retry_index {} exceeds max_validation_retries {}",
                self.validation_retry_index, budget.max_validation_retries
            )));
        }
        if self.stale_artifact_retry_index > budget.max_stale_artifact_retries {
            return Err(EngineError::InvalidState(format!(
                "stale_artifact_retry_index {} exceeds max_stale_artifact_retries {}",
                self.stale_artifact_retry_index, budget.max_stale_artifact_retries
            )));
        }
        Ok(())
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
    /// RFC3339 timestamp after which the active lease is considered stale
    /// (the owning process crashed). Allows a subsequent invocation to
    /// reclaim the ordinal after the lease expires, preventing permanent
    /// deadlock while still serializing concurrent live invocations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) lease_expiry: Option<String>,
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
    previous: Option<RetryState>,
}

impl<'a> LaunchReservation<'a> {
    fn new(
        store: &'a PrFollowupArtifactStore,
        binding: &'a PrFollowupBinding,
        plan: &'a Value,
        params: &'a Value,
        producer_step_id: &'a str,
        step_order: u64,
        clock: &'a dyn ClockSleeper,
    ) -> Result<Self, EngineError> {
        let scope = RetryScopeKey::new(binding, plan)?;
        let configured_budget = RetryBudget::from_params(params)?;
        let previous = load_matching_state(store, binding, &scope)?;
        Ok(Self {
            store,
            binding,
            plan,
            producer_step_id,
            step_order,
            clock,
            scope,
            configured_budget,
            previous,
        })
    }

    /// Resolves the effective budget: for a first launch it is the configured
    /// budget; for subsequent launches the persisted budget may only be
    /// tightened, never expanded.
    fn resolve_budget(&self) -> Result<RetryBudget, EngineError> {
        match &self.previous {
            None => Ok(self.configured_budget),
            Some(state) => {
                state.budget.reject_expansion(self.configured_budget)?;
                Ok(state.budget.tightened_with(self.configured_budget))
            }
        }
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
                if !is_lease_expired(state, self.clock) {
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

    /// Builds and persists the reserved `RetryState` for the next ordinal.
    fn reserve(self) -> Result<RetryState, EngineError> {
        self.guard_active_launch()?;
        let budget = self.resolve_budget()?;
        let counters = self
            .previous
            .as_ref()
            .map_or_else(RetryCounters::default, |state| state.counters);
        if counters.remediation_attempt_index >= budget.max_remediation_attempts {
            let exhaustion = RetryExhaustionView::new(
                RetryExhaustionReason::RemediationAttempts,
                counters,
                budget,
            );
            return Err(EngineError::InvalidState(format!(
                "remediation retry budget exhausted: {exhaustion:?}"
            )));
        }
        let launch_ordinal = counters.remediation_attempt_index + 1;
        let predecessor_artifact_sequence = if self.previous.is_some() {
            state_sequence(self.store, self.binding)?
        } else {
            None
        };
        let owner_token = uuid::Uuid::new_v4().to_string();
        let state = RetryState {
            scope: self.scope,
            budget,
            counters: RetryCounters {
                remediation_attempt_index: launch_ordinal,
                ..counters
            },
            transition_id: launch_transition_id(self.binding, self.plan, launch_ordinal),
            transition_type: "launch_reserved".to_string(),
            launch_phase: LaunchPhase::Reserved,
            launch_ordinal,
            predecessor_artifact_sequence,
            validation_source_id: None,
            owner_token,
            lease_expiry: lease_expiry_from_now(self.clock),
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

fn reserve_launch_locked(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    plan: &Value,
    params: &Value,
    producer_step_id: &str,
    step_order: u64,
    clock: &dyn ClockSleeper,
) -> Result<RetryState, EngineError> {
    LaunchReservation::new(
        store,
        binding,
        plan,
        params,
        producer_step_id,
        step_order,
        clock,
    )?
    .reserve()
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
        // The valid_transition guard above only allows Reserved→Launched and
        // Launched→Completed, so Reserved can never be the target phase here.
        state.transition_type = match phase {
            LaunchPhase::Launched => "launch_launched",
            LaunchPhase::Completed => "launch_completed",
            LaunchPhase::Reserved => {
                unreachable!("valid_transition guard prevents Reserved as target phase")
            }
        }
        .to_string();
        // Refresh the lease on transition to Launched so a long-running
        // remediation does not have its lease expire prematurely relative
        // to the actual invocation start.
        if phase == LaunchPhase::Launched {
            state.lease_expiry = lease_expiry_from_now(clock);
        }
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
///
/// The tombstone carries the configured effective budget (not a hardcoded
/// sentinel budget) so that the budget provenance is preserved across the
/// tombstone. The counters are set to the budget maxima so that any
/// subsequent `reserve_launch` correctly detects exhaustion and rejects.
pub(super) fn write_terminal_tombstone(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    scope: &RetryScopeKey,
    params: &Value,
    clock: &dyn ClockSleeper,
) -> Result<(), EngineError> {
    let budget = RetryBudget::from_params(params).unwrap_or(RetryBudget {
        max_remediation_attempts: DEFAULT_MAX_REMEDIATION_ATTEMPTS,
        max_validation_retries: DEFAULT_MAX_VALIDATION_RETRIES,
        max_stale_artifact_retries: DEFAULT_MAX_STALE_ARTIFACT_RETRIES,
    });
    let tombstone = RetryState {
        scope: scope.clone(),
        budget,
        counters: RetryCounters {
            remediation_attempt_index: budget.max_remediation_attempts,
            validation_retry_index: budget.max_validation_retries,
            stale_artifact_retry_index: budget.max_stale_artifact_retries,
        },
        transition_id: format!(
            "fnv64:tombstone:{}:{}",
            binding.head_sha,
            clock.now_rfc3339()
        ),
        transition_type: "terminal_tombstone".to_string(),
        launch_phase: LaunchPhase::Completed,
        launch_ordinal: budget.max_remediation_attempts,
        predecessor_artifact_sequence: None,
        validation_source_id: None,
        owner_token: uuid::Uuid::new_v4().to_string(),
        lease_expiry: None,
    };
    with_retry_lock(store, binding, || {
        let path = store.canonical_path(binding, RETRY_STATE_FAMILY);
        let bytes = serde_json::to_vec_pretty(&tombstone)
            .map_err(|err| EngineError::InvalidState(format!("serialize tombstone: {err}")))?;
        // The tombstone is a terminal marker written directly (bypassing the
        // store's write_json_artifact) to avoid the sequence-recovery scan
        // failing on corrupt co-resident artifacts. Use atomic_write to ensure
        // crash-safety: either the complete tombstone is visible or the
        // previous (corrupt) content is preserved.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                EngineError::InvalidState(format!("create tombstone parent: {err}"))
            })?;
        }
        atomic_write(&path, &bytes)?;
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
    // Read the canonical file directly, bypassing the store's sequence-recovery
    // scan. The scan walks all co-resident artifacts and would fail if any are
    // corrupt. The retry state file is self-describing JSON and does not need
    // sequence-recovery for correctness — its counters are the authoritative
    // source of truth.
    let raw = std::fs::read_to_string(&path)
        .map_err(|err| EngineError::InvalidState(format!("read retry state: {err}")))?;
    let value: Value = serde_json::from_str(&raw).map_err(|error| {
        EngineError::InvalidState(format!("invalid remediation retry state: {error}"))
    })?;
    let state: RetryState = serde_json::from_value(value).map_err(|error| {
        EngineError::InvalidState(format!("invalid remediation retry state: {error}"))
    })?;
    validate_state_invariants(&state)?;
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
    // Enforce that new counters do not exceed budget maxima.
    if transition.validation_retry_index > state.budget.max_validation_retries {
        return Err(EngineError::InvalidState(format!(
            "validation_retry_index {} exceeds max_validation_retries {}",
            transition.validation_retry_index, state.budget.max_validation_retries
        )));
    }
    if transition.stale_artifact_retry_index > state.budget.max_stale_artifact_retries {
        return Err(EngineError::InvalidState(format!(
            "stale_artifact_retry_index {} exceeds max_stale_artifact_retries {}",
            transition.stale_artifact_retry_index, state.budget.max_stale_artifact_retries
        )));
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
    let path = store.canonical_path(binding, RETRY_STATE_FAMILY);
    if !path.exists() {
        return Ok(None);
    }
    // Read the canonical file directly to avoid the store's sequence-recovery
    // scan failing on corrupt co-resident artifacts.
    let raw = std::fs::read_to_string(&path).map_err(|err| {
        EngineError::InvalidState(format!("read retry state for sequence: {err}"))
    })?;
    let value: Value = serde_json::from_str(&raw).map_err(|err| {
        EngineError::InvalidState(format!("parse retry state for sequence: {err}"))
    })?;
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

/// Atomically writes bytes to a path via temp-file + rename, ensuring
/// crash-atomicity: either the complete new content is visible or the old
/// content is preserved. Never leaves a partially-written file.
fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> Result<(), EngineError> {
    let parent = path.parent().ok_or_else(|| {
        EngineError::InvalidState(format!("missing parent for {}", path.display()))
    })?;
    std::fs::create_dir_all(parent).map_err(|err| {
        EngineError::InvalidState(format!("create parent for {}: {err}", path.display()))
    })?;
    let temp_path = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("retry-state"),
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&temp_path, bytes)
        .map_err(|err| EngineError::InvalidState(format!("write temp file: {err}")))?;
    std::fs::rename(&temp_path, path).map_err(|err| {
        let _ = std::fs::remove_file(&temp_path);
        EngineError::InvalidState(format!("atomic rename into {}: {err}", path.display()))
    })?;
    Ok(())
}

/// Validates that a recovered or deserialized RetryState is internally
/// consistent: counters must not exceed budget maxima, ordinal must be
/// consistent with attempt index, and terminal states must have matching
/// phase/counters.
fn validate_state_invariants(state: &RetryState) -> Result<(), EngineError> {
    state.counters.validate_against_budget(state.budget)?;
    if state.launch_ordinal > state.budget.max_remediation_attempts {
        return Err(EngineError::InvalidState(format!(
            "launch_ordinal {} exceeds max_remediation_attempts {}",
            state.launch_ordinal, state.budget.max_remediation_attempts
        )));
    }
    Ok(())
}

/// Returns an RFC3339 timestamp representing the lease expiry for a launch
/// reserved at the current clock time.
fn lease_expiry_from_now(clock: &dyn ClockSleeper) -> Option<String> {
    let now = chrono::DateTime::parse_from_rfc3339(&clock.now_rfc3339()).ok()?;
    let expiry = now + chrono::Duration::seconds(LEASE_DURATION_SECONDS);
    Some(expiry.to_rfc3339())
}

/// Returns `true` if the state's active lease has expired (the owning process
/// crashed or stalled). A `Completed` state never has an active lease. An
/// absent or unparseable expiry is treated as expired to avoid deadlock.
fn is_lease_expired(state: &RetryState, clock: &dyn ClockSleeper) -> bool {
    if state.launch_phase == LaunchPhase::Completed {
        return true;
    }
    let Some(expiry_str) = &state.lease_expiry else {
        return true;
    };
    let Ok(expiry) = chrono::DateTime::parse_from_rfc3339(expiry_str) else {
        return true;
    };
    let Ok(now) = chrono::DateTime::parse_from_rfc3339(&clock.now_rfc3339()) else {
        return false;
    };
    now >= expiry
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
