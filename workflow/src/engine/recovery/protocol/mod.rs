//! Recovery protocol (`RecoveryProtocolV1`): the single typed recovery
//! abstraction owning the phased model (prepare → reserve → execute →
//! finalize). [C1/C2/C4/C5/C12/B1/B2/B4/B6]
//!
//! This module owns the phased recovery model. It consumes the durable epoch
//! CAS, operation ledger, append-only attempt store, and immutable capsule
//! store directly — no in-memory persistence facade.
//!
//! Phase implementations live in the [`prepare`], [`reserve`], [`execute`],
//! and [`finalize`] submodules; this module owns the public type surface and
//! the single dispatch entry point.
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P09
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
//! @requirement:REQ-RP-001

mod execute;
pub mod executor;
mod finalize;
mod prepare;
mod reserve;

pub use executor::{
    RecoveryExecutionError, RecoveryExecutionInvocation, RecoveryExecutionResult, RecoveryExecutor,
    UnavailableRecoveryExecutor,
};

use rusqlite::Connection;

use super::adapters::AdapterError;
use super::capsule::ExecutionCapsuleV1;
use super::policy::StepRecoveryPolicy;
use crate::engine::workspace_ownership::WorkspaceAuthorization;
use crate::persistence::attempts::AttemptRow;
use crate::persistence::leases::IssueLease;
use crate::persistence::run_metadata::RunStatus;
use crate::persistence::wait_state::WaitStateRecord;

use std::path::{Path, PathBuf};

/// Sealed construction token shared by [`RecoveryAuthority`] and
/// [`PreparedRecovery`]. [C4]
///
/// `pub(super)` visibility allows the `prepare` submodule (a direct child of
/// `protocol`) to construct sealed types via `private::Sealed`, while code
/// outside the `protocol` module tree cannot name the token and therefore
/// cannot construct the sealed types.
pub(super) mod private {
    /// Sealed construction token. Instantiated only by the prepare phase.
    #[derive(Debug, Clone, Copy)]
    pub(super) struct Sealed;
}

/// Lease duration for a `Pending` recovery operation claim. [B3]
pub(super) const RECOVERY_LEASE_MINUTES: i64 = 5;

/// The single typed recovery entry point. [C4/B2]
///
/// Carries the caller's `expected_epoch` (the ONLY epoch input) and a
/// CLI-facing `operator_verb`. There is **no** authorization bool:
/// authorization is derived internally via the sealed
/// [`RecoveryAuthority`]. [C4/B2]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P09
/// @requirement:REQ-RP-001
#[derive(Debug, Clone)]
pub struct RecoveryRequest {
    /// The interrupted run to recover.
    pub run_id: String,
    /// The step to recover.
    pub step_id: String,
    /// The caller's view of the current epoch. [B2]
    pub expected_epoch: u64,
    /// CLI-facing operator intent (Resume | Retry | Rewind).
    pub operator_verb: OperatorVerb,
}

/// Sealed authority derived from exact durable state + descriptor-bound
/// [`WorkspaceAuthorization`]. [C4/B6]
///
/// Cannot be constructed outside this module: its fields are private and its
/// only constructor is a `pub(super)` `new` used by the prepare phase. [C4]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P09
/// @requirement:REQ-RP-001
#[derive(Debug)]
pub struct RecoveryAuthority {
    /// Sealed construction token (prevents external construction). [C4]
    _sealed: private::Sealed,
    /// Descriptor-bound workspace authorization. [B6]
    workspace_authorization: Option<WorkspaceAuthorization>,
    /// Exact immutable capsule binding. [C3/C8]
    capsule: ExecutionCapsuleV1,
    /// Exact source attempt (nullable for a fresh run). [C3]
    source_attempt: Option<AttemptRow>,
    /// Resolved step recovery policy. [C6]
    policy: StepRecoveryPolicy,
    /// Concrete recovery strategy selected from the policy. [C4/C6]
    strategy: RecoveryStrategy,
}

impl RecoveryAuthority {
    /// Construct the sealed authority during prepare. [C4/B6]
    pub(super) fn new(
        workspace_authorization: Option<WorkspaceAuthorization>,
        capsule: ExecutionCapsuleV1,
        source_attempt: Option<AttemptRow>,
        policy: StepRecoveryPolicy,
        strategy: RecoveryStrategy,
    ) -> Self {
        Self {
            _sealed: private::Sealed,
            workspace_authorization,
            capsule,
            source_attempt,
            policy,
            strategy,
        }
    }

