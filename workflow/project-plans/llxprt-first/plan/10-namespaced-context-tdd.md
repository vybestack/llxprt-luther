# Phase 10: Namespaced Context -- TDD Tests

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P10`

## Prerequisites

- Required: Phase 09a (Stub Verification) completed
- Verification: `grep -r "@plan:PLAN-20260408-LLXPRT-FIRST.P09" src/`
- Expected files from previous phase: Modified `src/engine/executor.rs` with `current_step_id` field and `set_current_step_id()` method

## Requirements Implemented (Expanded)

### REQ-LF-CTX-001: Namespaced variable references

**Full Text**: The StepContext shall support namespaced variable references in the form `{step_id.variable_name}`, resolving to the value set by the named step.
**Behavior**:
- GIVEN: Step `fetch_issue` set context variable `issue_title` = `"Fix bug"`
- WHEN: A later step interpolates `{fetch_issue.issue_title}`
- THEN: The result is `"Fix bug"`
**Why This Matters**: Cross-step variable references without overwrite collisions.

### REQ-LF-CTX-002: Unnamespaced references resolve most-recent-writer-first

**Full Text**: For unqualified `{variable_name}` lookup (no step prefix), the resolver shall search most-recent-writer-first across all step namespaces. It iterates `step_order` in reverse, checking each step's `namespaced_vars` for the key, and returns the first match. If no step namespace contains the key, it falls back to the `"config"` namespace in `namespaced_vars` (for config-seeded variables from `WorkflowConfig.variables`), then to flat `variables` (for built-ins like `work_dir`, `run_id`).
**Behavior**:
- GIVEN: Step `step_a` set `stdout` = `"first"`, then step `step_b` set `stdout` = `"second"`
- WHEN: Interpolating `{stdout}`
- THEN: The result is `"second"` (most-recent-writer-first: searches step_b's namespace first, finds `stdout` there)
- GIVEN: Config-seeded variable `target_repo` = `"my/repo"` (from `WorkflowConfig.variables`, loaded at run start into `namespaced_vars["config"]`) and no step has set `target_repo`
- WHEN: Interpolating `{target_repo}`
- THEN: The result is `"my/repo"` (no step namespace contains it, falls back to `"config"` namespace where config-seeded values live)
- GIVEN: Config-seeded variable `target_repo` = `"my/repo"` and step `setup` later set `target_repo` = `"other/repo"`
- WHEN: Interpolating `{target_repo}`
- THEN: The result is `"other/repo"` (most-recent-writer-first finds setup's namespace entry before reaching config namespace fallback)
- GIVEN: Config-seeded variable `target_repo` = `"my/repo"` loaded into `namespaced_vars["config"]`
- WHEN: Interpolating `{config.target_repo}`
- THEN: The result is `"my/repo"` (qualified lookup into the `"config"` namespace directly)
**Why This Matters**: Backward compatibility -- existing `{stdout}` references still work. Config-seeded variables (from `WorkflowConfig.variables`, loaded into `namespaced_vars["config"]` at run start by Phase 15) are available as bare names via the fallback chain and can be overridden by step outputs. They are also addressable as `{config.variable_name}` for explicit access.

### REQ-LF-CTX-003: Variables stored under current step_id

**Full Text**: When an executor sets a context variable during step execution, the engine shall store it namespaced under the current step_id.
**Behavior**:
- GIVEN: `set_current_step_id("fetch_issue")` called, then `set("issue_title", "Fix bug")`
- WHEN: `get("fetch_issue.issue_title")` is called
- THEN: Returns `Some("Fix bug")`
**Why This Matters**: Enables namespaced lookups.

### REQ-LF-CTX-004: Built-in and config-seeded variables without namespace

**Full Text**: Built-in variables (`work_dir`, `run_id`) and config-seeded variables (from `WorkflowConfig.variables`) shall remain resolvable without a namespace prefix. Config-seeded variables are loaded into `namespaced_vars["config"]` at run start (Phase 15) and can be overridden by step outputs — a step `set()` writes to the step's namespace in `namespaced_vars`, so a later step's value wins via most-recent-writer-first resolution before the config namespace fallback is reached.
**Behavior**:
- GIVEN: StepContext created with work_dir `/tmp/test` and run_id `"run-abc"`
- WHEN: Interpolating `{work_dir}` and `{run_id}`
- THEN: Both resolve to their values without needing a namespace prefix
- GIVEN: Config-seeded variable `target_repo` = `"owner/repo"` loaded at run start into `namespaced_vars["config"]`
- WHEN: Interpolating `{target_repo}` (no step has set `target_repo`)
- THEN: Resolves to `"owner/repo"` via the `"config"` namespace fallback
**Why This Matters**: Backward compatibility — existing step parameters referencing `{work_dir}` must continue to work. Config-seeded variables make `WorkflowConfig.variables` entries available as bare names (via the `"config"` namespace fallback) throughout all steps, and as `{config.variable_name}` for explicit access.

## Implementation Tasks

### Files to Create

- `tests/namespaced_context_tests.rs`
  - MUST include: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P10`
  - MUST include: `/// @requirement:REQ-LF-CTX-XXX` on every test
  - Tests expect REAL behavior — they will fail until Phase 11 implementation
  - NO `#[should_panic]` tests

