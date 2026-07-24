# Phase 18A: Canary Harness Verification

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P18A`

## Prerequisites

- Required: P18 completed.

## Verification Commands

```bash
set -euo pipefail
cargo test --test canary_harness_tests || exit 1
cargo test || exit 1
cargo clippy --workspace --all-targets --all-features -- -D warnings || exit 1
```

## Semantic Verification Checklist

For each of the three canaries, confirm all 9 gate stages were traversed:

- [x] Canary 1 (mixed type A): 9/9 stages, 0 violations.
- [x] Canary 2 (mixed type B): 9/9 stages, 0 violations.
- [x] Canary 3 (mixed type C): 9/9 stages, 0 violations.

### Invariant Verification (all three)

- [x] No direct SQL outside persistence layer.
- [x] No historical binary/config dependency (envelope digest match [C8]).
- [x] No manual git/GitHub mutation.
- [x] No duplicate effects (effect-intent state machine reconciles [C7]).
- [x] No ownership/lease/loop-limit/epoch-CAS violations.

### Consecutiveness

- [x] The three canaries ran consecutively (not parallel), each starting after
      the prior completed clean.

## Holistic Functionality Assessment

- What was verified: 3 consecutive mixed canaries, full gate, 0 violations
- Does it satisfy REQ-QUAL-001? PASS
- Verdict: PASS

## Failure Recovery

Two-cycle cap per canary. If a canary cannot pass after two cycles, record the
specific stage that fails and the invariant violated; do not weaken the canary.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P18A.md`
