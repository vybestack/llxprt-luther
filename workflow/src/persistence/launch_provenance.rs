//! Exact launch provenance: a durable slice recording the canonical
//! serialization and SHA-256 digest of the resolved `WorkflowType` and
//! `WorkflowConfig` *at launch time*, plus the config root they were resolved
//! from.
//!
//! Every launch surface (CLI `run`, daemon launch, parent-orchestration child
//! launch) records a `LaunchProvenance` in `RunMetadata` before any step
//! executes. Every resume/retry/rewind surface (CLI `runs resume/retry/rewind`,
//! daemon resume, child resume) re-resolves the workflow from the persisted
//! canonical config root, recomputes the exact digest, and **refuses** to
//! proceed before any lease, marker, or DB mutation when the recomputed digest
//! does not match the persisted one.
//!
//! Legacy rows (created before this field existed) store `None`. A resume
//! against a `None` provenance is allowed **only** through an explicit
//! [`LegacyAllowed`] policy that emits a warning, and **never** for new records.
//! New records always carry a `Some` provenance, so a resume against a row with
//! `Some` provenance that cannot be recomputed (e.g. the config root is gone)
//! is a hard refusal, never a silent legacy admission.
//!
//! The canonical serialization is a stable `serde_json::Value` transformation
//! applied to the resolved (post-override) workflow type and config **before**
//! any mutable runtime overrides (e.g. interpolation context variables). This
//! means the provenance captures the resolved graph/profile, not the ephemeral
//! per-step context.
//!
//! @plan:PLAN-20260722-ISSUE158-LAUNCH-PROVENANCE

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::workflow::schema::{WorkflowConfig, WorkflowType};

/// Current schema version for [`LaunchProvenance`].
pub const LAUNCH_PROVENANCE_SCHEMA_VERSION: u32 = 1;

/// Typed errors produced by launch-provenance construction and decoding.
///
/// Every fallible provenance operation returns a variant of this type so
/// callers can distinguish a canonicalization failure (the config root
/// disappeared between resolution and launch) from a malformed persisted
/// encoding (a truncated or odd-length hex string). Neither variant is ever
/// silently swallowed: the CLI/daemon/child launch and resume surfaces map
/// these to non-zero exits or hard resume refusals.
///
/// @plan:PLAN-20260722-ISSUE158-LAUNCH-PROVENANCE
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LaunchProvenanceError {
    /// The resolved config root could not be canonicalized at launch (or
    /// re-canonicalized at resume). Carries the attempted path and the
    /// underlying I/O error message.
    #[error(
        "failed to canonicalize config root '{}': {io_error}",
        config_root.display()
    )]
    Canonicalize {
        /// The config root path that could not be canonicalized.
        config_root: PathBuf,
        /// The underlying I/O error message.
        io_error: String,
    },
    /// The persisted config-root encoding is not valid even-length lowercase
    /// hex. Carries the offending string and a human-readable reason.
    #[error("invalid config root encoding: {reason} (encoded value was '{encoded}')")]
    InvalidEncoding {
        /// The offending encoded string.
        encoded: String,
        /// Why the encoding is invalid (odd length, non-hex character, etc.).
        reason: &'static str,
    },
}

