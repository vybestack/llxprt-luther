//! PR follow-through artifact store and writer implementation.
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03

//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
//! @requirement:REQ-PRFU-002,REQ-PRFU-004,REQ-PRFU-020
//! @pseudocode lines 5-7

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value};

use crate::engine::executors::pr_followup_types::{ArtifactSequenceMetadata, PrFollowupBinding};
use crate::engine::runner::EngineError;

/// Filesystem seam for artifact store root canonicalization.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
pub trait PrFollowupFilesystem: Send + Sync {
    fn canonicalize_root(&self, path: &Path) -> Result<PathBuf, EngineError>;
}

/// System filesystem used by default artifact store construction.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
#[derive(Debug, Default)]
pub struct SystemPrFollowupFilesystem;

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
impl PrFollowupFilesystem for SystemPrFollowupFilesystem {
    fn canonicalize_root(&self, path: &Path) -> Result<PathBuf, EngineError> {
        fs::create_dir_all(path)
            .map_err(|err| artifact_error(format!("create artifact root: {err}")))?;
        path.canonicalize()
            .map_err(|err| artifact_error(format!("canonicalize artifact root: {err}")))
    }
}

/// Clock/sleeper abstraction for deterministic post-PR polling and artifact timestamps.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-020
/// @pseudocode lines 33-37
pub trait ClockSleeper: Send + Sync {
    fn now_rfc3339(&self) -> String;

    fn sleep(&self, duration: std::time::Duration);
}

/// Production clock/sleeper installed by default executor constructors.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-020
/// @pseudocode lines 33-37
#[derive(Debug, Default)]
pub struct SystemClockSleeper;

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-020
/// @pseudocode lines 33-37
impl ClockSleeper for SystemClockSleeper {
    fn now_rfc3339(&self) -> String {
        chrono::Utc::now().to_rfc3339()
    }

    fn sleep(&self, duration: std::time::Duration) {
        std::thread::sleep(duration);
    }
}

/// Artifact writer trait for canonical/history paths, sequence allocation, and binding validation.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002,REQ-PRFU-004
/// @pseudocode lines 5-7
pub trait ArtifactWriter: Send + Sync {
    fn canonical_path(&self, binding: &PrFollowupBinding, artifact_family: &str) -> PathBuf;

    fn history_path(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
        sequence: &ArtifactSequenceMetadata,
    ) -> PathBuf;

    fn next_sequence(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
    ) -> Result<ArtifactSequenceMetadata, EngineError>;

    fn validate_binding(&self, expected: &PrFollowupBinding, actual: &PrFollowupBinding) -> bool;
}

/// Metadata inserted into every artifact store write.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct HistoryMetadata {
    pub canonical_path: String,
    pub history_path: String,
    pub artifact_family: String,
    pub is_canonical: bool,
    pub history_written_at: String,
}

/// Result of one atomic canonical/history artifact write.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactWriteRecord {
    pub sequence: ArtifactSequenceMetadata,
    pub canonical_path: PathBuf,
    pub history_path: PathBuf,
    pub failure_sequence: Option<u64>,
}

/// PR follow-through artifact store implementation.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002,REQ-PRFU-004
/// @pseudocode lines 5-7
#[derive(Clone, Debug)]
pub struct PrFollowupArtifactStore {
    root: PathBuf,
}

