# Phase 11: Namespaced Context -- Implementation

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P11`

## Prerequisites

- Required: Phase 10a (TDD Verification) completed
- Verification: `grep -r "@plan:PLAN-20260408-LLXPRT-FIRST.P10" tests/`
- Expected files: `tests/namespaced_context_tests.rs` with 15 failing tests

## Requirements Implemented (Expanded)

All REQ-LF-CTX-001 through REQ-LF-CTX-004. See Phase 10 for full expansion.

### REQ-LF-CTX-002: Unnamespaced references resolve most-recent-writer-first

For unqualified `{variable_name}` lookup (no step prefix), the resolver searches most-recent-writer-first across all step namespaces. It iterates `step_order` in reverse, checking each step's `namespaced_vars` for the key, and returns the first match. If no step namespace contains the key, it falls back to the `"config"` namespace in `namespaced_vars` (for config-seeded variables from `WorkflowConfig.variables`), then to flat `variables` (for built-ins like `work_dir`, `run_id`). This is the canonical algorithm — see `get()` pseudocode below.

## Implementation Tasks

### Files to Modify

- `src/engine/executor.rs`
  - Modify `StepContext::set()` (from pseudocode Component 3, lines 016-024)
    - If `current_step_id` is `Some(step_id)`: store in `namespaced_vars[step_id][key]`
    - Always store bare key `"key"` in `self.variables` (for backward compat with code that doesn't use namespaces)
  - Modify `StepContext::get()` (from pseudocode Component 3, lines 026-032)
    - If key contains `.` (e.g., `"fetch_issue.issue_title"`): split on first `.`, look up `namespaced_vars[step_id][variable_name]`
    - If key is bare (no `.`): search `self.step_order` in **reverse** (most-recent-first), checking each step's `namespaced_vars` entry for the key. Return the first match. If no step namespace match, fall back to `namespaced_vars["config"]` for config-seeded variables, then to `self.variables` for built-ins (`work_dir`, `run_id`) and pre-namespace-era bare keys.
  - Modify `interpolate_string()` (from pseudocode Component 3, lines 034-052)
    - Must handle both `{step_id.variable}` and `{variable}` placeholders
    - For `{step_id.variable}`: resolve via `namespaced_vars[step_id][variable]`
    - For `{variable}`: resolve via the most-recent-first search (same logic as `get()`)
    - Built-in variables (`work_dir`, `run_id`) and config-seeded variables (`WorkflowConfig.variables` entries in `namespaced_vars["config"]`) still resolve without namespace
  - ADD marker: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P11`

### Implementation Details (Pseudocode Reference)

#### `set()` method modification:

```
FUNCTION StepContext::set(key, value)
  IF self.current_step_id IS Some(step_id) THEN
    // Store in namespaced storage
    self.namespaced_vars
      .entry(step_id)
      .or_insert_with(HashMap::new)
      .insert(key, value)
  END IF
  // Always store bare key in variables HashMap (backward compat + pre-namespace-era bare keys)
  self.variables.insert(key, value)
END FUNCTION
```

#### `get()` method modification:

```
FUNCTION StepContext::get(key) -> Option<&String>
  // Case 1: Explicit namespace "step_id.variable"
  IF key CONTAINS '.' THEN
    (step_id, var_name) = key.split_at_first('.')
    RETURN self.namespaced_vars.get(step_id)?.get(var_name)
  END IF
  
  // Case 2: Unnamespaced — most-recent-first search across step namespaces
  FOR step_id IN self.step_order.iter().rev() DO
    IF let Some(vars) = self.namespaced_vars.get(step_id) THEN
      IF let Some(value) = vars.get(key) THEN
        RETURN Some(value)
      END IF
    END IF
  END FOR
  
  // Case 3: Fall back to "config" namespace (config-seeded vars from WorkflowConfig.variables)
  IF let Some(config_vars) = self.namespaced_vars.get("config") THEN
    IF let Some(value) = config_vars.get(key) THEN
      RETURN Some(value)
    END IF
  END IF
  
  // Case 4: Fall back to flat variables (built-ins like work_dir, run_id, pre-namespace bare keys)
  RETURN self.variables.get(key)
END FUNCTION
```

The key insight: **unqualified `{variable}` lookup (no step prefix) searches most-recent-writer-first across all step namespaces.** This is implemented as a reverse iteration over `step_order`, checking each step's `namespaced_vars` for the requested key. Example:
- `step_a` sets `foo = "alpha"`, `bar = "one"`
- `step_b` sets `foo = "beta"` (does NOT set `bar`)
- `get("foo")` → searches step_b first (most recent), finds `"beta"` → returns `"beta"` [OK]
- `get("bar")` → searches step_b first (no `bar`), then step_a (has `bar`) → returns `"one"` [OK]
- `get("step_a.foo")` → explicit namespace, returns `"alpha"` [OK]
- Config-seeded `target_repo = "my/repo"` (loaded at run start into `namespaced_vars["config"]`)
- `get("target_repo")` → searches step_b (no match), step_a (no match) → falls back to `"config"` namespace → returns `"my/repo"` [OK]
- `get("config.target_repo")` → explicit namespace lookup into `"config"` → returns `"my/repo"` [OK — qualified access]
- If `step_b` later sets `target_repo = "other/repo"`: `get("target_repo")` → searches step_b first → returns `"other/repo"` [OK — step output overrides config-seeded]


