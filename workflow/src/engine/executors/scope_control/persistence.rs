//! Atomic, immutable persistence of task-charter and status artifacts.
//!
//! Artifacts live under `{artifact_dir}/scope-control/{run_id}/`. Writes are
//! performed via create-new temp-file + rename to ensure atomicity and
//! collision safety. Replays of an identical charter are idempotent; a missing
//! matching status is repaired safely. Conflicts are rejected.
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use super::decision::measurement_digest;
use super::evaluation::ScopeEvaluation;
use super::measurement::PatchMeasurement;
use super::model::CanonicalTaskCharter;

/// Directory name below the artifact root for scope-control data.
pub const SCOPE_CONTROL_DIR: &str = "scope-control";

/// Filename for the canonical task charter.
pub const CHARTER_FILENAME: &str = "task-charter.json";

/// Filename for the scope-control status read model.
pub const STATUS_FILENAME: &str = "status.json";

/// Status read model persisted alongside the charter.
///
/// The `measurement` and `evaluation` fields are `Option` because the status
/// is created at charter time (slice 1) before any measurement exists, then
/// updated by the measurement step (slice 2). The status file itself is
/// updatable (unlike the immutable charter file) so that each measurement
/// refreshes the snapshot.
///
/// `prior_measurement` captures the most recent *distinct* measurement round
/// (issue 142). It is overwritten only when a new measurement's digest differs
/// from the current `measurement`, which preserves idempotent same-snapshot
/// replay while exposing growth deltas in status output.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScopeStatus {
    pub charter_id: String,
    pub run_id: String,
    pub digest: String,
    pub merge_base: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Latest patch measurement snapshot, set by the measurement step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measurement: Option<PatchMeasurement>,
    /// Latest scope evaluation against the charter, set by the measurement
    /// step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluation: Option<ScopeEvaluation>,
    /// Timestamp of the last measurement update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The most recent *distinct* measurement snapshot (different digest from
    /// `measurement`). Used to compute growth deltas for status observability.
    /// Preserved across same-snapshot replays for idempotency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_measurement: Option<PatchMeasurement>,
    /// Digest of the measurement captured as `prior_measurement`. Kept
    /// alongside the snapshot so growth comparisons are deterministic and do
    /// not require recomputing the digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_measurement_digest: Option<String>,
    /// Timestamp when `prior_measurement` was promoted from the live
    /// measurement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_measured_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Errors produced by the persistence layer.
#[derive(Debug)]
pub enum PersistenceError {
    Io(std::io::Error),
    Json(serde_json::Error),
    AlreadyExists(PathBuf),
    Conflict { path: PathBuf, message: String },
}