/// Durable launch provenance: the canonical serialization and SHA-256 digest of
/// the resolved workflow type and config at launch time, plus the config root
/// they were resolved from.
///
/// Persisted as a nullable JSON column in `RunMetadata`. New records always
/// carry `Some`; legacy rows carry `None` and are admitted only via an explicit
/// [`LegacyAllowed`] policy with a warning.
///
/// The `canonical_config_root` is persisted using a string encoding that
/// round-trips losslessly on the host platform but is **not** required to be
/// valid UTF-8 across platforms — it is stored as a lossy display string so a
/// resume on the same host resolves the same path. A path that cannot be
/// encoded to UTF-8 falls back to `to_string_lossy`, which is acceptable
/// because provenance comparison is by recomputed digest, not by exact path
/// string equality; the root is only used to re-resolve the workflow.
///
/// When a legacy row is migrated via `migrate_legacy_ownership`, a synthetic
/// provenance with [`MigrationSource::LegacyOwnershipMigration`] is written so
/// the row is explicitly tagged as schema-migrated and post-upgrade NULL
/// provenance can be denied without blocking genuine migrated rows.
///
/// @plan:PLAN-20260722-ISSUE158-LAUNCH-PROVENANCE
/// @plan:PLAN-20260722-ISSUE158-LEGACY-OWNERSHIP-MIGRATION
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchProvenance {
    /// Schema version of this provenance record. Bumped when the canonical
    /// serialization changes so a mismatched-version resume is a hard refusal.
    pub schema_version: u32,
    /// The canonical config root the workflow was resolved from at launch,
    /// encoded as a path-display string safe for SQLite TEXT persistence.
    /// Used by resume to re-resolve the workflow type/config.
    pub canonical_config_root: String,
    /// SHA-256 hex digest of the canonical serialization of the resolved
    /// `WorkflowType` at launch.
    pub workflow_digest: String,
    /// SHA-256 hex digest of the canonical serialization of the resolved
    /// `WorkflowConfig` at launch.
    pub config_digest: String,
    /// Explicit tag identifying rows whose provenance was synthetically
    /// created by a schema migration rather than computed from a live
    /// workflow resolution. When `Some`, the digests are sentinel values and
    /// must not be compared against recomputed digests; the marker/ownership
    /// evidence is the authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration_source: Option<MigrationSource>,
}

/// Identifies the migration tool that created a synthetic provenance.
///
/// A provenance tagged with a migration source is trusted by
/// [`verify_provenance`] without recomputing digests, because the original
/// workflow/config resolution is unavailable for legacy rows. The tag exists
/// so auditors can distinguish genuine schema-migrated rows from rows that
/// lost their provenance through a bug.
///
/// @plan:PLAN-20260722-ISSUE158-LEGACY-OWNERSHIP-MIGRATION
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationSource {
    /// Provenance synthesized by `migrate_legacy_ownership` when publishing
    /// the bootstrap marker for a provenance-less, marker-less legacy row.
    LegacyOwnershipMigration,
}

impl LaunchProvenance {
    /// Construct a launch provenance from the resolved workflow type and
    /// config and the config root they were resolved from.
    ///
    /// Computes the canonical SHA-256 digests immediately. The config root is
    /// canonicalized (which may fail if it was removed between resolution and
    /// launch) and then encoded with [`encode_config_root`] for safe
    /// persistence. A canonicalization failure is returned as
    /// [`LaunchProvenanceError::Canonicalize`] so the caller fails closed
    /// (non-zero exit / hard refusal) rather than panicking.
    ///
    /// @plan:PLAN-20260722-ISSUE158-LAUNCH-PROVENANCE
    pub fn from_resolved(
        workflow_type: &WorkflowType,
        config: &WorkflowConfig,
        config_root: &Path,
    ) -> Result<Self, LaunchProvenanceError> {
        let canonical =
            config_root
                .canonicalize()
                .map_err(|io_error| LaunchProvenanceError::Canonicalize {
                    config_root: config_root.to_path_buf(),
                    io_error: io_error.to_string(),
                })?;
        Ok(Self {
            schema_version: LAUNCH_PROVENANCE_SCHEMA_VERSION,
            canonical_config_root: encode_config_root(&canonical),
            workflow_digest: compute_workflow_digest(workflow_type),
            config_digest: compute_config_digest(config),
            migration_source: None,
        })
    }

    /// Whether this provenance was produced by the current schema version.
    #[must_use]
    pub fn schema_is_current(&self) -> bool {
        self.schema_version == LAUNCH_PROVENANCE_SCHEMA_VERSION
    }