    /// Borrow the descriptor-bound workspace authorization. [B6]
    #[must_use]
    pub fn workspace_authorization(&self) -> Option<WorkspaceAuthorization> {
        self.workspace_authorization
    }

    /// Borrow the exact immutable capsule. [C3/C8]
    #[must_use]
    pub fn capsule(&self) -> &ExecutionCapsuleV1 {
        &self.capsule
    }

    /// Borrow the exact source attempt, if any. [C3]
    #[must_use]
    pub fn source_attempt(&self) -> Option<&AttemptRow> {
        self.source_attempt.as_ref()
    }

    /// The resolved step recovery policy. [C6]
    #[must_use]
    pub fn policy(&self) -> StepRecoveryPolicy {
        self.policy.clone()
    }

    /// Borrow the concrete recovery strategy. [C4/C6]
    #[must_use]
    pub fn strategy(&self) -> &RecoveryStrategy {
        &self.strategy
    }
}

/// Exact checkpoint identity captured during prepare. [B1]
///
/// Represents the most-recently-rearmed checkpoint for the target step so
/// reserve can reselect/revalidate it inside the transaction. [B1]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P09
/// @requirement:REQ-RP-001
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointIdentity {
    /// The checkpoint id (UUID or equivalent stable identity).
    pub checkpoint_id: String,
    /// The step the checkpoint was captured at.
    pub step_id: String,
}

/// Sealed output of the prepare phase. [C5/B1/B6]
///
/// Preserves the exact authority snapshot (run/status/current-step/live-PID/
/// checkpoint/wait/lease) so reserve can reselect/revalidate before mutation.
/// [B1] Owns a retained [`VerifiedWorkspace`] anchor (OwnedFd-compatible)
/// obtained via `adjudicate_workspace_ownership` so descriptor-relative
/// revalidation can occur without reopening the path. [B6]
///
/// Cannot be constructed outside this module: its fields are private and its
/// only constructor is a `pub(super)` `new` used by the prepare phase. [C4]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P09
/// @requirement:REQ-RP-001
#[derive(Debug)]
pub struct PreparedRecovery {
    /// Sealed construction token (prevents external construction). [C4]
    _sealed: private::Sealed,
    /// The sealed authority. [C4/B6]
    pub(super) authority: RecoveryAuthority,
    /// Caller's expected epoch. [B2]
    pub(super) expected_epoch: u64,
    /// The target step id from the recovery request. [C5/C12]
    pub(super) step_id: String,
    /// The exact operator verb from the recovery request. [C5]
    pub(super) operator_verb: OperatorVerb,
    /// Exact operation id. [B3]
    pub(super) operation_id: String,
    /// Normalized logical request key. [B3]
    pub(super) logical_request_key: String,
    /// Normalized operator-intent binding digest. [B3]
    pub(super) intent_digest: String,
    // [B1] exact authority snapshot captured during prepare:
    /// Exact run status at prepare time. [B1]
    pub(super) run_status: RunStatus,
    /// Exact current step at prepare time. [B1]
    pub(super) current_step: Option<String>,
    /// Exact live PID at prepare time. [B1]
    pub(super) live_pid: Option<u32>,
    /// Exact checkpoint identity at prepare time. [B1]
    pub(super) checkpoint_identity: Option<CheckpointIdentity>,
    /// Exact wait state at prepare time. [B1]
    pub(super) wait_state: Option<WaitStateRecord>,
    /// Exact lease state at prepare time. [B1]
    pub(super) lease: Option<IssueLease>,
    /// [B6] retained anchor for descriptor-relative revalidation when required.
    pub(super) verified_workspace: Option<VerifiedWorkspace>,
    /// Exact workspace path supplied to prepare, used only to reopen for
    /// identity comparison after the retained descriptor is revalidated.
    pub(super) workspace_path: PathBuf,
}

/// Typed input bundle for constructing [`PreparedRecovery`]. [C5/B1/B6]
pub(super) struct PreparedRecoveryParts {
    pub(super) authority: RecoveryAuthority,
    pub(super) expected_epoch: u64,
    pub(super) step_id: String,
    pub(super) operator_verb: OperatorVerb,
    pub(super) operation_id: String,
    pub(super) logical_request_key: String,
    pub(super) intent_digest: String,
    pub(super) run_status: RunStatus,
    pub(super) current_step: Option<String>,
    pub(super) live_pid: Option<u32>,
    pub(super) checkpoint_identity: Option<CheckpointIdentity>,
    pub(super) wait_state: Option<WaitStateRecord>,
    pub(super) lease: Option<IssueLease>,
    pub(super) verified_workspace: Option<VerifiedWorkspace>,
    pub(super) workspace_path: PathBuf,
}

