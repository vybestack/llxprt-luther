# Phase 07: ExecutionCapsuleV1 Integration-First TDD (Milestone 2)

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P07`

## Prerequisites

- Required: P06A completed with PASS.

## Purpose

Write behavioral integration tests for `ExecutionCapsuleV1` build, envelope
digest verification [C8], immutable persistence, and adapter loading. Tests
exercise the capsule through the real persistence layer (SQLite).

## Requirements Implemented (Expanded)

### REQ-RP-002: Immutable canonical capsule with envelope digest [C8]
**Behavior**:
- GIVEN: a freshly resolved workflow type + config + provenance + base ref
- WHEN: `build_capsule_v1(...)` is called
- THEN: returns a capsule whose `verify_envelope_digest` succeeds [C8]
- GIVEN: a persisted capsule for run R
- WHEN: `persist_capsule_v1(conn, modified_capsule_for_R)` is called again
- THEN: returns an error (immutable; no overwrite)
- WHEN: `load_capsule_v1(conn, R)` is called
- THEN: returns the ORIGINAL capsule, byte-identical envelope digest

### REQ-RP-009: Versioned object-safe adapter [C8]
**Behavior**:
- GIVEN: a V1 capsule
- WHEN: `adapter_for(capsule)` is called
- THEN: returns a `Box<dyn CapsuleAdapter>` where `.version() == 1` [C8]
- GIVEN: a capsule with `schema_version = 99`
- WHEN: `adapter_for(capsule)` is called
- THEN: returns `AdapterError::UnsupportedCapsuleVersion(99)`

## Implementation Tasks

### Files to Create

- `tests/execution_capsule_integration_tests.rs`
  - MUST include: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P07`
  - Test cases:
    1. `build_capsule_v1` computes an envelope digest and `verify_envelope_digest` passes (REQ-RP-002) [C8]
    2. `build_capsule_v1` with a non-canonicalizable config root → `CapsuleError::Canonicalize` (REQ-RP-002)
    3. `persist_capsule_v1` then `load_capsule_v1` returns byte-identical envelope digest (REQ-RP-002) [C8]
    4. Re-persist (overwrite) for same run_id → error; load still returns original (REQ-RP-002)
    5. Tampered capsule (envelope digest changed) → `verify_envelope_digest` → `EnvelopeDigestMismatch` (REQ-RP-002) [C8]
    6. `adapter_for` V1 capsule → `V1Adapter` with `version() == 1` (REQ-RP-009) [C8]
    7. `adapter_for` unknown version → `UnsupportedCapsuleVersion` (REQ-RP-009)
    8. V1 adapter `envelope_digest` matches the capsule's embedded envelope digest (REQ-RP-009) [C8]
    9. Envelope digest changes if ANY replay authority field changes (run_id, config_root_encoding, resolved bytes, launch_provenance_digest, base_ref) (REQ-RP-002) [C8]
    10. Component digests (workflow/config) are metadata and do NOT affect envelope digest independently (REQ-RP-002) [C8]

## Required Code Markers

```rust
/// @plan PLAN-20260723-SELFHOST-RELIABILITY.P07
/// @requirement REQ-RP-002
#[test]
fn capsule_is_immutable_after_persist() { /* ... */ }
```

## Verification Commands

```bash
grep -r "@plan:PLAN-20260723-SELFHOST-RELIABILITY.P07" workflow/tests/execution_capsule_integration_tests.rs | wc -l
grep -r "should_panic" workflow/tests/execution_capsule_integration_tests.rs && echo "FAIL"
cargo test --test execution_capsule_integration_tests 2>&1 | head -30
# Expected: red phase (failures, not compile errors)
```

## Success Criteria

- 10+ behavioral tests, tagged, red phase.
- No reverse testing.
- Tests use real SQLite.
- Tests assert envelope digest (not capsule-scoped component digests). [C8]

## Failure Recovery

Strengthen assertions if tests pass with empty impl; fix stub constructors if
compile errors. Two-cycle cap.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P07.md`
