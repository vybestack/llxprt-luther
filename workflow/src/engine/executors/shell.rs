/// @plan:PLAN-20260408-STEP-EXEC.P03
/// @plan:PLAN-20260408-STEP-EXEC.P05
/// Shell executor - executes shell commands.
use std::process::{Command, Stdio};

use crate::engine::executor::{interpolate_string, StepContext, StepExecutor};
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;

/// Shell executor that runs shell commands.
pub struct ShellExecutor;

impl StepExecutor for ShellExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        // Extract "command" from params JSON
        let command_template = params
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EngineError::StepExecutionError {
                step_id: "shell".to_string(),
                message: "Missing 'command' parameter for shell executor".to_string(),
            })?;

        // Interpolate command string using context
        let interpolated_command = interpolate_string(command_template, context);

        // Run via sh -c
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&interpolated_command);
        cmd.current_dir(context.work_dir());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let output = match cmd.output() {
            Ok(output) => output,
            Err(e) => {
                return Err(EngineError::StepExecutionError {
                    step_id: "shell".to_string(),
                    message: format!("Failed to spawn command: {e}"),
                });
            }
        };

        // Capture stdout and stderr into context
        let stdout_str = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr_str = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code();

        context.set("stdout", &stdout_str);
        context.set("stderr", &stderr_str);
        if let Some(code) = exit_code {
            context.set("exit_code", &code.to_string());
        }

        // Exit 0 → Success, non-zero → Fixable
        if exit_code == Some(0) {
            Ok(StepOutcome::Success)
        } else {
            Ok(StepOutcome::Fixable)
        }
    }
}
