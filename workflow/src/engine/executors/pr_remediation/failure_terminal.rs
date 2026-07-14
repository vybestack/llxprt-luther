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

use std::fs;

use super::retry_state::{write_terminal_tombstone, RetryScopeKey, RetryState, RETRY_STATE_FAMILY};
use super::{artifact_root, binding_for_context, current_step_id, u64_param};
use crate::engine::executor::{StepContext, StepExecutor};
use crate::engine::executors::pr_followup_artifacts::{
    ArtifactWriter, ClockSleeper, PrFollowupArtifactStore, SystemClockSleeper,
    SystemPrFollowupFilesystem,
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
        let clock = SystemClockSleeper;
        let artifact_root = artifact_root(context, params)?;
        let store =
            PrFollowupArtifactStore::with_filesystem(&artifact_root, &SystemPrFollowupFilesystem)?;
        let binding = binding_for_context(context, params, &store, &clock)?;
        let retry_state = resolve_retry_state(&store, &binding, &clock)?;
        let step_id = current_step_id(context, "post_pr_failure_terminal");
        let step_order = u64_param(params, "step_order_index", 13);

        // Build the terminal payload with source selection.
        let source_selection = select_failure_source(&store, &binding);
        let payload =
            build_terminal_payload(&binding, retry_state.as_ref(), &source_selection, &clock);
        let idempotency_key = payload["idempotency_key"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let reason = payload["failure_reason"]
            .as_str()
            .unwrap_or("post_pr_failure");

        // Idempotency: reuse existing terminal artifact with matching key.
        if let Some(existing) = read_existing_terminal(&store, &binding)? {
            if existing.get("idempotency_key").and_then(Value::as_str) == Some(&idempotency_key) {
                return Ok(StepOutcome::Fatal);
            }
        }

        write_terminal_artifact_direct(
            &binding, &store, &step_id, step_order, &payload, reason, &clock,
        )?;
        Ok(StepOutcome::Fatal)
    }
}

/// Resolves the retry state, recovering from corruption via immutable history
/// or a durable tombstone. Distinguishes parse corruption from I/O errors.
fn resolve_retry_state(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    clock: &dyn ClockSleeper,
) -> Result<Option<RetryState>, EngineError> {
    if !store
        .canonical_path(binding, "pr-remediation-plan")
        .exists()
    {
        return Ok(None);
    }
    let plan = store.read_current_raw_json(binding, "pr-remediation-plan")?;
    let scope = RetryScopeKey::new(binding, &plan)?;
    match read_canonical_retry_state(store, binding) {
        Ok(state) => {
            let scope_clone = scope.clone();
            Ok(state.filter(|s| s.scope == scope_clone))
        }
        Err(error) => {
            let is_parse = is_parse_error(&error);
            if is_parse {
                // Parse corruption: attempt to recover from immutable history
                // before quarantining. History is immutable and append-only,
                // so the most recent valid same-scope snapshot is authoritative.
                if let Some(recovered) = recover_retry_state_from_history(store, binding, &scope)? {
                    // Republish the recovered valid state to the canonical path
                    // so the corruption is actually healed. The canonical file
                    // currently holds corrupt data; overwriting it with the
                    // recovered history snapshot restores consistency. We write
                    // directly (bypassing the store's sequence-recovery scan)
                    // because co-resident corrupt artifacts would cause the scan
                    // to fail.
                    republish_recovered_state(store, binding, &recovered)?;
                    return Ok(Some(recovered));
                }
                // No recoverable history — quarantine the corrupt file with
                // unique evidence and fail closed via tombstone. The tombstone
                // is written directly to the canonical path (not through the
                // store's write_json_artifact) because the corrupt history
                // would cause the store's sequence-recovery scan to fail.
                quarantine_corrupt_retry_state(store, binding)?;
                write_terminal_tombstone(store, binding, &scope, clock)?;
                Ok(None)
            } else {
                // I/O error — propagate rather than quarantining.
                Err(error)
            }
        }
    }
}

