# Phase 18: End-to-End Workflow Integration Test

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P18`

## Prerequisites

- Required: Phase 17a (Workflow TOML Verification) completed
- Verification: `test -f tests/fixtures/workflows/valid/llxprt-issue-fix-v1.toml`
- Expected: Workflow TOML and config files exist, parse successfully, all existing tests pass

## Purpose

Write end-to-end integration tests that load the actual workflow TOML and config files, create a mock workspace, and validate the complete workflow graph routes correctly. These tests verify the workflow definition is structurally sound and the engine can execute it with mock executors.

The tests do NOT call real `gh`, `llxprt`, `git`, or `npm` commands — they use mock executors that return configurable outcomes to exercise the workflow graph routing. The goal is to prove the TOML definition is correct and complete, not to test the actual tools.

## Requirements Implemented (Expanded)

### REQ-LF-ISSUE-001 through REQ-LF-ISSUE-004: Issue selection graph routing

**Behavior**:
- GIVEN: Workflow loaded from TOML
- WHEN: `select_issue` returns Success
- THEN: Engine transitions to `fetch_issue`
- WHEN: `select_issue` returns Fatal
- THEN: Engine transitions to `abandon_and_log`

### REQ-LF-FETCH-001 through REQ-LF-FETCH-003: Fetch issue routing

**Behavior**:
- GIVEN: Engine at `fetch_issue`
- WHEN: Step returns Success
- THEN: Engine transitions to `setup_workspace`
- WHEN: Step returns Fatal
- THEN: Engine transitions to `abandon_and_log`

### REQ-LF-WS-001 through REQ-LF-WS-003: Workspace setup routing

**Behavior**:
- GIVEN: Engine at `setup_workspace`
- WHEN: Step returns Success
- THEN: Engine transitions to `create_plan`

### REQ-LF-PLAN-001 through REQ-LF-PLAN-005: Planning loop routing

**Behavior**:
- GIVEN: Engine at `evaluate_plan`
- WHEN: Step returns Success (PLAN_APPROVED mapped via outcome_on_stdout)
- THEN: Engine transitions to `implement`
- WHEN: Step returns Fixable (PLAN_NEEDS_REVISION)
- THEN: Engine loops back to `create_plan`
- WHEN: Loop exceeds 5 iterations
- THEN: Engine returns Abandoned

### REQ-LF-IMPL-001 through REQ-LF-IMPL-003: Implementation routing

**Behavior**:
- GIVEN: Engine at `evaluate_impl`
- WHEN: Success → transitions to `run_tests`
- WHEN: Fixable → loops back to `implement`

### REQ-LF-TEST-001 through REQ-LF-TEST-003: Test/remediation loop routing

**Behavior**:
- GIVEN: Engine at `run_tests`
- WHEN: Success → transitions to `push_changes`
- WHEN: Fixable → transitions to `remediate`
- GIVEN: Engine at `remediate`, step returns Success
- THEN: Loops back to `run_tests`
- WHEN: Loop exceeds 5 iterations → Abandoned

### REQ-LF-PR-001 through REQ-LF-PR-004: PR submission routing

**Behavior**:
- GIVEN: Engine at `push_changes`
- WHEN: Success → `generate_pr_description` → `create_pr` → `log_completion`
- All three are separate steps with individual transitions

### REQ-LF-FAIL-001 through REQ-LF-FAIL-004: Failure routing

**Behavior**:
- GIVEN: Any step returns Fatal
- THEN: Engine routes to `abandon_and_log`
- GIVEN: Engine at `abandon_and_log`
- WHEN: Step completes
- THEN: Workflow terminates (no further transitions)

## Implementation Tasks

### Files to Create

- `tests/e2e_workflow_integration.rs`
  - MUST include: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P18`
  - MUST include: `/// @requirement:REQ-LF-XXX` on every test
  - Tests load TOML fixtures and exercise the engine with configurable mock executors

### Test Strategy

Tests use a `ConfigurableExecutor` — a test executor that can be configured per step to return specific `StepOutcome` values. The executor is registered for all step_types used in the workflow TOML (`"shell"`, `"verify"`). Tests load the workflow type and config from test fixtures, create `WorkflowInstance`, register the configurable executor, and call `runner.run()`.

#### ConfigurableExecutor Design

