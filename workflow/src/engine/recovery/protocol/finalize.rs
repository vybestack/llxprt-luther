//! Finalize phase: short transaction to append the attempt outcome and
//! finalize the operation as Completed. [C5/C12]
//!
//! Returns `Recovered` only after commit. The outcome fields appended are the
//! truthful [`RecoveryExecutionResult`] produced by the injected executor —
//! no production default fabricates success. [C12]
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
//! @requirement:REQ-RP-001

use rusqlite::{Connection, TransactionBehavior};

use crate::persistence::attempts::append_attempt_outcome;
use crate::persistence::recovery_operations::finalize_completed;

use super::executor::RecoveryExecutionResult;
use super::{
    map_persist, step_id_of, PreparedRecovery, RecoveryError, RecoveryOutcome, ReservedRecovery,
};

/// Run the finalize phase in a short transaction: append the attempt outcome
/// and finalize the operation as Completed. Returns `Recovered` only after
/// commit. [C5/C12]
pub(super) fn run(
    conn: &Connection,
    prepared: &PreparedRecovery,
    reserved: &ReservedRecovery,
    exec_result: RecoveryExecutionResult,
) -> Result<RecoveryOutcome, RecoveryError> {
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(map_persist("begin finalize tx"))?;

    append_attempt_outcome(
        &tx,
        reserved.attempt_id,
        &exec_result.step_status,
        &exec_result.state_snapshot,
        exec_result.runner_result.as_ref(),
        None,
    )
    .map_err(map_persist("append attempt outcome"))?;

    let serialized_outcome = serde_json::json!({
        "attempt_id": reserved.attempt_id,
        "step_status": exec_result.step_status,
        "status": "completed",
    })
    .to_string();
    finalize_completed(&tx, &reserved.operation_id, &serialized_outcome)
        .map_err(map_persist("finalize completed"))?;

    tx.commit().map_err(map_persist("commit finalize"))?;

    Ok(RecoveryOutcome::Recovered {
        resumed_at_step: step_id_of(prepared).to_string(),
        attempt_id: reserved.attempt_id,
        operation_id: reserved.operation_id.clone(),
    })
}