impl PreparedRecovery {
    /// Construct the sealed prepare output during prepare. [C4/B1/B6]
    pub(super) fn new(parts: PreparedRecoveryParts) -> Self {
        Self {
            _sealed: private::Sealed,
            authority: parts.authority,
            expected_epoch: parts.expected_epoch,
            step_id: parts.step_id,
            operator_verb: parts.operator_verb,
            operation_id: parts.operation_id,
            logical_request_key: parts.logical_request_key,
            intent_digest: parts.intent_digest,
            run_status: parts.run_status,
            current_step: parts.current_step,
            live_pid: parts.live_pid,
            checkpoint_identity: parts.checkpoint_identity,
            wait_state: parts.wait_state,
            lease: parts.lease,
            verified_workspace: parts.verified_workspace,
            workspace_path: parts.workspace_path,
        }
    }

    /// Borrow the sealed authority. [C4/B6]
    #[must_use]
    pub fn authority(&self) -> &RecoveryAuthority {
        &self.authority
    }

    /// The caller's expected epoch. [B2]
    #[must_use]
    pub fn expected_epoch(&self) -> u64 {
        self.expected_epoch
    }

    /// The exact operation id. [B3]
    #[must_use]
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    /// The target step id from the recovery request. [C5/C12]
    #[must_use]
    pub fn step_id(&self) -> &str {
        &self.step_id
    }

    /// The exact operator verb from the recovery request. [C5]
    #[must_use]
    pub fn operator_verb(&self) -> OperatorVerb {
        self.operator_verb
    }

    /// The normalized logical request key. [B3]
    #[must_use]
    pub fn logical_request_key(&self) -> &str {
        &self.logical_request_key
    }

    /// The normalized operator-intent binding digest. [B3]
    #[must_use]
    pub fn intent_digest(&self) -> &str {
        &self.intent_digest
    }

    /// Exact run status at prepare time. [B1]
    #[must_use]
    pub fn run_status(&self) -> &RunStatus {
        &self.run_status
    }

    /// Exact current step at prepare time. [B1]
    #[must_use]
    pub fn current_step(&self) -> Option<&str> {
        self.current_step.as_deref()
    }

    /// Exact live PID at prepare time. [B1]
    #[must_use]
    pub fn live_pid(&self) -> Option<u32> {
        self.live_pid
    }

    /// Exact checkpoint identity at prepare time. [B1]
    #[must_use]
    pub fn checkpoint_identity(&self) -> Option<&CheckpointIdentity> {
        self.checkpoint_identity.as_ref()
    }

    /// Exact wait state at prepare time. [B1]
    #[must_use]
    pub fn wait_state(&self) -> Option<&WaitStateRecord> {
        self.wait_state.as_ref()
    }

    /// Exact lease state at prepare time. [B1]
    #[must_use]
    pub fn lease(&self) -> Option<&IssueLease> {
        self.lease.as_ref()
    }

    /// Borrow the retained verified workspace anchor. [B6]
    #[must_use]
    pub fn verified_workspace(&self) -> Option<&VerifiedWorkspace> {
        self.verified_workspace.as_ref()
    }
}

/// Result of recovery dispatch. [C1/C2/B2/B3]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P09
/// @requirement:REQ-RP-001
#[derive(Debug)]
pub enum RecoveryOutcome {
    /// Recovery completed and finalized. [C12]
    Recovered {
        /// The step the run was resumed at.
        resumed_at_step: String,
        /// The durable attempt id of the finalized attempt.
        attempt_id: i64,
        /// The durable operation id. [B3]
        operation_id: String,
    },
    /// An exact completed duplicate was found; the prior outcome is returned.
    /// [C2]
    AlreadyApplied {
        /// The serialized prior outcome. [C2]
        prior_outcome: String,
        /// The durable attempt id of the prior outcome.
        attempt_id: i64,
        /// The durable operation id. [B3]
        operation_id: String,
    },
    /// Recovery refused (fail closed). [C4]
    Refused {
        /// Why the recovery was refused.
        reason: RefusalReason,
    },
    /// The epoch was advanced by a concurrent claim. [C1/B2]
    StaleEpoch {
        /// The current persisted epoch (advanced by a concurrent claim).
        persisted: u64,
        /// The epoch value the caller expected.
        expected: u64,
    },
    /// A conflicting duplicate was found. [C2/B3]
    Conflict {
        /// Detail explaining the conflict.
        detail: String,
    },
}