```rust
/// Test executor that returns configurable outcomes per step_id.
/// For steps not in the map, returns Success.
struct ConfigurableExecutor {
    outcomes: HashMap<String, Vec<StepOutcome>>,
    call_counts: RefCell<HashMap<String, usize>>,
}
```

Each step_id maps to a `Vec<StepOutcome>` — the executor returns outcomes in sequence for successive calls to the same step. This allows testing loops (first call returns Fixable, second returns Success).

### Test List

1. **`test_happy_path_all_steps_succeed`** (REQ-LF-ISSUE through REQ-LF-PR)
   - Load workflow TOML and config from fixtures
   - All steps return Success
   - Assert `RunOutcome::Success`
   - Assert engine visited all 14 steps in order (via event log or step execution count)

2. **`test_plan_loop_fixable_then_approved`** (REQ-LF-PLAN-003, REQ-LF-PLAN-004)
   - Load workflow from fixtures
   - `evaluate_plan` returns Fixable twice, then Success
   - Assert `RunOutcome::Success`
   - Assert `create_plan` was called 3 times total (initial + 2 loop-backs)

3. **`test_plan_loop_exceeds_limit_abandons`** (REQ-LF-PLAN-005)
   - Load workflow from fixtures
   - `evaluate_plan` always returns Fixable
   - Assert `RunOutcome::Abandoned`
   - Assert reason identifies the evaluate_plan→create_plan edge

4. **`test_test_remediation_loop_fixable_then_passes`** (REQ-LF-TEST-001, REQ-LF-TEST-002)
   - Load workflow from fixtures
   - `run_tests` returns Fixable twice, then Success
   - Assert `RunOutcome::Success`
   - Assert `remediate` was called 2 times

5. **`test_test_remediation_loop_exceeds_limit_abandons`** (REQ-LF-TEST-003)
   - Load workflow from fixtures
   - `run_tests` always returns Fixable
   - Assert `RunOutcome::Abandoned`
   - Assert reason identifies the remediate→run_tests edge

6. **`test_impl_evaluation_loop`** (REQ-LF-IMPL-002, REQ-LF-IMPL-003)
   - Load workflow from fixtures
   - `evaluate_impl` returns Fixable once, then Success
   - Assert `RunOutcome::Success`
   - Assert `implement` was called twice

7. **`test_fatal_at_select_issue_routes_to_abandon_and_log`** (REQ-LF-FAIL-001, REQ-LF-ISSUE-004)
   - Load workflow from fixtures
   - `select_issue` returns Fatal, `abandon_and_log` returns Success
   - Assert the engine transitions from `select_issue` to `abandon_and_log` via the fatal transition in the TOML
   - Assert `RunOutcome::Success` (the `abandon_and_log` step executed and the workflow reached a terminal state)
   - Verify `abandon_and_log` was actually called (via ConfigurableExecutor call counts)

8. **`test_fatal_at_any_step_routes_to_abandon_and_log`** (REQ-LF-FAIL-001)
   - For each of several key steps (fetch_issue, setup_workspace, implement, run_tests, push_changes):
     - Set that step to return Fatal, `abandon_and_log` returns Success, all others Success
     - Assert the engine routes through `abandon_and_log` (not immediate `RunOutcome::Failure`)
     - Assert `abandon_and_log` was executed in each case

9. **`test_workflow_type_loads_from_toml`** (REQ-LF-SEP-003)
   - Load `llxprt-issue-fix-v1.toml` from test fixtures via `resolve_workflow_type()`
   - Assert workflow_type_id is `"llxprt-issue-fix-v1"`
   - Assert 14 steps present
   - Assert transitions include per-edge `max_iterations` on loop-back edges
   - Assert specific step_ids exist: `select_issue`, `create_plan`, `evaluate_plan`, `run_tests`, `create_pr`, `abandon_and_log`

10. **`test_workflow_config_loads_from_toml`** (REQ-LF-PROF-002)
    - Load `llxprt-code.toml` from test fixtures via `resolve_workflow_config()`
    - Assert config_id is `"llxprt-code"`
    - Assert `variables` contains `profile_planning`, `profile_evaluating`, `target_repo`, `assignee`
    - Assert guard limits are set

