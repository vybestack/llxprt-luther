# Phase 00a: Preflight Verification

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P00a`

## Purpose

Verify ALL assumptions before writing any code. Every dependency, type, call path, and test infrastructure assumption in this plan must be confirmed against the actual codebase.

## Dependency Verification

| Dependency | Verification Command | Expected Status |
|---|---|---|
| `serde_json` | `cargo tree -p serde_json` | Installed (used for JSON parsing in ShellExecutor) |
| `serde` | `cargo tree -p serde` | Installed (used for Deserialize on schema types) |
| `toml` | `cargo tree -p toml` | Installed (used for TOML config parsing) |
| `rusqlite` | `cargo tree -p rusqlite` | Installed (used for checkpoint persistence) |
| `tempfile` | `grep tempfile Cargo.toml` | In dev-dependencies (used in tests) |
| `thiserror` | `cargo tree -p thiserror` | Installed (used for EngineError) |
| `chrono` | `cargo tree -p chrono` | Installed (used for timestamps) |

No new dependencies are needed. All JSON parsing uses `serde_json` (already present). All TOML parsing uses `toml` (already present).

## Type/Interface Verification

| Type Name | Expected Location | Verification | Match? |
|---|---|---|---|
| `StepExecutor` trait | `src/engine/executor.rs` | `grep "pub trait StepExecutor" src/engine/executor.rs` | Verify |
| `StepContext` struct | `src/engine/executor.rs` | Has `variables: HashMap<String, String>`, `work_dir`, `run_id` | Verify |
| `interpolate_string` fn | `src/engine/executor.rs` | Takes `(template: &str, context: &StepContext) -> String` | Verify |
| `ExecutorRegistry` struct | `src/engine/executor.rs` | Has `register()` and `dispatch()`, `with_defaults()` | Verify |
| `ShellExecutor` struct | `src/engine/executors/shell.rs` | Implements `StepExecutor`, runs `sh -c`, captures stdout/stderr | Verify |
| `StepOutcome` enum | `src/engine/transition.rs` | Variants: Success, Retryable, Fatal, Fixable, Abandon | Verify |
| `TransitionDef` struct | `src/workflow/schema.rs` | Fields: `from`, `to`, `condition: Option<String>` — NO `max_iterations` yet | Verify |
| `EngineRunner` struct | `src/engine/runner.rs` | Has `loop_count: u32` (single global counter) | Verify |
| `StateSnapshot` struct | `src/persistence/checkpoint.rs` | Has `loop_count: u32` (single global) — NO per-edge counts yet | Verify |
| `GuardLimits` struct | `src/workflow/schema.rs` | Has `max_iterations: Option<u32>` (global fallback) | Verify |
| `EngineError` enum | `src/engine/runner.rs` | Has `LoopLimitExceeded` variant | Verify |
| `WorkflowType` struct | `src/workflow/schema.rs` | Has `steps: Vec<StepDef>`, `transitions: Vec<TransitionDef>` | Verify |

## Call Path Verification

| Function/Method | Expected Caller | Evidence |
|---|---|---|
| `ShellExecutor::execute()` | `ExecutorRegistry::dispatch("shell", ...)` | registry.register("shell", Box::new(ShellExecutor)) in `with_defaults()` |
| `ExecutorRegistry::dispatch()` | `EngineRunner::execute_step()` | `self.registry.dispatch(step_type, &mut self.context, params)` in runner.rs |
| `interpolate_string()` | `ShellExecutor::execute()` | Called on command_template before execution |
| `EngineRunner::run()` | Tests and CLI | Main execution loop, calls execute_step in a loop |
| `resolve_transition_schema()` | `EngineRunner::resolve_next_step()` | Used for next-step resolution in runner |
| `save_checkpoint_with_conn()` | `EngineRunner::run()` | Called after each step completion |

## Test Infrastructure Verification

| Component | Test File | Verification |
|---|---|---|
| ShellExecutor unit tests | `tests/executor_unit_tests.rs` | Exists, has 20+ tests |
| Engine integration tests | `tests/engine_execution_integration.rs` | Exists, tests routing and loop behavior |
| Hello-world E2E | `tests/hello_world_workflow_integration.rs` | Exists, runs real executors through engine |
| Persistence tests | `tests/persistence_integration.rs` | Exists, tests checkpoint save/load |
| Test fixture TOML workflows | `tests/fixtures/workflows/valid/*.toml` | hello-world-v1.toml, issue-fix-v1.toml exist |
| Test fixture configs | `tests/fixtures/workflow-configs/valid/*.toml` | hello-world-config.toml, profile-0.toml exist |

## Schema Compatibility Analysis

### TransitionDef — Adding `max_iterations`

Current definition in `src/workflow/schema.rs`:
```rust
pub struct TransitionDef {
    pub from: String,
    pub to: String,
    pub condition: Option<String>,
}
```

Adding `max_iterations: Option<u32>` with `#[serde(default)]` is backward-compatible:
- Existing TOML files without `max_iterations` will deserialize to `None`
- Existing tests constructing `TransitionDef` will need to add the field
- **Risk**: There are TWO `TransitionDef` structs — one in `schema.rs` and one in `transition.rs`. Only `schema.rs` version is used for deserialization. The `transition.rs` version is used for test helpers. Both must be updated.

### StepContext — Namespaced Storage

Current storage: `variables: HashMap<String, String>` (flat key-value).

New storage: same HashMap, but keys stored as `step_id.variable_name`. The `get()` method needs two modes:
1. Exact key lookup: `get("fetch_issue.issue_title")` → direct HashMap lookup
2. Unnamespaced lookup: `get("issue_title")` → search all keys matching `*.issue_title`, most-recent-first

**Risk**: The "most-recent-first" ordering requires tracking insertion order or step execution order. `HashMap` does not preserve order. Either use `IndexMap` (new dependency) or track step execution order separately. Recommendation: use a Vec of step_ids to track order, keep HashMap for storage.

### StateSnapshot — Per-edge Loop Counts

Current: `loop_count: u32`.

New: add `edge_loop_counts: HashMap<String, u32>` (keyed by `"from:to"` string). Retain `loop_count` for backward compat during checkpoint load (old checkpoints won't have edge counts).

**Risk**: SQLite schema stores `loop_count` as a single integer column. Per-edge counts should go in the `context` JSON blob, not a new column. This avoids schema migration.

## Blocking Issues Found

1. **Two TransitionDef structs**: `src/workflow/schema.rs` and `src/engine/transition.rs` both define `TransitionDef`. The schema version is used for deserialization. The transition module version is used by `resolve_transition()`. Both need `max_iterations` field added. Must verify which one is canonical.

2. **StepContext insertion order**: HashMap doesn't preserve insertion order. For unnamespaced lookups ("most recent step first"), need to track step execution order. Add a `step_order: Vec<String>` field to StepContext.

3. **Checkpoint schema evolution**: Existing checkpoints store `loop_count` as integer. New per-edge counts go in `context` JSON blob. `loop_count` field retained as sum/fallback for backward compatibility.

## Verification Gate

- [ ] All dependencies verified (no new crates needed)
- [ ] All types match expectations
- [ ] All call paths are possible
- [ ] Test infrastructure ready
- [ ] Schema compatibility confirmed for TransitionDef, StepContext, StateSnapshot
- [ ] Two-TransitionDef issue addressed in plan

IF ANY CHECKBOX IS UNCHECKED: STOP and update plan before proceeding.

## Verification Commands

```bash
# Verify all deps exist
cargo tree -p serde_json && cargo tree -p serde && cargo tree -p toml && cargo tree -p rusqlite

# Verify key types exist
grep "pub trait StepExecutor" src/engine/executor.rs
grep "pub struct StepContext" src/engine/executor.rs
grep "pub fn interpolate_string" src/engine/executor.rs
grep "pub struct ShellExecutor" src/engine/executors/shell.rs
grep "pub struct TransitionDef" src/workflow/schema.rs
grep "pub struct TransitionDef" src/engine/transition.rs
grep "pub struct EngineRunner" src/engine/runner.rs
grep "pub struct StateSnapshot" src/persistence/checkpoint.rs
grep "loop_count" src/engine/runner.rs
grep "loop_count" src/persistence/checkpoint.rs

# Verify everything compiles and tests pass
cargo build --all-targets
cargo test

# Count existing tests
cargo test 2>&1 | grep "test result"
```
