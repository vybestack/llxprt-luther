/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
/// @plan:PLAN-20260408-STEP-EXEC.P03
/// Engine module - workflow execution runtime.

pub mod dagrs_runtime;
pub mod executor;
pub mod executors;
pub mod instance;
pub mod runner;
pub mod transition;

// Re-export transition types for convenience
pub use transition::{StepOutcome, resolve_transition, resolve_transition_schema};
// Re-export TransitionDef from workflow schema for test compatibility
pub use crate::workflow::schema::TransitionDef;
