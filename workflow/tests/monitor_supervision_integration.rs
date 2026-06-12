//! Monitor worker-supervision integration tests.
//!
//! Validates that the monitor supervises real OS worker processes: spawning,
//! restart/backoff under policy, degraded transition at the restart limit, and
//! graceful shutdown that actually terminates children. IPC `active_runs` must
//! reflect the true supervised process set.
//!
//! @plan:PLAN-20260404-INITIAL-RUNTIME.P10
//! @requirement:REQ-EARS-MON-002

use luther_workflow::monitor::{Monitor, MonitorConfig, ResourceLimits};
use std::time::Duration;

/// Build a config with a given restart policy and small restart budget so the
/// degraded path is reachable quickly in tests.
fn config_with(restart_policy: &str, max_restarts: u32) -> MonitorConfig {
    MonitorConfig {
        restart_policy: restart_policy.to_string(),
        backoff_strategy: "fixed".to_string(),
        initial_backoff_secs: 0,
        max_backoff_secs: 0,
        max_restarts,
        singleton_mode: false,
        ..Default::default()
    }
}

/// A spec for a child that exits immediately with the given code.
fn exit_with(code: i32) -> (String, Vec<String>) {
    (
        "sh".to_string(),
        vec!["-c".to_string(), format!("exit {code}")],
    )
}

/// @requirement:REQ-EARS-MON-002
/// Spawning a real worker is reflected in IPC active_runs / active_workers.
#[tokio::test]
async fn test_spawn_reflects_in_status() {
    let mut monitor = Monitor::start(config_with("no_restart", 3))
        .await
        .expect("monitor start");

    let id = monitor
        .spawn_worker_command(
            "w-status",
            "sleep",
            &["86400".to_string()],
            ResourceLimits::unlimited(),
        )
        .await
        .expect("spawn long-lived worker");

    assert_eq!(monitor.active_workers(), 1, "one worker should be tracked");
    let hb = monitor.next_heartbeat().await.expect("heartbeat");
    assert_eq!(hb.active_workers, 1, "heartbeat reflects supervised worker");
    assert_eq!(id, "w-status");

    monitor.shutdown().await.expect("shutdown");
    assert_eq!(monitor.active_workers(), 0, "no workers after shutdown");
}

/// @requirement:REQ-EARS-MON-002
/// A worker that exits 0 under on_failure is not restarted and is retired.
#[tokio::test]
async fn test_success_exit_not_restarted() {
    let mut monitor = Monitor::start(config_with("on_failure", 3))
        .await
        .expect("monitor start");
    let (prog, args) = exit_with(0);
    monitor
        .spawn_worker_command("w-ok", &prog, &args, ResourceLimits::unlimited())
        .await
        .expect("spawn");

    // Give the child a moment to exit, then supervise.
    tokio::time::sleep(Duration::from_millis(50)).await;
    monitor.supervise_tick().await.expect("supervise");

    assert_eq!(
        monitor.active_workers(),
        0,
        "successful worker should be retired, not restarted"
    );
}

/// @requirement:REQ-EARS-MON-002
/// A failing worker under on_failure is restarted up to the limit, then the
/// monitor enters the Degraded state.
#[tokio::test]
async fn test_failure_restarts_then_degrades() {
    let mut monitor = Monitor::start(config_with("on_failure", 2))
        .await
        .expect("monitor start");
    let (prog, args) = exit_with(1);
    monitor
        .spawn_worker_command("w-fail", &prog, &args, ResourceLimits::unlimited())
        .await
        .expect("spawn");

    // Drive several supervision passes; each observes the failed exit and
    // either respawns (until the limit) or retires + degrades.
    for _ in 0..6 {
        tokio::time::sleep(Duration::from_millis(30)).await;
        monitor.supervise_tick().await.expect("supervise");
    }

    assert_eq!(
        monitor.active_workers(),
        0,
        "worker retired after exhausting restarts"
    );
    let hb = monitor.next_heartbeat().await.expect("heartbeat");
    assert_eq!(hb.state, luther_workflow::monitor::MonitorState::Degraded);

    monitor.shutdown().await.expect("shutdown");
}

/// @requirement:REQ-EARS-MON-002
/// Under no_restart, even a failing worker is retired without restart.
#[tokio::test]
async fn test_no_restart_policy() {
    let mut monitor = Monitor::start(config_with("no_restart", 5))
        .await
        .expect("monitor start");
    let (prog, args) = exit_with(1);
    monitor
        .spawn_worker_command("w-noretry", &prog, &args, ResourceLimits::unlimited())
        .await
        .expect("spawn");

    tokio::time::sleep(Duration::from_millis(50)).await;
    monitor.supervise_tick().await.expect("supervise");

    assert_eq!(
        monitor.active_workers(),
        0,
        "no_restart retires failing worker without restart"
    );
}

/// @requirement:REQ-EARS-MON-005
/// Graceful shutdown terminates a long-lived child and drives state to Stopped.
#[tokio::test]
async fn test_shutdown_terminates_children() {
    let mut monitor = Monitor::start(config_with("always", 3))
        .await
        .expect("monitor start");
    monitor
        .spawn_worker_command(
            "w-long",
            "sleep",
            &["86400".to_string()],
            ResourceLimits::unlimited(),
        )
        .await
        .expect("spawn");
    assert_eq!(monitor.active_workers(), 1);

    monitor.shutdown().await.expect("shutdown");

    assert!(monitor.is_shutdown(), "monitor reports shutdown");
    assert_eq!(monitor.active_workers(), 0, "children terminated");
    let hb = monitor.next_heartbeat().await.expect("heartbeat");
    assert_eq!(hb.state, luther_workflow::monitor::MonitorState::Stopped);
}
