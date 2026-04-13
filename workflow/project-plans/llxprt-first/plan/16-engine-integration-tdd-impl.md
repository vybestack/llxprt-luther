# Phase 16: Engine Integration -- TDD + Implementation

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P16`

## Prerequisites

- Required: Phase 15a (Engine Integration Stub Verification) completed
- Verification: `grep -r "@plan:PLAN-20260408-LLXPRT-FIRST.P15" src/`
- Expected files from previous phases:
  - VerifyExecutor registered in default registry
  - Config variables loaded into StepContext
  - All four components individually implemented and tested

## Purpose

Write integration tests that exercise the four new components **working together through the engine**, then verify they pass. This is a combined TDD+Impl phase because the individual components are already implemented — we're testing the **integration seams**, not new behavior.

These tests use programmatic workflow construction (not TOML files) to isolate engine integration behavior from config loading.

## Requirements Implemented (Expanded)

### REQ-LF-PROF-003: Profile variable resolution through interpolation

**Full Text**: When a step references a profile variable, the engine shall resolve it through standard context variable interpolation — the workflow config values are loaded into context at run start.
**Behavior**:
- GIVEN: WorkflowConfig with `variables: {"profile_planning": "opusthinking"}`
- WHEN: A shell step runs `command = "echo {profile_planning}"`
- THEN: The command interpolates to `echo opusthinking`

### REQ-LF-PROF-004: Config-only profile changes

**Full Text**: Changing which model performs a role shall require only a workflow config edit, not a workflow type definition change.
**Behavior**:
- GIVEN: A workflow type using `{profile_planning}` in step parameters
- WHEN: Two different configs provide different values for `profile_planning`
- THEN: The same workflow type produces different interpolated commands

### REQ-LF-CTX-003: Executor-set variables namespaced by step_id

**Full Text**: When an executor sets a context variable during step execution, the engine shall store it namespaced under the current step_id.
**Behavior**:
- GIVEN: A workflow with steps A and B, both setting `stdout`
- WHEN: Step A runs, then step B runs
- THEN: `{A.stdout}` returns step A's value, `{B.stdout}` returns step B's, `{stdout}` returns step B's (most recent)

### REQ-LF-LOOP-001 through REQ-LF-LOOP-004: Per-edge limits through engine run

**Full Text**: Integration-level verification that per-edge loop limits work correctly when wired through the full engine `run()` path with real executor dispatch and transition resolution.
**Behavior**:
- GIVEN: A workflow with a loop-back transition and per-edge `max_iterations: 3`
- WHEN: The executor keeps returning Fixable
- THEN: The engine abandons after 3 iterations with a reason identifying the edge

## Implementation Tasks

### Files to Create

- `tests/engine_integration_llxprt_first.rs`
  - MUST include: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P16`
  - MUST include: `/// @requirement:REQ-LF-XXX` on every test
  - All tests construct workflows programmatically — no TOML loading
  - Uses real executors (ShellExecutor, NoOpExecutor) and real `EngineRunner::run()`

### Test Strategy

Tests create `WorkflowInstance` objects with in-code `WorkflowType` and `WorkflowConfig` structs, register real executors, and call `runner.run()`. They verify end-to-end integration of:
1. Config variables → context interpolation → shell command execution
2. Namespaced context → cross-step variable references
3. Per-edge loop limits → through real engine loop with transition resolution
4. VerifyExecutor registration → dispatch succeeds for `step_type = "verify"`

### Test List

1. **`test_config_variables_available_in_shell_steps`** (REQ-LF-PROF-003)
   - Create WorkflowConfig with `variables: {"my_var": "hello"}`
   - Create a shell step with command `echo {my_var}`
   - Run engine
   - Verify step completes successfully (command interpolated correctly)
   - Verify context has `stdout` containing `"hello"`

2. **`test_different_configs_resolve_different_profiles`** (REQ-LF-PROF-004)
   - Create the same WorkflowType with a step using `{profile_planning}` in params
   - Create two different WorkflowConfigs with different `profile_planning` values
   - Run engine with each config
   - Verify the interpolated commands differ

3. **`test_namespaced_context_across_real_steps`** (REQ-LF-CTX-001, REQ-LF-CTX-003)
   - Create a workflow: step_a (shell: `echo alpha`) → step_b (shell: `echo beta`) → step_c (shell: `echo {step_a.stdout}`)
   - Run engine
   - Verify step_c's context has stdout containing `"alpha"` (from step_a namespace)
   - Note: step_c's command references `{step_a.stdout}`, so the echo output should be "alpha"

4. **`test_unnamespaced_variable_gets_most_recent`** (REQ-LF-CTX-002)
   - Create a workflow: step_a (shell: `echo first`) → step_b (shell: `echo second`) → step_c (shell: `echo {stdout}`)
   - Run engine
   - Verify step_c's echo outputs `"second"` (most-recent bare `stdout` from step_b)

5. **`test_per_edge_loop_with_real_executor_dispatch`** (REQ-LF-LOOP-001, REQ-LF-LOOP-003)
   - Create a workflow with a loop: step_a → step_b → step_a (on fixable, `max_iterations: 2`)
   - Register step_b with an executor that always returns `Fixable`
   - Run engine
   - Verify `RunOutcome::Abandoned` with reason identifying the edge

6. **`test_independent_loops_through_engine`** (REQ-LF-LOOP-002)
   - Create a workflow with two independent loops, each with per-edge limits
   - Register executors that return Fixable a controlled number of times, then Success
   - Run engine
   - Verify `RunOutcome::Success` — both loops stay within their limits

