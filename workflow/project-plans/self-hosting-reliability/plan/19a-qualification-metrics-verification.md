# Phase 19A: Qualification Gate Verification

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P19A`

## Prerequisites

- Required: P19 completed (qualification report produced).

## Verification Commands

```bash
# Re-run the full suite and the canary harness
cargo test || exit 1
cargo test --test canary_harness_tests || exit 1
cargo test --test recovery_failpoint_matrix_tests || exit 1
cargo clippy -- -D warnings || exit 1

# Re-run the escape audit (see P19 commands)

# Verify epoch CAS, operation ledger, object-safe adapter
grep -rn "UPDATE recovery_epoch.*WHERE.*epoch" workflow/src/persistence/recovery_epoch.rs
grep -rn "WHERE.*status.*pending" workflow/src/persistence/recovery_operations.rs
grep -rn "fn version(&self)" workflow/src/engine/recovery/adapters/mod.rs
```

## Semantic Verification Checklist

1. **Are all qualification metrics measured (not PENDING)?** [yes/no]
2. **Are all targets met?** [yes/no]
3. **Is the qualification report present and evidence-backed?** [yes/no]

#### Final Gate
- [ ] Three consecutive mixed canaries: PASS.
- [ ] Zero prohibited escapes: confirmed.
- [ ] Failpoint matrix: 14/14.
- [ ] Typed merge binding (atomic tx [C11], strategy-specific proof [C10]): confirmed.
- [ ] Append-only (complete StateSnapshot [C3]) + epoch CAS ([C1]): confirmed.
- [ ] Operation ledger idempotency ([C2]): confirmed.
- [ ] RecoveryRequest has no trusted_internal ([C4]): confirmed.
- [ ] Protocol phased model ([C5/C12]): confirmed.
- [ ] No safety surface weakened (ownership, provenance, lease, checkpoint, CI,
      scope, review).

## Holistic Functionality Assessment (final)

- What was qualified: [self-hosting viability under bounded scope]
- Qualification result: [QUALIFIED / NOT QUALIFIED]
- Residual gaps (if any): [list, deferred to follow-up plans]

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
