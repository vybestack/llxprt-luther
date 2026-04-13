# Plan: Step Execution and Hello-World Workflow

Plan ID: PLAN-20260408-STEP-EXEC
Generated: 2026-04-08
Total Phases: 8 (plus preflight and verification phases)
Requirements: REQ-EXEC-001 through REQ-EXEC-010

## Critical Reminders

Before implementing ANY phase, ensure you have:

1. Completed preflight verification (Phase 0.5)
2. Read the specification at `project-plans/next/specification.md`
3. Written integration tests BEFORE unit tests
4. Verified all dependencies and types exist as assumed
5. Confirmed all 118 existing tests still pass

## Directory Layout

```text
project-plans/next/
  overview.md
  specification.md
  requirements-ears.md
  execution-tracker.md
  analysis/
    domain-model.md
    pseudocode/
      executor-dispatch.md
      shell-executor.md
      write-file-executor.md
      context-interpolation.md
  plan/
    00-overview.md
    00a-preflight-verification.md
    01-analysis.md
    01a-analysis-verification.md
    02-pseudocode.md
    02a-pseudocode-verification.md
    03-executor-stub.md
    03a-executor-stub-verification.md
    04-executor-tdd.md
    04a-executor-tdd-verification.md
    05-executor-impl.md
    05a-executor-impl-verification.md
    06-engine-integration-stub.md
    06a-engine-integration-stub-verification.md
    07-engine-integration-tdd.md
    07a-engine-integration-tdd-verification.md
    08-engine-integration-impl.md
    08a-engine-integration-impl-verification.md
    .completed/
```

## Execution Order

`00a → 01 → 01a → 02 → 02a → 03 → 03a → 04 → 04a → 05 → 05a → 06 → 06a → 07 → 07a → 08 → 08a`

## Phase Index

| Phase | File | Purpose |
|---|---|---|
| 00a | `00a-preflight-verification.md` | Verify assumptions, types, dependencies |
| 01 | `01-analysis.md` | Domain analysis — executor boundaries, existing engine touch points |
| 01a | `01a-analysis-verification.md` | Verify analysis artifacts |
| 02 | `02-pseudocode.md` | Pseudocode for executor trait, registry, shell/write_file executors, context interpolation |
| 02a | `02a-pseudocode-verification.md` | Verify pseudocode covers all requirements |
| 03 | `03-executor-stub.md` | Create `StepExecutor` trait, `ExecutorRegistry`, `StepContext`, `ExecutionError` — compiling stubs |
| 03a | `03a-executor-stub-verification.md` | Verify stubs compile, no tests broken |
| 04 | `04-executor-tdd.md` | Behavioral TDD — shell executor, write_file executor, registry dispatch, context interpolation |
| 04a | `04a-executor-tdd-verification.md` | Verify TDD tests compile but fail (red phase) |
| 05 | `05-executor-impl.md` | Implement executors to make TDD tests pass |
| 05a | `05a-executor-impl-verification.md` | Verify all executor tests pass, no placeholders |
| 06 | `06-engine-integration-stub.md` | Wire `ExecutorRegistry` into `EngineRunner`, add `StepContext` to instance — existing tests still pass |
| 06a | `06a-engine-integration-stub-verification.md` | Verify wiring compiles, all 118 existing tests pass |
| 07 | `07-engine-integration-tdd.md` | Behavioral TDD — engine dispatches to executors, hello-world workflow e2e test |
| 07a | `07a-engine-integration-tdd-verification.md` | Verify integration tests compile but fail (red phase) |
| 08 | `08-engine-integration-impl.md` | Complete engine dispatch, hello-world fixtures, make all tests pass |
| 08a | `08a-engine-integration-impl-verification.md` | Final verification — all tests pass, hello-world workflow runs, no placeholders |

## Risk Register

| Risk | Mitigation | Trigger |
|---|---|---|
| Existing tests break when execute_step changes | Phase 06 updates all existing callers to pass `ExecutorRegistry` with `NoOpExecutor`; no fallback path | Test failures after engine modification |
| Shell executor non-determinism | Use temp dirs, deterministic commands, `cargo test` with known-good inputs | Flaky hello-world integration test |
| Parameter interpolation edge cases | Simple `{key}` replacement only; documented constraints; tested with known keys | Unexpected interpolation results |
| Cargo not available in CI | Preflight verifies `cargo` availability; test marked with appropriate guards if needed | Shell executor can't find `cargo` |

## Rollback Policy

- If a phase fails verification, do not proceed.
- Revert only files touched by the failed phase.
- Preserve planning/evidence artifacts and re-run the failed phase.
- Resume only after PASS is recorded in `.completed/PXXA.md`.
