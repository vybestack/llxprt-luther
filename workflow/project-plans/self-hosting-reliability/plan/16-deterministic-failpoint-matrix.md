# Phase 16: Deterministic Failpoint Matrix

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P16`

## Prerequisites

- Required: P15A completed with PASS.

## Purpose

Establish a deterministic failpoint matrix that exercises every recovery-
critical interruption point and proves the system recovers correctly (or fails
closed) at each. This is the test bed for the viability gate stages
"deterministic interruption after worktree delta" and "supported recover".

The matrix is updated to reflect the epoch CAS + operation ledger concurrency
model [C13]: epoch is a distinct durable row (not MAX(generation)), CAS has an
affected-row check, and the operation ledger handles duplicate/recovery via
Pending/Completed/Refused/Conflict reconciliation.

## The Failpoint Matrix

Each failpoint is an interruption injected at a deterministic point. For each,
the matrix asserts the recovery outcome and the durable-state invariant.

| # | Failpoint | Injection Point | Expected Recovery | Invariant |
|---|-----------|-----------------|-------------------|-----------|
| F1 | Interrupt before capsule persist | after resolve, before persist | Launch refuses (no capsule) | No run row without capsule for new runs |
| F2 | Interrupt after capsule persist, before first step | after persist, before run() | `Recovered` at first step | Capsule immutable; epoch CAS holds [C1] |
| F3 | Interrupt after worktree delta, before commit | mid shell/write_file step | `ContinueWorkspace` after exact verify | Worktree/ownership/base/diagnostic verified; WorkspaceAuth revalidated in reserve tx [C4] |
| F4 | Interrupt during commit (effect intent prepared, not finalized) | after prepare_effect, before finalize | reconcile → `Completed` or `NeedsReissue` | No duplicate commit; guarded finalize [C7] |
| F5 | Interrupt during push (effect intent prepared, not finalized) | after prepare, before finalize | reconcile remote → reissue if needed | No duplicate push; guarded finalize [C7] |
| F6 | Stale epoch recovery | epoch advanced, old epoch requested | `StaleEpoch { persisted, expected }` | No durable mutation [C1] |
| F7 | Duplicate recovery (same operation key) | recover twice with same binding | `AlreadyApplied { prior_outcome }` (second call) | No new attempt row; operation ledger returns prior outcome [C2] |
| F8 | Tampered capsule envelope digest | envelope digest changed on disk | Resume refuses | No step executes [C8] |
| F9 | Ownership marker missing on resume (TOCTOU) | durable marker deleted between prepare and reserve | `Refused(NotAuthorized)` | No workspace mutation; WorkspaceAuth revalidated in CAS tx [C4] |
| F10 | Base ref changed on resume | base ref differs from capsule | `ContinueWorkspace` refused | No step executes |
| F11 | Legacy (no V1 capsule) recovery | pre-V1 run, including migrated-provenance sentinel | Salvage lineage, no exact continuation | Audit-only [C9] |
| F12 | Concurrent same-run recovery (epoch CAS race) | two recoverers | Exactly one proceeds via CAS affected-row check; other `StaleEpoch` | Single-writer fence; no synthetic attempts [C1] |
| F13 | Conflicting duplicate (different capsule/source binding) | recover with different binding | `Refused(ConflictingOperation)` | Operation ledger detects conflict [C2] |
| F14 | Protocol crash between execute and finalize | runner finished, finalize tx not committed | Re-recovery reconciles; effect intents reconcile | Protocol cannot return Recovered before finalize [C12] |

## Requirements Implemented

### REQ-RP-001 / REQ-RP-004 / REQ-RP-006 / REQ-RP-007 / REQ-RP-008 (matrix coverage)
The matrix proves each requirement holds at every failpoint, including the
epoch CAS, operation ledger, and effect-intent state machine.

## Implementation Tasks

### Files to Create

- `tests/recovery_failpoint_matrix_tests.rs`
  - One test per failpoint F1–F14.
  - Each test injects the interruption deterministically (e.g., set the
    interrupted flag, write a partial state, delete a marker, advance the epoch
    manually) and then calls `RecoveryProtocolV1::recover()` (or the
    launch/resume surface), asserting the expected outcome + durable invariant.
  - MUST include: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P16`
  - MUST include: `/// @requirement:REQ-RP-00X` per test

## Verification Commands

```bash
cargo test --test recovery_failpoint_matrix_tests || exit 1
grep -r "@plan:PLAN-20260723-SELFHOST-RELIABILITY.P16" workflow/tests/recovery_failpoint_matrix_tests.rs | wc -l
# Expected: 14+ occurrences
```

## Success Criteria

- All 14 failpoint tests pass.
- Each test asserts BOTH the recovery outcome AND a durable-state invariant.
- No failpoint allows an unsafe recovery (double effect, stale proceed, missing
  ownership proceed, conflicting duplicate proceed, pre-finalize Recovered).

## Failure Recovery

If a failpoint reveals a real bug: fix the implementation (not the test) in the
relevant earlier phase's module, re-run. Two-cycle cap per failpoint.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P16.md`
