# Phase 05: Epoch CAS + Operations Ledger + Append-Only Implementation (Milestone 1 Complete)

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P05`

## Prerequisites

- Required: P04A completed with PASS (red phase confirmed).

## Purpose

Implement the durable epoch CAS, operation ledger, append-only attempt store, and
effect-intent state machine to make all P04 tests pass, following pseudocode:
- epoch pseudocode lines 09–41 [C1]
- operations pseudocode lines 22–149 [C2/B3]
- attempts pseudocode lines 23–93 [C3/B4]
- intents pseudocode lines 20–151 [C7/B5]

This store is durable from the start; no in-memory facade is introduced or
replaced. `RecoveryProtocolV1` (P09–P11) will later call these same durable
functions.

> **Pseudocode references are to `02-pseudocode.md` component-local line
> numbers (B13).**

## Requirements Implemented

### REQ-RP-003: Append-only immutable attempt IDs with complete state [C3/B4]

**Implementation** (attempts pseudocode 23–93):
- `record_attempt_start`: INSERT with complete `StateSnapshot` JSON, capsule binding,
  snapshot digest, `operation_id`, `started_at`, `finalized_at = NULL`. Returns the
  new attempt_id via RETURNING. [B4]
- `append_attempt_outcome`: guarded UPDATE (`WHERE finalized_at IS NULL`) that
  completes a row inserted at reserve; sets step_status, runner_result_json,
  checkpoint_digest, finalized_at. [B4]
- `load_unfinalized_for_operation`: SELECT by operation_id WHERE finalized_at IS
  NULL (crash recovery). [B4]
- `latest_for_step`: SELECT by (run, step) ORDER BY attempt_id DESC LIMIT 1.
- `verify_snapshot_digest`: recompute SHA-256 of canonical StateSnapshot
  serialization, compare.

### REQ-RP-004: Epoch CAS + operation ledger (durable half) [C1/C2/B3]

**Implementation** (epoch pseudocode 17–36, operations pseudocode 22–149):
- `cas_advance_epoch`: conditional UPDATE with `WHERE epoch = ?expected`,
  affected-row check, returns `Advanced` or `Stale`. [C1/B2: this is the ONLY CAS]
- `compute_operation_id`: SHA-256 over exact bindings (durable row identity). [B3]
- `compute_logical_request_key`: SHA-256 over normalized logical bindings
  (uniqueness/conflict binding). [B3]
- `find_adoptable_pending`: SELECT WHERE logical_request_key matches AND lease
  expired. [B3]
- `try_adopt_pending`: guarded UPDATE of owner_pid/lease WHERE lease still expired. [B3]
- `insert_pending`: INSERT new operation row with logical_request_key, owner_pid,
  lease_expires_at, execution_attempt_id. [B3/B4]
- `finalize_completed/refused/conflict`: guarded UPDATE
  (`WHERE status = 'pending'`), affected-row check, returns `GuardFailed` if
  affected != 1. [C2]

### REQ-RP-008: Effect-intent state machine [C7/B5]

**Implementation** (intents pseudocode 34–151):
- `compute_effect_key`: SHA-256 over operation_id + attempt_id + sequence + kind. [C7]
- `prepare_effect`: canonicalize payload, compute digest, INSERT-or-LOAD with
  exact-binding comparison (payload_digest, expected_target, expected_predecessor).
  On mismatch → conflict state + `BindingConflict` error. On match → return
  existing intent. [B5]
- `reconcile_effect`: dispatch per kind (commit/push/open_pr/merge), compare
  observed to expected_target/predecessor. [C7]
- `finalize_effect`: guarded UPDATE (`WHERE status = 'prepared'`), affected-row
  check. [C7]

## Implementation Tasks

### Files to Modify

- `src/persistence/recovery_epoch.rs`
  - Implement `read_epoch` and `cas_advance_epoch` per epoch pseudocode 09–41.
  - Use `INSERT ... ON CONFLICT(run_id) DO UPDATE SET ... WHERE epoch = ?` for
    the CAS. Check affected rows. [B2: this is the ONLY CAS in the protocol.]
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05`, `/// @requirement:REQ-RP-004`