7. **`test_verify_executor_dispatches_through_registry`** (REQ-LF-VERIFY-001)
   - Create a step with `step_type = "verify"` and valid params
   - Verify the engine dispatches to VerifyExecutor without `StepExecutionError`
   - Note: The check commands will likely fail (no real npm/node), producing a Fatal outcome — that's fine, we're testing dispatch, not check execution

8. **`test_config_variables_and_namespaced_context_combined`** (REQ-LF-PROF-003, REQ-LF-CTX-001)
   - Create WorkflowConfig with `variables: {"repo": "my-repo"}`
   - Create step_a (shell: `echo {repo}`) → step_b (shell: `echo {step_a.stdout}`)
   - Run engine
   - Verify step_b echoes `"my-repo"` (config var → step_a → namespaced ref in step_b)

9. **`test_builtin_variables_still_resolve`** (REQ-LF-CTX-004)
   - Create a shell step with command `echo {run_id}`
   - Run engine
   - Verify stdout is non-empty (contains the UUID run_id)

10. **`test_fatal_with_transition_routes_to_target_step`** (REQ-LF-FAIL-001)
    - Create a workflow: step_a → step_b → step_c, with an additional transition `step_b → abandon_step` on `fatal`
    - Register step_b with an executor that returns `StepOutcome::Fatal`
    - Register `abandon_step` with an executor that returns `StepOutcome::Success` (or a NoOp)
    - Run engine
    - Verify that `abandon_step` was executed (not skipped)
    - Verify `RunOutcome::Success` (the engine follows the fatal transition to the terminal step and completes)

11. **`test_fatal_without_transition_returns_failure`** (REQ-LF-FAIL-001, backward compat)
    - Create a workflow: step_a → step_b → step_c, with NO fatal transition from step_b
    - Register step_b with an executor that returns `StepOutcome::Fatal`
    - Run engine
    - Verify `RunOutcome::Failure` at step_b (fallback behavior when no fatal transition exists)

12. **`test_run_completion_records_metadata`** (REQ-LF-FAIL-005)
    - Create a workflow with a DB path (use `EngineRunner::with_db_path()`)
    - Set context variable `issue_number` = `"42"` via config variables
    - Run engine to success
    - Query the run metadata store for the run_id
    - Assert the record contains: outcome = "success", run_id matches, step reached is the last step, issue_number = "42"

13. **`test_run_abandonment_records_metadata`** (REQ-LF-FAIL-005)
    - Create a workflow that exceeds a loop limit → `RunOutcome::Abandoned`
    - Query the run metadata store after run completes
    - Assert the record contains: outcome = "abandoned", step_id of the step where abandonment occurred

14. **`test_set_work_dir_preserves_seeded_variables`** (REQ-LF-PROF-003)
    - Create EngineRunner with config variables `{"my_var": "preserved"}`
    - Call `runner.set_work_dir(new_path)` to change work directory
    - Create and run a shell step that echoes `{my_var}`
    - Assert stdout contains `"preserved"` — the config variable survived the work_dir change

### Required Code Markers

```rust
/// @plan:PLAN-20260408-LLXPRT-FIRST.P16
/// @requirement:REQ-LF-XXX
#[test]
fn test_name() { ... }
```

## Verification Commands

### Automated Checks

```bash
# Plan markers
grep -c "@plan:PLAN-20260408-LLXPRT-FIRST.P16" tests/engine_integration_llxprt_first.rs
# Expected: 14+

# Requirement markers
grep -c "@requirement:REQ-LF" tests/engine_integration_llxprt_first.rs
# Expected: 14+

# All integration tests pass
cargo test --test engine_integration_llxprt_first 2>&1 | grep "test result"
# Expected: 14 passed, 0 failed

# Full test suite
cargo test 2>&1 | grep "test result"
# Expected: 0 failures

# No reverse testing
grep "should_panic" tests/engine_integration_llxprt_first.rs
# Expected: no output

# No TOML loading (tests are programmatic)
grep "resolve_workflow_type\|load_workflow" tests/engine_integration_llxprt_first.rs
# Expected: no output (or only in comments)

# Clippy
cargo clippy -- -D warnings
```

### Structural Verification Checklist

- [ ] Test file created: `tests/engine_integration_llxprt_first.rs`
- [ ] All 14 tests have plan markers
- [ ] All tests have requirement markers
- [ ] Tests use programmatic workflow construction (not TOML)
- [ ] Tests use real executors (ShellExecutor for shell steps)
- [ ] Tests call `runner.run()` (full engine loop)
- [ ] No `#[should_panic]` tests

### Semantic Verification Checklist

1. **Do the tests verify behavior, not structure?**
   - [ ] Tests assert on `RunOutcome` variants and context values, not internal wiring
   - [ ] Tests exercise real shell commands that produce observable output
   - [ ] Tests would fail if config variable loading was removed
   - [ ] Tests would fail if namespaced context was broken

2. **Are tests behavioral per goodtests.md?**
   - [ ] No mock theater — executors are real, commands are real
   - [ ] No tautologies — assertions verify outputs from real processing
   - [ ] Tests exercise realistic scenarios (config → interpolation → execution → context)

## Success Criteria

- 14 integration tests pass
- All existing tests pass
- Tests verify component interaction through the engine
- No TOML file dependencies in tests
- Fatal-transition routing verified (both with-transition and fallback cases)
- Run completion metadata recording verified (success and abandonment cases)
- set_work_dir() context preservation verified

## Failure Recovery

If this phase fails:

1. Rollback: `rm tests/engine_integration_llxprt_first.rs`
2. Verify: `cargo test` still passes

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P16.md`
