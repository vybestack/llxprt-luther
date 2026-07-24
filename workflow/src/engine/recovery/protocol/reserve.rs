//! Reserve phase: IMMEDIATE transaction revalidation, CAS, and allocation.
//! [B1/B2/B3/B4]
//!
//! Revalidates the epoch and authority snapshot, resolves duplicates
//! (Completed → AlreadyApplied, Conflict → Refused, Pending → adopt), performs
//! the single epoch CAS, allocates the attempt, and inserts the Pending
//! operation. [C1/C2/B2/B3/B4]

use chrono::Utc;
use rusqlite::{Connection, TransactionBehavior};

use crate::engine::workspace_ownership::{adjudicate_workspace_ownership, OwnershipVerdict};
use crate::persistence::attempts::{record_attempt_start, AttemptStart};
use crate::persistence::checkpoint::StateSnapshot;
use crate::persistence::recovery_epoch::{cas_advance_epoch, read_epoch, CasOutcome};
use crate::persistence::recovery_operations::{
    insert_pending, lookup_logical_operation, try_adopt_pending, AdoptOutcome, OperationStatus,
    PendingOperationInsert, RecoveryOperation,
};
use crate::persistence::wait_state::get_wait_state;

use super::{
    checkpoint_identity_of, map_persist, run_id_of, source_attempt_id_of, step_id_of,
    CheckpointIdentity, PreparedRecovery, RecoveryError, RecoveryOutcome, RecoveryStrategy,
    RefusalReason, ReservedRecovery, RECOVERY_LEASE_MINUTES,
};

/// Intermediate outcome from reserve that may short-circuit before execute.
pub(super) enum ReserveOutcome {
    /// Reserve succeeded; proceed to execute + finalize.
    Proceed(ReservedRecovery),
    /// A durable outcome was reached without execution (AlreadyApplied,
    /// StaleEpoch, Refused, Conflict).
    ShortCircuit(RecoveryOutcome),
}

/// Result of the epoch CAS: either the new epoch or a StaleEpoch short-circuit.
enum CasResolution {
    /// CAS advanced to the new epoch.
    Advanced(u64),
    /// CAS found a stale epoch; short-circuit with the persisted value.
    StaleEpoch(RecoveryOutcome),
}

/// Resolution of an existing operation for the logical request. [C2/B3]
///
/// Distinguishes "no existing operation" (fresh reservation path) from
/// "expired-lease Pending adopted" (proceed with existing operation/attempt/
/// epoch without a second CAS or duplicate insert) from "short-circuit
/// outcome" (AlreadyApplied/StaleEpoch/Refused/Conflict). [C2/B3]
enum ExistingResolution {
    /// No existing operation found; proceed with fresh CAS + allocate + insert.
    NotFound,
    /// A short-circuit outcome was reached (AlreadyApplied, Refused, etc.).
    ShortCircuit(RecoveryOutcome),
    /// An expired-lease Pending was adopted inside the transaction; proceed
    /// with the existing operation_id, attempt_id, and epoch. The adoption
    /// mutation (owner/lease update) is already applied; commit to persist it.
    /// No second CAS or duplicate insert occurs. [B3]
    AdoptedPending(ReservedRecovery),
}

