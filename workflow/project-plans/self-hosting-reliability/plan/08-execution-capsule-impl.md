# Phase 08: ExecutionCapsuleV1 Implementation (Milestone 2 Complete)

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P08`

## Prerequisites

- Required: P07A completed with PASS (red phase confirmed).

## Purpose

Implement `build_capsule_v1`, `build_envelope_frame`, `verify_envelope_digest`
[C8/B9], the immutable capsule store, and the V1 adapter to make all P07 tests
pass. Follow pseudocode:
- capsule pseudocode lines 43–103 [C8/B9]
- adapters pseudocode lines 10–15 [C8/B9]

Reuse the canonical serialization approach already established in
`src/persistence/launch_provenance.rs` (`canonicalize_workflow_type`,
`canonicalize_workflow_config`, `compute_provenance_digest`).

> **Pseudocode references are to `02-pseudocode.md` component-local line
> numbers (B13).**

## Requirements Implemented

### REQ-RP-002: Immutable canonical capsule with envelope digest [C8/B9]

**Implementation** (capsule pseudocode 43–103):
- `build_envelope_frame` produces a deterministic byte frame: fixed-width
  big-endian version header (schema/canonicalization/domain/provenance) +
  length-prefixed authority fields. [B9]
- `build_capsule_v1` canonicalizes inputs, serializes the resolved workflow/
  config bytes (via `canonicalize_workflow_type`/`canonicalize_workflow_config`),
  computes `launch_provenance_digest` (via `compute_provenance_digest`), computes
  the envelope digest over the framed canonical envelope. Component digests are
  metadata. [C8/B9]
- `persist_capsule_v1` uses `INSERT` with `run_id` PRIMARY KEY and refuses
  overwrite (no `ON CONFLICT DO UPDATE`); a second insert for the same run_id
  errors. [B10: no historical backfill]
- `load_capsule_v1` returns the stored capsule; `verify_envelope_digest`
  recomputes the framed envelope digest, compares, and performs fail-closed
  version dispatch against `SUPPORTED_*_VERSIONS`. [B9]

### REQ-RP-009: Versioned object-safe adapter [C8/B9]

**Implementation** (adapters pseudocode 10–15):
- `adapter_for` matches `schema_version` → `V1Adapter`; unknown → error
  (fail-closed). [B9]
- `V1Adapter::version(&self)` returns `1` (object-safe). [C8]
- `V1Adapter::envelope_digest` returns the capsule's embedded envelope digest. [C8]

## Implementation Tasks

### Files to Modify

- `src/engine/recovery/capsule.rs`
  - Implement `build_envelope_frame` (capsule lines 43–56): big-endian version
    header + length-prefixed authority fields. [B9]
  - Implement `build_capsule_v1` (capsule lines 59–80): canonicalize, serialize,
    compute framed envelope digest, build.
  - Implement `verify_envelope_digest` (capsule lines 83–103): fail-closed
    version dispatch against SUPPORTED_*_VERSIONS, recompute framed envelope,
    compare, error on mismatch. [C8/B9]
  - The framed envelope covers: schema/canonicalization/domain/provenance
    versions + run_id, config_root_encoding, resolved_workflow_bytes,
    resolved_config_bytes, launch_provenance_digest, base_ref. Component digests
    (workflow_digest, config_digest) are metadata. [C8/B9]
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08`, `/// @requirement:REQ-RP-002`

- `src/persistence/capsule_store.rs`
  - Implement `persist_capsule_v1`: `CREATE TABLE IF NOT EXISTS execution_capsules (...)`,
    `INSERT` (refuse duplicate run_id via PRIMARY KEY conflict → error, not replace).
  - Implement `load_capsule_v1`: `SELECT ... WHERE run_id = ?1`.
  - Table stores `envelope_digest` as authority; component digests as metadata. [C8]
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08`, `/// @requirement:REQ-RP-002`

- `src/engine/recovery/adapters/mod.rs`
  - Implement `adapter_for` (adapters lines 10–15). [C8]
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08`, `/// @requirement:REQ-RP-009`

- `src/engine/recovery/adapters/v1.rs`
  - Implement `V1Adapter` methods (`version`, `build_instance`, `step_def_for`,
    `envelope_digest`). [C8]
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08`

## Verification Commands

```bash
cargo test --test execution_capsule_integration_tests || exit 1
git diff workflow/tests/execution_capsule_integration_tests.rs | grep -E "^[+-]" | grep -v "^[+-]{3}" && echo "FAIL: tests modified"
grep -rn "println!\|dbg!\|todo!\|unimplemented!" workflow/src/engine/recovery/capsule.rs workflow/src/persistence/capsule_store.rs workflow/src/engine/recovery/adapters/ && echo "FAIL"
grep -rn -E "(placeholder|not yet|will be)" workflow/src/engine/recovery/ workflow/src/persistence/capsule_store.rs && echo "FAIL"
grep -r "@plan:PLAN-20260723-SELFHOST-RELIABILITY.P08" workflow/src/engine/recovery/ workflow/src/persistence/capsule_store.rs
cargo test || exit 1
```

## Success Criteria

- All P07 tests pass.
- No test modifications.
- No debug/placeholder code.
- Full suite passes.
- Milestone 2 gate: capsule persisted immutably; envelope digest verified. [C8]

## Failure Recovery

`git checkout` the modified impl files; re-run P08. Two-cycle cap.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P08.md`

## Milestone 2 Gate

After P08A passes, check the M2 box in `../execution-tracker.md`:
- [ ] `ExecutionCapsuleV1` persisted immutably at fresh launch. [B10: no backfill]
- [ ] ONE envelope digest over a FRAMED canonical envelope (fixed-width version
      header + length-prefixed authority fields); component digests are
      metadata. [C8/B9]
- [ ] Fail-closed version dispatch for unsupported schema/canonicalization/
      domain/provenance versions. [B9]
- [ ] Adapter is object-safe (`fn version(&self)`) with fail-closed dispatch. [C8/B9]
- [ ] `StepDef.recovery_policy` field added + included in canonical
      serialization. [B7]
- [ ] Overwrite/refuse behavior verified.
