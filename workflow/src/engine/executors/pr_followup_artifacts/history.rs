use super::*;

/// Proof that the complete run-wide artifact sequence ledger was validated for
/// one exact binding. The private binding prevents reuse across PRs or heads.
pub(crate) struct ValidatedHistoryLedger {
    binding: PrFollowupBinding,
}

struct HistoryEvidenceQuery<'a> {
    artifact_family: &'a str,
    source_head_sha: &'a str,
    output_head_sha: Option<&'a str>,
    evidence_sequence: Option<&'a ArtifactSequenceMetadata>,
}

impl HistoryEvidenceQuery<'_> {
    fn matches(&self, value: &Value) -> bool {
        self.sequence_matches(value)
            && value
                .get("input_head_sha")
                .and_then(Value::as_str)
                .unwrap_or_default()
                == self.source_head_sha
            && self.output_head_sha.is_none_or(|expected| {
                value
                    .get("output_head_sha")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    == expected
            })
    }

    fn sequence_matches(&self, value: &Value) -> bool {
        self.evidence_sequence.is_none_or(|sequence| {
            value.get("artifact_sequence").and_then(Value::as_u64)
                == Some(sequence.artifact_sequence)
                && value.get("write_sequence").and_then(Value::as_u64)
                    == Some(sequence.write_sequence)
                && value
                    .get("producer_step_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    == sequence.producer_step_id
        })
    }

    fn identity_description(&self) -> String {
        if let Some(sequence) = self.evidence_sequence {
            format!(
                "artifact_sequence={} write_sequence={} producer={}",
                sequence.artifact_sequence, sequence.write_sequence, sequence.producer_step_id
            )
        } else {
            format!(
                "source_head_sha={} output_head_sha={:?}",
                self.source_head_sha, self.output_head_sha
            )
        }
    }
}

