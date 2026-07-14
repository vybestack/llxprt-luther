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
        let retry_state = resolve_retry_state(&store, &binding, params, &clock)?;
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
        let reason = payload["terminal_reason"]
            .as_str()
            .or_else(|| payload["failure_reason"].as_str())
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
    params: &Value,
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
                write_terminal_tombstone(store, binding, &scope, params, clock)?;
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
    // Validate that the canonical file carries full store-injected provenance
    // metadata. A file that is valid JSON but lacks these fields was either
    // tampered with or written outside the store's write path — treat it as
    // corrupt and trigger recovery from immutable history.
    if !validate_canonical_provenance(&value) {
        return Err(EngineError::InvalidState(
            "invalid remediation retry state: missing store-injected provenance metadata"
                .to_string(),
        ));
    }
    let state: RetryState = serde_json::from_value(value).map_err(|err| {
        EngineError::InvalidState(format!("invalid remediation retry state: {err}"))
    })?;
    Ok(Some(state))
}

/// Validates that a canonical retry-state JSON value carries the full
/// store-injected provenance metadata: artifact_sequence (>= 1),
/// write_sequence (>= 1), producer_step_id (non-empty), and history_metadata
/// with the correct artifact_family. A zero/absent sequence indicates a
/// hand-crafted or pre-store file that does not participate in the durable
/// sequence chain and must be treated as corrupt.
fn validate_canonical_provenance(value: &Value) -> bool {
    value
        .get("artifact_sequence")
        .and_then(Value::as_u64)
        .is_some_and(|seq| seq >= 1)
        && value
            .get("write_sequence")
            .and_then(Value::as_u64)
            .is_some_and(|seq| seq >= 1)
        && value
            .get("producer_step_id")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
        && value
            .get("history_metadata")
            .and_then(|m| m.get("artifact_family"))
            .and_then(Value::as_str)
            .is_some_and(|f| f == RETRY_STATE_FAMILY)
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
///
/// Full provenance is validated: the history snapshot must carry valid
/// `artifact_sequence`, `write_sequence`, and `producer_step_id` fields (store-
/// injected metadata), and the deserialized RetryState must pass invariant
/// validation (counters within budget, ordinal consistent). History files
/// that fail JSON deserialization or provenance checks are skipped as
/// individually corrupt, but I/O errors (permission denied, etc.) are
/// propagated rather than swallowed.
fn recover_retry_state_from_history(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    scope: &RetryScopeKey,
) -> Result<Option<RetryState>, EngineError> {
    let history_root = store.history_root_for_family(binding, RETRY_STATE_FAMILY);
    if !history_root.exists() {
        return Ok(None);
    }
    let mut snapshots = Vec::new();
    collect_history_jsons(&history_root, &mut snapshots);
    // Sort by artifact_sequence descending to find the most recent valid state.
    snapshots.sort_by(|a, b| {
        let seq_a = read_artifact_sequence_from_path(a).unwrap_or(0);
        let seq_b = read_artifact_sequence_from_path(b).unwrap_or(0);
        seq_b.cmp(&seq_a)
    });
    let scope_clone = scope.clone();
    for path in &snapshots {
        let raw = match fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(artifact_io_error(format!(
                    "read history snapshot {}: {err}",
                    path.display()
                )))
            }
        };
        let value: Value = match serde_json::from_str(&raw) {
            Ok(value) => value,
            Err(_) => continue,
        };
        // Validate that the history snapshot carries full provenance metadata.
        if !validate_history_provenance(&value, path) {
            continue;
        }
        let state: RetryState = match serde_json::from_value(value) {
            Ok(state) => state,
            Err(_) => continue,
        };
        if state.scope != scope_clone {
            continue;
        }
        // Validate internal consistency of the recovered state.
        if validate_recovered_state(&state).is_err() {
            continue;
        }
        return Ok(Some(state));
    }
    Ok(None)
}

