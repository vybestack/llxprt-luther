//! Post-PR terminal failure projection.
//!
//! ## Terminal schema contract
//!
//! The terminal artifact records the deterministic, durable failure selected
//! from the active binding's candidate set. It includes:
//! - `terminal_state`: always `"fatal"` (not `"failed"`)
//! - `failed_step`: the producer step of the selected source failure
//! - `failure_reason`: the semantic reason of the selected source
//! - `source_artifacts`: all candidate source artifacts
//! - `source_failure_sequence` / `source_artifact_sequence` / etc.: the
//!   selected source provenance
//! - `selected_source_reason`: deterministic selection rationale
//! - `logged_at`: RFC3339 timestamp
//!
//! ## Idempotency
//!
//! A stable idempotency key (derived from binding + selected source provenance)
//! is embedded in the artifact. On replay, an existing terminal artifact with
//! the same idempotency key is reused rather than re-written.
//!
//! ## Corruption recovery
//!
//! Corrupt canonical retry state is recovered from immutable history. If
//! history recovery fails, a durable tombstone is written and the terminal
//! fails closed. Parse corruption is distinguished from I/O errors. Quarantine
//! evidence uses a unique suffix.

use std::sync::Arc;

use super::retry_state::{
    causal_exhaustion, reconcile_retry_policy_locked, write_terminal_tombstone_locked, LaunchPhase,
    RetryBudget, RetryScopeKey, RetryState, RETRY_STATE_FAMILY,
};
use super::{artifact_root, binding_for_context, current_step_id, u64_param};
use crate::engine::executor::{StepContext, StepExecutor};
use crate::engine::executors::pr_followup_artifacts::{
    ArtifactPublicationHook, ArtifactWriter, ClockSleeper, PrFollowupArtifactStore,
    SystemClockSleeper, SystemPrFollowupFilesystem, TerminalArtifactPublication,
};
use crate::engine::executors::pr_followup_types::PrFollowupBinding;
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;
use serde_json::{json, Value};

/// Terminal artifact family.
const TERMINAL_FAMILY: &str = "post-pr-failure-terminal";

#[derive(Debug, Default)]
pub struct PostPrFailureTerminalExecutor;

impl StepExecutor for PostPrFailureTerminalExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        params: &Value,
    ) -> Result<StepOutcome, EngineError> {
        execute_terminal(context, params, &SystemClockSleeper, None)
    }
}

pub struct PostPrFailureTerminalExecutorWithClock<C> {
    clock: C,
    publication_hook: Option<Arc<dyn ArtifactPublicationHook>>,
}

impl<C> PostPrFailureTerminalExecutorWithClock<C> {
    pub fn new(clock: C) -> Self {
        Self {
            clock,
            publication_hook: None,
        }
    }

    pub fn with_publication_hook(
        clock: C,
        publication_hook: Arc<dyn ArtifactPublicationHook>,
    ) -> Self {
        Self {
            clock,
            publication_hook: Some(publication_hook),
        }
    }
}

impl<C: ClockSleeper> StepExecutor for PostPrFailureTerminalExecutorWithClock<C> {
    fn execute(
        &self,
        context: &mut StepContext,
        params: &Value,
    ) -> Result<StepOutcome, EngineError> {
        execute_terminal(context, params, &self.clock, self.publication_hook.clone())
    }
}