### Test List

1. **`test_set_with_step_id_stores_namespaced_variable`** (REQ-LF-CTX-003)
   - Call `set_current_step_id("fetch_issue")`, then `set("issue_title", "Fix bug")`
   - Assert `get("fetch_issue.issue_title")` returns `Some("Fix bug")`

2. **`test_namespaced_get_returns_value_from_specific_step`** (REQ-LF-CTX-001)
   - Step `step_a` sets `result` = `"aaa"`, step `step_b` sets `result` = `"bbb"`
   - Assert `get("step_a.result")` = `"aaa"`, `get("step_b.result")` = `"bbb"`

3. **`test_unnamespaced_get_returns_most_recent_value`** (REQ-LF-CTX-002)
   - Step `step_a` sets `stdout` = `"first"`, step `step_b` sets `stdout` = `"second"`
   - Assert `get("stdout")` returns `"second"` (most-recent-writer-first: searches step_b's namespace first, finds `stdout` there)

4. **`test_interpolate_namespaced_placeholder`** (REQ-LF-CTX-001)
   - Step `fetch_issue` sets `issue_number` = `"42"`
   - Interpolate template `"Fixes #{fetch_issue.issue_number}"`
   - Assert result is `"Fixes #42"`

5. **`test_interpolate_unnamespaced_placeholder_still_works`** (REQ-LF-CTX-002)
   - Step `step_a` sets `greeting` = `"hello"`
   - Interpolate template `"{greeting} world"`
   - Assert result is `"hello world"`

6. **`test_interpolate_mixed_namespaced_and_unnamespaced`** (REQ-LF-CTX-001, REQ-LF-CTX-002)
   - Step `fetch_issue` sets `issue_number` = `"42"`, step `setup` sets `branch` = `"issue42"`
   - Interpolate `"branch {setup.branch} for issue {issue_number}"`
   - Assert result is `"branch issue42 for issue 42"`

7. **`test_builtin_work_dir_resolves_without_namespace`** (REQ-LF-CTX-004)
   - Create context with work_dir `/tmp/test`
   - Interpolate `"{work_dir}/output.txt"`
   - Assert result contains `/tmp/test/output.txt`

8. **`test_builtin_run_id_resolves_without_namespace`** (REQ-LF-CTX-004)
   - Create context with run_id `"run-abc"`
   - Interpolate `"Run: {run_id}"`
   - Assert result is `"Run: run-abc"`

9. **`test_set_without_step_id_stores_bare_key_only`** (REQ-LF-CTX-003, backward compat)
   - Do NOT call `set_current_step_id()`, call `set("foo", "bar")`
   - Assert `get("foo")` returns `"bar"`
   - Assert no namespaced key exists (no `"None.foo"` or similar)

10. **`test_namespaced_and_bare_keys_coexist`** (REQ-LF-CTX-001, REQ-LF-CTX-002)
    - Step `step_a` sets `val` = `"namespaced"`
    - Direct `set("other_key", "bare")` (without step_id, different key)
    - Assert `get("step_a.val")` = `"namespaced"` (explicit namespace)
    - Assert `get("val")` = `"namespaced"` (unnamespaced search finds step_a's namespace entry via most-recent-first)
    - Assert `get("other_key")` = `"bare"` (no step namespace set it, falls back to flat variables)

11. **`test_unknown_namespaced_key_returns_none`** (REQ-LF-CTX-001, edge case)
    - Assert `get("nonexistent_step.var")` returns `None`

12. **`test_interpolate_undefined_placeholder_left_as_is`** (backward compat)
    - Interpolate `"{undefined_var}"`
    - Assert result is `"{undefined_var}"` (unchanged)

13. **`test_config_seeded_variable_resolves_as_bare_name`** (REQ-LF-CTX-002, REQ-LF-CTX-004)
    - Insert `"target_repo"` = `"owner/repo"` directly into `namespaced_vars["config"]` (simulating config-seeded variable loaded at run start into the `"config"` namespace)
    - Assert `get("target_repo")` returns `"owner/repo"` (`"config"` namespace fallback)
    - Call `set_current_step_id("step_a")`, set some other variable `set("foo", "bar")`
    - Assert `get("target_repo")` still returns `"owner/repo"` (step_a's namespace doesn't contain it, falls back to `"config"` namespace)

14. **`test_step_output_overrides_config_seeded_variable`** (REQ-LF-CTX-002, REQ-LF-CTX-004)
    - Insert `"target_repo"` = `"owner/repo"` directly into `namespaced_vars["config"]` (simulating config-seeded variable)
    - Call `set_current_step_id("setup")`, then `set("target_repo", "other/repo")`
    - Assert `get("target_repo")` returns `"other/repo"` (most-recent-writer-first finds setup's namespace entry before reaching config namespace fallback)
    - Assert `get("setup.target_repo")` returns `"other/repo"` (explicit namespace)

15. **`test_config_variable_qualified_access`** (REQ-LF-CTX-001, REQ-LF-CTX-004)
    - Insert `"target_repo"` = `"owner/repo"` directly into `namespaced_vars["config"]` (simulating config-seeded variable)
    - Assert `get("config.target_repo")` returns `"owner/repo"` (qualified lookup into `"config"` namespace)



### Required Code Markers

```rust
/// @plan:PLAN-20260408-LLXPRT-FIRST.P10
/// @requirement:REQ-LF-CTX-XXX
#[test]
fn test_name() { ... }
```

## Verification Commands

### Automated Checks

```bash
# Plan markers
grep -c "@plan:PLAN-20260408-LLXPRT-FIRST.P10" tests/namespaced_context_tests.rs
# Expected: 15+

# Requirement coverage
grep -c "@requirement:REQ-LF-CTX" tests/namespaced_context_tests.rs
# Expected: 15+

# No reverse testing
grep "should_panic" tests/namespaced_context_tests.rs
# Expected: no output

# Compile
cargo build --all-targets

# Tests fail (Red phase)
cargo test --test namespaced_context_tests 2>&1 | grep "test result"
# Expected: failures > 0

# Existing tests pass
cargo test --test executor_unit_tests
cargo test --test shell_enhanced_tests
cargo test --test verify_executor_tests
```

### Structural Verification Checklist

- [ ] Phase 09 markers present in source
- [ ] Test file created: `tests/namespaced_context_tests.rs`
- [ ] All 15 tests have plan markers
- [ ] All tests have requirement markers
- [ ] No `#[should_panic]` tests
- [ ] Tests compile
- [ ] Tests fail with assertion errors (not compile errors)
- [ ] Existing tests unaffected

## Success Criteria

- 15 behavioral tests written
- All tests tagged with plan and requirement markers
- Tests fail naturally (assertion errors or panics from missing behavior)
- No reverse testing patterns
- Existing tests still pass

## Failure Recovery

If this phase fails:

1. Rollback: `rm tests/namespaced_context_tests.rs`
2. Verify: `cargo test` still passes
3. Re-run Phase 10

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P10.md`
