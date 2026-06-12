//! Service manager - platform dispatch for service lifecycle operations.
//!
//! Provides a single cross-platform entry point for installing, starting,
//! stopping, status-checking, and uninstalling the runtime service. Each
//! dispatch function routes to the launchd backend on macOS and the systemd
//! backend on Linux, returning a unified [`ServiceManagerError`] that carries
//! the platform, the failed operation, the underlying OS message, a log path,
//! and actionable remediation guidance (REQ-EARS-SVC-004).
//!
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P10
use std::fmt;
use std::path::PathBuf;
use thiserror::Error;

use crate::runtime_paths::get_log_dir;
use crate::service::spec::ServiceSpec;

#[cfg(target_os = "macos")]
use crate::service::launchd;
#[cfg(target_os = "linux")]
use crate::service::systemd;

/// Lifecycle operation performed against the platform service manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceOperation {
    Install,
    Start,
    Stop,
    Status,
    Uninstall,
}

impl fmt::Display for ServiceOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            ServiceOperation::Install => "install",
            ServiceOperation::Start => "start",
            ServiceOperation::Stop => "stop",
            ServiceOperation::Status => "status",
            ServiceOperation::Uninstall => "uninstall",
        };
        write!(f, "{label}")
    }
}

/// Unified error for platform service management operations.
#[derive(Debug, Error)]
pub enum ServiceManagerError {
    /// A platform service-manager command failed.
    #[error("{platform} service {operation} failed: {message}")]
    Operation {
        platform: &'static str,
        operation: ServiceOperation,
        message: String,
        log_path: Option<PathBuf>,
    },
    /// The host platform has no supported service backend.
    #[error("service management is not supported on this platform: {platform}")]
    UnsupportedPlatform { platform: &'static str },
}

impl ServiceManagerError {
    /// Platform identifier associated with this error (e.g. `macos`/`linux`).
    pub fn platform(&self) -> &'static str {
        match self {
            ServiceManagerError::Operation { platform, .. } => platform,
            ServiceManagerError::UnsupportedPlatform { platform } => platform,
        }
    }

    /// The operation that failed, if any.
    pub fn operation(&self) -> Option<ServiceOperation> {
        match self {
            ServiceManagerError::Operation { operation, .. } => Some(*operation),
            ServiceManagerError::UnsupportedPlatform { .. } => None,
        }
    }

    /// The log path users should inspect, if known.
    pub fn log_path(&self) -> Option<&PathBuf> {
        match self {
            ServiceManagerError::Operation { log_path, .. } => log_path.as_ref(),
            ServiceManagerError::UnsupportedPlatform { .. } => None,
        }
    }

    /// Platform- and operation-specific remediation guidance.
    ///
    /// Always includes the log path verbatim when known so operators can find
    /// the service output without guessing (REQ-EARS-SVC-004).
    pub fn get_remediation_steps(&self) -> Vec<String> {
        match self {
            ServiceManagerError::UnsupportedPlatform { platform } => vec![
                format!("Service management is not implemented for platform '{platform}'."),
                "Run the service directly with `service run --foreground`.".to_string(),
            ],
            ServiceManagerError::Operation {
                platform,
                operation,
                log_path,
                ..
            } => Self::operation_remediation(platform, *operation, log_path.as_ref()),
        }
    }

    fn operation_remediation(
        platform: &str,
        operation: ServiceOperation,
        log_path: Option<&PathBuf>,
    ) -> Vec<String> {
        let mut steps = vec![format!(
            "The `{operation}` operation failed; review the guidance below."
        )];
        if let Some(path) = log_path {
            steps.push(format!("Inspect service logs at: {}", path.display()));
        }
        match platform {
            "macos" => {
                steps.push(
                    "Validate the plist with `plutil -lint ~/Library/LaunchAgents/com.luther.*.plist`."
                        .to_string(),
                );
                steps.push("List loaded agents with `launchctl list | grep luther`.".to_string());
                steps.push(
                    "Check ~/Library/Logs/luther/ for captured stdout/stderr output.".to_string(),
                );
                steps.push(
                    "Verify ~/Library/LaunchAgents permissions allow writing the plist."
                        .to_string(),
                );
            }
            "linux" => {
                steps.push(
                    "View service logs with `journalctl --user -u <service> -n 100`.".to_string(),
                );
                steps.push(
                    "Check unit status with `systemctl --user status <service>`.".to_string(),
                );
                steps.push(
                    "Validate the unit file with `systemd-analyze --user verify <service>`."
                        .to_string(),
                );
                steps.push(
                    "Ensure a user session exists with `loginctl show-user $USER` (enable lingering if missing)."
                        .to_string(),
                );
            }
            other => {
                steps.push(format!(
                    "Platform '{other}' has no service backend; use `service run --foreground`."
                ));
            }
        }
        steps
    }
}

/// Resolve the current platform identifier.
fn current_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        std::env::consts::OS
    }
}

/// Build an [`ServiceManagerError::Operation`] for a backend failure.
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
fn operation_error(
    operation: ServiceOperation,
    message: String,
    spec: &ServiceSpec,
) -> ServiceManagerError {
    let log_path = spec
        .error_log_path
        .clone()
        .or_else(|| spec.log_path.clone())
        .or_else(|| Some(get_log_dir()));
    ServiceManagerError::Operation {
        platform: current_platform(),
        operation,
        message,
        log_path,
    }
}

