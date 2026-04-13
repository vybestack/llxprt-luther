# Phase 12: Per-edge Loop Limits -- Stub

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P12`

## Prerequisites

- Required: Phase 11a (Namespaced Context Impl Verification) completed
- Verification: `grep -r "@plan:PLAN-20260408-LLXPRT-FIRST.P11" src/`
- Expected: All tests pass, namespaced context fully implemented

## Requirements Implemented (Expanded)

This stub phase creates the skeleton for the following requirements. No behavior is implemented yet.

### REQ-LF-LOOP-001: Optional max_iterations on TransitionDef

**Full Text**: The engine shall support an optional `max_iterations` field on `TransitionDef` and track and enforce loop counts independently for each transition edge that specifies one.
**Behavior**:
- GIVEN: Transition `evaluate_plan -> create_plan` with `max_iterations: 5`
- WHEN: That transition is taken 6 times
- THEN: On the 6th attempt, the engine returns `RunOutcome::Abandoned`
**Why This Matters**: The plan-evaluate loop (up to 5) and test-remediate loop (up to 5) need independent counters.

### REQ-LF-LOOP-002: Per-edge loop tracking

**Full Text**: The engine shall track loop counts per transition edge, keyed by `from:to` step pair, not as a single global counter.
**Behavior**:
- GIVEN: Two independent loops -- plan loop (limit 3) and test loop (limit 3)
- WHEN: Plan loop executes 2 times, then test loop executes 2 times (4 total backward transitions)
- THEN: Neither loop's limit is exceeded -- a global counter would say 4 >= 3 but per-edge tracking correctly shows each at 2
**Why This Matters**: A global counter would incorrectly sum iterations across independent loops.

### REQ-LF-LOOP-003: Abandoned with edge identification

**Full Text**: If a per-edge loop count exceeds its configured `max_iterations`, then the engine shall return `Abandoned` with a message identifying the exceeded edge.
**Behavior**:
- GIVEN: Edge `evaluate_plan:create_plan` with `max_iterations: 2`
- WHEN: Count reaches 3
- THEN: `RunOutcome::Abandoned` with reason containing edge identification (e.g., step names)

### REQ-LF-LOOP-004: Global fallback

**Full Text**: The global `max_iterations` in `GuardLimits` shall serve as a fallback for transition edges that do not specify their own `max_iterations`.
**Behavior**:
- GIVEN: Global `max_iterations: 5` and a backward transition without per-edge limit
- WHEN: That transition is taken 6 times
- THEN: Engine returns `Abandoned` using the global limit

### REQ-LF-LOOP-005: Checkpoint persistence of per-edge counts

**Full Text**: When a checkpoint is persisted, the engine shall include per-edge loop counts in the state snapshot so they survive resume.
**Behavior**:
- GIVEN: A run with edge counts `{"A:B": 2, "C:D": 1}`
- WHEN: Checkpoint is saved and loaded
- THEN: Edge counts are restored correctly
**Why This Matters**: Resume from checkpoint must restore loop state to prevent exceeding limits.

## Implementation Tasks

### Files to Modify

- `src/workflow/schema.rs`
  - Add `#[serde(default)] pub max_iterations: Option<u32>` to `TransitionDef` (from pseudocode Component 4, lines 001-006)
  - ADD marker: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P12`
  - ADD marker: `/// @requirement:REQ-LF-LOOP-001`

- `src/engine/transition.rs`
  - Add `#[serde(default)] pub max_iterations: Option<u32>` to local `TransitionDef` (from pseudocode Component 4, lines 007-012)
  - ADD marker: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P12`

- `src/persistence/checkpoint.rs`
  - Add `pub edge_loop_counts: HashMap<String, u32>` to `StateSnapshot` (from pseudocode Component 4, lines 013-019)
  - Update `StateSnapshot::default()` to initialize `edge_loop_counts: HashMap::new()`
  - ADD marker: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P12`
  - ADD marker: `/// @requirement:REQ-LF-LOOP-005`

- `src/engine/runner.rs`
  - Replace `loop_count: u32` with `edge_loop_counts: HashMap<String, u32>` on `EngineRunner` (from pseudocode Component 4, lines 020-030)
  - Update `EngineRunner::new()` to initialize `edge_loop_counts: HashMap::new()`
  - Update `EngineRunner::with_db_path()` similarly
  - Update `loop_count()` to return `self.edge_loop_counts.values().sum()` (backward compat accessor)
  - Update `create_checkpoint()` to include `edge_loop_counts` in `StateSnapshot`
  - Do NOT change `run()` method yet — the loop enforcement logic stays as-is for now
  - ADD marker: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P12`
  - ADD marker: `/// @requirement:REQ-LF-LOOP-002`

### Constraints

- `TransitionDef` field is `Option<u32>` with `#[serde(default)]` — backward compatible for deserialization
- `edge_loop_counts` on `StateSnapshot` is added alongside existing `loop_count` (not replacing it)
- The `run()` method is NOT changed yet — loop enforcement changes come in Phase 14
- Existing tests that construct `TransitionDef` or `StateSnapshot` directly will need the new field — fix minimally
- All existing tests must still pass after these structural changes

### Required Code Markers

```rust
/// @plan:PLAN-20260408-LLXPRT-FIRST.P12
/// @requirement:REQ-LF-LOOP-XXX
```

## Verification Commands

### Automated Checks

```bash
# Plan markers
grep -r "@plan:PLAN-20260408-LLXPRT-FIRST.P12" src/ | wc -l
# Expected: 4+ (one per modified file)

# New field on schema TransitionDef
grep "max_iterations" src/workflow/schema.rs
# Expected: found

# New field on transition TransitionDef
grep "max_iterations" src/engine/transition.rs
# Expected: found

# New field on StateSnapshot
grep "edge_loop_counts" src/persistence/checkpoint.rs
# Expected: found

# Runner uses edge_loop_counts
grep "edge_loop_counts" src/engine/runner.rs
# Expected: found

# Build passes
cargo build --all-targets

# All tests pass
cargo test

# No version duplication
find src -name "*schema_v2*" -o -name "*runner_v2*"
# Expected: no output
```

### Structural Verification Checklist

- [ ] `max_iterations: Option<u32>` added to `schema::TransitionDef` with `#[serde(default)]`
- [ ] `max_iterations: Option<u32>` added to `transition::TransitionDef` with `#[serde(default)]`
- [ ] `edge_loop_counts: HashMap<String, u32>` added to `StateSnapshot`
- [ ] `StateSnapshot::default()` initializes `edge_loop_counts` to empty HashMap
- [ ] `EngineRunner.loop_count` replaced with `edge_loop_counts`
- [ ] `EngineRunner::loop_count()` returns sum of edge counts
- [ ] `create_checkpoint()` includes edge_loop_counts in snapshot
- [ ] `run()` method is NOT changed (loop enforcement is deferred to Phase 14)
- [ ] All existing tests pass (with minimal fixups for new struct fields)

## Success Criteria

- `cargo build --all-targets` passes
- `cargo test` passes (all existing tests, with struct field additions where needed)
- New fields exist on all target structs
- No behavioral changes to loop enforcement yet

## Failure Recovery

If this phase fails:

1. Rollback: `git checkout -- src/workflow/schema.rs src/engine/transition.rs src/persistence/checkpoint.rs src/engine/runner.rs`
2. Verify: `cargo test` passes

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P12.md`
