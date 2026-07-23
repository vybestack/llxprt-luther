//! Resolution and validation policy for llxprt diagnostic artifact paths.

use std::path::{Path, PathBuf};

use crate::engine::executor::{interpolate_string, StepContext};
use crate::engine::runner::EngineError;

pub(super) fn diagnostic_root(context: &StepContext) -> Result<PathBuf, EngineError> {
    let configured = context
        .get("artifact_dir")
        .or_else(|| context.get("artifact_root"));
    let root = configured.map_or_else(
        || crate::runtime_paths::get_artifacts_root().join(context.run_id()),
        PathBuf::from,
    );
    reject_unresolved_path(&root)?;
    Ok(if root.is_absolute() {
        root
    } else {
        context.work_dir().join(root)
    })
}

pub(super) fn resolve_stream_path(
    context: &StepContext,
    params: &serde_json::Value,
    key: &str,
) -> Result<Option<PathBuf>, EngineError> {
    let Some(template) = params.get(key).and_then(serde_json::Value::as_str) else {
        return Ok(None);
    };
    let path = PathBuf::from(interpolate_string(template, context));
    reject_unresolved_path(&path)?;
    Ok(Some(if path.is_absolute() {
        path
    } else {
        context.work_dir().join(path)
    }))
}

fn reject_unresolved_path(path: &Path) -> Result<(), EngineError> {
    let value = path.to_string_lossy();
    if value.contains('{') || value.contains('}') {
        return Err(path_error(format!(
            "diagnostic path contains unresolved template token: {value}"
        )));
    }
    Ok(())
}

pub(super) fn sanitize_filename_segment(step_id: &str) -> String {
    let sanitized: String = step_id
        .chars()
        .map(sanitize_filename_character)
        .take(96)
        .collect();
    if sanitized.is_empty() {
        "llxprt".to_string()
    } else {
        sanitized
    }
}

fn sanitize_filename_character(character: char) -> char {
    if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
        character
    } else {
        '_'
    }
}

fn path_error(message: impl Into<String>) -> EngineError {
    EngineError::StepExecutionError {
        step_id: "llxprt".to_string(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_id_is_safe_for_filenames() {
        assert_eq!(
            sanitize_filename_segment("../../create plan"),
            "______create_plan"
        );
        assert_eq!(sanitize_filename_segment(""), "llxprt");
    }

    #[test]
    fn unresolved_explicit_path_is_rejected() {
        let context = StepContext::new("/tmp/work".into(), "run".to_string());
        let error = resolve_stream_path(
            &context,
            &serde_json::json!({"stdout_file": "{missing}/stdout.log"}),
            "stdout_file",
        )
        .unwrap_err();
        assert!(error.to_string().contains("unresolved template token"));
    }
}
