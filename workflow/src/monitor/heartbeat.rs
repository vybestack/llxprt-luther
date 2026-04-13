//! Monitor heartbeat management - state persistence and health tracking.
//!
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P10

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use thiserror::Error;
use tokio::fs;
use tokio::sync::Mutex;
use std::sync::Arc;

use crate::runtime_paths::get_data_dir;

/// Monitor state enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MonitorState {
    Starting,
    Running,
    Degraded,
    Stopping,
    Stopped,
    Error,
}

impl std::fmt::Display for MonitorState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MonitorState::Starting => write!(f, "starting"),
            MonitorState::Running => write!(f, "running"),
            MonitorState::Degraded => write!(f, "degraded"),
            MonitorState::Stopping => write!(f, "stopping"),
            MonitorState::Stopped => write!(f, "stopped"),
            MonitorState::Error => write!(f, "error"),
        }
    }
}

/// Heartbeat metadata from monitor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Heartbeat {
    /// Unique instance identifier
    pub instance_id: String,
    /// Timestamp when heartbeat was generated (Unix epoch seconds)
    pub timestamp: i64,
    /// Uptime in seconds
    pub uptime_secs: i64,
    /// Version identifier
    pub version: i32,
    /// Current monitor state
    pub state: MonitorState,
    /// Active worker count
    pub active_workers: u32,
    /// Run ID if applicable
    pub run_id: Option<String>,
    /// Additional metadata
    pub metadata: HashMap<String, String>,
}

impl Heartbeat {
    /// Create a new heartbeat with the given instance ID and run ID.
    pub fn new(instance_id: impl Into<String>) -> Self {
        Self {
            instance_id: instance_id.into(),
            timestamp: Utc::now().timestamp(),
            uptime_secs: 0,
            version: 1,
            state: MonitorState::Starting,
            active_workers: 0,
            run_id: None,
            metadata: HashMap::new(),
        }
    }

    /// Set the run ID.
    #[must_use]
    pub fn with_run_id(mut self, run_id: impl Into<String>) -> Self {
        self.run_id = Some(run_id.into());
        self
    }

    /// Set the state.
    #[must_use]
    pub fn with_state(mut self, state: MonitorState) -> Self {
        self.state = state;
        self
    }

    /// Set the uptime.
    #[must_use]
    pub fn with_uptime(mut self, uptime_secs: i64) -> Self {
        self.uptime_secs = uptime_secs;
        self
    }

    /// Set active workers.
    #[must_use]
    pub fn with_active_workers(mut self, count: u32) -> Self {
        self.active_workers = count;
        self
    }
}

/// Error type for heartbeat operations.
#[derive(Debug, Error)]
pub enum HeartbeatError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serialization(String),
    #[error("Heartbeat not found for run: {0}")]
    NotFound(String),
}

/// Get the path to the heartbeat file for a given run ID.
fn get_heartbeat_path(run_id: &str) -> PathBuf {
    let mut path = get_data_dir();
    path.push("heartbeats");
    path.push(format!("{}.json", run_id));
    path
}

/// Write heartbeat to disk for a specific run.
///
/// # Arguments
/// * `run_id` - The run identifier
/// * `state` - The current monitor state to record
///
/// # Returns
/// Result indicating success or failure
pub async fn write_heartbeat(run_id: &str, state: &MonitorState) -> Result<(), HeartbeatError> {
    let heartbeat = Heartbeat::new(format!("monitor-{}", run_id))
        .with_run_id(run_id)
        .with_state(*state);

    write_heartbeat_full(run_id, &heartbeat).await
}

/// Write a complete heartbeat to disk.
///
/// # Arguments
/// * `run_id` - The run identifier
/// * `heartbeat` - The full heartbeat to write
///
/// # Returns
/// Result indicating success or failure
pub async fn write_heartbeat_full(run_id: &str, heartbeat: &Heartbeat) -> Result<(), HeartbeatError> {
    let path = get_heartbeat_path(run_id);

    // Ensure directory exists
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let json = serde_json::to_string_pretty(heartbeat)
        .map_err(|e| HeartbeatError::Serialization(e.to_string()))?;

    fs::write(&path, json).await?;
    Ok(())
}

