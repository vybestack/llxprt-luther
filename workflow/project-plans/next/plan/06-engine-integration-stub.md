# Phase 06: Engine Integration — Wire Executors Into Runner

## Phase ID

`PLAN-20260408-STEP-EXEC.P06`

## Prerequisites

- Required: Phase 05A completed with PASS
- Verification: `.completed/P05A.md` exists with PASS

## Purpose

Wire `ExecutorRegistry` and `StepContext` into `EngineRunner` so that `execute_step()` always dispatches through the registry. Update `EngineRunner::new()` to require an `ExecutorRegistry`. Update all existing callers (tests and main.rs) to supply one.

There is no backward-compatible fallback. No shims. No "if registry is present" branching. The old `execute_step()` that returned hardcoded `Success` is replaced entirely.

For existing tests whose workflows use step types like `"analysis"` and `"planning"` that have no real executor, a `NoOpExecutor` is provided. It returns `Success` for any step — but it must be explicitly registered, making the test's intent clear.

## Requirements Implemented

### REQ-EXEC-001: Executor Dispatch
**Full Text**: The engine shall dispatch step execution to a registered executor based on the step's `step_type` field.
**Implementation**: `EngineRunner` always has a registry. `execute_step()` always dispatches through it.

### REQ-EXEC-002: Unregistered Step Type → Fatal
**Full Text**: If no executor is registered for a step's `step_type`, then the engine shall return a Fatal outcome.
**Implementation**: `ExecutorRegistry::dispatch()` returns Fatal for unknown types. No silent fallback.

### REQ-EXEC-010: Existing Tests Updated
**Full Text**: All existing tests shall be updated to use the new constructor and continue to pass.
**Implementation**: Every `EngineRunner::new(instance)` call updated to `EngineRunner::new(instance, registry)`. Tests register a `NoOpExecutor` for their step types.

## Implementation Tasks

### Files to Create

- `src/engine/executors/noop.rs`
  - `NoOpExecutor` — implements `StepExecutor`, returns `Ok(StepOutcome::Success)` for any step
  - Intended for tests where step execution isn't the thing being tested
  - MUST include: `/// @plan:PLAN-20260408-STEP-EXEC.P06`

### Files to Modify

- `src/engine/executors/mod.rs`
  - ADD: `pub mod noop;`

- `src/engine/runner.rs`
  - Change `EngineRunner::new()` to take `(instance, registry)` — no optional, no Option
  - Add `StepContext` field, initialized from instance metadata
  - Replace `execute_step()` body: look up StepDef by step_id, dispatch through `registry.dispatch(step_type, step, ctx)`
  - Remove the old hardcoded `Ok(StepOutcome::Success)` return entirely
  - Remove `with_db_path` or update it to also require registry
  - ADD: `/// @plan:PLAN-20260408-STEP-EXEC.P06`

- `src/engine/mod.rs`
  - Add re-exports for `StepExecutor`, `ExecutorRegistry`, `StepContext`, `NoOpExecutor`
  - ADD: `/// @plan:PLAN-20260408-STEP-EXEC.P06`

- `src/main.rs`
  - `handle_run_command`: create `ExecutorRegistry::with_defaults()` and pass to `EngineRunner::new()`
  - ADD: `/// @plan:PLAN-20260408-STEP-EXEC.P06`

- **All existing test files that construct `EngineRunner`**:
  - Update to pass an `ExecutorRegistry` with `NoOpExecutor` registered for the step types used in the test
  - Use a helper like `ExecutorRegistry::with_noop_default()` or register explicitly
  - ADD: `/// @plan:PLAN-20260408-STEP-EXEC.P06` on modified test functions

## Verification Commands

```bash
# Compiles
cargo build --all-targets

# ALL tests pass — existing tests updated, not deleted
cargo test
# Expected: 118+ tests pass, 0 failures

# Old fallback gone — no hardcoded Success in execute_step
grep -n "Ok(StepOutcome::Success)" src/engine/runner.rs
# Expected: no matches in execute_step (may appear elsewhere for other purposes)

# Plan markers present
grep -r "@plan:PLAN-20260408-STEP-EXEC.P06" src/engine/

# NoOpExecutor exists
grep -r "NoOpExecutor" src/engine/executors/noop.rs
```

## Success Criteria

- `cargo build` succeeds
- All existing tests pass (updated, not shimmed)
- `EngineRunner::new()` always requires an `ExecutorRegistry`
- `execute_step()` always dispatches — no fallback path
- `NoOpExecutor` exists for test convenience

## Phase Completion Marker

Create: `project-plans/next/plan/.completed/P06.md`
