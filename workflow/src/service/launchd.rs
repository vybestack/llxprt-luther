//! Launchd service installation for macOS.
//!
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P10

use std::path::PathBuf;
use std::process::Command;
use thiserror::Error;

use crate::service::spec::{ServiceSpec, generate_launchd_plist};

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

/// Install a launchd service.
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
    let plist_path = get_plist_path(spec);
    let launch_agents_dir = get_launch_agents_dir();

    // Create LaunchAgents directory if it doesn't exist
    std::fs::create_dir_all(&launch_agents_dir)?;

    // Check if already installed (optional - can be forced)
    if plist_path.exists() {
        // Overwrite existing
    }

    // Generate and write plist
    let plist_content = generate_launchd_plist(spec);
    std::fs::write(&plist_path, plist_content)?;

    // Load the service using launchctl
    let output = Command::new("launchctl")
        .args(["load", &plist_path.to_string_lossy()])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);
        
        // Cleanup on failure
        let _ = std::fs::remove_file(&plist_path);
        
        return Err(LaunchdError::LaunchctlError {
            message: format!("Failed to load service: {}", stderr),
            exit_code,
        });
    }

    Ok(plist_path)
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
/// # Arguments
/// * `spec` - The service specification
///
/// # Returns
/// Result indicating success or failure
pub fn start_launchd_service(spec: &ServiceSpec) -> Result<(), LaunchdError> {
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
    use std::path::PathBuf;

    #[test]
    fn test_get_launch_agents_dir() {
        let dir = get_launch_agents_dir();
        let dir_str = dir.to_string_lossy();
        assert!(dir_str.contains("LaunchAgents") || dir_str.contains("~"));
    }

    #[test]
    fn test_get_plist_path() {
        let spec = ServiceSpec::new("test", "/bin/test")
            .with_label("com.luther.test");
        
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