impl PrFollowupArtifactStore {
    /// Reads an optional canonical artifact for the current head, returning
    /// `None` when the artifact is absent or when a prior-head artifact for
    /// the **same PR** occupies the canonical path. A prior-head artifact is
    /// stale with respect to the current head and must not poison the binding:
    /// it is treated as absent so that optional plan inputs (e.g.
    /// `post-pr-test-result`, which is not re-collected after a remediation
    /// push) gracefully degrade to `None` instead of raising a fatal binding
    /// mismatch.
    ///
    /// A genuinely different-PR artifact at the canonical path remains a
    /// binding-mismatch error because that indicates a routing or identity
    /// corruption that must not be silently swallowed.
    /// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
    /// @requirement:REQ-PRFU-002
    pub fn read_optional_current_json_for_head(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
    ) -> Result<Option<Value>, EngineError> {
        let path = self.canonical_path(binding, artifact_family);
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(artifact_error(format!("read {}: {err}", path.display()))),
        };
        let value: Value = serde_json::from_str(&content)
            .map_err(|err| artifact_error(format!("parse {}: {err}", path.display())))?;
        let actual = binding_from_value(&value)?;
        // Validate artifact family metadata before any stale-prior-head
        // shortcut: a wrong-family artifact at the canonical path is always
        // a fatal corruption, even if it happens to be a stale prior head.
        self.validate_artifact_metadata(artifact_family, &value)?;
        if actual.is_stale_prior_head_of(binding) {
            return Ok(None);
        }
        self.validate_artifact_value(binding, artifact_family, &value)?;
        self.validate_artifact_invariants(artifact_family, &value)?;
        Ok(Some(value))
    }

    /// Reads a canonical artifact that must be carried forward across a head
    /// change. Unlike `read_optional_current_json_for_head`, a prior-head
    /// artifact for the **same PR** is returned (not treated as absent)
    /// because the carried-forward artifact's contents (e.g. pending marker
    /// actions) embed per-action `source_head_sha` bindings that the consumer
    /// uses to locate immutable evidence from history.
    ///
    /// A genuinely different-PR artifact at the canonical path remains a
    /// fatal binding-mismatch error.
    /// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
    /// @requirement:REQ-PRFU-002
    pub fn read_carried_forward_json(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
    ) -> Result<Option<Value>, EngineError> {
        let path = self.canonical_path(binding, artifact_family);
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(artifact_error(format!("read {}: {err}", path.display()))),
        };
        let value: Value = serde_json::from_str(&content)
            .map_err(|err| artifact_error(format!("parse {}: {err}", path.display())))?;
        let actual = binding_from_value(&value)?;
        if actual.pr_identity_matches(binding) {
            // Same PR — validate metadata/invariants regardless of head match.
            self.validate_history_snapshot(artifact_family, &value, &path)?;
            validate_canonical_embedded_path(&value, &path)?;
            return Ok(Some(value));
        }
        // Different PR — this is a corruption, not a carry-forward.
        Err(artifact_error(format!(
            "artifact binding mismatch in carry-forward read: different PR identity in {}",
            path.display()
        )))
    }

    /// Locates an immutable history snapshot of `artifact_family` whose
    /// binding matches the PR identity of `binding` and whose payload
    /// `input_head_sha`/`output_head_sha` match `source_head_sha` and
    /// `output_head_sha`.
    ///
    /// Every candidate snapshot belonging to the same PR in the family
    /// directory is fully validated (artifact family metadata, history
    /// metadata, filename-embedded sequence identity, and sequence shape)
    /// before any candidate is accepted as evidence. A corrupt, wrong-family,
    /// malformed, or filename-mismatched artifact is a fatal error; it must
    /// never silently produce a false negative evidence miss. Because this
    /// directory is keyed by PR identity, an artifact belonging to a different
    /// PR is cross-contamination and is also fatal.
    ///
    /// When multiple candidates satisfy the head-identity criteria, the lookup
    /// fails as ambiguous rather than guessing. For exact-identity lookup,
    /// use [`Self::read_history_evidence_by_sequence`].
    /// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
    /// @requirement:REQ-PRFU-002
    pub fn read_history_json_by_head(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
        source_head_sha: &str,
        output_head_sha: Option<&str>,
    ) -> Result<Option<Value>, EngineError> {
        let ledger = self.validate_history_ledger(binding)?;
        self.read_validated_history_json_by_head(
            binding,
            &ledger,
            artifact_family,
            source_head_sha,
            output_head_sha,
        )
    }

    pub(crate) fn read_validated_history_json_by_head(
        &self,
        binding: &PrFollowupBinding,
        ledger: &ValidatedHistoryLedger,
        artifact_family: &str,
        source_head_sha: &str,
        output_head_sha: Option<&str>,
    ) -> Result<Option<Value>, EngineError> {
        self.read_history_evidence(
            binding,
            ledger,
            artifact_family,
            source_head_sha,
            output_head_sha,
            None,
        )
    }

    /// Cross-head evidence lookup anchored to an exact immutable sequence
    /// identity carried by the pending action. This selects the snapshot
    /// whose `artifact_sequence`/`write_sequence`/`producer_step_id` match
    /// `evidence_sequence`, and rejects any ambiguity at that identity.
    /// See [`Self::read_history_json_by_head`] for the corruption/ambiguity contract.
    /// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
    /// @requirement:REQ-PRFU-002
    pub fn read_history_evidence_by_sequence(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
        source_head_sha: &str,
        output_head_sha: Option<&str>,
        evidence_sequence: &ArtifactSequenceMetadata,
    ) -> Result<Option<Value>, EngineError> {
        let ledger = self.validate_history_ledger(binding)?;
        self.read_validated_history_evidence_by_sequence(
            binding,
            &ledger,
            artifact_family,
            source_head_sha,
            output_head_sha,
            evidence_sequence,
        )
    }

    pub(crate) fn validate_history_ledger(
        &self,
        binding: &PrFollowupBinding,
    ) -> Result<ValidatedHistoryLedger, EngineError> {
        self.recover_sequence_state(binding, None)?;
        Ok(ValidatedHistoryLedger {
            binding: binding.clone(),
        })
    }

    pub(crate) fn read_validated_history_evidence_by_sequence(
        &self,
        binding: &PrFollowupBinding,
        ledger: &ValidatedHistoryLedger,
        artifact_family: &str,
        source_head_sha: &str,
        output_head_sha: Option<&str>,
        evidence_sequence: &ArtifactSequenceMetadata,
    ) -> Result<Option<Value>, EngineError> {
        self.read_history_evidence(
            binding,
            ledger,
            artifact_family,
            source_head_sha,
            output_head_sha,
            Some(evidence_sequence),
        )
    }

    fn read_history_evidence(
        &self,
        binding: &PrFollowupBinding,
        ledger: &ValidatedHistoryLedger,
        artifact_family: &str,
        source_head_sha: &str,
        output_head_sha: Option<&str>,
        evidence_sequence: Option<&ArtifactSequenceMetadata>,
    ) -> Result<Option<Value>, EngineError> {
        if ledger.binding != *binding {
            return Err(artifact_error(
                "validated history ledger binding does not match evidence query binding",
            ));
        }
        let family_root = self.history_binding_root(binding).join(artifact_family);
        if !family_root.exists() {
            return Ok(None);
        }
        let query = HistoryEvidenceQuery {
            artifact_family,
            source_head_sha,
            output_head_sha,
            evidence_sequence,
        };
        let validated = self.validated_history_candidates(binding, &query, &family_root)?;
        let matches = validated
            .iter()
            .filter(|value| query.matches(value))
            .collect::<Vec<_>>();
        resolve_history_evidence(matches, &query, &family_root)
    }

    fn validated_history_candidates(
        &self,
        binding: &PrFollowupBinding,
        query: &HistoryEvidenceQuery<'_>,
        family_root: &Path,
    ) -> Result<Vec<Value>, EngineError> {
        let mut paths = Vec::new();
        collect_json_paths(family_root, &mut paths)?;
        paths.sort();
        paths
            .iter()
            .map(|path| {
                let raw = fs::read_to_string(path)
                    .map_err(|err| artifact_error(format!("read {}: {err}", path.display())))?;
                let value: Value = serde_json::from_str(&raw)
                    .map_err(|err| artifact_error(format!("parse {}: {err}", path.display())))?;
                let actual = binding_from_value(&value)?;
                if !actual.pr_identity_matches(binding) {
                    return Err(artifact_error(format!(
                        "history artifact binding mismatch under PR-keyed directory: {}",
                        path.display()
                    )));
                }
                self.validate_history_snapshot(query.artifact_family, &value, path)?;
                validate_history_filename(query.artifact_family, &value, path)?;
                validate_history_embedded_path(&value, path)?;
                Ok(value)
            })
            .collect()
    }
}