/// Install the service for the current platform.
pub fn install_service(spec: &ServiceSpec) -> Result<PathBuf, ServiceManagerError> {
    #[cfg(target_os = "macos")]
    let result = launchd::install_launchd_service(spec)
        .map_err(|e| operation_error(ServiceOperation::Install, e.to_string(), spec));
    #[cfg(target_os = "linux")]
    let result = systemd::install_systemd_service(spec)
        .map_err(|e| operation_error(ServiceOperation::Install, e.to_string(), spec));
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let result = {
        let _ = spec;
        Err(ServiceManagerError::UnsupportedPlatform {
            platform: current_platform(),
        })
    };
    result
}

/// Start the installed service for the current platform.
pub fn start_service(spec: &ServiceSpec) -> Result<(), ServiceManagerError> {
    #[cfg(target_os = "macos")]
    let result = launchd::start_launchd_service(spec)
        .map_err(|e| operation_error(ServiceOperation::Start, e.to_string(), spec));
    #[cfg(target_os = "linux")]
    let result = systemd::start_systemd_service(spec)
        .map_err(|e| operation_error(ServiceOperation::Start, e.to_string(), spec));
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let result = {
        let _ = spec;
        Err(ServiceManagerError::UnsupportedPlatform {
            platform: current_platform(),
        })
    };
    result
}

/// Stop the running service for the current platform.
pub fn stop_service(spec: &ServiceSpec) -> Result<(), ServiceManagerError> {
    #[cfg(target_os = "macos")]
    let result = launchd::stop_launchd_service(spec)
        .map_err(|e| operation_error(ServiceOperation::Stop, e.to_string(), spec));
    #[cfg(target_os = "linux")]
    let result = systemd::stop_systemd_service(spec)
        .map_err(|e| operation_error(ServiceOperation::Stop, e.to_string(), spec));
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let result = {
        let _ = spec;
        Err(ServiceManagerError::UnsupportedPlatform {
            platform: current_platform(),
        })
    };
    result
}

/// Get the status of the service for the current platform.
pub fn get_status(spec: &ServiceSpec) -> Result<String, ServiceManagerError> {
    #[cfg(target_os = "macos")]
    let result = launchd::get_service_status(spec)
        .map_err(|e| operation_error(ServiceOperation::Status, e.to_string(), spec));
    #[cfg(target_os = "linux")]
    let result = systemd::get_service_status(spec)
        .map_err(|e| operation_error(ServiceOperation::Status, e.to_string(), spec));
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let result = {
        let _ = spec;
        Err(ServiceManagerError::UnsupportedPlatform {
            platform: current_platform(),
        })
    };
    result
}

/// Uninstall the service for the current platform.
pub fn uninstall_service(spec: &ServiceSpec) -> Result<(), ServiceManagerError> {
    #[cfg(target_os = "macos")]
    let result = launchd::uninstall_launchd_service(spec)
        .map_err(|e| operation_error(ServiceOperation::Uninstall, e.to_string(), spec));
    #[cfg(target_os = "linux")]
    let result = systemd::uninstall_systemd_service(spec)
        .map_err(|e| operation_error(ServiceOperation::Uninstall, e.to_string(), spec));
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let result = {
        let _ = spec;
        Err(ServiceManagerError::UnsupportedPlatform {
            platform: current_platform(),
        })
    };
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_error(platform: &'static str, operation: ServiceOperation) -> ServiceManagerError {
        ServiceManagerError::Operation {
            platform,
            operation,
            message: "boom".to_string(),
            log_path: Some(PathBuf::from("/tmp/luther/logs/service.log")),
        }
    }

    #[test]
    fn operation_display_renders_lowercase_labels() {
        assert_eq!(ServiceOperation::Install.to_string(), "install");
        assert_eq!(ServiceOperation::Start.to_string(), "start");
        assert_eq!(ServiceOperation::Stop.to_string(), "stop");
        assert_eq!(ServiceOperation::Status.to_string(), "status");
        assert_eq!(ServiceOperation::Uninstall.to_string(), "uninstall");
    }

    #[test]
    fn error_accessors_expose_fields() {
        let err = sample_error("macos", ServiceOperation::Start);
        assert_eq!(err.platform(), "macos");
        assert_eq!(err.operation(), Some(ServiceOperation::Start));
        assert_eq!(
            err.log_path().map(|p| p.to_string_lossy().to_string()),
            Some("/tmp/luther/logs/service.log".to_string())
        );
    }

    #[test]
    fn macos_remediation_includes_log_and_launchctl_guidance() {
        let err = sample_error("macos", ServiceOperation::Install);
        let steps = err.get_remediation_steps().join("\n");
        assert!(steps.contains("/tmp/luther/logs/service.log"));
        assert!(steps.contains("plutil"));
        assert!(steps.contains("launchctl list"));
    }

    #[test]
    fn linux_remediation_includes_log_and_journalctl_guidance() {
        let err = sample_error("linux", ServiceOperation::Status);
        let steps = err.get_remediation_steps().join("\n");
        assert!(steps.contains("/tmp/luther/logs/service.log"));
        assert!(steps.contains("journalctl --user -u"));
        assert!(steps.contains("loginctl"));
    }

    #[test]
    fn unsupported_platform_has_foreground_fallback() {
        let err = ServiceManagerError::UnsupportedPlatform {
            platform: "freebsd",
        };
        assert_eq!(err.platform(), "freebsd");
        assert_eq!(err.operation(), None);
        let steps = err.get_remediation_steps().join("\n");
        assert!(steps.contains("freebsd"));
        assert!(steps.contains("service run --foreground"));
    }
}
