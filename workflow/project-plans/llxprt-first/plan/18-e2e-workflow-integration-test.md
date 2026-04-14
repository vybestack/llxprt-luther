# Phase 18: End-to-End Workflow Integration Tests

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P18`

## Prerequisites

- Required: Phase 17a (Workflow TOML Verification) completed
- Verification: `test -f tests/fixtures/workflows/valid/llxprt-issue-fix-v1.toml`
- Expected: Workflow TOML and config files exist, parse successfully, all existing tests pass

## Purpose

Write two categories of end-to-end integration tests:

1. **Graph routing tests** — Load real TOML fixtures, use mock executors, verify the workflow graph routes correctly for all outcome combinations. These prove the TOML definition is structurally sound.

2. **Live integration tests** — Actually call `gh` against the real GitHub repo to verify that issue listing, issue fetching, and comment retrieval work with real data. These prove the shell commands in the TOML are correct and that the system can interact with GitHub. These tests are gated behind `#[ignore]` so they don't run in CI without explicit opt-in (`cargo test -- --ignored`).

The tests do NOT hardcode issue numbers, milestone names, or content — they assert structural properties (got results, fields present, files created) so they remain stable as the repo evolves.

## Requirements Implemented (Expanded)

### REQ-LF-ISSUE-001 through REQ-LF-ISSUE-004: Issue selection

**Graph routing behavior**:
- GIVEN: Workflow loaded from TOML
- WHEN: `select_issue` returns Success → engine transitions to `setup_workspace`
- WHEN: `select_issue` returns Fatal → engine transitions to `abandon_and_log`

**Live integration behavior**:
- GIVEN: The real vybestack/llxprt-code repo (read from workflow config, not hardcoded)
- WHEN: We run the `gh` command from select_issue's TOML against it
- THEN: We get valid JSON with at least one issue number and title

### REQ-LF-FETCH-001 through REQ-LF-FETCH-004: Fetch issue data

**Live integration behavior**:
- GIVEN: A known-valid issue number from the repo
- WHEN: We run the `gh issue view` command from the TOML
- THEN: We get JSON with title, body, comments, and url fields
- AND: The body is non-empty text, comments is an array

### REQ-LF-WS-001 through REQ-LF-WS-004: Workspace setup

**Live integration behavior**:
- GIVEN: A temp directory as work_dir
- WHEN: We run the workspace setup commands
- THEN: A `.git/` directory exists, a branch was created, `.luther/` directory exists

### All other routing requirements (PLAN, IMPL, TEST, PR, FAIL)

Same graph routing tests as before — mock executors return configurable outcomes to exercise every transition path.

## Implementation Tasks

### Files to Create

- `tests/e2e_workflow_integration.rs` — Graph routing tests (always run)
- `tests/live_workflow_integration.rs` — Real `gh`/`git` integration tests (`#[ignore]` by default)

### Test File 1: `tests/e2e_workflow_integration.rs` — Graph Routing

These tests use `ConfigurableExecutor` (mock) to exercise the workflow graph. They prove the TOML definition routes correctly.

#### ConfigurableExecutor Design

```rust
/// Test executor that returns configurable outcomes per step_id.
/// For steps not in the map, returns Success.
struct ConfigurableExecutor {
    outcomes: HashMap<String, Vec<StepOutcome>>,
    call_counts: RefCell<HashMap<String, usize>>,
}
```

#### Test List (13 tests)

1. **`test_happy_path_all_steps_succeed`** (REQ-LF-ISSUE through REQ-LF-PR)
   - All steps return Success
   - Assert `RunOutcome::Success`, all 14 steps visited in order

2. **`test_plan_loop_fixable_then_approved`** (REQ-LF-PLAN-003, REQ-LF-PLAN-004)
   - `evaluate_plan` returns Fixable twice, then Success
   - Assert `create_plan` called 3 times

3. **`test_plan_loop_exceeds_limit_abandons`** (REQ-LF-PLAN-005)
   - `evaluate_plan` always returns Fixable
   - Assert `RunOutcome::Abandoned`

4. **`test_test_remediation_loop_fixable_then_passes`** (REQ-LF-TEST-001, REQ-LF-TEST-002)
   - `run_tests` returns Fixable twice, then Success
   - Assert `remediate` called 2 times

5. **`test_test_remediation_loop_exceeds_limit_abandons`** (REQ-LF-TEST-003)
   - `run_tests` always returns Fixable
   - Assert `RunOutcome::Abandoned`

6. **`test_impl_evaluation_loop`** (REQ-LF-IMPL-002, REQ-LF-IMPL-003)
   - `evaluate_impl` returns Fixable once, then Success
   - Assert `implement` called twice