/// CLI-facing operator intent. [C4]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P09
/// @requirement:REQ-RP-001
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatorVerb {
    /// Resume in-place after exact verification.
    Resume,
    /// Retry the step from scratch (or reconciled state).
    Retry,
    /// Rewind to an earlier checkpoint and re-enter.
    Rewind,
}

/// Reason a recovery is refused. [C2/C4/B6]
///
/// Aligned with P09A semantics, including [`RefusalReason::ConflictingOperation`]
/// for conflicting-duplicate refusals. [C2]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P09
/// @requirement:REQ-RP-001
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefusalReason {
    /// The step is not recoverable. [C6]
    NonRecoverable,
    /// Workspace/effect verification failed.
    VerificationFailed(String),
    /// The caller is not authorized. [C4/B6]
    NotAuthorized,
    /// Legacy run without a valid pre-execution V1 capsule (salvage-only).
    /// [C9]
    SalvageOnly,
    /// A conflicting recovery operation is in progress or was finalized for
    /// the same logical request with different exact bindings. [C2/B3]
    ConflictingOperation,
}

/// Concrete recovery strategy selected from a [`StepRecoveryPolicy`]. [C4/C6]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P09
/// @requirement:REQ-RP-001
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryStrategy {
    /// Resume in-place after exact verification. [B6]
    ContinueWorkspace,
    /// Re-enter the step from scratch.
    Reenter,
    /// Reconcile observed state, then re-enter.
    ReconcileThenReenter,
    /// Undo prior partial effect, then retry.
    CompensateThenRetry,
    /// Recovery refused (fail closed). [C4]
    Refused(RefusalReason),
}

/// Errors produced by the recovery protocol. [B1/B6]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P09
/// @requirement:REQ-RP-001
#[derive(Debug, thiserror::Error)]
pub enum RecoveryError {
    /// Underlying persistence store failure.
    #[error("persistence error: {0}")]
    Persistence(String),
    /// Capsule load/verification failure. [C8]
    #[error("capsule error: {0}")]
    Capsule(String),
    /// Workspace/effect verification failure.
    #[error("verification error: {0}")]
    Verification(String),
    /// A conflicting operation was detected. [C2/B3]
    #[error("operation conflict: {0}")]
    OperationConflict(String),
    /// The workspace is not owned by the run. [B6]
    #[error("workspace not owned")]
    WorkspaceNotOwned,
    /// The durable authority changed between prepare and reserve. [B1]
    #[error("authority changed between prepare and reserve")]
    AuthorityChanged,
    /// The workspace authorization was revoked between prepare and reserve.
    /// [B6]
    #[error("workspace authorization revoked")]
    WorkspaceAuthorizationRevoked,
    /// The injected recovery executor failed or was unavailable. [C5/C12]
    ///
    /// No production default may fabricate success: when the fail-closed
    /// [`UnavailableRecoveryExecutor`] is used, this error surfaces the
    /// unavailability. [C12]
    #[error("recovery execution error: {0}")]
    Execution(String),
}

/// Marker type for the V1 recovery protocol. [C5/C12]
///
/// The protocol owns the phased model (prepare → reserve → execute →
/// finalize). The single entry point is [`Self::recover`], which dispatches
/// exactly one code path and returns a [`RecoveryOutcome`]. [C5/C12]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P09
/// @requirement:REQ-RP-001
#[derive(Debug, Clone, Copy, Default)]
pub struct RecoveryProtocolV1;

impl RecoveryProtocolV1 {
    /// The single typed recovery entry point with a fail-closed executor.
    /// [C5/C12]
    ///
    /// Dispatches exactly one code path, returning a [`RecoveryOutcome`]. The
    /// protocol owns prepare → reserve → execute → finalize and cannot return
    /// [`RecoveryOutcome::Recovered`] before the runner outcome is finalized.
    /// [C5/C12]
    ///
    /// No production default may fabricate success: this entry point uses the
    /// fail-closed [`UnavailableRecoveryExecutor`], so any executable strategy
    /// (Reenter, ContinueWorkspace, ReconcileThenReenter, CompensateThenRetry)
    /// that reaches the execute phase returns [`RecoveryError::Execution`].
    /// Callers needing real execution must use [`Self::recover_with_executor`].
    /// [C12]
    ///
    /// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
    /// @requirement:REQ-RP-001
    pub fn recover(
        &self,
        conn: &Connection,
        workspace: &Path,
        request: &RecoveryRequest,
    ) -> Result<RecoveryOutcome, RecoveryError> {
        self.recover_with_executor(conn, workspace, request, &UnavailableRecoveryExecutor)
    }

