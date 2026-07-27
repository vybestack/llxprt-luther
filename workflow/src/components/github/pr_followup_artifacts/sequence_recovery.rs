//! Run-wide artifact sequence recovery and validation.
//!
//! Strict recovery validates a contiguous immutable ledger for normal writes.
//! Resilient recovery reserves filename high-water marks while isolating corrupt
//! snapshots so terminal publication cannot reuse a sequence. Both modes bound
//! memory per artifact file rather than imposing a lifetime cap on valid history.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::*;

/// Recovered sequence state derived from accepted same-run snapshots.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
#[derive(Debug, Default)]
pub(super) struct RecoveredSequenceState {
    pub(super) max_artifact_sequence: u64,
    pub(super) max_failure_sequence: u64,
    pub(super) max_write_sequence_by_family: BTreeMap<String, u64>,
    seen_artifact_sequences: BTreeSet<u64>,
    seen_failure_sequences: BTreeSet<u64>,
    seen_family_writes: BTreeSet<(String, u64)>,
    seen_writes_by_family: BTreeMap<String, BTreeSet<u64>>,
}

struct CanonicalHistoryBackfill {
    artifact_sequence: u64,
    path: PathBuf,
    bytes: Vec<u8>,
}

impl PrFollowupArtifactStore {
    pub fn next_sequence_for_step(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
        producer_step_id: &str,
    ) -> Result<ArtifactSequenceMetadata, EngineError> {
        self.with_binding_publication_lock(binding, || {
            let state = self.recover_sequence_state(binding, Some(artifact_family))?;
            Ok(ArtifactSequenceMetadata {
                artifact_sequence: checked_next_sequence(
                    state.max_artifact_sequence,
                    "artifact_sequence",
                )?,
                write_sequence: checked_next_sequence(
                    state
                        .max_write_sequence_by_family
                        .get(artifact_family)
                        .copied()
                        .unwrap_or_default(),
                    "write_sequence",
                )?,
                producer_step_id: producer_step_id.to_string(),
            })
        })
    }

    pub fn next_failure_sequence(&self, binding: &PrFollowupBinding) -> Result<u64, EngineError> {
        self.with_binding_publication_lock(binding, || {
            let state = self.recover_sequence_state(binding, None)?;
            checked_next_sequence(state.max_failure_sequence, "failure_sequence")
        })
    }

    fn validate_sequence_artifact_value(
        &self,
        expected: &PrFollowupBinding,
        artifact_family: &str,
        value: &Value,
    ) -> Result<(), EngineError> {
        let actual = binding_from_value(value)?;
        if !self.validate_sequence_binding(expected, &actual) {
            return Err(artifact_error("artifact binding mismatch"));
        }
        self.validate_artifact_metadata(artifact_family, value)
    }

    fn validate_sequence_binding(
        &self,
        expected: &PrFollowupBinding,
        actual: &PrFollowupBinding,
    ) -> bool {
        expected.schema_version == actual.schema_version
            && expected.schema_version != 0
            && expected.run_id == actual.run_id
            && !expected.run_id.is_empty()
            && expected.repository_owner == actual.repository_owner
            && !expected.repository_owner.is_empty()
            && expected.repository_name == actual.repository_name
            && !expected.repository_name.is_empty()
            && expected.pr_number == actual.pr_number
            && expected.pr_number != 0
            && expected.head_ref == actual.head_ref
            && !expected.head_ref.is_empty()
            && !expected.head_sha.is_empty()
            && !actual.head_sha.is_empty()
            && expected.base_ref == actual.base_ref
            && !expected.base_ref.is_empty()
            && expected.base_sha.as_ref().is_none_or(|sha| !sha.is_empty())
            && actual.base_sha.as_ref().is_none_or(|sha| !sha.is_empty())
    }
    pub(super) fn recover_sequence_state(
        &self,
        binding: &PrFollowupBinding,
        consumed_family: Option<&str>,
    ) -> Result<RecoveredSequenceState, EngineError> {
        let mut state = RecoveredSequenceState::default();
        let mut backfills = Vec::new();
        let mut last_path = self.recover_strict_history(binding, &mut state)?;
        let current_files = self.current_sequence_files(binding)?;
        if let Some(path) = last_path.as_deref() {
            state.validate_monotonic_sequence_contiguity(path)?;
        }
        for file in current_files {
            let path = file.path;
            let current_family = path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            let value = match serde_json::from_str::<Value>(&file.content) {
                Ok(value) => value,
                Err(_) if consumed_family == Some(current_family) => continue,
                Err(error) => {
                    return Err(artifact_error(format!("parse {}: {error}", path.display())))
                }
            };
            if consumed_family == Some(current_family)
                && self
                    .validate_artifact_value(binding, current_family, &value)
                    .is_err()
            {
                continue;
            }
            let Some(family) = artifact_family_from_value(&value) else {
                continue;
            };
            if family != current_family
                || self
                    .validate_sequence_artifact_value(binding, &family, &value)
                    .and_then(|()| validate_canonical_embedded_path(&value, &path))
                    .is_err()
            {
                continue;
            }
            let actual_binding = binding_from_value(&value)?;
            if let Some(backfill) =
                state.reconcile_canonical(self, &actual_binding, &family, &value, &path)?
            {
                backfills.push(backfill);
            }
            last_path = Some(path);
        }
        if let Some(path) = last_path.as_deref() {
            state.validate_monotonic_sequence_contiguity(path)?;
        }
        backfills.sort_by_key(|backfill| backfill.artifact_sequence);
        for backfill in backfills {
            super::path_safety::durable_create_new(&self.root, &backfill.path, &backfill.bytes)?;
        }
        Ok(state)
    }

