//! Execute phase: invoke the injected [`RecoveryExecutor`] outside the
//! transaction. [C5/C12]
//!
//! The protocol calls [`run`] **after** reserve commits (so the epoch CAS and
//! durable attempt allocation are durable) and **before** finalize opens its
//! transaction. No external work happens inside a SQLite writer transaction.
//! [C5/C12]
//!
//! No production default may fabricate success: the executor is always injected
//! (the plain entry points use [`UnavailableRecoveryExecutor`]), and this
//! module returns whatever the executor produces. [C12]
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
//! @requirement:REQ-RP-001

use std::path::Path;

use super::executor::{
    map_execution_error, RecoveryExecutionInvocation, RecoveryExecutionResult, RecoveryExecutor,
};
use super::{run_id_of, PreparedRecovery, RecoveryError, ReservedRecovery};

/// Invoke the injected executor outside the reserve transaction. [C5/C12]
///
/// The invocation borrows the exact prepared authority (capsule, strategy) and
/// the reserved durable ids (epoch, attempt). The executor runs external work;
/// its typed result is handed to finalize, which appends it to the durable
/// attempt row in a short transaction. [C5/C12/B4]
pub(super) fn run(
    prepared: &PreparedRecovery,
    reserved: &ReservedRecovery,
    workspace: &Path,
    executor: &dyn RecoveryExecutor,
) -> Result<RecoveryExecutionResult, RecoveryError> {
    let invocation = RecoveryExecutionInvocation {
        run_id: run_id_of(prepared),
        step_id: &prepared.step_id,
        operation_id: &reserved.operation_id,
        attempt_id: reserved.attempt_id,
        epoch: reserved.epoch,
        strategy: prepared.authority.strategy.clone(),
        capsule: prepared.authority.capsule(),
        workspace,
    };
    executor.execute(&invocation).map_err(map_execution_error())
}