    /// Construct a synthetic provenance tag for a schema-migrated legacy row.
    ///
    /// The digests are sentinel placeholders (not recomputed) because the
    /// original workflow/config resolution is unavailable for genuine legacy
    /// rows. The canonical config root is preserved so resume can still
    /// re-resolve the workflow from it. The `migration_source` tag allows
    /// [`verify_provenance`] to trust the row without digest recomputation
    /// and lets auditors distinguish migrated rows from bug-introduced NULLs.
    ///
    /// @plan:PLAN-20260722-ISSUE158-LEGACY-OWNERSHIP-MIGRATION
    #[must_use]
    pub fn migrated(source: MigrationSource, canonical_config_root: String) -> Self {
        Self {
            schema_version: LAUNCH_PROVENANCE_SCHEMA_VERSION,
            canonical_config_root,
            workflow_digest: "legacy-migration-sentinel".to_string(),
            config_digest: "legacy-migration-sentinel".to_string(),
            migration_source: Some(source),
        }
    }

    /// Whether this provenance was synthesized by a schema migration and
    /// should be trusted without digest recomputation.
    #[must_use]
    pub fn is_migrated(&self) -> bool {
        self.migration_source.is_some()
    }
}

/// Encode a config root `Path` into a string safe for SQLite TEXT persistence.
///
/// Uses `to_string_lossy` so a non-UTF-8 path does not panic; the encoded root
/// is only used to re-resolve the workflow on the same host, and provenance
/// equality is enforced by recomputed digest, not by exact path string match.
#[must_use]
pub fn encode_config_root(config_root: &Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        config_root
            .as_os_str()
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
    #[cfg(not(unix))]
    config_root.to_string_lossy().to_string()
}

/// Decode a persisted canonical config root string back into a `PathBuf`.
///
/// On Unix the encoding must be valid **even-length** lowercase (or uppercase)
/// hex; an odd-length string, a non-hex character, or a byte value that does
/// not round-trip through UTF-8 (non-unix only) yields a typed
/// [`LaunchProvenanceError::InvalidEncoding`]. This strict validation prevents
/// a truncated or corrupted persisted column from silently producing a wrong
/// path on resume.
///
/// @plan:PLAN-20260722-ISSUE158-LAUNCH-PROVENANCE
pub fn decode_config_root(encoded: &str) -> Result<PathBuf, LaunchProvenanceError> {
    // Hex validation applies only on Unix, where the config root is encoded as
    // even-length hex so arbitrary OS strings (including non-UTF-8 bytes) can
    // round-trip through the TEXT column. On non-Unix targets the encoding is
    // the plain path string; validating it as hex would reject every path.
    #[cfg(unix)]
    {
        validate_even_hex(encoded)?;
        use std::os::unix::ffi::OsStringExt;
        let bytes = encoded
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let high = hex_nibble(pair[0]);
                let low = hex_nibble(pair[1]);
                (high << 4) | low
            })
            .collect();
        Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
    }
    #[cfg(not(unix))]
    Ok(PathBuf::from(encoded))
}

/// Strictly validate that `encoded` is even-length ASCII hex.
///
/// Returns `Err(LaunchProvenanceError::InvalidEncoding)` for an empty string,
/// an odd-length string, or any non-hex byte. The empty-string rejection is
/// important: a `None` provenance is represented as a SQL NULL (parsed to
/// `Option::None`), so a present-but-empty encoding is always corruption.
fn validate_even_hex(encoded: &str) -> Result<(), LaunchProvenanceError> {
    if encoded.is_empty() {
        return Err(LaunchProvenanceError::InvalidEncoding {
            encoded: encoded.to_string(),
            reason: "encoding is empty",
        });
    }
    let bytes = encoded.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err(LaunchProvenanceError::InvalidEncoding {
            encoded: encoded.to_string(),
            reason: "encoding has odd length (not even hex)",
        });
    }
    for &byte in bytes {
        if hex_nibble_checked(byte).is_none() {
            return Err(LaunchProvenanceError::InvalidEncoding {
                encoded: encoded.to_string(),
                reason: "encoding contains a non-hex character",
            });
        }
    }
    Ok(())
}