    fn recover_strict_history(
        &self,
        binding: &PrFollowupBinding,
        state: &mut RecoveredSequenceState,
    ) -> Result<Option<PathBuf>, EngineError> {
        let history_root = self.history_binding_root(binding);
        if !path_safety::validate_contained_directory(&self.root, &history_root)? {
            return Ok(None);
        }
        let mut last_path = None;
        path_safety::visit_contained_json_files_with_budget(
            &self.root,
            &history_root,
            &mut path_safety::ReadBudget::without_aggregate_limit(),
            |file| {
                let path = file.path;
                let value = serde_json::from_str::<Value>(&file.content).map_err(|error| {
                    artifact_error(format!("parse {}: {error}", path.display()))
                })?;
                let family = artifact_family_from_value(&value).ok_or_else(|| {
                    artifact_error(format!(
                        "missing history_metadata.artifact_family in {}",
                        path.display()
                    ))
                })?;
                self.validate_sequence_artifact_value(binding, &family, &value)?;
                self.validate_history_snapshot(&family, &value, &path)?;
                validate_history_filename(&family, &value, &path)?;
                validate_history_embedded_path(&value, &path)?;
                state.accept_snapshot(&family, &value, &path)?;
                last_path = Some(path);
                Ok(())
            },
        )?;
        Ok(last_path)
    }

    fn prepare_canonical_history_backfill(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
        value: &Value,
    ) -> Result<CanonicalHistoryBackfill, EngineError> {
        let history_path = value
            .pointer("/history_metadata/history_path")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .ok_or_else(|| {
                artifact_error("canonical artifact is missing immutable history path")
            })?;
        let expected_parent = self.history_root_for_family(binding, artifact_family);
        if history_path.parent() != Some(expected_parent.as_path()) {
            return Err(artifact_error(format!(
                "canonical-only history path is outside the exact family root: {}",
                history_path.display()
            )));
        }
        self.validate_history_snapshot(artifact_family, value, &history_path)?;
        validate_history_filename(artifact_family, value, &history_path)?;
        let bytes = serde_json::to_vec_pretty(value).map_err(|error| {
            artifact_error(format!("serialize canonical history backfill: {error}"))
        })?;
        super::path_safety::validate_publication_size(&history_path, &bytes)?;
        Ok(CanonicalHistoryBackfill {
            artifact_sequence: require_u64(value, "artifact_sequence")?,
            path: history_path,
            bytes,
        })
    }

    /// Recovers allocation high-water marks while isolating corrupt candidate
    /// contents. Every observed immutable filename reserves its embedded
    /// artifact/write sequence, so a corrupt snapshot can create a gap but can
    /// never cause sequence reuse. Read and directory errors remain fatal.
    pub(super) fn recover_resilient_sequence_state(
        &self,
        binding: &PrFollowupBinding,
    ) -> Result<RecoveredSequenceState, EngineError> {
        let mut state = RecoveredSequenceState::default();
        let history_root = self.history_binding_root(binding);
        if path_safety::validate_contained_directory(&self.root, &history_root)? {
            path_safety::visit_contained_json_files_with_budget(
                &self.root,
                &history_root,
                &mut path_safety::ReadBudget::without_aggregate_limit(),
                |file| {
                    let path = file.path;
                    reserve_filename_sequences(&mut state, &path);
                    let Ok(value) = serde_json::from_str::<Value>(&file.content) else {
                        return Ok(());
                    };
                    let Some(family) = artifact_family_from_value(&value) else {
                        return Ok(());
                    };
                    if self
                        .validate_sequence_artifact_value(binding, &family, &value)
                        .and_then(|()| self.validate_history_snapshot(&family, &value, &path))
                        .and_then(|()| validate_history_filename(&family, &value, &path))
                        .and_then(|()| validate_history_embedded_path(&value, &path))
                        .is_ok()
                    {
                        reserve_valid_failure_sequence(&mut state, &value);
                    }
                    Ok(())
                },
            )?;
        }
        self.reserve_resilient_current_sequences(binding, &mut state)?;
        Ok(state)
    }

