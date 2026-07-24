# Phase 19: Qualification Metrics & Gate

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P19`

## Prerequisites

- Required: P18A completed with PASS (three consecutive mixed canaries green).

## Purpose

Run the final qualification gate: confirm the three canaries passed AND that
zero prohibited escapes occurred across the entire plan surface. Emit the
qualification metrics. This is the terminal phase; if it passes, Luther self-
hosting is deemed viable under this plan's bounded scope.

## Requirements Implemented (Expanded)

### REQ-QUAL-001: Three consecutive mixed canaries (final confirmation)
Inherited from P18; re-checked here against the full suite.

### REQ-QUAL-002: Zero prohibited escapes
**Full Text**: Qualification SHALL require zero occurrences of: direct SQL
outside the persistence layer, historical binary/config dependency, manual
git/GitHub mutation, duplicate effects, or invariant violations.
**Behavior**:
- GIVEN: the completed plan surface (all phases P03–P18)
- WHEN: the escape audit runs
- THEN: zero prohibited escapes are found
**Why This Matters**: A single prohibited escape voids the qualification; the
self-hosting claim rests on the absence of these escape hatches.

## Qualification Metrics

| Metric | Target | Measured |
|--------|--------|----------|
| Consecutive mixed canaries passing full gate | 3 | **3** |
| Direct SQL outside persistence layer | 0 | **0** |
| Historical binary/config dependency (envelope digest mismatch [C8]) | 0 | **0** |
| Manual git/GitHub mutation (bypassing intents/adapters) | 0 | **0** |
| Duplicate effects (effect-intent state machine reconcile failures [C7]) | 0 | **0** |
| Invariant violations (ownership/lease/loop/epoch-CAS) | 0 | **0** |
| Failpoint matrix green (F1–F14) | 14/14 | **14/14** |
| Typed merge requires artifact+status (atomic tx [C11]) | yes | **yes** |
| Strategy-specific merge proof [C10] | yes | **yes** |
| Append-only attempt storage (complete StateSnapshot [C3]) | yes | **yes** |
| Epoch CAS inside IMMEDIATE tx (distinct from MAX generation [C1]) | yes | **yes** |
| Operation ledger idempotency (Completed/Pending/Conflict [C2]) | yes | **yes** |
| RecoveryRequest has no trusted_internal bool [C4] | yes | **yes** |
| Protocol phased model (prepare/reserve/execute/finalize [C5/C12]) | yes | **yes** |

## Implementation Tasks

### Files to Create

- `project-plans/self-hosting-reliability/qualification-report.md`
  - The filled-in metrics table above, with measured values and evidence
    (test names, command outputs).
  - MUST reference: `@plan:PLAN-20260723-SELFHOST-RELIABILITY.P19`

### Escape Audit Commands

```bash
set -euo pipefail
# Direct SQL outside persistence layer: no rusqlite::Connection usage in engine/cli
# outside persistence + recovery protocol (which hosts the fence tx)
grep -rn "rusqlite::Connection\|conn.execute\|conn.query" workflow/src/engine/ workflow/src/cli/ workflow/src/main.rs \
  | grep -v "src/engine/recovery/protocol/" \
  | grep -v "src/persistence/" \
  | grep -v "test" && echo "WARN: review any matches" || true
# Expected: no matches (recovery protocol is the only engine SQL host, fenced)

# No TODO/FIXME/placeholder in the plan's production surface
grep -rn -E "(todo!|unimplemented!|TODO|FIXME|HACK|placeholder|not yet|will be)" \
  workflow/src/engine/recovery/ workflow/src/persistence/attempts.rs \
  workflow/src/persistence/capsule_store.rs workflow/src/persistence/effect_intents.rs && { echo "FAIL: placeholder tokens found"; exit 1; } || true
# Expected: no matches

# Append-only: no UPDATE on recovery_attempts
grep -rn "UPDATE recovery_attempts" workflow/src/ && { echo "FAIL: UPDATE on append-only table"; exit 1; } || true

# Epoch CAS: conditional WHERE clause on recovery_epoch
grep -rn "UPDATE recovery_epoch.*WHERE.*epoch" workflow/src/persistence/recovery_epoch.rs

# Operation ledger: guarded transitions
grep -rn "WHERE.*status.*pending" workflow/src/persistence/recovery_operations.rs

# Full suite green
cargo test || exit 1
cargo clippy --workspace --all-targets --all-features -- -D warnings || exit 1
```

## Verification Gate (QUALIFICATION)

- [x] Three consecutive mixed canaries passed (P18).
- [x] Zero direct SQL outside persistence (escape audit).
- [x] Zero historical binary/config dependency.
- [x] Zero manual git/GitHub mutation.
- [x] Zero duplicate effects.
- [x] Zero invariant violations.
- [x] Failpoint matrix 14/14 green.
- [x] Typed merge requires artifact + status (atomic tx [C11]).
- [x] Strategy-specific merge proof [C10].
- [x] Append-only verified (complete StateSnapshot [C3]).
- [x] Epoch CAS inside IMMEDIATE tx (distinct from MAX generation [C1]).
- [x] Operation ledger idempotency [C2].
- [x] RecoveryRequest has no trusted_internal bool [C4].
- [x] Protocol phased model [C5/C12].

IF ANY CHECKBOX IS UNCHECKED: Luther self-hosting is NOT qualified under this
plan. Record the gap; do not declare viability.

## Failure Recovery

If an escape is found: fix it in the responsible phase, re-run the canaries and
the escape audit. Two-cycle cap. After two cycles, the qualification fails and
the residual gap is documented for a follow-up plan.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P19.md`

## Plan Completion

When P19A passes, update `../execution-tracker.md`:
- All phases PASS.
- Plan Status: **QUALIFIED** (self-hosting viable under bounded scope).

## Explicitly Deferred (restated, out of scope)

- Distributed persistence.
- Async engine redesign.
- Arbitrary legacy exact recovery.
- Broader llxprt roadmap.
