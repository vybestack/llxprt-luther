# Phase 16A: Deterministic Failpoint Matrix Verification

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P16A`

## Prerequisites

- Required: P16 completed.

## Verification Commands

```bash
cargo test --test recovery_failpoint_matrix_tests || exit 1
cargo test || exit 1
cargo clippy -- -D warnings || exit 1
```

## Semantic Verification Checklist

For each failpoint F1–F14, confirm:

- [ ] F1: launch without capsule refuses.
- [ ] F2: interrupt before first step → `Recovered`, capsule immutable, epoch CAS holds. [C1]
- [ ] F3: interrupt after worktree delta → `ContinueWorkspace` after exact verify; WorkspaceAuth revalidated in reserve. [C4]
- [ ] F4: commit intent prepared-but-not-finalized → reconcile, no duplicate commit. [C7]
- [ ] F5: push intent prepared-but-not-finalized → reconcile remote, no duplicate push. [C7]
- [ ] F6: stale epoch → `StaleEpoch { persisted, expected }`, no mutation. [C1]
- [ ] F7: duplicate recovery (same binding) → `AlreadyApplied { prior_outcome }`, no new attempt row. [C2]
- [ ] F8: tampered envelope digest → resume refuses. [C8]
- [ ] F9: missing ownership (TOCTOU between prepare and reserve) → `Refused(NotAuthorized)`. [C4]
- [ ] F10: changed base ref → `ContinueWorkspace` refused.
- [ ] F11: legacy run (including migrated-provenance sentinel) → salvage, no exact continuation. [C9]
- [ ] F12: concurrent recovery → exactly one proceeds via CAS affected-row check. [C1]
- [ ] F13: conflicting duplicate (different binding) → `Refused(ConflictingOperation)`. [C2]
- [ ] F14: crash between execute and finalize → re-recovery reconciles; no pre-finalize Recovered. [C12]

#### Invariant Verification
- [ ] No failpoint permits a duplicate side effect.
- [ ] No failpoint permits recovery without exact verification (where required).
- [ ] No failpoint weakens ownership/lease/loop-limit safety.
- [ ] No failpoint permits `Recovered` before finalize commits. [C12]
- [ ] No failpoint uses synthetic attempts to bump epoch. [C1]

## Holistic Functionality Assessment (at completion)

- What was verified: [14 failpoints, each with outcome + invariant]
- Does it satisfy REQ-RP-001/004/006/007/008? [per requirement]
- Verdict: [PASS/FAIL]

## Failure Recovery

Two-cycle cap per failpoint. A failpoint that cannot be made green after two
cycles is recorded as a known gap and escalated (do not weaken the test).

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P16A.md`
