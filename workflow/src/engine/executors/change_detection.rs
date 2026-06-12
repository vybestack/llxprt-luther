//! Changed-path detection seam for the llxprt executor.
//!
//! Detecting llxprt success by polling `git status` is pragmatic but brittle:
//! it assumes a git worktree, git availability, and correct porcelain parsing.
//! This module puts that detection behind the [`ChangedPathDetector`] trait so
//! callers can:
//!
//! - choose tracked-only vs untracked-included detection
//!   ([`ChangeDetectionMode`]),
//! - distinguish "no changes" (`Ok(vec![])`) from "git missing / not a repo"
//!   (`Err`) instead of silently collapsing both into `None`, and
//! - unit-test the pure porcelain-parsing helpers in isolation.
//!
//! The production implementor [`GitChangedPathDetector`] shells out to `git`
//! and maps each failure mode to an explicit
//! [`EngineError::StepExecutionError`], mirroring the dependency-injection idiom
//! used by `GithubPrCommandRunner`/`SystemGithubPrCommandRunner` and
//! `ClockSleeper`/`SystemClockSleeper` elsewhere in this crate.

use std::io::ErrorKind;
use std::path::Path;
use std::process::Command;

use crate::engine::runner::EngineError;

/// Which changes a [`ChangedPathDetector`] should report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChangeDetectionMode {
    /// Only tracked-file changes (`git status --untracked-files=no`).
    TrackedOnly,
    /// Tracked changes plus untracked files (`--untracked-files=all`).
    /// This is the default to preserve historical behavior.
    #[default]
    IncludeUntracked,
}

impl ChangeDetectionMode {
    /// Parse the optional `change_detection_mode` step parameter.
    ///
    /// `"tracked_only"` maps to [`ChangeDetectionMode::TrackedOnly`]; anything
    /// else (including absent or unknown values) maps to the default
    /// [`ChangeDetectionMode::IncludeUntracked`] so existing workflows keep
    /// their `--untracked-files=all` behavior.
    #[must_use]
    pub fn from_param(value: Option<&str>) -> Self {
        match value {
            Some("tracked_only") => Self::TrackedOnly,
            _ => Self::IncludeUntracked,
        }
    }

    /// The `--untracked-files=...` argument value for this mode.
    #[must_use]
    pub fn untracked_files_arg(self) -> &'static str {
        match self {
            Self::TrackedOnly => "--untracked-files=no",
            Self::IncludeUntracked => "--untracked-files=all",
        }
    }
}

/// Detects the set of changed paths in a working directory.
///
/// Returning `Result` (not `Option`) is deliberate: callers must be able to
/// distinguish a clean tree (`Ok(vec![])`) from an environment error such as a
/// missing `git` binary or a non-repository working directory (`Err`).
pub trait ChangedPathDetector: Send + Sync {
    /// Detect changed paths under `work_dir` using `mode`.
    fn detect_changed_paths(
        &self,
        work_dir: &Path,
        mode: ChangeDetectionMode,
    ) -> Result<Vec<String>, EngineError>;
}

/// Production [`ChangedPathDetector`] backed by `git status --porcelain`.
#[derive(Debug, Clone, Copy, Default)]
pub struct GitChangedPathDetector;

