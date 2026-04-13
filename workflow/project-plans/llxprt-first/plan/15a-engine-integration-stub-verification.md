# Phase 15a: Engine Integration -- Stub Verification

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P15a`

## Prerequisites

- Required: Phase 15 completed

## Verification Commands

```bash
# Plan markers
grep -r "@plan:PLAN-20260408-LLXPRT-FIRST.P15" src/ | wc -l
# Expected: 4+

# VerifyExecutor in default registry
grep "verify" src/engine/executor.rs | grep "register"
# Expected: found

# Variables on WorkflowConfig
grep "pub variables" src/workflow/schema.rs
# Expected: found with HashMap<String, String>

# Config vars loaded into context
grep -A2 "config.variables" src/engine/runner.rs
# Expected: loop that calls context.set()

# No domain leakage
grep -rni "github\|llxprt\|npm\|tsc\|vitest\|eslint\|prettier" src/engine/runner.rs src/engine/executor.rs
# Expected: no output

# Compile
cargo build --all-targets

# All existing tests pass
cargo test

# Clippy
cargo clippy -- -D warnings

# Existing config files still load
cargo test --test config_binding_integration
```

### Semantic Verification

- [ ] `ExecutorRegistry::with_defaults()` returns a registry where `dispatch("verify", ...)` does not return `StepExecutionError`
- [ ] `WorkflowConfig` deserializes from existing TOML fixtures (no `[variables]` section) — field defaults to empty HashMap
- [ ] Config variables are loaded before any step executes — verified by reading `EngineRunner::new()` control flow
- [ ] No new public API surfaces introduced — all changes are internal wiring

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P15a.md`