/// Run the reserve phase inside an IMMEDIATE transaction. [B1/B2/B3/B4]
///
/// Resolves existing operations (Completed → AlreadyApplied, Conflict →
/// Refused, Pending with expired lease → adopt and proceed with existing
/// details), then revalidates the epoch and authority snapshot, performs the
/// single epoch CAS (only for fresh reservations), allocates the attempt, and
/// inserts the Pending operation. [C1/C2/B2/B3/B4]
pub(super) fn run(
    conn: &Connection,
    prepared: &PreparedRecovery,
) -> Result<ReserveOutcome, RecoveryError> {
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(map_persist("begin reserve tx"))?;

    let existing_resolution = resolve_existing_operation(&tx, prepared)?;

    // Short-circuit immediately for finalized operations (Completed/Conflict/
    // live-Pending). These return regardless of epoch or authority state.
    if let ExistingResolution::ShortCircuit(outcome) = existing_resolution {
        return Ok(ReserveOutcome::ShortCircuit(outcome));
    }

    // For NotFound and AdoptedPending, always revalidate epoch + authority
    // before any execution. The adoption mutation (if any) is inside this tx
    // and will be rolled back if revalidation fails.
    if let Some(outcome) = revalidate_epoch(&tx, prepared)? {
        return Ok(ReserveOutcome::ShortCircuit(outcome));
    }

    revalidate_authority_snapshot(&tx, prepared)?;
    revalidate_continue_workspace_authorization(prepared)?;

    if let Some(outcome) = check_strategy_refusal(prepared) {
        return Ok(ReserveOutcome::ShortCircuit(outcome));
    }

    match existing_resolution {
        ExistingResolution::NotFound => {
            // Fresh reservation: single CAS + allocate attempt + insert pending.
            let new_epoch = match perform_epoch_cas(&tx, prepared)? {
                CasResolution::Advanced(epoch) => epoch,
                CasResolution::StaleEpoch(outcome) => {
                    return Ok(ReserveOutcome::ShortCircuit(outcome));
                }
            };
            let attempt_id = allocate_attempt(&tx, prepared, new_epoch)?;
            insert_pending_operation(&tx, prepared, new_epoch, attempt_id)?;
            tx.commit().map_err(map_persist("commit reserve"))?;
            Ok(ReserveOutcome::Proceed(ReservedRecovery {
                operation_id: prepared.operation_id.clone(),
                attempt_id,
                epoch: new_epoch,
            }))
        }
        ExistingResolution::AdoptedPending(reserved) => {
            // The adoption mutation (owner/lease update) is already applied
            // inside this tx. Commit to persist it, then proceed with the
            // existing operation/attempt/epoch — no second CAS, no duplicate
            // insert. [B3]
            tx.commit().map_err(map_persist("commit adoption"))?;
            Ok(ReserveOutcome::Proceed(reserved))
        }
        ExistingResolution::ShortCircuit(_) => unreachable!(
            "ShortCircuit is handled by the early return above; this branch is unreachable"
        ),
    }
}

/// Revalidate the epoch: short-circuit with StaleEpoch if advanced. [C1/B2]
fn revalidate_epoch(
    tx: &Connection,
    prepared: &PreparedRecovery,
) -> Result<Option<RecoveryOutcome>, RecoveryError> {
    let persisted_epoch = read_epoch(tx, run_id_of(prepared)).map_err(map_persist("read epoch"))?;
    if persisted_epoch != prepared.expected_epoch {
        return Ok(Some(RecoveryOutcome::StaleEpoch {
            persisted: persisted_epoch,
            expected: prepared.expected_epoch,
        }));
    }
    Ok(None)
}

/// Revalidate the authority snapshot: reload every captured field inside the
/// IMMEDIATE transaction and compare for exact equality before any mutation.
/// [B1]
///
/// Returns [`RecoveryError::AuthorityChanged`] on any mismatch — covering
/// `run_status`, `current_step`, `live_pid` (process_pid), `checkpoint
/// identity`, `wait_state`, and `issue lease`. Both `Some`/`None` directions
/// are compared exactly: a field that was `Some` at prepare and is `None` at
/// reserve (or vice versa) is an authority change.
fn revalidate_authority_snapshot(
    tx: &Connection,
    prepared: &PreparedRecovery,
) -> Result<(), RecoveryError> {
    let current_metadata = get_run_for_reserve(tx, prepared)?;
    if current_metadata.status != prepared.run_status {
        return Err(RecoveryError::AuthorityChanged);
    }
    if current_metadata.current_step != prepared.current_step {
        return Err(RecoveryError::AuthorityChanged);
    }
    if current_metadata.process_pid != prepared.live_pid {
        return Err(RecoveryError::AuthorityChanged);
    }
    let current_checkpoint = load_reserve_checkpoint_identity(tx, run_id_of(prepared))?;
    if current_checkpoint != prepared.checkpoint_identity {
        return Err(RecoveryError::AuthorityChanged);
    }
    let current_wait = get_wait_state(tx, run_id_of(prepared))
        .map_err(map_persist("get wait state in reserve"))?;
    if current_wait != prepared.wait_state {
        return Err(RecoveryError::AuthorityChanged);
    }
    let current_lease = load_reserve_lease(tx, &current_metadata);
    if current_lease != prepared.lease {
        return Err(RecoveryError::AuthorityChanged);
    }
    Ok(())
}

/// Derive the checkpoint identity inside the reserve transaction. [B1]
fn load_reserve_checkpoint_identity(
    tx: &Connection,
    run_id: &str,
) -> Result<Option<CheckpointIdentity>, RecoveryError> {
    let Some(checkpoint) = crate::persistence::checkpoint::load_checkpoint_with_conn(tx, run_id)
        .map_err(|e| RecoveryError::Persistence(format!("load checkpoint in reserve: {e}")))?
    else {
        return Ok(None);
    };
    Ok(Some(checkpoint_identity_of(&checkpoint)))
}

