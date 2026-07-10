use luther_workflow::service::{Service, ServiceConfig};
use std::process;

/// Handle the service command by dispatching to the requested subcommand.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
pub async fn handle_service_command(args: &luther_workflow::cli::ServiceArgs) {
    use luther_workflow::cli::ServiceCommand;

    match &args.command {
        ServiceCommand::Run(run_args) => handle_service_run(run_args).await,
        ServiceCommand::Install(install_args) => handle_service_install(install_args),
        ServiceCommand::Start => handle_service_lifecycle(ServiceLifecycle::Start),
        ServiceCommand::Stop => handle_service_lifecycle(ServiceLifecycle::Stop),
        ServiceCommand::Uninstall => handle_service_lifecycle(ServiceLifecycle::Uninstall),
        ServiceCommand::Status(status_args) => handle_service_status(status_args),
    }
}

/// Build the install spec for the current executable and working directory.
///
/// When `config_override` is provided it is appended to the supervised
/// process's argument list as `--config <path>` so the persisted service
/// definition launches `service run --config <path>`, honoring the
/// `service install --config` flag.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
pub fn build_service_spec(
    binary_override: Option<std::path::PathBuf>,
    config_override: Option<std::path::PathBuf>,
    operation: luther_workflow::service::ServiceOperation,
) -> Result<luther_workflow::service::ServiceSpec, luther_workflow::service::ServiceManagerError> {
    let binary = match binary_override.or_else(|| std::env::current_exe().ok()) {
        Some(path) => path,
        None => {
            return Err(
                luther_workflow::service::ServiceManagerError::executable_resolution_failure(
                    operation,
                ),
            );
        }
    };
    let working_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let mut spec = luther_workflow::service::build_install_spec(binary, working_dir);
    if let Some(config_path) = config_override {
        spec = spec
            .with_arg("--config")
            .with_arg(config_path.to_string_lossy().to_string());
    }
    Ok(spec)
}

/// Run the foreground service process supervised by launchd/systemd.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
pub async fn handle_service_run(args: &luther_workflow::cli::ServiceRunArgs) {
    let config = ServiceConfig {
        foreground: args.foreground,
        ipc_socket_path: args.socket_path.as_ref().map_or_else(
            || {
                luther_workflow::runtime_paths::get_data_dir()
                    .join("luther.sock")
                    .to_string_lossy()
                    .to_string()
            },
            |p| p.to_string_lossy().to_string(),
        ),
        log_level: "info".to_string(),
    };

    let mode = if config.foreground {
        "foreground"
    } else {
        "supervised"
    };
    println!("Starting service ({mode} mode)...");

    match Service::start(config).await {
        Ok(mut service) => {
            let instance_id = service
                .get_status()
                .await
                .map(|s| s.instance_id)
                .unwrap_or_default();
            println!("Service started successfully. Instance ID: {instance_id}");
            println!("Press Ctrl+C to stop...");
            let _ = tokio::signal::ctrl_c().await;
            println!("Shutting down service...");
            let _ = service.stop().await;
        }
        Err(e) => {
            eprintln!("Failed to start service: {e}");
            process::exit(1);
        }
    }
}

/// Install the platform service (launchd plist / systemd unit).
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
pub fn handle_service_install(args: &luther_workflow::cli::ServiceInstallArgs) {
    let spec = match build_service_spec(
        args.binary.clone(),
        args.config.clone(),
        luther_workflow::service::ServiceOperation::Install,
    ) {
        Ok(spec) => spec,
        Err(e) => {
            report_service_error(&e);
            process::exit(1);
        }
    };
    match luther_workflow::service::install_service(&spec) {
        Ok(path) => {
            println!("Service installed at: {}", path.display());
            println!("Start it with `luther-workflow service start`.");
        }
        Err(e) => {
            report_service_error(&e);
            process::exit(1);
        }
    }
}