/// Recovered sequence state derived from accepted same-run snapshots.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
#[derive(Debug, Default)]
struct RecoveredSequenceState {
    max_artifact_sequence: u64,
    max_failure_sequence: u64,
    max_write_sequence_by_family: BTreeMap<String, u64>,
    seen_artifact_sequences: BTreeSet<u64>,
    seen_failure_sequences: BTreeSet<u64>,
    seen_family_writes: BTreeSet<(String, u64)>,
    seen_writes_by_family: BTreeMap<String, BTreeSet<u64>>,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002,REQ-PRFU-004
/// @pseudocode lines 5-7
impl PrFollowupArtifactStore {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn with_filesystem(
        root: &Path,
        filesystem: &dyn PrFollowupFilesystem,
    ) -> Result<Self, EngineError> {
        Ok(Self::new(filesystem.canonicalize_root(root)?))
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn next_sequence_for_step(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
        producer_step_id: &str,
    ) -> Result<ArtifactSequenceMetadata, EngineError> {
        let state = self.recover_sequence_state(binding, Some(artifact_family))?;
        Ok(ArtifactSequenceMetadata {
            artifact_sequence: state.max_artifact_sequence + 1,
            write_sequence: state
                .max_write_sequence_by_family
                .get(artifact_family)
                .copied()
                .unwrap_or_default()
                + 1,
            producer_step_id: producer_step_id.to_string(),
        })
    }

    pub fn next_failure_sequence(&self, binding: &PrFollowupBinding) -> Result<u64, EngineError> {
        let state = self.recover_sequence_state(binding, None)?;
        Ok(state.max_failure_sequence + 1)
    }

    // Pre-existing artifact writer API shape shared by follow-up executors.
    #[allow(clippy::too_many_arguments)]
    pub fn write_json_artifact<T: Serialize>(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
        producer_step_id: &str,
        step_order_index: u64,
        payload: &T,
        failure: Option<(&str, &str, Value)>,
        clock: &dyn ClockSleeper,
    ) -> Result<ArtifactWriteRecord, EngineError> {
        let sequence = self.next_sequence_for_step(binding, artifact_family, producer_step_id)?;
        let failure_sequence = if failure.is_some() {
            Some(self.next_failure_sequence(binding)?)
        } else {
            None
        };
        let canonical_path = self.canonical_path(binding, artifact_family);
        let history_path = self.history_path(binding, artifact_family, &sequence);
        let mut value = serde_json::to_value(payload)
            .map_err(|err| artifact_error(format!("serialize artifact payload: {err}")))?;
        self.inject_store_fields(
            binding,
            artifact_family,
            &sequence,
            step_order_index,
            &canonical_path,
            &history_path,
            failure_sequence,
            failure,
            clock,
            &mut value,
        )?;
        validate_json_object(&value)?;
        let bytes = serde_json::to_vec_pretty(&value)
            .map_err(|err| artifact_error(format!("serialize artifact json: {err}")))?;
        atomic_write(&history_path, &bytes)?;
        atomic_write(&canonical_path, &bytes)?;
        Ok(ArtifactWriteRecord {
            sequence,
            canonical_path,
            history_path,
            failure_sequence,
        })
    }

    // Pre-existing artifact writer API shape shared by follow-up executors.
    #[allow(clippy::too_many_arguments)]
    pub fn write_raw_text_artifact(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
        producer_step_id: &str,
        step_order_index: u64,
        artifact_name: &str,
        raw_text: &str,
        clock: &dyn ClockSleeper,
    ) -> Result<ArtifactWriteRecord, EngineError> {
        let sequence = self.next_sequence_for_step(binding, artifact_family, producer_step_id)?;
        let canonical_path = self.canonical_path(binding, artifact_family);
        let history_path = self
            .history_binding_root(binding)
            .join(artifact_family)
            .join(format!(
                "{}-{}-{}-{}.json",
                sequence.artifact_sequence,
                sequence.write_sequence,
                sequence.producer_step_id,
                sanitize_path_segment(artifact_name)
            ));
        let value = serde_json::json!({
            "artifact_name": artifact_name,
            "raw_text": raw_text
        });
        let mut value = value;
        self.inject_store_fields(
            binding,
            artifact_family,
            &sequence,
            step_order_index,
            &canonical_path,
            &history_path,
            None,
            None,
            clock,
            &mut value,
        )?;
        validate_json_object(&value)?;
        let bytes = serde_json::to_vec_pretty(&value)
            .map_err(|err| artifact_error(format!("serialize raw text artifact json: {err}")))?;
        atomic_write(&history_path, &bytes)?;
        atomic_write(&canonical_path, &bytes)?;
        Ok(ArtifactWriteRecord {
            sequence,
            canonical_path,
            history_path,
            failure_sequence: None,
        })
    }

    pub fn read_current_json(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
    ) -> Result<Value, EngineError> {
        let path = self.canonical_path(binding, artifact_family);
        let value = read_json_file(&path)?;
        self.validate_artifact_value(binding, artifact_family, &value)?;
        Ok(value)
    }

    pub fn read_current_raw_json(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
    ) -> Result<Value, EngineError> {
        read_json_file(&self.canonical_path(binding, artifact_family))
    }

    pub fn validate_artifact_value(
        &self,
        expected: &PrFollowupBinding,
        artifact_family: &str,
        value: &Value,
    ) -> Result<(), EngineError> {
        let actual = binding_from_value(value)?;
        if !self.validate_binding(expected, &actual) {
            return Err(artifact_error("artifact binding mismatch"));
        }
        self.validate_artifact_metadata(artifact_family, value)
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
            && expected.base_sha == actual.base_sha
    }

    fn validate_artifact_metadata(
        &self,
        artifact_family: &str,
        value: &Value,
    ) -> Result<(), EngineError> {
        let metadata = value
            .get("history_metadata")
            .and_then(Value::as_object)
            .ok_or_else(|| artifact_error("missing history_metadata"))?;
        let metadata_family = metadata
            .get("artifact_family")
            .and_then(Value::as_str)
            .ok_or_else(|| artifact_error("missing history_metadata.artifact_family"))?;
        if metadata_family != artifact_family {
            return Err(artifact_error(format!(
                "artifact family mismatch: expected {artifact_family}, got {metadata_family}"
            )));
        }
        require_u64(value, "artifact_sequence")?;
        require_u64(value, "write_sequence")?;
        require_string(value, "producer_step_id")?;
        require_u64(value, "step_order_index")?;
        require_string_from_object(metadata, "canonical_path")?;
        require_string_from_object(metadata, "history_path")?;
        require_bool_from_object(metadata, "is_canonical")?;
        require_string_from_object(metadata, "history_written_at")?;
        Ok(())
    }

    // Pre-existing store metadata shape shared by artifact writers.
    #[allow(clippy::too_many_arguments)]
    fn inject_store_fields(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
        sequence: &ArtifactSequenceMetadata,
        step_order_index: u64,
        canonical_path: &Path,
        history_path: &Path,
        failure_sequence: Option<u64>,
        failure: Option<(&str, &str, Value)>,
        clock: &dyn ClockSleeper,
        value: &mut Value,
    ) -> Result<(), EngineError> {
        let object = value
            .as_object_mut()
            .ok_or_else(|| artifact_error("artifact payload must serialize to a JSON object"))?;
        insert_binding_fields(object, binding);
        object.insert(
            "artifact_sequence".to_string(),
            Value::from(sequence.artifact_sequence),
        );
        object.insert(
            "write_sequence".to_string(),
            Value::from(sequence.write_sequence),
        );
        object.insert(
            "producer_step_id".to_string(),
            Value::from(sequence.producer_step_id.clone()),
        );
        object.insert(
            "step_order_index".to_string(),
            Value::from(step_order_index),
        );
        object.insert(
            "history_metadata".to_string(),
            serde_json::to_value(HistoryMetadata {
                canonical_path: canonical_path.display().to_string(),
                history_path: history_path.display().to_string(),
                artifact_family: artifact_family.to_string(),
                is_canonical: true,
                history_written_at: clock.now_rfc3339(),
            })
            .map_err(|err| artifact_error(format!("serialize history metadata: {err}")))?,
        );
        if let (Some(next_failure_sequence), Some((semantic_state, failure_reason, details))) =
            (failure_sequence, failure)
        {
            object.insert("semantic_state".to_string(), Value::from(semantic_state));
            object.insert("failure_reason".to_string(), Value::from(failure_reason));
            object.insert(
                "failure_sequence".to_string(),
                Value::from(next_failure_sequence),
            );
            object.insert("produced_at".to_string(), Value::from(clock.now_rfc3339()));
            object.insert("failure_details".to_string(), details);
        }
        Ok(())
    }

    fn recover_sequence_state(
        &self,
        binding: &PrFollowupBinding,
        consumed_family: Option<&str>,
    ) -> Result<RecoveredSequenceState, EngineError> {
        let mut state = RecoveredSequenceState::default();
        let mut last_path = None;
        for path in self.sequence_candidate_paths(binding, consumed_family)? {
            let value = read_json_file(&path)?;
            if let Some(family) = consumed_family {
                if path == self.canonical_path(binding, family)
                    && self
                        .validate_artifact_value(binding, family, &value)
                        .is_err()
                {
                    continue;
                }
            }

            let family = artifact_family_from_value(&value).ok_or_else(|| {
                artifact_error(format!(
                    "missing history_metadata.artifact_family in {}",
                    path.display()
                ))
            })?;
            self.validate_sequence_artifact_value(binding, &family, &value)?;
            state.accept_snapshot(&family, &value, &path)?;
            last_path = Some(path);
        }
        if let Some(path) = last_path.as_deref() {
            state.validate_monotonic_sequence_contiguity(path)?;
        }
        Ok(state)
    }

    fn sequence_candidate_paths(
        &self,
        binding: &PrFollowupBinding,
        consumed_family: Option<&str>,
    ) -> Result<Vec<PathBuf>, EngineError> {
        let mut paths = Vec::new();
        let mut history_families = BTreeSet::new();
        let history_root = self.history_binding_root(binding);
        if history_root.exists() {
            collect_json_paths(&history_root, &mut paths)?;
            for path in &paths {
                if let Some(family) = path
                    .parent()
                    .and_then(Path::file_name)
                    .and_then(|name| name.to_str())
                {
                    history_families.insert(family.to_string());
                }
            }
        }
        match consumed_family {
            Some(family) => {
                let current = self.canonical_path(binding, family);
                if current.exists() && !history_families.contains(family) {
                    paths.push(current);
                }
            }
            None => {
                let current_root = self
                    .root
                    .join("pr-followup")
                    .join("current")
                    .join(&binding.run_id)
                    .join(&binding.repository_owner)
                    .join(&binding.repository_name)
                    .join(binding.pr_number.to_string());
                if current_root.exists() {
                    for entry in fs::read_dir(current_root)
                        .map_err(|err| artifact_error(format!("read current dir: {err}")))?
                    {
                        let path = entry
                            .map_err(|err| {
                                artifact_error(format!("read current dir entry: {err}"))
                            })?
                            .path();
                        let family = path
                            .file_stem()
                            .and_then(|name| name.to_str())
                            .unwrap_or_default();
                        if path.extension().and_then(|ext| ext.to_str()) == Some("json")
                            && !history_families.contains(family)
                        {
                            paths.push(path);
                        }
                    }
                }
            }
        }
        paths.sort();
        Ok(paths)
    }

    fn history_binding_root(&self, binding: &PrFollowupBinding) -> PathBuf {
        self.root
            .join("pr-followup")
            .join("history")
            .join(&binding.run_id)
            .join(&binding.repository_owner)
            .join(&binding.repository_name)
            .join(binding.pr_number.to_string())
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
impl RecoveredSequenceState {
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
/// @requirement:REQ-PRFU-002,REQ-PRFU-004
/// @pseudocode lines 5-7
impl ArtifactWriter for PrFollowupArtifactStore {
    fn canonical_path(&self, binding: &PrFollowupBinding, artifact_family: &str) -> PathBuf {
        self.root
            .join("pr-followup")
            .join("current")
            .join(&binding.run_id)
            .join(&binding.repository_owner)
            .join(&binding.repository_name)
            .join(binding.pr_number.to_string())
            .join(format!("{artifact_family}.json"))
    }

    fn history_path(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
        sequence: &ArtifactSequenceMetadata,
    ) -> PathBuf {
        self.history_binding_root(binding)
            .join(artifact_family)
            .join(format!(
                "{}-{}-{}.json",
                sequence.artifact_sequence, sequence.write_sequence, sequence.producer_step_id
            ))
    }

    fn next_sequence(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
    ) -> Result<ArtifactSequenceMetadata, EngineError> {
        self.next_sequence_for_step(binding, artifact_family, artifact_family)
    }

    fn validate_binding(&self, expected: &PrFollowupBinding, actual: &PrFollowupBinding) -> bool {
        expected == actual
            && expected.schema_version != 0
            && !expected.run_id.is_empty()
            && !expected.repository_owner.is_empty()
            && !expected.repository_name.is_empty()
            && expected.pr_number != 0
            && !expected.head_ref.is_empty()
            && !expected.head_sha.is_empty()
            && !expected.base_ref.is_empty()
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), EngineError> {
    let parent = path
        .parent()
        .ok_or_else(|| artifact_error(format!("missing parent for {}", path.display())))?;
    fs::create_dir_all(parent).map_err(|err| artifact_error(format!("create parent: {err}")))?;
    let temp_path = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("artifact"),
        uuid::Uuid::new_v4()
    ));
    fs::write(&temp_path, bytes)
        .map_err(|err| artifact_error(format!("write temp file: {err}")))?;
    fs::rename(&temp_path, path).map_err(|err| {
        let _ = fs::remove_file(&temp_path);
        artifact_error(format!("atomic rename into {}: {err}", path.display()))
    })?;
    Ok(())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
fn collect_json_paths(root: &Path, paths: &mut Vec<PathBuf>) -> Result<(), EngineError> {
    for entry in fs::read_dir(root).map_err(|err| artifact_error(format!("read dir: {err}")))? {
        let entry = entry.map_err(|err| artifact_error(format!("read dir entry: {err}")))?;
        let path = entry.path();
        if path.is_dir() {
            collect_json_paths(&path, paths)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            paths.push(path);
        }
    }
    Ok(())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
fn read_json_file(path: &Path) -> Result<Value, EngineError> {
    let content = fs::read_to_string(path)
        .map_err(|err| artifact_error(format!("read {}: {err}", path.display())))?;
    serde_json::from_str(&content)
        .map_err(|err| artifact_error(format!("parse {}: {err}", path.display())))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
fn validate_json_object(value: &Value) -> Result<(), EngineError> {
    if value.is_object() {
        Ok(())
    } else {
        Err(artifact_error("artifact JSON must be an object"))
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002,REQ-PRFU-004
/// @pseudocode lines 5-7
fn binding_from_value(value: &Value) -> Result<PrFollowupBinding, EngineError> {
    Ok(PrFollowupBinding {
        schema_version: u32::try_from(require_u64(value, "schema_version")?)
            .map_err(|err| artifact_error(format!("schema_version out of range: {err}")))?,
        run_id: require_string(value, "run_id")?,
        repository_owner: require_string(value, "repository_owner")?,
        repository_name: require_string(value, "repository_name")?,
        pr_number: require_u64(value, "pr_number")?,
        head_ref: require_string(value, "head_ref")?,
        head_sha: require_string(value, "head_sha")?,
        base_ref: require_string(value, "base_ref")?,
        base_sha: value
            .get("base_sha")
            .map(|base_sha| {
                if base_sha.is_null() {
                    Ok(None)
                } else {
                    base_sha
                        .as_str()
                        .map(|value| Some(value.to_string()))
                        .ok_or_else(|| artifact_error("base_sha must be string or null"))
                }
            })
            .transpose()?
            .flatten(),
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
fn artifact_family_from_value(value: &Value) -> Option<String> {
    value
        .get("history_metadata")?
        .get("artifact_family")?
        .as_str()
        .map(ToString::to_string)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002,REQ-PRFU-004
/// @pseudocode lines 5-7
fn insert_binding_fields(object: &mut Map<String, Value>, binding: &PrFollowupBinding) {
    object.insert(
        "schema_version".to_string(),
        Value::from(binding.schema_version),
    );
    object.insert("run_id".to_string(), Value::from(binding.run_id.clone()));
    object.insert(
        "repository_owner".to_string(),
        Value::from(binding.repository_owner.clone()),
    );
    object.insert(
        "repository_name".to_string(),
        Value::from(binding.repository_name.clone()),
    );
    object.insert("pr_number".to_string(), Value::from(binding.pr_number));
    object.insert(
        "head_ref".to_string(),
        Value::from(binding.head_ref.clone()),
    );
    object.insert(
        "head_sha".to_string(),
        Value::from(binding.head_sha.clone()),
    );
    object.insert(
        "base_ref".to_string(),
        Value::from(binding.base_ref.clone()),
    );
    object.insert(
        "base_sha".to_string(),
        binding
            .base_sha
            .as_ref()
            .map_or(Value::Null, |base_sha| Value::from(base_sha.clone())),
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
fn require_u64(value: &Value, field: &str) -> Result<u64, EngineError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| artifact_error(format!("missing or invalid integer field {field}")))
}

/// @pseudocode lines 5-7
fn require_string(value: &Value, field: &str) -> Result<String, EngineError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| artifact_error(format!("missing or invalid string field {field}")))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
fn require_string_from_object(
    object: &Map<String, Value>,
    field: &str,
) -> Result<String, EngineError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| artifact_error(format!("missing or invalid string field {field}")))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
fn require_bool_from_object(object: &Map<String, Value>, field: &str) -> Result<bool, EngineError> {
    object
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| artifact_error(format!("missing or invalid bool field {field}")))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
fn artifact_error(message: impl Into<String>) -> EngineError {
    EngineError::StepExecutionError {
        step_id: "pr_followup_artifact_store".to_string(),

        message: message.into(),
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
    for expected in 1..=max_sequence {
        if !seen_sequences.contains(&expected) {
            return Err(artifact_error(format!(
                "non-monotonic {sequence_name}: missing sequence {expected} before {max_sequence} in {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn sanitize_path_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}