/// Load the matching lease inside the reserve transaction using the same
/// resolution as prepare. [B1]
///
/// Mirrors [`super::prepare::load_issue_lease`]: the primary lookup is by
/// `run_id`; when that returns `Ok(None)` we fall back to the issue lookup so
/// the authority snapshot captures the logically-covering lease.
fn load_reserve_lease(
    tx: &Connection,
    metadata: &crate::persistence::RunMetadata,
) -> Option<crate::persistence::leases::IssueLease> {
    let repo = metadata.repository.as_deref()?;
    let issue_number = metadata.issue_lease_number()?;
    match crate::persistence::leases::get_lease_for_run(tx, &metadata.run_id) {
        Ok(Some(lease)) => Some(lease),
        Ok(None) => crate::persistence::leases::get_lease_for_issue(tx, repo, issue_number)
            .ok()
            .flatten(),
        Err(_) => crate::persistence::leases::get_lease_for_issue(tx, repo, issue_number)
            .ok()
            .flatten(),
    }
}

/// Strategy refusal (fail-closed policy). [C4/C6]
fn check_strategy_refusal(prepared: &PreparedRecovery) -> Option<RecoveryOutcome> {
    if let RecoveryStrategy::Refused(reason) = &prepared.authority.strategy {
        return Some(RecoveryOutcome::Refused {
            reason: reason.clone(),
        });
    }
    None
}

/// Perform the single epoch CAS at reserve. [C1/B2]
fn perform_epoch_cas(
    tx: &Connection,
    prepared: &PreparedRecovery,
) -> Result<CasResolution, RecoveryError> {
    let cas = cas_advance_epoch(tx, run_id_of(prepared), prepared.expected_epoch)
        .map_err(map_persist("CAS epoch"))?;
    match cas {
        CasOutcome::Advanced { to, .. } => Ok(CasResolution::Advanced(to)),
        CasOutcome::Stale { persisted, .. } => {
            Ok(CasResolution::StaleEpoch(RecoveryOutcome::StaleEpoch {
                persisted,
                expected: prepared.expected_epoch,
            }))
        }
    }
}

/// Allocate the durable attempt at reserve (before any effect). [B4]
fn allocate_attempt(
    tx: &Connection,
    prepared: &PreparedRecovery,
    new_epoch: u64,
) -> Result<i64, RecoveryError> {
    let state_snapshot = StateSnapshot::default();
    record_attempt_start(
        tx,
        &AttemptStart {
            run_id: run_id_of(prepared),
            epoch: new_epoch,
            source_attempt_id: source_attempt_id_of(prepared),
            operation_id: &prepared.operation_id,
            step_id: step_id_of(prepared),
            capsule_schema_version: prepared.authority.capsule.schema_version,
            capsule_envelope_digest: &prepared.authority.capsule.envelope_digest,
            state_snapshot: &state_snapshot,
        },
    )
    .map_err(map_persist("record attempt start"))
}

/// Insert the Pending operation with a guarded owner/lease claim. [B3]
fn insert_pending_operation(
    tx: &Connection,
    prepared: &PreparedRecovery,
    new_epoch: u64,
    attempt_id: i64,
) -> Result<(), RecoveryError> {
    let owner_pid = std::process::id();
    let lease_expires_at = Utc::now() + chrono::Duration::minutes(RECOVERY_LEASE_MINUTES);
    insert_pending(
        tx,
        &PendingOperationInsert {
            operation_id: prepared.operation_id.clone(),
            run_id: run_id_of(prepared).to_string(),
            epoch: new_epoch,
            step_id: step_id_of(prepared).to_string(),
            capsule_envelope_digest: prepared.authority.capsule.envelope_digest.clone(),
            source_attempt_id: source_attempt_id_of(prepared),
            logical_request_key: prepared.logical_request_key.clone(),
            intent_digest: prepared.intent_digest.clone(),
            owner_pid,
            lease_expires_at,
            execution_attempt_id: attempt_id,
        },
    )
    .map_err(map_persist("insert pending"))
}

/// Resolve an existing operation for the logical request, producing a
/// resolution that may short-circuit, adopt-and-proceed, or proceed fresh.
/// [C2/B3]
///
/// Returns [`ExistingResolution::NotFound`] when no existing operation blocks
/// reservation (fresh path). Returns [`ExistingResolution::AdoptedPending`]
/// when an expired-lease Pending operation was adopted and execution should
/// proceed with the existing operation/attempt/epoch. Returns
/// [`ExistingResolution::ShortCircuit`] for Completed/Conflict/live-Pending.
fn resolve_existing_operation(
    tx: &Connection,
    prepared: &PreparedRecovery,
) -> Result<ExistingResolution, RecoveryError> {
    let existing = lookup_logical_operation(tx, &prepared.logical_request_key)
        .map_err(map_persist("lookup logical operation"))?;
    let Some(existing) = existing else {
        return Ok(ExistingResolution::NotFound);
    };
    resolve_existing_status(tx, prepared, &existing)
}

