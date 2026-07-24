# Phase 18: Canary Harness (Three Consecutive Mixed Canaries)

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P18`

## Prerequisites

- Required: P17A completed with PASS.

## Purpose

Build and run the canary harness that drives a run through the full viability
gate (9 stages) and proves self-hosting viability via three consecutive mixed
canaries with zero invariant violations.

## Requirements Implemented (Expanded)

### REQ-QUAL-001: Three consecutive mixed canaries
**Full Text**: Qualification SHALL require three consecutive mixed canaries
completing the full viability gate with zero invariant violations.
**Behavior**:
- GIVEN: the canary harness configured with mixed workflow types/configs
- WHEN: three consecutive canaries run
- THEN: each traverses all 9 viability gate stages and completes with zero
       invariant violations
**Why This Matters**: A single green run is not viability; three consecutive
mixed runs prove the recovery model holds across variation.

## Viability Gate Stages (the canary must traverse all 9)

1. Fresh launch — capsule persisted (envelope digest [C8]), lease claimed, ownership provisioned.
2. Deterministic interruption after worktree delta.
3. Supported recover — `RecoveryProtocolV1` dispatched (phased model [C5]).
4. Exact working-tree verification — `ContinueWorkspace` verifies; WorkspaceAuth revalidated in reserve [C4].
5. Allowlist staging — `git add` only allowlisted paths.
6. Commit/push — via effect-intent state machine (guarded finalize [C7]).
7. PR binding — run bound to a PR with verified identity.
8. Stable final-head CI/review — CI and review pass on final head.
9. Typed merge and strategy-specific proof — typed artifact + strategy-specific
   reachability proof [C10] + atomic artifact+status tx [C11] + durable `Merged`.

## Implementation Tasks

### Files to Create

- `tests/canary_harness_tests.rs` (or `xtask`-driven harness)
  - A harness that:
    - launches a mixed canary (varying workflow type/config across the three),
    - injects a deterministic interruption after a worktree delta (failpoint F3),
    - recovers via `RecoveryProtocolV1`,
    - verifies exact working tree,
    - stages allowlisted paths,
    - commits/pushes via effect intents,
    - binds a PR,
    - asserts stable final-head CI/review (mocked/stubbed remote),
    - completes typed merge,
    - checks zero invariant violations.
  - Runs three consecutive mixed canaries.
  - MUST include: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P18`
  - MUST include: `/// @requirement:REQ-QUAL-001,REQ-QUAL-002`

### Invariant Checks (per canary)

- No direct SQL outside the persistence layer.
- No historical binary/config dependency (capsule digest matches).
- No manual git/GitHub mutation (all via intents/adapters).
- No duplicate effects (effect intents reconcile).
- No invariant violations (ownership, lease, loop-limit, epoch CAS).

## Verification Commands

```bash
cargo test --test canary_harness_tests || exit 1
grep -r "@plan:PLAN-20260723-SELFHOST-RELIABILITY.P18" workflow/tests/canary_harness_tests.rs | wc -l
grep -r "@requirement:REQ-QUAL-001" workflow/tests/canary_harness_tests.rs
```

## Success Criteria

- Three consecutive mixed canaries pass the full viability gate.
- Zero invariant violations across all three.
- No prohibited escape (direct SQL, historical dependency, manual mutation,
  duplicate effects).

## Failure Recovery

If a canary fails: diagnose via the failpoint matrix (P16); fix the
implementation, not the canary. Two-cycle cap per canary. After two cycles,
record the residual gap rather than weakening the canary.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P18.md`