    fn reserve_resilient_current_sequences(
        &self,
        binding: &PrFollowupBinding,
        state: &mut RecoveredSequenceState,
    ) -> Result<(), EngineError> {
        for file in self.current_sequence_files(binding)? {
            self.reserve_resilient_current_file(binding, state, file)?;
        }
        Ok(())
    }

    fn reserve_resilient_current_file(
        &self,
        binding: &PrFollowupBinding,
        state: &mut RecoveredSequenceState,
        file: path_safety::ContainedFile,
    ) -> Result<(), EngineError> {
        let Ok(value) = serde_json::from_str::<Value>(&file.content) else {
            return Ok(());
        };
        let Some(family) = artifact_family_from_value(&value) else {
            return Ok(());
        };
        if file.path.file_stem().and_then(|name| name.to_str()) == Some(&family)
            && self
                .validate_sequence_artifact_value(binding, &family, &value)
                .and_then(|()| validate_canonical_embedded_path(&value, &file.path))
                .is_ok()
        {
            reserve_valid_current_sequences(state, &family, &value);
        }
        Ok(())
    }

    fn current_sequence_files(
        &self,
        binding: &PrFollowupBinding,
    ) -> Result<Vec<path_safety::ContainedFile>, EngineError> {
        let current_root = self.current_binding_root(binding);
        if !path_safety::validate_contained_directory(&self.root, &current_root)? {
            return Ok(Vec::new());
        }
        let mut files = path_safety::read_contained_json_directory_with_budget(
            &self.root,
            &current_root,
            &mut path_safety::ReadBudget::without_aggregate_limit(),
        )?;
        files.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(files)
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
fn reserve_filename_sequences(state: &mut RecoveredSequenceState, path: &Path) {
    let Some(stem) = path.file_stem().and_then(|name| name.to_str()) else {
        return;
    };
    let mut parts = stem.splitn(3, '-');
    let artifact_sequence = parts.next().and_then(|part| part.parse::<u64>().ok());
    let write_sequence = parts.next().and_then(|part| part.parse::<u64>().ok());
    if let Some(artifact_sequence) = artifact_sequence.filter(|sequence| *sequence > 0) {
        state.max_artifact_sequence = state.max_artifact_sequence.max(artifact_sequence);
    }
    let family = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str());
    if let (Some(family), Some(write_sequence)) =
        (family, write_sequence.filter(|sequence| *sequence > 0))
    {
        state
            .max_write_sequence_by_family
            .entry(family.to_string())
            .and_modify(|maximum| *maximum = (*maximum).max(write_sequence))
            .or_insert(write_sequence);
    }
}

fn reserve_valid_failure_sequence(state: &mut RecoveredSequenceState, value: &Value) {
    if let Some(sequence) = value
        .get("failure_sequence")
        .and_then(Value::as_u64)
        .filter(|sequence| *sequence > 0)
    {
        state.max_failure_sequence = state.max_failure_sequence.max(sequence);
    }
}

fn reserve_valid_current_sequences(
    state: &mut RecoveredSequenceState,
    artifact_family: &str,
    value: &Value,
) {
    if let Some(sequence) = value
        .get("artifact_sequence")
        .and_then(Value::as_u64)
        .filter(|sequence| *sequence > 0)
    {
        state.max_artifact_sequence = state.max_artifact_sequence.max(sequence);
    }
    if let Some(sequence) = value
        .get("write_sequence")
        .and_then(Value::as_u64)
        .filter(|sequence| *sequence > 0)
    {
        state
            .max_write_sequence_by_family
            .entry(artifact_family.to_string())
            .and_modify(|maximum| *maximum = (*maximum).max(sequence))
            .or_insert(sequence);
    }
    reserve_valid_failure_sequence(state, value);
}

impl RecoveredSequenceState {
    fn reconcile_canonical(
        &mut self,
        store: &PrFollowupArtifactStore,
        binding: &PrFollowupBinding,
        artifact_family: &str,
        value: &Value,
        path: &Path,
    ) -> Result<Option<CanonicalHistoryBackfill>, EngineError> {
        let artifact_sequence = require_u64(value, "artifact_sequence")?;
        let write_sequence = require_u64(value, "write_sequence")?;
        let artifact_seen = self.seen_artifact_sequences.contains(&artifact_sequence);
        let write_seen = self
            .seen_family_writes
            .contains(&(artifact_family.to_string(), write_sequence));
        match (artifact_seen, write_seen) {
            (true, true) => Ok(None),
            (false, false) => {
                let backfill =
                    store.prepare_canonical_history_backfill(binding, artifact_family, value)?;
                self.accept_snapshot(artifact_family, value, path)?;
                Ok(Some(backfill))
            }
            _ => Err(artifact_error(format!(
                "canonical sequence identity is inconsistent with immutable history in {}",
                path.display()
            ))),
        }
    }

