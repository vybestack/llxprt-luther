# Phase 09: Namespaced Context -- Stub

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P09`

## Prerequisites

- Required: Phase 08a (VerifyExecutor Impl Verification) completed
- Verification: `grep -r "@plan:PLAN-20260408-LLXPRT-FIRST.P08" src/`
- Expected: All tests pass, VerifyExecutor fully implemented

## Requirements Implemented (Expanded)

This stub phase creates the skeleton for the following requirements. No behavior is implemented yet.

### REQ-LF-CTX-001: Namespaced variable references

**Full Text**: The StepContext shall support namespaced variable references in the form `{step_id.variable_name}`, resolving to the value set by the named step.
**Behavior**:
- GIVEN: Step `fetch_issue` set context variable `issue_title` = `"Fix bug"`
- WHEN: A later step interpolates `{fetch_issue.issue_title}`
- THEN: The result is `"Fix bug"`
**Why This Matters**: Step 13 (`create_pr`) needs to reference `{fetch_issue.issue_number}` from step 1 without it being overwritten by intermediate steps.

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
**Why This Matters**: Backward compatibility with existing `{stdout}` references while also enabling deterministic resolution order. Config-seeded variables (from `WorkflowConfig.variables`, loaded into `namespaced_vars["config"]` at run start by Phase 15) are available as bare names via the fallback chain and can be overridden by step outputs. They are also addressable as `{config.variable_name}` for explicit access.

### REQ-LF-CTX-003: Executor sets variables namespaced under current step_id

**Full Text**: When an executor sets a context variable during step execution, the engine shall store it namespaced under the current step_id.
**Behavior**:
- GIVEN: `set_current_step_id("fetch_issue")` called, then `set("issue_title", "Fix bug")`
- WHEN: `get("fetch_issue.issue_title")` is called
- THEN: Returns `Some("Fix bug")`
**Why This Matters**: Enables step-scoped storage so each step's output variables are independently addressable.

### REQ-LF-CTX-004: Built-in and config-seeded variables remain accessible without namespace

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

### Files to Modify

- `src/engine/executor.rs`
  - Add `current_step_id: Option<String>` field to `StepContext` struct (from pseudocode Component 3, line 005)
  - Add `step_order: Vec<String>` field to `StepContext` — tracks the order of step execution for most-recent-first resolution (REQ-LF-CTX-002)
  - Add `namespaced_vars: HashMap<String, HashMap<String, String>>` field to `StepContext` — maps step_id → {key → value} for namespaced storage (REQ-LF-CTX-001, REQ-LF-CTX-003)
  - Add `set_current_step_id()` method stub to `StepContext` (from pseudocode Component 3, lines 012-014)
  - The `set()` and `get()` methods are NOT changed yet — just the new fields and setter are added
  - Update `StepContext::new()` to initialize `current_step_id: None`, `step_order: Vec::new()`, `namespaced_vars: HashMap::new()`
  - ADD marker: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P09`
  - ADD marker: `/// @requirement:REQ-LF-CTX-001,REQ-LF-CTX-002,REQ-LF-CTX-003,REQ-LF-CTX-004`

### Stub Specifications

1. **`current_step_id: Option<String>`** field on `StepContext`
   - Corresponds to pseudocode Component 3, line 005
   - Initialized to `None` in `new()`

2. **`step_order: Vec<String>`** field on `StepContext`
   - Tracks the chronological order of step_ids that have been executed
   - Used by `get()` and `interpolate_string()` to implement most-recent-writer-first resolution for unqualified (no step prefix) lookups: iterates `step_order` in reverse, checking each step's `namespaced_vars` for the key. If no step namespace contains the key, falls back to the `"config"` namespace in `namespaced_vars` (where config-seeded values live), then to flat `variables` (where built-ins like `work_dir`, `run_id` live).
   - Initialized to `Vec::new()` in `new()`

3. **`namespaced_vars: HashMap<String, HashMap<String, String>>`** field on `StepContext`
   - Maps step_id → {variable_name → value}
   - Used for both explicit `{step_id.variable_name}` lookups and for most-recent-first unnamespaced resolution
   - Initialized to `HashMap::new()` in `new()`

4. **`set_current_step_id(&mut self, step_id: &str)`**
   - Corresponds to pseudocode Component 3, lines 012-014
   - Sets `self.current_step_id = Some(step_id.to_string())`
   - Appends `step_id` to `self.step_order` if not already the last entry (avoids duplicates on re-entry)
   - This is a real implementation (trivial setter), not a `todo!()` — setting a field is not deferred logic

### Constraints

- Do NOT modify `set()` or `get()` methods yet
- Do NOT modify `interpolate_string()` yet
- Do NOT modify `src/engine/runner.rs` yet
- Do NOT modify any existing tests
- All existing tests must still pass

### Required Code Markers

```rust
/// @plan:PLAN-20260408-LLXPRT-FIRST.P09
/// @requirement:REQ-LF-CTX-001,REQ-LF-CTX-003,REQ-LF-CTX-004
```

## Verification Commands

### Automated Checks

```bash
# Plan markers
grep -r "@plan:PLAN-20260408-LLXPRT-FIRST.P09" src/ | wc -l
# Expected: 2+ (field + method)

# New fields exist
grep "current_step_id" src/engine/executor.rs
# Expected: found (field declaration + initialization + setter)

grep "step_order" src/engine/executor.rs
# Expected: found (field declaration + initialization)

grep "namespaced_vars" src/engine/executor.rs
# Expected: found (field declaration + initialization)

# New method exists
grep "fn set_current_step_id" src/engine/executor.rs
# Expected: found

# Build passes
cargo build --all-targets

# All tests pass
cargo test
```

### Structural Verification Checklist

- [ ] `current_step_id: Option<String>` field added to StepContext
- [ ] `step_order: Vec<String>` field added to StepContext
- [ ] `namespaced_vars: HashMap<String, HashMap<String, String>>` field added to StepContext
- [ ] `StepContext::new()` initializes `current_step_id: None`, `step_order: Vec::new()`, `namespaced_vars: HashMap::new()`
- [ ] `set_current_step_id()` method exists, sets the field, and appends to step_order
- [ ] `set()` method is unchanged
- [ ] `get()` method is unchanged
- [ ] `interpolate_string()` is unchanged
- [ ] All existing tests pass

## Success Criteria

- `cargo build --all-targets` passes
- `cargo test` passes (all existing tests)
- New field and method exist on StepContext
- No behavioral changes to existing code

## Failure Recovery

If this phase fails:

1. Rollback: `git checkout -- src/engine/executor.rs`
2. Verify: `cargo test` passes

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P09.md`
