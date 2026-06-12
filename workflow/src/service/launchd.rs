//! Launchd service installation for macOS.
//!
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P10
use std::path::PathBuf;
use std::process::Command;
use thiserror::Error;

use crate::service::spec::{ensure_log_directories, generate_launchd_plist, ServiceSpec};

/// Error type for launchd service operations.
#[derive(Debug, Error)]
pub enum LaunchdError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Launchctl error: {message}")]
    LaunchctlError { message: String, exit_code: i32 },
    #[error("Service already installed: {0}")]
    AlreadyInstalled(String),
    #[error("Service not found: {0}")]
    NotFound(String),
}

impl LaunchdError {
    /// Platform-specific remediation guidance for launchd failures.
    ///
    /// Mirrors the diagnostics style used elsewhere (e.g. GithubError) and
    /// satisfies REQ-EARS-SVC-004 by surfacing the log location plus the
    /// launchctl/plutil commands an operator needs.
    ///
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P10
    pub fn get_remediation_steps(&self) -> Vec<String> {
        let log_dir = crate::runtime_paths::get_log_dir();
        vec![
            format!("Inspect service logs under: {}", log_dir.display()),
            "Validate the plist with `plutil -lint ~/Library/LaunchAgents/com.luther.*.plist`."
                .to_string(),
            "List loaded agents with `launchctl list | grep luther`.".to_string(),
            "Confirm ~/Library/LaunchAgents is writable for the current user.".to_string(),
        ]
    }
}

/// Get the default LaunchAgents directory path.
pub fn get_launch_agents_dir() -> PathBuf {
    dirs::home_dir()
        .map(|home| home.join("Library").join("LaunchAgents"))
        .unwrap_or_else(|| PathBuf::from("~/Library/LaunchAgents"))
}

/// Get the full path to the plist file for a service.
pub fn get_plist_path(spec: &ServiceSpec) -> PathBuf {
    get_launch_agents_dir().join(spec.plist_file_name())
}

/// Write the launchd plist for a service without loading or starting it.
///
/// This performs only the filesystem side of installation: it ensures the
/// `~/Library/LaunchAgents` directory exists and writes the generated plist.
/// It intentionally does **not** call `launchctl load`, so the daemon is not
/// started as a side effect of installation. The daemon is started later by
/// [`start_launchd_service`], keeping the `install` and `start` lifecycle steps
/// cleanly separated and consistent with the systemd backend.
///
/// # Arguments
/// * `spec` - The service specification
///
/// # Returns
/// Result containing the path to the written plist file
///
/// # Errors
/// Returns LaunchdError if the directory cannot be created or the plist cannot
/// be written.
pub fn write_launchd_plist(spec: &ServiceSpec) -> Result<PathBuf, LaunchdError> {
    let plist_path = get_plist_path(spec);
    let launch_agents_dir = get_launch_agents_dir();

    // Create LaunchAgents directory if it doesn't exist
    std::fs::create_dir_all(&launch_agents_dir)?;

    // Ensure the stdout/stderr log directories exist so the supervisor can
    // capture diagnostics on the very first start.
    ensure_log_directories(spec)?;

    // Generate and write plist (overwrites any existing plist).
    let plist_content = generate_launchd_plist(spec);
    std::fs::write(&plist_path, plist_content)?;

    Ok(plist_path)
}

/// Install a launchd service.
///
/// Installation is side-effect-free with respect to the running daemon: it only
/// writes the plist to `~/Library/LaunchAgents` and does not load or start the
/// service. Use [`start_launchd_service`] to load and start it. This mirrors the
/// systemd backend, which writes/enables the unit at install time without
/// starting it, so the cross-platform `install` contract is consistent.
///
/// # Arguments
/// * `spec` - The service specification
///
/// # Returns
/// Result containing the path to the installed plist file
///
/// # Errors
/// Returns LaunchdError if installation fails
pub fn install_launchd_service(spec: &ServiceSpec) -> Result<PathBuf, LaunchdError> {
    write_launchd_plist(spec)
}

/// Uninstall a launchd service.
///
/// # Arguments
/// * `spec` - The service specification
///
/// # Returns
/// Result indicating success or failure
pub fn uninstall_launchd_service(spec: &ServiceSpec) -> Result<(), LaunchdError> {
    let plist_path = get_plist_path(spec);

    if !plist_path.exists() {
        return Err(LaunchdError::NotFound(spec.label.clone()));
    }

    // Unload the service
    let output = Command::new("launchctl")
        .args(["unload", &plist_path.to_string_lossy()])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);

        return Err(LaunchdError::LaunchctlError {
            message: format!("Failed to unload service: {}", stderr),
            exit_code,
        });
    }

    // Remove the plist file
    std::fs::remove_file(&plist_path)?;

    Ok(())
}

