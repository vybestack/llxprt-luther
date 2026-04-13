# Phase 0.5: Preflight Verification

## Phase ID

`PLAN-20260408-STEP-EXEC.P00A`

## Purpose

Verify ALL assumptions before writing any code.

## Dependency Verification

| Dependency | Verification | Status |
|---|---|---|
| std::process::Command | Rust stdlib — always available | PENDING |
| serde_json::Value | `cargo tree -p serde_json` | PENDING |
| tempfile (dev-dep) | `cargo tree -p tempfile` | PENDING |
| cargo binary | `which cargo` | PENDING |

No new crate dependencies are introduced by this plan.

## Type/Interface Verification

| Type Name | Expected Location | Expected Shape | Status |
|---|---|---|---|
| `StepDef` | `src/workflow/schema.rs` | `{ step_id, step_type, description, parameters: Option<serde_json::Value> }` | PENDING |
| `StepOutcome` | `src/engine/transition.rs` | enum with `Success, Retryable, Fatal, Fixable, Abandon` | PENDING |
| `WorkflowInstance` | `src/engine/instance.rs` | struct with `workflow_type, config, run_id, current_state` | PENDING |
| `EngineRunner` | `src/engine/runner.rs` | struct with `execute_step(&mut self, step_id: &str) -> Result<StepOutcome, EngineError>` | PENDING |
| `WorkflowConfig.runtime.max_retries` | `src/workflow/schema.rs` | `u32` field on `RuntimeConfig` | PENDING |
| `WorkflowConfig.guard_limits.max_iterations` | `src/workflow/schema.rs` | `Option<u32>` field on `GuardLimits` | PENDING |

## Call Path Verification

| Function | Expected Caller | Evidence Required |
|---|---|---|
| `EngineRunner::execute_step()` | `EngineRunner::run()` loop | grep shows call in runner.rs |
| `StepDef.parameters` | executor `execute()` method (new) | field exists on StepDef |
| `WorkflowInstance::create()` | `main.rs handle_run_command` and tests | grep shows usage |

## Test Infrastructure Verification

| Component | Verification | Status |
|---|---|---|
| Integration test pattern | `tests/*.rs` files exist and run | PENDING |
| tempfile crate in dev-deps | `grep tempfile Cargo.toml` | PENDING |
| rstest in dev-deps | `grep rstest Cargo.toml` | PENDING |
| Existing tests pass | `cargo test` exits 0 | PENDING |

## Blocking Issues Found

[To be filled during execution]

## Verification Gate

- [ ] All dependencies verified
- [ ] All types match expectations
- [ ] All call paths are possible
- [ ] Test infrastructure ready
- [ ] Existing tests pass (118 tests, 0 failures)

IF ANY CHECKBOX IS UNCHECKED: STOP and update plan before proceeding.
