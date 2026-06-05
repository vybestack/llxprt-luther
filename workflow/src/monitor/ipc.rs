//! Monitor IPC management - Unix socket communication for status queries.
//!
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P10
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::monitor::heartbeat::{Heartbeat, HeartbeatError, MonitorState};
use crate::runtime_paths::get_data_dir;

/// Error type for IPC operations.
#[derive(Debug, Error)]
pub enum IpcError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Socket error: {0}")]
    Socket(String),
    #[error("Serialization error: {0}")]
    Serialization(String),
    #[error("Heartbeat error: {0}")]
    Heartbeat(#[from] HeartbeatError),
}

/// IPC request types.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum IpcRequest {
    #[serde(rename = "status")]
    Status { include_heartbeats: bool },
    #[serde(rename = "heartbeat")]
    Heartbeat { run_id: String },
    #[serde(rename = "shutdown")]
    Shutdown,
    #[serde(rename = "ping")]
    Ping,
}

/// IPC response types.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum IpcResponse {
    #[serde(rename = "status")]
    Status {
        instance_id: String,
        state: String,
        uptime_secs: i64,
        active_runs: Vec<String>,
    },
    #[serde(rename = "heartbeat")]
    Heartbeat(Heartbeat),
    #[serde(rename = "ok")]
    Ok,
    #[serde(rename = "error")]
    Error { code: String, message: String },
    #[serde(rename = "pong")]
    Pong { timestamp: i64 },
}

/// Create an IPC endpoint (Unix socket) at a unique path.
///
/// # Returns
/// Result containing the PathBuf to the created socket
///
/// # Errors
/// Returns IpcError if socket creation fails
pub fn create_ipc_endpoint() -> Result<PathBuf, IpcError> {
    let mut path = get_data_dir();
    path.push("ipc");

    // Create the IPC directory if it doesn't exist
    std::fs::create_dir_all(&path)?;

    // Use the instance ID in the socket name for uniqueness
    let instance_id = format!("luther-monitor-{}", std::process::id());
    path.push(format!("{}.sock", instance_id));

    // Remove old socket file if it exists
    if path.exists() {
        std::fs::remove_file(&path)?;
    }

    Ok(path)
}

/// Get the default IPC socket path.
pub fn get_default_ipc_path() -> PathBuf {
    let mut path = get_data_dir();
    path.push("ipc");
    path.push("luther-monitor.sock");
    path
}

/// Serve status requests on the given endpoint.
///
/// # Arguments
/// * `endpoint` - Path to the Unix socket
/// * `state` - Shared state containing the current monitor state
///
/// # Returns
/// JoinHandle for the server task
pub fn serve_status(
    endpoint: &Path,
    state: Arc<Mutex<SharedState>>,
) -> JoinHandle<Result<(), IpcError>> {
    let endpoint = endpoint.to_path_buf();

    tokio::spawn(async move {
        // Remove old socket file if it exists
        if endpoint.exists() {
            std::fs::remove_file(&endpoint)?;
        }

        let listener = UnixListener::bind(&endpoint)?;

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let state = state.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(stream, state).await {
                            eprintln!("IPC connection error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    eprintln!("IPC accept error: {}", e);
                    return Err(IpcError::Io(e));
                }
            }
        }
    })
}

/// Shared state for the IPC server.
pub struct SharedState {
    pub instance_id: String,
    pub state: MonitorState,
    pub uptime_secs: i64,
    pub active_runs: Vec<String>,
    pub heartbeats: HashMap<String, Heartbeat>,
}

impl SharedState {
    /// Create new shared state.
    pub fn new(instance_id: impl Into<String>) -> Self {
        Self {
            instance_id: instance_id.into(),
            state: MonitorState::Starting,
            uptime_secs: 0,
            active_runs: Vec::new(),
            heartbeats: HashMap::new(),
        }
    }

    /// Create new shared state with a specific state.
    #[must_use]
    pub fn with_state(mut self, state: MonitorState) -> Self {
        self.state = state;
        self
    }
}

