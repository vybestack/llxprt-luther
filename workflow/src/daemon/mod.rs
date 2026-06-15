//! Per-config daemon lifecycle state and persistence.
//!
//! Issue #48 introduces a first-class `daemon` command family that supervises
//! one foreground daemon instance per workflow config while allowing an
//! aggregate CLI view across configs. This module owns the persistent state
//! model (`DaemonState`/`DaemonStatus`), the on-disk path layout and
//! persistence (`DaemonStore`), and a cross-platform liveness check
//! (`is_daemon_alive`).
//!
//! The store is parameterized over a root directory so production uses
//! [`crate::runtime_paths::get_daemons_root`] while tests inject an isolated
//! temporary directory; `get_data_dir()` has no environment override, so an
//! injectable root is the only way to keep persistence unit-testable.
//!
//! @plan:PLAN-20260404-INITIAL-RUNTIME.P09
//! @requirement:REQ-EARS-SVC-001
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[cfg(not(target_os = "linux"))]
use std::process::Command;

/// Lifecycle status of a per-config daemon instance.
///
/// Mirrors the `MonitorState` style in `monitor/heartbeat.rs` but is scoped to
/// the daemon supervisor states required by issue #48.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DaemonStatus {
    /// Daemon is starting up but not yet fully running.
    Starting,
    /// Daemon is running and emitting heartbeats.
    Running,
    /// Daemon received a shutdown signal and is winding down.
    Stopping,
    /// Daemon has exited cleanly.
    Stopped,
}

impl std::fmt::Display for DaemonStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DaemonStatus::Starting => write!(f, "starting"),
            DaemonStatus::Running => write!(f, "running"),
            DaemonStatus::Stopping => write!(f, "stopping"),
            DaemonStatus::Stopped => write!(f, "stopped"),
        }
    }
}

/// Persistent state for a single per-config daemon instance.
///
/// Serialized to `<root>/<config_id>/state.json` so other CLI commands can
/// build an aggregate view across configs.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonState {
    /// Workflow config identifier (config file stem).
    pub config_id: String,
    /// Operating-system process id of the daemon.
    pub pid: u32,
    /// Unix epoch seconds when the daemon started.
    pub start_timestamp: i64,
    /// Current lifecycle status.
    pub status: DaemonStatus,
    /// Unix epoch seconds of the most recent heartbeat.
    pub heartbeat_timestamp: i64,
    /// Schema version for forward compatibility.
    pub version: u32,
}

impl DaemonState {
    /// Create a new `Starting` state for `config_id` using the current process.
    pub fn new(config_id: impl Into<String>) -> Self {
        let now = Utc::now().timestamp();
        Self {
            config_id: config_id.into(),
            pid: std::process::id(),
            start_timestamp: now,
            status: DaemonStatus::Starting,
            heartbeat_timestamp: now,
            version: 1,
        }
    }

    /// Override the recorded process id (used by tests with mock daemons).
    #[must_use]
    pub fn with_pid(mut self, pid: u32) -> Self {
        self.pid = pid;
        self
    }

    /// Set the lifecycle status (builder form).
    #[must_use]
    pub fn with_status(mut self, status: DaemonStatus) -> Self {
        self.status = status;
        self
    }

    /// Transition the lifecycle status in place.
    pub fn set_status(&mut self, status: DaemonStatus) {
        self.status = status;
    }

    /// Refresh the heartbeat timestamp to now.
    pub fn touch_heartbeat(&mut self) {
        self.heartbeat_timestamp = Utc::now().timestamp();
    }

    /// Uptime in seconds relative to `now` (clamped at zero).
    #[must_use]
    pub fn uptime_secs(&self, now: i64) -> i64 {
        (now - self.start_timestamp).max(0)
    }
}

/// On-disk layout and persistence for per-config daemon state.
///
/// Production callers use [`DaemonStore::production`]; tests use
/// [`DaemonStore::at`] with a temporary directory for isolation.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
#[derive(Debug, Clone)]
pub struct DaemonStore {
    root: PathBuf,
}

