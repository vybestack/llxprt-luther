use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

pub const DEFAULT_WORKFLOW_PATH_PATTERN: &str = ".github/workflows/**";
pub const DEFAULT_REQUIRED_SCOPE: &str = "workflow";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkflowAuthPreflightConfig {
    #[serde(default = "default_workflow_path_patterns")]
    pub workflow_path_patterns: Vec<String>,
    #[serde(default = "default_required_scopes")]
    pub required_scopes: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum WorkflowAuthOutcome {
    Pass,
    Fatal,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DetectedWorkflowPath {
    pub path: String,
    pub source: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkflowAuthPreflightReport {
    pub outcome: WorkflowAuthOutcome,
    pub auth_method: String,
    pub workflow_path_patterns: Vec<String>,
    pub detected_workflow_paths: Vec<DetectedWorkflowPath>,
    pub required_scopes: Vec<String>,
    pub observed_scopes: Vec<String>,
    pub missing_capability: Option<String>,
    pub recommended_operator_action: Option<String>,
}

impl Default for WorkflowAuthPreflightConfig {
    fn default() -> Self {
        Self {
            workflow_path_patterns: default_workflow_path_patterns(),
            required_scopes: default_required_scopes(),
        }
    }
}

pub fn default_workflow_path_patterns() -> Vec<String> {
    vec![DEFAULT_WORKFLOW_PATH_PATTERN.to_string()]
}

pub fn default_required_scopes() -> Vec<String> {
    vec![DEFAULT_REQUIRED_SCOPE.to_string()]
}

pub fn detect_workflow_paths<I, S>(
    paths: I,
    source: &str,
    patterns: &[String],
) -> Vec<DetectedWorkflowPath>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut detected = BTreeSet::new();
    for path in paths {
        let path = path.as_ref();
        if matches_any_workflow_pattern(path, patterns) {
            detected.insert(path.to_string());
        }
    }

    detected
        .into_iter()
        .map(|path| DetectedWorkflowPath {
            path,
            source: source.to_string(),
        })
        .collect()
}

pub fn matches_any_workflow_pattern(path: &str, patterns: &[String]) -> bool {
    patterns
        .iter()
        .any(|pattern| matches_workflow_pattern(path, pattern))
}

pub fn matches_workflow_pattern(path: &str, pattern: &str) -> bool {
    let normalized = path.trim_matches(|ch: char| matches!(ch, '`' | '"' | '\''));
    if pattern == DEFAULT_WORKFLOW_PATH_PATTERN {
        return normalized.starts_with(".github/workflows/");
    }

    if let Some(prefix) = pattern.strip_suffix("/**") {
        return normalized
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'));
    }

    matches_path_segments(normalized, pattern)
}

fn matches_path_segments(path: &str, pattern: &str) -> bool {
    // Middle `**` patterns are intentionally not supported here; the workflow
    // preflight config uses exact segments plus the suffix `/**` fast-path.
    let path_segments = path.split('/').collect::<Vec<_>>();
    let pattern_segments = pattern.split('/').collect::<Vec<_>>();
    path_segments.len() == pattern_segments.len()
        && path_segments
            .iter()
            .zip(pattern_segments.iter())
            .all(|(path, pattern)| matches_segment(path, pattern))
}

fn matches_segment(path: &str, pattern: &str) -> bool {
    if !pattern.contains('*') {
        return path == pattern;
    }

    let parts = pattern.split('*').collect::<Vec<_>>();
    let mut remainder = path;
    let mut middle_start = 0;
    if !pattern.starts_with('*') {
        let first = parts.first().copied().unwrap_or_default();
        if !remainder.starts_with(first) {
            return false;
        }
        remainder = &remainder[first.len()..];
        middle_start = 1;
    }

    let middle_end = if pattern.ends_with('*') {
        parts.len()
    } else {
        parts.len().saturating_sub(1)
    };
    for part in parts[middle_start..middle_end]
        .iter()
        .filter(|part| !part.is_empty())
    {
        if let Some(index) = remainder.find(part) {
            remainder = &remainder[index + part.len()..];
        } else {
            return false;
        }
    }

    pattern.ends_with('*')
        || parts
            .last()
            .is_none_or(|last| last.is_empty() || remainder.ends_with(last))
}

pub fn classify_push_rejection(stderr: &str) -> Option<String> {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("refusing to allow") && lower.contains("workflow") {
        Some("missing workflow scope for workflow-file push".to_string())
    } else {
        None
    }
}

pub fn build_report(
    auth_method: String,
    config: WorkflowAuthPreflightConfig,
    detected_workflow_paths: Vec<DetectedWorkflowPath>,
    observed_scopes: Vec<String>,
) -> WorkflowAuthPreflightReport {
    let missing_scopes = missing_scopes(&config.required_scopes, &observed_scopes);
    let has_workflow_paths = !detected_workflow_paths.is_empty();
    let requires_oauth_scope = auth_method == "https_oauth" && has_workflow_paths;
    let missing_capability = if requires_oauth_scope && !missing_scopes.is_empty() {
        Some(format!(
            "missing required OAuth scope(s): {}",
            missing_scopes.join(", ")
        ))
    } else if has_workflow_paths && !matches!(auth_method.as_str(), "ssh" | "https_oauth") {
        Some("unable to prove credentials can update workflow files".to_string())
    } else {
        None
    };

    WorkflowAuthPreflightReport {
        outcome: if missing_capability.is_some() {
            WorkflowAuthOutcome::Fatal
        } else {
            WorkflowAuthOutcome::Pass
        },
        auth_method,
        workflow_path_patterns: config.workflow_path_patterns,
        detected_workflow_paths,
        required_scopes: config.required_scopes,
        observed_scopes,
        recommended_operator_action: missing_capability.as_ref().map(|capability| {
            if capability.starts_with("missing required OAuth scope") {
                "Refresh the GitHub token with workflow scope, or switch to an SSH push remote, before pushing workflow files."
                    .to_string()
            } else {
                "Use an SSH push remote, or configure a github.com HTTPS remote with a valid OAuth token, before pushing workflow files."
                    .to_string()
            }
        }),
        missing_capability,
    }
}

pub fn classify_remote_url(remote_url: &str) -> String {
    if remote_url.starts_with("git@github.com:")
        || remote_url.starts_with("ssh://git@github.com/")
        || remote_url.starts_with("ssh://github.com/")
    {
        "ssh".to_string()
    } else if remote_url.starts_with("git@") || remote_url.starts_with("ssh://") {
        "unknown".to_string()
    } else if let Some(rest) = remote_url.strip_prefix("https://") {
        let host_and_path = rest.rsplit_once('@').map_or(rest, |(_, after_at)| after_at);
        if host_and_path.starts_with("github.com/") {
            "https_oauth".to_string()
        } else {
            "unknown_https".to_string()
        }
    } else {
        "unknown".to_string()
    }
}

pub fn parse_porcelain_paths(output: &[u8]) -> Vec<String> {
    let mut paths = Vec::new();
    let mut entries = output.split(|byte| *byte == 0);
    while let Some(entry) = entries.next() {
        if is_rename_or_copy_entry(entry) {
            if let Some(path) = parse_porcelain_entry(entry) {
                paths.push(path);
            }
            if let Some(old_path) = entries.next().and_then(parse_bare_porcelain_path) {
                paths.push(old_path);
            }
        } else if let Some(path) = parse_porcelain_entry(entry) {
            paths.push(path);
        }
    }
    paths
}

fn is_rename_or_copy_entry(entry: &[u8]) -> bool {
    matches!(entry.first(), Some(b'R' | b'C'))
}

fn parse_bare_porcelain_path(entry: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(entry);
    let path = text.trim();
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

fn parse_porcelain_entry(entry: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(entry);
    let path = if is_rename_or_copy_entry(entry) {
        text.split_once(char::is_whitespace)
            .map_or("", |(_, path)| path)
    } else {
        text.get(3..).unwrap_or_default()
    }
    .trim();

    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

fn missing_scopes(required_scopes: &[String], observed_scopes: &[String]) -> Vec<String> {
    required_scopes
        .iter()
        .filter(|scope| {
            !observed_scopes
                .iter()
                .any(|observed| observed.trim().eq_ignore_ascii_case(scope.trim()))
        })
        .cloned()
        .collect()
}

pub fn extract_workflow_paths_from_text(
    text: &str,
    source: &str,
    patterns: &[String],
) -> Vec<DetectedWorkflowPath> {
    let candidates = text
        .split(|ch: char| {
            ch.is_whitespace() || matches!(ch, '`' | '"' | '\'' | ',' | ')' | '(' | '[' | ']')
        })
        .map(|token| token.trim_matches(|ch: char| matches!(ch, ':' | ';')))
        .filter(|token| !token.is_empty());
    detect_workflow_paths(candidates, source, patterns)
}

pub fn artifact_path(artifact_dir: &Path, configured_path: Option<&str>) -> std::path::PathBuf {
    configured_path.map_or_else(
        || artifact_dir.join("workflow-auth-preflight.json"),
        |path| {
            let configured = std::path::PathBuf::from(path);
            if configured.is_absolute() {
                configured
            } else {
                artifact_dir.join(configured)
            }
        },
    )
}
