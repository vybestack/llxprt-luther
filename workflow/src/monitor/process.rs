//! Monitor process management - singleton locks and process lifecycle.
//!
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P10

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use thiserror::Error;

/// Configuration for monitor process behavior.
#[derive(Debug, Clone)]
pub struct MonitorConfig {
    /// Restart policy: no_restart, always, on_failure
    pub restart_policy: String,
    /// Backoff strategy: fixed, exponential
    pub backoff_strategy: String,
    /// Initial backoff in seconds
    pub initial_backoff_secs: u64,
    /// Maximum backoff in seconds
    pub max_backoff_secs: u64,
    /// Maximum number of restart attempts
    pub max_restarts: u32,
    /// Enable singleton mode (only one monitor instance)
    pub singleton_mode: bool,
    /// Path for PID/lock file
    pub lock_file_path: Option<String>,
    /// Heartbeat interval in seconds (for compatibility with tests)
    pub heartbeat_interval_secs: u64,
    /// Maximum missed heartbeats before considering monitor dead (for compatibility with tests)
    pub max_missed_heartbeats: u32,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            restart_policy: "on_failure".to_string(),
            backoff_strategy: "exponential".to_string(),
            initial_backoff_secs: 1,
            max_backoff_secs: 60,
            max_restarts: 5,
            singleton_mode: true,
            lock_file_path: None,
            heartbeat_interval_secs: 1,
            max_missed_heartbeats: 3,
        }
    }
}

/// Error type for monitor operations.
#[derive(Debug, Error, Clone)]
pub enum MonitorError {
    #[error("Lock acquisition failed: {message}")]
    LockError { message: String },
    #[error("Lock held by another process (PID: {pid})")]
    LockHeld { pid: u32 },
    #[error("Monitor error: {message}")]
    General { message: String },
    #[error("Heartbeat error: {message}")]
    HeartbeatError { message: String },
    #[error("IPC error: {message}")]
    IpcError { message: String },
    #[error("Monitor already running")]
    AlreadyRunning,
}

/// Guard type for singleton lock - ensures lock is released on drop.
#[derive(Debug)]
pub struct SingletonGuard {
    lock_path: String,
    released: Arc<Mutex<bool>>,
}

impl SingletonGuard {
    /// Get the path of the lock file.
    pub fn lock_path(&self) -> &str {
        &self.lock_path
    }

    /// Check if the lock has been released.
    pub async fn is_released(&self) -> bool {
        *self.released.lock().await
    }
}

impl Drop for SingletonGuard {
    fn drop(&mut self) {
        // Remove the lock file synchronously
        let _ = fs::remove_file(&self.lock_path);
    }
}

/// Acquire a singleton lock for the given scope.
///
/// # Arguments
/// * `scope` - A unique scope identifier for the lock (e.g., "luther-monitor")
///   Can also be a full path ending in `.lock` (e.g., "/tmp/luther-test.lock")
///
/// # Returns
/// Result containing the singleton guard if lock is acquired
///
/// # Errors
/// Returns MonitorError::LockHeld if another process holds the lock
pub fn acquire_singleton_lock(scope: &str) -> Result<SingletonGuard, MonitorError> {
    // Determine the lock path - if it ends with .lock, treat as full path
    let lock_path = if scope.ends_with(".lock") {
        scope.to_string()
    } else {
        format!("/tmp/{}.lock", scope.replace("/", "_"))
    };
    let lock_file = Path::new(&lock_path);

    // Create parent directory if needed
    if let Some(parent) = lock_file.parent() {
        let _ = fs::create_dir_all(parent);
    }

    // Check if lock file exists
    if lock_file.exists() {
        let contents = fs::read_to_string(lock_file).map_err(|e| MonitorError::LockError {
            message: format!("Failed to read lock file: {}", e),
        })?;

        let pid: u32 = contents.trim().parse().map_err(|_| MonitorError::LockError {
            message: "Lock file contains invalid PID".to_string(),
        })?;

        // Check if process is still alive
        // On Linux, check /proc/PID
        // On macOS and other Unix systems, try to send signal 0 to check if process exists
        #[cfg(target_os = "linux")]
        let process_exists = Path::new(&format!("/proc/{}", pid)).exists();
        
        #[cfg(not(target_os = "linux"))]
        let process_exists = {
            Command::new("kill")
                .args(["-0", &pid.to_string()])
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false)
        };

        // If the lock is held by a DIFFERENT process, we cannot acquire it
        if process_exists && pid != std::process::id() {
            return Err(MonitorError::LockHeld { pid });
        }
        
        // If the lock is held by THIS process, we also cannot acquire it again
        // (this handles the test case of starting two monitors in the same process)
        if pid == std::process::id() {
            return Err(MonitorError::LockHeld { pid: std::process::id() });
        }

        // Process is dead, we can steal the lock
    }

    // Write our PID to lock file
    fs::write(lock_file, std::process::id().to_string()).map_err(|e| MonitorError::LockError {
        message: format!("Failed to write lock file: {}", e),
    })?;

    Ok(SingletonGuard {
        lock_path,
        released: Arc::new(Mutex::new(false)),
    })
}

