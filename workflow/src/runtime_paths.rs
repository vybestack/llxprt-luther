//! Runtime path utilities for Luther workflow system.
//!
//! Provides standardized path locations for data, config, and runtime files
//! using the `directories` crate for cross-platform support.
///
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P10
use directories::ProjectDirs;
use std::path::PathBuf;

/// Get the project directories handle.
/// Returns None if home directory cannot be determined.
fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("com", "luther", "luther-workflow")
}

/// Get the application data directory.
/// Used for persistent runtime data like heartbeats, state files.
///
/// # Returns
/// PathBuf to the data directory (e.g., ~/Library/Application Support/luther-workflow on macOS)
pub fn get_data_dir() -> PathBuf {
    project_dirs()
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".luther-workflow"))
}

/// Get the application configuration directory.
/// Used for user-editable configuration files.
///
/// # Returns
/// PathBuf to the config directory (e.g., ~/Library/Application Support/luther-workflow on macOS)
pub fn get_config_dir() -> PathBuf {
    project_dirs()
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".luther-workflow"))
}

/// Get the runtime directory for a specific run ID.
/// Used for run-specific temporary files and state.
///
/// # Arguments
/// * `run_id` - The unique identifier for the run
///
/// # Returns
/// PathBuf to the run-specific directory
pub fn get_run_dir(run_id: &str) -> PathBuf {
    let mut path = project_dirs()
        .map(|d| {
            d.runtime_dir()
                .map(|r| r.to_path_buf())
                .unwrap_or_else(|| d.data_dir().join("run"))
        })
        .unwrap_or_else(|| PathBuf::from(".luther-workflow/run"));
    path.push(run_id);
    path
}

/// Get the root directory for workflow artifacts.
/// Used for storing workflow outputs, logs, and generated files.
///
/// # Returns
/// PathBuf to the artifacts root directory
pub fn get_artifacts_root() -> PathBuf {
    project_dirs()
        .map(|d| d.data_dir().join("artifacts"))
        .unwrap_or_else(|| PathBuf::from(".luther-workflow/artifacts"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_data_dir_returns_valid_path() {
        let path = get_data_dir();
        assert!(!path.as_os_str().is_empty());
        assert!(
            path.to_string_lossy().contains("luther") || path.to_string_lossy().contains(".luther")
        );
    }

    #[test]
    fn get_config_dir_returns_valid_path() {
        let path = get_config_dir();
        assert!(!path.as_os_str().is_empty());
        assert!(
            path.to_string_lossy().contains("luther") || path.to_string_lossy().contains(".luther")
        );
    }

    #[test]
    fn get_run_dir_includes_run_id() {
        let path = get_run_dir("test-run-123");
        let path_str = path.to_string_lossy();
        assert!(path_str.contains("test-run-123"));
    }

    #[test]
    fn get_artifacts_root_returns_valid_path() {
        let path = get_artifacts_root();
        assert!(!path.as_os_str().is_empty());
        assert!(
            path.to_string_lossy().contains("artifacts")
                || path.to_string_lossy().contains(".luther")
        );
    }
}
