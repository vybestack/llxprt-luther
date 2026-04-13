# Phase 05: Executor Implementation

## Phase ID

`PLAN-20260408-STEP-EXEC.P05`

## Prerequisites

- Required: Phase 04A completed with PASS
- Verification: `.completed/P04A.md` exists with PASS

## Requirements Implemented

### REQ-EXEC-001: Executor Dispatch
**Full Text**: The engine shall dispatch step execution to a registered executor based on the step's `step_type` field.
**Implementation**: `ExecutorRegistry::dispatch()` looks up executor by step_type and calls `execute()`.

### REQ-EXEC-002: Unregistered Step Type → Fatal
**Full Text**: If no executor is registered, return Fatal.
**Implementation**: `dispatch()` returns `Ok(StepOutcome::Fatal)` with error context for unknown types.

### REQ-EXEC-003: Shell Executor
**Full Text**: Run shell command, capture stdout/stderr, map exit codes.
**Implementation**: `ShellExecutor::execute()` uses `std::process::Command::new("sh").arg("-c").arg(command)`.

### REQ-EXEC-004: Write-File Executor
**Full Text**: Write content to path relative to work_dir.
**Implementation**: `WriteFileExecutor::execute()` resolves path, creates dirs, writes content.

### REQ-EXEC-005: Step Context
**Full Text**: Context carries key-value pairs across executions.
**Implementation**: `StepContext.values` is a `HashMap<String, serde_json::Value>` populated by executors.

### REQ-EXEC-006: Variable Interpolation
**Full Text**: Parameters support `{variable}` interpolation.
**Implementation**: `interpolate_string()` replaces `{key}` patterns from context values and built-ins.

### REQ-EXEC-008: Shell Failure → Fixable
**Full Text**: Non-zero exit returns Fixable with captured output.
**Implementation**: Exit code checked; stdout/stderr stored as `{step_id}.stdout`/`{step_id}.stderr`.

### REQ-EXEC-009: Shell Spawn Failure → Fatal
**Full Text**: Spawn failure returns Fatal.
**Implementation**: `Command::output()` error caught and mapped to Fatal.

## Implementation Tasks

### Files to Modify

- `src/engine/executor.rs`
  - Replace `todo!()` stubs with real implementations
  - Implement `ExecutorRegistry::dispatch()` — look up by step_type, call execute
  - Implement `ExecutorRegistry::with_defaults()` — register shell and write_file executors
  - Implement `interpolate_string()` — `{key}` replacement
  - Implement `StepContext::new()` and value accessors
  - ADD: `/// @plan:PLAN-20260408-STEP-EXEC.P05`
  - Reference pseudocode: `analysis/pseudocode/executor-dispatch.md` lines for dispatch
  - Reference pseudocode: `analysis/pseudocode/context-interpolation.md` lines for interpolation

- `src/engine/executors/shell.rs`
  - Implement `ShellExecutor::execute()`
  - Extract `command` from `step.parameters`
  - Interpolate variables in command string
  - Spawn `sh -c <command>` via `std::process::Command`
  - Capture stdout/stderr, store in context
  - Map exit code: 0 → Success, non-zero → Fixable, spawn error → Fatal
  - ADD: `/// @plan:PLAN-20260408-STEP-EXEC.P05`
  - Reference pseudocode: `analysis/pseudocode/shell-executor.md`

- `src/engine/executors/write_file.rs`
  - Implement `WriteFileExecutor::execute()`
  - Extract `path` and `content` from `step.parameters`
  - Interpolate variables in path and content
  - Resolve path relative to `ctx.work_dir`
  - Create parent directories if needed
  - Write content to file
  - ADD: `/// @plan:PLAN-20260408-STEP-EXEC.P05`
  - Reference pseudocode: `analysis/pseudocode/write-file-executor.md`

### Zero Tolerance for Placeholders

This is an IMPLEMENTATION phase:
- `todo!()` = FAIL
- `unimplemented!()` = FAIL
- `// TODO:` = FAIL
- Empty function bodies = FAIL

## Verification Commands

```bash
# All Phase 04 tests pass
cargo test --test executor_unit_tests

# No placeholders
grep -rn "todo!\|unimplemented!" src/engine/executor.rs src/engine/executors/
# Expected: no matches

grep -rn "// TODO\|// FIXME\|placeholder" src/engine/executor.rs src/engine/executors/
# Expected: no matches

# Existing tests still pass
cargo test

# Clippy clean
cargo clippy -- -D warnings
```

## Success Criteria

- All Phase 04 TDD tests pass
- All existing 118+ tests pass
- No placeholders in executor code
- Clippy passes

## Phase Completion Marker

Create: `project-plans/next/plan/.completed/P05.md`