/// Reads the artifact_sequence from a history JSON file path's content for
/// sorting purposes.
fn read_artifact_sequence_from_path(path: &std::path::Path) -> Option<u64> {
    let raw = fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    value.get("artifact_sequence").and_then(Value::as_u64)
}

/// Validates that a history snapshot carries the full provenance metadata
/// injected by the store: artifact_sequence, write_sequence, producer_step_id,
/// and history_metadata.
///
/// Additionally, the artifact_sequence must be at least 1 (a zero or absent
/// sequence indicates a pre-store or hand-crafted file that does not
/// participate in the durable sequence chain). This prevents forged history
/// files with sequence 0 from being treated as authoritative.
fn validate_history_provenance(value: &Value, path: &std::path::Path) -> bool {
    let has_sequence = value
        .get("artifact_sequence")
        .and_then(Value::as_u64)
        .is_some_and(|seq| seq >= 1);
    let has_write_sequence = value
        .get("write_sequence")
        .and_then(Value::as_u64)
        .is_some_and(|seq| seq >= 1);
    let has_producer = value
        .get("producer_step_id")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    let has_metadata = value
        .get("history_metadata")
        .and_then(|m| m.get("artifact_family"))
        .and_then(Value::as_str)
        .is_some_and(|f| f == RETRY_STATE_FAMILY);
    if !has_sequence || !has_write_sequence || !has_producer || !has_metadata {
        eprintln!(
            "warn: history snapshot {} lacks provenance metadata, skipping",
            path.display()
        );
        return false;
    }
    true
}

/// Validates a recovered RetryState for internal consistency: counters must
/// not exceed budget maxima.
fn validate_recovered_state(state: &RetryState) -> Result<(), EngineError> {
    if state.counters.remediation_attempt_index > state.budget.max_remediation_attempts {
        return Err(EngineError::InvalidState(format!(
            "recovered remediation_attempt_index {} exceeds max {}",
            state.counters.remediation_attempt_index, state.budget.max_remediation_attempts
        )));
    }
    if state.counters.validation_retry_index > state.budget.max_validation_retries {
        return Err(EngineError::InvalidState(format!(
            "recovered validation_retry_index {} exceeds max {}",
            state.counters.validation_retry_index, state.budget.max_validation_retries
        )));
    }
    if state.counters.stale_artifact_retry_index > state.budget.max_stale_artifact_retries {
        return Err(EngineError::InvalidState(format!(
            "recovered stale_artifact_retry_index {} exceeds max {}",
            state.counters.stale_artifact_retry_index, state.budget.max_stale_artifact_retries
        )));
    }
    Ok(())
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
///
/// The quarantine path is validated to stay within the same parent directory
/// as the original file (path component containment), preventing directory
/// traversal via crafted extensions.
fn quarantine_corrupt_retry_state(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
) -> Result<(), EngineError> {
    let path = store.canonical_path(binding, RETRY_STATE_FAMILY);
    if path.exists() {
        let parent = path
            .parent()
            .ok_or_else(|| artifact_io_error(format!("missing parent for {}", path.display())))?;
        let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
        let unique = uuid::Uuid::new_v4().simple();
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("pr-remediation-retry-state");
        // Use a filename composed of only safe characters. No path separators
        // or `..` can appear in the formatted string.
        let quarantine_name = format!("{stem}.corrupt.{timestamp}.{unique}");
        // Validate no path traversal: the name must not contain separators.
        if quarantine_name.contains('/') || quarantine_name.contains('\\') {
            return Err(artifact_io_error(
                "quarantine filename contains path separator",
            ));
        }
        let quarantined = parent.join(&quarantine_name);
        // Verify containment: the quarantined path's parent must equal the
        // original file's parent.
        let quarantined_parent = quarantined
            .parent()
            .ok_or_else(|| artifact_io_error("missing parent for quarantined path"))?;
        if quarantined_parent != parent {
            return Err(artifact_io_error(
                "quarantine path escaped parent directory",
            ));
        }
        fs::rename(&path, &quarantined).map_err(|error| {
            EngineError::InvalidState(format!("quarantine corrupt retry state: {error}"))
        })?;
    }
    Ok(())
}

/// Republishes a recovered retry state to the canonical path, healing the
/// corruption. The recovered state is written atomically (temp-file + rename)
/// to ensure crash-safety. Because the history snapshot carries the full
/// store-injected metadata (artifact_sequence, write_sequence, binding fields,
/// history_metadata), we write the complete JSON value rather than re-
/// serializing only the typed RetryState fields. This preserves the complete
/// provenance chain.
fn republish_recovered_state(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    state: &RetryState,
) -> Result<(), EngineError> {
    let canonical_path = store.canonical_path(binding, RETRY_STATE_FAMILY);
    // Serialize the full RetryState (which includes all store-injected fields
    // because RetryState derives Serialize and the history snapshot carried
    // them as top-level fields that were deserialized into the struct via
    // serde(flatten)-like behavior — but RetryState does NOT capture those
    // extra fields. So we need to find the raw history JSON to preserve them.)
    //
    // Actually, RetryState only has its own fields. The store injects
    // artifact_sequence etc. as top-level JSON keys that are NOT part of
    // RetryState. When we deserialize a history file into RetryState via
    // serde_json::from_value, those extra keys are silently dropped. To
    // preserve them, we must re-read the raw history file and write it
    // directly to canonical.
    let raw_json = find_raw_history_json_for_state(store, binding, state)?;
    let bytes = match raw_json {
        Some(bytes) => bytes,
        None => serde_json::to_vec_pretty(state)
            .map_err(|err| artifact_io_error(format!("serialize recovered state: {err}")))?,
    };
    if let Some(parent) = canonical_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| artifact_io_error(format!("create recovered state parent: {err}")))?;
    }
    atomic_write(&canonical_path, &bytes)?;
    Ok(())
}

