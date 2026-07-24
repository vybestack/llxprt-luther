# Phase 04: Epoch CAS + Operations Ledger + Append-Only Integration-First TDD (Milestone 1)

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P04`

## Prerequisites

- Required: P03A completed with PASS.

## Purpose

Write behavioral integration tests for the durable epoch CAS, the operation
ledger idempotency, the append-only attempt store, and the effect-intent state
machine. These tests exercise the real durable store (SQLite) directly.

Because the durable store lands in Milestone 1 (P03–P05) ahead of the capsule
and protocol, these tests assert durable invariants against the store itself —
no in-memory facade, no protocol dependency. The protocol (P09–P11) will later
consume this same durable store.

## Requirements Implemented (Expanded)

### REQ-RP-003: Append-only immutable attempt IDs with complete state [C3/B4]
**Behavior**:
- GIVEN: an empty attempt store
- WHEN: `record_attempt_start(R, epoch=1, source=None, op_id, "s1", capsule_v1, envelope_digest, snapshot1)` then `append_attempt_outcome(attempt_id, "completed", snapshot1, runner_result, None)`
- THEN: a started row exists with `finalized_at = NULL` before outcome is appended [B4]; after append, `finalized_at` is set
- WHEN: `record_attempt_start(R, epoch=1, source=Some(1), op_id2, "s2", ...)` then `append_attempt_outcome(...)`
- THEN: two rows with strictly increasing attempt_ids; `latest_for_step(R,"s1")` returns the first with complete `StateSnapshot`; `latest_for_step(R,"s2")` returns the second
- WHEN: `verify_snapshot_digest` is called on a loaded row
- THEN: it succeeds (digest matches the canonical serialization)
- WHEN: `load_unfinalized_for_operation(conn, op_id)` is called for an operation whose attempt-start was recorded but outcome was NOT appended [B4]
- THEN: it returns the row with `finalized_at = NULL` (crash recovery)
- WHEN: `append_attempt_outcome` is called on a row that already has `finalized_at` set [B4]
- THEN: returns `OutcomeAlreadyAppended` (guarded)

### REQ-RP-004: Epoch CAS + operation ledger idempotency (durable) [C1/C2/B3]
**Behavior**:
- GIVEN: `read_epoch(R) == 0`
- WHEN: `cas_advance_epoch(tx, R, 0)` is called
- THEN: epoch advances to 1; `CasOutcome::Advanced { from: 0, to: 1 }`
- WHEN: `cas_advance_epoch(tx, R, 0)` is called AGAIN (stale expected)
- THEN: `CasOutcome::Stale { persisted: 1, expected: 0 }`
- WHEN: `insert_pending(tx, op_id, R, 0, "s1", envelope_digest, None, logical_key, intent_digest, pid, lease, attempt_id)` then `lookup_logical_operation(tx, logical_key)`
- THEN: returns the pending operation with `owner_pid` and `lease_expires_at` [B3]
- WHEN: `finalize_completed(tx, op_id, outcome_json)` is called
- THEN: status transitions to Completed with serialized_outcome
- WHEN: `finalize_completed` is called on an already-Completed operation
- THEN: returns `GuardFailed` (guarded transition)
- WHEN: `find_adoptable_pending(tx, logical_key, now)` is called for a Pending op with expired lease [B3]
- THEN: returns the adoptable operation
- WHEN: `try_adopt_pending(tx, op_id, new_pid, new_lease, now)` succeeds [B3]
- THEN: returns `AdoptOutcome::Adopted`; owner_pid is updated

### REQ-RP-008: Effect-intent state machine [C7/B5]
**Behavior**:
- GIVEN: a commit about to be issued
- WHEN: `prepare_effect(conn, op_id, attempt_id, 0, Commit, payload, expected_target, expected_predecessor)` is called
- THEN: a row exists with `status='prepared'`, `payload_digest=sha256(canonical_payload)`, stable `effect_key`
- WHEN: `prepare_effect` is called AGAIN with the SAME exact binding (same payload_digest, expected_target, expected_predecessor) [B5]
- THEN: returns the EXISTING intent (insert-or-load, idempotent)
- WHEN: `prepare_effect` is called with the same `effect_key` but a DIFFERENT exact binding [B5]
- THEN: the intent transitions to `conflict` and returns `BindingConflict`
- WHEN: `reconcile_effect(conn, key, observed_match)` is called
- THEN: returns `Completed` and `finalize_effect` sets `status='completed'`
- WHEN: `reconcile_effect` on a `prepared` intent where observed differs from expected AND from predecessor
- THEN: returns `Conflict`
- WHEN: `finalize_effect` is called on an already-completed effect
- THEN: returns `GuardFailed`

## Implementation Tasks

### Files to Create

- `tests/epoch_operations_attempts_integration_tests.rs`
  - MUST include: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P04`
  - Test cases:
    1. epoch CAS advances from 0 to 1 (REQ-RP-004) [C1]
    2. epoch CAS stale returns Stale with persisted value (REQ-RP-004) [C1]
    3. epoch CAS detects concurrent advance (affected-row check) (REQ-RP-004) [C1]
    4. insert_pending then lookup_operation returns pending with owner_pid/lease (REQ-RP-004) [C2/B3]
    5. finalize_completed transitions to Completed with outcome (REQ-RP-004) [C2]
    6. finalize on already-finalized returns GuardFailed (REQ-RP-004) [C2]
    7. record_attempt_start creates started row with finalized_at=NULL; append_attempt_outcome sets finalized_at (REQ-RP-003) [C3/B4]
    8. attempt_ids strictly monotonic (REQ-RP-003) [C3]
    9. no existing row is mutated except guarded outcome-append (append-only) (REQ-RP-003) [C3/B4]
    10. verify_snapshot_digest succeeds on loaded row (REQ-RP-003) [C3]
    11. load_unfinalized_for_operation returns crash-recovery row (REQ-RP-003) [B4]
    12. append_attempt_outcome on already-finalized returns OutcomeAlreadyAppended (REQ-RP-003) [B4]
    13. prepare_effect stores digest + Prepared status before effect (REQ-RP-008) [C7]
    14. prepare_effect same exact binding returns existing intent (insert-or-load) (REQ-RP-008) [B5]
    15. prepare_effect different binding with same key → BindingConflict (REQ-RP-008) [B5]
    16. reconcile_effect Completed when observed matches expected_target (REQ-RP-008) [C7]
    17. reconcile_effect Conflict when observed is unexpected (REQ-RP-008) [C7]
    18. finalize_effect guard: double finalize returns GuardFailed (REQ-RP-008) [C7]
    19. compute_effect_key is deterministic for same binding (REQ-RP-008) [C7]
    20. find_adoptable_pending finds expired-lease pending op (REQ-RP-004) [B3]
    21. try_adopt_pending adopts expired-lease op (REQ-RP-004) [B3]

- `tests/effect_intents_integration_tests.rs` (or combined file)

## Required Code Markers

```rust
/// @plan PLAN-20260723-SELFHOST-RELIABILITY.P04
/// @requirement REQ-RP-004
#[test]
fn epoch_cas_stale_returns_stale_with_persisted_value() { /* ... */ }
```

## Verification Commands

```bash
grep -r "@plan:PLAN-20260723-SELFHOST-RELIABILITY.P04" workflow/tests/ | wc -l
grep -r "should_panic" workflow/tests/epoch_operations_attempts_integration_tests.rs && echo "FAIL"
cargo test --test epoch_operations_attempts_integration_tests 2>&1 | head -30
# Expected: red phase
```

## Success Criteria

- 21+ tests, tagged, red phase.
- Tests assert durable row counts / monotonicity / no-mutation / CAS-stale /
  guard-failed / insert-or-load exact-binding / lease-adoption / crash-recovery
  invariants against real SQLite. [C3/B4/B5]

## Failure Recovery

Two-cycle cap.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P04.md`