/// Start a launchd service.
///
/// Because [`install_launchd_service`] no longer loads the plist, start is
/// responsible for loading the job (idempotently) before kicking it off. The
/// plist must already exist on disk (i.e. install must have run first); a
/// missing plist returns [`LaunchdError::NotFound`].
///
/// # Arguments
/// * `spec` - The service specification
///
/// # Returns
/// Result indicating success or failure
pub fn start_launchd_service(spec: &ServiceSpec) -> Result<(), LaunchdError> {
    let plist_path = get_plist_path(spec);
    if !plist_path.exists() {
        return Err(LaunchdError::NotFound(spec.label.clone()));
    }

    // Load the job first. `launchctl load` is idempotent enough for our needs:
    // if the job is already loaded it reports an error which we tolerate, since
    // the subsequent `start` will surface any genuine failure.
    let _ = Command::new("launchctl")
        .args(["load", &plist_path.to_string_lossy()])
        .output()?;

    let output = Command::new("launchctl")
        .args(["start", &spec.label])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);

        return Err(LaunchdError::LaunchctlError {
            message: format!("Failed to start service: {}", stderr),
            exit_code,
        });
    }

    Ok(())
}

/// Stop a launchd service.
///
/// # Arguments
/// * `spec` - The service specification
///
/// # Returns
/// Result indicating success or failure
pub fn stop_launchd_service(spec: &ServiceSpec) -> Result<(), LaunchdError> {
    let output = Command::new("launchctl")
        .args(["stop", &spec.label])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);

        return Err(LaunchdError::LaunchctlError {
            message: format!("Failed to stop service: {}", stderr),
            exit_code,
        });
    }

    Ok(())
}

/// Check if a launchd service is installed.
///
/// # Arguments
/// * `spec` - The service specification
///
/// # Returns
/// true if the service plist file exists
pub fn is_service_installed(spec: &ServiceSpec) -> bool {
    get_plist_path(spec).exists()
}

/// Get the status of a launchd service.
///
/// # Arguments
/// * `spec` - The service specification
///
/// # Returns
/// Result containing the service status as a string
pub fn get_service_status(spec: &ServiceSpec) -> Result<String, LaunchdError> {
    let output = Command::new("launchctl")
        .args(["list", &spec.label])
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if output.status.success() {
        Ok(stdout.to_string())
    } else {
        // Service not loaded or other error
        Err(LaunchdError::LaunchctlError {
            message: stderr.to_string(),
            exit_code: output.status.code().unwrap_or(-1),
        })
    }
}

/// Enable a launchd service (for automatic start on boot/user login).
///
/// # Arguments
/// * `spec` - The service specification
/// * `domain` - The domain (user, gui, or system)
///
/// # Returns
/// Result indicating success or failure
pub fn enable_launchd_service(spec: &ServiceSpec, domain: &str) -> Result<(), LaunchdError> {
    let output = Command::new("launchctl")
        .args(["enable", &format!("{}/{}", domain, spec.label)])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);

        return Err(LaunchdError::LaunchctlError {
            message: format!("Failed to enable service: {}", stderr),
            exit_code,
        });
    }

    Ok(())
}

/// Disable a launchd service.
///
/// # Arguments
/// * `spec` - The service specification
/// * `domain` - The domain (user, gui, or system)
///
/// # Returns
/// Result indicating success or failure
pub fn disable_launchd_service(spec: &ServiceSpec, domain: &str) -> Result<(), LaunchdError> {
    let output = Command::new("launchctl")
        .args(["disable", &format!("{}/{}", domain, spec.label)])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);

        return Err(LaunchdError::LaunchctlError {
            message: format!("Failed to disable service: {}", stderr),
            exit_code,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_launch_agents_dir() {
        let dir = get_launch_agents_dir();
        let dir_str = dir.to_string_lossy();
        assert!(dir_str.contains("LaunchAgents") || dir_str.contains("~"));
    }

    #[test]
    fn test_get_plist_path() {
        let spec = ServiceSpec::new("test", "/bin/test").with_label("com.luther.test");

        let path = get_plist_path(&spec);
        let path_str = path.to_string_lossy();

        assert!(path_str.contains("com.luther.test.plist"));
        assert!(path_str.contains("LaunchAgents"));
    }

    #[test]
    fn test_is_service_installed_check() {
        // This test would need a temp directory to fully test
        // For now, just verify the function returns false for non-existent paths
        let spec = ServiceSpec::new("nonexistent-test-service-xyz", "/bin/nonexistent")
            .with_label("com.luther.nonexistent-test-service-xyz");

        // Should be false since it doesn't exist
        assert!(!is_service_installed(&spec));
    }
}