/// Lifecycle operations that share the same dispatch shape.
pub enum ServiceLifecycle {
    Start,
    Stop,
    Uninstall,
}

/// Start/stop/uninstall the platform service.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
pub fn handle_service_lifecycle(action: ServiceLifecycle) {
    let operation = match action {
        ServiceLifecycle::Start => luther_workflow::service::ServiceOperation::Start,
        ServiceLifecycle::Stop => luther_workflow::service::ServiceOperation::Stop,
        ServiceLifecycle::Uninstall => luther_workflow::service::ServiceOperation::Uninstall,
    };
    let spec = match build_service_spec(None, None, operation) {
        Ok(spec) => spec,
        Err(e) => {
            report_service_error(&e);
            process::exit(1);
        }
    };
    let (result, success) = match action {
        ServiceLifecycle::Start => (
            luther_workflow::service::start_service(&spec),
            "Service started.",
        ),
        ServiceLifecycle::Stop => (
            luther_workflow::service::stop_service(&spec),
            "Service stopped.",
        ),
        ServiceLifecycle::Uninstall => (
            luther_workflow::service::uninstall_service(&spec),
            "Service uninstalled.",
        ),
    };
    match result {
        Ok(()) => println!("{success}"),
        Err(e) => {
            report_service_error(&e);
            process::exit(1);
        }
    }
}

/// Show the platform service status, optionally as JSON.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
pub fn handle_service_status(args: &luther_workflow::cli::ServiceStatusArgs) {
    let spec = match build_service_spec(
        None,
        None,
        luther_workflow::service::ServiceOperation::Status,
    ) {
        Ok(spec) => spec,
        Err(e) => {
            if args.json {
                report_service_error_json(&e);
            } else {
                report_service_error(&e);
            }
            process::exit(1);
        }
    };
    match luther_workflow::service::get_status(&spec) {
        Ok(status) => {
            if args.json {
                let payload = serde_json::json!({
                    "status": "ok",
                    "detail": status,
                });
                println!("{payload}");
            } else {
                println!("Service status:");
                println!("{status}");
            }
        }
        Err(e) => {
            if args.json {
                report_service_error_json(&e);
            } else {
                report_service_error(&e);
            }
            process::exit(1);
        }
    }
}

/// Print a structured, human-readable error block for service failures.
///
/// Surfaces platform, operation, OS-level message, log location, and
/// remediation steps (REQ-EARS-SVC-004). This helper only reports; the caller
/// owns the process exit decision so both the JSON and non-JSON paths handle
/// termination consistently.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
pub fn report_service_error(err: &luther_workflow::service::ServiceManagerError) {
    eprintln!("Service operation failed.");
    eprintln!("  Platform: {}", err.platform());
    if let Some(op) = err.operation() {
        eprintln!("  Operation: {op}");
    }
    eprintln!("  Error: {err}");
    if let Some(path) = err.log_path() {
        eprintln!("  Log location: {}", path.display());
    }
    eprintln!("  Remediation steps:");
    for step in err.get_remediation_steps() {
        eprintln!("    - {step}");
    }
}

/// Emit the same service-error fields as a JSON object for `--json` consumers.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
pub fn report_service_error_json(err: &luther_workflow::service::ServiceManagerError) {
    let operation = err.operation().map(|op| op.to_string());
    let log_path = err.log_path().map(|p| p.display().to_string());
    let payload = serde_json::json!({
        "status": "error",
        "platform": err.platform(),
        "operation": operation,
        "error": err.to_string(),
        "log_path": log_path,
        "remediation": err.get_remediation_steps(),
    });
    println!("{payload}");
}

/// Derive the config id (file stem) from a `--config` path, mirroring
/// `handle_run_command`.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
pub fn daemon_config_id(config: &std::path::Path) -> String {
    config.file_stem().map_or_else(
        || "default".to_string(),
        |s| s.to_string_lossy().to_string(),
    )
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod service_tests;
