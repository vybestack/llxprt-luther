//! PR follow-through artifact store and writer implementation.
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
//! @requirement:REQ-PRFU-002,REQ-PRFU-004,REQ-PRFU-020
//! @pseudocode lines 5-7
mod history;
mod identity_discovery;
mod path_safety;
pub use path_safety::{
    sanitize_path_segment, PrFollowupFilesystem, SystemPrFollowupFilesystem,
    MAX_ARTIFACT_FILE_BYTES, MAX_ARTIFACT_READ_BYTES,
};
mod sequence_recovery;
mod terminal_validation;
mod terminal_write;
pub(crate) use history::ValidatedHistoryLedger;
use history::*;

use self::identity_discovery::{discover_current_pr_artifacts, is_current_pr_identity};
use self::terminal_write::NoopArtifactPublicationHook;
pub(crate) use self::terminal_write::{
    ArtifactLaunchBinding, ImmutableReceiptRequest, RecoverableCurrentArtifact,
    RecoverableHistoryCandidate,
};
pub use self::terminal_write::{
    ArtifactPublicationHook, ArtifactPublicationStage, TerminalArtifactPublication,
};
use serde::Serialize;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::engine::executors::pr_followup_types::{
    ArtifactSequenceMetadata, CiFailures, PostPrFailureTerminal, PrCheckStatus, PrFollowupBinding,
};
use crate::engine::runner::EngineError;

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

/// Binding and provenance shared by every artifact write request.
pub struct ArtifactWriteContext<'a> {
    binding: &'a PrFollowupBinding,
    artifact_family: &'a str,
    producer_step_id: &'a str,
    step_order_index: u64,
    clock: &'a dyn ClockSleeper,
}

impl<'a> ArtifactWriteContext<'a> {
    #[must_use]
    pub fn new(
        binding: &'a PrFollowupBinding,
        artifact_family: &'a str,
        producer_step_id: &'a str,
        step_order_index: u64,
        clock: &'a dyn ClockSleeper,
    ) -> Self {
        Self {
            binding,
            artifact_family,
            producer_step_id,
            step_order_index,
            clock,
        }
    }
}

struct ArtifactFailure<'a> {
    semantic_state: &'a str,
    reason: &'a str,
    details: Value,
}

/// Typed request for writing a JSON artifact and its store-managed envelope.
pub struct JsonArtifactWriteRequest<'a, T: ?Sized> {
    context: ArtifactWriteContext<'a>,
    payload: &'a T,
    failure: Option<ArtifactFailure<'a>>,
}

impl<'a, T: ?Sized> JsonArtifactWriteRequest<'a, T> {
    #[must_use]
    pub fn new(
        context: ArtifactWriteContext<'a>,
        payload: &'a T,
        failure: Option<(&'a str, &'a str, Value)>,
    ) -> Self {
        Self {
            context,
            payload,
            failure: failure.map(|(semantic_state, reason, details)| ArtifactFailure {
                semantic_state,
                reason,
                details,
            }),
        }
    }
}

/// Identity embedded by a producer to make conditional publication replay-safe.
pub struct ArtifactReplayKey<'a> {
    field: &'a str,
    value: &'a str,
    allow_superseding_source: bool,
}

impl<'a> ArtifactReplayKey<'a> {
    #[must_use]
    pub fn new(field: &'a str, value: &'a str) -> Self {
        Self {
            field,
            value,
            allow_superseding_source: false,
        }
    }

    #[must_use]
    pub(crate) fn superseding(field: &'a str, value: &'a str) -> Self {
        Self {
            field,
            value,
            allow_superseding_source: true,
        }
    }
}

/// Typed request for preserving raw text in an artifact envelope.
pub struct RawTextArtifactWriteRequest<'a> {
    context: ArtifactWriteContext<'a>,
    artifact_name: &'a str,
    raw_text: &'a str,
}

