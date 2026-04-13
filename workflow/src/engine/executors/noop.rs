/// @plan:PLAN-20260408-STEP-EXEC.P03
/// `NoOp` executor - always returns `Success` for testing.
use crate::engine::executor::{StepContext, StepExecutor};
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;

/// No-op executor that always returns `Success`.
/// Used for tests that don't need real execution.
pub struct NoOpExecutor;

impl StepExecutor for NoOpExecutor {
    fn execute(
        &self,
        _context: &mut StepContext,
        _params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        Ok(StepOutcome::Success)
    }
}
