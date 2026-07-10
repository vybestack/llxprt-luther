use super::*;
use luther_workflow::service::{ServiceManagerError, ServiceOperation};
use std::path::PathBuf;

#[test]
fn build_service_spec_uses_binary_override() {
    let spec = build_service_spec(
        Some(PathBuf::from("/opt/bin/luther-workflow")),
        None,
        ServiceOperation::Install,
    )
    .expect("spec builds with explicit binary override");
    assert_eq!(spec.binary_path, PathBuf::from("/opt/bin/luther-workflow"));
    assert!(
        !spec.args.iter().any(|a| a == "--config"),
        "no config arg when config_override is None"
    );
}

#[test]
fn build_service_spec_appends_config_override() {
    let spec = build_service_spec(
        Some(PathBuf::from("/opt/bin/luther-workflow")),
        Some(PathBuf::from("/etc/luther/prod.toml")),
        ServiceOperation::Install,
    )
    .expect("spec builds with config override");
    let joined = spec.args.join(" ");
    assert!(
        joined.contains("--config"),
        "config override should append --config flag, got: {joined}"
    );
    assert!(
        joined.contains("/etc/luther/prod.toml"),
        "config override should include the config path, got: {joined}"
    );
    // The --config flag must be immediately followed by the path value.
    let idx = spec
        .args
        .iter()
        .position(|a| a == "--config")
        .expect("--config present");
    assert_eq!(
        spec.args.get(idx + 1).map(String::as_str),
        Some("/etc/luther/prod.toml")
    );
}

#[test]
fn build_service_spec_falls_back_to_current_exe() {
    // With no override, resolution falls back to current_exe(), which is
    // available under the test harness, so this should succeed.
    let spec = build_service_spec(None, None, ServiceOperation::Start)
        .expect("current_exe resolution succeeds in test harness");
    assert!(
        !spec.binary_path.as_os_str().is_empty(),
        "resolved binary path should not be empty"
    );
}

#[test]
fn executable_resolution_failure_is_actionable() {
    let err = ServiceManagerError::executable_resolution_failure(ServiceOperation::Install);
    assert_eq!(err.operation(), Some(ServiceOperation::Install));
    let message = err.to_string();
    assert!(
        message.contains("current_exe"),
        "error should mention current_exe, got: {message}"
    );
    assert!(
        message.contains("--binary"),
        "error should mention the --binary override, got: {message}"
    );
    // A resolution failure carries no log path.
    assert!(err.log_path().is_none());
    // Remediation steps are always non-empty so operators have guidance.
    assert!(!err.get_remediation_steps().is_empty());
}

#[test]
fn report_service_error_reports_without_exiting() {
    // report_service_error only reports; it must return so the caller owns
    // the exit decision for both JSON and non-JSON paths.
    let err = ServiceManagerError::executable_resolution_failure(ServiceOperation::Status);
    report_service_error(&err);
    // Reaching this line proves the helper returned rather than exiting.
}

#[test]
fn report_service_error_json_reports_without_exiting() {
    let err = ServiceManagerError::executable_resolution_failure(ServiceOperation::Status);
    report_service_error_json(&err);
    // Reaching this line proves the JSON helper returned rather than exiting.
}

#[test]
fn daemon_config_id_uses_file_stem() {
    assert_eq!(
        daemon_config_id(std::path::Path::new("/etc/luther/prod.toml")),
        "prod"
    );
    assert_eq!(
        daemon_config_id(std::path::Path::new("workflow.yaml")),
        "workflow"
    );
}

#[test]
fn daemon_config_id_defaults_when_no_stem() {
    assert_eq!(daemon_config_id(std::path::Path::new("/")), "default");
    assert_eq!(daemon_config_id(std::path::Path::new("")), "default");
}

#[test]
fn service_lifecycle_variants_construct() {
    // Exercise each lifecycle variant so the enum is covered.
    let variants = [
        ServiceLifecycle::Start,
        ServiceLifecycle::Stop,
        ServiceLifecycle::Uninstall,
    ];
    let operations: Vec<ServiceOperation> = variants
        .iter()
        .map(|action| match action {
            ServiceLifecycle::Start => ServiceOperation::Start,
            ServiceLifecycle::Stop => ServiceOperation::Stop,
            ServiceLifecycle::Uninstall => ServiceOperation::Uninstall,
        })
        .collect();
    assert_eq!(
        operations,
        vec![
            ServiceOperation::Start,
            ServiceOperation::Stop,
            ServiceOperation::Uninstall,
        ]
    );
}
