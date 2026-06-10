//! Monitor module - supervision and lifecycle management for workflow runs.
//!
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
pub mod heartbeat;
pub mod ipc;
pub mod process;

pub use heartbeat::{
    delete_heartbeat, read_all_heartbeats, read_heartbeat, write_heartbeat, write_heartbeat_full,
    Heartbeat, HeartbeatError, HeartbeatWriter, MonitorState,
};
pub use ipc::{
    connect_ipc, create_ipc_endpoint, get_default_ipc_path, send_request, serve_status, IpcError,
    IpcRequest, IpcResponse, SharedState,
};
pub use process::{
    acquire_singleton_lock, calculate_backoff, is_process_alive, release_singleton_lock,
    MonitorConfig, ProcessState, SingletonGuard,
};

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;

/// Resource limits for a config profile.
pub struct ResourceLimits {
    pub max_memory_mb: u64,
    pub max_cpu_percent: f64,
}

/// Configuration profile.
pub struct ConfigProfile {
    pub name: String,
    pub max_concurrent_runs: u32,
    pub resource_limits: ResourceLimits,
}

/// Monitor instance for supervising workflow runs.
pub struct Monitor {
    // Retained so monitor runtime policy remains available as supervision expands.
    #[allow(dead_code)]
    config: process::MonitorConfig,
    instance_id: String,
    start_time: Instant,
    shutdown: bool,
    workers: Vec<String>,
    ipc_handle: Option<tokio::task::JoinHandle<Result<(), ipc::IpcError>>>,
    state: Arc<Mutex<ipc::SharedState>>,
    _lock_guard: Option<SingletonLock>, // Holds singleton lock if acquired
}

impl Monitor {
    /// Start a new monitor instance.
    pub async fn start(config: process::MonitorConfig) -> Result<Self, process::MonitorError> {
        let instance_id = format!("monitor-{}", std::process::id());
        let state = Arc::new(Mutex::new(ipc::SharedState::new(&instance_id)));

        // Set initial state
        {
            let mut s = state.lock().await;
            s.state = heartbeat::MonitorState::Running;
        }

        Ok(Self {
            config,
            instance_id,
            start_time: Instant::now(),
            shutdown: false,
            workers: Vec::new(),
            ipc_handle: None,
            state,
            _lock_guard: None,
        })
    }

    /// Start monitor in single-instance mode (with lock).
    pub async fn start_single_instance(
        config: process::MonitorConfig,
    ) -> Result<Self, process::MonitorError> {
        // Try to acquire singleton lock
        let guard = SingletonLock::acquire("luther-monitor").map_err(|e| match e {
            process::MonitorError::LockHeld { pid } => process::MonitorError::LockHeld { pid },
            _ => process::MonitorError::LockError {
                message: e.to_string(),
            },
        })?;

        let mut monitor = Self::start(config).await?;

        // Store the lock guard to keep it alive
        monitor._lock_guard = Some(guard);

        // Start IPC server
        let endpoint = create_ipc_endpoint().map_err(|e| process::MonitorError::General {
            message: e.to_string(),
        })?;
        monitor.ipc_handle = Some(serve_status(&endpoint, monitor.state.clone()));

        Ok(monitor)
    }

    /// Get next heartbeat.
    pub async fn next_heartbeat(&self) -> Option<heartbeat::Heartbeat> {
        let uptime = self.start_time.elapsed().as_secs() as i64;
        let state = self.state.lock().await;

        Some(heartbeat::Heartbeat {
            instance_id: self.instance_id.clone(),
            timestamp: chrono::Utc::now().timestamp(),
            uptime_secs: uptime,
            version: 1,
            state: state.state,
            active_workers: state.active_runs.len() as u32,
            run_id: None,
            metadata: HashMap::new(),
        })
    }

    /// Spawn a new worker.
    pub async fn spawn_worker(&mut self, task: &str) -> String {
        let id = format!("worker-{}", self.workers.len());
        self.workers.push(id.clone());

        // Update shared state
        let mut state = self.state.lock().await;
        state.active_runs.push(task.to_string());

        id
    }

    /// Graceful shutdown.
    pub async fn shutdown(&mut self) -> Result<(), process::MonitorError> {
        self.shutdown = true;
        self.workers.clear();

        // Update shared state
        let mut state = self.state.lock().await;
        state.state = heartbeat::MonitorState::Stopping;
        state.active_runs.clear();

        Ok(())
    }

    /// Check if monitor is shutdown.
    pub fn is_shutdown(&self) -> bool {
        self.shutdown
    }

    /// Get count of active workers.
    pub fn active_workers(&self) -> usize {
        self.workers.len()
    }
}

/// Singleton lock for single-instance mode.
#[derive(Debug)]
pub struct SingletonLock {
    // Kept for diagnostics when lock-acquisition reporting is extended.
    #[allow(dead_code)]
    path: String,
    guard: Option<SingletonGuard>,
}