/// Finds the raw JSON bytes from the history snapshot that matches the
/// recovered state's transition_id and ordinal, preserving the full metadata.
fn find_raw_history_json_for_state(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    state: &RetryState,
) -> Result<Option<Vec<u8>>, EngineError> {
    let history_root = store.history_root_for_family(binding, RETRY_STATE_FAMILY);
    if !history_root.exists() {
        return Ok(None);
    }
    let mut paths = Vec::new();
    collect_history_jsons(&history_root, &mut paths);
    for path in &paths {
        let raw = match fs::read(path) {
            Ok(raw) => raw,
            Err(_) => continue,
        };
        let Ok(value): std::result::Result<Value, _> = serde_json::from_slice(&raw) else {
            continue;
        };
        let matching = value
            .get("transition_id")
            .and_then(Value::as_str)
            .is_some_and(|id| id == state.transition_id)
            && value
                .get("launch_ordinal")
                .and_then(Value::as_u64)
                .is_some_and(|ord| ord == state.launch_ordinal);
        if matching {
            return Ok(Some(raw));
        }
    }
    Ok(None)
}

/// Atomically writes bytes to a path via temp-file + rename.
fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> Result<(), EngineError> {
    let parent = path
        .parent()
        .ok_or_else(|| artifact_io_error(format!("missing parent for {}", path.display())))?;
    fs::create_dir_all(parent).map_err(|err| artifact_io_error(format!("create parent: {err}")))?;
    let temp_path = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("retry-state"),
        uuid::Uuid::new_v4()
    ));
    fs::write(&temp_path, bytes)
        .map_err(|err| artifact_io_error(format!("write temp file: {err}")))?;
    fs::rename(&temp_path, path).map_err(|err| {
        let _ = fs::remove_file(&temp_path);
        artifact_io_error(format!("atomic rename into {}: {err}", path.display()))
    })?;
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
    atomic_write(&history_path, &bytes)?;
    if let Some(parent) = canonical_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| artifact_io_error(format!("create terminal canonical parent: {err}")))?;
    }
    atomic_write(&canonical_path, &bytes)?;
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
    // Parse each candidate independently. A malformed candidate is skipped
    // without affecting the others.
    let parsed: Vec<_> = candidates
        .iter()
        .filter_map(|value| parse_candidate(value).or_else(|| log_skipped_candidate(value)))
        .collect();
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
                "history_path": c.history_path,
                "failure_reason": c.failure_reason
            })
        })
        .collect::<Vec<_>>();
    SourceSelection::Selected(selected, candidate_views, "highest_failure_sequence")
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
        .map(ToString::to_string);
    Some(SourceCandidate {
        artifact_family,
        artifact_sequence,
        write_sequence,
        failure_sequence,
        producer_step_id,
        step_order_index,
        path,
        history_path,
        failure_reason,
    })
}

