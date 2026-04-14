# Phase 15: Engine Integration -- Stub

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P15`

## Prerequisites

- Required: Phase 14a (Per-edge Loop Impl Verification) completed
- Verification: `grep -r "@plan:PLAN-20260408-LLXPRT-FIRST.P14" src/`
- Expected: All per-edge loop tests pass, all existing tests pass

## Purpose

Wire the four new components (enhanced ShellExecutor, VerifyExecutor, namespaced context, per-edge loops) together through the engine. This phase makes the structural connections — ensuring all pieces compile together and the VerifyExecutor is registered in the default executor registry. It also fixes the engine's fatal outcome routing so that `StepOutcome::Fatal` flows through `resolve_next_step()` instead of early-returning `RunOutcome::Failure`, enabling `condition = "fatal"` transitions in the workflow definition to actually be followed (REQ-LF-FAIL-001).

It also adds a `variables` section to `WorkflowConfig` so that workflow instance configs can inject variables (profile mappings, repo config) into `StepContext` at run start.

## Requirements Implemented (Expanded)

### REQ-LF-SEP-001: No domain-specific code in engine

**Full Text**: The workflow engine shall contain no GitHub-specific, llxprt-specific, or Node/TypeScript-specific code. All domain operations shall be performed by executors dispatched via the registry.
**Behavior**:
- GIVEN: The engine source files (`src/engine/*.rs`)
- WHEN: Searched for domain-specific terms (`github`, `gh `, `llxprt`, `npm`, `tsc`, `vitest`)
- THEN: No matches found — all domain logic is in executor implementations or TOML config

### REQ-LF-PROF-003: Config variables loaded into context

**Full Text**: When a step references a profile variable, the engine shall resolve it through standard context variable interpolation — the workflow config values are loaded into context at run start.
**Behavior**:
- GIVEN: A workflow config with `[variables]` section containing `profile_planning = "opusthinking"`
- WHEN: A workflow run starts
- THEN: `{profile_planning}` resolves to `"opusthinking"` in any step's command interpolation

### REQ-LF-FAIL-001: Fatal outcome routes through transition table

**Full Text**: When a step returns `Fatal`, the engine shall check the transition table for a `condition = "fatal"` edge from that step. If found, follow it (e.g., to `abandon_and_log`). If not found, fall back to `RunOutcome::Failure`.
**Behavior**:
- GIVEN: A workflow with a transition `from = "some_step", to = "abandon_and_log", condition = "fatal"`
- WHEN: `some_step` returns `StepOutcome::Fatal`
- THEN: The engine checks the transition table for a matching fatal transition, finds it, and routes to `abandon_and_log` — executing that step before completing the run. When `abandon_and_log` completes (a terminal step with no outgoing transitions), the run ends with `RunOutcome::Success` (the workflow completed its defined path, including cleanup).
- GIVEN: A workflow where a step returns `StepOutcome::Fatal` and NO fatal transition is defined for that step
- WHEN: The engine processes the fatal outcome
- THEN: The engine falls back to `RunOutcome::Failure` immediately (backward-compatible)
**Why This Matters**: The current engine hardcodes `StepOutcome::Fatal → RunOutcome::Failure` in `runner.rs`, bypassing the transition table entirely. This means fatal transitions in TOML (e.g., `from = "fetch_issue", to = "abandon_and_log", condition = "fatal"`) would never be followed. The engine must check for a fatal transition first, and only fall back to immediate `RunOutcome::Failure` if no transition exists. This is a generic engine enhancement, not workflow-specific.

## Implementation Tasks

### Files to Modify

- `src/engine/executors/mod.rs`
  - Ensure `pub mod verify;` and `pub use verify::VerifyExecutor;` are present (should already be from P06)
  - ADD marker: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P15`

- `src/engine/executor.rs` — `ExecutorRegistry::with_defaults()`
  - Add `registry.register("verify", Box::new(crate::engine::executors::VerifyExecutor));`
  - This makes VerifyExecutor available to any workflow TOML that uses `step_type = "verify"`
  - ADD marker: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P15`

- `src/workflow/schema.rs` — `WorkflowConfig`
  - Add `#[serde(default)] pub variables: HashMap<String, String>` field to `WorkflowConfig`
  - Import `std::collections::HashMap` if not already present
  - This allows workflow config files to include a `[variables]` section
  - ADD marker: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P15`
  - ADD marker: `/// @requirement:REQ-LF-PROF-003`

- `src/engine/runner.rs` — `EngineRunner::new()` and `EngineRunner::with_db_path()`
  - After creating `StepContext`, load config variables into context:
    ```rust
    for (key, value) in &instance.config.variables {
        context.set(key, value);
    }
    ```
  - This seeds the context with profile mappings and other config values before any step runs
  - ADD marker: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P15`
  - ADD marker: `/// @requirement:REQ-LF-PROF-003`

- `src/engine/runner.rs` — `EngineRunner::run()` — Fatal outcome handling
  - **Current behavior**: The `run()` method contains a `match outcome` block that converts `StepOutcome::Fatal` directly into `RunOutcome::Failure`, bypassing the transition table. This means TOML `condition = "fatal"` transitions are dead code.
  - **Required change**: Remove the early-return for `StepOutcome::Fatal` from the match block. Instead, convert the outcome to a transition condition string (`"fatal"` for Fatal, `"success"` for Success, `"fixable"` for Fixable) and call `resolve_next_step()`. The engine checks the transition table for a matching edge from the current step:
    - **If a matching transition is found** (`Some(next_step_id)`): continue execution to that step. For example, Fatal with a `condition = "fatal"` edge routes to `abandon_and_log`. The target step runs normally; when IT completes and reaches a terminal state (no outgoing transition), the run ends with `RunOutcome::Success` (the workflow completed its defined path, including cleanup).
    - **If NO matching transition is found** (`None`): the behavior depends on the outcome:
      - `Fatal` with no fatal transition → `RunOutcome::Failure` (backward-compatible fallback)
      - `Success` with no success transition → `RunOutcome::Success` (terminal step reached, workflow complete)
      - `Fixable` with no fixable transition → `RunOutcome::Failure` (no recovery path defined)
  - This change is implemented in Phase 15 (not deferred to Phase 16) because it is structural engine wiring — it makes the transition table the single source of truth for all outcome routing, which the integration tests in Phase 16 depend on.
    ```
    // BEFORE (current):
    StepOutcome::Fatal => return Ok(RunOutcome::Failure { ... })
    
    // AFTER (required):
    // Remove the Fatal early-return from the match block entirely.
    // Convert outcome to condition string: Fatal→"fatal", Success→"success", Fixable→"fixable"
    // Call resolve_next_step(current_step_id, condition)
    // If resolve_next_step returns Some(next): continue execution to that step
    // If resolve_next_step returns None:
    //   Fatal → RunOutcome::Failure (no fatal transition defined)
    //   Success → RunOutcome::Success (terminal step, workflow complete)
    //   Fixable → RunOutcome::Failure (no recovery path defined)
    ```
  - ADD marker: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P15`
  - ADD marker: `/// @requirement:REQ-LF-FAIL-001`

### Work directory initialization from config variables (REQ-LF-WS-001)

- `src/engine/runner.rs` — `EngineRunner::new()` and `EngineRunner::with_db_path()`
  - After loading config variables into context, check if `work_dir` is set in the variables
  - If present, resolve it to an absolute path and set it on the StepContext via `set_work_dir()`
  - If the directory does not exist, **create it** (including parents) via `std::fs::create_dir_all()`
  - This is generic engine behavior: the config can optionally specify a working directory. If it does, the engine ensures it exists and uses it. If it doesn't, the engine uses its default work_dir (typically the current directory or a temp path).
  - The engine knows nothing about *why* the work_dir is set — it's just a config variable that happens to control where steps execute. The workflow's `setup_workspace` step (in the TOML) handles git clone/checkout within that directory.
  - Implementation:
    ```rust
    // After loading config variables into context:
    if let Some(work_dir) = instance.config.variables.get("work_dir") {
        let path = std::path::PathBuf::from(work_dir);
        std::fs::create_dir_all(&path)
            .map_err(|e| EngineError::InvalidState(
                format!("Failed to create work_dir '{}': {}", work_dir, e)
            ))?;
        context.set_work_dir(path);
    }
    ```
  - ADD marker: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P15`
  - ADD marker: `/// @requirement:REQ-LF-WS-001`

### Fix: set_work_dir() must preserve seeded context variables (REQ-LF-PROF-003)

- `src/engine/runner.rs` — `EngineRunner::set_work_dir()`
  - **Current bug**: `set_work_dir()` calls `StepContext::new(work_dir, run_id)` which creates a fresh context, dropping ALL previously seeded variables (config variables, any variables set by prior steps).
  - **Required fix**: Instead of recreating StepContext, add a `set_work_dir()` method to `StepContext` itself that only changes the `work_dir` field without touching `variables`, `namespaced_vars`, `step_order`, or `current_step_id`.
  - Implementation:
    ```rust
    // In StepContext:
    pub fn set_work_dir(&mut self, work_dir: PathBuf) {
        self.work_dir = work_dir;
    }
    
    // In EngineRunner::set_work_dir():
    pub fn set_work_dir(&mut self, work_dir: std::path::PathBuf) {
        self.context.set_work_dir(work_dir);
    }
    ```
  - This ensures config variables seeded in `EngineRunner::new()` survive work_dir changes.
  - ADD marker: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P15`
  - ADD marker: `/// @requirement:REQ-LF-PROF-003`

### Constraints

- No new test files in this phase — integration tests come in Phase 16
- No new behavior in executors — executor code is unchanged. The engine's `run()` loop is updated to route fatal outcomes through `resolve_next_step()` instead of early-returning (REQ-LF-FAIL-001).
- All existing tests must continue to pass (WorkflowConfig gains an optional field with `#[serde(default)]`)
- Existing config TOML files without `[variables]` must still deserialize successfully

### REQ-LF-FAIL-005: Run completion metadata recording

**Full Text**: When a run completes (success or abandonment), the engine shall record the outcome, run_id, issue number, and step reached in the run metadata store.
**Behavior**:
- GIVEN: A workflow run that reaches completion (either `RunOutcome::Success`, `RunOutcome::Failure`, or `RunOutcome::Abandoned`)
- WHEN: The `run()` method returns
- THEN: The engine writes a record to the run metadata store (SQLite) containing: the `RunOutcome` variant, `run_id`, the last `step_id` reached, and any relevant context variables (e.g., `issue_number` if set)
**Why This Matters**: Without this, there is no audit trail of what happened during a run. Operators need to know which issue was being worked, where it got to, and whether it succeeded or failed.

## Implementation Tasks (continued)

### Run Completion Metadata

- `src/engine/runner.rs` — `EngineRunner::run()`
  - Before each `return Ok(RunOutcome::...)` in `run()`, persist a completion record:
    - Call a new helper `record_run_completion(&self, outcome: &RunOutcome)` that:
      - Extracts `run_id` from `self.instance.run_id`
      - Extracts the last `step_id` from the current execution state
      - Reads `issue_number` from `self.context.get("issue_number")` (may be `None` if the run failed before issue selection)
      - Writes to the run metadata table via `save_run_metadata_with_conn()`
    - The run metadata store already exists in `src/persistence/sqlite.rs` — use the existing infrastructure
  - ADD marker: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P15`
  - ADD marker: `/// @requirement:REQ-LF-FAIL-005`

### Required Code Markers

```rust
/// @plan:PLAN-20260408-LLXPRT-FIRST.P15
/// @requirement:REQ-LF-SEP-001
/// @requirement:REQ-LF-PROF-003
/// @requirement:REQ-LF-FAIL-001
/// @requirement:REQ-LF-FAIL-005
```

## Verification Commands

### Automated Checks

```bash
# Plan markers
grep -r "@plan:PLAN-20260408-LLXPRT-FIRST.P15" src/ | wc -l
# Expected: 4+ (one per modified file)

# VerifyExecutor registered in defaults
grep "verify" src/engine/executor.rs | grep "register"
# Expected: registry.register("verify", ...)

# Variables field on WorkflowConfig
grep "variables" src/workflow/schema.rs
# Expected: found

# Config variables loaded into context in runner
grep "config.variables" src/engine/runner.rs
# Expected: found

# No domain-specific code in engine (REQ-LF-SEP-001)
grep -rni "github\|llxprt\|npm\|tsc\|vitest\|eslint\|prettier" src/engine/runner.rs src/engine/executor.rs
# Expected: no output

# Compile
cargo build --all-targets

# All existing tests pass
cargo test

# Clippy
cargo clippy -- -D warnings
```

### Structural Verification Checklist

- [ ] VerifyExecutor registered in `ExecutorRegistry::with_defaults()`
- [ ] `variables: HashMap<String, String>` added to `WorkflowConfig` with `#[serde(default)]`
- [ ] Config variables loaded into `StepContext` in `EngineRunner::new()`
- [ ] Config variables loaded into `StepContext` in `EngineRunner::with_db_path()`
- [ ] Existing config TOML files still deserialize (variables field is optional)
- [ ] No domain-specific terms in engine source
- [ ] Fatal early-return removed from `run()` — fatal outcomes flow through `resolve_next_step()`
- [ ] All existing tests pass

## Success Criteria

- `cargo build --all-targets` passes
- `cargo test` passes (all existing tests)
- VerifyExecutor accessible via `step_type = "verify"` in any workflow
- Config variables flow into StepContext at run start
- No domain-specific code in engine
- Fatal outcomes route through `resolve_next_step()` — no early-return for `StepOutcome::Fatal` in `run()`

## Failure Recovery

If this phase fails:

1. Rollback: `git checkout -- src/engine/executor.rs src/engine/executors/mod.rs src/workflow/schema.rs src/engine/runner.rs`
2. Verify: `cargo test` passes

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P15.md`