This properly implements REQ-LF-CTX-002's "most-recent-writer-first search across all step namespaces."

Note: `set()` dual-writes to both `namespaced_vars[step_id]` and flat `variables`. The flat `variables` store acts as a fallback for built-in variables (`work_dir`, `run_id`) and keys set before any step context is active. Config-seeded variables (from `WorkflowConfig.variables`) are loaded into `namespaced_vars["config"]` at run start by Phase 15 — they live in the `"config"` namespace, NOT in flat `variables`. Config-seeded variables are available as bare names (via the `"config"` namespace fallback in Case 3) and as `{config.variable_name}` (via explicit namespace lookup in Case 1). They can be overridden by step outputs — when a step calls `set()`, the value is written to `namespaced_vars[step_id]`, so the most-recent-writer-first search in Case 2 finds the step's value before reaching the `"config"` namespace fallback. For keys set during step execution, the namespace search in `get()` Case 2 always finds the value before reaching the config or flat fallbacks.

#### `interpolate_string()` modification:

```
FUNCTION interpolate_string(template, context) -> String
  result = template
  
  // Collect all resolvable keys: namespaced + unnamespaced
  all_placeholders = find_all_patterns_matching("{...}", template)
  
  // Sort by length descending to prevent partial replacement
  sort_descending_by_length(all_placeholders)
  
  FOR EACH placeholder IN all_placeholders DO
    key = strip_braces(placeholder)
    IF let Some(value) = context.get(key) THEN
      result = result.replace(placeholder, value)
    END IF
  END FOR
  
  RETURN result
END FUNCTION
```

The interpolation function delegates to `get()` which handles both namespaced and unnamespaced resolution. The regex/pattern matching extracts both `{step_id.variable}` and `{variable}` forms.

### Constraints

- Do NOT modify any test files
- Do NOT modify `src/engine/runner.rs` yet (that's Phase 15-16)
- All 14 tests from Phase 10 must pass
- All existing tests must still pass
- No `todo!()`, `unimplemented!()`, `println!()`, or `dbg!()` in final code

## Verification Commands

### Automated Checks

```bash
# All context tests pass
cargo test --test namespaced_context_tests || exit 1
# Expected: 15 passed, 0 failed

# Full test suite
cargo test || exit 1

# No test modifications
git diff tests/namespaced_context_tests.rs | head -5
# Expected: no output

# No debug code
grep -rn "println!\|dbg!\|todo!\|unimplemented!" src/engine/executor.rs
# Expected: No matches

# Plan markers
grep -c "@plan:PLAN-20260408-LLXPRT-FIRST.P11" src/engine/executor.rs
# Expected: 1+

# Clippy
cargo clippy -- -D warnings
```

### Deferred Implementation Detection

```bash
grep -rn "todo!\|unimplemented!" src/engine/executor.rs
# Expected: No matches

grep -rn "// TODO\|// FIXME\|placeholder\|not yet" src/engine/executor.rs
# Expected: No matches
```

### Semantic Verification Checklist

1. **Does the code DO what the requirements say?**
   - [ ] Namespaced storage: `set("key", "val")` with step_id stores in `namespaced_vars[step_id]["key"]`
   - [ ] Bare storage: `set("key", "val")` also stores in flat `variables` for backward compat
   - [ ] Namespaced lookup: `get("step_id.var")` splits on `.` and returns from `namespaced_vars`
   - [ ] Unnamespaced lookup: `get("var")` iterates `step_order` in **reverse** and searches `namespaced_vars` per step — most-recent-writer-first across all step namespaces (REQ-LF-CTX-002)
   - [ ] Built-ins: `{work_dir}` and `{run_id}` still resolve without namespace (fall back to flat `variables`)
   - [ ] Config-seeded variables: values from `WorkflowConfig.variables` (loaded into `namespaced_vars["config"]` at run start) resolve as bare names via `"config"` namespace fallback
   - [ ] Config-seeded override: step output overrides config-seeded variable — namespace search finds step value before reaching `"config"` namespace fallback

   - [ ] Interpolation: `{step_id.variable}` resolved in templates via `get()` delegation
   - [ ] `get("var")` when `step_a` sets it but `step_b` doesn't returns `step_a`'s value (not `None`)

2. **Is this REAL implementation, not placeholder?**
   - [ ] Deferred implementation detection passed
   - [ ] `set()` method stores in both `namespaced_vars` and `variables`
   - [ ] `get()` method implements reverse iteration over `step_order`

3. **Backward compatibility preserved?**
   - [ ] Existing tests pass unchanged
   - [ ] `set()` without `set_current_step_id()` stores bare key only (no namespace)
   - [ ] `get("foo")` without namespace falls back to flat `variables` when no step has set it

## Success Criteria

- All 14 namespaced context tests pass
- All existing tests pass
- No todo!() or debug code
- Clippy passes
- Plan and requirement markers present

## Failure Recovery

If this phase fails:

1. Rollback: `git checkout -- src/engine/executor.rs`
2. Verify: `cargo test --test executor_unit_tests` still passes
3. Re-run Phase 11

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P11.md`
