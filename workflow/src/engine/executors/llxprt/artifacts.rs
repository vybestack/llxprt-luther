//! Durable, bounded stdout/stderr diagnostics for llxprt executor steps.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::engine::executor::StepContext;
use crate::engine::runner::EngineError;

pub(super) const STREAM_CAPTURE_LIMIT: usize = 64 * 1024;
const EXCERPT_LIMIT: usize = (STREAM_CAPTURE_LIMIT - 160) / 2;
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) type SharedCapture = Arc<Mutex<StreamCapture>>;

pub(super) struct DiagnosticArtifacts {
    pub(super) stdout: SharedCapture,
    pub(super) stderr: SharedCapture,
    manifest_path: PathBuf,
    run_id: String,
    step_id: String,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
}

pub(super) struct StreamCapture {
    path: PathBuf,
    head: String,
    tail: String,
    total_bytes: usize,
}

impl DiagnosticArtifacts {
    pub(super) fn initialize(
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<Self, EngineError> {
        let step_id = context
            .get("current_step_id")
            .cloned()
            .unwrap_or_else(|| "llxprt".to_string());
        let root = super::artifact_paths::diagnostic_root(context)?;
        let default_dir = root.join("llxprt-diagnostics");
        let safe_step_id = super::artifact_paths::sanitize_filename_segment(&step_id);
        let stdout_path =
            super::artifact_paths::resolve_stream_path(context, params, "stdout_file")?
                .unwrap_or_else(|| default_dir.join(format!("{safe_step_id}-stdout.log")));
        let stderr_path =
            super::artifact_paths::resolve_stream_path(context, params, "stderr_file")?
                .unwrap_or_else(|| default_dir.join(format!("{safe_step_id}-stderr.log")));
        let manifest_path = default_dir.join(format!("{safe_step_id}-manifest.json"));
        prepare_file(&stdout_path)?;
        prepare_file(&stderr_path)?;
        prepare_parent(&manifest_path)?;

        let artifacts = Self {
            stdout: Arc::new(Mutex::new(StreamCapture::new(stdout_path.clone()))),
            stderr: Arc::new(Mutex::new(StreamCapture::new(stderr_path.clone()))),
            manifest_path,
            run_id: context.run_id().to_string(),
            step_id,
            stdout_path,
            stderr_path,
        };
        artifacts.publish_manifest()?;
        context.set(
            "stdout_artifact_path",
            &artifacts.stdout_path.to_string_lossy(),
        );
        context.set(
            "stderr_artifact_path",
            &artifacts.stderr_path.to_string_lossy(),
        );
        context.set(
            "llxprt_diagnostic_manifest_path",
            &artifacts.manifest_path.to_string_lossy(),
        );
        Ok(artifacts)
    }

    pub(super) fn publish_manifest(&self) -> Result<(), EngineError> {
        let stdout_bytes = total_bytes(&self.stdout);
        let stderr_bytes = total_bytes(&self.stderr);
        let value = serde_json::json!({
            "run_id": self.run_id,
            "step_id": self.step_id,
            "stdout_path": self.stdout_path,
            "stderr_path": self.stderr_path,
            "stream_capture_limit": STREAM_CAPTURE_LIMIT,
            "stdout_bytes_seen": stdout_bytes,
            "stderr_bytes_seen": stderr_bytes,
        });
        let content = serde_json::to_vec_pretty(&value)
            .map_err(|error| artifact_error(format!("serialize diagnostic manifest: {error}")))?;
        replace_file(&self.manifest_path, &content)
    }
}

impl StreamCapture {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            head: String::new(),
            tail: String::new(),
            total_bytes: 0,
        }
    }

    pub(super) fn append(&mut self, bytes: &[u8]) -> Result<(), EngineError> {
        self.total_bytes = self.total_bytes.saturating_add(bytes.len());
        let text = String::from_utf8_lossy(bytes);
        append_prefix(&mut self.head, &text, EXCERPT_LIMIT);
        self.tail.push_str(&text);
        trim_start_to_limit(&mut self.tail, EXCERPT_LIMIT);
        replace_file(&self.path, self.render().as_bytes())
    }

    pub(super) fn render(&self) -> String {
        if self.total_bytes <= STREAM_CAPTURE_LIMIT {
            return format!("{}{}", self.head, self.tail_without_head_overlap());
        }
        let omitted = self
            .total_bytes
            .saturating_sub(self.head.len().saturating_add(self.tail.len()));
        format!(
            "{}\n--- {omitted} bytes omitted; showing head and tail ---\n{}",
            self.head, self.tail
        )
    }

    pub(super) const fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    fn tail_without_head_overlap(&self) -> &str {
        let overlap = self
            .head
            .len()
            .saturating_add(self.tail.len())
            .saturating_sub(self.total_bytes);
        safe_suffix(&self.tail, self.tail.len().saturating_sub(overlap))
    }
}

