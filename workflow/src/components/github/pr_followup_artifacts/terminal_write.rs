//! Durable terminal and recoverable artifact publication.
//!
//! Publication is serialized per binding and writes immutable history before
//! atomically replacing canonical state. Terminal publication additionally
//! recovers from valid history while refusing to supersede immutable identity.

mod receipt_validation;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use super::path_safety::{
    acquire_publication_lock, durable_create_new, durable_replace, publish_in_retained_directory,
    retain_publication_parent, validate_contained_directory, validate_contained_file,
    validate_publication_size,
};
use super::*;
use receipt_validation::*;
use serde_json::Value;

/// Typed inputs for a terminal publication whose canonical identity is
/// immutable once its idempotency key has been committed.
pub struct TerminalArtifactPublication<'a> {
    pub binding: &'a PrFollowupBinding,
    pub artifact_family: &'a str,
    pub producer_step_id: &'a str,
    pub step_order_index: u64,
    pub payload: &'a Value,
    pub failure_reason: &'a str,
    pub idempotency_key: &'a str,
    pub clock: &'a dyn ClockSleeper,
    pub allow_distinct_idempotency_keys: bool,
}

pub(crate) enum RecoverableCurrentArtifact {
    Missing,
    Valid(Value),
    Corrupt,
}

pub(crate) struct RecoverableHistoryCandidate {
    pub(crate) path: PathBuf,
    pub(crate) value: Option<Value>,
    pub(crate) validation_error: Option<String>,
}

pub(crate) struct ArtifactLaunchBinding<'a> {
    pub(crate) transition_id: &'a str,
    pub(crate) ordinal: u64,
}

pub(crate) struct CapturedImmutableReceipt {
    pub(crate) receipt: Value,
    pub(crate) replay_error: Option<String>,
}

pub(crate) struct ImmutableReceiptRequest<'a> {
    pub(crate) binding: &'a PrFollowupBinding,
    pub(crate) source_family: &'a str,
    pub(crate) receipt_family: &'a str,
    pub(crate) producer_step_id: &'a str,
    pub(crate) step_order_index: u64,
    pub(crate) clock: &'a dyn ClockSleeper,
    pub(crate) expected_launch: Option<ArtifactLaunchBinding<'a>>,
}

/// Durable-publication checkpoints used to inject storage faults and block a
/// publisher without replacing the production locking and fsync path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactPublicationStage {
    BeforeHistory,
    AfterHistory,
    BeforeCanonical,
}

/// Synchronous seam around durable artifact publication.
pub trait ArtifactPublicationHook: Send + Sync {
    fn checkpoint(
        &self,
        _stage: ArtifactPublicationStage,
        _history_path: &Path,
        _canonical_path: &Path,
    ) -> Result<(), EngineError> {
        Ok(())
    }
}

#[derive(Debug)]
pub(super) struct NoopArtifactPublicationHook;

impl ArtifactPublicationHook for NoopArtifactPublicationHook {}

fn validate_sidecar_extension(extension: &str) -> Result<(), EngineError> {
    let mut components = Path::new(extension).components();
    let is_safe = !extension.is_empty()
        && !extension.as_bytes().contains(&0)
        && matches!(
            (components.next(), components.next()),
            (Some(std::path::Component::Normal(component)), None)
                if component == std::ffi::OsStr::new(extension)
        );
    if is_safe {
        Ok(())
    } else {
        Err(artifact_error(
            "sidecar extension must be a nonempty safe filename component",
        ))
    }
}

impl PrFollowupArtifactStore {
    pub(crate) fn with_binding_publication_lock<R>(
        &self,
        binding: &PrFollowupBinding,
        action: impl FnOnce() -> Result<R, EngineError>,
    ) -> Result<R, EngineError> {
        let binding_root = self.current_binding_root(binding);
        let process_lock = process_publication_lock(&binding_root)?;
        let _process_guard = process_lock
            .lock()
            .map_err(|_| artifact_error("in-process artifact publication lock was poisoned"))?;
        let _file_lock = acquire_publication_lock(&self.root, &binding_root)?;
        action()
    }

