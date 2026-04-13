# Phase 08: Engine Integration and Hello-World Implementation

## Phase ID

`PLAN-20260408-STEP-EXEC.P08`

## Prerequisites

- Required: Phase 07A completed with PASS
- Verification: `.completed/P07A.md` exists with PASS

## Requirements Implemented

### REQ-EXEC-001: Executor Dispatch (full integration)
**Full Text**: The engine shall dispatch step execution to a registered executor based on the step's `step_type` field.
**Implementation**: `EngineRunner::execute_step()` with registry dispatches to real executors, passes StepContext, returns real outcomes.

### REQ-EXEC-005: Context Passes Through Engine Run Loop
**Full Text**: Step context carries key-value pairs across step executions.
**Implementation**: `StepContext` lives on `EngineRunner`, passed to each `execute_step()` call, accumulates values across the run.

### REQ-EXEC-006: Variable Interpolation in Engine Context
**Full Text**: Step parameters support `{variable}` interpolation.
**Implementation**: Before dispatching, engine interpolates step parameters using current context values.

### REQ-EXEC-007: Hello-World Workflow Runs End-to-End
**Full Text**: When the hello-world workflow is executed, the engine shall create a Rust project, write a test, write an implementation, run `cargo test`, and reach a `Success` outcome.
**Implementation**: All pieces connected — fixtures, executors, engine, context — producing a passing `cargo test` in a temp directory.

### REQ-EXEC-010: All Existing Tests Pass
**Full Text**: All existing tests shall be updated to use the new constructor and continue to pass.
**Implementation**: Already updated in Phase 06 to pass `ExecutorRegistry`. No fallback path.

## Implementation Tasks

### Files to Modify

- `src/engine/runner.rs`
  - Complete `execute_step()` dispatch: look up StepDef by step_id, get step_type, dispatch through registry
  - Pass `StepContext` to executor, collect results
  - Store executor output in context after each step
  - Interpolate step parameters before dispatch
  - ADD: `/// @plan:PLAN-20260408-STEP-EXEC.P08`
  - Reference pseudocode: `analysis/pseudocode/executor-dispatch.md`

- `src/main.rs` (optional enhancement)
  - When `handle_run_command` creates an `EngineRunner`, use `with_executor_registry()` and default executors
  - This makes `luther run` actually execute steps for real workflows
  - ADD: `/// @plan:PLAN-20260408-STEP-EXEC.P08`

### Files to Verify Unchanged

- Existing test files in `tests/` should only have been modified in Phase 06 (constructor changes). No further changes needed here.
- `src/engine/instance.rs` — no changes needed
- `src/workflow/schema.rs` — no changes needed

### Zero Tolerance for Placeholders

This is the FINAL IMPLEMENTATION phase:
- `todo!()` = FAIL
- `unimplemented!()` = FAIL
- `// TODO:` = FAIL
- Any hardcoded `Ok(StepOutcome::Success)` in `execute_step()` = FAIL

## Verification Commands

```bash
# All new integration tests pass
cargo test --test hello_world_workflow_integration
# Expected: 5+ tests pass

# All executor unit tests still pass
cargo test --test executor_unit_tests
# Expected: 12+ tests pass

# ALL tests pass
cargo test
# Expected: 130+ tests pass (118 existing + 12+ executor + 5+ integration), 0 failures

# No placeholders in engine
grep -rn "todo!\|unimplemented!" src/engine/
# Expected: no matches (dagrs_runtime.rs stubs are pre-existing and acceptable)

# No placeholders in executors
grep -rn "todo!\|unimplemented!" src/engine/executor.rs src/engine/executors/
# Expected: no matches

# Clippy clean
cargo clippy -- -D warnings

# Manual verification: run hello-world workflow via CLI
cargo run -- run --workflow-type hello-world-v1 --config hello-world-config
# Expected: "Workflow completed successfully!"
```

## Success Criteria

1. All Phase 07 integration tests pass — including hello-world e2e
2. All Phase 04 executor unit tests pass
3. All existing 118+ tests pass (backward compatibility)
4. No placeholders in executor or engine code
5. Clippy clean
6. `luther run --workflow-type hello-world-v1` actually creates a project, writes files, runs cargo test, and succeeds
7. Total test count: 135+ (118 existing + new executor + new integration)

## Phase Completion Marker

Create: `project-plans/next/plan/.completed/P08.md`