pub(super) fn capture_text(capture: &SharedCapture) -> String {
    capture
        .lock()
        .map_or_else(|_| String::new(), |capture| capture.render())
}

pub(super) fn append(capture: &SharedCapture, bytes: &[u8]) -> Result<(), EngineError> {
    capture
        .lock()
        .map_err(|_| artifact_error("diagnostic capture lock poisoned"))?
        .append(bytes)
}

pub(super) fn total_bytes(capture: &SharedCapture) -> usize {
    capture.lock().map_or(0, |capture| capture.total_bytes())
}

fn append_prefix(target: &mut String, value: &str, limit: usize) {
    if target.len() >= limit {
        return;
    }
    let remaining = limit - target.len();
    target.push_str(safe_prefix(value, remaining));
}

fn trim_start_to_limit(value: &mut String, limit: usize) {
    if value.len() <= limit {
        return;
    }
    let keep = safe_suffix(value, limit).to_string();
    *value = keep;
}

fn safe_prefix(value: &str, limit: usize) -> &str {
    let mut end = limit.min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn safe_suffix(value: &str, limit: usize) -> &str {
    let mut start = value.len().saturating_sub(limit);
    while !value.is_char_boundary(start) {
        start += 1;
    }
    &value[start..]
}

fn prepare_file(path: &Path) -> Result<(), EngineError> {
    prepare_parent(path)?;
    replace_file(path, b"")
}

fn prepare_parent(path: &Path) -> Result<(), EngineError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| artifact_error(format!("create diagnostic directory: {error}")))?;
    }
    Ok(())
}

fn replace_file(path: &Path, content: &[u8]) -> Result<(), EngineError> {
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = path.with_extension(format!(
        "{}.{}.{}.tmp",
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or(""),
        std::process::id(),
        sequence
    ));
    std::fs::write(&temporary, content)
        .map_err(|error| artifact_error(format!("write diagnostic artifact: {error}")))?;
    std::fs::rename(&temporary, path)
        .map_err(|error| artifact_error(format!("publish diagnostic artifact: {error}")))
}

fn artifact_error(message: impl Into<String>) -> EngineError {
    EngineError::StepExecutionError {
        step_id: "llxprt".to_string(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_parent_is_created_when_stream_paths_are_explicit() {
        let temp = tempfile::tempdir().unwrap();
        let explicit = temp.path().join("explicit");
        let mut context = StepContext::new(temp.path().to_path_buf(), "run".to_string());
        context.set_current_step_id("evaluate_impl");
        context.set(
            "artifact_dir",
            &temp.path().join("artifacts").to_string_lossy(),
        );
        let params = serde_json::json!({
            "stdout_file": explicit.join("stdout.log"),
            "stderr_file": explicit.join("stderr.log")
        });

        let artifacts = DiagnosticArtifacts::initialize(&mut context, &params).unwrap();

        assert!(artifacts.manifest_path.exists());
        assert!(artifacts.stdout_path.exists());
        assert!(artifacts.stderr_path.exists());
    }

    #[test]
    fn bounded_capture_retains_head_and_tail() {
        let temp = tempfile::tempdir().unwrap();
        let mut capture = StreamCapture::new(temp.path().join("stdout.log"));
        capture.append(b"HEAD").unwrap();
        capture
            .append(&vec![b'x'; STREAM_CAPTURE_LIMIT * 2])
            .unwrap();
        capture.append(b"TAIL").unwrap();
        let rendered = capture.render();
        assert!(rendered.starts_with("HEAD"));
        assert!(rendered.ends_with("TAIL"));
        assert!(rendered.len() <= STREAM_CAPTURE_LIMIT);
        assert!(rendered.contains("bytes omitted"));
    }
}