7. **`test_fatal_at_select_issue_routes_to_abandon_and_log`** (REQ-LF-FAIL-001, REQ-LF-ISSUE-004)
   - `select_issue` returns Fatal
   - Assert engine routes to `abandon_and_log`

8. **`test_fatal_at_any_step_routes_to_abandon_and_log`** (REQ-LF-FAIL-001)
   - For several key steps: set to Fatal, verify routing to `abandon_and_log`

9. **`test_workflow_type_loads_from_toml`** (REQ-LF-SEP-003)
   - Load TOML, assert 14 steps, transitions include per-edge limits

10. **`test_workflow_config_loads_from_toml`** (REQ-LF-PROF-002)
    - Load config, assert variables contain `profile_planning`, `profile_evaluating`, `target_repo`, `work_dir`

11. **`test_workflow_graph_completeness`** (REQ-LF-FAIL-001)
    - Every non-terminal step has a `fatal` → `abandon_and_log` transition

12. **`test_config_variables_injected_into_context`** (REQ-LF-PROF-003)
    - Mock executor checks context for profile variables during execution

13. **`test_run_completion_records_metadata`** (REQ-LF-FAIL-005)
    - Run happy path with temp DB, verify run metadata record exists

### Test File 2: `tests/live_workflow_integration.rs` — Real Integration

These tests call real external tools (`gh`, `git`). They are `#[ignore]` by default and require:
- `gh` CLI authenticated
- Network access to GitHub
- Run with: `cargo test --test live_workflow_integration -- --ignored`

**Critical design rule**: These tests read `target_repo` and all other repo-specific values from the TOML config fixture (`tests/fixtures/workflow-configs/valid/llxprt-code.toml`). Nothing is hardcoded in the test Rust source. If someone creates a different workflow config for a different repo, the same test structure should work.

#### Test List (6 tests)

1. **`test_can_list_issues_from_repo`** (REQ-LF-ISSUE-001, REQ-LF-ISSUE-002)
   - Load `target_repo` from the workflow config TOML fixture
   - Run: `gh issue list --repo {target_repo} --state open --json number,title --limit 5`
   - Assert: stdout parses as JSON array
   - Assert: at least 1 issue returned (the repo has 100+ open issues)
   - Assert: each entry has `number` (integer) and `title` (non-empty string)
   - Does NOT assert specific issue numbers or titles (those change)

2. **`test_can_list_milestones_from_repo`** (REQ-LF-ISSUE-001)
   - Load `target_repo` from config
   - Run: `gh api repos/{target_repo}/milestones --jq '.[].title'`
   - Assert: at least 1 milestone returned
   - Does NOT assert specific milestone names

3. **`test_can_fetch_issue_details`** (REQ-LF-FETCH-001, REQ-LF-FETCH-002)
   - Load `target_repo` from config
   - First get any valid issue number: `gh issue list --repo {target_repo} --state open --json number --limit 1`
   - Then fetch it: `gh issue view {number} --repo {target_repo} --json title,body,comments,url`
   - Assert: JSON has `title` (non-empty), `body` (string), `comments` (array), `url` (string containing "github.com")
   - Does NOT assert content of body or comments

4. **`test_fetch_writes_issue_files`** (REQ-LF-FETCH-002, REQ-LF-DATA-002)
   - Load `target_repo` from config
   - Create a temp directory as work_dir, create `.luther/` in it
   - Get any valid issue number, then run the full fetch_issue command from the TOML (with interpolation of `{issue_number}` and `{target_repo}`)
   - Assert: `.luther/issue.md` exists and is non-empty
   - Assert: `.luther/issue-raw.json` exists and is valid JSON
   - Assert: stdout JSON has `title` and `url` keys (for context_map)
   - Does NOT assert file content

5. **`test_workspace_setup_creates_clone`** (REQ-LF-WS-001, REQ-LF-WS-002, REQ-LF-WS-004)
   - Load `target_repo` from config
   - Create a temp directory, set `work_dir` to a subpath that doesn't exist yet
   - Run the setup_workspace shell commands (with `{work_dir}`, `{target_repo}`, `{base_branch}`, `{issue_number}` interpolated from config + a test issue number)
   - Assert: `{work_dir}/.git/` exists
   - Assert: `{work_dir}/.luther/` exists
   - Assert: current branch is `issue{issue_number}` (via `git -C {work_dir} branch --show-current`)
   - Cleanup: `rm -rf` the temp dir

