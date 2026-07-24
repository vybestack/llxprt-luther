# Phase 04A: Epoch + Operations + Append-Only TDD Verification (Red Phase)

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P04A`

## Prerequisites

- Required: P04 completed.

## Verification Commands

```bash
set -o pipefail
cargo test --test epoch_operations_attempts_integration_tests 2>&1 | tail -20
cargo test --test effect_intents_integration_tests 2>&1 | tail -20
grep -rn "should_panic" workflow/tests/epoch_operations_attempts_integration_tests.rs workflow/tests/effect_intents_integration_tests.rs && exit 1 || true
```

## Structural Verification Checklist

- [x] 15+ tests tagged `@plan:...P04`.
- [x] No `#[should_panic]`.
- [x] Tests use real SQLite connections.

## Semantic Verification Checklist

1. Does the epoch CAS test assert `Stale` is returned with the **persisted**
   value (not just a failure)? [yes/no] [C1]
2. Does the epoch CAS test assert the affected-row check detects concurrent
   advance? [yes/no] [C1]
3. Does the operation ledger test assert `GuardFailed` on double-finalize?
   [yes/no] [C2]
4. Does the append-only test assert the ORIGINAL row is unchanged after a new
   append (including its complete `StateSnapshot`)? [yes/no] [C3]
5. Does the snapshot-digest test assert the digest equals an independently
   computed SHA-256 of the canonical `StateSnapshot` serialization? [yes/no] [C3]
6. Does the effect-intent test assert the digest equals an independently computed
   SHA-256 of the canonical payload? [yes/no] [C7]
7. Would the reconcile test FAIL if `reconcile_effect` always returned
   `Completed`? [yes/no] [C7]

## Failure Recovery

Two-cycle cap on semantic review.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P04A.md`