impl SingletonLock {
    /// Acquire singleton lock.
    pub fn acquire(path: &str) -> Result<Self, process::MonitorError> {
        let guard = acquire_singleton_lock(path)?;
        Ok(Self {
            path: path.to_string(),
            guard: Some(guard),
        })
    }
}

impl Drop for SingletonLock {
    fn drop(&mut self) {
        if let Some(guard) = self.guard.take() {
            release_singleton_lock(guard);
        }
    }
}

/// Backoff strategy for restarts.
pub enum BackoffStrategy {
    Exponential {
        initial_secs: u64,
        max_secs: u64,
        multiplier: f64,
    },
    Fixed {
        secs: u64,
    },
}

/// Restart policy configuration.
pub struct RestartPolicy {
    pub max_restarts: u32,
    pub backoff_strategy: BackoffStrategy,
}

impl RestartPolicy {
    /// Calculate backoff delay for restart attempt.
    pub fn calculate_backoff(&self, attempt: u32) -> Duration {
        match &self.backoff_strategy {
            BackoffStrategy::Exponential {
                initial_secs,
                max_secs,
                multiplier,
            } => {
                let delay = (*initial_secs as f64) * multiplier.powi(attempt as i32 - 1);
                let delay_secs = (delay as u64).min(*max_secs);
                Duration::from_secs(delay_secs.max(*initial_secs))
            }
            BackoffStrategy::Fixed { secs } => Duration::from_secs(*secs),
        }
    }
}

/// Action to take when entering degraded state.
pub enum DegradedAction {
    AlertAndWait,
    AlertAndContinue,
    Shutdown,
}

impl DegradedAction {
    /// Check if this action requires an alert.
    pub fn requires_alert(&self) -> bool {
        matches!(
            self,
            DegradedAction::AlertAndWait | DegradedAction::AlertAndContinue
        )
    }

    /// Check if this action allows new work.
    pub fn allows_new_work(&self) -> bool {
        matches!(self, DegradedAction::AlertAndContinue)
    }
}

/// Tracks restart attempts and degraded state.
pub struct RestartTracker {
    policy: RestartPolicy,
    restart_count: u32,
}

impl RestartTracker {
    /// Create new restart tracker.
    pub fn new(policy: RestartPolicy) -> Self {
        Self {
            policy,
            restart_count: 0,
        }
    }

    /// Record a restart attempt.
    pub fn record_restart(&mut self) {
        self.restart_count += 1;
    }

    /// Check if should enter degraded state.
    pub fn should_enter_degraded(&self) -> bool {
        self.restart_count >= self.policy.max_restarts
    }

    /// Get the degraded action.
    pub fn get_degraded_action(&self) -> DegradedAction {
        DegradedAction::AlertAndWait
    }
}

/// Select a configuration profile by name.
pub fn select_profile<'a>(
    name: &str,
    profiles: &'a [ConfigProfile],
) -> Result<&'a ConfigProfile, process::MonitorError> {
    profiles
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| process::MonitorError::General {
            message: format!("Profile '{}' not found", name),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_restart_policy_exponential_backoff() {
        let policy = RestartPolicy {
            max_restarts: 5,
            backoff_strategy: BackoffStrategy::Exponential {
                initial_secs: 1,
                max_secs: 60,
                multiplier: 2.0,
            },
        };

        assert_eq!(policy.calculate_backoff(1), Duration::from_secs(1));
        assert_eq!(policy.calculate_backoff(2), Duration::from_secs(2));
        assert_eq!(policy.calculate_backoff(3), Duration::from_secs(4));
        assert_eq!(policy.calculate_backoff(10), Duration::from_secs(60)); // capped
    }

    #[test]
    fn test_restart_tracker() {
        let policy = RestartPolicy {
            max_restarts: 3,
            backoff_strategy: BackoffStrategy::Fixed { secs: 5 },
        };

        let mut tracker = RestartTracker::new(policy);

        assert!(!tracker.should_enter_degraded());
        tracker.record_restart();
        assert!(!tracker.should_enter_degraded());
        tracker.record_restart();
        tracker.record_restart();
        assert!(tracker.should_enter_degraded());

        let action = tracker.get_degraded_action();
        assert!(action.requires_alert());
        assert!(!action.allows_new_work());
    }

    #[test]
    fn test_select_profile() {
        let profiles = vec![
            ConfigProfile {
                name: "dev".to_string(),
                max_concurrent_runs: 2,
                resource_limits: ResourceLimits {
                    max_memory_mb: 512,
                    max_cpu_percent: 50.0,
                },
            },
            ConfigProfile {
                name: "prod".to_string(),
                max_concurrent_runs: 10,
                resource_limits: ResourceLimits {
                    max_memory_mb: 4096,
                    max_cpu_percent: 80.0,
                },
            },
        ];

        let dev = select_profile("dev", &profiles).expect("Should find dev");
        assert_eq!(dev.max_concurrent_runs, 2);

        let prod = select_profile("prod", &profiles).expect("Should find prod");
        assert_eq!(prod.max_concurrent_runs, 10);

        assert!(select_profile("staging", &profiles).is_err());
    }
}