/// Dispatch on the existing operation status to produce a resolution.
fn resolve_existing_status(
    tx: &Connection,
    prepared: &PreparedRecovery,
    existing: &RecoveryOperation,
) -> Result<ExistingResolution, RecoveryError> {
    match existing.status {
        OperationStatus::Completed => resolve_completed(prepared, existing),
        OperationStatus::Refused | OperationStatus::Conflict => {
            resolve_refused_or_conflict(prepared, existing)
        }
        OperationStatus::Pending => resolve_pending(tx, prepared, existing),
    }
}

/// Resolve a Completed operation: AlreadyApplied on matching binding, else
/// conflict. [C2]
///
/// Fails closed with [`RecoveryError::Persistence`] when a matched Completed
/// operation has no `execution_attempt_id` — a completed operation must always
/// have one (allocated at reserve and finalized with the attempt in the
/// serialized outcome). A missing attempt id signals ledger corruption.
fn resolve_completed(
    prepared: &PreparedRecovery,
    existing: &RecoveryOperation,
) -> Result<ExistingResolution, RecoveryError> {
    if existing.capsule_envelope_digest != prepared.authority.capsule.envelope_digest {
        return Ok(ExistingResolution::ShortCircuit(RecoveryOutcome::Refused {
            reason: RefusalReason::ConflictingOperation,
        }));
    }
    let attempt_id = existing.execution_attempt_id.ok_or_else(|| {
        RecoveryError::Persistence(format!(
            "completed operation '{}' has no execution_attempt_id (ledger corruption)",
            existing.operation_id
        ))
    })?;
    Ok(ExistingResolution::ShortCircuit(
        RecoveryOutcome::AlreadyApplied {
            prior_outcome: existing.serialized_outcome.clone().unwrap_or_default(),
            attempt_id,
            operation_id: existing.operation_id.clone(),
        },
    ))
}

/// Resolve a finalized Refused or Conflict operation. [C2/B3]
///
/// On **matching** capsule binding, the prior terminal outcome is replayed as
/// a short-circuit — the same logical request was already refused/conflicted
/// with the same exact bindings, so the decision stands. Re-running the
/// recovery (fresh CAS + insert_pending) would be incorrect: it would ignore
/// the durable terminal decision and silently re-execute.
///
/// On **mismatched** capsule binding, the existing operation is for a
/// different exact operation on the same logical request — a conflicting
/// operation refusal is returned.
fn resolve_refused_or_conflict(
    prepared: &PreparedRecovery,
    existing: &RecoveryOperation,
) -> Result<ExistingResolution, RecoveryError> {
    if existing.capsule_envelope_digest != prepared.authority.capsule.envelope_digest {
        return Ok(ExistingResolution::ShortCircuit(RecoveryOutcome::Refused {
            reason: RefusalReason::ConflictingOperation,
        }));
    }
    // Matching binding: replay the persisted terminal outcome.
    let terminal_outcome = existing.serialized_outcome.clone().unwrap_or_default();
    Ok(ExistingResolution::ShortCircuit(match existing.status {
        OperationStatus::Refused => RecoveryOutcome::Refused {
            reason: persisted_refusal_reason(&terminal_outcome),
        },
        OperationStatus::Conflict => RecoveryOutcome::Conflict {
            detail: terminal_outcome,
        },
        // Unreachable: this function is only called for Refused | Conflict.
        _ => RecoveryOutcome::Conflict {
            detail: "unexpected terminal status in resolve_refused_or_conflict".to_string(),
        },
    }))
}

/// Parse a persisted refusal detail back into a [`RefusalReason`].
///
/// The `serialized_outcome` of a finalized Refused operation stores the
/// human-readable refusal detail (set by the protocol). We map the detail back
/// to the closest matching [`RefusalReason`] so callers see a typed refusal
/// rather than a generic conflict.
fn persisted_refusal_reason(detail: &str) -> RefusalReason {
    if detail.contains("not recoverable") {
        RefusalReason::NonRecoverable
    } else if detail.contains("not authorized") {
        RefusalReason::NotAuthorized
    } else if detail.contains("salvage") {
        RefusalReason::SalvageOnly
    } else if detail.contains("conflicting") {
        RefusalReason::ConflictingOperation
    } else {
        RefusalReason::VerificationFailed(detail.to_string())
    }
}

