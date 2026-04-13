# Phase 13: Per-edge Loop Limits -- TDD Tests

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P13`

## Prerequisites

- Required: Phase 12a (Stub Verification) completed
- Verification: `grep -r "@plan:PLAN-20260408-LLXPRT-FIRST.P12" src/`
- Expected files from previous phase: Modified `src/workflow/schema.rs`, `src/engine/transition.rs`, `src/persistence/checkpoint.rs`, `src/engine/runner.rs` with new fields

## Requirements Implemented (Expanded)

### REQ-LF-LOOP-001: Per-edge max_iterations on TransitionDef

**Full Text**: The engine shall support an optional `max_iterations` field on `TransitionDef` and track and enforce loop counts independently for each transition edge that specifies one.
**Behavior**:
- GIVEN: Transition `evaluate_plan → create_plan` with `max_iterations: 3`
- WHEN: That transition is taken 4 times
- THEN: On the 4th attempt, the engine returns `Abandoned`

### REQ-LF-LOOP-002: Per-edge tracking (not global)

**Full Text**: The engine shall track loop counts per transition edge, keyed by `from:to` step pair, not as a single global counter.
**Behavior**:
- GIVEN: Two independent loops — plan loop (limit 3) and test loop (limit 3)
- WHEN: Plan loop executes 2 times, then test loop executes 2 times (4 total)
- THEN: Neither loop's limit is exceeded (global counter would say 4 ≥ 3)

### REQ-LF-LOOP-003: Abandoned with edge identification

**Full Text**: If a per-edge loop count exceeds its configured `max_iterations`, then the engine shall return `Abandoned` with a message identifying the exceeded edge.
**Behavior**:
- GIVEN: Edge `evaluate:plan` with `max_iterations: 2`
- WHEN: Count reaches 3
- THEN: `RunOutcome::Abandoned` with reason containing `"evaluate:plan"` or equivalent

### REQ-LF-LOOP-004: Global fallback

**Full Text**: The global `max_iterations` in `GuardLimits` shall serve as a fallback for transition edges that do not specify their own `max_iterations`.
**Behavior**:
- GIVEN: Global `max_iterations: 5` and a transition without per-edge limit
- WHEN: That backward transition is taken 6 times
- THEN: Engine returns `Abandoned` using the global limit

### REQ-LF-LOOP-005: Checkpoint persistence of edge counts

**Full Text**: When a checkpoint is persisted, the engine shall include per-edge loop counts in the state snapshot so they survive resume.
**Behavior**:
- GIVEN: A run with edge counts `{"A:B": 2, "C:D": 1}`
- WHEN: Checkpoint is saved and loaded
- THEN: Edge counts are restored correctly

## Implementation Tasks

### Files to Create

- `tests/per_edge_loop_tests.rs`
  - MUST include: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P13`
  - MUST include: `/// @requirement:REQ-LF-LOOP-XXX` on every test
  - Tests expect REAL behavior — they will fail until Phase 14 implementation
  - NO `#[should_panic]` tests

### Test Strategy

Tests construct workflows programmatically with NoOpExecutor or a test executor that returns configurable outcomes. Multiple transitions include `max_iterations`. The runner is exercised through `run()`.

### Test List

1. **`test_per_edge_limit_abandons_when_exceeded`** (REQ-LF-LOOP-001, REQ-LF-LOOP-003)
   - Create workflow: A → B → A (on fixable) with `max_iterations: 2` on B→A transition
   - Executor B always returns `Fixable`
   - Run engine
   - Assert `RunOutcome::Abandoned` and reason mentions the edge

2. **`test_per_edge_limit_allows_iterations_within_limit`** (REQ-LF-LOOP-001)
   - Create workflow: A → B → A (on fixable, `max_iterations: 3`) → C (on success)
   - Executor B returns `Fixable` twice, then `Success`
   - Run engine
   - Assert `RunOutcome::Success` (2 loops within limit of 3)

3. **`test_independent_loops_tracked_separately`** (REQ-LF-LOOP-002)
   - Create workflow: A → B → A (fixable, limit 3) → C → D → C (fixable, limit 3) → E
   - B returns `Fixable` twice then `Success`, D returns `Fixable` twice then `Success`
   - Run engine
   - Assert `RunOutcome::Success` — neither loop exceeds 3 despite 4 total backward transitions

