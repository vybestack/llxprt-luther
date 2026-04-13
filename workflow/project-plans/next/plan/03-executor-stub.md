# Phase 03: Executor Trait and Registry Stub

## Phase ID

`PLAN-20260408-STEP-EXEC.P03`

## Prerequisites

- Required: Phase 02A completed with PASS
- Verification: `.completed/P02A.md` exists with PASS

## Requirements Implemented

### REQ-EXEC-001: Step Executor Dispatch (stub)

**Full Text**: The engine shall dispatch step execution to a registered executor based on the step's `step_type` field.
**Behavior (stub)**: Types compile, dispatch method exists but may use `todo!()`
**Why This Matters**: Establishes the extension point for all step execution

### REQ-EXEC-005: Step Context (stub)

**Full Text**: The step context shall carry key-value pairs across step executions within a single run.
**Behavior (stub)**: StepContext struct compiles with all fields
**Why This Matters**: Context is the data bus between steps

## Implementation Tasks

### Files to Create

- `src/engine/executor.rs`
  - `StepExecutor` trait with `execute()` method
  - `ExecutorRegistry` struct with `register()` and `dispatch()` methods
  - `StepContext` struct with `run_id`, `workflow_type_id`, `config_id`, `work_dir`, `values`
  - `ExecutionError` enum
  - `interpolate_string()` function (can use `todo!()` in stub phase)
  - MUST include: `/// @plan:PLAN-20260408-STEP-EXEC.P03`

- `src/engine/executors/mod.rs`
  - `pub mod shell;`
  - `pub mod write_file;`
  - MUST include: `/// @plan:PLAN-20260408-STEP-EXEC.P03`

- `src/engine/executors/shell.rs`
  - `ShellExecutor` struct implementing `StepExecutor` (can use `todo!()`)
  - MUST include: `/// @plan:PLAN-20260408-STEP-EXEC.P03`

- `src/engine/executors/write_file.rs`
  - `WriteFileExecutor` struct implementing `StepExecutor` (can use `todo!()`)
  - MUST include: `/// @plan:PLAN-20260408-STEP-EXEC.P03`

### Files to Modify

- `src/engine/mod.rs`
  - ADD: `pub mod executor;`
  - ADD: `pub mod executors;`
  - ADD comment: `/// @plan:PLAN-20260408-STEP-EXEC.P03`

## Stub Phase Rules

In this stub phase, the following ARE allowed:
- `todo!()` macro in method bodies
- Returning default values where appropriate
- Empty struct impls

The following are NOT allowed:
- `// TODO:` comments
- Creating V2/copy files
- Modifying existing tests

## Verification Commands

```bash
# New files exist
ls src/engine/executor.rs
ls src/engine/executors/mod.rs
ls src/engine/executors/shell.rs
ls src/engine/executors/write_file.rs

# Compiles
cargo build --all-targets

# Existing tests still pass
cargo test

# Plan markers present
grep -r "@plan:PLAN-20260408-STEP-EXEC.P03" src/engine/
```

## Success Criteria

- All new files created with plan markers
- `cargo build` succeeds
- All existing 118 tests pass
- `StepExecutor` trait is defined with correct signature
- `StepContext` has all required fields

## Phase Completion Marker

Create: `project-plans/next/plan/.completed/P03.md`