    pub(super) fn publish_artifact(
        &self,
        history_path: &Path,
        canonical_path: &Path,
        bytes: &[u8],
    ) -> Result<(), EngineError> {
        validate_publication_size(history_path, bytes)?;
        validate_publication_size(canonical_path, bytes)?;
        let history_parent = retain_publication_parent(&self.root, history_path)?;
        let canonical_parent = retain_publication_parent(&self.root, canonical_path)?;
        self.publication_hook.checkpoint(
            ArtifactPublicationStage::BeforeHistory,
            history_path,
            canonical_path,
        )?;
        publish_in_retained_directory(&history_parent, history_path, bytes, true)?;
        self.publication_hook.checkpoint(
            ArtifactPublicationStage::AfterHistory,
            history_path,
            canonical_path,
        )?;
        history_parent.verify_identity(&self.root)?;
        canonical_parent.verify_identity(&self.root)?;
        self.publication_hook.checkpoint(
            ArtifactPublicationStage::BeforeCanonical,
            history_path,
            canonical_path,
        )?;
        history_parent.verify_identity(&self.root)?;
        canonical_parent.verify_identity(&self.root)?;
        publish_in_retained_directory(&canonical_parent, canonical_path, bytes, false)?;
        history_parent.verify_identity(&self.root)?;
        canonical_parent.verify_identity(&self.root)
    }

    pub(super) fn resolve_authenticated_replay(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
        producer_step_id: &str,
        replay_key: &ArtifactReplayKey<'_>,
    ) -> Result<Option<ArtifactWriteRecord>, EngineError> {
        let canonical_path = self.canonical_path(binding, artifact_family);
        if super::path_safety::validate_contained_file(&self.root, &canonical_path)? {
            let current = self.read_json_path(&canonical_path)?;
            let current_producer = current.get("producer_step_id").and_then(Value::as_str);
            let current_key = current.get(replay_key.field).and_then(Value::as_str);
            if current_producer != Some(producer_step_id) {
                if current_key == Some(replay_key.value) {
                    return Err(artifact_error(
                        "replay identity belongs to an unexpected producer",
                    ));
                }
            } else if current_key != Some(replay_key.value) {
                if !replay_key.allow_superseding_source {
                    return Err(artifact_error(format!(
                        "refusing to replace {artifact_family} validation from a different replay source"
                    )));
                }
            } else {
                self.validate_artifact_value(binding, artifact_family, &current)?;
                validate_canonical_embedded_path(&current, &canonical_path)?;
                self.validate_artifact_invariants(artifact_family, &current)?;
                self.validate_canonical_matches_immutable_history(
                    binding,
                    artifact_family,
                    &current,
                )?;
                return artifact_write_record(&current, canonical_path).map(Some);
            }
        }

        let mut matching = None;
        let mut ambiguous = false;
        self.visit_terminal_history_candidates(binding, artifact_family, |candidate| {
            if candidate.validation_error.is_none() {
                if let Some(value) = candidate.value {
                    let candidate_key = value.get(replay_key.field).and_then(Value::as_str);
                    let candidate_producer = value.get("producer_step_id").and_then(Value::as_str);
                    if candidate_key == Some(replay_key.value) {
                        if candidate_producer != Some(producer_step_id) {
                            return Err(artifact_error(
                                "replay identity belongs to an unexpected producer",
                            ));
                        }
                        if matching.is_some() {
                            ambiguous = true;
                        } else {
                            matching = Some(value);
                        }
                    }
                }
            }
            Ok(())
        })?;
        if ambiguous {
            return Err(artifact_error(
                "replay canonical recovery is ambiguous across immutable history",
            ));
        }
        let Some(existing) = matching else {
            return Ok(None);
        };
        self.restore_canonical_from_history_locked(binding, artifact_family, &existing)?;
        artifact_write_record(&existing, canonical_path).map(Some)
    }

