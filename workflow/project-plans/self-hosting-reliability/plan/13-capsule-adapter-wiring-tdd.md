# Phase 13: Capsule Adapter Wiring Integration-First TDD

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P13`

## Prerequisites

- Required: P12A completed with PASS.

## Purpose

Write integration tests proving the binary executes a versioned capsule through
an object-safe adapter [C8] on both fresh launch and resume. These tests
exercise the launch→persist→load→adapter→run flow end to end (against in-memory
SQLite and temp dirs, mirroring existing `tests/*.rs` patterns).

## Requirements Implemented (Expanded)

### REQ-RP-002 + REQ-RP-009 end-to-end
**Behavior**:
- GIVEN: a fresh launch with a resolved workflow
- WHEN: the launch surface runs
- THEN: an `ExecutionCapsuleV1` with envelope digest is persisted before any
       step; the run executes through the V1 adapter (`Box<dyn CapsuleAdapter>`) [C8]
- GIVEN: an interrupted run with a persisted capsule
- WHEN: the resume surface runs
- THEN: it loads the capsule, verifies the envelope digest, dispatches through
       `adapter_for`, and resumes at the interrupted step [C8]
- GIVEN: a run whose persisted capsule envelope digest does not match (tampered config)
- WHEN: the resume surface runs
- THEN: it refuses before any step (digest mismatch)

## Implementation Tasks

### Files to Create

- `tests/capsule_wiring_integration_tests.rs`
  - MUST include: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P13`
  - Test cases:
    1. Fresh launch via `app/run.rs` path → capsule persisted before run row
       mutates (capsule-before-run-row ordering) (REQ-RP-002/009) [B8]
    2. Daemon launch via `app/daemon_run.rs` path → capsule persisted (REQ-RP-002/009) [B8]
    3. Child launch via `parent_orchestration/child_workflow.rs` → capsule
       persisted (REQ-RP-002/009) [B8]
    4. Resume via `continuation_execution.rs` reconstruct → loads capsule +
       verifies envelope digest + dispatches adapter (REQ-RP-009) [C8/B9/B8]
    5. Child resume via `resume_child_workflow` → loads capsule via adapter (REQ-RP-009) [B8]
    6. Tampered capsule envelope digest → resume refuses (REQ-RP-002/009) [C8/B9]
    7. Unknown capsule version on resume → adapter error, no step executes (REQ-RP-009) [C8/B9]
    8. Capsule persist failure aborts launch (no run row created) (REQ-RP-002) [B8/B10]

## Required Code Markers

```rust
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P13
/// @requirement:REQ-RP-009
#[test]
fn resume_loads_capsule_and_dispatches_adapter() { /* ... */ }
```

## Verification Commands

```bash
set -euo pipefail
count=$(grep -r "@plan:PLAN-20260723-SELFHOST-RELIABILITY.P13" workflow/tests/capsule_wiring_integration_tests.rs | wc -l)
[ "$count" -ge 8 ] || { echo "FAIL: expected 8+ P13 markers, found $count"; exit 1; }
grep -r "should_panic" workflow/tests/capsule_wiring_integration_tests.rs && { echo "FAIL"; exit 1; } || true
cargo test --test capsule_wiring_integration_tests 2>&1 | head -30
# Expected: red phase
```

## Success Criteria

- 8+ end-to-end wiring tests, tagged, red phase.
- Tests assert capsule persistence + object-safe adapter dispatch on ACTUAL
  surfaces, not just types. [C8/B8]

## Failure Recovery

Two-cycle cap.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P13.md`
