//! The error type returned by the executor contract.
//!
//! Extracted from `runner.rs` so that `StepExecutor`, `StepContext`,
//! `StepOutcome`, and `EngineError` form a contract that does not reach into
//! persistence, the runner, or any domain. Every component implements
//! `StepExecutor`, so anything this type depends on is a dependency every
//! component package would inherit.
//!
//! The `From<LlxprtError>` conversion deliberately does not live here: it is
//! implemented beside the LLxprt adapter, because a conversion belongs to the
//! code that understands the thing being converted.

use crate::engine::transition::StepOutcome;
use thiserror::Error;

/// Errors that can occur during workflow execution.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
/// @requirement:REQ-EARS-ENG-003,REQ-EARS-PERSIST-004
#[derive(Error, Debug)]
pub enum EngineError {
    #[error("step execution failed: {step_id} - {message}")]
    StepExecutionError { step_id: String, message: String },

    #[error("transition not found from {step_id} with outcome {outcome:?}")]
    TransitionNotFound {
        step_id: String,
        outcome: StepOutcome,
    },

    #[error("loop limit exceeded at step {step_id}")]
    LoopLimitExceeded { step_id: String },

    #[error("retry limit exceeded for step {step_id}")]
    RetryLimitExceeded { step_id: String },

    #[error("persistence error: {0}")]
    PersistenceError(String),

    #[error("invalid state: {0}")]
    InvalidState(String),

    #[error("step not found: {0}")]
    StepNotFound(String),

    /// An external tool a step depends on is unusable.
    ///
    /// Replaces three variants that named LLxprt directly. This type is
    /// returned by `StepExecutor::execute`, which every component implements,
    /// so a variant naming one tool made it impossible to relocate any
    /// component into a domain-free package — the package would have had to
    /// depend on a type that knows what LLxprt is.
    ///
    /// The message is formatted by the domain that raised it and carried
    /// through verbatim, so the text a user sees is unchanged and stays owned
    /// by the code that understands the tool. The engine needs to know only
    /// that a required tool failed, which is what it did with all three
    /// variants anyway: they shared a single match arm that set a diagnostic
    /// and returned `Fatal`.
    #[error("{message}")]
    ToolUnavailable { message: String },

    /// Workspace ownership verification failed while routing into a
    /// `failure_cleanup` step. This is a terminal ownership failure: the run
    /// must not execute the workspace-mutating cleanup shell script (e.g.
    /// `abandon_and_log`) because the workspace is not owned by this run.
    /// Instead, the runner protects the issue lease and records a terminal
    /// failure outcome without workspace mutation. This prevents a misleading
    /// "ownership-fatal → abandon_and_log" path where an ownership auth
    /// failure pretends cleanup will run.
    #[error("{0}")]
    OwnershipFailure(OwnershipFailureDetails),
}

/// Details of a terminal workspace ownership failure encountered while routing
/// into a `failure_cleanup` step. Carries the targeted cleanup step and the
/// ownership verification rejection reason so the runner can record a terminal
/// failure outcome without executing the workspace-mutating cleanup.
#[derive(Debug, Clone)]
pub struct OwnershipFailureDetails {
    /// The `failure_cleanup` step that would have been entered.
    pub failed_step: String,
    /// The ownership verification rejection reason.
    pub reason: String,
}

impl std::fmt::Display for OwnershipFailureDetails {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "workspace ownership failure before cleanup step {}: {}",
            self.failed_step, self.reason
        )
    }
}
