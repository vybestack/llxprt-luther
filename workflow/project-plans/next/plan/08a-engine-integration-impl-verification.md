# Phase 08a: Final Verification — Engine Integration and Hello-World

## Phase ID

`PLAN-20260408-STEP-EXEC.P08A`

## Prerequisites

- Required: Phase 08 completed
- Verification: `.completed/P08.md` exists

## Verification Protocol

This is the FINAL verification. Be thorough.

### 1. Placeholder Detection (MANDATORY — run first)

```bash
# Check executor code
grep -rn "todo!\|unimplemented!" src/engine/executor.rs src/engine/executors/
# Expected: no matches

# Check engine runner
grep -rn "todo!\|unimplemented!" src/engine/runner.rs
# Expected: no matches (dagrs_runtime.rs is pre-existing, not in scope)

# Check for TODO/FIXME comments
grep -rn "// TODO\|// FIXME\|// HACK\|placeholder\|not yet" src/engine/executor.rs src/engine/executors/ src/engine/runner.rs
# Expected: no matches
```

**IF ANY MATCH: STOP. VERDICT IS FAIL.**

### 2. Build and Test

```bash
# Full build
cargo build --all-targets

# All tests
cargo test 2>&1 | grep "^test result"
# Expected: All suites pass, total 135+ tests, 0 failures

# Clippy
cargo clippy -- -D warnings
```

### 3. Semantic Verification

- [ ] `execute_step()` in runner.rs: actually dispatches through registry (read the code, not just check it compiles)
- [ ] `ShellExecutor::execute()`: actually calls `Command::new("sh")` (not a stub)
- [ ] `WriteFileExecutor::execute()`: actually calls `std::fs::write` (not a stub)
- [ ] `StepContext.values`: actually populated after step execution (trace the code)
- [ ] `interpolate_string()`: actually replaces `{key}` patterns (not a no-op)
- [ ] Hello-world integration test: actually runs `cargo init` and `cargo test` in a temp dir

### 4. No Fallback Path

- [ ] `EngineRunner::new()` requires `ExecutorRegistry` — no optional, no default without it
- [ ] No `Ok(StepOutcome::Success)` hardcoded in `execute_step()`
- [ ] All existing tests updated (in Phase 06) to supply a registry and still pass
- [ ] `grep -rn "Ok(StepOutcome::Success)" src/engine/runner.rs` — not present inside `execute_step`

### 5. Hello-World End-to-End

```bash
# Via integration test
cargo test --test hello_world_workflow_integration -- --nocapture
# Expected: tests pass, output shows actual cargo operations

# Via CLI (manual)
cargo run -- run --workflow-type hello-world-v1 --config hello-world-config
# Expected: "Workflow completed successfully!" with step-by-step output
```

### 6. Feature Actually Works

Trace one complete execution path:
1. CLI receives `run --workflow-type hello-world-v1`
2. Config loader resolves workflow type and config from fixtures
3. EngineRunner created with executor registry (shell + write_file)
4. `run()` loop begins at first step `init_project`
5. `execute_step("init_project")` → dispatches to ShellExecutor → runs `cargo init` → Success
6. Transition: init_project → write_test
7. `execute_step("write_test")` → dispatches to WriteFileExecutor → writes test file → Success
8. Transition: write_test → write_impl
9. `execute_step("write_impl")` → dispatches to WriteFileExecutor → writes lib.rs → Success
10. Transition: write_impl → run_tests
11. `execute_step("run_tests")` → dispatches to ShellExecutor → runs `cargo test` → Success (test passes)
12. Transition: run_tests → complete
13. `execute_step("complete")` → ShellExecutor → `echo done` → Success
14. No more transitions → `RunOutcome::Success`

## Requirement Coverage Summary

| Requirement | Test Evidence | Code Evidence |
|---|---|---|
| REQ-EXEC-001 | executor_unit_tests + hello_world integration | executor.rs dispatch() |
| REQ-EXEC-002 | executor_unit_tests unregistered type test | executor.rs dispatch() fallthrough |
| REQ-EXEC-003 | executor_unit_tests shell success test | executors/shell.rs |
| REQ-EXEC-004 | executor_unit_tests write_file test | executors/write_file.rs |
| REQ-EXEC-005 | hello_world integration context test | executor.rs StepContext |
| REQ-EXEC-006 | executor_unit_tests interpolation test | executor.rs interpolate_string() |
| REQ-EXEC-007 | hello_world_workflow_integration e2e test | all pieces connected |
| REQ-EXEC-008 | executor_unit_tests shell failure test | executors/shell.rs exit code handling |
| REQ-EXEC-009 | executor_unit_tests spawn failure test | executors/shell.rs spawn error handling |
| REQ-EXEC-010 | full cargo test (118+ existing pass, updated in P06) | EngineRunner::new() requires registry, tests use NoOpExecutor |

## Verdict Rules

- PASS: All placeholder detection clean, all tests pass (135+), semantic verification confirms real dispatch, hello-world runs end-to-end, backward compatibility preserved
- FAIL: Any placeholder found, any test failure, hollow implementation, or backward compatibility broken

**THERE IS NO CONDITIONAL PASS.**

## Phase Completion Marker

Create: `project-plans/next/plan/.completed/P08A.md`

Contents must include:
- Exact test output counts
- grep outputs proving no placeholders
- Statement confirming hello-world workflow executed successfully
- Statement confirming all 118+ pre-existing tests still pass
