//! Retry-state history reconciliation and recovered-state validation.
//!
//! Immutable history is authoritative when canonical state is absent, corrupt,
//! or behind. Only snapshots forming the predecessor chain for the exact retry
//! scope are eligible for restoration.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;

use crate::engine::executors::pr_followup_artifacts::{
    ArtifactWriter, PrFollowupArtifactStore, RecoverableCurrentArtifact,
    RecoverableHistoryCandidate,
};
use crate::engine::executors::pr_followup_types::PrFollowupBinding;
use crate::engine::runner::EngineError;

use super::retry_state::{
    LaunchPhase, RetryBudget, RetryCounters, RetryScopeKey, RetryState, RETRY_STATE_FAMILY,
};

#[derive(Debug)]
pub(super) struct RetryStateCorruption {
    detail: String,
}

impl RetryStateCorruption {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }

    pub(super) fn into_engine_error(self) -> EngineError {
        EngineError::InvalidState(self.detail)
    }
}

#[derive(Debug)]
pub(super) enum RetryStateLoadError {
    Corruption(RetryStateCorruption),
    Storage(EngineError),
}

impl RetryStateLoadError {
    pub(super) fn into_engine_error(self) -> EngineError {
        match self {
            Self::Corruption(error) => error.into_engine_error(),
            Self::Storage(error) => error,
        }
    }
}