    /// Reads independently recoverable snapshots for one exact family and
    /// binding. Candidate data corruption is isolated to that candidate; real
    /// filesystem failures are propagated so recovery never turns an I/O
    /// outage into an apparently empty history.
    pub fn read_recoverable_history_family(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
    ) -> Result<Vec<Value>, EngineError> {
        let candidates = self.read_recoverable_history_candidates(binding, artifact_family)?;
        let mut values = candidates
            .into_iter()
            .filter(|candidate| candidate.validation_error.is_none())
            .filter_map(|candidate| candidate.value)
            .collect::<Vec<_>>();
        values.sort_by_key(|value| {
            value
                .get("artifact_sequence")
                .and_then(Value::as_u64)
                .unwrap_or_default()
        });
        Ok(values)
    }

    pub(crate) fn read_pr_identity_history_candidates(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
    ) -> Result<Vec<RecoverableHistoryCandidate>, EngineError> {
        let mut candidates = self.read_recoverable_history_candidates(binding, artifact_family)?;
        for candidate in &mut candidates {
            let Some(value) = candidate.value.as_ref() else {
                continue;
            };
            candidate.validation_error = binding_from_value(value)
                .and_then(|actual| {
                    if !actual.pr_identity_matches(binding) {
                        return Err(artifact_error(
                            "history artifact has a different PR identity",
                        ));
                    }
                    self.validate_artifact_metadata(artifact_family, value)
                })
                .and_then(|()| {
                    self.validate_history_snapshot(artifact_family, value, &candidate.path)
                })
                .and_then(|()| validate_history_filename(artifact_family, value, &candidate.path))
                .and_then(|()| validate_history_embedded_path(value, &candidate.path))
                .err()
                .map(|error| error.to_string());
        }
        Ok(candidates)
    }

    pub(crate) fn read_recoverable_history_candidates(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
    ) -> Result<Vec<RecoverableHistoryCandidate>, EngineError> {
        self.read_recoverable_history_candidates_with_budget(
            binding,
            artifact_family,
            &mut super::path_safety::ReadBudget::default(),
        )
    }

    pub(crate) fn visit_recoverable_history_candidates(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
        visitor: impl FnMut(RecoverableHistoryCandidate) -> Result<(), EngineError>,
    ) -> Result<(), EngineError> {
        self.visit_recoverable_history_candidates_with_budget(
            binding,
            artifact_family,
            &mut super::path_safety::ReadBudget::without_aggregate_limit(),
            visitor,
        )
    }

    pub(super) fn visit_terminal_history_candidates(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
        visitor: impl FnMut(RecoverableHistoryCandidate) -> Result<(), EngineError>,
    ) -> Result<(), EngineError> {
        self.visit_recoverable_history_candidates_with_budget(
            binding,
            artifact_family,
            &mut super::path_safety::ReadBudget::without_aggregate_limit(),
            visitor,
        )
    }

    fn read_recoverable_history_candidates_with_budget(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
        budget: &mut super::path_safety::ReadBudget,
    ) -> Result<Vec<RecoverableHistoryCandidate>, EngineError> {
        let mut candidates = Vec::new();
        self.visit_recoverable_history_candidates_with_budget(
            binding,
            artifact_family,
            budget,
            |candidate| {
                candidates.push(candidate);
                Ok(())
            },
        )?;
        candidates.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(candidates)
    }

    fn visit_recoverable_history_candidates_with_budget(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
        budget: &mut super::path_safety::ReadBudget,
        mut visitor: impl FnMut(RecoverableHistoryCandidate) -> Result<(), EngineError>,
    ) -> Result<(), EngineError> {
        let family_root = self.history_root_for_family(binding, artifact_family);
        if !validate_contained_directory(&self.root, &family_root)? {
            return Ok(());
        }
        super::path_safety::visit_contained_json_files_with_budget(
            &self.root,
            &family_root,
            budget,
            |file| {
                let parsed = serde_json::from_str::<Value>(&file.content);
                let validation_error = match &parsed {
                    Ok(value) => self
                        .validate_artifact_value(binding, artifact_family, value)
                        .and_then(|()| {
                            self.validate_history_snapshot(artifact_family, value, &file.path)
                        })
                        .and_then(|()| {
                            validate_history_filename(artifact_family, value, &file.path)
                        })
                        .and_then(|()| validate_history_embedded_path(value, &file.path))
                        .err()
                        .map(|error| error.to_string()),
                    Err(error) => Some(format!("parse {}: {error}", file.path.display())),
                };
                visitor(RecoverableHistoryCandidate {
                    path: file.path,
                    value: parsed.ok(),
                    validation_error,
                })
            },
        )
    }