- `src/persistence/recovery_operations.rs`
  - Implement `compute_operation_id` [B3], `compute_logical_request_key` [B3],
    `lookup_operation`, `find_adoptable_pending` [B3], `try_adopt_pending` [B3],
    `insert_pending` (with logical_request_key, owner_pid, lease_expires_at,
    execution_attempt_id) [B3/B4], `finalize_completed`, `finalize_refused`,
    `finalize_conflict` per operations pseudocode 22–149.
  - Guarded transitions: `WHERE status = 'pending'` + affected-row check.
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05`, `/// @requirement:REQ-RP-004`

- `src/persistence/attempts.rs`
  - Implement `record_attempt_start` [B4], `append_attempt_outcome` [B4],
    `latest_for_step`, `load_attempt`, `load_unfinalized_for_operation` [B4],
    `verify_snapshot_digest` per attempts pseudocode 23–93.
  - Use `INSERT ... RETURNING attempt_id` (RETURNING already used in
    `src/persistence/leases.rs`).
  - `append_attempt_outcome`: guarded UPDATE `WHERE finalized_at IS NULL`. [B4]
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05`, `/// @requirement:REQ-RP-003`

- `src/persistence/effect_intents.rs`
  - Implement `compute_effect_key`, `prepare_effect` [B5: insert-or-load],
    `load_effect`, `reconcile_effect`, `finalize_effect` per intents pseudocode
    34–151.
  - `prepare_effect`: if key exists, compare exact binding; mismatch → conflict. [B5]
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05`, `/// @requirement:REQ-RP-008`

## Verification Commands

```bash
set -euo pipefail
cargo test --test epoch_operations_attempts_integration_tests || exit 1
cargo test --test effect_intents_integration_tests || exit 1
git diff workflow/tests/ | grep -E "^[+-]" | grep -v "^[+-]{3}" && { echo "FAIL: tests modified"; exit 1; } || true
grep -rn "println!\|dbg!\|todo!\|unimplemented!" workflow/src/persistence/recovery_epoch.rs workflow/src/persistence/recovery_operations.rs workflow/src/persistence/attempts.rs workflow/src/persistence/effect_intents.rs && { echo "FAIL"; exit 1; } || true
grep -rn -E "(placeholder|not yet|will be)" workflow/src/persistence/recovery_epoch.rs workflow/src/persistence/recovery_operations.rs workflow/src/persistence/attempts.rs workflow/src/persistence/effect_intents.rs && { echo "FAIL"; exit 1; } || true
cargo test || exit 1
cargo clippy --workspace --all-targets --all-features -- -D warnings || exit 1
```

## Success Criteria

- All P04 tests pass (durable store).
- No test modifications.
- No debug/placeholder code.
- Full suite passes.
- Milestone 1 gate: epoch CAS durable; operation ledger durable; append-only with
  complete StateSnapshot durable; effect intents durable.

## Failure Recovery

`git checkout` the modified files; re-run P05. Two-cycle cap.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P05.md`

## Milestone 1 Gate

After P05A passes, check the M1 box in `../execution-tracker.md`:
- [ ] Epoch is a distinct durable row (not MAX(generation)); CAS with
      affected-row check. [C1/B2: the ONLY CAS in the protocol]
- [ ] Operation ledger with stable operation_id + logical_request_key [B3],
      guarded owner/lease claim for Pending [B3], execution_attempt_id binding
      [B4], Pending/Completed/Refused/Conflict, guarded transitions. [C2]
- [ ] Attempts: record_attempt_start [B4] + append_attempt_outcome [B4]
      (complete StateSnapshot, capsule binding, snapshot digest, runner_result,
      operation binding; guarded outcome-append).
- [ ] Effect intents with stable key, binding, canonical payload/digest,
      insert-or-load exact-binding comparison [B5], guarded finalize. [C7]
- [ ] No in-memory persistence facade was introduced at any point.