/// Reads the canonical retry-state file and deserializes it. Does not go
/// through the store's sequence-recovery scan, so corrupt history files do
/// not cause parse errors. Used by the terminal's corruption-recovery path.
fn read_canonical_retry_state(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
) -> Result<Option<RetryState>, EngineError> {
    let path = store.canonical_path(binding, RETRY_STATE_FAMILY);
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path)
        .map_err(|err| artifact_io_error(format!("read retry state: {err}")))?;
    let value: Value = serde_json::from_str(&raw).map_err(|err| {
        EngineError::InvalidState(format!("invalid remediation retry state: {err}"))
    })?;
    let state: RetryState = serde_json::from_value(value).map_err(|err| {
        EngineError::InvalidState(format!("invalid remediation retry state: {err}"))
    })?;
    Ok(Some(state))
}

/// Returns `true` if the error indicates a JSON parse corruption (as opposed
/// to a transient I/O error).
fn is_parse_error(error: &EngineError) -> bool {
    let message = error.to_string();
    message.contains("parse") || message.contains("invalid remediation retry state")
}

/// Attempts to recover the most recent valid retry state from immutable
/// history snapshots. History files are keyed by artifact_sequence and are
/// never overwritten, so the latest valid one for the scope is authoritative.
/// Reads files directly (not through the store's sequence-recovery scan) to
/// avoid failing on corrupt canonical artifacts.
fn recover_retry_state_from_history(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    scope: &RetryScopeKey,
) -> Result<Option<RetryState>, EngineError> {
    let history_root = store.history_root_for_family(binding, RETRY_STATE_FAMILY);
    if !history_root.exists() {
        return Ok(None);
    }
    let mut paths = Vec::new();
    collect_history_jsons(&history_root, &mut paths);
    paths.sort();
    paths.reverse();
    let scope_clone = scope.clone();
    for path in &paths {
        let raw = match fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(_) => continue,
        };
        let value: Value = match serde_json::from_str(&raw) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let state: RetryState = match serde_json::from_value(value) {
            Ok(state) => state,
            Err(_) => continue,
        };
        if state.scope == scope_clone {
            return Ok(Some(state));
        }
    }
    Ok(None)
}

fn collect_history_jsons(root: &std::path::Path, paths: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_history_jsons(&path, paths);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            paths.push(path);
        }
    }
}

/// Quarantines a corrupt retry-state file with unique evidence suffix to
/// prevent collisions on repeated quarantine attempts.
fn quarantine_corrupt_retry_state(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
) -> Result<(), EngineError> {
    let path = store.canonical_path(binding, RETRY_STATE_FAMILY);
    if path.exists() {
        let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
        let unique = uuid::Uuid::new_v4().simple();
        let quarantined = path.with_extension(format!("corrupt.{timestamp}.{unique}"));
        fs::rename(&path, &quarantined).map_err(|error| {
            EngineError::InvalidState(format!("quarantine corrupt retry state: {error}"))
        })?;
    }
    Ok(())
}

/// Republishes a recovered retry state to the canonical path, healing the
/// corruption. The recovered state was read from immutable history (which
/// includes store-injected metadata like `artifact_sequence`), so we serialize
/// it directly and overwrite the corrupt canonical file. This bypasses the
/// store's `write_json_artifact` (and its sequence-recovery scan) because
/// co-resident corrupt artifacts would cause the scan to fail.
fn republish_recovered_state(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    state: &RetryState,
) -> Result<(), EngineError> {
    let canonical_path = store.canonical_path(binding, RETRY_STATE_FAMILY);
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|err| artifact_io_error(format!("serialize recovered state: {err}")))?;
    if let Some(parent) = canonical_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| artifact_io_error(format!("create recovered state parent: {err}")))?;
    }
    fs::write(&canonical_path, &bytes)
        .map_err(|err| artifact_io_error(format!("write recovered state: {err}")))?;
    Ok(())
}

fn artifact_io_error(message: impl Into<String>) -> EngineError {
    EngineError::InvalidState(message.into())
}