    fn accept_snapshot(
        &mut self,
        artifact_family: &str,
        value: &Value,
        path: &Path,
    ) -> Result<(), EngineError> {
        let artifact_sequence = require_u64(value, "artifact_sequence")?;
        let write_sequence = require_u64(value, "write_sequence")?;
        if artifact_sequence == 0 || write_sequence == 0 {
            return Err(artifact_error(format!(
                "sequence values must start at one in {}",
                path.display()
            )));
        }
        if !self.seen_artifact_sequences.insert(artifact_sequence) {
            return Err(artifact_error(format!(
                "duplicate artifact_sequence {artifact_sequence} in {}",
                path.display()
            )));
        }
        if !self
            .seen_family_writes
            .insert((artifact_family.to_string(), write_sequence))
        {
            return Err(artifact_error(format!(
                "duplicate write_sequence {write_sequence} for {artifact_family} in {}",
                path.display()
            )));
        }
        if let Some(failure_sequence) = value.get("failure_sequence").and_then(Value::as_u64) {
            if failure_sequence == 0 || !self.seen_failure_sequences.insert(failure_sequence) {
                return Err(artifact_error(format!(
                    "duplicate or zero failure_sequence {failure_sequence} in {}",
                    path.display()
                )));
            }
            self.max_failure_sequence = self.max_failure_sequence.max(failure_sequence);
        }
        self.max_artifact_sequence = self.max_artifact_sequence.max(artifact_sequence);
        self.max_write_sequence_by_family
            .entry(artifact_family.to_string())
            .and_modify(|max| *max = (*max).max(write_sequence))
            .or_insert(write_sequence);
        self.seen_writes_by_family
            .entry(artifact_family.to_string())
            .or_default()
            .insert(write_sequence);
        Ok(())
    }

    fn validate_monotonic_sequence_contiguity(&self, path: &Path) -> Result<(), EngineError> {
        require_contiguous_sequence(
            "artifact_sequence",
            &self.seen_artifact_sequences,
            self.max_artifact_sequence,
            path,
        )?;
        for (artifact_family, write_sequences) in &self.seen_writes_by_family {
            let max_write_sequence = self
                .max_write_sequence_by_family
                .get(artifact_family)
                .copied()
                .unwrap_or_default();
            require_contiguous_sequence(
                &format!("write_sequence for {artifact_family}"),
                write_sequences,
                max_write_sequence,
                path,
            )?;
        }
        require_contiguous_sequence(
            "failure_sequence",
            &self.seen_failure_sequences,
            self.max_failure_sequence,
            path,
        )?;
        Ok(())
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
fn require_contiguous_sequence(
    sequence_name: &str,
    seen_sequences: &BTreeSet<u64>,
    max_sequence: u64,
    path: &Path,
) -> Result<(), EngineError> {
    let mut expected = 1_u64;
    for sequence in seen_sequences {
        if *sequence != expected {
            return Err(artifact_error(format!(
                "non-monotonic {sequence_name}: missing sequence {expected} before {max_sequence} in {}",
                path.display()
            )));
        }
        expected = expected.checked_add(1).ok_or_else(|| {
            artifact_error(format!(
                "{sequence_name} validation overflow in {}",
                path.display()
            ))
        })?;
    }
    if seen_sequences.last().copied().unwrap_or_default() != max_sequence {
        return Err(artifact_error(format!(
            "non-monotonic {sequence_name}: maximum mismatch before {max_sequence} in {}",
            path.display()
        )));
    }
    Ok(())
}