/// Read the most recent heartbeat from disk.
///
/// # Arguments
/// * `run_id` - The run identifier
///
/// # Returns
/// Result containing Some(Heartbeat) if found, None if not found
pub async fn read_heartbeat(run_id: &str) -> Result<Option<Heartbeat>, HeartbeatError> {
    let path = get_heartbeat_path(run_id);

    match fs::read_to_string(&path).await {
        Ok(content) => {
            let heartbeat: Heartbeat = serde_json::from_str(&content)
                .map_err(|e| HeartbeatError::Serialization(e.to_string()))?;
            Ok(Some(heartbeat))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(HeartbeatError::Io(e)),
    }
}

/// Read all heartbeats from disk.
///
/// # Returns
/// Result containing a map of run_id -> Heartbeat
pub async fn read_all_heartbeats() -> Result<HashMap<String, Heartbeat>, HeartbeatError> {
    let mut heartbeats = HashMap::new();
    let data_dir = get_data_dir();
    let heartbeats_dir = data_dir.join("heartbeats");

    if !heartbeats_dir.exists() {
        return Ok(heartbeats);
    }

    let mut entries = fs::read_dir(&heartbeats_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if let Some(ext) = path.extension() {
            if ext == "json" {
                if let Some(stem) = path.file_stem() {
                    let run_id = stem.to_string_lossy().to_string();
                    if let Ok(Some(heartbeat)) = read_heartbeat(&run_id).await {
                        heartbeats.insert(run_id, heartbeat);
                    }
                }
            }
        }
    }

    Ok(heartbeats)
}

/// Delete a heartbeat file for a given run ID.
///
/// # Arguments
/// * `run_id` - The run identifier
///
/// # Returns
/// Result indicating success or failure
pub async fn delete_heartbeat(run_id: &str) -> Result<(), HeartbeatError> {
    let path = get_heartbeat_path(run_id);
    match fs::remove_file(&path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(HeartbeatError::Io(e)),
    }
}

/// Heartbeat writer that maintains state and periodically writes heartbeats.
pub struct HeartbeatWriter {
    run_id: String,
    instance_id: String,
    start_time: DateTime<Utc>,
    state: Arc<Mutex<MonitorState>>,
    active_workers: Arc<Mutex<u32>>,
}

impl HeartbeatWriter {
    /// Create a new heartbeat writer.
    pub fn new(run_id: impl Into<String>, instance_id: impl Into<String>) -> Self {
        Self {
            run_id: run_id.into(),
            instance_id: instance_id.into(),
            start_time: Utc::now(),
            state: Arc::new(Mutex::new(MonitorState::Starting)),
            active_workers: Arc::new(Mutex::new(0)),
        }
    }

    /// Update the monitor state.
    pub async fn set_state(&self, state: MonitorState) {
        *self.state.lock().await = state;
    }

    /// Set active worker count.
    pub async fn set_active_workers(&self, count: u32) {
        *self.active_workers.lock().await = count;
    }

    /// Write current heartbeat to disk.
    pub async fn write(&self) -> Result<(), HeartbeatError> {
        let uptime = Utc::now().signed_duration_since(self.start_time);
        
        let heartbeat = Heartbeat {
            instance_id: self.instance_id.clone(),
            timestamp: Utc::now().timestamp(),
            uptime_secs: uptime.num_seconds(),
            version: 1,
            state: *self.state.lock().await,
            active_workers: *self.active_workers.lock().await,
            run_id: Some(self.run_id.clone()),
            metadata: HashMap::new(),
        };

        write_heartbeat_full(&self.run_id, &heartbeat).await
    }

    /// Start periodic heartbeat writing with the given interval.
    pub fn start_periodic(self, interval_secs: u64) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(interval_secs));
            loop {
                interval.tick().await;
                if let Err(e) = self.write().await {
                    eprintln!("Failed to write heartbeat: {}", e);
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_test_dir() {
        // Note: This would require refactoring to inject paths for testing
        // For now, we test the core logic without file operations
    }

    #[test]
    fn test_heartbeat_creation() {
        let hb = Heartbeat::new("test-instance")
            .with_run_id("run-001")
            .with_state(MonitorState::Running)
            .with_uptime(123)
            .with_active_workers(5);

        assert_eq!(hb.instance_id, "test-instance");
        assert_eq!(hb.run_id, Some("run-001".to_string()));
        assert_eq!(hb.state, MonitorState::Running);
        assert_eq!(hb.uptime_secs, 123);
        assert_eq!(hb.active_workers, 5);
        assert!(hb.timestamp > 0);
    }

    #[test]
    fn test_heartbeat_serialization() {
        let hb = Heartbeat::new("test-instance")
            .with_run_id("run-001")
            .with_state(MonitorState::Running);

        let json = serde_json::to_string_pretty(&hb).unwrap();
        let deserialized: Heartbeat = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.instance_id, hb.instance_id);
        assert_eq!(deserialized.run_id, hb.run_id);
        assert_eq!(deserialized.state, hb.state);
    }

    #[test]
    fn test_get_heartbeat_path() {
        let path = get_heartbeat_path("test-run-123");
        let path_str = path.to_string_lossy();
        assert!(path_str.contains("heartbeats"));
        assert!(path_str.contains("test-run-123.json"));
    }
}
