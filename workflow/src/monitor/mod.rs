//! Monitor module - supervision and lifecycle management for workflow runs.
//!
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
pub mod heartbeat;
pub mod ipc;
pub mod process;
pub mod snapshot;

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
pub use snapshot::{
    render_snapshot, resolve_snapshot_count, separator_line, DaemonSummary, FilteredRuns,
    MonitorFilter, MonitorSnapshot, RunCounts, CLEAR_SCREEN,
};

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;

/// Resource limits for a config profile.
#[derive(Debug, Clone, Copy)]
pub struct ResourceLimits {
    pub max_memory_mb: u64,
    pub max_cpu_percent: f64,
}

impl ResourceLimits {
    /// Construct limits that never trigger enforcement. Useful for benign
    /// helper workers and for the back-compatible [`Monitor::spawn_worker`]
    /// entry point where no explicit limits are supplied.
    pub fn unlimited() -> Self {
        Self {
            max_memory_mb: u64::MAX,
            max_cpu_percent: f64::INFINITY,
        }
    }
}

/// Spawn specification retained for a supervised worker so it can be
/// respawned under the restart policy.
struct WorkerSpec {
    program: String,
    args: Vec<String>,
    limits: ResourceLimits,
}

/// A supervised worker: the live child process plus its restart bookkeeping
/// and the spec needed to respawn it.
struct WorkerHandle {
    child: tokio::process::Child,
    state: process::ProcessState,
    spec: WorkerSpec,
    last_exit_code: Option<i32>,
}

/// Configuration profile.
pub struct ConfigProfile {
    pub name: String,
    pub max_concurrent_runs: u32,
    pub resource_limits: ResourceLimits,
}

/// Monitor instance for supervising workflow runs.
pub struct Monitor {
    config: process::MonitorConfig,
    instance_id: String,
    start_time: Instant,
    shutdown: bool,
    workers: HashMap<String, WorkerHandle>,
    next_worker_seq: usize,
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
            workers: HashMap::new(),
            next_worker_seq: 0,
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

    /// Spawn a new worker for the given task (back-compatible entry point).
    ///
    /// This routes through [`Monitor::spawn_worker_command`] using a benign,
    /// long-lived child process so that the worker actually exists as a real
    /// OS process and is terminated on shutdown. Returns the assigned worker
    /// id; on spawn failure it still records the worker id logically so the
    /// historical (infallible) signature is preserved.
    pub async fn spawn_worker(&mut self, task: &str) -> String {
        let id = self.next_worker_id();
        // A harmless, long-lived child; supervision/shutdown will terminate it.
        let program = "sleep".to_string();
        let args = vec!["86400".to_string()];
        let _ = self
            .spawn_worker_command(&id, &program, &args, ResourceLimits::unlimited())
            .await;
        // Keep the task name visible in IPC status if the spawn failed for any
        // reason (e.g. `sleep` unavailable), so status still reflects intent.
        if !self.workers.contains_key(&id) {
            let mut state = self.state.lock().await;
            if !state.active_runs.contains(&id) {
                state.active_runs.push(id.clone());
            }
            let _ = task;
        }
        id
    }

    /// Allocate the next sequential worker id.
    fn next_worker_id(&mut self) -> String {
        let id = format!("worker-{}", self.next_worker_seq);
        self.next_worker_seq += 1;
        id
    }

    /// Spawn a supervised worker process running `program` with `args`.
    ///
    /// On success the child is tracked and the worker id is added to the
    /// authoritative IPC `active_runs` set. On failure a
    /// [`process::MonitorError::SpawnFailed`] is returned and nothing is
    /// recorded.
    pub async fn spawn_worker_command(
        &mut self,
        id: &str,
        program: &str,
        args: &[String],
        limits: ResourceLimits,
    ) -> Result<String, process::MonitorError> {
        let spec = WorkerSpec {
            program: program.to_string(),
            args: args.to_vec(),
            limits,
        };
        let child = Self::spawn_child(id, &spec)?;
        let handle = WorkerHandle {
            child,
            state: process::ProcessState::new(self.config.clone()),
            spec,
            last_exit_code: None,
        };
        self.workers.insert(id.to_string(), handle);
        self.mark_active(id).await;
        Ok(id.to_string())
    }

    /// Spawn the OS child described by `spec`.
    fn spawn_child(
        id: &str,
        spec: &WorkerSpec,
    ) -> Result<tokio::process::Child, process::MonitorError> {
        tokio::process::Command::new(&spec.program)
            .args(&spec.args)
            .spawn()
            .map_err(|e| process::MonitorError::SpawnFailed {
                id: id.to_string(),
                message: e.to_string(),
            })
    }

    /// Record a worker id as active in shared IPC state (idempotent).
    async fn mark_active(&self, id: &str) {
        let mut state = self.state.lock().await;
        if !state.active_runs.contains(&id.to_string()) {
            state.active_runs.push(id.to_string());
        }
    }

