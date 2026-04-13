# Phase 14: Per-edge Loop Limits -- Implementation

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P14`

## Prerequisites

- Required: Phase 13a (TDD Verification) completed
- Verification: `grep -r "@plan:PLAN-20260408-LLXPRT-FIRST.P13" tests/`
- Expected files: `tests/per_edge_loop_tests.rs` with 10 failing tests

## Requirements Implemented (Expanded)

All REQ-LF-LOOP-001 through REQ-LF-LOOP-005. See Phase 13 for full expansion.

## Implementation Tasks

### Files to Modify

- `src/engine/runner.rs`
  - Modify `run()` method loop enforcement logic (from pseudocode Component 4, lines 038-093)
    - After resolving `next_step`, before transitioning:
    - Compute edge key: `format!("{}:{}", current_step_id, next_step_id)`
    - Find the matching `TransitionDef` for this from/outcome pair
    - Get per-edge limit: `transition_def.max_iterations.unwrap_or(self.max_loops)`
    - Only check for backward transitions (`is_loop_back()` returns true)
    - Get current count from `self.edge_loop_counts`
    - If count ≥ limit: return `RunOutcome::Abandoned` with reason identifying the edge
    - Else: increment `self.edge_loop_counts[edge_key]`
    - Remove the old `self.loop_count += 1` logic
  - Add `find_transition()` helper method (from pseudocode Component 4, lines 112-122)
    - Looks up the `TransitionDef` matching from step and outcome condition
    - Returns `Option<&schema::TransitionDef>` to access `max_iterations`
  - Modify `set_current_step_id` call in `run()` loop (from pseudocode Component 4, line 048)
    - Before `execute_step()`, call `self.context.set_current_step_id(&current_step_id)` to enable namespaced context storage
  - Update checkpoint resume logic to load `edge_loop_counts` from snapshot
  - ADD marker: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P14`

- `src/persistence/checkpoint.rs`
  - Modify `save_checkpoint_with_conn()` to serialize `edge_loop_counts` into the `context` JSON blob (from pseudocode Component 4, lines 095-104)
  - Modify `load_checkpoint_with_conn()` to deserialize `edge_loop_counts` from the `context` JSON blob
  - The `edge_loop_counts` are stored as a JSON key `"__edge_loop_counts"` inside the context blob to avoid a schema migration
  - ADD marker: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P14`

### Implementation Details (Pseudocode Reference)

#### `run()` method loop enforcement (Component 4, lines 064-092):

```
// After resolving next_step:
MATCH next_step
  Some(next_step_id) =>
    edge_key = format!("{}:{}", current_step_id, next_step_id)
    
    // Find the transition def to check max_iterations
    transition_def = find_transition(current_step_id, outcome, transitions)
    edge_limit = transition_def
      .and_then(|t| t.max_iterations)
      .unwrap_or(self.max_loops)
    
    // Only count backward edges
    IF self.is_loop_back(current_step_id, next_step_id) THEN
      current_count = self.edge_loop_counts.get(edge_key).copied().unwrap_or(0)
      IF current_count >= edge_limit THEN
        RETURN Ok(RunOutcome::Abandoned {
          step_id: current_step_id,
          reason: format!("Per-edge loop limit ({}) exceeded on edge {}", edge_limit, edge_key)
        })
      END IF
      self.edge_loop_counts.insert(edge_key, current_count + 1)
    END IF
    
    current_step_id = next_step_id
    ...
```

#### `find_transition()` helper (Component 4, lines 112-122):

```
FUNCTION find_transition(from, outcome, transitions) -> Option<&TransitionDef>
  outcome_str = outcome.to_string()
  FOR EACH t IN transitions DO
    IF t.from == from THEN
      IF t.condition == Some(outcome_str) OR (t.condition IS None AND outcome == Success) THEN
        RETURN Some(t)
      END IF
    END IF
  END FOR
  RETURN None
END FUNCTION
```

#### Checkpoint persistence:

```
// In save_checkpoint_with_conn:
// Store edge_loop_counts in context JSON blob under reserved key
context_data["__edge_loop_counts"] = serde_json::to_value(edge_loop_counts)

// In load_checkpoint_with_conn:
// Extract edge_loop_counts from context JSON blob
edge_loop_counts = context_data.get("__edge_loop_counts")
  .and_then(|v| serde_json::from_value(v))
  .unwrap_or_default()
```

### Constraints

- Do NOT modify any test files
- All 10 tests from Phase 13 must pass
- All existing tests must still pass (including engine_execution_integration)
- No `todo!()`, `unimplemented!()`, `println!()`, or `dbg!()` in final code
- Existing `is_loop_back()` method can be kept and reused

## Verification Commands

### Automated Checks

```bash
# All per-edge loop tests pass
cargo test --test per_edge_loop_tests || exit 1
# Expected: 10 passed, 0 failed

# Full test suite
cargo test || exit 1

# No test modifications
git diff tests/per_edge_loop_tests.rs | head -5
# Expected: no output

# No debug code
grep -rn "println!\|dbg!\|todo!\|unimplemented!" src/engine/runner.rs
# Expected: No matches

grep -rn "println!\|dbg!\|todo!\|unimplemented!" src/persistence/checkpoint.rs
# Expected: No matches

# Plan markers
grep -c "@plan:PLAN-20260408-LLXPRT-FIRST.P14" src/engine/runner.rs
# Expected: 1+

grep -c "@plan:PLAN-20260408-LLXPRT-FIRST.P14" src/persistence/checkpoint.rs
# Expected: 1+

# Clippy
cargo clippy -- -D warnings
```

### Deferred Implementation Detection

```bash
grep -rn "todo!\|unimplemented!" src/engine/runner.rs src/persistence/checkpoint.rs
# Expected: No matches

grep -rn "// TODO\|// FIXME\|placeholder\|not yet" src/engine/runner.rs src/persistence/checkpoint.rs
# Expected: No matches
```

### Semantic Verification Checklist

1. **Does the code DO what the requirements say?**
   - [ ] Per-edge tracking: each backward transition tracked independently by `from:to` key
   - [ ] Per-edge limit: `TransitionDef.max_iterations` checked against per-edge count
   - [ ] Global fallback: edges without `max_iterations` use `GuardLimits.max_iterations`
   - [ ] Abandoned with identification: reason message includes edge key or step names
   - [ ] Forward transitions: not counted (only backward transitions per `is_loop_back()`)
   - [ ] Checkpoint persistence: `edge_loop_counts` serialized into context JSON blob
   - [ ] Checkpoint resume: `edge_loop_counts` deserialized from context JSON blob

2. **Is this REAL implementation, not placeholder?**
   - [ ] Deferred implementation detection passed
   - [ ] `run()` method actually checks `edge_loop_counts` against limit

3. **Backward compatibility preserved?**
   - [ ] `loop_count()` accessor still returns sum of edge counts
   - [ ] Existing engine integration tests pass
   - [ ] Old checkpoints without `__edge_loop_counts` in context load correctly (empty map)

## Success Criteria

- All 10 per-edge loop tests pass
- All existing tests pass
- No todo!() or debug code
- Clippy passes
- Plan and requirement markers present

## Failure Recovery

If this phase fails:

1. Rollback: `git checkout -- src/engine/runner.rs src/persistence/checkpoint.rs`
2. Verify: `cargo test --test engine_execution_integration` still passes
3. Re-run Phase 14

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P14.md`
