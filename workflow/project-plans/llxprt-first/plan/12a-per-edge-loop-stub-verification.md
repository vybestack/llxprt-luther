# Phase 12a: Per-edge Loop Limits -- Stub Verification

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P12a`

## Prerequisites

- Required: Phase 12 completed

## Verification Commands

```bash
# Plan markers
grep -r "@plan:PLAN-20260408-LLXPRT-FIRST.P12" src/ | wc -l
# Expected: 4+

# New field on schema TransitionDef
grep "max_iterations" src/workflow/schema.rs
# Expected: Option<u32> with serde(default)

# New field on transition TransitionDef
grep "max_iterations" src/engine/transition.rs
# Expected: Option<u32> with serde(default)

# New field on StateSnapshot
grep "edge_loop_counts" src/persistence/checkpoint.rs
# Expected: HashMap<String, u32>

# Runner uses edge_loop_counts instead of loop_count
grep "edge_loop_counts" src/engine/runner.rs | wc -l
# Expected: 5+ (field, new(), with_db_path(), loop_count(), create_checkpoint())

# Existing loop_count accessor returns sum
grep -A3 "fn loop_count" src/engine/runner.rs
# Expected: self.edge_loop_counts.values().sum() or similar

# Backward compatible deserialization
grep "serde(default)" src/workflow/schema.rs | grep -c "max_iterations\|default"
# Expected: at least 1 (the serde default on the field)

# Build passes
cargo build --all-targets

# All tests pass
cargo test
```

### Semantic Verification

- [ ] `schema::TransitionDef.max_iterations` is `Option<u32>` with `#[serde(default)]`
- [ ] `transition::TransitionDef.max_iterations` is `Option<u32>` with `#[serde(default)]`
- [ ] `StateSnapshot.edge_loop_counts` is `HashMap<String, u32>`
- [ ] `StateSnapshot::default()` initializes `edge_loop_counts` to empty HashMap
- [ ] Existing TOML fixtures still deserialize correctly (no `max_iterations` → `None`)
- [ ] `EngineRunner.loop_count: u32` is replaced with `edge_loop_counts: HashMap<String, u32>`
- [ ] `run()` method loop enforcement is NOT changed (deferred to Phase 14)
- [ ] Tests constructing TransitionDef or StateSnapshot directly have been minimally updated

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P12a.md`
