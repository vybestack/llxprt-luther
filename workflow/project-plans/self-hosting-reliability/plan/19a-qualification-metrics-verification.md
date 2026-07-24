# Phase 19A: Qualification Gate Verification

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P19A`

## Prerequisites

- Required: P19 completed (qualification report produced).

## Verification Commands

```bash
set -euo pipefail
# Re-run the full suite and the canary harness
cargo test || exit 1
cargo test --test canary_harness_tests || exit 1
cargo test --test recovery_failpoint_matrix_tests || exit 1
cargo clippy --workspace --all-targets --all-features -- -D warnings || exit 1

# Re-run the escape audit (see P19 commands)

# Verify epoch CAS, operation ledger, object-safe adapter
grep -rn "UPDATE recovery_epoch.*WHERE.*epoch" workflow/src/persistence/recovery_epoch.rs
grep -rn "WHERE.*status.*pending" workflow/src/persistence/recovery_operations.rs
grep -rn "fn version(&self)" workflow/src/engine/recovery/adapters/mod.rs
```

## Semantic Verification Checklist

1. **Are all qualification metrics measured (not PENDING)?** [yes]
2. **Are all targets met?** [yes]
3. **Is the qualification report present and evidence-backed?** [yes]

### Final Gate

- [x] Three consecutive mixed canaries: PASS.
- [x] Zero prohibited escapes: confirmed.
- [x] Failpoint matrix: 14/14.
- [x] Typed merge binding (atomic tx [C11], strategy-specific proof [C10]): confirmed.
- [x] Append-only (complete StateSnapshot [C3]) + epoch CAS ([C1]): confirmed.
- [x] Operation ledger idempotency ([C2]): confirmed.
- [x] RecoveryRequest has no trusted_internal ([C4]): confirmed.
- [x] Protocol phased model ([C5/C12]): confirmed.
- [x] No safety surface weakened (ownership, provenance, lease, checkpoint, CI,
      scope, review).

## Holistic Functionality Assessment (final)

- What was qualified: self-hosting viability under bounded scope
- Qualification result: QUALIFIED
- Residual gaps (if any): none within bounded scope

## Plan Completion

Update `../execution-tracker.md`:
- Plan Status: **QUALIFIED** (or NOT QUALIFIED with documented gaps).
- All phase rows marked PASS.
- Completion markers all checked.

## Failure Recovery

If qualification fails after two cycles: the plan does NOT declare viability.
Document the specific failing metric and the proposed follow-up. Do not weaken
any gate to force a pass.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P19A.md`