/// Handle a single IPC connection.
async fn handle_connection(
    mut stream: UnixStream,
    state: Arc<Mutex<SharedState>>,
) -> Result<(), IpcError> {
    // Read the request (up to 4KB)
    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await?;

    if n == 0 {
        return Ok(());
    }

    buf.truncate(n);

    // Parse the request
    let request: IpcRequest = match serde_json::from_slice(&buf) {
        Ok(req) => req,
        Err(e) => {
            let response = IpcResponse::Error {
                code: "PARSE_ERROR".to_string(),
                message: e.to_string(),
            };
            send_response(&mut stream, &response).await?;
            return Ok(());
        }
    };

    // Handle the request
    let response = match request {
        IpcRequest::Status {
            include_heartbeats: _,
        } => {
            let state = state.lock().await;
            IpcResponse::Status {
                instance_id: state.instance_id.clone(),
                state: state.state.to_string(),
                uptime_secs: state.uptime_secs,
                active_runs: state.active_runs.clone(),
            }
        }
        IpcRequest::Heartbeat { run_id } => {
            let state = state.lock().await;
            match state.heartbeats.get(&run_id) {
                Some(hb) => IpcResponse::Heartbeat(hb.clone()),
                None => IpcResponse::Error {
                    code: "NOT_FOUND".to_string(),
                    message: format!("Heartbeat not found for run: {}", run_id),
                },
            }
        }
        IpcRequest::Shutdown => {
            // In a real implementation, this would trigger shutdown
            IpcResponse::Ok
        }
        IpcRequest::Ping => IpcResponse::Pong {
            timestamp: chrono::Utc::now().timestamp(),
        },
    };

    send_response(&mut stream, &response).await?;
    Ok(())
}

/// Send a response to the client.
async fn send_response(stream: &mut UnixStream, response: &IpcResponse) -> Result<(), IpcError> {
    let json = serde_json::to_vec(response).map_err(|e| IpcError::Serialization(e.to_string()))?;
    stream.write_all(&json).await?;
    stream.shutdown().await?;
    Ok(())
}

/// Connect to an IPC endpoint.
///
/// # Arguments
/// * `endpoint` - Path to the Unix socket
///
/// # Returns
/// Result containing the connected UnixStream
pub async fn connect_ipc(endpoint: &Path) -> Result<UnixStream, IpcError> {
    let stream = UnixStream::connect(endpoint).await?;
    Ok(stream)
}

/// Send a request and receive a response.
///
/// # Arguments
/// * `stream` - The connected stream
/// * `request` - The request to send
///
/// # Returns
/// Result containing the response
pub async fn send_request(
    stream: &mut UnixStream,
    request: &IpcRequest,
) -> Result<IpcResponse, IpcError> {
    // Send request
    let json = serde_json::to_vec(request).map_err(|e| IpcError::Serialization(e.to_string()))?;
    stream.write_all(&json).await?;
    stream.shutdown().await?;

    // Read response
    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await?;
    buf.truncate(n);

    let response =
        serde_json::from_slice(&buf).map_err(|e| IpcError::Serialization(e.to_string()))?;

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_ipc_endpoint_creation() {
        let path = create_ipc_endpoint().expect("Should create endpoint");
        let path_str = path.to_string_lossy();
        assert!(path_str.contains("ipc"));
        assert!(path_str.contains("luther-monitor"));
        assert!(path_str.contains(".sock"));
    }

    #[test]
    fn test_ipc_request_serialization() {
        let req = IpcRequest::Status {
            include_heartbeats: true,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("status"));
        assert!(json.contains("include_heartbeats"));

        let deserialized: IpcRequest = serde_json::from_str(&json).unwrap();
        match deserialized {
            IpcRequest::Status { include_heartbeats } => {
                assert!(include_heartbeats);
            }
            _ => panic!("Wrong request type"),
        }
    }

    #[test]
    fn test_ipc_response_serialization() {
        let resp = IpcResponse::Pong { timestamp: 12345 };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("pong"));
        assert!(json.contains("12345"));

        let deserialized: IpcResponse = serde_json::from_str(&json).unwrap();
        match deserialized {
            IpcResponse::Pong { timestamp } => {
                assert_eq!(timestamp, 12345);
            }
            _ => panic!("Wrong response type"),
        }
    }

    #[test]
    fn test_shared_state() {
        let state = SharedState::new("test-instance").with_state(MonitorState::Running);

        assert_eq!(state.instance_id, "test-instance");
        assert_eq!(state.state, MonitorState::Running);
    }
}