11. **`test_workflow_graph_completeness`** (REQ-LF-FAIL-001)
    - Load workflow type from fixtures
    - For each non-terminal step, assert there exists at least one `fatal` → `abandon_and_log` transition
    - For each non-terminal step, assert there exists at least one non-fatal outgoing transition
    - Assert `abandon_and_log` and `log_completion` have no outgoing transitions (terminal)

12. **`test_config_variables_injected_into_context`** (REQ-LF-PROF-003)
    - Load config with `variables` section
    - Create EngineRunner from the loaded config
    - Run with all-success mock executors
    - Verify that profile variables were available during step execution
    - (Can be tested by having a mock executor that checks context for specific variable names)

13. **`test_run_completion_records_metadata_in_e2e`** (REQ-LF-FAIL-005)
    - Load workflow from fixtures, use `EngineRunner::with_db_path()` with a temp database
    - Run the happy path (all steps succeed)
    - Query the run metadata store for the run_id
    - Assert a completion record exists with outcome = "success" and the run_id

### Required Code Markers

```rust
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-XXX
#[test]
fn test_name() { ... }
```

## Verification Commands

### Automated Checks

```bash
# Plan markers
grep -c "@plan:PLAN-20260408-LLXPRT-FIRST.P18" tests/e2e_workflow_integration.rs
# Expected: 13+

# Requirement coverage
grep -c "@requirement:REQ-LF" tests/e2e_workflow_integration.rs
# Expected: 13+

# All E2E tests pass
cargo test --test e2e_workflow_integration 2>&1 | grep "test result"
# Expected: 13 passed, 0 failed

# Full test suite
cargo test 2>&1 | grep "test result"
# Expected: 0 failures

# Tests load from TOML fixtures (not programmatic construction)
grep "resolve_workflow_type\|resolve_workflow_config" tests/e2e_workflow_integration.rs
# Expected: found (tests load from TOML)

# No external tool invocations
grep "Command::new\|process::Command" tests/e2e_workflow_integration.rs
# Expected: no output (tests use mock executors, not real commands)

# Clippy
cargo clippy -- -D warnings
```

### Structural Verification Checklist

- [ ] Test file created: `tests/e2e_workflow_integration.rs`
- [ ] All 13 tests have plan markers
- [ ] All tests have requirement markers
- [ ] Tests load TOML from test fixtures (not hardcoded in code)
- [ ] Tests use ConfigurableExecutor (configurable outcomes per step)
- [ ] No real external commands (gh, git, llxprt, npm) invoked
- [ ] No `#[should_panic]` tests
- [ ] Tests compile and pass

### Semantic Verification Checklist

1. **Do the tests verify real workflow graph routing?**
   - [ ] Happy path visits all 15 steps in correct order
   - [ ] Plan loop iterates correct number of times before succeeding or abandoning
   - [ ] Test/remediation loop iterates correctly
   - [ ] Impl evaluation loop iterates correctly
   - [ ] Fatal at any step terminates correctly

2. **Are tests behavioral per goodtests.md?**
   - [ ] Tests assert on RunOutcome and step execution counts, not internal engine state
   - [ ] Tests would fail if a transition was missing from the TOML
   - [ ] Tests would fail if per-edge max_iterations was wrong
   - [ ] No tautologies — outcomes depend on real engine transition resolution

3. **Do tests cover the requirement groups?**
   - [ ] REQ-LF-ISSUE: select_issue routing (success and fatal)
   - [ ] REQ-LF-FETCH: fetch_issue routing
   - [ ] REQ-LF-WS: setup_workspace routing
   - [ ] REQ-LF-PLAN: Planning loop with bounded iterations
   - [ ] REQ-LF-IMPL: Implementation evaluation routing
   - [ ] REQ-LF-TEST: Test/remediation loop with bounded iterations
   - [ ] REQ-LF-PR: PR submission routing (push → description → create_pr → log)
   - [ ] REQ-LF-FAIL: Fatal routing to abandon_and_log
   - [ ] REQ-LF-PROF: Config variables and profile resolution
   - [ ] REQ-LF-SEP: TOML loads and parses correctly

## Success Criteria

- 13 behavioral E2E tests pass
- All tests load real TOML fixtures
- Complete workflow graph routing verified
- All requirement groups covered
- No external tool dependencies in tests

## Failure Recovery

If this phase fails:

1. Rollback: `rm tests/e2e_workflow_integration.rs`
2. Verify: `cargo test` still passes

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P18.md`
