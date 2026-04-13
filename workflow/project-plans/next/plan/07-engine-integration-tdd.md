# Phase 07: Behavioral TDD — Engine Integration and Hello-World Workflow

## Phase ID

`PLAN-20260408-STEP-EXEC.P07`

## Prerequisites

- Required: Phase 06A completed with PASS
- Verification: `.completed/P06A.md` exists with PASS

## Requirements Implemented (Tests Only)

### REQ-EXEC-001: Engine Dispatches to Executors
**Test**: Create `EngineRunner` with registry, run a simple 2-step workflow with shell steps, verify both execute.

### REQ-EXEC-002: Engine Handles Unregistered Step Type
**Test**: Run a workflow with an unregistered step_type via the engine runner, verify Fatal/Failure outcome.

### REQ-EXEC-005: Context Passes Between Steps via Engine
**Test**: Step A writes to context, step B reads from context — through the engine run loop (not just executor unit tests).

### REQ-EXEC-007: Hello-World Workflow End-to-End
**Full Text**: When the hello-world workflow is executed, the engine shall create a Rust project, write a test, write an implementation, run `cargo test`, and reach a `Success` outcome.
**Test**: Load hello-world-v1.toml and hello-world-config.toml, create EngineRunner with default executors, call `run()`, assert `RunOutcome::Success`.

### REQ-EXEC-010: Existing Tests Updated
**Test**: Existing tests already updated in Phase 06 to use registry. Full suite passes.

## Implementation Tasks

### Files to Create

- `tests/fixtures/workflows/valid/hello-world-v1.toml`
  - Hello-world workflow type definition (from specification)
  - Steps: init_project, write_test, write_impl, run_tests, complete
  - Transitions: linear with fixable loop from run_tests back to write_impl

- `tests/fixtures/workflow-configs/valid/hello-world-config.toml`
  - Config for hello-world workflow
  - max_retries: 2, max_iterations: 3, workspace_strategy: temp_clone

- `tests/hello_world_workflow_integration.rs`
  - MUST include: `/// @plan:PLAN-20260408-STEP-EXEC.P07`
  - MUST include: `/// @requirement:REQ-EXEC-007`
  - Test: Load workflow fixtures, create runner with executors, execute in temp dir, assert Success
  - Test: Engine dispatches to shell executor through run loop
  - Test: Engine dispatches to write_file executor through run loop
  - Test: Context values pass between steps through engine
  - Test: Unregistered step_type through engine produces failure
  - Minimum 5 behavioral integration tests

### Test Design Rules

- Tests expect REAL behavior — RunOutcome::Success after actual cargo test passes
- NO `#[should_panic]`
- Tests use `tempfile::tempdir()` for working directories
- Hello-world test will actually run `cargo init` and `cargo test` in a temp dir
- Each test has `@plan` and `@requirement` markers

## Verification Commands

```bash
# Fixture files exist
ls tests/fixtures/workflows/valid/hello-world-v1.toml
ls tests/fixtures/workflow-configs/valid/hello-world-config.toml

# Test file exists
ls tests/hello_world_workflow_integration.rs

# Tests compile (may fail to run until Phase 08)
cargo test --test hello_world_workflow_integration --no-run 2>&1 || true

# Plan markers
grep -c "@plan:PLAN-20260408-STEP-EXEC.P07" tests/hello_world_workflow_integration.rs
# Expected: 5+

# No reverse testing
grep -c "should_panic" tests/hello_world_workflow_integration.rs
# Expected: 0
```

## Success Criteria

- 5+ behavioral integration tests created
- Hello-world workflow fixtures created and parseable
- Tests tagged with plan and requirement markers
- Tests fail naturally (TDD red phase) — they depend on Phase 08 engine integration

## Phase Completion Marker

Create: `project-plans/next/plan/.completed/P07.md`