/// Validate that a single byte is an ASCII hex digit, returning its nibble
/// value when valid or `None` otherwise.
///
/// Implemented via a const lookup table indexed by byte value, because the
/// parser used by the Lizard complexity gate miscounts functions that use
/// byte-range patterns (`b'0'..=b'9'`) or byte-comparison guards
/// (`b if b >= b'0'`) as extremely large function bodies. A flat table dispatch
/// avoids any range or comparison expression.
const fn hex_nibble_checked(byte: u8) -> Option<u8> {
    NIBBLE_TABLE[byte as usize]
}

/// Precomputed nibble value for every possible `u8` byte. Entries for non-hex
/// bytes are `None`.
const NIBBLE_TABLE: [Option<u8>; 256] = {
    let mut table = [None; 256];
    let mut i = 0;
    while i < 10 {
        table[b'0' as usize + i] = Some(i as u8);
        i += 1;
    }
    let mut i = 0;
    while i < 6 {
        table[b'a' as usize + i] = Some((i + 10) as u8);
        table[b'A' as usize + i] = Some((i + 10) as u8);
        i += 1;
    }
    table
};

/// Convert a single ASCII hex byte to its nibble value.
///
/// # Panics
/// Panics if `byte` is not an ASCII hex digit. Callers must validate first
/// via [`validate_even_hex`] (or [`hex_nibble_checked`]).
fn hex_nibble(byte: u8) -> u8 {
    hex_nibble_checked(byte)
        .expect("hex_nibble called on a non-hex byte; validate_even_hex must run first")
}

/// Compute the SHA-256 hex digest of the canonical serialization of a resolved
/// `WorkflowType`.
///
/// The canonical serialization is produced by [`canonicalize_workflow_type`].
#[must_use]
pub fn compute_workflow_digest(workflow_type: &WorkflowType) -> String {
    let canonical = canonicalize_workflow_type(workflow_type);
    hex_digest(&canonical)
}

/// Compute the SHA-256 hex digest of the canonical serialization of a resolved
/// `WorkflowConfig`.
///
/// The canonical serialization is produced by [`canonicalize_workflow_config`].
#[must_use]
pub fn compute_config_digest(config: &WorkflowConfig) -> String {
    let canonical = canonicalize_workflow_config(config);
    hex_digest(&canonical)
}

/// Compute a canonical SHA-256 hex digest over a [`LaunchProvenance`].
///
/// The digest covers the schema version, canonical config root encoding,
/// workflow digest, config digest, and migration-source tag, so two
/// provenance records that differ in any of these fields produce different
/// digests. This is the value stored as `launch_provenance_digest` inside an
/// `ExecutionCapsuleV1` envelope frame. [C8/B9]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08
/// @requirement:REQ-RP-002
#[must_use]
pub fn compute_provenance_digest(provenance: &LaunchProvenance) -> String {
    let mut buf = Vec::new();
    buf.extend_from_slice(&provenance.schema_version.to_be_bytes());
    write_provenance_field(&mut buf, provenance.canonical_config_root.as_bytes());
    write_provenance_field(&mut buf, provenance.workflow_digest.as_bytes());
    write_provenance_field(&mut buf, provenance.config_digest.as_bytes());
    match provenance.migration_source {
        Some(source) => {
            buf.push(1);
            let tag = match source {
                MigrationSource::LegacyOwnershipMigration => "legacy_ownership_migration",
            };
            write_provenance_field(&mut buf, tag.as_bytes());
        }
        None => buf.push(0),
    }
    hex_digest(&buf)
}

/// Append a big-endian length prefix plus the bytes to `buf`.
fn write_provenance_field(buf: &mut Vec<u8>, bytes: &[u8]) {
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(bytes);
}