fn resolve_history_evidence(
    matches: Vec<&Value>,
    query: &HistoryEvidenceQuery<'_>,
    family_root: &Path,
) -> Result<Option<Value>, EngineError> {
    match matches.as_slice() {
        [] => Ok(None),
        [value] => Ok(Some((*value).clone())),
        _ => Err(artifact_error(format!(
            "ambiguous history evidence for {}: {} matched {} candidates in {}",
            query.artifact_family,
            query.identity_description(),
            matches.len(),
            family_root.display()
        ))),
    }
}

impl PrFollowupArtifactStore {
    /// Validates a history snapshot to the same standard as a canonical
    /// artifact read, plus an explicit check that the artifact family in the
    /// embedded `history_metadata` matches the requested family. This ensures
    /// that corrupt or wrong-family history artifacts are never silently
    /// treated as absent.
    pub(super) fn validate_history_snapshot(
        &self,
        artifact_family: &str,
        value: &Value,
        path: &Path,
    ) -> Result<(), EngineError> {
        let metadata_family = value
            .get("history_metadata")
            .and_then(|m| m.get("artifact_family"))
            .and_then(Value::as_str);
        match metadata_family {
            Some(family) if family == artifact_family => {}
            Some(family) => {
                return Err(artifact_error(format!(
                    "history artifact family mismatch: expected {artifact_family}, got {family} in {}",
                    path.display()
                )));
            }
            None => {
                return Err(artifact_error(format!(
                    "history artifact missing history_metadata.artifact_family in {}",
                    path.display()
                )));
            }
        }
        self.validate_artifact_metadata(artifact_family, value)?;
        self.validate_artifact_invariants(artifact_family, value)?;
        Ok(())
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
pub(super) fn collect_json_paths(root: &Path, paths: &mut Vec<PathBuf>) -> Result<(), EngineError> {
    for entry in fs::read_dir(root).map_err(|err| artifact_error(format!("read dir: {err}")))? {
        let entry = entry.map_err(|err| artifact_error(format!("read dir entry: {err}")))?;
        let path = entry.path();
        let file_type = fs::symlink_metadata(&path)
            .map_err(|err| artifact_error(format!("read metadata for {}: {err}", path.display())))?
            .file_type();
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_json_paths(&path, paths)?;
        } else if file_type.is_file()
            && path.extension().and_then(|ext| ext.to_str()) == Some("json")
        {
            paths.push(path);
        }
    }
    Ok(())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
pub(super) fn read_json_file(path: &Path) -> Result<Value, EngineError> {
    let content = fs::read_to_string(path)
        .map_err(|err| artifact_error(format!("read {}: {err}", path.display())))?;
    serde_json::from_str(&content)
        .map_err(|err| artifact_error(format!("parse {}: {err}", path.display())))
}

pub(super) fn validate_json_object(value: &Value) -> Result<(), EngineError> {
    if value.is_object() {
        Ok(())
    } else {
        Err(artifact_error("artifact JSON must be an object"))
    }
}

/// Validates that the history filename matches the artifact-embedded sequence
/// identity. History filenames are written by the store as
/// `{artifact_sequence}-{write_sequence}-{producer_step_id}.json`; a mismatch
/// between the filename and the embedded metadata indicates corruption
/// (manual rename, partial write, or cross-family contamination) and must be
/// rejected before the artifact can be used as evidence.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
pub(super) fn validate_history_filename(
    artifact_family: &str,
    value: &Value,
    path: &Path,
) -> Result<(), EngineError> {
    let artifact_sequence = value
        .get("artifact_sequence")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            artifact_error(format!("missing artifact_sequence in {}", path.display()))
        })?;
    let write_sequence = value
        .get("write_sequence")
        .and_then(Value::as_u64)
        .ok_or_else(|| artifact_error(format!("missing write_sequence in {}", path.display())))?;
    let producer_step_id = value
        .get("producer_step_id")
        .and_then(Value::as_str)
        .ok_or_else(|| artifact_error(format!("missing producer_step_id in {}", path.display())))?;
    if artifact_sequence == 0 || write_sequence == 0 {
        return Err(artifact_error(format!(
            "zero sequence value in {artifact_family} family at {}",
            path.display()
        )));
    }
    if producer_step_id.is_empty() {
        return Err(artifact_error(format!(
            "empty producer_step_id in {}",
            path.display()
        )));
    }
    let expected_stem = match value.get("artifact_name").and_then(Value::as_str) {
        Some(artifact_name) => format!(
            "{}-{}-{}-{}",
            artifact_sequence,
            write_sequence,
            producer_step_id,
            sanitize_path_segment(artifact_name)
        ),
        None => format!(
            "{}-{}-{}",
            artifact_sequence, write_sequence, producer_step_id
        ),
    };
    let actual_stem = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if actual_stem != expected_stem {
        return Err(artifact_error(format!(
            "history filename mismatch: expected exact stem {expected_stem}, got {actual_stem} in {}",
            path.display()
        )));
    }
    Ok(())
}

