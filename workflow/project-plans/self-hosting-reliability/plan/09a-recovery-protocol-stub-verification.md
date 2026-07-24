# Phase 09A: Recovery Protocol Stub Verification

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P09A`

## Prerequisites

- Required: P09 completed.

## Purpose

Verify the stub skeleton compiles and exposes the types the integration tests
(P10) need, without yet implementing behavior.

## Verification Commands

```bash
cargo build --all-targets || exit 1
cargo clippy -- -D warnings || exit 1
grep -r "@plan:PLAN-20260723-SELFHOST-RELIABILITY.P09" workflow/src/engine/recovery/ | wc -l
grep -r "@requirement:REQ-RP-001" workflow/src/engine/recovery/protocol.rs
grep -r "@requirement:REQ-RP-005" workflow/src/engine/recovery/policy.rs
# C4 verification
grep -rn "trusted_internal" workflow/src/engine/recovery/protocol.rs && echo "FAIL: trusted_internal present"
```

## Structural Verification Checklist

- [ ] `cargo build --all-targets` succeeds.
- [ ] `cargo clippy -- -D warnings` succeeds.
- [ ] `RecoveryProtocolV1`, `RecoveryRequest`, `RecoveryAuthority` (sealed),
      `PreparedRecovery` (sealed), `RecoveryOutcome`, `RecoveryStrategy`,
      `StepRecoveryPolicy` are public and constructible (the sealed types only
      internally). [C4]
- [ ] `RecoveryRequest` does NOT carry `trusted_internal: bool`. [C4]
- [ ] No `// TODO` / `// FIXME` comments (todo!() macro is permitted in stub).
- [ ] No duplicate-version file names.

## Semantic Verification Checklist

- [ ] `RecoveryRequest` carries `run_id` and `step_id` (NO authorization bool). [C4]
- [ ] `RecoveryAuthority` is NOT public-constructible (sealed; constructed only
      inside the protocol). [C4]
- [ ] `RecoveryOutcome` has `AlreadyApplied { operation_id, prior_outcome }`
      (not just attempt_id) and `StaleEpoch` (not `StaleGeneration`). [C1/C2]
- [ ] `StepRecoveryPolicy` has all six variants from the spec.
- [ ] `policy_for_step` takes `step_def: &StepDef` (not just `step_type: &str`). [C6]
- [ ] `select_strategy` takes only `policy` (no `authorized_internal` parameter). [C4]
- [ ] `RefusalReason` includes `ConflictingOperation`. [C2]
- [ ] No in-memory persistence facade is introduced.

## Failure Recovery

If verification fails, remediate P09. Review-cycle cap: two cycles.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P09A.md`