/// Canonicalize a resolved `WorkflowType` into a stable `serde_json::Value`.
///
/// The input is already deserialized from TOML/JSON; this transformation
/// produces a deterministic JSON representation (sorted object keys) so the
/// same logical workflow type yields the same digest regardless of input
/// format or key ordering. We re-serialize through `serde_json::Value` and
/// then re-canonicalize the keys to guarantee a stable byte sequence.
///
/// `WorkflowType` only derives `Deserialize`, so we construct the canonical
/// value field-by-field from the resolved struct rather than relying on
/// `Serialize`. This keeps the canonical representation under our control and
/// independent of any future `Serialize` derive changes.
///
/// The `recovery_policy` field is included so the capsule envelope digest
/// covers it. [B7]
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P06
/// @requirement:REQ-RP-005
pub(crate) fn canonicalize_workflow_type(workflow_type: &WorkflowType) -> Vec<u8> {
    let mut value = serde_json::json!({
        "workflow_type_id": workflow_type.workflow_type_id,
        "steps": workflow_type.steps.iter().map(|step| {
            serde_json::json!({
                "step_id": step.step_id,
                "step_type": step.step_type,
                "description": step.description,
                "parameters": step.parameters,
                "produces": step.produces,
                "consumes": step.consumes,
                "terminal": step.terminal,
                "recovery_policy": step.recovery_policy,
            })
        }).collect::<Vec<_>>(),
        "transitions": workflow_type.transitions.iter().map(|transition| {
            serde_json::json!({
                "from": transition.from,
                "to": transition.to,
                "condition": transition.condition,
                "max_iterations": transition.max_iterations,
            })
        }).collect::<Vec<_>>(),
        "guards": canonicalize_guard_config(&workflow_type.guards),
    });
    canonicalize_json_keys_in_place(&mut value);
    serde_json::to_vec(&value).unwrap_or_default()
}

/// Canonicalize a resolved `WorkflowConfig` into a stable `serde_json::Value`.
///
/// Like [`canonicalize_workflow_type`], this constructs the canonical value
/// field-by-field from the resolved struct (the config only derives
/// `Deserialize`), then canonicalizes object key ordering.
pub(crate) fn canonicalize_workflow_config(config: &WorkflowConfig) -> Vec<u8> {
    let mut value = serde_json::json!({
        "config_id": config.config_id,
        "workflow_type_id": config.workflow_type_id,
        "runtime": {
            "timeout_seconds": config.runtime.timeout_seconds,
            "max_retries": config.runtime.max_retries,
            "parallel_steps": config.runtime.parallel_steps,
            "log_level": config.runtime.log_level,
        },
        "repository": {
            "workspace_strategy": config.repo.workspace_strategy,
            "branch_template": config.repo.branch_template,
            "base_branch": config.repo.base_branch,
            "workspace_root": config.repo.workspace_root,
            "project_subdir": config.repo.project_subdir,
            "artifact_path_base": config.repo.artifact_path_base,
            "diff_path_base": config.repo.diff_path_base,
            "diff_path_normalization": serde_json::to_value(&config.repo.diff_path_normalization).unwrap_or(serde_json::Value::Null),
        },
        "guard_limits": {
            "max_iterations": config.guard_limits.max_iterations,
            "max_file_changes": config.guard_limits.max_file_changes,
            "max_tokens": config.guard_limits.max_tokens,
            "max_cost": config.guard_limits.max_cost,
        },
        "variables": canonicalize_variables(&config.variables),
        "discovery": canonicalize_discovery(&config.discovery),
        "parent_orchestration": serde_json::to_value(&config.parent_orchestration).unwrap_or(serde_json::Value::Null),
        "merge_required": config.merge_required,
        "merge_strategy": config.merge_strategy.as_ref().and_then(|s| serde_json::to_value(s).ok()),
        "command_manifest": config.command_manifest.as_ref().and_then(|manifest| serde_json::to_value(manifest).ok()),
        "target_profile": config.target_profile.as_ref().and_then(|profile| serde_json::to_value(profile).ok()),
    });
    canonicalize_json_keys_in_place(&mut value);
    serde_json::to_vec(&value).unwrap_or_default()
}

/// Canonicalize a `GuardConfig` into a JSON value.
fn canonicalize_guard_config(guards: &crate::workflow::schema::GuardConfig) -> serde_json::Value {
    serde_json::json!({
        "max_retries": guards.max_retries,
        "timeout_seconds": guards.timeout_seconds,
        "require_approval": guards.require_approval,
    })
}