    /// Variant of [`Self::recover`] accepting a [`RecoveryPhaseObserver`] and
    /// using a fail-closed executor. [C12]
    ///
    /// The observer is invoked between the prepare and reserve phases so tests
    /// can deterministically mutate durable state to simulate TOCTOU or
    /// authority changes without `cfg(test)` seams. [B1/B6]
    ///
    /// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
    /// @requirement:REQ-RP-001
    pub fn recover_with_observer(
        &self,
        conn: &Connection,
        workspace: &Path,
        request: &RecoveryRequest,
        observer: &dyn RecoveryPhaseObserver,
    ) -> Result<RecoveryOutcome, RecoveryError> {
        self.recover_with_observer_and_executor(
            conn,
            workspace,
            request,
            observer,
            &UnavailableRecoveryExecutor,
        )
    }

    /// Variant of [`Self::recover`] accepting an injected [`RecoveryExecutor`].
    /// [C5/C12]
    ///
    /// The executor is invoked **outside** the SQLite writer transactions
    /// (after reserve commits, before finalize opens its transaction) for
    /// every executable strategy. Refused strategies short-circuit in reserve
    /// and never reach the executor. [C12]
    ///
    /// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
    /// @requirement:REQ-RP-001
    pub fn recover_with_executor(
        &self,
        conn: &Connection,
        workspace: &Path,
        request: &RecoveryRequest,
        executor: &dyn executor::RecoveryExecutor,
    ) -> Result<RecoveryOutcome, RecoveryError> {
        self.recover_with_observer_and_executor(
            conn,
            workspace,
            request,
            &NoOpRecoveryPhaseObserver,
            executor,
        )
    }

    /// Full composition entry point accepting both a [`RecoveryPhaseObserver`]
    /// and an injected [`RecoveryExecutor`]. [C5/C12]
    ///
    /// The observer is invoked between prepare and reserve; the executor is
    /// invoked outside transactions between reserve and finalize. This is the
    /// single dispatch path: every other entry point delegates here. [C12]
    ///
    /// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
    /// @requirement:REQ-RP-001
    pub fn recover_with_observer_and_executor(
        &self,
        conn: &Connection,
        workspace: &Path,
        request: &RecoveryRequest,
        observer: &dyn RecoveryPhaseObserver,
        executor: &dyn executor::RecoveryExecutor,
    ) -> Result<RecoveryOutcome, RecoveryError> {
        let prepared = match prepare::run(conn, workspace, request) {
            Ok(prepared) => prepared,
            Err(RecoveryError::Verification(detail)) => {
                return Ok(RecoveryOutcome::Refused {
                    reason: RefusalReason::VerificationFailed(detail),
                });
            }
            Err(RecoveryError::WorkspaceNotOwned) => {
                return Ok(RecoveryOutcome::Refused {
                    reason: RefusalReason::NotAuthorized,
                });
            }
            Err(error) => return Err(error),
        };

        observer.on_prepare_complete(conn, request);

        let reserved = match reserve::run(conn, &prepared) {
            Ok(reserve::ReserveOutcome::Proceed(reserved)) => reserved,
            Ok(reserve::ReserveOutcome::ShortCircuit(outcome)) => return Ok(outcome),
            Err(
                RecoveryError::WorkspaceAuthorizationRevoked | RecoveryError::WorkspaceNotOwned,
            ) => {
                return Ok(RecoveryOutcome::Refused {
                    reason: RefusalReason::NotAuthorized,
                });
            }
            Err(RecoveryError::Verification(detail)) => {
                return Ok(RecoveryOutcome::Refused {
                    reason: RefusalReason::VerificationFailed(detail),
                });
            }
            Err(error) => return Err(error),
        };

        // Execute phase: external work runs OUTSIDE the reserve transaction
        // (already committed) and before the finalize transaction. [C5/C12]
        let exec_result = execute::run(&prepared, &reserved, workspace, executor)?;

        finalize::run(conn, &prepared, &reserved, exec_result)
    }
}