impl<'a> RawTextArtifactWriteRequest<'a> {
    #[must_use]
    pub fn new(
        context: ArtifactWriteContext<'a>,
        artifact_name: &'a str,
        raw_text: &'a str,
    ) -> Self {
        Self {
            context,
            artifact_name,
            raw_text,
        }
    }
}

struct StoreFieldContext<'a> {
    write: ArtifactWriteContext<'a>,
    sequence: &'a ArtifactSequenceMetadata,
    canonical_path: &'a Path,
    history_path: &'a Path,
    failure_sequence: Option<u64>,
    failure: Option<ArtifactFailure<'a>>,
}

/// PR follow-through artifact store implementation.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002,REQ-PRFU-004
/// @pseudocode lines 5-7
#[derive(Clone)]
pub struct PrFollowupArtifactStore {
    root: PathBuf,
    publication_hook: Arc<dyn ArtifactPublicationHook>,
}

impl std::fmt::Debug for PrFollowupArtifactStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PrFollowupArtifactStore")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002,REQ-PRFU-004
/// @pseudocode lines 5-7
impl PrFollowupArtifactStore {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self {
            root: path_safety::canonicalize_root_alias(root),
            publication_hook: Arc::new(NoopArtifactPublicationHook),
        }
    }

    #[must_use]
    pub fn with_publication_hook(
        root: PathBuf,
        publication_hook: Arc<dyn ArtifactPublicationHook>,
    ) -> Self {
        Self {
            root: path_safety::canonicalize_root_alias(root),
            publication_hook,
        }
    }

    pub fn with_filesystem(
        root: &Path,
        filesystem: &dyn PrFollowupFilesystem,
    ) -> Result<Self, EngineError> {
        Ok(Self::new(filesystem.canonicalize_root(root)?))
    }

    pub fn with_filesystem_and_publication_hook(
        root: &Path,
        filesystem: &dyn PrFollowupFilesystem,
        publication_hook: Arc<dyn ArtifactPublicationHook>,
    ) -> Result<Self, EngineError> {
        Ok(Self::with_publication_hook(
            filesystem.canonicalize_root(root)?,
            publication_hook,
        ))
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn find_current_pr_artifact_for_run(
        &self,
        run_id: &str,
        requested: &PrFollowupBinding,
    ) -> Result<Option<Value>, EngineError> {
        if requested.run_id != run_id {
            return Err(artifact_error("requested PR binding run_id mismatch"));
        }
        if requested.pr_number != 0 {
            return self.read_requested_pr_artifact(run_id, requested);
        }

        let current_root = self
            .root
            .join("pr-followup")
            .join("current")
            .join(sanitize_path_segment(run_id));
        let mut budget = path_safety::ReadBudget::default();
        let discovered =
            discover_current_pr_artifacts(&self.root, &current_root, run_id, &mut budget)?;
        let mut validated = Vec::new();
        for (path, value) in discovered {
            self.validate_discovered_pr_artifact_with_budget(&path, &value, &mut budget)?;
            validated.push((path, value));
        }
        match validated.len() {
            0 => Ok(None),
            1 => Ok(Some(validated.remove(0).1)),
            _ => {
                let paths = validated
                    .iter()
                    .map(|(path, _)| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                Err(artifact_error(format!(
                    "multiple PR identity artifacts found for run {run_id}; provide repository_owner, repository_name, and pr_number parameters; conflicting artifacts: {paths}"
                )))
            }
        }
    }

    fn read_requested_pr_artifact(
        &self,
        run_id: &str,
        requested: &PrFollowupBinding,
    ) -> Result<Option<Value>, EngineError> {
        let path = self.canonical_path(requested, "pr");
        if !path_safety::validate_contained_file(&self.root, &path)? {
            return Ok(None);
        }
        let mut budget = path_safety::ReadBudget::default();
        let value = self.read_json_path_with_budget(&path, &mut budget)?;
        let actual =
            self.validate_discovered_pr_artifact_with_budget(&path, &value, &mut budget)?;
        if value.get("run_id").and_then(Value::as_str) != Some(run_id)
            || !requested.pr_identity_matches(&actual)
        {
            return Err(artifact_error(
                "requested PR binding identity does not match the direct PR artifact",
            ));
        }
        Ok(is_current_pr_identity(&value).then_some(value))
    }

    pub(crate) fn read_untrusted_current_json(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
    ) -> Result<Option<Value>, EngineError> {
        let path = self.canonical_path(binding, artifact_family);
        if !path_safety::validate_contained_file(&self.root, &path)? {
            return Ok(None);
        }
        let raw = path_safety::read_contained_file_with_budget(
            &self.root,
            &path,
            &mut path_safety::ReadBudget::default(),
        )?;
        serde_json::from_str(&raw)
            .map(Some)
            .map_err(|error| artifact_error(format!("parse {}: {error}", path.display())))
    }

    pub(crate) fn remediation_launch_result_path(
        &self,
        binding: &PrFollowupBinding,
        launch_ordinal: u64,
        owner_token: &str,
    ) -> PathBuf {
        self.canonical_path(binding, "pr-remediation-result")
            .with_file_name(format!(
                "pr-remediation-result-{launch_ordinal}-{}.json",
                sanitize_path_segment(owner_token)
            ))
    }

    pub(crate) fn write_remediation_launch_result(
        &self,
        binding: &PrFollowupBinding,
        launch_ordinal: u64,
        owner_token: &str,
        payload: &Value,
    ) -> Result<(), EngineError> {
        let path = self.remediation_launch_result_path(binding, launch_ordinal, owner_token);
        let bytes = serde_json::to_vec_pretty(payload).map_err(|error| {
            artifact_error(format!(
                "serialize remediation launch result {}: {error}",
                path.display()
            ))
        })?;
        path_safety::durable_replace(&self.root, &path, &bytes)
    }

    pub(crate) fn read_untrusted_remediation_launch_result(
        &self,
        binding: &PrFollowupBinding,
        launch_ordinal: u64,
        owner_token: &str,
    ) -> Result<Option<Value>, EngineError> {
        let path = self.remediation_launch_result_path(binding, launch_ordinal, owner_token);
        if !path_safety::validate_contained_file(&self.root, &path)? {
            return Ok(None);
        }
        let raw = path_safety::read_contained_file_with_budget(
            &self.root,
            &path,
            &mut path_safety::ReadBudget::default(),
        )?;
        serde_json::from_str(&raw)
            .map(Some)
            .map_err(|error| artifact_error(format!("parse {}: {error}", path.display())))
    }

    pub(crate) fn promote_remediation_launch_result_locked(
        &self,
        binding: &PrFollowupBinding,
        launch_ordinal: u64,
        owner_token: &str,
    ) -> Result<bool, EngineError> {
        let source = self.remediation_launch_result_path(binding, launch_ordinal, owner_token);
        if !path_safety::validate_contained_file(&self.root, &source)? {
            return Ok(false);
        }
        let raw = path_safety::read_contained_file_with_budget(
            &self.root,
            &source,
            &mut path_safety::ReadBudget::default(),
        )?;
        serde_json::from_str::<Value>(&raw)
            .map_err(|error| artifact_error(format!("parse {}: {error}", source.display())))?;
        let canonical = self.canonical_path(binding, "pr-remediation-result");
        path_safety::durable_replace(&self.root, &canonical, raw.as_bytes())?;
        Ok(true)
    }

    fn validate_discovered_pr_artifact_with_budget(
        &self,
        path: &Path,
        value: &Value,
        budget: &mut path_safety::ReadBudget,
    ) -> Result<PrFollowupBinding, EngineError> {
        let actual = binding_from_value(value)?;
        self.validate_artifact_value(&actual, "pr", value)?;
        validate_canonical_embedded_path(value, path)?;
        self.validate_artifact_invariants("pr", value)?;
        self.validate_canonical_matches_immutable_history_with_budget(
            &actual, "pr", value, budget,
        )?;
        Ok(actual)
    }

    pub fn write_json_artifact<T: Serialize + ?Sized>(
        &self,
        request: JsonArtifactWriteRequest<'_, T>,
    ) -> Result<ArtifactWriteRecord, EngineError> {
        self.with_binding_publication_lock(request.context.binding, || {
            self.write_json_artifact_locked(request)
        })
    }

    pub fn write_json_artifact_once<T: Serialize + ?Sized>(
        &self,
        request: JsonArtifactWriteRequest<'_, T>,
        replay_key: ArtifactReplayKey<'_>,
    ) -> Result<ArtifactWriteRecord, EngineError> {
        self.with_binding_publication_lock(request.context.binding, || {
            self.write_json_artifact_once_locked(request, replay_key)
        })
    }

    pub(crate) fn write_json_artifact_once_locked<T: Serialize + ?Sized>(
        &self,
        request: JsonArtifactWriteRequest<'_, T>,
        replay_key: ArtifactReplayKey<'_>,
    ) -> Result<ArtifactWriteRecord, EngineError> {
        if let Some(record) = self.resolve_authenticated_replay(
            request.context.binding,
            request.context.artifact_family,
            request.context.producer_step_id,
            &replay_key,
        )? {
            return Ok(record);
        }
        self.write_json_artifact_locked(request)
    }

    pub(crate) fn write_json_artifact_locked<T: Serialize + ?Sized>(
        &self,
        request: JsonArtifactWriteRequest<'_, T>,
    ) -> Result<ArtifactWriteRecord, EngineError> {
        let JsonArtifactWriteRequest {
            context,
            payload,
            failure,
        } = request;
        let binding = context.binding;
        let artifact_family = context.artifact_family;
        let state = self.recover_sequence_state(binding, Some(artifact_family))?;
        let sequence = ArtifactSequenceMetadata {
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
            producer_step_id: context.producer_step_id.to_string(),
        };
        let failure_sequence = failure
            .as_ref()
            .map(|_| checked_next_sequence(state.max_failure_sequence, "failure_sequence"))
            .transpose()?;
        let canonical_path = self.canonical_path(binding, artifact_family);
        let history_path = self.history_path(binding, artifact_family, &sequence);
        let mut value = serde_json::to_value(payload)
            .map_err(|err| artifact_error(format!("serialize artifact payload: {err}")))?;
        self.inject_store_fields(
            StoreFieldContext {
                write: context,
                sequence: &sequence,
                canonical_path: &canonical_path,
                history_path: &history_path,
                failure_sequence,
                failure,
            },
            &mut value,
        )?;
        validate_json_object(&value)?;
        validate_family_invariants(artifact_family, &value)?;
        let bytes = serde_json::to_vec_pretty(&value)
            .map_err(|err| artifact_error(format!("serialize artifact json: {err}")))?;
        self.publish_artifact(&history_path, &canonical_path, &bytes)?;
        Ok(ArtifactWriteRecord {
            sequence,
            canonical_path,
            history_path,
            failure_sequence,
        })
    }

    pub fn write_raw_text_artifact(
        &self,
        request: RawTextArtifactWriteRequest<'_>,
    ) -> Result<ArtifactWriteRecord, EngineError> {
        let RawTextArtifactWriteRequest {
            context,
            artifact_name,
            raw_text,
        } = request;
        let binding = context.binding;
        let artifact_family = context.artifact_family;
        self.with_binding_publication_lock(binding, || {
            let state = self.recover_sequence_state(binding, Some(artifact_family))?;
            let sequence = ArtifactSequenceMetadata {
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
                producer_step_id: context.producer_step_id.to_string(),
            };
            let canonical_path = self.canonical_path(binding, artifact_family);
            let history_path = self
                .history_binding_root(binding)
                .join(sanitize_path_segment(artifact_family))
                .join(format!(
                    "{}-{}-{}-{}.json",
                    sequence.artifact_sequence,
                    sequence.write_sequence,
                    sanitize_path_segment(&sequence.producer_step_id),
                    sanitize_path_segment(artifact_name)
                ));
            let mut value = serde_json::json!({
                "artifact_name": artifact_name,
                "raw_text": raw_text
            });
            self.inject_store_fields(
                StoreFieldContext {
                    write: context,
                    sequence: &sequence,
                    canonical_path: &canonical_path,
                    history_path: &history_path,
                    failure_sequence: None,
                    failure: None,
                },
                &mut value,
            )?;
            validate_json_object(&value)?;
            let bytes = serde_json::to_vec_pretty(&value).map_err(|err| {
                artifact_error(format!("serialize raw text artifact json: {err}"))
            })?;
            self.publish_artifact(&history_path, &canonical_path, &bytes)?;
            Ok(ArtifactWriteRecord {
                sequence,
                canonical_path,
                history_path,
                failure_sequence: None,
            })
        })
    }

    pub fn read_current_json(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
    ) -> Result<Value, EngineError> {
        let mut budget = path_safety::ReadBudget::default();
        let path = self.canonical_path(binding, artifact_family);
        let value = self.read_json_path_with_budget(&path, &mut budget)?;
        self.validate_artifact_value(binding, artifact_family, &value)?;
        validate_canonical_embedded_path(&value, &path)?;
        self.validate_artifact_invariants(artifact_family, &value)?;
        self.validate_canonical_matches_immutable_history_with_budget(
            binding,
            artifact_family,
            &value,
            &mut budget,
        )?;
        Ok(value)
    }

    pub(super) fn read_json_path(&self, path: &Path) -> Result<Value, EngineError> {
        self.read_json_path_with_budget(path, &mut path_safety::ReadBudget::default())
    }

    fn read_json_path_with_budget(
        &self,
        path: &Path,
        budget: &mut path_safety::ReadBudget,
    ) -> Result<Value, EngineError> {
        let content = path_safety::read_contained_file_with_budget(&self.root, path, budget)?;
        serde_json::from_str(&content)
            .map_err(|err| artifact_error(format!("parse {}: {err}", path.display())))
    }

    // Immutable history lookup methods are implemented in `history`.

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

    /// Validates per-family typed invariants for routing-relevant artifacts.
    /// Kept separate from `validate_artifact_value` so the family-agnostic
    /// sequence-recovery path is not subject to routing-state invariants.
    /// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
    /// @requirement:REQ-PRFU-007
    pub fn validate_artifact_invariants(
        &self,
        artifact_family: &str,
        value: &Value,
    ) -> Result<(), EngineError> {
        validate_family_invariants(artifact_family, value)
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

    fn inject_store_fields(
        &self,
        context: StoreFieldContext<'_>,
        value: &mut Value,
    ) -> Result<(), EngineError> {
        let StoreFieldContext {
            write,
            sequence,
            canonical_path,
            history_path,
            failure_sequence,
            failure,
        } = context;
        let object = value
            .as_object_mut()
            .ok_or_else(|| artifact_error("artifact payload must serialize to a JSON object"))?;
        insert_binding_fields(object, write.binding);
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
            Value::from(write.step_order_index),
        );
        object.insert(
            "history_metadata".to_string(),
            serde_json::to_value(HistoryMetadata {
                canonical_path: canonical_path.display().to_string(),
                history_path: history_path.display().to_string(),
                artifact_family: write.artifact_family.to_string(),
                is_canonical: true,
                history_written_at: write.clock.now_rfc3339(),
            })
            .map_err(|err| artifact_error(format!("serialize history metadata: {err}")))?,
        );
        if let (Some(next_failure_sequence), Some(failure)) = (failure_sequence, failure) {
            object.insert(
                "semantic_state".to_string(),
                Value::from(failure.semantic_state),
            );
            object.insert("failure_reason".to_string(), Value::from(failure.reason));
            object.insert(
                "failure_sequence".to_string(),
                Value::from(next_failure_sequence),
            );
            object.insert(
                "produced_at".to_string(),
                Value::from(write.clock.now_rfc3339()),
            );
            object.insert("failure_details".to_string(), failure.details);
        }
        Ok(())
    }

    fn current_binding_root(&self, binding: &PrFollowupBinding) -> PathBuf {
        self.binding_root("current", binding)
    }

    fn history_binding_root(&self, binding: &PrFollowupBinding) -> PathBuf {
        self.binding_root("history", binding)
    }

    pub fn history_root_for_family(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
    ) -> PathBuf {
        self.history_binding_root(binding)
            .join(sanitize_path_segment(artifact_family))
    }

    fn binding_root(&self, collection: &str, binding: &PrFollowupBinding) -> PathBuf {
        self.root
            .join("pr-followup")
            .join(collection)
            .join(sanitize_path_segment(&binding.run_id))
            .join(sanitize_path_segment(&binding.repository_owner))
            .join(sanitize_path_segment(&binding.repository_name))
            .join(binding.pr_number.to_string())
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002,REQ-PRFU-004
/// @pseudocode lines 5-7
impl ArtifactWriter for PrFollowupArtifactStore {
    fn canonical_path(&self, binding: &PrFollowupBinding, artifact_family: &str) -> PathBuf {
        self.current_binding_root(binding)
            .join(format!("{}.json", sanitize_path_segment(artifact_family)))
    }

    fn history_path(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
        sequence: &ArtifactSequenceMetadata,
    ) -> PathBuf {
        self.history_binding_root(binding)
            .join(sanitize_path_segment(artifact_family))
            .join(format!(
                "{}-{}-{}.json",
                sequence.artifact_sequence,
                sequence.write_sequence,
                sanitize_path_segment(&sequence.producer_step_id)
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
/// @requirement:REQ-PRFU-002,REQ-PRFU-004
/// @pseudocode lines 5-7
fn artifact_write_record(
    value: &Value,
    canonical_path: PathBuf,
) -> Result<ArtifactWriteRecord, EngineError> {
    let sequence = ArtifactSequenceMetadata {
        artifact_sequence: require_u64(value, "artifact_sequence")?,
        write_sequence: require_u64(value, "write_sequence")?,
        producer_step_id: require_string(value, "producer_step_id")?,
    };
    if sequence.artifact_sequence == 0 || sequence.write_sequence == 0 {
        return Err(artifact_error(
            "replayed artifact sequences must be positive",
        ));
    }
    let history_path = value
        .pointer("/history_metadata/history_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| artifact_error("replayed artifact is missing history path"))?;
    Ok(ArtifactWriteRecord {
        sequence,
        canonical_path,
        history_path,
        failure_sequence: value.get("failure_sequence").and_then(Value::as_u64),
    })
}

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

/// Dispatches per-family typed invariant validation for the artifact families
/// that participate in workflow routing decisions. Unknown families pass
/// through (only generic envelope/binding checks apply to them).
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-007
fn validate_family_invariants(artifact_family: &str, value: &Value) -> Result<(), EngineError> {
    match artifact_family {
        "pr-check-status" => {
            let typed: PrCheckStatus = serde_json::from_value(value.clone()).map_err(|err| {
                artifact_error(format!("deserialize pr-check-status artifact: {err}"))
            })?;
            typed.validate_invariants().map_err(artifact_error)
        }
        "ci-failures" => {
            let typed: CiFailures = serde_json::from_value(value.clone()).map_err(|err| {
                artifact_error(format!("deserialize ci-failures artifact: {err}"))
            })?;
            typed.validate_invariants().map_err(artifact_error)
        }
        "post-pr-failure-terminal" => terminal_validation::validate_terminal_artifact(value),
        _ => Ok(()),
    }
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

pub(super) fn checked_next_sequence(current: u64, sequence_name: &str) -> Result<u64, EngineError> {
    current.checked_add(1).ok_or_else(|| {
        artifact_error(format!(
            "cannot allocate {sequence_name}: sequence space is exhausted"
        ))
    })
}