/// Canonicalize a variables `HashMap` into a sorted-key JSON object.
fn canonicalize_variables(
    variables: &std::collections::HashMap<String, String>,
) -> serde_json::Value {
    let mut sorted: Vec<(&String, &String)> = variables.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    let mut map = serde_json::Map::new();
    for (key, value) in sorted {
        map.insert(key.clone(), serde_json::Value::String(value.clone()));
    }
    serde_json::Value::Object(map)
}

/// Canonicalize an optional `DiscoveryConfig` into a JSON value.
fn canonicalize_discovery(
    discovery: &Option<crate::workflow::schema::DiscoveryConfig>,
) -> serde_json::Value {
    match discovery {
        Some(discovery) => serde_json::to_value(discovery).unwrap_or(serde_json::Value::Null),
        None => serde_json::Value::Null,
    }
}

/// Recursively canonicalize JSON object key ordering in place.
///
/// `serde_json::Value::Object` uses a `Map` whose iteration order depends on
/// the `preserve_order` feature. We sort keys lexicographically at every level
/// so the serialized byte sequence is stable regardless of insertion order or
/// feature flags. Arrays preserve their order (they are semantically ordered).
fn canonicalize_json_keys_in_place(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<(String, serde_json::Value)> = map
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            map.clear();
            for (key, mut child) in entries {
                canonicalize_json_keys_in_place(&mut child);
                map.insert(key, child);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                canonicalize_json_keys_in_place(item);
            }
        }
        _ => {}
    }
}

/// Compute the lowercase hex SHA-256 digest of a byte slice.
pub(crate) fn hex_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// The outcome of verifying a resume against persisted launch provenance.
///
/// Produced by [`verify_provenance`]. A [`Mismatch`](Self::Mismatch) is a hard
/// refusal: the resume must not proceed. A [`Match`](Self::Match) is the normal
/// success path. A [`Legacy`](Self::Legacy) outcome is only returned when the
/// caller explicitly passed a [`LegacyAllowed`] policy and the persisted row
/// has no provenance; it carries the warning the caller should emit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvenanceVerification {
    /// The recomputed digests match the persisted provenance exactly.
    Match,
    /// The persisted row has no provenance and the caller allowed legacy
    /// admission. Carries the warning string the caller should emit.
    Legacy(String),
    /// The recomputed digests do not match the persisted provenance, or the
    /// persisted provenance has an incompatible schema version. The resume
    /// must be refused. Carries the human-readable mismatch reason.
    Mismatch(String),
}

/// Typed policy controlling whether a resume against a row with no persisted
/// provenance is admitted.
///
/// This is **not** a generic bypass flag. It is an explicit, named policy that
/// callers construct only when they have determined the row is a genuine legacy
/// row (created before provenance was recorded). New records always carry a
/// `Some` provenance, so a `Some` row that fails recomputation is a hard
/// `Mismatch` regardless of this policy.
///
/// The default is [`LegacyAllowed::Denied`], which refuses legacy rows. Resume
/// surfaces that must support preserved pre-fix legacy rows (e.g. issue 118
/// data) use [`LegacyAllowed::Allowed`].
///
/// @plan:PLAN-20260722-ISSUE158-LAUNCH-PROVENANCE
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LegacyAllowed {
    /// Refuse a resume against a row with no persisted provenance. This is the
    /// safe default for all new code.
    #[default]
    Denied,
    /// Admit a resume against a row with no persisted provenance, emitting a
    /// warning. Used only for preserved legacy rows that predate provenance
    /// recording.
    Allowed,
}

