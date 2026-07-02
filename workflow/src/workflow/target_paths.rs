use std::path::{Path, PathBuf};

use crate::workflow::schema::{DiffPathNormalization, RepoConfig};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetPathConfig {
    pub project_subdir: Option<PathBuf>,
    pub artifact_path_base: Option<PathBuf>,
    pub diff_path_base: Option<PathBuf>,
    pub diff_path_normalization: DiffPathNormalization,
}

impl Default for TargetPathConfig {
    fn default() -> Self {
        Self {
            project_subdir: None,
            artifact_path_base: None,
            diff_path_base: None,
            diff_path_normalization: DiffPathNormalization::RepoRelative,
        }
    }
}

impl TargetPathConfig {
    pub fn from_repo_config(repository: &RepoConfig) -> Self {
        Self {
            project_subdir: repository.project_subdir.clone().map(PathBuf::from),
            artifact_path_base: repository.artifact_path_base.clone().map(PathBuf::from),
            diff_path_base: repository.diff_path_base.clone().map(PathBuf::from),
            diff_path_normalization: repository.diff_path_normalization.clone(),
        }
    }

    pub fn project_dir(&self, repo_root: &Path) -> PathBuf {
        join_optional(repo_root, self.project_subdir.as_deref())
    }

    pub fn artifact_base_dir(&self, repo_root: &Path) -> PathBuf {
        join_optional(repo_root, self.artifact_path_base.as_deref())
    }

    pub fn diff_base_dir(&self, repo_root: &Path) -> PathBuf {
        join_optional(repo_root, self.diff_path_base.as_deref())
    }

    pub fn normalize_diff_path(&self, repo_relative_path: &str) -> Option<String> {
        match self.diff_path_normalization {
            DiffPathNormalization::RepoRelative => Some(repo_relative_path.to_string()),
            DiffPathNormalization::BaseRelative => self.strip_diff_base(repo_relative_path),
        }
    }

    fn strip_diff_base(&self, repo_relative_path: &str) -> Option<String> {
        let base = self.diff_path_base.as_ref()?;
        let path = Path::new(repo_relative_path);
        path.strip_prefix(base)
            .ok()
            .map(|stripped| stripped.to_string_lossy().into_owned())
    }
}

fn join_optional(root: &Path, relative: Option<&Path>) -> PathBuf {
    match relative {
        Some(relative) if !relative.as_os_str().is_empty() => root.join(relative),
        _ => root.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_config(
        project_subdir: Option<&str>,
        diff_path_base: Option<&str>,
        normalization: DiffPathNormalization,
    ) -> RepoConfig {
        RepoConfig {
            workspace_strategy: "temp".to_string(),
            branch_template: "issue{issue_number}".to_string(),
            base_branch: Some("main".to_string()),
            workspace_root: None,
            project_subdir: project_subdir.map(ToString::to_string),
            artifact_path_base: None,
            diff_path_base: diff_path_base.map(ToString::to_string),
            diff_path_normalization: normalization,
        }
    }

    #[test]
    fn project_dir_defaults_to_repo_root_or_project_subdir() {
        let root = Path::new("/repo");
        let default_paths = TargetPathConfig::from_repo_config(&repo_config(
            None,
            None,
            DiffPathNormalization::RepoRelative,
        ));
        assert_eq!(default_paths.project_dir(root), PathBuf::from("/repo"));

        let nested_paths = TargetPathConfig::from_repo_config(&repo_config(
            Some("workflow"),
            None,
            DiffPathNormalization::RepoRelative,
        ));
        assert_eq!(
            nested_paths.project_dir(root),
            PathBuf::from("/repo/workflow")
        );
    }

    #[test]
    fn diff_path_normalization_can_strip_configured_base() {
        let paths = TargetPathConfig::from_repo_config(&repo_config(
            Some("workflow"),
            Some("workflow"),
            DiffPathNormalization::BaseRelative,
        ));
        assert_eq!(
            paths.normalize_diff_path("workflow/src/lib.rs").as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(paths.normalize_diff_path("README.md"), None);
    }
}