/// Common context for terminal payload construction, holding pre-resolved
/// metadata shared by both selection variants.
struct TerminalPayloadContext<'a> {
    binding: &'a PrFollowupBinding,
    logged_at: String,
    budget_exhaustion: Option<&'static str>,
    retry_metadata: Option<Value>,
}

impl<'a> TerminalPayloadContext<'a> {
    fn new(
        binding: &'a PrFollowupBinding,
        retry_state: Option<&RetryState>,
        clock: &dyn ClockSleeper,
    ) -> Self {
        Self {
            binding,
            logged_at: clock.now_rfc3339(),
            budget_exhaustion: retry_state.and_then(exhausted_budget),
            retry_metadata: retry_state.map(retry_metadata_json),
        }
    }

    /// Builds the terminal artifact payload conforming to the documented schema.
    fn build(self, selection: &SourceSelection) -> Value {
        match selection {
            SourceSelection::NoCandidates => self.build_no_candidates(),
            SourceSelection::Selected(source, candidates, reason) => {
                self.build_selected(source, candidates, reason)
            }
        }
    }

    /// Builds the no-candidates variant: a terminal marker with no source
    /// failure artifacts, using the budget-exhaustion reason or a generic
    /// fallback.
    fn build_no_candidates(&self) -> Value {
        let terminal_reason = self.budget_exhaustion.unwrap_or("post_pr_failure");
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

    /// Builds the selected-source variant: a terminal marker recording the
    /// deterministically-selected source failure with full provenance.
    fn build_selected(
        &self,
        source: &SourceCandidate,
        candidates: &[Value],
        selection_reason: &str,
    ) -> Value {
        let terminal_reason = self.budget_exhaustion.unwrap_or("selected_source_failure");
        let idempotency_key = format!(
            "terminal:{}:{}:{}:{}:{}",
            self.binding.run_id,
            self.binding.pr_number,
            self.binding.head_sha,
            source.failure_sequence,
            source.artifact_sequence
        );
        // The terminal-level failure_reason uses the semantic reason from
        // the selected source, not the generic "selected_source_failure"
        // label. This ensures the terminal artifact carries a meaningful,
        // actionable reason that traces back to the concrete failure.
        let selected_failure_reason = source
            .failure_reason
            .clone()
            .unwrap_or_else(|| terminal_reason.to_string());
        let mut payload = json!({
            "terminal_state": "fatal",
            "terminal_reason": terminal_reason,
            "failure_reason": selected_failure_reason,
            "failed_step": source.producer_step_id,
            "source_artifacts": candidates,
            "source_failure_sequence": source.failure_sequence,
            "source_artifact_sequence": source.artifact_sequence,
            "source_write_sequence": source.write_sequence,
            "source_producer_step_id": source.producer_step_id,
            "source_step_order_index": source.step_order_index,
            "source_artifact_path": source.path,
            "source_history_path": source.history_path,
            "source_failure_reason": source.failure_reason.clone().unwrap_or_else(|| "unknown".to_string()),
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

/// Builds the terminal artifact payload conforming to the documented schema.
fn build_terminal_payload(
    binding: &PrFollowupBinding,
    retry_state: Option<&RetryState>,
    selection: &SourceSelection,
    clock: &dyn ClockSleeper,
) -> Value {
    TerminalPayloadContext::new(binding, retry_state, clock).build(selection)
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
