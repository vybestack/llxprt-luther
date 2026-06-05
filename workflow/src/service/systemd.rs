//! Systemd service installation for Linux.
//!
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P10
use std::path::PathBuf;
use std::process::Command;
use thiserror::Error;

use crate::service::spec::{generate_systemd_unit, ServiceSpec};

/// Error type for systemd service operations.
#[derive(Debug, Error)]
pub enum SystemdError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Systemctl error: {message}")]
    SystemctlError { message: String, exit_code: i32 },
    #[error("Service already installed: {0}")]
    AlreadyInstalled(String),
    #[error("Service not found: {0}")]
    NotFound(String),
}

/// Get the default systemd user unit directory path.
pub fn get_systemd_user_dir() -> PathBuf {
    dirs::home_dir()
        .map(|home| home.join(".config").join("systemd").join("user"))
        .unwrap_or_else(|| PathBuf::from("~/.config/systemd/user"))
}

/// Get the full path to the unit file for a service.
pub fn get_unit_path(spec: &ServiceSpec) -> PathBuf {
    get_systemd_user_dir().join(spec.unit_file_name())
}

/// Install a systemd service (user mode).
///
/// # Arguments
/// * `spec` - The service specification
///
/// # Returns
/// Result containing the path to the installed unit file
///
/// # Errors
/// Returns SystemdError if installation fails
pub fn install_systemd_service(spec: &ServiceSpec) -> Result<PathBuf, SystemdError> {
    let unit_path = get_unit_path(spec);
    let systemd_dir = get_systemd_user_dir();

    // Create systemd user directory if it doesn't exist
    std::fs::create_dir_all(&systemd_dir)?;

    // Check if already installed (optional - can be forced)
    if unit_path.exists() {
        // Overwrite existing
    }

    // Generate and write unit file
    let unit_content = generate_systemd_unit(spec);
    std::fs::write(&unit_path, unit_content)?;

    // Reload systemd daemon
    let output = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);

        // Cleanup on failure
        let _ = std::fs::remove_file(&unit_path);

        return Err(SystemdError::SystemctlError {
            message: format!("Failed to reload systemd: {}", stderr),
            exit_code,
        });
    }

    // Enable the service if it should run at load
    if spec.run_at_load {
        let output = Command::new("systemctl")
            .args(["--user", "enable", &spec.name])
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let exit_code = output.status.code().unwrap_or(-1);

            // Cleanup on failure - but leave the unit file for debugging
            return Err(SystemdError::SystemctlError {
                message: format!("Failed to enable service: {}", stderr),
                exit_code,
            });
        }
    }

    Ok(unit_path)
}

/// Uninstall a systemd service.
///
/// # Arguments
/// * `spec` - The service specification
///
/// # Returns
/// Result indicating success or failure
pub fn uninstall_systemd_service(spec: &ServiceSpec) -> Result<(), SystemdError> {
    let unit_path = get_unit_path(spec);

    if !unit_path.exists() {
        return Err(SystemdError::NotFound(spec.name.clone()));
    }

    // Stop the service if running
    let _ = Command::new("systemctl")
        .args(["--user", "stop", &spec.name])
        .output();

    // Disable the service
    let output = Command::new("systemctl")
        .args(["--user", "disable", &spec.name])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Continue even if disable fails - file might not be enabled
        eprintln!("Warning: Failed to disable service: {}", stderr);
    }

    // Remove the unit file
    std::fs::remove_file(&unit_path)?;

    // Reload systemd daemon
    let output = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);

        return Err(SystemdError::SystemctlError {
            message: format!("Failed to reload systemd: {}", stderr),
            exit_code,
        });
    }

    Ok(())
}

/// Start a systemd service.
///
/// # Arguments
/// * `spec` - The service specification
///
/// # Returns
/// Result indicating success or failure
pub fn start_systemd_service(spec: &ServiceSpec) -> Result<(), SystemdError> {
    let output = Command::new("systemctl")
        .args(["--user", "start", &spec.name])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);

        return Err(SystemdError::SystemctlError {
            message: format!("Failed to start service: {}", stderr),
            exit_code,
        });
    }

    Ok(())
}

