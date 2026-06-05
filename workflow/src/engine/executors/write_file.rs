/// @plan:PLAN-20260408-STEP-EXEC.P03
/// @plan:PLAN-20260408-STEP-EXEC.P05
/// `WriteFile` executor - writes content to files.
use std::fs;

use crate::engine::executor::{interpolate_string, StepContext, StepExecutor};
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;

/// `WriteFile` executor that writes content to files.
pub struct WriteFileExecutor;

impl StepExecutor for WriteFileExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        // Extract "path" and "content" from params JSON
        let path_template = params.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
            EngineError::StepExecutionError {
                step_id: "write_file".to_string(),
                message: "Missing 'path' parameter for write_file executor".to_string(),
            }
        })?;

        let content_template = params
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EngineError::StepExecutionError {
                step_id: "write_file".to_string(),
                message: "Missing 'content' parameter for write_file executor".to_string(),
            })?;

        // Interpolate both strings using context
        let interpolated_path = interpolate_string(path_template, context);
        let interpolated_content = interpolate_string(content_template, context);

        // Resolve path relative to work_dir
        let full_path = context.work_dir().join(&interpolated_path);

        // Create parent directories if needed
        if let Some(parent) = full_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).map_err(|e| EngineError::StepExecutionError {
                    step_id: "write_file".to_string(),
                    message: format!("Failed to create parent directory: {e}"),
                })?;
            }
        }

        // Write content
        fs::write(&full_path, &interpolated_content).map_err(|e| {
            EngineError::StepExecutionError {
                step_id: "write_file".to_string(),
                message: format!("Failed to write file: {e}"),
            }
        })?;

        Ok(StepOutcome::Success)
    }
}
