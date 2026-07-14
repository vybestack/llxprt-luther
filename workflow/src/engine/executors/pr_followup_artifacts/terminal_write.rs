//! Terminal direct-write artifact store operations.
//!
//! These methods support executors that bypass the normal `write_json_artifact`
//! path (e.g. the post-PR failure terminal) because they run in a failure
//! context where co-resident canonical artifacts may be corrupt. Sequence
//! allocation derives from immutable history snapshots only, and failure-
//! candidate enumeration excludes routing/planning artifacts.

use super::*;

impl PrFollowupArtifactStore {
    /// Computes the next sequence for a direct (bypass) write by scanning only
    /// immutable history snapshots. This avoids the full sequence-recovery scan
    /// which may fail on corrupt co-resident canonical artifacts. History files
    /// are immutable and were validated when written, so they are a safe source
    /// for sequence recovery.
    ///
    /// Returns the allocated `ArtifactSequenceMetadata` for the new write.
    pub fn next_sequence_from_history(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
        producer_step_id: &str,
    ) -> Result<ArtifactSequenceMetadata, EngineError> {
        let state = self.recover_sequence_state_from_history(binding)?;
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

    /// Computes the next failure sequence from history-only snapshots.
    pub fn next_failure_sequence_from_history(
        &self,
        binding: &PrFollowupBinding,
    ) -> Result<u64, EngineError> {
        let state = self.recover_sequence_state_from_history(binding)?;
        Ok(state.max_failure_sequence + 1)
    }

    /// Injects binding fields, sequence metadata, and history metadata into a
    /// value being prepared for a direct (bypass) write. This is the public
    /// equivalent of the private `inject_store_fields`, usable by executors
    /// that write directly (bypassing `write_json_artifact`) but still need
    /// complete artifact metadata.
    #[allow(clippy::too_many_arguments)]
    pub fn inject_artifact_metadata(
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
        self.inject_store_fields(
            binding,
            artifact_family,
            sequence,
            step_order_index,
            canonical_path,
            history_path,
            failure_sequence,
            failure,
            clock,
            value,
        )
    }

    /// Scans all current-head canonical artifacts for `binding` and returns
    /// those that carry a `failure_sequence` (failure artifacts). Each returned
    /// value is validated for binding identity. This is the candidate set used
    /// for deterministic terminal source selection.
    ///
    /// Routing and planning artifacts that may carry a `failure_sequence` for
    /// budget/sequence purposes (e.g. `pr-remediation-plan`,
    /// `pr-remediation-retry-state`) are excluded because they are not terminal
    /// failure sources — they describe the workflow's decision to halt, not the
    /// concrete failure condition that must be surfaced. The terminal artifact
    /// family itself is also excluded to prevent self-reference.
    pub fn read_current_failure_candidates(
        &self,
        binding: &PrFollowupBinding,
        excluded_family: &str,
    ) -> Result<Vec<Value>, EngineError> {
        let binding_dir = self
            .root
            .join("pr-followup")
            .join("current")
            .join(&binding.run_id)
            .join(&binding.repository_owner)
            .join(&binding.repository_name)
            .join(binding.pr_number.to_string());
        if !binding_dir.exists() {
            return Ok(Vec::new());
        }
        let mut paths = Vec::new();
        collect_json_paths(&binding_dir, &mut paths)?;
        paths.sort();
        let mut candidates = Vec::new();
        for path in &paths {
            let family = path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if family == excluded_family || is_routing_artifact(family) {
                continue;
            }
            let value = read_json_file(path)?;
            let actual = binding_from_value(&value)?;
            if !self.validate_binding(binding, &actual) {
                continue;
            }
            if value
                .get("failure_sequence")
                .and_then(Value::as_u64)
                .is_some()
            {
                candidates.push(value);
            }
        }
        Ok(candidates)
    }

    /// Recovers sequence state from immutable history snapshots only, skipping
    /// all canonical (current) artifacts. Used by direct-write paths (e.g.
    /// terminal artifact) that must allocate sequences even when co-resident
    /// canonical artifacts may be corrupt. History snapshots are immutable and
    /// were validated when written, so they are safe to scan.
    ///
    /// History files that cannot be parsed or validated are silently skipped.
    /// This is intentional: the terminal step operates in a failure context
    /// where corruption has already been handled by the retry-state recovery
    /// path (quarantine + tombstone). The terminal's sequence allocation
    /// derives the next monotonically-increasing sequence from the remaining
    /// valid history without enforcing strict contiguity, because skipped
    /// corrupt files may introduce gaps that have already been accounted for.
    fn recover_sequence_state_from_history(
        &self,
        binding: &PrFollowupBinding,
    ) -> Result<RecoveredSequenceState, EngineError> {
        let mut state = RecoveredSequenceState::default();
        let history_root = self.history_binding_root(binding);
        if history_root.exists() {
            let mut paths = Vec::new();
            collect_json_paths(&history_root, &mut paths)?;
            paths.sort();
            for path in &paths {
                let raw = match fs::read_to_string(path) {
                    Ok(raw) => raw,
                    Err(_) => continue,
                };
                let value: Value = match serde_json::from_str(&raw) {
                    Ok(value) => value,
                    Err(_) => continue,
                };
                let family = match artifact_family_from_value(&value) {
                    Some(family) => family,
                    None => continue,
                };
                if self
                    .validate_sequence_artifact_value(binding, &family, &value)
                    .is_err()
                {
                    continue;
                }
                if self
                    .validate_history_snapshot(&family, &value, path)
                    .is_err()
                {
                    continue;
                }
                if validate_history_filename(&family, &value, path).is_err() {
                    continue;
                }
                if validate_history_embedded_path(&value, path).is_err() {
                    continue;
                }
                // Accept the snapshot but ignore duplicate/contiguity errors
                // for this best-effort scan — we only need the max sequence.
                let _ = state.accept_snapshot(&family, &value, path);
            }
        }
        Ok(state)
    }

    /// Returns the history directory for a specific artifact family under the
    /// binding's history root. Used by corruption-recovery paths to scan
    /// immutable history snapshots.
    pub fn history_root_for_family(
        &self,
        binding: &PrFollowupBinding,
        artifact_family: &str,
    ) -> PathBuf {
        self.history_binding_root(binding)
            .join(sanitize_path_segment(artifact_family))
    }
}

/// Returns `true` if `family` is a routing/planning artifact that may carry a
/// `failure_sequence` for budget or sequence purposes but is NOT a terminal
/// failure source. These artifacts describe the workflow's decision to halt or
/// route, not a concrete failure condition that the terminal must surface.
///
/// Terminal candidate enumeration excludes these families so that only actual
/// failure-source artifacts (test results, remediation results, CI failures,
/// etc.) are eligible for deterministic source selection.
const ROUTING_ARTIFACT_FAMILIES: &[&str] = &[
    "pr-remediation-plan",
    "pr-remediation-retry-state",
    "pending-feedback-marker-actions",
    "post-pr-iteration-guard",
];

fn is_routing_artifact(family: &str) -> bool {
    ROUTING_ARTIFACT_FAMILIES.contains(&family)
}
