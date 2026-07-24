//! Injectable recovery execution dependency. [C5/C12]
//!
//! The [`RecoveryExecutor`] trait is the object-safe seam through which the
//! recovery protocol invokes external work (the step runner). P11 owns the
//! trait surface and the protocol's invocation of it **outside** SQLite writer
//! transactions. P12 owns the real capsule-backed runner adapter that
//! implements this trait.
//!
//! Fail-closed contract: the production default ([`UnavailableRecoveryExecutor`])
//! **never fabricates success**. Plain [`super::RecoveryProtocolV1::recover`]
//! delegates to it, so any executable strategy that reaches the execute phase
//! without a real executor returns [`RecoveryError::Execution`].
//! [C5/C12]
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
//! @requirement:REQ-RP-001

use std::path::Path;

use crate::engine::recovery::capsule::ExecutionCapsuleV1;
use crate::persistence::checkpoint::StateSnapshot;

use super::RecoveryStrategy;

/// Typed input bundle passed to [`RecoveryExecutor::execute`]. [C5/C12]
///
/// Carries the exact durable context the executor needs to run the step: the
/// immutable capsule binding, the concrete strategy, the reserved epoch/
/// attempt ids, and the workspace path (for ContinueWorkspace verification).
/// All references borrow from the prepared recovery state so no clone is
/// required. [C3/C8]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
/// @requirement:REQ-RP-001
#[derive(Debug)]
pub struct RecoveryExecutionInvocation<'a> {
    /// The run being recovered.
    pub run_id: &'a str,
    /// The step to execute.
    pub step_id: &'a str,
    /// The durable operation id reserved at reserve.
    pub operation_id: &'a str,
    /// The durable attempt id allocated at reserve. [B4]
    pub attempt_id: i64,
    /// The epoch at which this recovery was reserved. [B2]
    pub epoch: u64,
    /// The concrete recovery strategy selected for this step. [C4/C6]
    pub strategy: RecoveryStrategy,
    /// The exact immutable capsule binding. [C3/C8]
    pub capsule: &'a ExecutionCapsuleV1,
    /// The workspace path (for ContinueWorkspace verification). [B6]
    pub workspace: &'a Path,
}

/// Typed output of [`RecoveryExecutor::execute`] carrying the actual step
/// status, state snapshot, and runner result. [C5/C12]
///
/// The protocol's finalize phase appends these fields to the durable attempt
/// row. No production default may construct a success result — only a real
/// executor (P12) or a test stub may do so. [C12]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
/// @requirement:REQ-RP-001
#[derive(Debug, Clone)]
pub struct RecoveryExecutionResult {
    /// The step status produced by execution (e.g. `"completed"`, `"failed"`).
    pub step_status: String,
    /// The complete workflow state snapshot after execution. [C3]
    pub state_snapshot: StateSnapshot,
    /// The durable runner result (recoverable after a crash). [B4]
    pub runner_result: Option<serde_json::Value>,
}

/// Errors produced by [`RecoveryExecutor::execute`]. [C5/C12]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
/// @requirement:REQ-RP-001
#[derive(Debug, thiserror::Error)]
pub enum RecoveryExecutionError {
    /// No real executor is configured (fail-closed default). [C5/C12]
    ///
    /// The protocol maps this to [`RecoveryError::Execution`] so plain
    /// [`super::RecoveryProtocolV1::recover`] cannot fabricate success.
    #[error("recovery executor unavailable: {0}")]
    Unavailable(String),
    /// Execution ran but the step failed. [C5/C12]
    #[error("recovery execution failed: {0}")]
    Failed(String),
}

/// Object-safe recovery execution seam. [C5/C12]
///
/// The recovery protocol calls [`execute`](Self::execute) **outside** SQLite
/// writer transactions for every executable strategy (Reenter,
/// ContinueWorkspace, ReconcileThenReenter, CompensateThenRetry). Refused
/// strategies short-circuit in reserve and never reach the executor.
///
/// P11 owns the trait and the protocol's invocation of it. P12 owns the real
/// capsule-backed runner adapter. No production default may fabricate success;
/// the default [`UnavailableRecoveryExecutor`] always returns
/// [`RecoveryExecutionError::Unavailable`]. [C5/C12]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
/// @requirement:REQ-RP-001
pub trait RecoveryExecutor: std::fmt::Debug {
    /// Execute the recovery step outside any transaction. [C5/C12]
    ///
    /// The protocol guarantees the invocation carries the reserved epoch and
    /// allocated attempt id. The executor must not open its own SQLite writer
    /// transaction on the protocol's connection — external side effects are
    /// recorded by the protocol's finalize phase after this returns.
    fn execute(
        &self,
        invocation: &RecoveryExecutionInvocation<'_>,
    ) -> Result<RecoveryExecutionResult, RecoveryExecutionError>;
}

/// Fail-closed default executor: never fabricates success. [C5/C12]
///
/// Used by [`super::RecoveryProtocolV1::recover`] and
/// [`super::RecoveryProtocolV1::recover_with_observer`] so that plain
/// (non-injected) entry points cannot return [`super::RecoveryOutcome::Recovered`]
/// for an executable strategy without a real executor. [C12]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
/// @requirement:REQ-RP-001
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableRecoveryExecutor;

impl RecoveryExecutor for UnavailableRecoveryExecutor {
    fn execute(
        &self,
        _invocation: &RecoveryExecutionInvocation<'_>,
    ) -> Result<RecoveryExecutionResult, RecoveryExecutionError> {
        Err(RecoveryExecutionError::Unavailable(
            "no recovery executor is configured (P12 provides the capsule-backed runner adapter)"
                .to_string(),
        ))
    }
}

use super::RecoveryError;

/// Map a [`RecoveryExecutionError`] into a [`RecoveryError::Execution`]. [C5]
pub(super) fn map_execution_error() -> impl Fn(RecoveryExecutionError) -> RecoveryError {
    |e: RecoveryExecutionError| RecoveryError::Execution(e.to_string())
}
