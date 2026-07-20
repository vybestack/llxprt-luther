//! Per-run path resolution for daemon-launched workflows.
//!
//! [`DaemonPathBases`] wraps configured base roots; each daemon-launched run
//! gets `base / issue-N / run-id` so concurrent runs cannot collide. Bases are
//! optional: a one-shot CLI run has no daemon bases.
//!
//! @plan:issue-117

use std::path::{Component, PathBuf};

/// Structured daemon base roots used to construct isolated per-run paths.
///
/// Configured `work_dir`/`artifact_dir` values from the resolved
/// workflow config variables are treated as base roots; each daemon-launched
/// run gets `base / issue-N / run-id` so concurrent runs for the same config
/// cannot collide. Bases are optional: a one-shot CLI run has no daemon bases,
/// so existing engine fallbacks continue to apply.
/// @plan:issue-117
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DaemonPathBases {
    pub work_dir_base: Option<PathBuf>,
    pub artifact_dir_base: Option<PathBuf>,
}

impl DaemonPathBases {
    /// Build the per-run work and artifact directories for an issue + run id.
    ///
    /// Returns `None` for a directory when its base is absent. The `run_id` must
    /// be a single relative path component before it is joined under the daemon
    /// base root.
    /// @plan:issue-117
    pub fn per_run_paths(&self, issue_number: u64, run_id: &str) -> Result<PerRunPaths, String> {
        validate_run_id_path_component(run_id)?;
        let issue_segment = format!("issue-{issue_number}");
        Ok(PerRunPaths {
            work_dir: self
                .work_dir_base
                .as_ref()
                .map(|base| base.join(&issue_segment).join(run_id)),
            artifact_dir: self
                .artifact_dir_base
                .as_ref()
                .map(|base| base.join(&issue_segment).join(run_id)),
        })
    }
}

/// Validate that `run_id` is a single safe path component (no separators,
/// no parent traversals, no Windows-style backslashes).
fn validate_run_id_path_component(run_id: &str) -> Result<(), String> {
    if run_id.is_empty() {
        return Err("run_id must not be empty".to_string());
    }
    let run_id_path = PathBuf::from(run_id);
    let mut components = run_id_path.components();
    if run_id.contains('\\') {
        return Err(format!(
            "run_id must be a single safe path component: {run_id}"
        ));
    }
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(()),
        _ => Err(format!(
            "run_id must be a single safe path component: {run_id}"
        )),
    }
}

/// Resolved per-run work/artifact directories for a single daemon launch.
/// @plan:issue-117
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PerRunPaths {
    pub work_dir: Option<PathBuf>,
    pub artifact_dir: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_run_paths_isolate_concurrent_issues() {
        let bases = DaemonPathBases {
            work_dir_base: Some(std::path::PathBuf::from(
                "/tmp/luther-workspaces/llxprt-luther",
            )),
            artifact_dir_base: Some(std::path::PathBuf::from(
                "/tmp/luther-artifacts/llxprt-luther",
            )),
        };
        let paths_109 = bases.per_run_paths(109, "run-aaa").unwrap();
        let paths_110 = bases.per_run_paths(110, "run-bbb").unwrap();
        assert_ne!(paths_109.work_dir, paths_110.work_dir);
        assert_ne!(paths_109.artifact_dir, paths_110.artifact_dir);
        assert_eq!(
            paths_109.work_dir.as_deref().unwrap().to_str().unwrap(),
            "/tmp/luther-workspaces/llxprt-luther/issue-109/run-aaa"
        );
        assert_eq!(
            paths_109.artifact_dir.as_deref().unwrap().to_str().unwrap(),
            "/tmp/luther-artifacts/llxprt-luther/issue-109/run-aaa"
        );
    }

    #[test]
    fn per_run_paths_rejects_unsafe_run_id_components() {
        let bases = DaemonPathBases {
            work_dir_base: Some(std::path::PathBuf::from("/tmp/work")),
            artifact_dir_base: Some(std::path::PathBuf::from("/tmp/artifacts")),
        };

        assert!(bases.per_run_paths(1, "../escape").is_err());
        assert!(bases.per_run_paths(1, "/tmp/escape").is_err());
    }

    #[test]
    fn per_run_paths_rejects_windows_style_separators() {
        let bases = DaemonPathBases {
            work_dir_base: Some(std::path::PathBuf::from("/tmp/work")),
            artifact_dir_base: Some(std::path::PathBuf::from("/tmp/artifacts")),
        };

        assert!(bases.per_run_paths(1, "foo\\..\\escape").is_err());
    }
}