/// Normalize an [`OperatorVerb`] into a canonical intent string. [B3]
///
/// The normalized intent is used to compute `operation_id`,
/// `logical_request_key`, and `intent_digest`. Normalization ensures that the
/// same operator verb always maps to the same canonical string regardless of
/// future enum growth.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P09
/// @requirement:REQ-RP-001
#[must_use]
pub fn normalize_operator_verb(verb: OperatorVerb) -> &'static str {
    match verb {
        OperatorVerb::Resume => "resume",
        OperatorVerb::Retry => "retry",
        OperatorVerb::Rewind => "rewind",
    }
}

/// Observer hook invoked between recovery protocol phases. [B1/B6]
///
/// This is an architecturally clean dependency-injection seam (not a
/// `cfg(test)` gate) that allows the protocol to notify an observer between
/// the prepare and reserve phases. The default implementation
/// ([`NoOpRecoveryPhaseObserver`]) is a no-op, so production callers are
/// unaffected. Tests use a custom observer to deterministically mutate durable
/// state between phases (simulating TOCTOU and authority changes).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P09
/// @requirement:REQ-RP-001
pub trait RecoveryPhaseObserver: std::fmt::Debug {
    /// Invoked after prepare completes and before reserve begins. [B1/B6]
    ///
    /// The protocol has loaded the capsule and prepared recovery state but has
    /// not yet opened the reserve transaction. An observer may inspect or
    /// mutate durable state here to simulate concurrent changes (TOCTOU,
    /// authority changes).
    ///
    /// The `Connection` is provided so the observer can act on the same
    /// durable store the protocol uses. [B1/B6]
    fn on_prepare_complete(&self, _conn: &Connection, _request: &RecoveryRequest) {}
}

/// Default no-op observer. [B1/B6]
///
/// Used by [`RecoveryProtocolV1::recover`] (the non-observer entry point).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P09
/// @requirement:REQ-RP-001
#[derive(Debug, Clone, Copy, Default)]
pub struct NoOpRecoveryPhaseObserver;

impl RecoveryPhaseObserver for NoOpRecoveryPhaseObserver {}

use crate::engine::workspace_ownership::VerifiedWorkspace;

/// Reserved state produced by reserve, consumed by execute and finalize.
pub(super) struct ReservedRecovery {
    pub(super) operation_id: String,
    pub(super) attempt_id: i64,
    pub(super) epoch: u64,
}

/// Map a rusqlite error into a [`RecoveryError::Persistence`]. [B1]
pub(super) fn map_persist(context: &str) -> impl Fn(rusqlite::Error) -> RecoveryError + '_ {
    let ctx = context.to_string();
    move |e: rusqlite::Error| RecoveryError::Persistence(format!("{ctx}: {e}"))
}

/// Map an [`AdapterError`] into a [`RecoveryError::Capsule`]. [C8]
pub(super) fn map_adapter() -> impl Fn(AdapterError) -> RecoveryError {
    |e: AdapterError| RecoveryError::Capsule(e.to_string())
}

/// Extract the run id from a prepared recovery.
///
/// Reserve and finalize need the run id; the canonical source is the
/// authority's capsule, which carries the exact immutable binding. [C3/C8]
pub(super) fn run_id_of(prepared: &PreparedRecovery) -> &str {
    &prepared.authority.capsule.run_id
}

/// Extract the exact request step id from a prepared recovery.
///
/// Reserve and finalize need the step id; the canonical source is the request's
/// `step_id` retained on [`PreparedRecovery`], never inferred from `run_id`.
/// [C5/C12]
pub(super) fn step_id_of(prepared: &PreparedRecovery) -> &str {
    &prepared.step_id
}

/// Derive a stable `step@timestamp` checkpoint identity from a loaded
/// checkpoint. [B1]
///
/// Shared by the prepare and reserve phases so both compute the identity with
/// identical logic, ensuring exact-equality comparison is meaningful.
pub(super) fn checkpoint_identity_of(
    checkpoint: &crate::persistence::checkpoint::Checkpoint,
) -> CheckpointIdentity {
    CheckpointIdentity {
        checkpoint_id: format!(
            "{}@{}",
            checkpoint.step_id,
            checkpoint.timestamp.to_rfc3339()
        ),
        step_id: checkpoint.step_id.clone(),
    }
}

/// Determine the source attempt id captured during prepare, if any. [C3]
pub(super) fn source_attempt_id_of(prepared: &PreparedRecovery) -> Option<i64> {
    prepared
        .authority
        .source_attempt
        .as_ref()
        .map(|a| a.attempt_id)
}