/// Writes the terminal artifact directly to the canonical path, bypassing the
/// store's sequence-recovery scan. This is necessary because the terminal step
/// runs in a failure context where co-resident artifacts (e.g. a corrupt
/// retry-state) would cause the store's sequence scan to fail. The terminal
/// artifact is a terminal marker — it does not participate in sequence recovery.
///
/// Despite bypassing the normal write path, the terminal artifact carries the
/// complete binding/sequence metadata contract: binding fields, artifact_sequence,
/// write_sequence, producer_step_id, step_order_index, and history_metadata.
/// Sequences are computed from immutable history only, which is always valid.
fn write_terminal_artifact_direct(
    binding: &PrFollowupBinding,
    store: &PrFollowupArtifactStore,
    step_id: &str,
    step_order: u64,
    payload: &Value,
    reason: &str,
    clock: &dyn ClockSleeper,
) -> Result<(), EngineError> {
    let sequence = store.next_sequence_from_history(binding, TERMINAL_FAMILY, step_id)?;
    let canonical_path = store.canonical_path(binding, TERMINAL_FAMILY);
    let history_path = store.history_path(binding, TERMINAL_FAMILY, &sequence);

    let mut value = payload.clone();
    store.inject_artifact_metadata(
        binding,
        TERMINAL_FAMILY,
        &sequence,
        step_order,
        &canonical_path,
        &history_path,
        None,
        None,
        clock,
        &mut value,
    )?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "failure_reason".to_string(),
            Value::from(reason.to_string()),
        );
    }

    let bytes = serde_json::to_vec_pretty(&value)
        .map_err(|err| artifact_io_error(format!("serialize terminal: {err}")))?;
    if let Some(parent) = history_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| artifact_io_error(format!("create terminal history parent: {err}")))?;
    }
    fs::write(&history_path, &bytes)
        .map_err(|err| artifact_io_error(format!("write terminal history: {err}")))?;
    if let Some(parent) = canonical_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| artifact_io_error(format!("create terminal canonical parent: {err}")))?;
    }
    fs::write(&canonical_path, &bytes)
        .map_err(|err| artifact_io_error(format!("write terminal canonical: {err}")))?;
    Ok(())
}

/// Reads an existing terminal artifact for the binding, returning `None` if
/// absent. Reads the file directly to avoid the store's sequence-recovery
/// scan failing on corrupt co-resident artifacts.
fn read_existing_terminal(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
) -> Result<Option<Value>, EngineError> {
    let path = store.canonical_path(binding, TERMINAL_FAMILY);
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path)
        .map_err(|err| artifact_io_error(format!("read terminal: {err}")))?;
    let value: Value = serde_json::from_str(&raw)
        .map_err(|err| artifact_io_error(format!("parse terminal: {err}")))?;
    Ok(Some(value))
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
}

