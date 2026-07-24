# Phase 10A: Recovery Protocol TDD Verification (Red Phase)

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P10A`

## Prerequisites

- Required: P10 completed.

## Purpose

Verify the TDD tests are in a proper red phase: they fail because behavior is
absent, not because they are malformed.

## Verification Commands

```bash
set -euo pipefail
cargo test --test recovery_protocol_integration_tests 2>&1 | tail -20
grep -rn "should_panic" workflow/tests/recovery_protocol_integration_tests.rs && { echo "FAIL"; exit 1; } || true
```

## Structural Verification Checklist

- [ ] 14+ tests exist, tagged `@plan:...P10`.
- [ ] No `#[should_panic]`.
- [ ] Tests construct real `RecoveryRequest` values (without `trusted_internal`)
      and assert `RecoveryOutcome` variants. [C4]
- [ ] Tests use an in-memory SQLite connection (real persistence, not mocked).

## Semantic Verification Checklist

1. Does each test transform a real input → asserted `RecoveryOutcome`? [yes/no]
2. Would each test FAIL if `recover()` returned a wrong variant? [yes/no]
3. Does the idempotency test assert NO new attempt row was appended on re-issue
   AND that the prior outcome is returned? [yes/no] [C2]
4. Does the conflicting-duplicate test assert `Refused(ConflictingOperation)`
   when the capsule/source binding differs? [yes/no] [C2]
5. Does the stale-epoch test assert NO durable mutation occurred? [yes/no] [C1]
6. Does the ContinueWorkspace refusal test assert the specific `RefusalReason`? [yes/no]
7. Does the TOCTOU test confirm `RecoveryAuthority` is NOT constructed when
   ownership mismatches? [yes/no] [C4]
8. Does the policy test confirm generic shell/write_file default to
   `NonRecoverable` without an explicit declaration? [yes/no] [C6]
9. Does the policy test confirm `SAFE_RERUN_STEPS` members map to `Idempotent`?
   [yes/no] [C6]

## Failure Recovery

If tests pass (should be red): the assertions are too weak; strengthen them.
If tests compile-error: the stub is missing a constructor; fix the stub (P09)
without weakening tests.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P10A.md`