impl DaemonStore {
    /// Construct a store rooted at the production daemons directory.
    #[must_use]
    pub fn production() -> Self {
        Self {
            root: crate::runtime_paths::get_daemons_root(),
        }
    }

    /// Construct a store rooted at an arbitrary directory (tests).
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Root directory backing this store.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Per-config directory `<root>/<config_id>/`.
    #[must_use]
    pub fn dir(&self, config_id: &str) -> PathBuf {
        self.root.join(config_id)
    }

    /// Per-config state file `<root>/<config_id>/state.json`.
    #[must_use]
    pub fn state_path(&self, config_id: &str) -> PathBuf {
        self.dir(config_id).join("state.json")
    }

    /// Per-config lock file `<root>/<config_id>/daemon.lock`.
    ///
    /// The `.lock` suffix makes `acquire_singleton_lock` treat the value as a
    /// full path rather than a `/tmp` scope name.
    #[must_use]
    pub fn lock_path(&self, config_id: &str) -> PathBuf {
        self.dir(config_id).join("daemon.lock")
    }

    /// Persist `state` as pretty JSON, creating parent directories as needed.
    ///
    /// # Errors
    /// Returns an I/O error if directories cannot be created or the file cannot
    /// be written, or a serialization error.
    pub fn write(&self, state: &DaemonState) -> std::io::Result<()> {
        let dir = self.dir(&state.config_id);
        std::fs::create_dir_all(&dir)?;
        let json = serde_json::to_string_pretty(state)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(self.state_path(&state.config_id), json)
    }

    /// Read the state for `config_id`, returning `None` when absent or
    /// unreadable/malformed.
    #[must_use]
    pub fn read(&self, config_id: &str) -> Option<DaemonState> {
        let contents = std::fs::read_to_string(self.state_path(config_id)).ok()?;
        serde_json::from_str(&contents).ok()
    }