impl std::fmt::Display for PersistenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "IO error: {err}"),
            Self::Json(err) => write!(f, "serialization error: {err}"),
            Self::AlreadyExists(path) => {
                write!(f, "artifact already exists (immutable): {}", path.display())
            }
            Self::Conflict { path, message } => {
                write!(f, "conflict at {}: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for PersistenceError {}

impl From<std::io::Error> for PersistenceError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<serde_json::Error> for PersistenceError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}

/// Resolve the per-run scope-control directory.
#[must_use]
pub fn scope_control_dir(artifact_dir: &Path, run_id: &str) -> PathBuf {
    artifact_dir.join(SCOPE_CONTROL_DIR).join(run_id)
}

/// Resolve the canonical charter path for a run.
#[must_use]
pub fn charter_path(dir: &Path) -> PathBuf {
    dir.join(CHARTER_FILENAME)
}

/// Resolve the status path for a run.
#[must_use]
pub fn status_path(dir: &Path) -> PathBuf {
    dir.join(STATUS_FILENAME)
}

/// Reject a run_id whose path components escape the artifact directory.
fn validate_run_id(run_id: &str) -> Result<(), PersistenceError> {
    if run_id.is_empty() {
        return Err(PersistenceError::Conflict {
            path: PathBuf::from(SCOPE_CONTROL_DIR),
            message: "run_id must not be empty".into(),
        });
    }
    let path = Path::new(run_id);
    if path.components().any(|c| {
        matches!(
            c,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    }) {
        return Err(PersistenceError::Conflict {
            path: PathBuf::from(run_id),
            message: "run_id must not contain path traversal components".into(),
        });
    }
    Ok(())
}

/// Atomically write `value` as pretty-printed JSON to `path`, refusing to
/// overwrite an existing file to enforce immutability.
///
/// The write is atomic and race-safe: a temp file is created with
/// `create_new` (refusing collisions), synced, then hard-linked into place.
/// `link()` is atomic and fails with `AlreadyExists` if the target path
/// already exists, closing the TOCTOU window that a pre-check + rename would
/// leave. If the final `path` already exists,
/// [`PersistenceError::AlreadyExists`] is returned without modifying the file.
///
/// The containing directory is fsynced after the link (where supported) so
/// that a crash does not leave the link metadata uncommitted.
pub fn write_immutable_json<T: Serialize>(path: &Path, value: &T) -> Result<(), PersistenceError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(value)?;
    write_atomic_create_new(path, json.as_bytes())?;
    if let Some(parent) = path.parent() {
        let _ = fsync_dir(parent);
    }
    Ok(())
}

/// Write `data` to `path` atomically using a temp file + hard link. Fails with
/// `AlreadyExists` if `path` already exists. Race-safe: concurrent writers
/// each create a unique temp file; the first `link()` succeeds and subsequent
/// ones fail cleanly.
fn write_atomic_create_new(path: &Path, data: &[u8]) -> Result<(), PersistenceError> {
    let temp_path = collision_safe_temp_path(path);
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        file.write_all(data)?;
        file.sync_all()?;
    }
    // hard_link is atomic: it fails if `path` already exists. This closes the
    // TOCTOU window between existence-check and write.
    if let Err(e) = fs::hard_link(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            return Err(PersistenceError::AlreadyExists(path.to_path_buf()));
        }
        return Err(PersistenceError::Io(e));
    }
    // Remove the temp link; the target link remains.
    let _ = fs::remove_file(&temp_path);
    Ok(())
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<(), PersistenceError> {
    let temp_path = collision_safe_temp_path(path);
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        file.write_all(data)?;
        file.sync_all()?;
    }
    fs::rename(&temp_path, path)?;
    Ok(())
}

/// Atomically write `value` as pretty-printed JSON to `path`, overwriting any
/// existing file.
///
/// Unlike [`write_immutable_json`], this is used for the status read model
/// which is updated by each measurement step. The write is still crash-safe:
/// a temp file is created, synced, and atomically renamed into place. A crash
/// during the write leaves either the old or the new file, never a partial
/// file. The containing directory is fsynced where supported.
pub fn write_updatable_json<T: Serialize>(path: &Path, value: &T) -> Result<(), PersistenceError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(value)?;
    write_atomic(path, json.as_bytes())?;
    if let Some(parent) = path.parent() {
        let _ = fsync_dir(parent);
    }
    Ok(())
}

/// Fsync the containing directory of an artifact so rename metadata survives
/// a crash. On platforms where directory fsync is not supported (or where the
/// directory cannot be opened) this is a best-effort no-op so callers never
/// fail solely because the fsync was unavailable.
fn fsync_dir(dir: &Path) -> Result<(), std::io::Error> {
    let file = std::fs::File::open(dir)?;
    let _ = file.sync_data();
    Ok(())
}

fn collision_safe_temp_path(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("artifact");
    let unique = uuid::Uuid::new_v4().simple().to_string();
    parent.join(format!(".{file_name}.tmp.{unique}"))
}

/// Read and deserialize an artifact file.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, PersistenceError> {
    let data = fs::read(path)?;
    Ok(serde_json::from_slice(&data)?)
}

