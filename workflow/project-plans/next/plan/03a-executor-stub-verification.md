# Phase 03a: Executor Stub Verification

## Phase ID

`PLAN-20260408-STEP-EXEC.P03A`

## Prerequisites

- Required: Phase 03 completed
- Verification: `.completed/P03.md` exists

## Verification Checklist

### Structural

- [ ] `src/engine/executor.rs` exists with `@plan:PLAN-20260408-STEP-EXEC.P03` marker
- [ ] `src/engine/executors/mod.rs` exists with marker
- [ ] `src/engine/executors/shell.rs` exists with marker
- [ ] `src/engine/executors/write_file.rs` exists with marker
- [ ] `src/engine/mod.rs` has `pub mod executor;` and `pub mod executors;`

### Compilation

- [ ] `cargo build --all-targets` passes

### Backward Compatibility

- [ ] `cargo test` passes with all existing tests (118+)

### Type Shapes

- [ ] `StepExecutor` trait has `execute(&self, step: &StepDef, ctx: &mut StepContext) -> Result<StepOutcome, ExecutionError>`
- [ ] `StepContext` has fields: `run_id`, `workflow_type_id`, `config_id`, `work_dir`, `values`
- [ ] `ExecutorRegistry` has `register()` and `dispatch()` methods
- [ ] `ExecutionError` enum has at least: `ParameterMissing`, `IoError`, `SpawnError`

## Verdict Rules

- PASS: Compiles, all tests pass, correct type shapes
- FAIL: Compilation failure, test regression, or missing types

## Phase Completion Marker

Create: `project-plans/next/plan/.completed/P03A.md`
