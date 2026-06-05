//! Git repository adapter for workspace and branch management.
//!
//! Provides functions for repository configuration, workspace resolution,
//! branch preparation, and push operations using git commands.
///
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P10
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;

/// Repository configuration for workspace and branch management.
#[derive(Debug, Clone)]
pub struct RepositoryConfig {
    /// Source repository URL or path
    pub source: String,
    /// Workspace path strategy (shared, per_run, temp)
    pub workspace: String,
    /// Base branch for creating new branches
    pub base_branch: Option<String>,
    /// Branch name template (e.g., "luther-fix-{run_id}")
    pub branch_template: String,
}

impl RepositoryConfig {
    /// Create a new repository configuration with default values.
    #[must_use]
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            workspace: "shared".to_string(),
            base_branch: Some("main".to_string()),
            branch_template: "luther-{run_id}".to_string(),
        }
    }

    /// Set the workspace strategy.
    #[must_use]
    pub fn with_workspace(mut self, workspace: impl Into<String>) -> Self {
        self.workspace = workspace.into();
        self
    }

    /// Set the base branch.
    #[must_use]
    pub fn with_base_branch(mut self, branch: impl Into<String>) -> Self {
        self.base_branch = Some(branch.into());
        self
    }

    /// Set the branch template.
    #[must_use]
    pub fn with_branch_template(mut self, template: impl Into<String>) -> Self {
        self.branch_template = template.into();
        self
    }
}

/// Error type for repository preparation operations.
#[derive(Debug, Error)]
pub enum RepoPrepError {
    #[error("Git operation failed: {message}")]
    GitError {
        message: String,
        exit_code: Option<i32>,
    },
    #[error("Invalid repository path: {path}")]
    InvalidPath { path: String },
    #[error("Branch operation failed: {message}")]
    BranchError { message: String },
    #[error("Push failed: {message}")]
    PushError { message: String },
    #[error("Workspace error: {message}")]
    WorkspaceError { message: String },
}

impl RepoPrepError {
    /// Get structured diagnostics for this error.
    pub fn get_diagnostics(&self) -> HashMap<String, String> {
        let mut diag = HashMap::new();
        diag.insert("error_type".to_string(), "RepoPrepError".to_string());
        diag.insert("message".to_string(), self.to_string());
        diag.insert("timestamp".to_string(), chrono::Utc::now().to_rfc3339());

        match self {
            RepoPrepError::GitError { exit_code, .. } => {
                if let Some(code) = exit_code {
                    diag.insert("exit_code".to_string(), code.to_string());
                }
            }
            RepoPrepError::InvalidPath { path } => {
                diag.insert("path".to_string(), path.clone());
            }
            _ => {}
        }

        diag
    }
}

/// Resolve the workspace path for a given configuration and run ID.
///
/// # Arguments
/// * `config` - The repository configuration
/// * `run_id` - The unique identifier for the run
///
/// # Returns
/// PathBuf to the resolved workspace directory
pub fn resolve_workspace_path(config: &RepositoryConfig, run_id: &str) -> PathBuf {
    let base = Path::new(&config.workspace);

    // If workspace contains the run_id template token, substitute it
    let workspace_str = config.workspace.replace("{run_id}", run_id);
    let path = Path::new(&workspace_str);

    // If absolute path, use it directly
    if path.is_absolute() {
        return path.to_path_buf();
    }

    // If the original workspace path has a run-specific component,
    // create a per-run subdirectory
    if config.workspace.contains("{run_id}") || config.workspace == "per_run" {
        let mut result = base.to_path_buf();
        if config.workspace == "per_run" {
            result.push("workspaces");
        }
        result.push(run_id);
        result
    } else {
        // Shared workspace - use as-is
        path.to_path_buf()
    }
}

/// Prepare a branch for the run by checking out existing or creating from base.
///
/// # Arguments
/// * `workspace` - Path to the git workspace
/// * `base` - Base branch name to create from if target doesn't exist
/// * `name_template` - Template for branch name (supports {run_id} substitution)
/// * `run_id` - The run identifier to substitute in the template
///
/// # Returns
/// Result containing the branch name that was checked out/created
pub fn prepare_branch(
    workspace: &Path,
    base: &str,
    name_template: &str,
    run_id: &str,
) -> Result<String, RepoPrepError> {
    // Validate workspace exists
    if !workspace.exists() {
        return Err(RepoPrepError::InvalidPath {
            path: workspace.to_string_lossy().to_string(),
        });
    }

    // Generate branch name from template
    let branch_name = name_template.replace("{run_id}", run_id);

    // Check if branch exists
    let output = Command::new("git")
        .args(["branch", "--list", "--all", &branch_name])
        .current_dir(workspace)
        .output()
        .map_err(|e| RepoPrepError::GitError {
            message: format!("Failed to list branches: {}", e),
            exit_code: None,
        })?;

    let branch_list = String::from_utf8_lossy(&output.stdout);
    let branch_exists = branch_list.trim().contains(&branch_name);

    if branch_exists {
        // Checkout existing branch
        let checkout = Command::new("git")
            .args(["checkout", &branch_name])
            .current_dir(workspace)
            .output()
            .map_err(|e| RepoPrepError::GitError {
                message: format!("Failed to checkout branch: {}", e),
                exit_code: None,
            })?;

        if !checkout.status.success() {
            let stderr = String::from_utf8_lossy(&checkout.stderr);
            return Err(RepoPrepError::GitError {
                message: format!("Checkout failed: {}", stderr),
                exit_code: checkout.status.code(),
            });
        }
    } else {
        // Create new branch from base
        // First checkout base
        let checkout_base = Command::new("git")
            .args(["checkout", base])
            .current_dir(workspace)
            .output()
            .map_err(|e| RepoPrepError::GitError {
                message: format!("Failed to checkout base branch: {}", e),
                exit_code: None,
            })?;

        if !checkout_base.status.success() {
            let stderr = String::from_utf8_lossy(&checkout_base.stderr);
            return Err(RepoPrepError::GitError {
                message: format!("Checkout base failed: {}", stderr),
                exit_code: checkout_base.status.code(),
            });
        }

        // Create new branch
        let create = Command::new("git")
            .args(["checkout", "-b", &branch_name])
            .current_dir(workspace)
            .output()
            .map_err(|e| RepoPrepError::GitError {
                message: format!("Failed to create branch: {}", e),
                exit_code: None,
            })?;

        if !create.status.success() {
            let stderr = String::from_utf8_lossy(&create.stderr);
            return Err(RepoPrepError::BranchError {
                message: format!("Failed to create branch: {}", stderr),
            });
        }
    }

    Ok(branch_name)
}

