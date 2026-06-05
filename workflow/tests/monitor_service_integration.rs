//! Monitor and Service integration tests.
//!
//! Tests for monitor heartbeat, shutdown behavior, single instance mode,
//! and config profile selection.

use std::time::Duration;
use tokio::time::timeout;

/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
/// @requirement:REQ-EARS-MON-002
/// Test: Monitor heartbeat includes metadata
#[tokio::test]
async fn test_monitor_heartbeat_metadata() {
    // GIVEN: a running monitor instance
    let config = luther_workflow::monitor::MonitorConfig::default();

    let monitor = luther_workflow::monitor::Monitor::start(config)
        .await
        .expect("Failed to start monitor");

    // WHEN: waiting for heartbeat
    let heartbeat = timeout(Duration::from_secs(2), monitor.next_heartbeat())
        .await
        .expect("Timeout waiting for heartbeat")
        .expect("Monitor stopped unexpectedly");

    // THEN: heartbeat contains required metadata
    assert!(
        !heartbeat.instance_id.is_empty(),
        "Heartbeat should include instance ID"
    );
    assert!(
        heartbeat.timestamp > 0,
        "Heartbeat should include timestamp"
    );
    assert!(
        heartbeat.uptime_secs >= 0,
        "Heartbeat should include uptime"
    );
    assert!(heartbeat.version >= 0, "Heartbeat should include version");
}

/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
/// @requirement:REQ-EARS-MON-005
/// Test: Monitor graceful shutdown
#[tokio::test]
async fn test_monitor_shutdown_graceful() {
    // GIVEN: a running monitor with active workers
    let config = luther_workflow::monitor::MonitorConfig::default();

    let mut monitor = luther_workflow::monitor::Monitor::start(config)
        .await
        .expect("Failed to start monitor");

    // Start some work
    let _worker = monitor.spawn_worker("test-task").await;

    // WHEN: requesting shutdown
    let shutdown_result = timeout(Duration::from_secs(5), monitor.shutdown())
        .await
        .expect("Shutdown timed out");

    // THEN: all workers complete, resources released
    assert!(
        shutdown_result.is_ok(),
        "Graceful shutdown should succeed: {:?}",
        shutdown_result.err()
    );
    assert!(monitor.is_shutdown(), "Monitor should be in shutdown state");
    assert_eq!(
        monitor.active_workers(),
        0,
        "All workers should be terminated"
    );
}

/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
/// @requirement:REQ-EARS-SCALE-001
/// Test: Single instance mode prevents concurrent monitors
#[tokio::test]
async fn test_single_instance_mode() {
    // GIVEN: a running monitor in single-instance mode
    let config = luther_workflow::monitor::MonitorConfig::default();

    let _first_monitor = luther_workflow::monitor::Monitor::start_single_instance(config.clone())
        .await
        .expect("First monitor should start in single-instance mode");

    // WHEN: attempting to start second monitor
    let second_result = luther_workflow::monitor::Monitor::start_single_instance(config).await;

    // THEN: second monitor fails with AlreadyRunning error
    assert!(
        second_result.is_err(),
        "Second monitor should fail in single-instance mode"
    );
    match second_result {
        Err(e) => {
            let error_str = e.to_string().to_lowercase();
            assert!(
                error_str.contains("already running")
                    || error_str.contains("single instance")
                    || error_str.contains("lock")
                    || error_str.contains("held"),
                "Error should indicate single instance conflict: {e}"
            );
        }
        Ok(_) => panic!("Expected second monitor to fail"),
    }
}

/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
/// @requirement:REQ-EARS-SCALE-003
/// Test: Config profile selection
#[tokio::test]
async fn test_config_profile_selection() {
    // GIVEN: multiple config profiles available
    let profiles = vec![
        luther_workflow::monitor::ConfigProfile {
            name: "development".to_string(),
            max_concurrent_runs: 2,
            resource_limits: luther_workflow::monitor::ResourceLimits {
                max_memory_mb: 1024,
                max_cpu_percent: 50.0,
            },
        },
        luther_workflow::monitor::ConfigProfile {
            name: "production".to_string(),
            max_concurrent_runs: 10,
            resource_limits: luther_workflow::monitor::ResourceLimits {
                max_memory_mb: 4096,
                max_cpu_percent: 80.0,
            },
        },
    ];

    // WHEN: selecting profiles
    let dev_profile = luther_workflow::monitor::select_profile("development", &profiles)
        .expect("Should find development profile");

    let prod_profile = luther_workflow::monitor::select_profile("production", &profiles)
        .expect("Should find production profile");

    // THEN: correct profiles with their settings are returned
    assert_eq!(dev_profile.name, "development");
    assert_eq!(dev_profile.max_concurrent_runs, 2);
    assert_eq!(dev_profile.resource_limits.max_memory_mb, 1024);

    assert_eq!(prod_profile.name, "production");
    assert_eq!(prod_profile.max_concurrent_runs, 10);
    assert_eq!(prod_profile.resource_limits.max_memory_mb, 4096);

    // Non-existent profile should fail
    let missing = luther_workflow::monitor::select_profile("staging", &profiles);
    assert!(missing.is_err(), "Should fail for non-existent profile");
}