    /// Read every persisted daemon state, sorted by `config_id` for
    /// deterministic output. Unreadable or malformed entries are skipped.
    #[must_use]
    pub fn read_all(&self) -> Vec<DaemonState> {
        let mut states = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return states;
        };
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if let Some(state) = self.read(&name) {
                states.push(state);
            }
        }
        states.sort_by(|a, b| a.config_id.cmp(&b.config_id));
        states
    }

    /// Remove only the `state.json` for `config_id` (never the directory tree).
    ///
    /// Not called on stop; provided for optional cleanup. Absent files are
    /// treated as success.
    ///
    /// # Errors
    /// Returns an I/O error only for failures other than a missing file.
    pub fn delete(&self, config_id: &str) -> std::io::Result<()> {
        match std::fs::remove_file(self.state_path(config_id)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// Outcome of attempting to stop a single daemon instance.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// A live daemon was signalled and terminated.
    Stopped,
    /// The recorded daemon was already dead (idempotent).
    AlreadyStopped,
    /// No state file existed for the config (idempotent).
    NotFound,
}

/// Terminate `pid` gracefully, escalating to SIGKILL if it does not exit.
///
/// Sends `SIGTERM`, polls liveness for roughly two seconds, then sends
/// `SIGKILL` if the process is still alive. Uses `kill` via
/// `std::process::Command` to avoid a `libc` dependency, consistent with the
/// existing `kill -0` checks in `monitor/process.rs`.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
#[cfg(unix)]
pub fn terminate_pid(pid: u32) {
    let _ = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .output();
    for _ in 0..20 {
        if !is_daemon_alive(pid) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .output();
}

/// Stop the daemon recorded for `config_id` in `store`.
///
/// Reads the persisted state, signals a live process, and reports the outcome.
/// The `state.json` file is intentionally **not** deleted so other commands can
/// still observe the last known state, and so workspaces/artifacts/logs are
/// never affected. Stopping an absent or already-dead daemon is idempotent.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
#[cfg(unix)]
#[must_use]
pub fn stop_daemon(store: &DaemonStore, config_id: &str) -> StopOutcome {
    let Some(state) = store.read(config_id) else {
        return StopOutcome::NotFound;
    };
    if !is_daemon_alive(state.pid) {
        return StopOutcome::AlreadyStopped;
    }
    terminate_pid(state.pid);
    StopOutcome::Stopped
}

/// Check whether a process with `pid` is currently alive.
///
/// On Linux this checks `/proc/<pid>`; on other Unix targets (notably macOS,
/// which has no `/proc`) it sends signal 0 via `kill -0`, matching the
/// lock-file liveness check in `monitor/process.rs`.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
#[must_use]
pub fn is_daemon_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn daemon_state_roundtrips_json() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P09
        let state = DaemonState::new("llxprt-code")
            .with_pid(4242)
            .with_status(DaemonStatus::Running);
        let json = serde_json::to_string(&state).expect("serialize");
        let parsed: DaemonState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(state, parsed);
        assert_eq!(parsed.config_id, "llxprt-code");
        assert_eq!(parsed.pid, 4242);
        assert_eq!(parsed.status, DaemonStatus::Running);
        assert_eq!(parsed.version, 1);
    }

    #[test]
    fn daemon_status_display_strings() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P09
        assert_eq!(DaemonStatus::Starting.to_string(), "starting");
        assert_eq!(DaemonStatus::Running.to_string(), "running");
        assert_eq!(DaemonStatus::Stopping.to_string(), "stopping");
        assert_eq!(DaemonStatus::Stopped.to_string(), "stopped");
    }

    #[test]
    fn store_paths_are_per_config() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P09
        let store = DaemonStore::at("/tmp/daemons-root");
        assert_eq!(store.dir("alpha"), PathBuf::from("/tmp/daemons-root/alpha"));
        assert_eq!(
            store.state_path("alpha"),
            PathBuf::from("/tmp/daemons-root/alpha/state.json")
        );
        let lock = store.lock_path("alpha");
        assert_eq!(lock, PathBuf::from("/tmp/daemons-root/alpha/daemon.lock"));
        assert!(lock.to_string_lossy().ends_with(".lock"));
    }

    #[test]
    fn read_missing_returns_none() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P09
        let tmp = TempDir::new().expect("tempdir");
        let store = DaemonStore::at(tmp.path());
        assert!(store.read("absent").is_none());
        assert!(store.read_all().is_empty());
    }

    #[test]
    fn write_then_read_roundtrips_via_store() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P09
        let tmp = TempDir::new().expect("tempdir");
        let store = DaemonStore::at(tmp.path());
        let state = DaemonState::new("cfg-a").with_status(DaemonStatus::Running);
        store.write(&state).expect("write");
        let read = store.read("cfg-a").expect("present");
        assert_eq!(read, state);
    }

    #[test]
    fn read_all_is_sorted_and_independent() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P09
        let tmp = TempDir::new().expect("tempdir");
        let store = DaemonStore::at(tmp.path());
        for id in ["charlie", "alpha", "bravo"] {
            store.write(&DaemonState::new(id)).expect("write");
        }
        let all = store.read_all();
        let ids: Vec<&str> = all.iter().map(|s| s.config_id.as_str()).collect();
        assert_eq!(ids, vec!["alpha", "bravo", "charlie"]);
    }

    #[test]
    fn delete_only_removes_state_file() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P09
        let tmp = TempDir::new().expect("tempdir");
        let store = DaemonStore::at(tmp.path());
        store.write(&DaemonState::new("cfg")).expect("write");
        store.delete("cfg").expect("delete");
        assert!(store.read("cfg").is_none());
        assert!(store.dir("cfg").exists());
        // Deleting again is idempotent.
        store.delete("cfg").expect("idempotent delete");
    }

    #[test]
    fn is_daemon_alive_self_is_true() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P09
        assert!(is_daemon_alive(std::process::id()));
    }

    #[test]
    fn is_daemon_alive_unused_pid_is_false() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P09
        // A very high PID is extremely unlikely to be live.
        assert!(!is_daemon_alive(4_000_000_000));
    }

    #[test]
    fn uptime_is_clamped_and_computed() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P09
        let mut state = DaemonState::new("cfg");
        state.start_timestamp = 1_000;
        assert_eq!(state.uptime_secs(1_100), 100);
        assert_eq!(state.uptime_secs(500), 0);
    }
}