/// Selects the latest valid active-binding failure by durable sequence/provenance.
/// Selection is deterministic: highest failure_sequence, then highest
/// artifact_sequence, then write_sequence, then producer_step_id.
enum SourceSelection {
    Selected(SourceCandidate, Vec<Value>, &'static str),
    NoCandidates,
}

fn select_failure_source(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
) -> SourceSelection {
    let candidates = match store.read_current_failure_candidates(binding, TERMINAL_FAMILY) {
        Ok(candidates) => candidates,
        Err(_) => return SourceSelection::NoCandidates,
    };
    if candidates.is_empty() {
        return SourceSelection::NoCandidates;
    }
    let parsed = candidates
        .iter()
        .filter_map(parse_candidate)
        .collect::<Vec<_>>();
    if parsed.is_empty() {
        return SourceSelection::NoCandidates;
    }
    let mut sorted = parsed.clone();
    sorted.sort_by(|a, b| {
        b.failure_sequence
            .cmp(&a.failure_sequence)
            .then(b.artifact_sequence.cmp(&a.artifact_sequence))
            .then(b.write_sequence.cmp(&a.write_sequence))
            .then(b.producer_step_id.cmp(&a.producer_step_id))
    });
    // `sorted` is a non-empty clone of `parsed` (checked above), so `first`
    // always yields the deterministically-selected highest-sequence candidate.
    let selected = match sorted.first() {
        Some(candidate) => candidate.clone(),
        None => return SourceSelection::NoCandidates,
    };
    let candidate_views = parsed
        .iter()
        .map(|c| {
            json!({
                "artifact_family": c.artifact_family,
                "artifact_sequence": c.artifact_sequence,
                "write_sequence": c.write_sequence,
                "failure_sequence": c.failure_sequence,
                "producer_step_id": c.producer_step_id,
                "path": c.path,
                "history_path": c.history_path
            })
        })
        .collect::<Vec<_>>();
    SourceSelection::Selected(selected, candidate_views, "highest_failure_sequence")
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
    Some(SourceCandidate {
        artifact_family,
        artifact_sequence,
        write_sequence,
        failure_sequence,
        producer_step_id,
        step_order_index,
        path,
        history_path,
    })
}

/// Builds the terminal artifact payload conforming to the documented schema.
fn build_terminal_payload(
    binding: &PrFollowupBinding,
    retry_state: Option<&RetryState>,
    selection: &SourceSelection,
    clock: &dyn ClockSleeper,
) -> Value {
    let logged_at = clock.now_rfc3339();
    let budget_exhaustion = retry_state.and_then(exhausted_budget);
    let retry_metadata = retry_state.map(|state| {
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
    });
    match selection {
        SourceSelection::NoCandidates => {
            let terminal_reason = budget_exhaustion.unwrap_or("post_pr_failure");
            let idempotency_key = format!(
                "terminal:{}:{}:{}:{}",
                binding.run_id, binding.pr_number, binding.head_sha, terminal_reason
            );
            let mut payload = json!({
                "terminal_state": "fatal",
                "terminal_reason": terminal_reason,
                "failure_reason": terminal_reason,
                "failed_step": "post_pr_failure_terminal",
                "source_artifacts": [],
                "selected_source_reason": "no_failure_candidates",
                "idempotency_key": idempotency_key,
                "logged_at": logged_at
            });
            if let Some(reason) = budget_exhaustion {
                payload["exhausted_budget"] = json!(reason);
            }
            merge_retry_metadata(&mut payload, &retry_metadata);
            payload
        }
        SourceSelection::Selected(source, candidates, selection_reason) => {
            let terminal_reason = budget_exhaustion.unwrap_or("selected_source_failure");
            let idempotency_key = format!(
                "terminal:{}:{}:{}:{}:{}",
                binding.run_id,
                binding.pr_number,
                binding.head_sha,
                source.failure_sequence,
                source.artifact_sequence
            );
            let mut payload = json!({
                "terminal_state": "fatal",
                "terminal_reason": terminal_reason,
                "failure_reason": terminal_reason,
                "failed_step": source.producer_step_id,
                "source_artifacts": candidates,
                "source_failure_sequence": source.failure_sequence,
                "source_artifact_sequence": source.artifact_sequence,
                "source_write_sequence": source.write_sequence,
                "source_producer_step_id": source.producer_step_id,
                "source_step_order_index": source.step_order_index,
                "source_artifact_path": source.path,
                "source_history_path": source.history_path,
                "selected_source_reason": selection_reason,
                "idempotency_key": idempotency_key,
                "logged_at": logged_at
            });
            if let Some(reason) = budget_exhaustion {
                payload["exhausted_budget"] = json!(reason);
            }
            merge_retry_metadata(&mut payload, &retry_metadata);
            payload
        }
    }
}

fn exhausted_budget(state: &RetryState) -> Option<&'static str> {
    if state.counters.remediation_attempt_index >= state.budget.max_remediation_attempts {
        Some("remediation_attempts_exhausted")
    } else if state.counters.validation_retry_index >= state.budget.max_validation_retries {
        Some("validation_retries_exhausted")
    } else if state.counters.stale_artifact_retry_index >= state.budget.max_stale_artifact_retries {
        Some("stale_artifact_retries_exhausted")
    } else {
        None
    }
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