impl ChangedPathDetector for GitChangedPathDetector {
    fn detect_changed_paths(
        &self,
        work_dir: &Path,
        mode: ChangeDetectionMode,
    ) -> Result<Vec<String>, EngineError> {
        let output = Command::new("git")
            .args(["status", "--porcelain", mode.untracked_files_arg()])
            .current_dir(work_dir)
            .output()
            .map_err(|err| {
                if err.kind() == ErrorKind::NotFound {
                    change_detection_error(
                        "git binary not found on PATH; cannot detect changed paths",
                    )
                } else {
                    change_detection_error(format!("failed to spawn git status: {err}"))
                }
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if output.status.code() == Some(128) {
                return Err(change_detection_error(format!(
                    "git status failed (exit 128): work_dir is not a git repository / has no worktree: {}",
                    stderr.trim()
                )));
            }
            return Err(change_detection_error(format!(
                "git status failed (exit {:?}): {}",
                output.status.code(),
                stderr.trim()
            )));
        }

        let paths = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(parse_status_path)
            .collect::<Vec<_>>();
        Ok(paths)
    }
}

/// Build the explicit error surfaced for change-detection failures.
fn change_detection_error(message: impl Into<String>) -> EngineError {
    EngineError::StepExecutionError {
        step_id: "llxprt".to_string(),
        message: message.into(),
    }
}

/// Parse a single `git status --porcelain` line into its changed path.
///
/// The two status columns plus a space occupy the first three bytes; the rest
/// is the path. Rename/copy entries are formatted `old -> new`, in which case
/// the destination path is returned.
pub(crate) fn parse_status_path(line: &str) -> Option<String> {
    let path = line.get(3..)?.trim();
    if path.is_empty() {
        return None;
    }
    let path = path.split(" -> ").last().unwrap_or(path);
    Some(path.to_string())
}

/// Filter out paths that were already present in the initial snapshot.
pub(crate) fn new_changed_paths(paths: &[String], initial_changed_paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter(|path| !initial_changed_paths.contains(path))
        .cloned()
        .collect()
}

/// Decide whether the changed `paths` satisfy the required exact paths and
/// substring patterns. An empty `paths` slice never satisfies the requirements.
pub(crate) fn diff_requirements_met(
    paths: &[String],
    required_changed_paths: &[String],
    required_changed_path_patterns: &[String],
) -> bool {
    if paths.is_empty() {
        return false;
    }

    required_changed_paths
        .iter()
        .all(|required| paths.iter().any(|path| path == required))
        && required_changed_path_patterns
            .iter()
            .all(|pattern| paths.iter().any(|path| path.contains(pattern)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_status_path_handles_basic_statuses() {
        assert_eq!(
            parse_status_path(" M src/a.rs").as_deref(),
            Some("src/a.rs")
        );
        assert_eq!(parse_status_path("A  b.rs").as_deref(), Some("b.rs"));
        assert_eq!(parse_status_path(" D c.rs").as_deref(), Some("c.rs"));
        assert_eq!(parse_status_path("?? u.txt").as_deref(), Some("u.txt"));
    }

    #[test]
    fn parse_status_path_extracts_rename_and_copy_destination() {
        assert_eq!(parse_status_path("R  old -> new").as_deref(), Some("new"));
        assert_eq!(parse_status_path("C  s -> d").as_deref(), Some("d"));
    }

    #[test]
    fn parse_status_path_trims_and_rejects_empty() {
        assert_eq!(
            parse_status_path(" M   spaced.rs  ").as_deref(),
            Some("spaced.rs")
        );
        assert_eq!(parse_status_path(""), None);
        assert_eq!(parse_status_path("ab"), None);
        assert_eq!(parse_status_path("   "), None);
    }

    #[test]
    fn parse_status_path_preserves_unicode_paths() {
        assert_eq!(
            parse_status_path(" M src/café/naïve.rs").as_deref(),
            Some("src/café/naïve.rs")
        );
    }

    #[test]
    fn new_changed_paths_filters_initial_snapshot() {
        let paths = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let initial = vec!["b".to_string()];
        assert_eq!(
            new_changed_paths(&paths, &initial),
            vec!["a".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn new_changed_paths_empty_initial_is_passthrough() {
        let paths = vec!["a".to_string(), "b".to_string()];
        assert_eq!(new_changed_paths(&paths, &[]), paths);
    }

    #[test]
    fn diff_requirements_met_empty_paths_is_false() {
        assert!(!diff_requirements_met(&[], &["a".to_string()], &[]));
    }

    #[test]
    fn diff_requirements_met_exact_required_paths() {
        let paths = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];
        assert!(diff_requirements_met(
            &paths,
            &["src/a.rs".to_string(), "src/b.rs".to_string()],
            &[]
        ));
        assert!(!diff_requirements_met(
            &paths,
            &["src/missing.rs".to_string()],
            &[]
        ));
    }

    #[test]
    fn diff_requirements_met_substring_patterns() {
        let paths = vec!["src/engine/executors/llxprt.rs".to_string()];
        assert!(diff_requirements_met(
            &paths,
            &[],
            &["executors/".to_string()]
        ));
        assert!(!diff_requirements_met(
            &paths,
            &[],
            &["adapters/".to_string()]
        ));
    }

    #[test]
    fn diff_requirements_met_mixed_partial_is_false() {
        let paths = vec!["src/a.rs".to_string()];
        assert!(!diff_requirements_met(
            &paths,
            &["src/a.rs".to_string()],
            &["nonexistent".to_string()]
        ));
    }

    #[test]
    fn diff_requirements_met_is_case_sensitive() {
        let paths = vec!["src/A.rs".to_string()];
        assert!(!diff_requirements_met(
            &paths,
            &["src/a.rs".to_string()],
            &[]
        ));
    }

    #[test]
    fn mode_from_param_maps_values() {
        assert_eq!(
            ChangeDetectionMode::from_param(Some("tracked_only")),
            ChangeDetectionMode::TrackedOnly
        );
        assert_eq!(
            ChangeDetectionMode::from_param(Some("include_untracked")),
            ChangeDetectionMode::IncludeUntracked
        );
        assert_eq!(
            ChangeDetectionMode::from_param(None),
            ChangeDetectionMode::IncludeUntracked
        );
        assert_eq!(
            ChangeDetectionMode::from_param(Some("bogus")),
            ChangeDetectionMode::IncludeUntracked
        );
        assert_eq!(
            ChangeDetectionMode::default(),
            ChangeDetectionMode::IncludeUntracked
        );
    }

    #[test]
    fn mode_untracked_files_arg() {
        assert_eq!(
            ChangeDetectionMode::TrackedOnly.untracked_files_arg(),
            "--untracked-files=no"
        );
        assert_eq!(
            ChangeDetectionMode::IncludeUntracked.untracked_files_arg(),
            "--untracked-files=all"
        );
    }
}