    pub(crate) fn capture_immutable_receipt(
        &self,
        request: ImmutableReceiptRequest<'_>,
    ) -> Result<CapturedImmutableReceipt, EngineError> {
        let binding = request.binding;
        self.with_binding_publication_lock(binding, || {
            let mut budget = super::path_safety::ReadBudget::default();
            let source_path = self.canonical_path(binding, request.source_family);
            let raw_text = super::path_safety::read_contained_file_with_budget(
                &self.root,
                &source_path,
                &mut budget,
            )?;
            let raw = serde_json::from_str::<Value>(&raw_text).map_err(|error| {
                artifact_error(format!("parse {}: {error}", source_path.display()))
            })?;
            let histories = self
                .read_recoverable_history_candidates_with_budget(
                    binding,
                    request.receipt_family,
                    &mut budget,
                )?
                .into_iter()
                .filter(|candidate| candidate.validation_error.is_none())
                .filter_map(|candidate| candidate.value)
                .collect::<Vec<_>>();
            if validated_result_source_id(&raw).is_some() {
                let receipt = self.restore_validated_result_receipt(
                    binding,
                    request.receipt_family,
                    &raw,
                    &histories,
                )?;
                return Ok(CapturedImmutableReceipt {
                    receipt,
                    replay_error: validated_result_launch_error(
                        &raw,
                        request.expected_launch.as_ref(),
                    ),
                });
            }
            let source_identity = immutable_source_identity(raw_text.as_bytes());
            if let Some(receipt) = unique_receipt_for_source(&histories, &source_identity)? {
                self.restore_canonical_from_history_locked(
                    binding,
                    request.receipt_family,
                    receipt,
                )?;
                return Ok(CapturedImmutableReceipt {
                    receipt: receipt.clone(),
                    replay_error: None,
                });
            }
            let payload = serde_json::json!({
                "agent_result_payload": raw,
                "agent_result_source_identity": source_identity,
            });
            self.write_json_artifact_locked(JsonArtifactWriteRequest::new(
                ArtifactWriteContext::new(
                    binding,
                    request.receipt_family,
                    request.producer_step_id,
                    request.step_order_index,
                    request.clock,
                ),
                &payload,
                None,
            ))?;
            Ok(CapturedImmutableReceipt {
                receipt: self.read_current_json(binding, request.receipt_family)?,
                replay_error: None,
            })
        })
    }

    fn restore_validated_result_receipt(
        &self,
        binding: &PrFollowupBinding,
        receipt_family: &str,
        result: &Value,
        histories: &[Value],
    ) -> Result<Value, EngineError> {
        let source_id = validated_result_source_id(result)
            .ok_or_else(|| artifact_error("validated result is missing source identity"))?;
        let matching = histories
            .iter()
            .filter(|receipt| receipt_validation_source_id(receipt).as_deref() == Some(source_id))
            .collect::<Vec<_>>();
        match matching.as_slice() {
            [receipt] => {
                self.restore_canonical_from_history_locked(binding, receipt_family, receipt)?;
                Ok((*receipt).clone())
            }
            [] => Err(artifact_error(
                "validated result has no immutable agent-result receipt",
            )),
            _ => Err(artifact_error("validated result receipt is ambiguous")),
        }
    }

