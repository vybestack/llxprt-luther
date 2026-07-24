# Phase 14: Capsule Adapter Wiring Implementation

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P14`

## Prerequisites

- Required: P13A completed with PASS (red phase confirmed).

## Purpose

Implement the launch→persist and resume→load→adapter wiring so all P13 tests
pass. The P08B fresh-launch path already builds the capsule and atomically persists
it with the initial run row. P14 preserves that invariant. The resume path loads
it and dispatches through `adapter_for` → `Box<dyn CapsuleAdapter>` and the
capsule-backed `RecoveryExecutor`.

**[B8]** The wiring targets the ACTUAL resume surfaces (verified by source
inspection), NOT generic placeholders. Fresh launch remains one atomic
run-plus-capsule transaction; no separate capsule persistence path is added.

> **Pseudocode references are to `02-pseudocode.md` component-local line
> numbers (B13): capsule lines 43–103 and adapters lines 01–20.**

## Requirements Implemented

### REQ-RP-002: Immutable canonical capsule with envelope digest (launch wiring) [C8]
Fresh-launch surfaces build + persist the capsule before any step.

### REQ-RP-009: Versioned capsule execution via object-safe adapters (resume wiring) [C8]
Resume loads the capsule, verifies the envelope digest, dispatches through
`adapter_for`, and reconstructs the `WorkflowInstance` via the adapter.

## Implementation Tasks

### Files to Modify [B8]

- `src/app/run.rs`
  - **Fresh launch path** (`handle_run_command` →
    `create_durable_runner_with_provenance` →
    `EngineRunner::with_db_path_for_launch`): preserve the existing P08B call to
    `persist_launch_atomically`, which inserts the initial `Starting` row and
    immutable capsule together. A failure leaves neither row. [B8]
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14`, `/// @requirement:REQ-RP-002,REQ-RP-009`

- `src/app/daemon_run.rs`
  - **Daemon launch path**: `LaunchProvenance::from_resolved` records provenance,
    then `create_durable_runner_with_provenance`. Build + persist capsule BEFORE
    `run_daemon_runner` executes. [B8]
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14`, `/// @requirement:REQ-RP-002,REQ-RP-009`

- `src/engine/executors/parent_orchestration/child_workflow.rs` /
    `child_run.rs`
  - **Child launch** (`launch_child_workflow`): build + persist capsule before
    child runner executes. [B8]
  - **Child resume** (`resume_child_workflow` via `prepare_child_resume_readonly`):
    load the capsule instead of reconstructing the instance ad hoc. [B8]
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14`, `/// @requirement:REQ-RP-009`

- `src/app/runs/continuation_execution.rs`
  - **Resume path** (`reconstruct_runner` / `reconstruct_runner_with_config`):
    load the persisted capsule via `capsule_store::load_capsule_v1` +
    `verify_envelope_digest` [C8/B9] + `adapter_for`; reconstruct the
    `WorkflowInstance` through the adapter instead of ad-hoc reconstruction. [B8]
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14`, `/// @requirement:REQ-RP-009`

- `src/engine/runner.rs`
  - `resume_from_checkpoint()` (private): if surfaced, load capsule via
    `capsule_store::load_capsule_v1`, `verify_envelope_digest` [C8/B9],
    `adapter_for`, reconstruct the instance via the adapter.
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14`, `/// @requirement:REQ-RP-009`

- `src/engine/recovery/adapters/v1.rs`
  - Implement `version(&self) -> u32 { 1 }`, `build_instance(capsule) -> WorkflowInstance`,
    `step_def_for(capsule, step_id)`, `envelope_digest(capsule) -> envelope digest`. [C8/B9]
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14`

### Atomic Run-and-Capsule Persistence [B8]

The immutable capsule and initial `Starting` run row MUST be inserted in the
same SQLite IMMEDIATE transaction through `persist_launch_atomically`. Any
capsule validation, run collision, capsule collision, constraint, or commit
failure rolls back both rows. This ensures every fresh run row has its exact
capsule before execution and neither an orphan capsule nor a capsule-less fresh
run is intentionally published. [B10]

## Verification Commands

```bash
cargo test --test capsule_wiring_integration_tests || exit 1
git diff workflow/tests/capsule_wiring_integration_tests.rs | grep -E "^[+-]" | grep -v "^[+-]{3}" && echo "FAIL: tests modified"
grep -rn "println!\|dbg!\|todo!\|unimplemented!" workflow/src/engine/runner.rs workflow/src/app/run.rs workflow/src/app/daemon_run.rs workflow/src/engine/executors/parent_orchestration/child_workflow.rs workflow/src/app/runs/continuation_execution.rs workflow/src/engine/recovery/adapters/v1.rs | grep -i capsule && echo "FAIL"
# B8: capsule wiring in actual surfaces
grep -rn "build_capsule_v1\|persist_capsule_v1\|load_capsule_v1\|adapter_for" workflow/src/app/run.rs workflow/src/app/daemon_run.rs workflow/src/engine/executors/parent_orchestration/child_workflow.rs workflow/src/app/runs/continuation_execution.rs workflow/src/engine/runner.rs
cargo test || exit 1
cargo clippy -- -D warnings || exit 1
```

## Success Criteria

- All P13 tests pass.
- No test modifications.
- No debug/placeholder code in the wiring.
- Full suite passes.
- Adapter dispatch is object-safe (`Box<dyn CapsuleAdapter>`). [C8]

## Failure Recovery

`git checkout` modified files; re-run P14. Two-cycle cap.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P14.md`
