//! Service IPC contract integration tests.
//!
//! Tests for service foreground mode, IPC status endpoint,
//! and service failure diagnostics.

use std::time::Duration;
use tokio::time::timeout;

/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
/// @requirement:REQ-EARS-SVC-001
/// Test: Service foreground mode (no daemonization)
#[tokio::test]
async fn test_service_foreground_mode() {
    // GIVEN: service configuration with foreground mode
    let config = luther_workflow::service::ServiceConfig {
        foreground: true,
        ipc_socket_path: "/tmp/luther-test-foreground.sock".to_string(),
        log_level: "info".to_string(),
    };

    // WHEN: starting service in foreground mode
    let mut service = luther_workflow::service::Service::start(config)
        .await
        .expect("Should start service in foreground mode");

    // THEN: service runs in current process (not daemonized)
    assert!(
        service.is_foreground(),
        "Service should be in foreground mode"
    );
    assert!(!service.is_daemonized(), "Service should not be daemonized");
    assert!(service.is_running(), "Service should be running");

    // Verify service is accessible without PID file
    let status = service.get_status().await.expect("Should get status");
    assert_eq!(
        status.state,
        luther_workflow::service::ServiceState::Running
    );

    // Clean up
    service.stop().await.expect("Should stop service");
}

/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
/// @requirement:REQ-EARS-SVC-003
/// Test: IPC status endpoint returns health information
#[tokio::test]
async fn test_ipc_status_endpoint() {
    // GIVEN: a running service with IPC enabled
    let config = luther_workflow::service::ServiceConfig {
        foreground: true,
        ipc_socket_path: "/tmp/luther-test-status.sock".to_string(),
        log_level: "info".to_string(),
    };

    let mut service = luther_workflow::service::Service::start(config)
        .await
        .expect("Should start service");

    // Create IPC client
    let client = luther_workflow::service::IpcClient::connect("/tmp/luther-test-status.sock")
        .await
        .expect("Should connect to IPC socket");

    // WHEN: requesting status via IPC
    let status_request = luther_workflow::service::StatusRequest {
        include_metrics: true,
        include_active_runs: true,
    };

    let response = timeout(Duration::from_secs(5), client.get_status(status_request))
        .await
        .expect("IPC status request timed out")
        .expect("IPC status request failed");

    // THEN: response contains health and status information
    assert!(
        !response.instance_id.is_empty(),
        "Status should include instance ID"
    );
    assert!(response.uptime_secs >= 0, "Status should include uptime");
    assert!(response.version >= 0, "Status should include version");

    // Verify metrics are included
    assert!(
        response.metrics.is_some(),
        "Status should include metrics when requested"
    );
    let metrics = response.metrics.unwrap();
    assert!(
        metrics.memory_usage_mb >= 0,
        "Metrics should include memory usage"
    );
    assert!(
        metrics.cpu_usage_percent >= 0.0,
        "Metrics should include CPU usage"
    );

    // Verify active runs info
    assert!(
        response.active_runs.is_some(),
        "Status should include active runs when requested"
    );

    // Clean up
    service.stop().await.expect("Should stop service");
}

/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
/// @requirement:REQ-EARS-SVC-004
/// Test: Service failure diagnostics
#[tokio::test]
async fn test_service_failure_diagnostics() {
    // GIVEN: a service that encounters a failure
    let config = luther_workflow::service::ServiceConfig {
        foreground: true,
        ipc_socket_path: "/tmp/luther-test-fail.sock".to_string(),
        log_level: "debug".to_string(),
    };

    let mut service = luther_workflow::service::Service::start(config)
        .await
        .expect("Should start service");

    // Simulate a failure condition
    let failure_result = service
        .simulate_failure(luther_workflow::service::FailureType::InternalError)
        .await;

    // WHEN: failure occurs
    assert!(
        failure_result.is_err(),
        "Simulated failure should return error"
    );
    let err = failure_result.unwrap_err();

    // THEN: error contains diagnostic information
    let diag = err.get_diagnostics();
    assert!(
        !diag.is_empty(),
        "Error should provide structured diagnostics"
    );

    // Verify diagnostics include error code
    assert!(
        diag.contains_key("error_code"),
        "Diagnostics should include error code"
    );

    // Verify diagnostics include timestamp
    assert!(
        diag.contains_key("timestamp"),
        "Diagnostics should include timestamp"
    );

    // Verify recovery suggestions are provided
    let recovery = err.get_recovery_suggestions();
    assert!(
        !recovery.is_empty(),
        "Error should provide recovery suggestions"
    );

    // Clean up
    let _ = service.stop().await;
}

/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
/// @requirement:REQ-EARS-SVC-001
/// Test: Daemon (OS-supervised) mode is explicit without self-forking.
#[tokio::test]
async fn test_service_daemon_mode_is_explicit() {
    // GIVEN: service configuration with foreground disabled
    let config = luther_workflow::service::ServiceConfig {
        foreground: false,
        ipc_socket_path: "/tmp/luther-test-daemon.sock".to_string(),
        log_level: "info".to_string(),
    };

    // WHEN: starting service (no self-fork is performed)
    let mut service = luther_workflow::service::Service::start(config)
        .await
        .expect("Should start service in supervised mode");

    // THEN: the mode is reported as daemonized/non-foreground
    assert!(
        service.is_daemonized(),
        "Service should report daemonized mode"
    );
    assert!(
        !service.is_foreground(),
        "Service should not report foreground mode"
    );

    // Clean up
    service.stop().await.expect("Should stop service");
}