/// Persist both the canonical charter and its status for a run.
///
/// Creates the run-scoped directory if needed, then writes `task-charter.json`
/// and `status.json` immutably. If both artifacts already exist and match the
/// charter, the operation is idempotent (returns the existing paths). If the
/// charter exists but the status is missing, the status is repaired. Any
/// conflict (mismatched charter digest, mismatched status) is rejected.
pub fn persist_charter_and_status(
    artifact_dir: &Path,
    charter: &CanonicalTaskCharter,
) -> Result<(PathBuf, PathBuf), PersistenceError> {
    validate_run_id(&charter.run_id)?;
    let dir = scope_control_dir(artifact_dir, &charter.run_id);
    fs::create_dir_all(&dir)?;
    let charter_p = charter_path(&dir);
    let status_p = status_path(&dir);

    if charter_p.exists() {
        let existing: CanonicalTaskCharter = read_json(&charter_p)?;
        if existing.digest != charter.digest {
            return Err(PersistenceError::Conflict {
                path: charter_p.clone(),
                message: format!(
                    "charter digest mismatch: existing '{}' vs new '{}'",
                    existing.digest, charter.digest
                ),
            });
        }
        // Charter matches; repair missing status if needed.
        if status_p.exists() {
            let existing_status: ScopeStatus = read_json(&status_p)?;
            if existing_status.digest != charter.digest {
                return Err(PersistenceError::Conflict {
                    path: status_p.clone(),
                    message: format!(
                        "status digest mismatch: existing '{}' vs charter '{}'",
                        existing_status.digest, charter.digest
                    ),
                });
            }
            return Ok((charter_p, status_p));
        }
        write_immutable_json(&status_p, &status_for(charter))?;
        return Ok((charter_p, status_p));
    }

    write_immutable_json(&charter_p, charter)?;
    if status_p.exists() {
        let existing_status: ScopeStatus = read_json(&status_p)?;
        if existing_status.digest != charter.digest {
            return Err(PersistenceError::Conflict {
                path: status_p.clone(),
                message: format!(
                    "status digest mismatch: existing '{}' vs charter '{}'",
                    existing_status.digest, charter.digest
                ),
            });
        }
        return Ok((charter_p, status_p));
    }
    write_immutable_json(&status_p, &status_for(charter))?;
    Ok((charter_p, status_p))
}

fn status_for(charter: &CanonicalTaskCharter) -> ScopeStatus {
    ScopeStatus {
        charter_id: charter.charter_id.clone(),
        run_id: charter.run_id.clone(),
        digest: charter.digest.clone(),
        merge_base: charter.merge_base.clone(),
        created_at: chrono::Utc::now(),
        measurement: None,
        evaluation: None,
        measured_at: None,
        prior_measurement: None,
        prior_measurement_digest: None,
        prior_measured_at: None,
    }
}

