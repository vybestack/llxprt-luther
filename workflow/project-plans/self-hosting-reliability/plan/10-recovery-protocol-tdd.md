# Phase 10: RecoveryProtocolV1 Integration-First TDD (Milestone 3)

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P10`

## Prerequisites

- Required: P09A completed with PASS.

## Purpose

Write BEHAVIORAL integration tests that drive
`RecoveryProtocolV1::recover()`. These tests define the contract the
implementation (P11) must satisfy. They MUST fail naturally (assertion failures
or `todo!()` panics) — never test for panic behavior, never use `#[should_panic]`.

Integration-first: these tests exercise the protocol through its public API and
its interaction with the durable persistence layer (SQLite), not isolated unit
internals. The protocol's epoch CAS, operation ledger, and idempotency consume
the durable stores landed in P05 — no in-memory facade.

## Requirements Implemented (Expanded)

### REQ-RP-004: Epoch-fenced idempotent recovery with operation ledger [C1/C2]
**Behavior**:
- GIVEN: a run with persisted epoch E and a Completed operation for (run, step, capsule, source_attempt)
- WHEN: `recover(request{step})` is called again with the same binding
- THEN: returns `RecoveryOutcome::AlreadyApplied { operation_id, prior_outcome }`
       with no new durable mutation [C2]
- GIVEN: a run with a Pending operation for (run, step, capsule, source_attempt)
- WHEN: `recover(request{step})` is called again
- THEN: the pending operation is reconciled (not duplicated) [C2]
- GIVEN: a run with a Completed operation for (run, step, capsule, source_attempt)
- WHEN: `recover(request{step})` is called with a DIFFERENT capsule/source binding
- THEN: returns `RecoveryOutcome::Refused { reason: ConflictingOperation }` [C2]
- GIVEN: a run with persisted epoch E
- WHEN: `recover(request)` is called after epoch has advanced (stale)
- THEN: returns `RecoveryOutcome::StaleEpoch { persisted, expected }` [C1]

### REQ-RP-005: Step recovery policy from canonical StepDef [C6]
**Behavior**:
- GIVEN: a generic `shell` step_id WITHOUT an explicit `recovery_policy`
       declaration
- WHEN: `policy_for_step(step_def, "shell")` is called
- THEN: returns `NonRecoverable` (generic shell/write_file default to
       NonRecoverable) [C6]
- GIVEN: a canonical step with `recovery_policy = ContinueWorkspace` declared
- WHEN: `policy_for_step(step_def, that_step_id)` is called
- THEN: returns `ContinueWorkspace` [C6]
- GIVEN: a step_id in `SAFE_RERUN_STEPS` (e.g. "watch_pr_checks")
- WHEN: `policy_for_step(step_def, "watch_pr_checks")` is called
- THEN: returns `Idempotent` [C6]
- GIVEN: an unknown step_id
- WHEN: `policy_for_step(step_def, "mystery")`
- THEN: returns `NonRecoverable`

### REQ-RP-006: ContinueWorkspace exact verification with sealed authority [C4]
**Behavior**:
- GIVEN: an interrupted run with a matching worktree, ownership marker, base ref, diagnostic
- WHEN: `recover(request{step: canonical_continue_step})` is called
- THEN: returns `RecoveryOutcome::Recovered { resumed_at_step, attempt_id }`
- GIVEN: an interrupted run whose worktree path differs from the capsule's
- WHEN: `recover(...)` is called
- THEN: returns `RecoveryOutcome::Refused { reason: VerificationFailed }`
- GIVEN: a request where the descriptor-bound `WorkspaceAuthorization` does NOT
       match the actual worktree ownership (TOCTOU)
- WHEN: `recover(...)` is called
- THEN: returns `RecoveryOutcome::Refused { reason: NotAuthorized }` (the
       `RecoveryAuthority` is NOT constructed; no internal trust bypass) [C4]

## Implementation Tasks

### Files to Create

- `tests/recovery_protocol_integration_tests.rs`
  - Integration tests against an in-memory SQLite connection + the protocol API.
  - Each test has a doc comment: `/// @requirement:REQ-RP-00X`
  - MUST include: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10`
  - Test cases (behavioral, real data flows):
    1. Fresh recovery with valid expected_epoch → `Recovered` (REQ-RP-001) [C5/B2]
    2. Re-issue same (run, step, capsule, source) → `AlreadyApplied` with prior_outcome, no new attempt row (REQ-RP-004) [C2]
    3. Pending duplicate → reconciled via guarded owner/lease claim, not duplicated (REQ-RP-004) [C2/B3]
    4. Conflicting duplicate (different capsule/source binding) → `Conflict` (REQ-RP-004) [C2/B3]
    5. Stale epoch → `StaleEpoch`, no durable mutation (REQ-RP-004) [C1/B2]
    6. Generic `shell` step_id without declaration → policy `NonRecoverable` → `Refused` (REQ-RP-005) [C6]
    7. Canonical step with `ContinueWorkspace` declared → policy `ContinueWorkspace` (REQ-RP-005) [C6/B7]
    8. `SAFE_RERUN_STEPS` step_id → policy `Idempotent` (REQ-RP-005) [C6]
    9. ContinueWorkspace with matching worktree/ownership/base/diagnostic → `Recovered` (REQ-RP-006)
    10. ContinueWorkspace with mismatched worktree → `Refused(VerificationFailed)` (REQ-RP-006)
    11. ContinueWorkspace with mismatched ownership (TOCTOU) → `Refused(NotAuthorized)` via `adjudicate_workspace_ownership` revalidation (REQ-RP-006) [C4/B6]
    12. Recovery loads immutable capsule and verifies envelope digest (REQ-RP-002 interplay) [C8]
    13. Epoch CAS advances at reserve; re-recovery returns AlreadyApplied (REQ-RP-004) [C1/C12/B2]
    14. Protocol does NOT return Recovered before finalize commits (REQ-RP-001) [C5/C12]
    15. Authority changed between prepare and reserve (run_status or checkpoint changed) → `AuthorityChanged` error, no mutation (REQ-RP-004) [B1]
    16. Durable attempt-start recorded at reserve before execute; crash recovery loads unfinalized attempt (REQ-RP-003) [B4]
    17. Expired-lease Pending operation is adoptable by a second recoverer (REQ-RP-004) [B3]
    18. No finalize CAS: finalize does NOT advance epoch (single CAS at reserve only) (REQ-RP-004) [B2]

### Files to Modify

- `tests/` module wiring as needed (mirror existing `tests/*.rs` patterns).

## Required Code Markers

```rust
/// @plan PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement REQ-RP-004
#[test]
fn conflicting_duplicate_refuses() { /* ... */ }
```

## Verification Commands

```bash
grep -r "@plan:PLAN-20260723-SELFHOST-RELIABILITY.P10" workflow/tests/recovery_protocol_integration_tests.rs | wc -l
# Expected: 14+ occurrences

grep -r "should_panic" workflow/tests/recovery_protocol_integration_tests.rs && echo "FAIL"

cargo test --test recovery_protocol_integration_tests 2>&1 | head -30
# Expected: failures, not "test result: ok"
```

## Success Criteria

- 18+ behavioral integration tests created.
- Tests tagged with P10 and requirement IDs.
- Tests fail with assertion failures or `todo!()` panics (red phase).
- No `#[should_panic]`, no reverse testing.

## Failure Recovery

If tests compile-error (rather than assertion-fail): fix the stub to make the
types constructible, then re-run. Do NOT weaken the test assertions.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P10.md`