fn execute_terminal(
    context: &mut StepContext,
    params: &Value,
    clock: &dyn ClockSleeper,
    publication_hook: Option<Arc<dyn ArtifactPublicationHook>>,
) -> Result<StepOutcome, EngineError> {
    let artifact_root = artifact_root(context, params)?;
    let store = match publication_hook {
        Some(hook) => PrFollowupArtifactStore::with_filesystem_and_publication_hook(
            &artifact_root,
            &SystemPrFollowupFilesystem,
            hook,
        )?,
        None => {
            PrFollowupArtifactStore::with_filesystem(&artifact_root, &SystemPrFollowupFilesystem)?
        }
    };
    let binding = binding_for_context(context, params, &store, clock)?;
    let step_id = current_step_id(context, "post_pr_failure_terminal");
    let step_order = u64_param(params, "step_order_index", 13);

    store.with_binding_publication_lock(&binding, || {
        if store
            .resolve_committed_terminal_locked(&binding, TERMINAL_FAMILY)?
            .is_some()
        {
            return Ok(StepOutcome::Fatal);
        }
        let mut retry_state = resolve_retry_state_locked(&store, &binding, params, clock)?;
        if let Some(state) = retry_state.as_mut() {
            reconcile_retry_policy_locked(
                &store,
                &binding,
                &step_id,
                step_order,
                state,
                RetryBudget::from_params(params)?,
                clock,
            )?;
        }
        let active_launch = retry_state
            .as_ref()
            .map(|state| active_launch_is_unexpired(state, clock))
            .transpose()?
            .unwrap_or(false);
        let source_selection = select_failure_source(&store, &binding)?;
        let payload = build_terminal_payload(
            &binding,
            retry_state.as_ref(),
            &source_selection,
            active_launch,
            clock,
        );
        let idempotency_key = payload["idempotency_key"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let reason = payload["failure_reason"]
            .as_str()
            .or_else(|| payload["terminal_reason"].as_str())
            .unwrap_or("post_pr_failure");

        store.publish_terminal_once_locked(&TerminalArtifactPublication {
            binding: &binding,
            artifact_family: TERMINAL_FAMILY,
            producer_step_id: &step_id,
            step_order_index: step_order,
            payload: &payload,
            failure_reason: reason,
            idempotency_key: &idempotency_key,
            clock,
            allow_distinct_idempotency_keys: false,
        })?;
        Ok(StepOutcome::Fatal)
    })
}

/// Resolves the retry state, recovering from corruption via immutable history
/// or a durable tombstone. Distinguishes parse corruption from I/O errors.
fn active_launch_is_unexpired(
    state: &RetryState,
    clock: &dyn ClockSleeper,
) -> Result<bool, EngineError> {
    if !matches!(
        state.launch_phase,
        LaunchPhase::Reserved | LaunchPhase::Launched
    ) {
        return Ok(false);
    }
    Ok(!super::retry_lease::is_lease_expired(state, clock)?)
}

fn resolve_retry_state_locked(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    params: &Value,
    clock: &dyn ClockSleeper,
) -> Result<Option<RetryState>, EngineError> {
    let plan = match store.read_recoverable_current_json(binding, "pr-remediation-plan")? {
        crate::engine::executors::pr_followup_artifacts::RecoverableCurrentArtifact::Missing => {
            None
        }
        crate::engine::executors::pr_followup_artifacts::RecoverableCurrentArtifact::Valid(
            value,
        ) => Some(value),
        crate::engine::executors::pr_followup_artifacts::RecoverableCurrentArtifact::Corrupt => {
            return Err(artifact_io_error("remediation plan is corrupt"))
        }
    };
    let canonical_exists = !matches!(
        store.read_recoverable_current_json(binding, RETRY_STATE_FAMILY)?,
        crate::engine::executors::pr_followup_artifacts::RecoverableCurrentArtifact::Missing
    );
    let history_scope = retry_scope_from_history(store, binding)?;
    let scope = match plan.as_ref() {
        Some(plan) => Some(RetryScopeKey::new(binding, plan)?),
        None => history_scope,
    };
    let Some(scope) = scope else {
        if canonical_exists {
            return Err(EngineError::InvalidState(
                "retry state is corrupt and no plan or valid history can establish its scope"
                    .to_string(),
            ));
        }
        return Ok(None);
    };
    match super::retry_history::load_matching_state_classified_locked(store, binding, &scope) {
        Ok(Some(state)) => return Ok(Some(state)),
        Ok(None) if !canonical_exists => return Ok(None),
        Ok(None) => {}
        Err(super::retry_history::RetryStateLoadError::Corruption(_)) => {}
        Err(super::retry_history::RetryStateLoadError::Storage(error)) => return Err(error),
    }
    quarantine_corrupt_retry_state(store, binding)?;
    write_terminal_tombstone_locked(store, binding, &scope, params, clock).map(Some)
}

fn retry_scope_from_history(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
) -> Result<Option<RetryScopeKey>, EngineError> {
    let history = store.read_recoverable_history_family(binding, RETRY_STATE_FAMILY)?;
    for value in history.iter().rev() {
        if let Ok(state) = serde_json::from_value::<RetryState>(value.clone()) {
            return Ok(Some(state.scope));
        }
    }
    Ok(None)
}

/// Quarantines a corrupt retry-state file with unique evidence suffix to
/// prevent collisions on repeated quarantine attempts.
///
/// The quarantine path is validated to stay within the same parent directory
/// as the original file (path component containment), preventing directory
/// traversal via crafted extensions.
fn quarantine_corrupt_retry_state(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
) -> Result<(), EngineError> {
    let path = store.canonical_path(binding, RETRY_STATE_FAMILY);
    if matches!(
        store.read_recoverable_current_json(binding, RETRY_STATE_FAMILY)?,
        crate::engine::executors::pr_followup_artifacts::RecoverableCurrentArtifact::Missing
    ) {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| artifact_io_error(format!("missing parent for {}", path.display())))?;
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let unique = uuid::Uuid::new_v4().simple();
    let stem = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("pr-remediation-retry-state");
    let quarantined = parent.join(format!("{stem}.corrupt.{timestamp}.{unique}"));
    store.rename_contained_file(&path, &quarantined)
}

fn artifact_io_error(message: impl Into<String>) -> EngineError {
    EngineError::InvalidState(message.into())
}

/// A candidate failure artifact extracted from the store.
#[derive(Clone)]
struct SourceCandidate {
    artifact_family: String,
    artifact_sequence: u64,
    write_sequence: u64,
    failure_sequence: u64,
    producer_step_id: String,
    step_order_index: u64,
    path: String,
    history_path: String,
    failure_reason: Option<String>,
}

/// Selects the latest valid active-binding failure by durable sequence/provenance.
/// Selection is deterministic: highest failure_sequence, then highest
/// artifact_sequence, then write_sequence, then producer_step_id.
///
/// Each candidate is validated in isolation: a corrupt or malformed candidate
/// is skipped without invalidating the remaining candidates. This prevents one
/// bad artifact from suppressing the entire candidate set.
enum SourceSelection {
    Selected {
        candidates: Vec<SourceCandidate>,
        selected_index: usize,
        reason: &'static str,
    },
    NoCandidates,
}

fn select_failure_source(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
) -> Result<SourceSelection, EngineError> {
    let mut candidates = Vec::new();
    store.visit_failure_history_candidates(binding, TERMINAL_FAMILY, |value| {
        if let Some(candidate) = parse_candidate(&value).or_else(|| log_skipped_candidate(&value)) {
            candidates.push(candidate);
        }
        Ok(())
    })?;
    if candidates.is_empty() {
        return Ok(SourceSelection::NoCandidates);
    }
    candidates.sort_by(source_candidate_order);
    Ok(SourceSelection::Selected {
        selected_index: candidates.len() - 1,
        candidates,
        reason: "highest_failure_sequence",
    })
}

fn source_candidate_order(left: &SourceCandidate, right: &SourceCandidate) -> std::cmp::Ordering {
    left.failure_sequence
        .cmp(&right.failure_sequence)
        .then(left.artifact_sequence.cmp(&right.artifact_sequence))
        .then(left.write_sequence.cmp(&right.write_sequence))
        .then(left.producer_step_id.cmp(&right.producer_step_id))
}

/// Logs a skipped candidate and returns None for use in filter_map. This
/// ensures isolated candidate validation: one bad candidate does not abort
/// the entire parse loop.
fn log_skipped_candidate(value: &Value) -> Option<SourceCandidate> {
    let family = value
        .get("history_metadata")
        .and_then(|m| m.get("artifact_family"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let seq = value
        .get("artifact_sequence")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    eprintln!("warn: terminal source candidate {family} seq={seq} failed validation, skipping");
    None
}

fn parse_candidate(value: &Value) -> Option<SourceCandidate> {
    let failure_sequence = value.get("failure_sequence").and_then(Value::as_u64)?;
    let artifact_sequence = value.get("artifact_sequence").and_then(Value::as_u64)?;
    let write_sequence = value.get("write_sequence").and_then(Value::as_u64)?;
    let producer_step_id = value
        .get("producer_step_id")
        .and_then(Value::as_str)?
        .to_string();
    let step_order_index = value.get("step_order_index").and_then(Value::as_u64)?;
    let artifact_family = value
        .get("history_metadata")
        .and_then(|m| m.get("artifact_family"))
        .and_then(Value::as_str)?
        .to_string();
    let path = value
        .get("history_metadata")
        .and_then(|m| m.get("canonical_path"))
        .and_then(Value::as_str)?
        .to_string();
    let history_path = value
        .get("history_metadata")
        .and_then(|m| m.get("history_path"))
        .and_then(Value::as_str)?
        .to_string();
    let failure_reason = value
        .get("failure_reason")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .map(ToString::to_string)?;
    Some(SourceCandidate {
        artifact_family,
        artifact_sequence,
        write_sequence,
        failure_sequence,
        producer_step_id,
        step_order_index,
        path,
        history_path,
        failure_reason: Some(failure_reason),
    })
}

/// Common context for terminal payload construction, holding pre-resolved
/// metadata shared by both selection variants.
struct TerminalPayloadContext<'a> {
    binding: &'a PrFollowupBinding,
    logged_at: String,
    terminal_override: Option<&'static str>,
    budget_exhaustion: Option<&'static str>,
    retry_metadata: Option<Value>,
}

impl<'a> TerminalPayloadContext<'a> {
    fn new(
        binding: &'a PrFollowupBinding,
        retry_state: Option<&RetryState>,
        active_launch: bool,
        clock: &dyn ClockSleeper,
    ) -> Self {
        Self {
            binding,
            logged_at: clock.now_rfc3339(),
            terminal_override: active_launch.then_some("active_remediation_launch"),
            budget_exhaustion: retry_state.and_then(exhausted_budget),
            retry_metadata: retry_state.map(retry_metadata_json),
        }
    }

    /// Builds the terminal artifact payload conforming to the documented schema.
    fn build(self, selection: &SourceSelection) -> Value {
        match selection {
            SourceSelection::NoCandidates => self.build_no_candidates(),
            SourceSelection::Selected {
                candidates,
                selected_index,
                reason,
            } => self.build_selected(candidates, *selected_index, reason),
        }
    }

    /// Builds the no-candidates variant: a terminal marker with no source
    /// failure artifacts, using the budget-exhaustion reason or a generic
    /// fallback.
    fn build_no_candidates(&self) -> Value {
        let terminal_reason = self
            .terminal_override
            .or(self.budget_exhaustion)
            .unwrap_or("post_pr_failure");
        let idempotency_key = format!(
            "terminal:{}:{}:{}:{}",
            self.binding.run_id, self.binding.pr_number, self.binding.head_sha, terminal_reason
        );
        let mut payload = json!({
            "terminal_state": "fatal",
            "terminal_reason": terminal_reason,
            "failure_reason": terminal_reason,
            "failed_step": "post_pr_failure_terminal",
            "source_artifacts": [],
            "selected_source_reason": "no_failure_candidates",
            "idempotency_key": idempotency_key,
            "logged_at": self.logged_at
        });
        self.apply_exhaustion(&mut payload);
        merge_retry_metadata(&mut payload, &self.retry_metadata);
        payload
    }

    /// Builds the selected-source variant: a terminal marker recording every
    /// eligible source as lightweight provenance and the deterministic winner.
    fn build_selected(
        &self,
        candidates: &[SourceCandidate],
        selected_index: usize,
        selection_reason: &str,
    ) -> Value {
        let source = &candidates[selected_index];
        let terminal_reason = self
            .terminal_override
            .or(self.budget_exhaustion)
            .unwrap_or("selected_source_failure");
        let idempotency_key = format!(
            "terminal:{}:{}:{}:{}:{}:{}",
            self.binding.run_id,
            self.binding.pr_number,
            self.binding.head_sha,
            terminal_reason,
            source.failure_sequence,
            source.artifact_sequence
        );
        let selected_failure_reason = source
            .failure_reason
            .clone()
            .unwrap_or_else(|| terminal_reason.to_string());
        let source_artifacts = candidates
            .iter()
            .map(source_candidate_json)
            .collect::<Vec<_>>();
        let mut payload = json!({
            "terminal_state": "fatal",
            "terminal_reason": terminal_reason,
            "failure_reason": selected_failure_reason,
            "failed_step": source.producer_step_id,
            "source_artifacts": source_artifacts,
            "source_failure_sequence": source.failure_sequence,
            "source_artifact_sequence": source.artifact_sequence,
            "source_write_sequence": source.write_sequence,
            "source_producer_step_id": source.producer_step_id,
            "source_step_order_index": source.step_order_index,
            "source_artifact_path": source.path,
            "source_history_path": source.history_path,
            "source_failure_reason": source.failure_reason,
            "source_artifact_family": source.artifact_family,
            "selected_source_reason": selection_reason,
            "idempotency_key": idempotency_key,
            "logged_at": self.logged_at
        });
        self.apply_exhaustion(&mut payload);
        merge_retry_metadata(&mut payload, &self.retry_metadata);
        payload
    }

    /// Applies the budget-exhaustion field if present.
    fn apply_exhaustion(&self, payload: &mut Value) {
        if let Some(reason) = self.budget_exhaustion {
            payload["exhausted_budget"] = json!(reason);
        }
    }
}

fn source_candidate_json(source: &SourceCandidate) -> Value {
    json!({
        "artifact_family": source.artifact_family,
        "artifact_sequence": source.artifact_sequence,
        "write_sequence": source.write_sequence,
        "failure_sequence": source.failure_sequence,
        "producer_step_id": source.producer_step_id,
        "step_order_index": source.step_order_index,
        "path": source.path,
        "history_path": source.history_path,
        "failure_reason": source.failure_reason
    })
}

/// Builds the terminal artifact payload conforming to the documented schema.
fn build_terminal_payload(
    binding: &PrFollowupBinding,
    retry_state: Option<&RetryState>,
    selection: &SourceSelection,
    active_launch: bool,
    clock: &dyn ClockSleeper,
) -> Value {
    TerminalPayloadContext::new(binding, retry_state, active_launch, clock).build(selection)
}

/// Constructs the retry-state metadata JSON value from a `RetryState`.
fn retry_metadata_json(state: &RetryState) -> Value {
    json!({
        "remediation_attempt_index": state.counters.remediation_attempt_index,
        "max_remediation_attempts": state.budget.max_remediation_attempts,
        "validation_retry_index": state.counters.validation_retry_index,
        "max_validation_retries": state.budget.max_validation_retries,
        "stale_artifact_retry_index": state.counters.stale_artifact_retry_index,
        "max_stale_artifact_retries": state.budget.max_stale_artifact_retries,
        "retry_transition_id": state.transition_id,
        "retry_launch_phase": state.launch_phase,
    })
}

fn exhausted_budget(state: &RetryState) -> Option<&'static str> {
    causal_exhaustion(state)
}

/// Merges optional retry-state metadata fields into the terminal payload.
fn merge_retry_metadata(payload: &mut Value, metadata: &Option<Value>) {
    if let Some(metadata) = metadata {
        for key in [
            "remediation_attempt_index",
            "max_remediation_attempts",
            "validation_retry_index",
            "max_validation_retries",
            "stale_artifact_retry_index",
            "max_stale_artifact_retries",
            "retry_transition_id",
            "retry_launch_phase",
        ] {
            if let Some(value) = metadata.get(key) {
                payload[key] = value.clone();
            }
        }
    }
}