    pub(crate) fn read_recoverable_current_json(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
    ) -> Result<RecoverableCurrentArtifact, EngineError> {
        let path = self.canonical_path(binding, artifact_family);
        if !validate_contained_file(&self.root, &path)? {
            return Ok(RecoverableCurrentArtifact::Missing);
        }
        let mut budget = super::path_safety::ReadBudget::default();
        let raw =
            super::path_safety::read_contained_file_with_budget(&self.root, &path, &mut budget)?;
        let value = match serde_json::from_str::<Value>(&raw) {
            Ok(value) => value,
            Err(_) => return Ok(RecoverableCurrentArtifact::Corrupt),
        };
        let validation = self
            .validate_artifact_value(binding, artifact_family, &value)
            .and_then(|()| validate_canonical_embedded_path(&value, &path))
            .and_then(|()| self.validate_artifact_invariants(artifact_family, &value))
            .and_then(|()| {
                self.validate_canonical_matches_immutable_history_with_budget(
                    binding,
                    artifact_family,
                    &value,
                    &mut budget,
                )
            });
        if validation.is_err() {
            return Ok(RecoverableCurrentArtifact::Corrupt);
        }
        Ok(RecoverableCurrentArtifact::Valid(value))
    }

    pub(crate) fn rename_contained_file(
        &self,
        source: &Path,
        destination: &Path,
    ) -> Result<(), EngineError> {
        super::path_safety::rename_contained_file(&self.root, source, destination)
    }

    pub fn publish_immutable_sidecar(
        &self,
        binding: &PrFollowupBinding,
        record: &ArtifactWriteRecord,
        extension: &str,
        bytes: &[u8],
    ) -> Result<PathBuf, EngineError> {
        validate_sidecar_extension(extension)?;
        self.with_binding_publication_lock(binding, || {
            let source_parent = record.history_path.parent().ok_or_else(|| {
                artifact_error("sidecar source history snapshot has no parent directory")
            })?;
            if source_parent.parent() != Some(self.history_binding_root(binding).as_path()) {
                return Err(artifact_error(
                    "sidecar source is outside an exact binding history family root",
                ));
            }
            let sidecar = record.history_path.with_extension(extension);
            if sidecar.parent() != Some(source_parent) {
                return Err(artifact_error(
                    "derived sidecar path changed the source history parent",
                ));
            }
            if !validate_contained_file(&self.root, &record.history_path)? {
                return Err(artifact_error("sidecar source history snapshot is missing"));
            }
            validate_publication_size(&sidecar, bytes)?;
            durable_create_new(&self.root, &sidecar, bytes)?;
            Ok(sidecar)
        })
    }

    pub(crate) fn restore_canonical_from_history_locked(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
        value: &Value,
    ) -> Result<(), EngineError> {
        self.validate_artifact_value(binding, artifact_family, value)?;
        let history_path = value
            .pointer("/history_metadata/history_path")
            .and_then(Value::as_str)
            .ok_or_else(|| artifact_error("missing history path for canonical restoration"))?;
        validate_history_embedded_path(value, Path::new(history_path))?;
        let canonical_path = self.canonical_path(binding, artifact_family);
        let embedded_canonical = value
            .pointer("/history_metadata/canonical_path")
            .and_then(Value::as_str)
            .ok_or_else(|| artifact_error("missing canonical path for restoration"))?;
        if Path::new(embedded_canonical) != canonical_path {
            return Err(artifact_error("canonical restoration path mismatch"));
        }
        let bytes = serde_json::to_vec_pretty(value)
            .map_err(|err| artifact_error(format!("serialize restored canonical: {err}")))?;
        durable_replace(&self.root, &canonical_path, &bytes)
    }

    pub fn publish_terminal_once(
        &self,
        publication: TerminalArtifactPublication<'_>,
    ) -> Result<ArtifactWriteRecord, EngineError> {
        self.with_binding_publication_lock(publication.binding, || {
            self.publish_terminal_once_locked(&publication)
        })
    }

