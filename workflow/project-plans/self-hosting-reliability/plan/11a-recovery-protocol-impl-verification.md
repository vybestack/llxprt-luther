# Phase 11A: Recovery Protocol Implementation Verification

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P11A`

## Prerequisites

- Required: P11 completed.

## Purpose

Verify the implementation is real (not placeholder) and satisfies the
requirements, with semantic checks beyond markers.

## Verification Commands

```bash
set -euo pipefail
cargo test --test recovery_protocol_integration_tests || exit 1
cargo test || exit 1
cargo clippy --workspace --all-targets --all-features -- -D warnings || exit 1

# Deferred implementation detection
grep -rn -E "(todo!|unimplemented!|TODO|FIXME|HACK|placeholder|not yet|will be)" workflow/src/engine/recovery/protocol/ workflow/src/engine/recovery/policy.rs && { echo "FAIL: placeholder tokens found"; exit 1; } || true
# Expected: no matches in implementation code

# No empty function bodies
grep -rn "fn .* \{\s*\}" workflow/src/engine/recovery/protocol/ workflow/src/engine/recovery/policy.rs && { echo "FAIL: empty function body"; exit 1; } || true
# Expected: no matches in implementation

# Confirm no in-memory facade; protocol calls durable store directly
grep -rn "recovery_epoch::\|recovery_operations::\|attempts::\|capsule_store::" workflow/src/engine/recovery/protocol/
# Expected: matches proving durable consumption

# C4: no trusted_internal in protocol
grep -rn "trusted_internal" workflow/src/engine/recovery/protocol/ && { echo "FAIL: trusted_internal present"; exit 1; } || true
```

## Semantic Verification Checklist

### Behavioral Verification Questions
1. **Does `recover()` DO what REQ-RP-001 says?**
   - [x] I read `protocol/mod.rs`; one entry point dispatches; no parallel path.
2. **Is the epoch CAS real?**
   - [x] The reserve phase opens a short IMMEDIATE tx before the CAS.
   - [x] A stale epoch returns `StaleEpoch` after rollback. [C1]
3. **Is the operation ledger reconciliation real?**
   - [x] Completed duplicate returns prior outcome. [C2]
   - [x] Pending duplicate reconciles (not duplicated). [C2]
   - [x] Conflicting duplicate refuses. [C2]
4. **Is the sealed authority real?**
   - [x] `RecoveryAuthority` is constructed only inside the protocol from exact
        durable state. [C4]
   - [x] `WorkspaceAuthorization` is revalidated in the reserve tx (TOCTOU). [C4]
   - [x] No `trusted_internal` bool exists. [C4]
5. **Is the phased model real?**
   - [x] Prepare runs outside any tx. [C5]
   - [x] Reserve is a short IMMEDIATE tx. [C5]
   - [x] Execute runs with no tx. [C5]
   - [x] Finalize is a short IMMEDIATE tx and only it may return Recovered. [C5/C12]
6. **Would P10 tests FAIL if implementation were removed?**
   - [x] Yes — they assert `RecoveryOutcome` variants and durable row counts.
7. **No in-memory facade?**
   - [x] `recover()` calls `recovery_epoch::cas_advance_epoch` /
        `recovery_operations::lookup_operation` /
        `attempts::record_attempt_start` / `capsule_store::load_capsule_v1`
        (durable).

### Integration Points Verified

- [x] `recover()` reads from the durable persistence layer (real SQLite, not mocked).
- [x] `ContinueWorkspace` delegates to `workspace_ownership` verification (same
      types used by `src/engine/runner.rs`).
- [x] `policy_for_step` covers the `SAFE_RERUN_STEPS` set referenced in
      `src/engine/continuation.rs` and consumes the canonical StepDef. [C6]

### Edge Cases Verified (via P10 tests)

- [x] Stale epoch rejected. [C1]
- [x] Conflicting duplicate refused. [C2]
- [x] Generic shell/write_file step_id fails closed (`NonRecoverable`). [C6]
- [x] TOCTOU ownership mismatch refused. [C4]
- [x] Mismatched worktree/base refused.
- [x] Protocol does not return Recovered before finalize. [C12]

## Holistic Functionality Assessment

- What was implemented: protocol dispatch + phased model + epoch CAS + operation
  ledger + sealed authority + policy
- Does it satisfy REQ-RP-001/004/005/006? PASS — per requirement
- Data flow: request → prepare(no tx) → reserve(IMMEDIATE tx: CAS + ledger) → execute(no tx) → finalize(IMMEDIATE tx: guarded) → outcome
- Verdict: PASS

## Failure Recovery

Two-cycle cap on semantic review. If gaps remain after two cycles, record them
as follow-ups and proceed (do not expand scope).

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P11A.md`
