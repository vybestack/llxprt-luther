# Phase 05A: Epoch + Operations + Append-Only Implementation Verification

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P05A`

## Prerequisites

- Required: P05 completed.

## Verification Commands

```bash
set -euo pipefail
cargo test || exit 1
cargo clippy --workspace --all-targets --all-features -- -D warnings || exit 1
grep -rn -E "(todo!|unimplemented!|TODO|FIXME|HACK|placeholder|not yet|will be)" workflow/src/persistence/recovery_epoch.rs workflow/src/persistence/recovery_operations.rs workflow/src/persistence/attempts.rs workflow/src/persistence/effect_intents.rs && { echo "FAIL: placeholder tokens found"; exit 1; } || true
# Confirm no UPDATE of existing attempt rows (epoch CAS UPDATE is on recovery_epoch, not recovery_attempts)
# The only allowed mutation on recovery_attempts is append_attempt_outcome's guarded finalize UPDATE.
grep -rn "UPDATE recovery_attempts" workflow/src/persistence/attempts.rs | grep -v "finalized_at IS NULL" && { echo "FAIL: unguarded UPDATE on append-only table"; exit 1; } || true
# Confirm epoch CAS uses conditional WHERE clause
grep -rn "WHERE.*epoch" workflow/src/persistence/recovery_epoch.rs
```

## Semantic Verification Checklist

1. **Is the epoch distinct from attempts?** `read_epoch` reads `recovery_epoch`,
   not `MAX(generation)` from `recovery_attempts`. [verified] [C1]
2. **Is the CAS real?** `cas_advance_epoch` uses conditional UPDATE with
   `WHERE epoch = ?` and checks affected rows. [verified] [C1]
3. **Are operation transitions guarded?** `finalize_*` uses
   `WHERE status = 'pending'` + affected-row check. [verified] [C2]
4. **Are attempt rows truly append-only?** No `UPDATE recovery_attempts`
   statement exists. New rows carry complete `StateSnapshot` + capsule binding
   + snapshot digest. [verified] [C3]
5. **Are effect intents recorded before issuance?** `prepare_effect` runs before
   the effect; the digest is computed in-record; guarded finalize. [verified] [C7]
6. **No in-memory facade?** The store is durable from the start. [verified]

### Integration Points Verified

- [ ] `read_epoch` / `cas_advance_epoch` read/write the real `recovery_epoch` table.
- [ ] `lookup_operation` / `insert_pending` / `finalize_*` read/write the real
      `recovery_operations` table.
- [ ] `record_attempt_start` / `append_attempt_outcome` / `latest_for_step`
      read/write the real `recovery_attempts` table.
- [ ] Effect intents are in a separate table (`effect_intents`) from attempts.
- [ ] `RETURNING` clause used consistent with `src/persistence/leases.rs`.

### Edge Cases Verified (via P04 tests)

- [ ] Epoch CAS stale returns persisted value.
- [ ] Operation guard fails on double-finalize.
- [ ] Append preserves history (original row unchanged).
- [ ] Reconcile distinguishes Completed/Conflict/NeedsReissue.

## Holistic Functionality Assessment (at completion)

- What was implemented: [durable epoch CAS + operation ledger + append-only store with complete StateSnapshot + effect-intent state machine]
- Does it satisfy REQ-RP-003/004/008? [per requirement]
- Data flow: cas_advance_epoch → durable epoch; insert_pending → durable operation; append_attempt → durable row with complete state; prepare_effect → durable row + digest
- Verdict: [PASS/FAIL]

## Failure Recovery

Two-cycle cap on semantic review.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P05A.md`
