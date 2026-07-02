use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommandManifest {
    pub commands: Vec<CommandEntry>,
    pub groups: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandEntry {
    pub id: String,
    pub argv: Vec<String>,
    #[serde(default)]
    pub working_directory: Option<String>,
    #[serde(default)]
    pub project_subdirectory: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    #[serde(default = "default_acceptable_exit_codes")]
    pub acceptable_exit_codes: Vec<i32>,
    #[serde(default)]
    pub capture: CapturePolicy,
    #[serde(default)]
    pub stdout: StreamExpectations,
    #[serde(default)]
    pub stderr: StreamExpectations,
    #[serde(default)]
    pub artifacts: ArtifactExpectations,
    #[serde(default)]
    pub failure_outcome: FailureOutcome,
    #[serde(default)]
    pub retry: RetryPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CapturePolicy {
    #[serde(default = "default_capture_enabled")]
    pub stdout: bool,
    #[serde(default = "default_capture_enabled")]
    pub stderr: bool,
    #[serde(default = "default_capture_limit_bytes")]
    pub limit_bytes: usize,
}

impl Default for CapturePolicy {
    fn default() -> Self {
        Self {
            stdout: true,
            stderr: true,
            limit_bytes: default_capture_limit_bytes(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StreamExpectations {
    pub required_patterns: Vec<String>,
    pub forbidden_patterns: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ArtifactExpectations {
    pub required: Vec<ArtifactExpectation>,
    pub forbidden: Vec<ArtifactExpectation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactExpectation {
    pub path: String,
    #[serde(default)]
    pub kind: ArtifactKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    #[default]
    Any,
    File,
    Directory,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureOutcome {
    #[default]
    Fatal,
    Fixable,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub retry_exit_codes: Vec<i32>,
}

fn default_acceptable_exit_codes() -> Vec<i32> {
    vec![0]
}

fn default_capture_enabled() -> bool {
    true
}

fn default_capture_limit_bytes() -> usize {
    64 * 1024
}