6. **`test_workspace_setup_reuses_existing_clone`** (REQ-LF-WS-001)
   - Same as above but run the setup commands TWICE
   - Second run should succeed (fetch+reset path, not clone path)
   - Assert: `.git/` still exists, branch is correct

#### Key Design Constraints for Live Tests

- **All repo-specific values come from the TOML config** — `target_repo`, `base_branch`, `assignee`, etc. Tests load them from `tests/fixtures/workflow-configs/valid/llxprt-code.toml`. The test code itself contains NO repo URLs, org names, or profile names.
- **Assertions are structural, not content-specific** — never assert specific issue titles, body text, or comment content. Assert types, non-emptiness, field presence, valid JSON structure.
- **`#[ignore]` on all live tests** — they require network, `gh` auth, and may be slow. Run explicitly with `--ignored`.
- **Temp directories for all filesystem operations** — use `tempfile::TempDir` for workspace tests. Clean up always.
- **Shell commands extracted from TOML** — ideally, tests read the actual step commands from the loaded workflow type TOML and interpolate variables, rather than duplicating the commands in Rust. This ensures the tests validate the SAME commands that the workflow will actually run. If that's too complex, the commands can be written in the test but must match the TOML exactly (verified by the Phase 18a checklist).

### Required Code Markers

```rust
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-XXX
#[test]  // or #[ignore] for live tests
fn test_name() { ... }
```

## Verification Commands

### Automated Checks

```bash
# Plan markers — graph routing tests
grep -c "@plan:PLAN-20260408-LLXPRT-FIRST.P18" tests/e2e_workflow_integration.rs
# Expected: 13+

# Plan markers — live tests
grep -c "@plan:PLAN-20260408-LLXPRT-FIRST.P18" tests/live_workflow_integration.rs
# Expected: 6+

# Graph routing tests pass
cargo test --test e2e_workflow_integration 2>&1 | grep "test result"
# Expected: 13 passed, 0 failed

# Live tests are ignored by default
cargo test --test live_workflow_integration 2>&1 | grep "test result"
# Expected: 0 passed, 0 failed, 6 ignored

# Live tests pass when explicitly run (requires gh auth + network)
cargo test --test live_workflow_integration -- --ignored 2>&1 | grep "test result"
# Expected: 6 passed, 0 failed

# No hardcoded repo names in test source
grep -n "vybestack\|llxprt-code\|acoliver" tests/live_workflow_integration.rs
# Expected: no output (all values loaded from TOML config)

# No hardcoded profile names in test source
grep -n "opusthinking\|deepthinker\|typescriptexpert" tests/live_workflow_integration.rs
# Expected: no output

# Full test suite
cargo test 2>&1 | grep "test result"
# Expected: 0 failures

# Clippy
cargo clippy -- -D warnings
```

### Structural Verification Checklist

- [ ] `tests/e2e_workflow_integration.rs` created with 13 graph routing tests
- [ ] `tests/live_workflow_integration.rs` created with 6 live integration tests
- [ ] All live tests have `#[ignore]` attribute
- [ ] Live tests load repo-specific values from TOML config fixture, not hardcoded
- [ ] No hardcoded repo names, profile names, or org names in test Rust source
- [ ] Graph routing tests use ConfigurableExecutor (configurable outcomes per step)
- [ ] No `#[should_panic]` tests
- [ ] All non-ignored tests compile and pass

### Semantic Verification Checklist

1. **Graph routing tests verify real TOML-driven routing**
   - [ ] Happy path visits all 14 steps in correct order
   - [ ] All three loop paths (plan, impl, test) tested for success and abandon
   - [ ] Fatal at any step terminates correctly via transition table
   - [ ] Config variables flow into context

2. **Live tests verify real external tool interaction**
   - [ ] Can list issues from the actual repo
   - [ ] Can list milestones from the actual repo
   - [ ] Can fetch issue details with title, body, comments
   - [ ] fetch_issue command writes .luther/issue.md correctly
   - [ ] Workspace setup creates clone, branch, and .luther/ directory
   - [ ] Workspace setup works on second run (reuse path)

3. **No domain leakage in test infrastructure**
   - [ ] ConfigurableExecutor is generic (usable with any workflow)
   - [ ] Live test helpers read config from TOML (swappable for different projects)

## Success Criteria

- 13 graph routing tests pass (always)
- 6 live integration tests pass when run with `--ignored`
- Complete workflow graph routing verified through mock tests
- Real GitHub integration verified through live tests
- All repo-specific values come from config, not Rust source

## Failure Recovery

If this phase fails:

1. Rollback: `rm tests/e2e_workflow_integration.rs tests/live_workflow_integration.rs`
2. Verify: `cargo test` still passes

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P18.md`