/// Update the status read model with a measurement and evaluation snapshot.
///
/// This reads the existing status, preserves the immutable charter fields
/// (`charter_id`, `run_id`, `digest`, `merge_base`, `created_at`), sets the
/// measurement and evaluation fields, and writes the updated status atomically
/// via [`write_updatable_json`].
///
/// # Errors
/// Returns [`PersistenceError`] if the status file cannot be read or written,
/// or if the status digest does not match the charter digest (charter
/// immutability violation).
pub fn update_status_measurement(
    artifact_dir: &Path,
    run_id: &str,
    measurement: &PatchMeasurement,
    evaluation: &ScopeEvaluation,
) -> Result<(), PersistenceError> {
    validate_run_id(run_id)?;
    let dir = scope_control_dir(artifact_dir, run_id);
    let status_p = status_path(&dir);
    let charter_p = charter_path(&dir);

    let mut existing: ScopeStatus = read_json(&status_p)?;
    let charter: CanonicalTaskCharter = read_json(&charter_p)?;
    if existing.digest != charter.digest {
        return Err(PersistenceError::Conflict {
            path: status_p.clone(),
            message: format!(
                "status digest mismatch: existing '{}' vs charter '{}'",
                existing.digest, charter.digest
            ),
        });
    }

    // Promote the prior snapshot when the new measurement is *distinct* from
    // the live measurement. This preserves idempotent same-snapshot replay
    // (identical digest → prior unchanged) while capturing growth deltas when
    // the implementation round advances (issue 142).
    let new_digest = measurement_digest(measurement);
    let promoted_at = chrono::Utc::now();
    let prior_changed = match &existing.measurement {
        None => false, // First measurement: nothing to promote.
        Some(prev) => measurement_digest(prev) != new_digest,
    };
    if prior_changed {
        let prev = existing.measurement.clone().expect("checked Some above");
        existing.prior_measurement = Some(prev.clone());
        existing.prior_measurement_digest = Some(measurement_digest(&prev));
        existing.prior_measured_at = existing.measured_at.or(Some(promoted_at));
    }

    existing.measurement = Some(measurement.clone());
    existing.evaluation = Some(evaluation.clone());
    existing.measured_at = Some(promoted_at);

    write_updatable_json(&status_p, &existing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::executors::scope_control::model::{
        DraftBudget, DraftReviewCaps, DraftSubsystem, TaskCharterDraft, CHARTER_SCHEMA_VERSION,
    };
    use tempfile::TempDir;

    fn sample_charter() -> CanonicalTaskCharter {
        let draft = TaskCharterDraft {
            charter_id: "TEST-001".into(),
            issue_number: 42,
            run_id: "run-1".into(),
            merge_base: "abc123".into(),
            acceptance_criteria: vec!["AC-1".into()],
            non_goals: vec![],
            subsystems: vec![DraftSubsystem {
                id: "core".into(),
                paths: vec!["src/core".into()],
            }],
            budget: DraftBudget {
                max_files_changed: 5,
                max_added_lines: 200,
                max_new_modules: 2,
                max_dependencies_added: 0,
                max_public_apis_added: 3,
            },
            review_caps: DraftReviewCaps {
                initial_full_reviews: 1,
                max_delta_reviews: 2,
                final_acceptance_reviews: 1,
                max_mutating_remediation_rounds: 2,
            },
            mandatory_gates: vec!["cargo test".into()],
        };
        super::super::model::normalize_charter(&draft)
    }

    #[test]
    fn persist_writes_both_artifacts() {
        let tmp = TempDir::new().expect("tempdir");
        let charter = sample_charter();
        let (charter_p, status_p) =
            persist_charter_and_status(tmp.path(), &charter).expect("persist");

        assert!(charter_p.exists());
        assert!(status_p.exists());
    }

    #[test]
    fn persisted_charter_is_readable() {
        let tmp = TempDir::new().expect("tempdir");
        let charter = sample_charter();
        let (charter_p, _status_p) =
            persist_charter_and_status(tmp.path(), &charter).expect("persist");

        let read_back: CanonicalTaskCharter = read_json(&charter_p).expect("read");
        assert_eq!(read_back.charter_id, charter.charter_id);
        assert_eq!(read_back.digest, charter.digest);
        assert_eq!(read_back.schema_version, CHARTER_SCHEMA_VERSION);
    }

    #[test]
    fn persisted_status_is_readable() {
        let tmp = TempDir::new().expect("tempdir");
        let charter = sample_charter();
        let (_charter_p, status_p) =
            persist_charter_and_status(tmp.path(), &charter).expect("persist");

        let status: ScopeStatus = read_json(&status_p).expect("read");
        assert_eq!(status.charter_id, charter.charter_id);
        assert_eq!(status.digest, charter.digest);
        assert_eq!(status.merge_base, charter.merge_base);
    }

    #[test]
    fn second_persist_same_charter_is_idempotent() {
        let tmp = TempDir::new().expect("tempdir");
        let charter = sample_charter();
        let (charter_p_1, status_p_1) =
            persist_charter_and_status(tmp.path(), &charter).expect("first persist ok");

        let (charter_p_2, status_p_2) =
            persist_charter_and_status(tmp.path(), &charter).expect("replay is idempotent");

        assert_eq!(charter_p_1, charter_p_2);
        assert_eq!(status_p_1, status_p_2);
    }

    #[test]
    fn persist_repairs_missing_status() {
        let tmp = TempDir::new().expect("tempdir");
        let charter = sample_charter();
        let (charter_p, status_p) =
            persist_charter_and_status(tmp.path(), &charter).expect("persist");
        fs::remove_file(&status_p).expect("remove status");

        assert!(!status_p.exists());
        persist_charter_and_status(tmp.path(), &charter).expect("repair status");
        assert!(status_p.exists());

        let repaired: ScopeStatus = read_json(&status_p).expect("read repaired status");
        assert_eq!(repaired.digest, charter.digest);
        let _ = charter_p;
    }

    #[test]
    fn persist_rejects_conflicting_charter() {
        let tmp = TempDir::new().expect("tempdir");
        let charter = sample_charter();
        persist_charter_and_status(tmp.path(), &charter).expect("first persist ok");

        let draft = TaskCharterDraft {
            charter_id: "DIFFERENT".into(),
            issue_number: charter.issue_number,
            run_id: charter.run_id.clone(),
            merge_base: charter.merge_base.clone(),
            acceptance_criteria: charter.acceptance_criteria.clone(),
            non_goals: charter.non_goals.clone(),
            subsystems: charter
                .subsystems
                .iter()
                .map(|(id, paths)| DraftSubsystem {
                    id: id.clone(),
                    paths: paths.clone(),
                })
                .collect(),
            budget: DraftBudget {
                max_files_changed: charter.budget.max_files_changed,
                max_added_lines: charter.budget.max_added_lines,
                max_new_modules: charter.budget.max_new_modules,
                max_dependencies_added: charter.budget.max_dependencies_added,
                max_public_apis_added: charter.budget.max_public_apis_added,
            },
            review_caps: DraftReviewCaps {
                initial_full_reviews: charter.review_caps.initial_full_reviews,
                max_delta_reviews: charter.review_caps.max_delta_reviews,
                final_acceptance_reviews: charter.review_caps.final_acceptance_reviews,
                max_mutating_remediation_rounds: charter
                    .review_caps
                    .max_mutating_remediation_rounds,
            },
            mandatory_gates: charter.mandatory_gates.clone(),
        };
        let conflicting = super::super::model::normalize_charter(&draft);
        let result = persist_charter_and_status(tmp.path(), &conflicting);
        assert!(matches!(result, Err(PersistenceError::Conflict { .. })));
    }

    #[test]
    fn persist_rejects_traversal_run_id() {
        let tmp = TempDir::new().expect("tempdir");
        let mut charter = sample_charter();
        charter.run_id = "../escape".into();
        let result = persist_charter_and_status(tmp.path(), &charter);
        assert!(matches!(result, Err(PersistenceError::Conflict { .. })));
    }

    #[test]
    fn atomic_write_does_not_leave_temp_file() {
        let tmp = TempDir::new().expect("tempdir");
        let charter = sample_charter();
        let (charter_p, _) = persist_charter_and_status(tmp.path(), &charter).expect("persist");

        let parent = charter_p.parent().expect("parent dir");
        let temps: Vec<_> = fs::read_dir(parent)
            .expect("read dir")
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(".task-charter.json.tmp"))
            })
            .collect();
        assert!(temps.is_empty(), "temp file should have been renamed");
    }

    #[test]
    fn artifacts_under_correct_directory() {
        let tmp = TempDir::new().expect("tempdir");
        let charter = sample_charter();
        let (charter_p, _) = persist_charter_and_status(tmp.path(), &charter).expect("persist");

        let expected_parent = tmp.path().join(SCOPE_CONTROL_DIR).join("run-1");
        assert_eq!(charter_p.parent().expect("has parent"), expected_parent);
    }

    // --- Issue 142: prior-snapshot promotion and growth observability ---

    use crate::engine::executors::scope_control::evaluation::{
        ScopeEvaluation, Violation, ViolationCode,
    };
    use crate::engine::executors::scope_control::measurement::PatchMeasurement;

    fn within_budget_eval() -> ScopeEvaluation {
        ScopeEvaluation {
            within_budget: true,
            within_subsystems: true,
            at_merge_base: false,
            violations: vec![],
        }
    }

    /// Build a measurement with distinct totals so each call with different
    /// arguments yields a different digest.
    fn measurement(head: &str, files: u32, lines: u32) -> PatchMeasurement {
        PatchMeasurement {
            merge_base: "abc123".into(),
            head_sha: head.into(),
            divergence: 0,
            files_changed: files,
            added_lines: lines,
            binary_files: 0,
            new_modules: 1,
            dependencies_added: 0,
            content_digest: String::new(),
            public_apis_added: 0,
            changed_paths: vec!["src/core/a.rs".into()],
            changed_subsystems: vec!["core".into()],
            file_details: vec![],
        }
    }

    #[test]
    fn first_measurement_records_no_prior() {
        let tmp = TempDir::new().expect("tempdir");
        let charter = sample_charter();
        persist_charter_and_status(tmp.path(), &charter).expect("persist");

        let m = measurement("head1", 2, 50);
        update_status_measurement(tmp.path(), "run-1", &m, &within_budget_eval()).expect("update");

        let status: ScopeStatus =
            read_json(&status_path(&scope_control_dir(tmp.path(), "run-1"))).expect("read");
        assert!(status.measurement.is_some(), "current measurement set");
        assert!(
            status.prior_measurement.is_none(),
            "first measurement must not set a prior"
        );
        assert!(
            status.prior_measurement_digest.is_none(),
            "first measurement must not set a prior digest"
        );
        assert!(
            status.prior_measured_at.is_none(),
            "first measurement must not set a prior timestamp"
        );
    }

    #[test]
    fn same_snapshot_replay_is_idempotent_no_prior_promotion() {
        let tmp = TempDir::new().expect("tempdir");
        let charter = sample_charter();
        persist_charter_and_status(tmp.path(), &charter).expect("persist");

        let m = measurement("head1", 2, 50);

        // First measurement.
        update_status_measurement(tmp.path(), "run-1", &m, &within_budget_eval())
            .expect("first update");

        // Replay the identical snapshot.
        update_status_measurement(tmp.path(), "run-1", &m, &within_budget_eval())
            .expect("replay update");

        let status: ScopeStatus =
            read_json(&status_path(&scope_control_dir(tmp.path(), "run-1"))).expect("read");
        assert!(status.measurement.is_some());
        assert!(
            status.prior_measurement.is_none(),
            "replay of the same snapshot must not promote a prior"
        );
        assert!(
            status.prior_measurement_digest.is_none(),
            "replay must not set a prior digest"
        );
    }

    #[test]
    fn second_distinct_measurement_promotes_prior() {
        let tmp = TempDir::new().expect("tempdir");
        let charter = sample_charter();
        persist_charter_and_status(tmp.path(), &charter).expect("persist");

        let m1 = measurement("head1", 2, 50);
        let m2 = measurement("head2", 4, 120);

        update_status_measurement(tmp.path(), "run-1", &m1, &within_budget_eval())
            .expect("first update");
        let first_measured_at = {
            let s: ScopeStatus =
                read_json(&status_path(&scope_control_dir(tmp.path(), "run-1"))).expect("read");
            s.measured_at.expect("measured_at set")
        };

        update_status_measurement(tmp.path(), "run-1", &m2, &within_budget_eval())
            .expect("second update");

        let status: ScopeStatus =
            read_json(&status_path(&scope_control_dir(tmp.path(), "run-1"))).expect("read");
        let prior = status
            .prior_measurement
            .as_ref()
            .expect("prior promoted on distinct measurement");
        assert_eq!(prior.head_sha, "head1");
        assert_eq!(prior.files_changed, 2);
        assert_eq!(prior.added_lines, 50);

        let current = status
            .measurement
            .as_ref()
            .expect("current measurement set");
        assert_eq!(current.head_sha, "head2");
        assert_eq!(current.files_changed, 4);
        assert_eq!(current.added_lines, 120);

        // The prior digest must match the digest of the prior snapshot.
        assert_eq!(
            status.prior_measurement_digest.as_deref(),
            Some(measurement_digest(prior).as_str())
        );
        // The prior timestamp must reflect the previous measured_at.
        assert_eq!(status.prior_measured_at, Some(first_measured_at));
    }

    #[test]
    fn third_distinct_measurement_advances_prior_to_second() {
        let tmp = TempDir::new().expect("tempdir");
        let charter = sample_charter();
        persist_charter_and_status(tmp.path(), &charter).expect("persist");

        update_status_measurement(
            tmp.path(),
            "run-1",
            &measurement("head1", 2, 50),
            &within_budget_eval(),
        )
        .expect("first");
        update_status_measurement(
            tmp.path(),
            "run-1",
            &measurement("head2", 4, 120),
            &within_budget_eval(),
        )
        .expect("second");
        update_status_measurement(
            tmp.path(),
            "run-1",
            &measurement("head3", 5, 200),
            &within_budget_eval(),
        )
        .expect("third");

        let status: ScopeStatus =
            read_json(&status_path(&scope_control_dir(tmp.path(), "run-1"))).expect("read");
        let prior = status.prior_measurement.as_ref().expect("prior");
        assert_eq!(prior.head_sha, "head2", "prior should be the second round");
        let current = status.measurement.as_ref().expect("current");
        assert_eq!(current.head_sha, "head3");
    }

    #[test]
    fn prior_survives_over_budget_then_back_within() {
        // Ensure prior promotion works regardless of evaluation outcome.
        let tmp = TempDir::new().expect("tempdir");
        let charter = sample_charter();
        persist_charter_and_status(tmp.path(), &charter).expect("persist");

        let over_eval = ScopeEvaluation {
            within_budget: false,
            within_subsystems: true,
            at_merge_base: false,
            violations: vec![Violation {
                code: ViolationCode::BudgetAddedLines,
                message: "over".into(),
            }],
        };

        update_status_measurement(
            tmp.path(),
            "run-1",
            &measurement("head1", 2, 50),
            &within_budget_eval(),
        )
        .expect("first");
        update_status_measurement(
            tmp.path(),
            "run-1",
            &measurement("head2", 4, 600),
            &over_eval,
        )
        .expect("second over-budget");

        let status: ScopeStatus =
            read_json(&status_path(&scope_control_dir(tmp.path(), "run-1"))).expect("read");
        assert!(status.prior_measurement.is_some());
        assert!(status.evaluation.as_ref().is_some_and(|e| !e.within_budget));
    }
}