    pub(crate) fn publish_terminal_once_locked(
        &self,
        publication: &TerminalArtifactPublication<'_>,
    ) -> Result<ArtifactWriteRecord, EngineError> {
        let canonical_path = self.canonical_path(publication.binding, publication.artifact_family);
        if let Some(record) = self.resolve_existing_terminal(publication, &canonical_path)? {
            return Ok(record);
        }
        self.create_terminal_publication(publication, canonical_path)
    }

    pub(crate) fn resolve_committed_terminal_locked(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
    ) -> Result<Option<ArtifactWriteRecord>, EngineError> {
        let canonical_path = self.canonical_path(binding, artifact_family);
        if validate_contained_file(&self.root, &canonical_path)? {
            if let Ok(value) = self.read_json_path(&canonical_path) {
                let valid = self
                    .validate_artifact_value(binding, artifact_family, &value)
                    .and_then(|()| validate_canonical_embedded_path(&value, &canonical_path))
                    .and_then(|()| self.validate_artifact_invariants(artifact_family, &value));
                if valid.is_ok() {
                    self.validate_canonical_matches_immutable_history(
                        binding,
                        artifact_family,
                        &value,
                    )?;
                    let idempotency_key = value
                        .get("idempotency_key")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            artifact_error("terminal artifact is missing idempotency key")
                        })?;
                    return existing_publication_record(&value, &canonical_path, idempotency_key)
                        .map(Some);
                }
            }
        }

        let mut existing = None;
        let mut ambiguous = false;
        self.visit_terminal_history_candidates(binding, artifact_family, |candidate| {
            if candidate.validation_error.is_none() {
                if let Some(value) = candidate.value {
                    if existing.is_some() {
                        ambiguous = true;
                    } else {
                        existing = Some(value);
                    }
                }
            }
            Ok(())
        })?;
        if ambiguous {
            return Err(artifact_error(
                "terminal canonical recovery is ambiguous across immutable history",
            ));
        }
        let Some(existing) = existing else {
            return Ok(None);
        };
        let idempotency_key = existing
            .get("idempotency_key")
            .and_then(Value::as_str)
            .ok_or_else(|| artifact_error("terminal history is missing idempotency key"))?;
        self.restore_canonical_from_history_locked(binding, artifact_family, &existing)?;
        existing_publication_record(&existing, &canonical_path, idempotency_key).map(Some)
    }

    fn resolve_existing_terminal(
        &self,
        publication: &TerminalArtifactPublication<'_>,
        canonical_path: &Path,
    ) -> Result<Option<ArtifactWriteRecord>, EngineError> {
        let current =
            self.read_recoverable_current_json(publication.binding, publication.artifact_family)?;
        if let RecoverableCurrentArtifact::Valid(existing) = &current {
            if existing.get("idempotency_key").and_then(Value::as_str)
                == Some(publication.idempotency_key)
            {
                return existing_publication_record(
                    existing,
                    canonical_path,
                    publication.idempotency_key,
                )
                .map(Some);
            }
            if !publication.allow_distinct_idempotency_keys {
                return Err(artifact_error(
                    "refusing to overwrite terminal artifact with a different idempotency key",
                ));
            }
        }

        let mut has_valid_history = false;
        let mut matching = None;
        let mut ambiguous = false;
        self.visit_terminal_history_candidates(
            publication.binding,
            publication.artifact_family,
            |candidate| {
                if candidate.validation_error.is_none() {
                    if let Some(value) = candidate.value {
                        has_valid_history = true;
                        if value.get("idempotency_key").and_then(Value::as_str)
                            == Some(publication.idempotency_key)
                        {
                            if matching.is_some() {
                                ambiguous = true;
                            } else {
                                matching = Some(value);
                            }
                        }
                    }
                }
                Ok(())
            },
        )?;
        if ambiguous {
            return Err(artifact_error(
                "terminal canonical recovery is ambiguous across immutable history",
            ));
        }
        if let Some(existing) = matching {
            self.restore_canonical_from_history_locked(
                publication.binding,
                publication.artifact_family,
                &existing,
            )?;
            return existing_publication_record(
                &existing,
                canonical_path,
                publication.idempotency_key,
            )
            .map(Some);
        }
        if matches!(current, RecoverableCurrentArtifact::Corrupt) {
            return Err(artifact_error(
                "corrupt terminal canonical has no unambiguous matching immutable history",
            ));
        }
        if !has_valid_history || publication.allow_distinct_idempotency_keys {
            return Ok(None);
        }
        Err(artifact_error(
            "refusing to supersede an immutable terminal publication",
        ))
    }

    fn create_terminal_publication(
        &self,
        publication: &TerminalArtifactPublication<'_>,
        canonical_path: PathBuf,
    ) -> Result<ArtifactWriteRecord, EngineError> {
        let state = self.recover_resilient_sequence_state(publication.binding)?;
        let sequence = ArtifactSequenceMetadata {
            artifact_sequence: checked_next_sequence(
                state.max_artifact_sequence,
                "artifact_sequence",
            )?,
            write_sequence: checked_next_sequence(
                state
                    .max_write_sequence_by_family
                    .get(publication.artifact_family)
                    .copied()
                    .unwrap_or_default(),
                "write_sequence",
            )?,
            producer_step_id: publication.producer_step_id.to_string(),
        };
        let history_path =
            self.history_path(publication.binding, publication.artifact_family, &sequence);
        let mut value = publication.payload.clone();
        self.inject_store_fields(
            StoreFieldContext {
                write: ArtifactWriteContext::new(
                    publication.binding,
                    publication.artifact_family,
                    publication.producer_step_id,
                    publication.step_order_index,
                    publication.clock,
                ),
                sequence: &sequence,
                canonical_path: &canonical_path,
                history_path: &history_path,
                failure_sequence: None,
                failure: None,
            },
            &mut value,
        )?;
        value["failure_reason"] = Value::from(publication.failure_reason);
        validate_family_invariants(publication.artifact_family, &value)?;
        let bytes = serde_json::to_vec_pretty(&value)
            .map_err(|err| artifact_error(format!("serialize terminal artifact: {err}")))?;
        self.publish_artifact(&history_path, &canonical_path, &bytes)?;
        Ok(ArtifactWriteRecord {
            sequence,
            canonical_path,
            history_path,
            failure_sequence: None,
        })
    }

    pub(super) fn validate_canonical_matches_immutable_history(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
        canonical: &Value,
    ) -> Result<(), EngineError> {
        self.validate_canonical_matches_immutable_history_with_budget(
            binding,
            artifact_family,
            canonical,
            &mut super::path_safety::ReadBudget::default(),
        )
    }

    pub(super) fn validate_canonical_matches_immutable_history_with_budget(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
        canonical: &Value,
        budget: &mut super::path_safety::ReadBudget,
    ) -> Result<(), EngineError> {
        let history_path = canonical
            .pointer("/history_metadata/history_path")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                artifact_error("canonical artifact is missing immutable history path")
            })?;
        let history_path = Path::new(history_path);
        let expected_root = self.history_root_for_family(binding, artifact_family);
        let actual_parent = history_path.parent().ok_or_else(|| {
            artifact_error(format!(
                "history path has no parent: {}",
                history_path.display()
            ))
        })?;
        if !paths_match(expected_root.to_string_lossy().as_ref(), actual_parent) {
            return Err(artifact_error(format!(
                "canonical immutable history is outside the exact family root: {}",
                history_path.display()
            )));
        }
        let history = self.read_json_path_with_budget(history_path, budget)?;
        self.validate_artifact_value(binding, artifact_family, &history)?;
        self.validate_history_snapshot(artifact_family, &history, history_path)?;
        validate_history_filename(artifact_family, &history, history_path)?;
        validate_history_embedded_path(&history, history_path)?;
        if &history != canonical {
            return Err(artifact_error(format!(
                "canonical artifact differs from its immutable history snapshot at {}",
                history_path.display()
            )));
        }
        Ok(())
    }

    /// Visits validated immutable failure snapshots for the binding while
    /// retaining at most one candidate payload in memory at a time.
    pub(crate) fn visit_failure_history_candidates(
        &self,
        binding: &PrFollowupBinding,
        excluded_family: &str,
        mut visitor: impl FnMut(Value) -> Result<(), EngineError>,
    ) -> Result<(), EngineError> {
        let history_root = self.history_binding_root(binding);
        if !validate_contained_directory(&self.root, &history_root)? {
            return Ok(());
        }
        let mut budget = super::path_safety::ReadBudget::without_aggregate_limit();
        super::path_safety::visit_contained_json_files_with_budget(
            &self.root,
            &history_root,
            &mut budget,
            |file| {
                let path = file.path;
                let family_from_path = path
                    .parent()
                    .and_then(Path::file_name)
                    .and_then(|name| name.to_str())
                    .unwrap_or_default();
                if family_from_path == excluded_family || is_routing_artifact(family_from_path) {
                    return Ok(());
                }
                let Ok(value) = serde_json::from_str::<Value>(&file.content) else {
                    return Ok(());
                };
                let Ok(actual_binding) = binding_from_value(&value) else {
                    return Ok(());
                };
                if !binding.pr_identity_matches(&actual_binding) {
                    return Err(artifact_error(format!(
                        "failure history contains a different PR identity in {}",
                        path.display()
                    )));
                }
                if actual_binding.is_stale_prior_head_of(binding) {
                    return Ok(());
                }
                let Some(family) = artifact_family_from_value(&value) else {
                    return Ok(());
                };
                if family != family_from_path {
                    return Ok(());
                }
                let valid = self
                    .validate_artifact_value(binding, &family, &value)
                    .and_then(|()| self.validate_history_snapshot(&family, &value, &path))
                    .and_then(|()| validate_history_filename(&family, &value, &path))
                    .and_then(|()| validate_history_embedded_path(&value, &path));
                if valid.is_err()
                    || value
                        .get("failure_sequence")
                        .and_then(Value::as_u64)
                        .is_none_or(|sequence| sequence == 0)
                {
                    return Ok(());
                }
                visitor(value)
            },
        )
    }
}

