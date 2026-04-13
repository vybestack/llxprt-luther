# Phase 04: Behavioral TDD — Executors

## Phase ID

`PLAN-20260408-STEP-EXEC.P04`

## Prerequisites

- Required: Phase 03A completed with PASS
- Verification: `.completed/P03A.md` exists with PASS
- Expected files from previous phase: `src/engine/executor.rs`, `src/engine/executors/{mod,shell,write_file}.rs`

## Requirements Implemented (Tests Only)

### REQ-EXEC-001: Executor Dispatch
**Full Text**: The engine shall dispatch step execution to a registered executor based on the step's `step_type` field.
**Test**: Register an executor for type "shell", dispatch a step with step_type="shell", verify the executor is called.

### REQ-EXEC-002: Unregistered Step Type
**Full Text**: If no executor is registered for a step's `step_type`, then the engine shall return a `Fatal` outcome.
**Test**: Dispatch a step with unregistered type, verify Fatal outcome.

### REQ-EXEC-003: Shell Executor Success
**Full Text**: ShellExecutor shall run a shell command, capture stdout/stderr, map exit 0 to Success.
**Test**: Execute `echo hello`, verify Success outcome, verify stdout captured in context.

### REQ-EXEC-004: Write-File Executor
**Full Text**: WriteFileExecutor shall write content to path relative to work_dir.
**Test**: Write a file to temp dir, verify file exists with correct content.

### REQ-EXEC-005: Context Carries Values
**Full Text**: Step context shall carry key-value pairs across executions.
**Test**: Execute step A that writes to context, execute step B, verify B can read A's output.

### REQ-EXEC-006: Variable Interpolation
**Full Text**: Step parameters shall support `{variable}` interpolation.
**Test**: Set `work_dir` in context, interpolate `{work_dir}/foo`, verify resolved path.

### REQ-EXEC-008: Shell Failure → Fixable
**Full Text**: Non-zero exit returns Fixable with stdout/stderr in context.
**Test**: Execute `exit 1`, verify Fixable outcome, verify stderr captured.

### REQ-EXEC-009: Shell Spawn Failure → Fatal
**Full Text**: If command cannot be spawned, return Fatal.
**Test**: Execute a nonexistent binary, verify Fatal outcome.

## Implementation Tasks

### Files to Create

- `tests/executor_unit_tests.rs`
  - MUST include: `/// @plan:PLAN-20260408-STEP-EXEC.P04`
  - Tests for: registry dispatch, unregistered type fallback, shell success, shell failure, shell spawn failure, write_file success, write_file IO error, context value passing, variable interpolation
  - Minimum 12 behavioral tests

### Test Design Rules

- Tests expect REAL behavior (Success/Fixable/Fatal outcomes with correct context values)
- NO testing for `todo!()` or panic
- NO `#[should_panic]`
- Tests will naturally fail until Phase 05 implementation
- Use `tempfile::tempdir()` for filesystem tests
- Each test has `@plan` and `@requirement` markers

## Verification Commands

```bash
# Test file exists
ls tests/executor_unit_tests.rs

# Tests compile (some may fail to compile if stubs are incomplete — that's OK for TDD red phase)
cargo test --test executor_unit_tests --no-run 2>&1 || true

# Plan markers present
grep -c "@plan:PLAN-20260408-STEP-EXEC.P04" tests/executor_unit_tests.rs
# Expected: 12+ occurrences

# No reverse testing
grep -c "should_panic" tests/executor_unit_tests.rs
# Expected: 0
```

## Success Criteria

- 12+ behavioral tests created
- Tests tagged with plan and requirement markers
- Tests expect real behavior (not panic/stub behavior)
- Tests fail naturally when run (TDD red phase)

## Phase Completion Marker

Create: `project-plans/next/plan/.completed/P04.md`
