/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
/// @plan:PLAN-20260408-STEP-EXEC.P03
/// Engine module - workflow execution runtime.
///
/// `runner::EngineRunner` is the single supported execution engine: a durable,
/// resumable, outcome-routed state machine backed by SQLite checkpointing. An
/// earlier `dagrs`-backed scaffold was removed because the `dagrs` static DAG
/// model does not fit Luther's dynamic, resumable, transition-routed semantics.
pub mod executor;
pub mod executors;
pub mod instance;
pub mod runner;
pub mod transition;

// Re-export transition types for convenience
pub use transition::{resolve_transition, resolve_transition_schema, StepOutcome};
// Re-export TransitionDef from workflow schema for test compatibility
pub use crate::workflow::schema::TransitionDef;
