/// @plan:PLAN-20260404-INITIAL-RUNTIME.P06
/// Artifact persistence - writes and manages per-run output files.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::persistence::checkpoint::PersistenceError;

/// Record of a persisted artifact for a workflow run.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P06
/// @requirement:REQ-EARS-PERSIST-003
#[derive(Debug, Clone)]
pub struct ArtifactRecord {
    /// The run_id this artifact belongs to.
    pub run_id: String,
    /// Absolute path to the artifact file.
    pub artifact_path: PathBuf,
    /// Kind/type of artifact (e.g., "log", "output", "trace").
    pub artifact_kind: String,
    /// The step_id that produced this artifact.
    pub step_id: String,
    /// When the artifact was written.
    pub created_at: DateTime<Utc>,
    /// Size in bytes (if known).
    pub size_bytes: Option<u64>,
}

impl ArtifactRecord {
    /// Create a new artifact record.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P06
    pub fn new(
        run_id: impl Into<String>,
        artifact_path: impl Into<PathBuf>,
        artifact_kind: impl Into<String>,
        step_id: impl Into<String>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            artifact_path: artifact_path.into(),
            artifact_kind: artifact_kind.into(),
            step_id: step_id.into(),
            created_at: Utc::now(),
            size_bytes: None,
        }
    }

    /// Set the size in bytes.
    pub fn with_size(mut self, bytes: u64) -> Self {
        self.size_bytes = Some(bytes);
        self
    }
}

/// Get the artifacts directory for a run.
/// Returns a deterministic path based on the run_id.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P06
/// @requirement:REQ-EARS-PERSIST-003
pub fn get_artifacts_dir(
    artifacts_root: impl AsRef<Path>,
    run_id: &str,
) -> PathBuf {
    // Deterministic path: <artifacts_root>/<run_id>/
    artifacts_root.as_ref().join(run_id)
}

/// Write an artifact file for a run.
/// Creates the run-scoped directory if needed and writes the content.
/// Returns the path to the written file.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P06
/// @requirement:REQ-EARS-PERSIST-003,REQ-EARS-PERSIST-004
pub fn write_artifact(
    run_id: &str,
    name: &str,
    content: &[u8],
) -> Result<PathBuf, PersistenceError> {
    let artifacts_root = default_artifacts_root();
    let run_dir = get_artifacts_dir(&artifacts_root, run_id);

    // Create the run-scoped directory if it doesn't exist
    std::fs::create_dir_all(&run_dir)?;

    // Construct the artifact path
    let artifact_path = run_dir.join(name);

    // Write the content to the file
    std::fs::write(&artifact_path, content)?;

    Ok(artifact_path)
}

/// Write an artifact with a specific kind/type annotation.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P06
pub fn write_artifact_with_kind(
    run_id: &str,
    name: &str,
    content: &[u8],
    artifact_kind: &str,
    step_id: &str,
) -> Result<ArtifactRecord, PersistenceError> {
    let artifacts_root = default_artifacts_root();
    let run_dir = get_artifacts_dir(&artifacts_root, run_id);

    // Create the run-scoped directory if it doesn't exist
    std::fs::create_dir_all(&run_dir)?;

    // Construct the artifact path
    let artifact_path = run_dir.join(name);

    // Write the content to the file
    std::fs::write(&artifact_path, content)?;

    // Get the file size
    let size_bytes = std::fs::metadata(&artifact_path).ok().map(|m| m.len());

    // Create and return the artifact record
    let record = ArtifactRecord::new(run_id, artifact_path, artifact_kind, step_id)
        .with_size(size_bytes.unwrap_or(0));

    Ok(record)
}

/// Read an artifact file for a run.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P06
pub fn read_artifact(
    run_id: &str,
    name: &str,
) -> Result<Vec<u8>, PersistenceError> {
    let artifacts_root = default_artifacts_root();
    let artifact_path = get_artifacts_dir(&artifacts_root, run_id).join(name);

    // Read the file content
    let content = std::fs::read(&artifact_path)?;

    Ok(content)
}

/// List all artifacts for a run.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P06
pub fn list_artifacts(run_id: &str) -> Result<Vec<ArtifactRecord>, PersistenceError> {
    let artifacts_root = default_artifacts_root();
    let run_dir = get_artifacts_dir(&artifacts_root, run_id);

    // Check if the directory exists
    if !run_dir.exists() {
        return Ok(Vec::new());
    }

    let mut artifacts = Vec::new();

    // Read directory entries
    for entry in std::fs::read_dir(&run_dir)? {
        let entry = entry?;
        let path = entry.path();

        // Only include files
        if path.is_file() {
            let size_bytes = entry.metadata().ok().map(|m| m.len());

            // Create a basic artifact record (kind and step_id are unknown)
            let record = ArtifactRecord::new(run_id, path, "unknown", "unknown")
                .with_size(size_bytes.unwrap_or(0));

            artifacts.push(record);
        }
    }

    Ok(artifacts)
}

/// Get the default artifacts root directory.
/// Uses environment LUTHER_ARTIFACTS_ROOT or defaults to ./artifacts
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P06
pub fn default_artifacts_root() -> PathBuf {
    std::env::var("LUTHER_ARTIFACTS_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./artifacts"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_record_can_be_created() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P06
        let record = ArtifactRecord::new(
            "run-123",
            PathBuf::from("/artifacts/run-123/output.log"),
            "log",
            "step-1",
        );
        assert_eq!(record.run_id, "run-123");
        assert_eq!(record.artifact_kind, "log");
        assert_eq!(record.step_id, "step-1");
    }

    #[test]
    fn artifact_record_with_size() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P06
        let record = ArtifactRecord::new(
            "run-456",
            PathBuf::from("/artifacts/run-456/data.json"),
            "output",
            "step-2",
        )
        .with_size(1024);
        assert_eq!(record.size_bytes, Some(1024));
    }

    #[test]
    fn get_artifacts_dir_is_deterministic() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P06
        let root = PathBuf::from("/tmp/artifacts");
        let dir1 = get_artifacts_dir(&root, "run-abc-123");
        let dir2 = get_artifacts_dir(&root, "run-abc-123");
        assert_eq!(dir1, dir2);
        assert!(dir1.to_string_lossy().contains("run-abc-123"));
    }

    #[test]
    fn default_artifacts_root_returns_path() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P06
        let root = default_artifacts_root();
        // Should be either from env or default ./artifacts
        assert!(!root.as_os_str().is_empty());
    }
}
