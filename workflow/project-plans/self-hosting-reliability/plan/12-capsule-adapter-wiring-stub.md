# Phase 12: Capsule Adapter Wiring Stub

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P12`

## Prerequisites

- Required: P11A completed with PASS (all three milestone gates checked).

## Purpose

Wire the capsule adapter into the engine's actual launch/resume surfaces so
that the current binary executes versioned capsules via object-safe adapters
[C8/B9]. **[B8]** The wiring targets the actual surfaces: `app/run.rs`,
`app/daemon_run.rs`, `parent_orchestration/child_workflow.rs`/`child_run.rs`,
`app/runs/continuation_execution.rs`. This phase creates the wiring skeleton;
P13 writes the integration tests; P14 implements.

> **Pseudocode references are to `02-pseudocode.md` component-local line
> numbers (B13).**

## Requirements Implemented (Expanded)

### REQ-RP-009: Versioned capsule execution via object-safe adapters (wiring half) [C8]
**Behavior**:
- GIVEN: a fresh launch via the CLI/daemon
- WHEN: the launch surface resolves the workflow
- THEN: it builds and persists an `ExecutionCapsuleV1` (with envelope digest)
       before any step
- GIVEN: a resume surface
- WHEN: it loads the run
- THEN: it loads the persisted capsule and dispatches through `adapter_for` →
       `Box<dyn CapsuleAdapter>` [C8]

## Implementation Tasks

### Files to Modify (stubs) [B8]

- `src/app/run.rs` / `src/app/daemon_run.rs`
  - **Fresh launch**: preserve the P08B `with_db_path_for_launch` path, which
    already builds the capsule and atomically inserts the initial run row plus
    immutable capsule in one IMMEDIATE transaction. Do not add separate
    persistence. [B8]
  - Mark the daemon resume call site that P14 will route through the production
    capsule-backed `RecoveryExecutor`.

- `src/engine/executors/parent_orchestration/child_workflow.rs` / `child_run.rs`
  - **Child launch**: preserve the P08B atomic launch pair.
  - **Child resume**: mark the call site for capsule-backed executor dispatch.
    [B8]
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P12`, `/// @requirement:REQ-RP-009`

- `src/app/runs/continuation_execution.rs`
  - **Resume** (`reconstruct_runner`): stub `load_capsule_v1` +
    `verify_envelope_digest` + `adapter_for` for capsule-driven reconstruction. [B8]
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P12`, `/// @requirement:REQ-RP-009`

- `src/engine/runner.rs`
  - `resume_from_checkpoint()` (currently a **private** method): stub
    `adapter_for` dispatch. [C8/B9]
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P12`, `/// @requirement:REQ-RP-009`

- `src/engine/recovery/mod.rs`
  - Re-export the wiring helpers.
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P12`

## Verification Commands

```bash
cargo build --all-targets || exit 1
cargo clippy -- -D warnings || exit 1
grep -r "@plan:PLAN-20260723-SELFHOST-RELIABILITY.P12" workflow/src/engine/runner.rs workflow/src/main.rs
grep -rn "// TODO\|// FIXME" workflow/src/engine/runner.rs workflow/src/main.rs | grep -i capsule && echo "FAIL"
```

## Success Criteria

- Compiles with stubs.
- Launch/resume call sites are identified and marked.
- Adapter dispatch is through `Box<dyn CapsuleAdapter>` (object-safe). [C8]

## Failure Recovery

`git checkout` modified files. Two-cycle cap.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P12.md`