impl RetryCounters {
    /// Validates that no counter exceeds the corresponding budget maximum.
    /// Recovered or deserialized state must satisfy this invariant to be
    /// considered internally consistent.
    pub(super) fn validate_against_budget(&self, budget: RetryBudget) -> Result<(), EngineError> {
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

pub(super) fn load_current_state(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    scope: &RetryScopeKey,
) -> Result<Option<RetryState>, EngineError> {
    load_matching_state(store, binding, scope)
}

pub(super) fn load_matching_state(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    scope: &RetryScopeKey,
) -> Result<Option<RetryState>, EngineError> {
    store.with_binding_publication_lock(binding, || {
        load_matching_state_locked(store, binding, scope)
    })
}

pub(super) fn load_matching_state_locked(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    scope: &RetryScopeKey,
) -> Result<Option<RetryState>, EngineError> {
    load_matching_state_classified_locked(store, binding, scope)
        .map_err(RetryStateLoadError::into_engine_error)
}

pub(super) fn load_matching_state_classified_locked(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    scope: &RetryScopeKey,
) -> Result<Option<RetryState>, RetryStateLoadError> {
    let history = scan_retry_history(store, binding, scope)?;
    let history_present = history.candidate_count != 0;
    let latest = latest_compact_retry_snapshot(history).map_err(RetryStateLoadError::Corruption)?;
    match store
        .read_recoverable_current_json(binding, RETRY_STATE_FAMILY)
        .map_err(RetryStateLoadError::Storage)?
    {
        RecoverableCurrentArtifact::Valid(value) => {
            let canonical_sequence = artifact_sequence(&value).map_err(|error| {
                RetryStateLoadError::Corruption(RetryStateCorruption::new(error.to_string()))
            })?;
            let canonical = deserialize_retry_state(value.clone()).map_err(|error| {
                RetryStateLoadError::Corruption(RetryStateCorruption::new(error.to_string()))
            })?;
            if canonical.scope != *scope {
                return recover_latest_retry_snapshot(store, binding, latest)
                    .map_err(RetryStateLoadError::Storage);
            }
            if let Some((latest_sequence, latest_value, latest_state)) = latest {
                if latest_sequence < canonical_sequence {
                    return Err(RetryStateLoadError::Corruption(RetryStateCorruption::new(
                        "retry-state canonical is ahead of immutable history",
                    )));
                }
                if latest_sequence > canonical_sequence || latest_value != value {
                    store
                        .restore_canonical_from_history_locked(
                            binding,
                            RETRY_STATE_FAMILY,
                            &latest_value,
                        )
                        .map_err(RetryStateLoadError::Storage)?;
                    return Ok(Some(latest_state));
                }
            }
            Ok(Some(canonical))
        }
        RecoverableCurrentArtifact::Missing => {
            if latest.is_none() && history_present {
                return Err(RetryStateLoadError::Corruption(RetryStateCorruption::new(
                    "retry-state canonical is missing and latest immutable history has no valid exact scope",
                )));
            }
            recover_latest_retry_snapshot(store, binding, latest)
                .map_err(RetryStateLoadError::Storage)
        }
        RecoverableCurrentArtifact::Corrupt => {
            let canonical_path = store.canonical_path(binding, RETRY_STATE_FAMILY);
            if let Ok(raw_canonical) = store.read_json_path(&canonical_path) {
                let canonical_sequence = raw_canonical
                    .get("artifact_sequence")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                let latest_sequence = latest
                    .as_ref()
                    .map(|(sequence, _, _)| *sequence)
                    .unwrap_or_default();
                if canonical_sequence > latest_sequence {
                    return Err(RetryStateLoadError::Corruption(RetryStateCorruption::new(
                        "retry-state canonical is corrupt and ahead of exact-scope immutable history",
                    )));
                }
            }
            match latest {
                Some(latest) => recover_latest_retry_snapshot(store, binding, Some(latest))
                    .map_err(RetryStateLoadError::Storage),
                None => Err(RetryStateLoadError::Corruption(RetryStateCorruption::new(
                    "retry-state canonical is corrupt and exact-scope immutable history is unavailable",
                ))),
            }
        }
    }
}

struct CompactRetryCandidate {
    path: PathBuf,
    validation_error: Option<String>,
    state: Result<RetryState, String>,
}

struct CompactRetryHistory {
    candidate_count: usize,
    scoped: BTreeMap<u64, CompactRetryCandidate>,
    unscoped: Vec<(u64, PathBuf, String)>,
    latest_value: Option<(u64, Value)>,
    duplicate: Option<(u64, PathBuf)>,
}

fn scan_retry_history(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    scope: &RetryScopeKey,
) -> Result<CompactRetryHistory, RetryStateLoadError> {
    let mut history = CompactRetryHistory {
        candidate_count: 0,
        scoped: BTreeMap::new(),
        unscoped: Vec::new(),
        latest_value: None,
        duplicate: None,
    };
    store
        .visit_recoverable_history_candidates(binding, RETRY_STATE_FAMILY, |candidate| {
            history.candidate_count += 1;
            let sequence = candidate_sequence(&candidate);
            let Some(value) = candidate.value else {
                history.unscoped.push((
                    sequence,
                    candidate.path,
                    candidate
                        .validation_error
                        .unwrap_or_else(|| "artifact JSON is unavailable".to_string()),
                ));
                return Ok(());
            };
            let candidate_scope = value
                .get("scope")
                .cloned()
                .and_then(|value| serde_json::from_value::<RetryScopeKey>(value).ok());
            if candidate_scope.as_ref() != Some(scope) {
                if candidate_scope.is_none() {
                    history.unscoped.push((
                        sequence,
                        candidate.path,
                        candidate.validation_error.unwrap_or_else(|| {
                            "scope is unavailable in an otherwise valid artifact envelope"
                                .to_string()
                        }),
                    ));
                }
                return Ok(());
            }
            if history.scoped.contains_key(&sequence) {
                history.duplicate = Some((sequence, candidate.path));
                return Ok(());
            }
            let state = deserialize_retry_state(value.clone()).map_err(|error| error.to_string());
            history.scoped.insert(
                sequence,
                CompactRetryCandidate {
                    path: candidate.path,
                    validation_error: candidate.validation_error,
                    state,
                },
            );
            if history
                .latest_value
                .as_ref()
                .is_none_or(|(latest, _)| sequence > *latest)
            {
                history.latest_value = Some((sequence, value));
            }
            Ok(())
        })
        .map_err(RetryStateLoadError::Storage)?;
    Ok(history)
}

fn latest_compact_retry_snapshot(
    history: CompactRetryHistory,
) -> Result<Option<(u64, Value, RetryState)>, RetryStateCorruption> {
    if let Some((sequence, path)) = history.duplicate {
        return Err(RetryStateCorruption::new(format!(
            "duplicate exact-scope retry history sequence {sequence} at {}",
            path.display()
        )));
    }
    let reset_sequence = history
        .scoped
        .iter()
        .rev()
        .find_map(|(sequence, candidate)| {
            candidate
                .validation_error
                .is_none()
                .then_some(())
                .and_then(|()| candidate.state.as_ref().ok())
                .filter(|state| {
                    state.history_chain_reset && state.transition_type == "terminal_tombstone"
                })
                .map(|_| *sequence)
        });
    for (sequence, path, error) in &history.unscoped {
        if reset_sequence.is_none_or(|reset| *sequence >= reset) {
            return Err(RetryStateCorruption::new(format!(
                "retry history {} has missing or corrupt scope after the latest authenticated exact-scope reset: {error}",
                path.display()
            )));
        }
    }
    let mut latest = None;
    for (sequence, candidate) in history
        .scoped
        .iter()
        .filter(|(sequence, _)| reset_sequence.is_none_or(|reset| **sequence >= reset))
    {
        if let Some(error) = &candidate.validation_error {
            return Err(RetryStateCorruption::new(format!(
                "invalid exact-scope retry history {}: {error}",
                candidate.path.display()
            )));
        }
        let state = candidate.state.as_ref().map_err(|error| {
            RetryStateCorruption::new(format!(
                "invalid exact-scope retry history {}: {error}",
                candidate.path.display()
            ))
        })?;
        let expected_predecessor = latest;
        if state.predecessor_artifact_sequence != expected_predecessor {
            return Err(RetryStateCorruption::new(format!(
                "broken exact-scope retry history chain at {}: expected predecessor {:?}, found {:?}",
                candidate.path.display(),
                expected_predecessor,
                state.predecessor_artifact_sequence
            )));
        }
        latest = Some(*sequence);
    }
    let Some(sequence) = latest else {
        return Ok(None);
    };
    let (value_sequence, value) = history.latest_value.ok_or_else(|| {
        RetryStateCorruption::new("latest exact-scope retry history payload is unavailable")
    })?;
    if value_sequence != sequence {
        return Err(RetryStateCorruption::new(
            "latest exact-scope retry history payload does not match validated chain",
        ));
    }
    let state = history
        .scoped
        .get(&sequence)
        .and_then(|candidate| candidate.state.as_ref().ok())
        .cloned()
        .ok_or_else(|| RetryStateCorruption::new("latest retry state is unavailable"))?;
    Ok(Some((sequence, value, state)))
}

#[cfg(test)]
pub(super) fn latest_retry_snapshot(
    history: &[RecoverableHistoryCandidate],
    scope: &RetryScopeKey,
) -> Result<Option<(u64, Value, RetryState)>, RetryStateCorruption> {
    let mut scoped = history
        .iter()
        .filter_map(|candidate| {
            let value = candidate.value.as_ref()?;
            let candidate_scope = value
                .get("scope")
                .cloned()
                .and_then(|scope| serde_json::from_value::<RetryScopeKey>(scope).ok());
            (candidate_scope.as_ref() == Some(scope)).then_some(candidate)
        })
        .collect::<Vec<_>>();
    scoped.sort_by_key(|candidate| candidate_sequence(candidate));
    let reset_index = latest_authenticated_reset_index(&scoped);
    let first = reset_index.unwrap_or_default();
    let reset_sequence = reset_index.map(|index| candidate_sequence(scoped[index]));
    reject_unscoped_corruption_since_reset(history, reset_sequence)?;

    let mut latest = None;
    for candidate in scoped.into_iter().skip(first) {
        let value = candidate
            .value
            .as_ref()
            .expect("scoped candidates have values");
        if let Some(error) = &candidate.validation_error {
            return Err(RetryStateCorruption::new(format!(
                "invalid exact-scope retry history {}: {error}",
                candidate.path.display()
            )));
        }
        let sequence = artifact_sequence(value).map_err(|error| {
            RetryStateCorruption::new(format!(
                "invalid exact-scope retry history {}: {error}",
                candidate.path.display()
            ))
        })?;
        let state = deserialize_retry_state(value.clone()).map_err(|error| {
            RetryStateCorruption::new(format!(
                "invalid exact-scope retry history {}: {error}",
                candidate.path.display()
            ))
        })?;
        let expected_predecessor = latest
            .as_ref()
            .map(|(previous_sequence, _, _)| *previous_sequence);
        if state.predecessor_artifact_sequence != expected_predecessor {
            return Err(RetryStateCorruption::new(format!(
                "broken exact-scope retry history chain at {}: expected predecessor {:?}, found {:?}",
                candidate.path.display(),
                expected_predecessor,
                state.predecessor_artifact_sequence
            )));
        }
        latest = Some((sequence, value.clone(), state));
    }
    Ok(latest)
}

#[cfg(test)]
fn latest_authenticated_reset_index(scoped: &[&RecoverableHistoryCandidate]) -> Option<usize> {
    scoped.iter().rposition(|candidate| {
        candidate.validation_error.is_none()
            && candidate.value.as_ref().is_some_and(|value| {
                deserialize_retry_state(value.clone()).is_ok_and(|state| {
                    state.history_chain_reset && state.transition_type == "terminal_tombstone"
                })
            })
    })
}

#[cfg(test)]
fn reject_unscoped_corruption_since_reset(
    history: &[RecoverableHistoryCandidate],
    reset_sequence: Option<u64>,
) -> Result<(), RetryStateCorruption> {
    for candidate in history {
        let scope_is_readable = candidate
            .value
            .as_ref()
            .and_then(|value| value.get("scope"))
            .cloned()
            .and_then(|scope| serde_json::from_value::<RetryScopeKey>(scope).ok())
            .is_some();
        if !scope_is_readable
            && reset_sequence.is_none_or(|reset| candidate_sequence(candidate) >= reset)
        {
            return Err(RetryStateCorruption::new(format!(
                "retry history {} has missing or corrupt scope after the latest authenticated exact-scope reset: {}",
                candidate.path.display(),
                candidate
                    .validation_error
                    .as_deref()
                    .unwrap_or("scope is unavailable in an otherwise valid artifact envelope")
            )));
        }
    }
    Ok(())
}

fn candidate_sequence(candidate: &RecoverableHistoryCandidate) -> u64 {
    history_filename_artifact_sequence(&candidate.path)
}

fn history_filename_artifact_sequence(path: &std::path::Path) -> u64 {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.split('-').next())
        .and_then(|sequence| sequence.parse().ok())
        .unwrap_or_default()
}

fn recover_latest_retry_snapshot(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    latest: Option<(u64, Value, RetryState)>,
) -> Result<Option<RetryState>, EngineError> {
    let Some((_, value, state)) = latest else {
        return Ok(None);
    };
    store.restore_canonical_from_history_locked(binding, RETRY_STATE_FAMILY, &value)?;
    Ok(Some(state))
}

fn deserialize_retry_state(value: Value) -> Result<RetryState, EngineError> {
    let state = serde_json::from_value(value).map_err(|error| {
        EngineError::InvalidState(format!("invalid remediation retry state: {error}"))
    })?;
    validate_state_invariants(&state)?;
    Ok(state)
}

fn artifact_sequence(value: &Value) -> Result<u64, EngineError> {
    value
        .get("artifact_sequence")
        .and_then(Value::as_u64)
        .filter(|sequence| *sequence > 0)
        .ok_or_else(|| {
            EngineError::InvalidState(
                "retry state is missing a positive artifact_sequence".to_string(),
            )
        })
}

pub(super) fn state_sequence_locked(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
) -> Result<Option<u64>, EngineError> {
    match store.read_recoverable_current_json(binding, RETRY_STATE_FAMILY)? {
        RecoverableCurrentArtifact::Missing => Ok(None),
        RecoverableCurrentArtifact::Valid(value) => artifact_sequence(&value).map(Some),
        RecoverableCurrentArtifact::Corrupt => Err(EngineError::InvalidState(
            "retry-state canonical is corrupt while deriving predecessor".to_string(),
        )),
    }
}

/// Validates that a recovered or deserialized RetryState is internally
/// consistent: counters must not exceed budget maxima, ordinal must be
/// consistent with attempt index, and terminal states must have matching
/// phase/counters.
pub(super) fn validate_state_invariants(state: &RetryState) -> Result<(), EngineError> {
    state.counters.validate_against_budget(state.budget)?;
    if state.launch_ordinal > state.budget.max_remediation_attempts {
        return Err(EngineError::InvalidState(format!(
            "launch_ordinal {} exceeds max_remediation_attempts {}",
            state.launch_ordinal, state.budget.max_remediation_attempts
        )));
    }
    if state.history_chain_reset
        && (state.transition_type != "terminal_tombstone"
            || state.predecessor_artifact_sequence.is_some())
    {
        return Err(EngineError::InvalidState(
            "retry history reset requires a predecessor-free terminal tombstone".to_string(),
        ));
    }
    if state.transition_type == "terminal_tombstone" && !state.history_chain_reset {
        return Err(EngineError::InvalidState(
            "terminal retry tombstone must reset its history chain".to_string(),
        ));
    }
    if state.launch_ordinal != state.counters.remediation_attempt_index {
        return Err(EngineError::InvalidState(format!(
            "launch_ordinal {} differs from remediation_attempt_index {}",
            state.launch_ordinal, state.counters.remediation_attempt_index
        )));
    }
    if matches!(
        state.launch_phase,
        LaunchPhase::Reserved | LaunchPhase::Launched
    ) {
        if state.owner_token.is_empty()
            || state.lease_expiry.is_none()
            || state.invocation_timeout_seconds == 0
        {
            return Err(EngineError::InvalidState(
                "active retry state requires owner token, lease expiry, and invocation timeout"
                    .to_string(),
            ));
        }
        chrono::DateTime::parse_from_rfc3339(state.lease_expiry.as_deref().expect("checked above"))
            .map_err(|error| {
                EngineError::InvalidState(format!("retry lease expiry is not RFC3339: {error}"))
            })?;
    }
    Ok(())
}
