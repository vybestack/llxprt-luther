//! Service lifecycle integration tests.
//!
//! Covers install-spec construction, supervisor directive generation, the
//! cross-platform manager dispatch, and the unified error remediation guidance.
//! Tests are cross-platform and must not depend on a live launchctl/systemctl
//! side effect (CI runs on ubuntu-latest only).
//!
//! @plan:PLAN-20260404-INITIAL-RUNTIME.P10
//! @requirement:REQ-EARS-SVC-004

use luther_workflow::runtime_paths::get_log_dir;
use luther_workflow::service::{
    build_install_spec, generate_launchd_plist, generate_systemd_unit, get_status,
    install_target_path, ServiceManagerError, ServiceOperation,
};

#[test]
fn install_spec_uses_current_exe_and_default_log_paths() {
    let binary = std::env::current_exe().expect("current exe");
    let working_dir = std::env::current_dir().expect("current dir");
    let spec = build_install_spec(binary.clone(), working_dir.clone());

    assert_eq!(spec.binary_path, binary);
    assert_eq!(spec.working_dir, working_dir);

    let log_dir = get_log_dir();
    assert_eq!(spec.log_path, Some(log_dir.join("service.out.log")));
    assert_eq!(spec.error_log_path, Some(log_dir.join("service.err.log")));
}

#[test]
fn install_spec_generates_launchd_supervisor_directives() {
    let spec = build_install_spec("/usr/local/bin/luther", "/var/lib/luther");
    let plist = generate_launchd_plist(&spec);

    assert!(plist.contains("<key>StandardOutPath</key>"));
    assert!(plist.contains("<key>StandardErrorPath</key>"));
    assert!(plist.contains("<key>KeepAlive</key>"));
    assert!(plist.contains("<key>RunAtLoad</key>"));
}

#[test]
fn install_spec_generates_systemd_supervisor_directives() {
    let spec = build_install_spec("/usr/local/bin/luther", "/var/lib/luther");
    let unit = generate_systemd_unit(&spec);

    assert!(unit.contains("StandardOutput=append:"));
    assert!(unit.contains("Restart=on-failure"));
    assert!(unit.contains("WantedBy="));
}

#[test]
fn manager_error_remediation_macos_includes_log_and_launchctl() {
    let err = ServiceManagerError::Operation {
        platform: "macos",
        operation: ServiceOperation::Install,
        message: "launchctl load failed".to_string(),
        log_path: Some(get_log_dir().join("service.err.log")),
    };

    let steps = err.get_remediation_steps().join("\n");
    assert!(steps.contains("service.err.log"));
    assert!(steps.contains("plutil"));
    assert!(steps.contains("launchctl list"));
    assert_eq!(err.platform(), "macos");
    assert_eq!(err.operation(), Some(ServiceOperation::Install));
}

#[test]
fn manager_error_remediation_linux_includes_log_and_journalctl() {
    let err = ServiceManagerError::Operation {
        platform: "linux",
        operation: ServiceOperation::Start,
        message: "systemctl start failed".to_string(),
        log_path: Some(get_log_dir().join("service.err.log")),
    };

    let steps = err.get_remediation_steps().join("\n");
    assert!(steps.contains("service.err.log"));
    assert!(steps.contains("journalctl --user -u"));
    assert!(steps.contains("loginctl"));
    assert_eq!(err.platform(), "linux");
    assert_eq!(err.operation(), Some(ServiceOperation::Start));
}

#[test]
fn unsupported_platform_error_offers_foreground_fallback() {
    let err = ServiceManagerError::UnsupportedPlatform {
        platform: "freebsd",
    };
    assert_eq!(err.operation(), None);
    let steps = err.get_remediation_steps().join("\n");
    assert!(steps.contains("freebsd"));
    assert!(steps.contains("service run --foreground"));
}

#[test]
fn manager_dispatch_is_reachable_on_host_platform() {
    // On supported platforms the call routes to launchd/systemd; on others it
    // returns UnsupportedPlatform. We must not assert live supervisor results
    // on CI, so we only assert the dispatch is reachable and well-typed.
    let spec = build_install_spec("/usr/local/bin/luther", "/var/lib/luther");
    match get_status(&spec) {
        Ok(_) => {}
        Err(ServiceManagerError::UnsupportedPlatform { platform }) => {
            assert!(!platform.is_empty());
        }
        Err(ServiceManagerError::Operation {
            platform,
            operation,
            ..
        }) => {
            assert!(!platform.is_empty());
            assert_eq!(operation, ServiceOperation::Status);
        }
    }
}

#[test]
fn log_dir_is_non_empty_and_references_luther() {
    let log_dir = get_log_dir();
    let s = log_dir.to_string_lossy();
    assert!(!s.is_empty());
    assert!(s.contains("luther") || s.contains(".luther"));
}

#[test]
fn install_service_dispatch_is_well_typed() {
    // Smoke-test the install dispatch routing without performing a real,
    // host-mutating install. `install_target_path` resolves the same
    // platform-specific destination as `install_service` but never writes
    // files or invokes launchctl/systemctl, so this test stays free of live
    // side effects (CI runs on ubuntu-latest only).
    let spec = build_install_spec("/nonexistent/luther-binary-xyz", "/tmp");
    match install_target_path(&spec) {
        Ok(path) => {
            // On supported platforms we get the plist/unit destination path.
            assert!(!path.as_os_str().is_empty());
        }
        Err(ServiceManagerError::UnsupportedPlatform { platform }) => {
            assert!(!platform.is_empty());
        }
        Err(ServiceManagerError::Operation { operation, .. }) => {
            assert_eq!(operation, ServiceOperation::Install);
        }
    }
}