/// Stop a systemd service.
///
/// # Arguments
/// * `spec` - The service specification
///
/// # Returns
/// Result indicating success or failure
pub fn stop_systemd_service(spec: &ServiceSpec) -> Result<(), SystemdError> {
    let output = Command::new("systemctl")
        .args(["--user", "stop", &spec.name])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);

        return Err(SystemdError::SystemctlError {
            message: format!("Failed to stop service: {}", stderr),
            exit_code,
        });
    }

    Ok(())
}

/// Restart a systemd service.
///
/// # Arguments
/// * `spec` - The service specification
///
/// # Returns
/// Result indicating success or failure
pub fn restart_systemd_service(spec: &ServiceSpec) -> Result<(), SystemdError> {
    let output = Command::new("systemctl")
        .args(["--user", "restart", &spec.name])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);

        return Err(SystemdError::SystemctlError {
            message: format!("Failed to restart service: {}", stderr),
            exit_code,
        });
    }

    Ok(())
}

/// Check if a systemd service is installed.
///
/// # Arguments
/// * `spec` - The service specification
///
/// # Returns
/// true if the service unit file exists
pub fn is_service_installed(spec: &ServiceSpec) -> bool {
    get_unit_path(spec).exists()
}

/// Get the status of a systemd service.
///
/// # Arguments
/// * `spec` - The service specification
///
/// # Returns
/// Result containing the service status as a string
pub fn get_service_status(spec: &ServiceSpec) -> Result<String, SystemdError> {
    let output = Command::new("systemctl")
        .args(["--user", "status", &spec.name])
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // systemctl status returns exit code 3 if service is not running, which is still a valid status
    if output.status.code() == Some(3) || output.status.success() {
        Ok(format!("{}{}", stdout, stderr))
    } else {
        Err(SystemdError::SystemctlError {
            message: stderr.to_string(),
            exit_code: output.status.code().unwrap_or(-1),
        })
    }
}

/// Enable a systemd service.
///
/// # Arguments
/// * `spec` - The service specification
///
/// # Returns
/// Result indicating success or failure
pub fn enable_systemd_service(spec: &ServiceSpec) -> Result<(), SystemdError> {
    let output = Command::new("systemctl")
        .args(["--user", "enable", &spec.name])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);

        return Err(SystemdError::SystemctlError {
            message: format!("Failed to enable service: {}", stderr),
            exit_code,
        });
    }

    Ok(())
}

/// Disable a systemd service.
///
/// # Arguments
/// * `spec` - The service specification
///
/// # Returns
/// Result indicating success or failure
pub fn disable_systemd_service(spec: &ServiceSpec) -> Result<(), SystemdError> {
    let output = Command::new("systemctl")
        .args(["--user", "disable", &spec.name])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);

        return Err(SystemdError::SystemctlError {
            message: format!("Failed to disable service: {}", stderr),
            exit_code,
        });
    }

    Ok(())
}

/// Check if systemd is available on this system.
pub fn is_systemd_available() -> bool {
    Command::new("systemctl")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_get_systemd_user_dir() {
        let dir = get_systemd_user_dir();
        let dir_str = dir.to_string_lossy();
        assert!(dir_str.contains("systemd") || dir_str.contains("~"));
        assert!(dir_str.contains("user") || dir_str.contains("~"));
    }

    #[test]
    fn test_get_unit_path() {
        let spec = ServiceSpec::new("test", "/bin/test");

        let path = get_unit_path(&spec);
        let path_str = path.to_string_lossy();

        assert!(path_str.contains("test.service"));
        assert!(path_str.contains("systemd") || path_str.contains("~"));
    }

    #[test]
    fn test_is_service_installed_check() {
        // This test would need a temp directory to fully test
        // For now, just verify the function returns false for non-existent paths
        let spec = ServiceSpec::new("nonexistent-test-service-xyz", "/bin/nonexistent");

        // Should be false since it doesn't exist
        assert!(!is_service_installed(&spec));
    }

    #[test]
    fn test_is_systemd_available() {
        // This test is platform-dependent
        // On Linux with systemd, it should return true
        // On macOS, it should return false
        let available = is_systemd_available();

        // Just verify it doesn't panic
        println!("Systemd available: {}", available);
    }
}