4. **`test_global_fallback_used_when_no_per_edge_limit`** (REQ-LF-LOOP-004)
   - Create workflow: A → B → A (fixable, NO per-edge limit) with global `max_iterations: 2`
   - Executor B always returns `Fixable`
   - Run engine
   - Assert `RunOutcome::Abandoned` after 2 iterations (using global fallback)

5. **`test_per_edge_limit_overrides_global`** (REQ-LF-LOOP-001, REQ-LF-LOOP-004)
   - Create workflow: A → B → A (fixable, per-edge limit 5), global `max_iterations: 2`
   - Executor B returns `Fixable` 3 times then `Success`
   - Run engine
   - Assert `RunOutcome::Success` — per-edge limit (5) overrides global (2)

6. **`test_edge_counts_survive_checkpoint_roundtrip`** (REQ-LF-LOOP-005)
   - Create `StateSnapshot` with `edge_loop_counts: {"A:B": 2, "C:D": 1}`
   - Save checkpoint, load checkpoint
   - Assert loaded snapshot has same `edge_loop_counts`

7. **`test_abandoned_reason_identifies_edge`** (REQ-LF-LOOP-003)
   - Create workflow with edge limit exceeded
   - Capture `RunOutcome::Abandoned { reason, .. }`
   - Assert `reason` contains identifying info (step names or edge key)

8. **`test_forward_transitions_not_counted`** (REQ-LF-LOOP-002, edge case)
   - Create workflow: A → B → C → D (all forward, all success)
   - All transitions have `max_iterations: 1`
   - Run engine
   - Assert `RunOutcome::Success` — forward transitions don't increment edge counters

9. **`test_loop_count_accessor_returns_sum_of_edge_counts`** (backward compat)
   - Exercise engine through 2 iterations of loop A → B → A
   - Assert `runner.loop_count()` returns 2

10. **`test_mixed_per_edge_and_global_limits`** (REQ-LF-LOOP-001, REQ-LF-LOOP-004)
    - Create workflow: loop1 A→B→A (per-edge limit 2), loop2 C→D→C (no per-edge, global=5)
    - B returns Fixable 3 times (exceeds limit 2)
    - Assert `RunOutcome::Abandoned` at the A→B→A loop, NOT at C→D→C

### Required Code Markers

```rust
/// @plan:PLAN-20260408-LLXPRT-FIRST.P13
/// @requirement:REQ-LF-LOOP-XXX
#[test]
fn test_name() { ... }
```

## Verification Commands

### Automated Checks

```bash
# Plan markers
grep -c "@plan:PLAN-20260408-LLXPRT-FIRST.P13" tests/per_edge_loop_tests.rs
# Expected: 10+

# Requirement coverage
grep -c "@requirement:REQ-LF-LOOP" tests/per_edge_loop_tests.rs
# Expected: 10+

# No reverse testing
grep "should_panic" tests/per_edge_loop_tests.rs
# Expected: no output

# Compile
cargo build --all-targets

# Tests fail (Red phase)
cargo test --test per_edge_loop_tests 2>&1 | grep "test result"
# Expected: failures > 0

# Existing tests pass
cargo test --test executor_unit_tests
cargo test --test engine_execution_integration
cargo test --test namespaced_context_tests
```

### Structural Verification Checklist

- [ ] Phase 12 markers present in source
- [ ] Test file created: `tests/per_edge_loop_tests.rs`
- [ ] All 10 tests have plan markers
- [ ] All tests have requirement markers
- [ ] No `#[should_panic]` tests
- [ ] Tests compile
- [ ] Tests fail with assertion errors (not compile errors)
- [ ] Existing tests unaffected

## Success Criteria

- 10 behavioral tests written
- All tests tagged with plan and requirement markers
- Tests fail naturally (assertion errors because per-edge enforcement not yet implemented)
- No reverse testing patterns
- Existing tests still pass

## Failure Recovery

If this phase fails:

1. Rollback: `rm tests/per_edge_loop_tests.rs`
2. Verify: `cargo test` still passes
3. Re-run Phase 13

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P13.md`