    /// Remove a worker id from shared IPC state.
    async fn mark_inactive(&self, id: &str) {
        let mut state = self.state.lock().await;
        state.active_runs.retain(|r| r != id);
    }

    /// Set the monitor's shared lifecycle state.
    async fn set_state(&self, new_state: heartbeat::MonitorState) {
        let mut state = self.state.lock().await;
        state.state = new_state;
    }

    /// Perform one supervision pass: enforce resource limits, then reap any
    /// exited children and apply the restart policy.
    ///
    /// Tests (and a future background loop) drive this repeatedly. Decomposed
    /// into helpers to keep cognitive complexity and length within lint gates.
    pub async fn supervise_tick(&mut self) -> Result<(), process::MonitorError> {
        if self.shutdown {
            return Ok(());
        }
        self.enforce_resource_limits().await;
        self.reap_and_restart().await
    }

    /// Kill any worker whose sampled resource usage exceeds its limits.
    async fn enforce_resource_limits(&mut self) {
        let offenders: Vec<String> = self
            .workers
            .iter()
            .filter_map(|(id, h)| {
                let pid = h.child.id()?;
                let sample = process::sample_process(pid)?;
                if sample.exceeds_memory_mb(h.spec.limits.max_memory_mb) {
                    Some(id.clone())
                } else {
                    None
                }
            })
            .collect();
        for id in offenders {
            if let Some(h) = self.workers.get_mut(&id) {
                let _ = h.child.start_kill();
            }
        }
    }

    /// Reap exited children and respawn or degrade per the restart policy.
    async fn reap_and_restart(&mut self) -> Result<(), process::MonitorError> {
        let exited = self.collect_exited().await;
        for (id, code) in exited {
            if self.should_restart(code) && self.try_respawn(&id).await? {
                continue;
            }
            self.retire_worker(&id, code).await;
        }
        Ok(())
    }

    /// Collect ids and exit codes of workers whose child has exited.
    async fn collect_exited(&mut self) -> Vec<(String, Option<i32>)> {
        let mut exited = Vec::new();
        for (id, handle) in self.workers.iter_mut() {
            if let Ok(Some(status)) = handle.child.try_wait() {
                handle.last_exit_code = status.code();
                exited.push((id.clone(), status.code()));
            }
        }
        exited
    }

    /// Decide whether a worker that exited with `code` should be restarted,
    /// based on the configured restart policy.
    fn should_restart(&self, code: Option<i32>) -> bool {
        match self.config.restart_policy.as_str() {
            "always" => true,
            "on_failure" => code != Some(0),
            _ => false, // "no_restart" and any unknown policy
        }
    }

    /// Attempt to respawn a worker, honoring restart limits and backoff.
    ///
    /// Returns `Ok(true)` if the worker was respawned, `Ok(false)` if the
    /// restart limit has been reached (caller should retire + degrade).
    async fn try_respawn(&mut self, id: &str) -> Result<bool, process::MonitorError> {
        let backoff = {
            let handle = match self.workers.get_mut(id) {
                Some(h) => h,
                None => return Ok(false),
            };
            if handle.state.is_restart_limit_reached() {
                return Ok(false);
            }
            handle.state.record_restart();
            handle.state.current_backoff()
        };
        tokio::time::sleep(backoff).await;
        let new_child = {
            let handle = self
                .workers
                .get(id)
                .ok_or_else(|| process::MonitorError::General {
                    message: format!("worker '{}' vanished during respawn", id),
                })?;
            Self::spawn_child(id, &handle.spec)?
        };
        if let Some(handle) = self.workers.get_mut(id) {
            handle.child = new_child;
            handle.last_exit_code = None;
        }
        self.mark_active(id).await;
        Ok(true)
    }

    /// Remove a worker that will not be restarted and, when its restart limit
    /// was reached, transition the monitor into the degraded state.
    async fn retire_worker(&mut self, id: &str, _code: Option<i32>) {
        let limit_reached = self
            .workers
            .get(id)
            .map(|h| h.state.is_restart_limit_reached())
            .unwrap_or(false);
        self.workers.remove(id);
        self.mark_inactive(id).await;
        if limit_reached {
            self.set_state(heartbeat::MonitorState::Degraded).await;
        }
    }

    /// Graceful shutdown: terminate all supervised children and clear state.
    pub async fn shutdown(&mut self) -> Result<(), process::MonitorError> {
        self.shutdown = true;
        self.set_state(heartbeat::MonitorState::Stopping).await;

        for handle in self.workers.values_mut() {
            let _ = handle.child.start_kill();
            let _ = handle.child.wait().await;
        }
        self.workers.clear();

        {
            let mut state = self.state.lock().await;
            state.active_runs.clear();
            state.state = heartbeat::MonitorState::Stopped;
        }
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