/// Resolve a Pending operation: adopt if lease expired, else reconcile by not
/// duplicating. [B3]
fn resolve_pending(
    tx: &Connection,
    prepared: &PreparedRecovery,
    existing: &RecoveryOperation,
) -> Result<ExistingResolution, RecoveryError> {
    if existing.capsule_envelope_digest != prepared.authority.capsule.envelope_digest {
        return Ok(ExistingResolution::ShortCircuit(RecoveryOutcome::Refused {
            reason: RefusalReason::ConflictingOperation,
        }));
    }

    let now = Utc::now();
    let new_pid = std::process::id();
    let new_lease = now + chrono::Duration::minutes(RECOVERY_LEASE_MINUTES);
    let adopt = try_adopt_pending(tx, &existing.operation_id, new_pid, new_lease, now)
        .map_err(map_persist("try adopt pending"))?;
    resolve_adopt_outcome(existing, adopt)
}

/// Map the adoption outcome to a reserve resolution. [B3]
///
/// When a pending operation's lease has expired and is successfully adopted,
/// returns [`ExistingResolution::AdoptedPending`] carrying the existing
/// operation_id, attempt_id, and epoch so the caller proceeds with execution
/// without a second CAS or duplicate insert. When the lease is still live
/// (still owned by another recoverer), returns a short-circuit
/// [`RecoveryOutcome::AlreadyApplied`]. [B3]
///
/// Fails closed with [`RecoveryError::Persistence`] when a matched Pending
/// operation has no `execution_attempt_id` — a pending operation allocated at
/// reserve must always have one. A missing attempt id signals ledger
/// corruption.
fn resolve_adopt_outcome(
    existing: &RecoveryOperation,
    adopt: AdoptOutcome,
) -> Result<ExistingResolution, RecoveryError> {
    let attempt_id = existing.execution_attempt_id.ok_or_else(|| {
        RecoveryError::Persistence(format!(
            "pending operation '{}' has no execution_attempt_id (ledger corruption)",
            existing.operation_id
        ))
    })?;
    match adopt {
        AdoptOutcome::Adopted => Ok(ExistingResolution::AdoptedPending(ReservedRecovery {
            operation_id: existing.operation_id.clone(),
            attempt_id,
            epoch: existing.epoch,
        })),
        AdoptOutcome::StillOwned => Ok(ExistingResolution::ShortCircuit(
            RecoveryOutcome::AlreadyApplied {
                prior_outcome: existing.serialized_outcome.clone().unwrap_or_default(),
                attempt_id,
                operation_id: existing.operation_id.clone(),
            },
        )),
    }
}

/// Revalidate descriptor-bound authorization for ContinueWorkspace. [B6]
fn revalidate_continue_workspace_authorization(
    prepared: &PreparedRecovery,
) -> Result<(), RecoveryError> {
    if prepared.authority.strategy != RecoveryStrategy::ContinueWorkspace {
        return Ok(());
    }
    let verified = prepared
        .verified_workspace
        .as_ref()
        .ok_or(RecoveryError::WorkspaceAuthorizationRevoked)?;
    if Some(verified.authorization()) != prepared.authority.workspace_authorization {
        return Err(RecoveryError::WorkspaceAuthorizationRevoked);
    }
    reverify_workspace_marker_for_continue(prepared)
}

/// Re-verify the workspace ownership marker for ContinueWorkspace reserve. [B6]
fn reverify_workspace_marker_for_continue(
    prepared: &PreparedRecovery,
) -> Result<(), RecoveryError> {
    match adjudicate_workspace_ownership(&prepared.workspace_path, run_id_of(prepared)) {
        OwnershipVerdict::Owned(current)
            if Some(current.authorization()) == prepared.authority.workspace_authorization =>
        {
            Ok(())
        }
        OwnershipVerdict::Owned(_)
        | OwnershipVerdict::NoEvidence
        | OwnershipVerdict::Rejected(_) => Err(RecoveryError::WorkspaceAuthorizationRevoked),
    }
}

/// Load run metadata for reserve-time authority revalidation. [B1]
fn get_run_for_reserve(
    tx: &Connection,
    prepared: &PreparedRecovery,
) -> Result<crate::persistence::RunMetadata, RecoveryError> {
    crate::persistence::sqlite::get_run_with_conn(tx, run_id_of(prepared))
        .map_err(map_persist("get run in reserve"))?
        .ok_or_else(|| {
            RecoveryError::Persistence(format!("run not found in reserve: {}", run_id_of(prepared)))
        })
}
