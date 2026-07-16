//! Timeout recovery: frozen partial snapshot (issue 142, slice 6).
//!
//! When an llxprt timeout or idle_timeout occurs after worktree changes,
//! Luther must:
//!
//! 1. Freeze mutation (persist a partial snapshot).
//! 2. Run the configured targeted compile/check gate where feasible.
//! 3. Map every changed path to a charter subsystem and acceptance criterion.
//! 4. Expose a recovery-required status.
//! 5. Block broad continuation through the scope barrier until an explicit
//!    scope decision is made.
//!
//! This module provides the data model and persistence for the frozen partial
//! snapshot. The barrier integration reuses
//! [`enforce_scope_barrier`](super::decision::enforce_scope_barrier).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::measurement::PatchMeasurement;
use super::model::CanonicalTaskCharter;
use super::persistence::{read_json, scope_control_dir, write_updatable_json, PersistenceError};

/// Filename for the timeout recovery snapshot artifact.
pub const TIMEOUT_SNAPSHOT_FILENAME: &str = "timeout-snapshot.json";

/// The kind of timeout that triggered the recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeoutKind {
    Timeout,
    IdleTimeout,
}

/// A changed path mapped to its charter subsystem and acceptance criterion.
///
/// When no subsystem matches, the path is flagged as `unmapped`, which blocks
/// continuation until a scope decision resolves it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MappedChange {
    pub path: String,
    /// The subsystem ID from the charter that contains this path, or `None`
    /// if the path is outside all declared subsystems.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subsystem: Option<String>,
}

/// The frozen partial snapshot persisted when a timeout occurs after
/// worktree changes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TimeoutSnapshot {
    pub run_id: String,
    pub charter_id: String,
    pub charter_digest: String,
    pub timeout_kind: TimeoutKind,
    /// Snapshot of the worktree measurement at the time of timeout.
    pub measurement: PatchMeasurement,
    /// Changed paths mapped to charter subsystems.
    #[serde(default)]
    pub mapped_changes: Vec<MappedChange>,
    /// Whether a partial compile check was configured.
    #[serde(default)]
    pub partial_compile_configured: bool,
    /// Whether the partial compile check passed (if run).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial_compile_passed: Option<bool>,
    /// Process evidence from the timed-out llxprt process.
    #[serde(default)]
    pub process_evidence: ProcessEvidence,
    /// Whether recovery is required before broad continuation.
    pub recovery_required: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Process evidence captured from the timed-out llxprt process, persisted
/// in the frozen snapshot so an operator (or the scope decision replay)
/// can inspect the exact failure boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ProcessEvidence {
    /// The exit code set by the executor (124 for timeout, or the process's
    /// own exit code if it was killed mid-stream). `None` when the process
    /// was killed without an exit code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Whether the timeout was the wall-clock limit (`timeout`) rather than
    /// output-stall (`idle_timeout`).
    pub wall_clock_timeout: bool,
    /// Whether the process was killed (SIGKILL/terminate) versus exiting on
    /// its own.
    #[serde(default)]
    pub process_killed: bool,
}

/// Read-model status for timeout recovery, projected into the scope status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TimeoutRecoveryStatus {
    pub recovery_required: bool,
    pub timeout_kind: TimeoutKind,
    pub unmapped_path_count: u32,
    pub mapped_changes_count: u32,
    pub partial_compile_configured: bool,
    pub partial_compile_passed: Option<bool>,
}

impl TimeoutRecoveryStatus {
    /// Extract the status projection from a snapshot.
    #[must_use]
    pub fn from_snapshot(snapshot: &TimeoutSnapshot) -> Self {
        let unmapped = snapshot
            .mapped_changes
            .iter()
            .filter(|c| c.subsystem.is_none())
            .count() as u32;
        Self {
            recovery_required: snapshot.recovery_required,
            timeout_kind: snapshot.timeout_kind,
            unmapped_path_count: unmapped,
            mapped_changes_count: snapshot.mapped_changes.len() as u32,
            partial_compile_configured: snapshot.partial_compile_configured,
            partial_compile_passed: snapshot.partial_compile_passed,
        }
    }
}

/// Map changed paths to charter subsystems.
///
/// A path maps to a subsystem if it falls within that subsystem's path
/// prefixes. Paths that match no subsystem are flagged as `None` (unmapped),
/// which prevents silent scope accumulation.
#[must_use]
pub fn map_changes_to_subsystems(
    changed_paths: &[String],
    charter: &CanonicalTaskCharter,
) -> Vec<MappedChange> {
    changed_paths
        .iter()
        .map(|path| {
            let subsystem = find_subsystem(path, charter);
            MappedChange {
                path: path.clone(),
                subsystem,
            }
        })
        .collect()
}

