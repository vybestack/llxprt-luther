# Phase 02: Pseudocode

## Phase ID

`PLAN-20260408-STEP-EXEC.P02`

## Prerequisites

- Required: Phase 01A completed with PASS
- Verification: `.completed/P01A.md` exists with PASS

## Purpose

Write numbered pseudocode for all new components. Implementation phases MUST reference these line numbers.

## Pseudocode Files to Create

### 1. `analysis/pseudocode/executor-dispatch.md`

Covers:
- `StepExecutor` trait definition
- `ExecutorRegistry` struct and dispatch logic
- `ExecutionError` enum
- Default/fallback behavior for unregistered step_types
- Registry construction with built-in executors

### 2. `analysis/pseudocode/shell-executor.md`

Covers:
- Parameter extraction (`command`, `working_dir`, `timeout_seconds`)
- Variable interpolation in command string
- `std::process::Command` spawning with `sh -c`
- stdout/stderr capture into StepContext
- Exit code → StepOutcome mapping (0=Success, non-zero=Fixable)
- Spawn failure → Fatal

### 3. `analysis/pseudocode/write-file-executor.md`

Covers:
- Parameter extraction (`path`, `content`, `mkdir`)
- Variable interpolation in path and content
- Parent directory creation
- File write
- Error → Fatal mapping

### 4. `analysis/pseudocode/context-interpolation.md`

Covers:
- `StepContext` struct definition
- `interpolate_string()` function — `{key}` replacement from context values
- Built-in variables: `{work_dir}`, `{run_id}`, `{config_id}`, `{workflow_type_id}`
- Context value storage after step execution
- Value retrieval by key (with `{step_id.stdout}` convention)

## Requirements

- Every pseudocode file has numbered lines (line 1, line 2, etc.)
- Every line corresponds to one logical operation
- Implementation phases will cite these line numbers

## Verification Commands

```bash
# All pseudocode files exist
ls project-plans/next/analysis/pseudocode/executor-dispatch.md
ls project-plans/next/analysis/pseudocode/shell-executor.md
ls project-plans/next/analysis/pseudocode/write-file-executor.md
ls project-plans/next/analysis/pseudocode/context-interpolation.md

# Lines are numbered
grep -c "^[0-9]" project-plans/next/analysis/pseudocode/executor-dispatch.md
# Expected: 15+ numbered lines
```

## Success Criteria

- Four pseudocode files, all with numbered lines
- All REQ-EXEC requirements mapped to at least one pseudocode section
- Coverage of happy path, error paths, and edge cases

## Phase Completion Marker

Create: `project-plans/next/plan/.completed/P02.md`
