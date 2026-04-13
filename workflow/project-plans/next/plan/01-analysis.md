# Phase 01: Domain Analysis

## Phase ID

`PLAN-20260408-STEP-EXEC.P01`

## Prerequisites

- Required: Phase 00A (preflight) completed
- Verification: `.completed/P00A.md` exists with PASS

## Purpose

Analyze the existing engine code to understand exact modification points, identify the boundary between engine routing and step execution, and document integration touch points for the executor system.

## Analysis Tasks

### 1. Engine Execution Boundary

Analyze `src/engine/runner.rs::execute_step()` to document:
- Current signature and return type
- How it's called from `run()` loop
- What data is available at the call site (step_id, instance, config)
- How StepOutcome flows back into transition resolution

### 2. Step Definition Data Flow

Trace how `StepDef` data flows from TOML → schema → engine:
- How `parameters` field is parsed (serde_json::Value)
- Where step_type is available for dispatch
- How to look up a StepDef by step_id from WorkflowInstance

### 3. Existing Test Compatibility

Analyze existing tests to understand:
- Which tests call `execute_step()` directly vs through `run()`
- Which tests create `WorkflowInstance` with what step_types
- What would break if `execute_step()` no longer returns hardcoded Success
- Strategy for backward compatibility (test-only default executor vs registry fallback)

### 4. Context Passing Design

Analyze where step context should live:
- Can it be a field on `EngineRunner`? (runner already has mutable access)
- Should it be on `WorkflowInstance`? (instance is owned by runner)
- What lifetime/ownership constraints exist?

### 5. Integration Touch Points

Document every file that needs modification with specific line references.

## Deliverables

Create `project-plans/next/analysis/domain-model.md` containing:
1. Engine execution boundary analysis
2. Step definition data flow diagram
3. Existing test compatibility strategy
4. Context ownership decision
5. File-by-file modification plan with line references

## Verification Commands

```bash
# Analysis file exists
ls project-plans/next/analysis/domain-model.md

# Contains all required sections
grep -c "## " project-plans/next/analysis/domain-model.md
# Expected: 5+ section headers
```

## Success Criteria

- All five analysis areas documented with evidence from the codebase
- Specific file:line references for every modification point
- Clear backward-compatibility strategy that preserves existing 118 tests

## Phase Completion Marker

Create: `project-plans/next/plan/.completed/P01.md`