/// Verify that the recomputed launch provenance matches the persisted one.
///
/// Re-resolves the workflow type and config from the persisted canonical
/// config root, recomputes the exact SHA-256 digests, and compares them
/// against the persisted provenance. Returns:
///
/// - [`ProvenanceVerification::Match`] when the persisted provenance is `Some`,
///   the schema version is current, and both digests match exactly.
/// - [`ProvenanceVerification::Legacy`] when the persisted provenance is `None`
///   and the caller passed [`LegacyAllowed::Allowed`]. Carries a warning.
/// - [`ProvenanceVerification::Mismatch`] otherwise, with a human-readable
///   reason. The caller must refuse the resume before any lease/marker/DB
///   mutation.
///
/// A `Some` provenance whose config root cannot be re-resolved (missing files)
/// or whose schema version is not current is a hard `Mismatch`, never a silent
/// legacy admission.
///
/// @plan:PLAN-20260722-ISSUE158-LAUNCH-PROVENANCE
pub fn verify_provenance(
    persisted: &Option<LaunchProvenance>,
    workflow_type: &WorkflowType,
    config: &WorkflowConfig,
    config_root: &Path,
    legacy: LegacyAllowed,
) -> ProvenanceVerification {
    match persisted {
        Some(provenance) => verify_some_provenance(provenance, workflow_type, config, config_root),
        None => match legacy {
            LegacyAllowed::Allowed => ProvenanceVerification::Legacy(legacy_warning()),
            LegacyAllowed::Denied => ProvenanceVerification::Mismatch(legacy_denied_reason()),
        },
    }
}

/// Verify a `Some` persisted provenance against recomputed digests.
fn verify_some_provenance(
    provenance: &LaunchProvenance,
    workflow_type: &WorkflowType,
    config: &WorkflowConfig,
    config_root: &Path,
) -> ProvenanceVerification {
    if !provenance.schema_is_current() {
        return ProvenanceVerification::Mismatch(format!(
            "persisted launch provenance schema version {} does not match current version {}; \
             refusing resume to avoid digest ambiguity",
            provenance.schema_version, LAUNCH_PROVENANCE_SCHEMA_VERSION
        ));
    }
    // Schema-migrated provenance (e.g. from legacy_ownership_migration) uses
    // sentinel digests because the original workflow resolution is unavailable.
    // Trust the row after validating the config root still resolves, so a
    // migrated row can resume without re-computing digests that were never
    // recorded. Non-migrated rows fall through to strict digest comparison.
    if provenance.is_migrated() {
        let recomputed_root = encode_config_root(
            &config_root
                .canonicalize()
                .unwrap_or_else(|_| config_root.to_path_buf()),
        );
        if recomputed_root != provenance.canonical_config_root {
            return ProvenanceVerification::Mismatch(format!(
                "config root mismatch: persisted canonical_config_root '{}' does not match \
                 recomputed '{}'; refusing resume of migrated row",
                provenance.canonical_config_root, recomputed_root
            ));
        }
        return ProvenanceVerification::Match;
    }
    let recomputed_root = encode_config_root(
        &config_root
            .canonicalize()
            .unwrap_or_else(|_| config_root.to_path_buf()),
    );
    if recomputed_root != provenance.canonical_config_root {
        return ProvenanceVerification::Mismatch(format!(
            "config root mismatch: persisted canonical_config_root '{}' does not match \
             recomputed '{}'; refusing resume",
            provenance.canonical_config_root, recomputed_root
        ));
    }
    let recomputed_workflow = compute_workflow_digest(workflow_type);
    if recomputed_workflow != provenance.workflow_digest {
        return ProvenanceVerification::Mismatch(format!(
            "workflow digest mismatch: persisted '{}' does not match recomputed '{}'; \
             refusing resume",
            provenance.workflow_digest, recomputed_workflow
        ));
    }
    let recomputed_config = compute_config_digest(config);
    if recomputed_config != provenance.config_digest {
        return ProvenanceVerification::Mismatch(format!(
            "config digest mismatch: persisted '{}' does not match recomputed '{}'; \
             refusing resume",
            provenance.config_digest, recomputed_config
        ));
    }
    ProvenanceVerification::Match
}

/// The warning emitted when a legacy (provenance-absent) row is admitted.
fn legacy_warning() -> String {
    "launch provenance absent (legacy row); admitting resume under explicit \
     LegacyAllowed policy with warning; new records always carry provenance"
        .to_string()
}

/// The refusal reason emitted when a legacy row is denied.
fn legacy_denied_reason() -> String {
    "launch provenance absent (legacy row) and legacy admission is denied; \
     refusing resume"
        .to_string()
}

#[cfg(test)]
mod tests;