fn find_subsystem(path: &str, charter: &CanonicalTaskCharter) -> Option<String> {
    charter
        .subsystems
        .iter()
        .flat_map(|(sub_id, prefixes)| {
            prefixes
                .iter()
                .filter(|prefix| !prefix.is_empty() && is_path_within(path, prefix))
                .map(move |prefix| (prefix.len(), sub_id))
        })
        .max_by_key(|(prefix_len, _)| *prefix_len)
        .map(|(_, sub_id)| sub_id.clone())
}

fn is_path_within(path: &str, prefix: &str) -> bool {
    let normalized_prefix = prefix
        .strip_suffix("/**")
        .or_else(|| prefix.strip_suffix("/"))
        .unwrap_or(prefix);
    if path == normalized_prefix {
        return true;
    }
    let path = Path::new(path);
    let prefix = Path::new(normalized_prefix);
    path.starts_with(prefix)
}

/// Resolve the timeout snapshot artifact path.
fn timeout_snapshot_path(artifact_dir: &Path, run_id: &str) -> PathBuf {
    scope_control_dir(artifact_dir, run_id).join(TIMEOUT_SNAPSHOT_FILENAME)
}

/// Read the timeout snapshot for a run, if one exists.
pub fn read_timeout_snapshot(
    artifact_dir: &Path,
    run_id: &str,
) -> Result<Option<TimeoutSnapshot>, PersistenceError> {
    let path = timeout_snapshot_path(artifact_dir, run_id);
    match read_json(&path) {
        Ok(snapshot) => Ok(Some(snapshot)),
        Err(PersistenceError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// Handle a timeout by persisting a frozen partial snapshot.
///
/// This is the core entry point called when the llxprt executor detects a
/// timeout or idle_timeout after worktree changes. It:
///
/// 1. Maps changed paths to charter subsystems (fail closed for unmapped).
/// 2. Sets `recovery_required = true` — broad continuation is blocked.
/// 3. Records whether a partial compile is configured.
/// 4. Persists the snapshot atomically.
///
/// The caller must subsequently return `StepOutcome::Wait` so the scope
/// barrier blocks until a scope decision resolves the recovery.
pub fn handle_timeout_recovery(
    artifact_dir: &Path,
    run_id: &str,
    charter: &CanonicalTaskCharter,
    measurement: &PatchMeasurement,
    timeout_kind: TimeoutKind,
    partial_compile_configured: bool,
    process_evidence: &ProcessEvidence,
) -> Result<TimeoutSnapshot, PersistenceError> {
    let mapped_changes = map_changes_to_subsystems(&measurement.changed_paths, charter);
    let snapshot = TimeoutSnapshot {
        run_id: run_id.to_string(),
        charter_id: charter.charter_id.clone(),
        charter_digest: charter.digest.clone(),
        timeout_kind,
        measurement: measurement.clone(),
        mapped_changes,
        partial_compile_configured,
        partial_compile_passed: None,
        process_evidence: process_evidence.clone(),
        recovery_required: true,
        created_at: chrono::Utc::now(),
    };
    let path = timeout_snapshot_path(artifact_dir, run_id);
    write_updatable_json(&path, &snapshot)?;
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::executors::scope_control::measurement::{ChangeStatus, FileChange};
    use crate::engine::executors::scope_control::model::{
        normalize_charter, DraftBudget, DraftReviewCaps, DraftSubsystem, TaskCharterDraft,
    };
    use tempfile::TempDir;

    fn sample_charter() -> CanonicalTaskCharter {
        let draft = TaskCharterDraft {
            charter_id: "T".into(),
            issue_number: 1,
            run_id: "r".into(),
            merge_base: "abc".into(),
            acceptance_criteria: vec!["AC-1".into()],
            non_goals: vec!["NG".into()],
            subsystems: vec![DraftSubsystem {
                id: "core".into(),
                paths: vec!["src/core".into()],
            }],
            budget: DraftBudget {
                max_files_changed: 10,
                max_added_lines: 500,
                max_new_modules: 3,
                max_dependencies_added: 0,
                max_public_apis_added: 5,
            },
            review_caps: DraftReviewCaps {
                initial_full_reviews: 1,
                max_delta_reviews: 2,
                final_acceptance_reviews: 1,
                max_mutating_remediation_rounds: 2,
            },
            mandatory_gates: vec!["cargo test".into()],
        };
        normalize_charter(&draft)
    }

    fn sample_measurement() -> PatchMeasurement {
        PatchMeasurement {
            merge_base: "abc".into(),
            head_sha: "def".into(),
            divergence: 1,
            files_changed: 2,
            added_lines: 50,
            binary_files: 0,
            new_modules: 1,
            dependencies_added: 0,
            content_digest: String::new(),
            public_apis_added: 1,
            changed_paths: vec!["src/core/a.rs".into(), "src/other/b.rs".into()],
            changed_subsystems: vec!["core".into()],
            file_details: vec![FileChange {
                path: "src/core/a.rs".into(),
                status: ChangeStatus::Modified,
                added_lines: Some(50),
                deleted_lines: Some(0),
                is_binary: false,
            }],
        }
    }

    #[test]
    fn map_changes_maps_to_subsystem() {
        let charter = sample_charter();
        let mapped =
            map_changes_to_subsystems(&["src/core/a.rs".into(), "src/other/b.rs".into()], &charter);
        assert_eq!(mapped.len(), 2);
        assert_eq!(mapped[0].subsystem.as_deref(), Some("core"));
        assert_eq!(mapped[1].subsystem, None); // unmapped
    }

    #[test]
    fn handle_timeout_persists_snapshot() {
        let tmp = TempDir::new().unwrap();
        let charter = sample_charter();
        let measurement = sample_measurement();
        let snapshot = handle_timeout_recovery(
            tmp.path(),
            "r",
            &charter,
            &measurement,
            TimeoutKind::IdleTimeout,
            true,
            &ProcessEvidence::default(),
        )
        .unwrap();

        assert!(snapshot.recovery_required);
        assert_eq!(snapshot.timeout_kind, TimeoutKind::IdleTimeout);
        assert!(snapshot.partial_compile_configured);
        assert_eq!(snapshot.mapped_changes.len(), 2);
    }

    #[test]
    fn read_returns_none_when_no_snapshot() {
        let tmp = TempDir::new().unwrap();
        let result = read_timeout_snapshot(tmp.path(), "r").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn read_returns_persisted_snapshot() {
        let tmp = TempDir::new().unwrap();
        let charter = sample_charter();
        let measurement = sample_measurement();
        handle_timeout_recovery(
            tmp.path(),
            "r",
            &charter,
            &measurement,
            TimeoutKind::Timeout,
            false,
            &ProcessEvidence::default(),
        )
        .unwrap();

        let read = read_timeout_snapshot(tmp.path(), "r").unwrap().unwrap();
        assert_eq!(read.run_id, "r");
        assert!(read.recovery_required);
        assert!(!read.partial_compile_configured);
    }

    #[test]
    fn status_projection_counts_unmapped() {
        let snapshot = TimeoutSnapshot {
            run_id: "r".into(),
            charter_id: "T".into(),
            charter_digest: "d".into(),
            timeout_kind: TimeoutKind::Timeout,
            measurement: sample_measurement(),
            mapped_changes: vec![
                MappedChange {
                    path: "src/core/a.rs".into(),
                    subsystem: Some("core".into()),
                },
                MappedChange {
                    path: "src/other/b.rs".into(),
                    subsystem: None,
                },
            ],
            partial_compile_configured: true,
            partial_compile_passed: Some(false),
            process_evidence: ProcessEvidence::default(),
            recovery_required: true,
            created_at: chrono::Utc::now(),
        };
        let status = TimeoutRecoveryStatus::from_snapshot(&snapshot);
        assert!(status.recovery_required);
        assert_eq!(status.unmapped_path_count, 1);
        assert_eq!(status.mapped_changes_count, 2);
        assert!(status.partial_compile_configured);
        assert_eq!(status.partial_compile_passed, Some(false));
    }

    #[test]
    fn recovery_blocks_until_resolved() {
        let tmp = TempDir::new().unwrap();
        let charter = sample_charter();
        let measurement = sample_measurement();
        handle_timeout_recovery(
            tmp.path(),
            "r",
            &charter,
            &measurement,
            TimeoutKind::IdleTimeout,
            true,
            &ProcessEvidence::default(),
        )
        .unwrap();

        // A snapshot exists and recovery is required.
        let snapshot = read_timeout_snapshot(tmp.path(), "r").unwrap().unwrap();
        assert!(snapshot.recovery_required);
    }
}