/// Validates that the embedded `history_metadata.history_path` matches the
/// actual on-disk path of the history snapshot. A mismatch indicates
/// corruption (manual rename, copy between stores, or cross-contamination)
/// and must be rejected so a history snapshot can never masquerade under a
/// false path identity.
///
/// Both paths are canonicalized before comparison so that platform-specific
/// symlink prefixes (e.g. macOS `/var` -> `/private/var`) do not produce
/// false positives.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
pub(super) fn validate_history_embedded_path(
    value: &Value,
    path: &Path,
) -> Result<(), EngineError> {
    let embedded_history = value
        .get("history_metadata")
        .and_then(|m| m.get("history_path"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            artifact_error(format!(
                "missing history_metadata.history_path in {}",
                path.display()
            ))
        })?;
    if !paths_match(embedded_history, path) {
        return Err(artifact_error(format!(
            "history path mismatch: embedded {embedded_history:?} does not match actual {}",
            path.display()
        )));
    }
    Ok(())
}

/// Validates that the embedded `history_metadata.canonical_path` matches the
/// actual on-disk canonical path. Used for canonical-path reads (including
/// carry-forward reads) to ensure the artifact's embedded canonical path
/// identity matches where it actually lives on disk.
///
/// Both paths are canonicalized before comparison so that platform-specific
/// symlink prefixes (e.g. macOS `/var` -> `/private/var`) do not produce
/// false positives when the store root was constructed via different
/// canonicalization paths.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
pub(super) fn validate_canonical_embedded_path(
    value: &Value,
    path: &Path,
) -> Result<(), EngineError> {
    let embedded_canonical = value
        .get("history_metadata")
        .and_then(|m| m.get("canonical_path"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            artifact_error(format!(
                "missing history_metadata.canonical_path in {}",
                path.display()
            ))
        })?;
    if !paths_match(embedded_canonical, path) {
        return Err(artifact_error(format!(
            "canonical path mismatch: embedded {embedded_canonical:?} does not match actual {}",
            path.display()
        )));
    }
    Ok(())
}

/// Compares persisted and current path identities after resolving filesystem
/// aliases. Both paths must exist and resolve to the same object; failed
/// canonicalization is rejected so missing or escaping lookalikes cannot pass.
pub(super) fn paths_match(embedded: &str, actual: &Path) -> bool {
    let Ok(embedded) = fs::canonicalize(Path::new(embedded)) else {
        return false;
    };
    let Ok(actual) = fs::canonicalize(actual) else {
        return false;
    };
    embedded == actual
}
