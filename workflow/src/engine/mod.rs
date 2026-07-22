/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
/// @plan:PLAN-20260408-STEP-EXEC.P03
/// Engine module - workflow execution runtime.
///
/// `runner::EngineRunner` is the single supported execution engine: a durable,
/// resumable, outcome-routed state machine backed by SQLite checkpointing. An
/// earlier `dagrs`-backed scaffold was removed because the `dagrs` static DAG
/// model does not fit Luther's dynamic, resumable, transition-routed semantics.
pub mod continuation;
pub mod executor;
pub mod executors;
pub mod instance;
pub mod runner;
/// Typed validation of identity-bearing tokens interpolated into shell
/// commands. @plan:PLAN-20260722-ISSUE158-SHELL-SAFE-TOKENS
pub mod shell_safe_tokens;
pub mod transition;
/// Cohesive workspace ownership abstraction (two-phase evidence).
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub mod workspace_ownership;

// Re-export transition types for convenience
pub use runner::{EngineRunner, RunContext, RunOutcome};
pub use transition::{resolve_transition, resolve_transition_schema, StepOutcome};
// Re-export continuation operator API.
// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub use continuation::{
    commit_continuation, continuation_overrides, prepare_continuation,
    prepare_resume_authorization, ContinuationKind, ContinuationRequest, ContinuationValidation,
    PreparedResume, ResumeAuthorizationError, RewindTarget,
};
// Re-export TransitionDef from workflow schema for test compatibility
pub use crate::workflow::schema::TransitionDef;
