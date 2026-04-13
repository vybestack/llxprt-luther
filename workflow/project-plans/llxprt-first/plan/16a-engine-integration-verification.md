# Phase 16a: Engine Integration -- Verification

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P16a`

## Prerequisites

- Required: Phase 16 completed

## Verification Commands

```bash
# Plan markers
grep -c "@plan:PLAN-20260408-LLXPRT-FIRST.P16" tests/engine_integration_llxprt_first.rs
# Expected: 10+

# Requirement coverage
grep -c "@requirement:REQ-LF" tests/engine_integration_llxprt_first.rs
# Expected: 10+

# All integration tests pass
cargo test --test engine_integration_llxprt_first 2>&1 | grep "test result"
# Expected: 10 passed, 0 failed

# Full test suite
cargo test 2>&1 | grep "test result"
# Expected: 0 failures

# Clippy
cargo clippy -- -D warnings

# No domain-specific code in engine (re-verify after integration)
grep -rni "github\|llxprt\|npm\|tsc\|vitest" src/engine/runner.rs src/engine/executor.rs
# Expected: no output
```

### Semantic Verification

- [ ] Config variables flow: WorkflowConfig.variables → StepContext → interpolate_string → shell command
- [ ] Namespaced context flow: step_a sets stdout → stored as `step_a.stdout` → accessible by step_c via `{step_a.stdout}`
- [ ] Per-edge loop flow: transition with max_iterations → edge_loop_counts check → Abandoned on exceed
- [ ] VerifyExecutor dispatch: `step_type = "verify"` → ExecutorRegistry → VerifyExecutor.execute()
- [ ] Built-in variables: `{run_id}` and `{work_dir}` still resolve in all steps
- [ ] Fatal routing: Fatal outcome with `condition = "fatal"` transition → engine follows it to target step (e.g., `abandon_and_log`); Fatal outcome without fatal transition → fallback `RunOutcome::Failure`
- [ ] No existing test breakage from integration wiring

### Integration Quality Assessment

- [ ] Tests cover the integration of ALL four new components (shell enhancements, verify executor, namespaced context, per-edge loops)
- [ ] Tests cover component **interaction** (e.g., config vars → namespaced context → shell interpolation)
- [ ] Tests are purely behavioral — they assert observable outcomes, not internal state
- [ ] No tests require external tools (npm, gh, llxprt) — all use simple echo/cat commands

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P16a.md`