/// Push the current branch to a remote.
///
/// # Arguments
/// * `workspace` - Path to the git workspace
/// * `remote` - Remote name (e.g., "origin")
/// * `branch` - Branch name to push
///
/// # Returns
/// Result indicating success or failure
pub fn push_branch(workspace: &Path, remote: &str, branch: &str) -> Result<(), RepoPrepError> {
    // Validate workspace exists
    if !workspace.exists() {
        return Err(RepoPrepError::InvalidPath {
            path: workspace.to_string_lossy().to_string(),
        });
    }

    // Push branch
    let output = Command::new("git")
        .args(["push", "-u", remote, branch])
        .current_dir(workspace)
        .output()
        .map_err(|e| RepoPrepError::GitError {
            message: format!("Failed to execute push: {}", e),
            exit_code: None,
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(RepoPrepError::PushError {
            message: format!("Push failed: {}", stderr),
        });
    }

    Ok(())
}

/// Clone a repository to the specified path.
///
/// # Arguments
/// * `source` - Repository URL or path to clone from
/// * `destination` - Path where the repository should be cloned
///
/// # Returns
/// Result indicating success or failure
pub fn clone_repository(source: &str, destination: &Path) -> Result<(), RepoPrepError> {
    let output = Command::new("git")
        .args(["clone", source, &destination.to_string_lossy()])
        .output()
        .map_err(|e| RepoPrepError::GitError {
            message: format!("Failed to execute clone: {}", e),
            exit_code: None,
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(RepoPrepError::GitError {
            message: format!("Clone failed: {}", stderr),
            exit_code: output.status.code(),
        });
    }

    Ok(())
}

/// Initialize a new git repository at the specified path.
///
/// # Arguments
/// * `path` - Path where the repository should be initialized
///
/// # Returns
/// Result indicating success or failure
pub fn init_repository(path: &Path) -> Result<(), RepoPrepError> {
    std::fs::create_dir_all(path).map_err(|e| RepoPrepError::WorkspaceError {
        message: format!("Failed to create directory: {}", e),
    })?;

    let output = Command::new("git")
        .args(["init"])
        .current_dir(path)
        .output()
        .map_err(|e| RepoPrepError::GitError {
            message: format!("Failed to execute init: {}", e),
            exit_code: None,
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(RepoPrepError::GitError {
            message: format!("Init failed: {}", stderr),
            exit_code: output.status.code(),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn resolve_workspace_path_shared_strategy() {
        let config = RepositoryConfig {
            source: "/tmp/repo".to_string(),
            workspace: "/shared/workspace".to_string(),
            base_branch: Some("main".to_string()),
            branch_template: "fix-{run_id}".to_string(),
        };

        let path = resolve_workspace_path(&config, "run-001");
        assert_eq!(path, PathBuf::from("/shared/workspace"));
    }

    #[test]
    fn resolve_workspace_path_per_run_strategy() {
        let config = RepositoryConfig {
            source: "/tmp/repo".to_string(),
            workspace: "per_run".to_string(),
            base_branch: Some("main".to_string()),
            branch_template: "fix-{run_id}".to_string(),
        };

        let path = resolve_workspace_path(&config, "run-001");
        assert!(path.to_string_lossy().contains("workspaces"));
        assert!(path.to_string_lossy().contains("run-001"));
    }

    #[test]
    fn resolve_workspace_path_with_template() {
        let config = RepositoryConfig {
            source: "/tmp/repo".to_string(),
            workspace: "/workspaces/{run_id}".to_string(),
            base_branch: Some("main".to_string()),
            branch_template: "fix-{run_id}".to_string(),
        };

        let path = resolve_workspace_path(&config, "run-001");
        assert_eq!(path, PathBuf::from("/workspaces/run-001"));
    }
}