fn existing_publication_record(
    value: &Value,
    canonical_path: &Path,
    idempotency_key: &str,
) -> Result<ArtifactWriteRecord, EngineError> {
    if value.get("idempotency_key").and_then(Value::as_str) != Some(idempotency_key) {
        return Err(artifact_error(
            "refusing to overwrite terminal artifact with a different idempotency key",
        ));
    }
    let sequence = ArtifactSequenceMetadata {
        artifact_sequence: require_u64(value, "artifact_sequence")?,
        write_sequence: require_u64(value, "write_sequence")?,
        producer_step_id: require_string(value, "producer_step_id")?,
    };
    let history_path = value
        .pointer("/history_metadata/history_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| artifact_error("terminal artifact is missing history path"))?;
    Ok(ArtifactWriteRecord {
        sequence,
        canonical_path: canonical_path.to_path_buf(),
        history_path,
        failure_sequence: None,
    })
}

type ProcessPublicationLocks = Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>;

fn process_publication_lock(binding_root: &Path) -> Result<Arc<Mutex<()>>, EngineError> {
    static LOCKS: OnceLock<ProcessPublicationLocks> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| artifact_error("artifact publication lock registry was poisoned"))?;
    if let Some(lock) = locks.get(binding_root).and_then(Weak::upgrade) {
        return Ok(lock);
    }
    locks.retain(|_, lock| lock.strong_count() > 0);
    let lock = Arc::new(Mutex::new(()));
    locks.insert(binding_root.to_path_buf(), Arc::downgrade(&lock));
    Ok(lock)
}

const ROUTING_ARTIFACT_FAMILIES: &[&str] = &[
    "pr-remediation-plan",
    "pr-remediation-retry-state",
    "pending-feedback-marker-actions",
    "post-pr-iteration-guard",
];

fn is_routing_artifact(family: &str) -> bool {
    ROUTING_ARTIFACT_FAMILIES.contains(&family)
}