/// Release a singleton lock explicitly.
///
/// # Arguments
/// * `guard` - The singleton guard to release
///
/// Note: This is typically called automatically when the guard is dropped,
/// but can be called explicitly for explicit control.
pub fn release_singleton_lock(guard: SingletonGuard) {
    let _ = fs::remove_file(&guard.lock_path);
    // Guard is dropped here, which will also attempt cleanup
}

/// Calculate backoff delay based on strategy and attempt count.
///
/// # Arguments
/// * `strategy` - The backoff strategy ("fixed" or "exponential")
/// * `initial_secs` - Initial backoff in seconds
/// * `max_secs` - Maximum backoff in seconds
/// * `attempt` - The attempt number (1-indexed)
///
/// # Returns
/// Duration for the backoff delay
pub fn calculate_backoff(
    strategy: &str,
    initial_secs: u64,
    max_secs: u64,
    attempt: u32,
) -> Duration {
    match strategy {
        "exponential" => {
            let delay = initial_secs * 2_u64.pow(attempt.saturating_sub(1));
            Duration::from_secs(delay.min(max_secs))
        }
        _ => Duration::from_secs(initial_secs),
    }
}

/// Process state for tracking restart attempts.
#[derive(Debug)]
pub struct ProcessState {
    restart_count: u32,
    last_restart: Option<std::time::Instant>,
    config: MonitorConfig,
}

impl ProcessState {
    /// Create a new process state with the given configuration.
    pub fn new(config: MonitorConfig) -> Self {
        Self {
            restart_count: 0,
            last_restart: None,
            config,
        }
    }

    /// Record a restart attempt.
    pub fn record_restart(&mut self) {
        self.restart_count += 1;
        self.last_restart = Some(std::time::Instant::now());
    }

    /// Get the current restart count.
    pub fn restart_count(&self) -> u32 {
        self.restart_count
    }

    /// Check if restart limit has been reached.
    pub fn is_restart_limit_reached(&self) -> bool {
        self.restart_count >= self.config.max_restarts
    }

    /// Calculate the current backoff duration.
    pub fn current_backoff(&self) -> Duration {
        calculate_backoff(
            &self.config.backoff_strategy,
            self.config.initial_backoff_secs,
            self.config.max_backoff_secs,
            self.restart_count,
        )
    }

    /// Get the time since last restart.
    pub fn time_since_last_restart(&self) -> Option<Duration> {
        self.last_restart.map(|t| t.elapsed())
    }
}

/// Check if a process with the given PID is alive.
#[cfg(unix)]
pub fn is_process_alive(pid: u32) -> bool {
    // On Unix, check if /proc/PID exists
    Path::new(&format!("/proc/{}", pid)).exists()
}

#[cfg(not(unix))]
pub fn is_process_alive(_pid: u32) -> bool {
    // On non-Unix systems, assume process exists
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_singleton_lock_acquire_and_release() {
        let scope = "test-scope-1";
        let lock_path = format!("/tmp/{}.lock", scope);

        // Clean up any existing lock
        let _ = fs::remove_file(&lock_path);

        // Acquire lock
        let guard = acquire_singleton_lock(scope).expect("Should acquire lock");
        assert!(Path::new(&lock_path).exists());

        // Try to acquire again (should fail)
        let result = acquire_singleton_lock(scope);
        assert!(matches!(result, Err(MonitorError::LockHeld { .. })));

        // Release lock
        release_singleton_lock(guard);
        thread::sleep(Duration::from_millis(100));
        assert!(!Path::new(&lock_path).exists());
    }

    #[test]
    fn test_calculate_backoff_fixed() {
        let delay = calculate_backoff("fixed", 5, 60, 5);
        assert_eq!(delay, Duration::from_secs(5));
    }

    #[test]
    fn test_calculate_backoff_exponential() {
        assert_eq!(calculate_backoff("exponential", 1, 60, 1), Duration::from_secs(1));
        assert_eq!(calculate_backoff("exponential", 1, 60, 2), Duration::from_secs(2));
        assert_eq!(calculate_backoff("exponential", 1, 60, 3), Duration::from_secs(4));
        assert_eq!(calculate_backoff("exponential", 1, 60, 6), Duration::from_secs(32));
        assert_eq!(calculate_backoff("exponential", 1, 60, 10), Duration::from_secs(60)); // Capped at max
    }

    #[test]
    fn test_process_state_restart_tracking() {
        let config = MonitorConfig {
            max_restarts: 3,
            backoff_strategy: "exponential".to_string(),
            initial_backoff_secs: 1,
            max_backoff_secs: 10,
            ..Default::default()
        };

        let mut state = ProcessState::new(config);
        assert_eq!(state.restart_count(), 0);
        assert!(!state.is_restart_limit_reached());

        state.record_restart();
        assert_eq!(state.restart_count(), 1);

        state.record_restart();
        state.record_restart();
        assert_eq!(state.restart_count(), 3);
        assert!(state.is_restart_limit_reached());

        assert_eq!(state.current_backoff(), Duration::from_secs(4)); // 1 * 2^2
    }
}
