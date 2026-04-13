# Luther MVP Implementation Plan: Overview

## Summary

This plan takes Luther from its current state (all production code is stubs throwing "Not yet implemented", with 424 tests already written) to a fully working system. Every stub gets replaced with real working code. The existing tests define behavioral contracts — when implementation is correct, tests flip from fail to pass.

**Current state:** 73 tests pass (fakes + logger), 351 fail against stubs.
**Target state:** All 424 tests pass, `bun run lint && bun run check:quality` clean.

## Ground Rules

1. **Tests are the spec.** Do not modify any test file. If a test expectation seems wrong, flag it — but implement to satisfy it.
2. **Conventions are law.** Tab indentation, `.js` import extensions, max 50 lines/function, no `any`, no inline disables, Biome formatting.
3. **Regression is unacceptable.** After each phase, all 73 currently-passing tests must still pass.
4. **Types are frozen.** `src/engine/types.ts` is complete. Exception: `AbandonPROutput` in `src/steps/abandon-pr.ts` needs a `reason` field (tests expect `result.reason`).
5. **DI everywhere.** All external I/O flows through injected `Dependencies`. Production code never touches `Bun.spawn`, `console.*`, or `fs.*` directly — it uses `ProcessRunner`, `OutputManager`, etc.

## Dependency DAG

```
Tier 0 (no Luther deps):
  Phase 02: lib/output.ts
  Phase 03: lib/session.ts
  Phase 04: engine/persistence.ts + engine/retry.ts
  Phase 05: lib/cli.ts

Tier 1 (depends on Tier 0 interfaces only):
  Phase 06: lib/gh.ts
  Phase 07: lib/llxprt.ts

Tier 2 (depends on Tier 0 + Tier 1):
  Phase 08: engine/machine.ts (XState wiring)

Tier 3 (depends on Tier 2 — step handlers):
  Phase 09: steps/scan-issues, plan-fix, review-plan
  Phase 10: steps/implement-fix, run-tests, fix-tests
  Phase 11: steps/commit-push, submit-pr, watch-pr-checks
  Phase 12: steps/diagnose-ci, triage-cr, respond-cr
  Phase 13: steps/remediate, abandon-pr, log-outcome

Tier 4 (depends on everything):
  Phase 14: engine/runner.ts + src/index.ts (orchestration + entry point)

Tier 5 (verification):
  Phase 15: Integration tests pass (happy-path, abandonment)
```

Phases within a tier can be implemented in any order. Tiers must be completed before higher tiers. Phase 01 is a preparatory types-only phase.

## Phase List

| Phase | File | Scope | Tests Targeted |
|-------|------|-------|----------------|
| 01 | `01-types-fixup.md` | Add `reason` field to `AbandonPROutput` | 0 new (prep) |
| 02 | `02-lib-output.md` | `FileOutputManager` (5 methods) | ~12 tests |
| 03 | `03-lib-session.md` | `FileSessionManager` (4 methods) | ~8 tests |
| 04 | `04-engine-persistence-retry.md` | `persistence.ts` (3 fns) + `retry.ts` (1 fn) | ~24 tests |
| 05 | `05-lib-cli.md` | `parseArgs` + `index.ts` entry point | ~11 tests |
| 06 | `06-lib-gh.md` | `GhClient` (13 methods) | ~14 tests |
| 07 | `07-lib-llxprt.md` | `LlxprtSubprocess` (1 method) | ~6 tests |
| 08 | `08-engine-machine.md` | `loadWorkflowDefinition` + `createLutherMachine` | ~65 tests |
| 09 | `09-steps-scan-plan-review.md` | `scanIssues` + `planFix` + `reviewPlan` | ~40 tests |
| 10 | `10-steps-implement-test.md` | `implementFix` + `runTests` + `fixTests` | ~27 tests |
| 11 | `11-steps-commit-submit-watch.md` | `commitPush` + `submitPR` + `watchPRChecks` | ~28 tests |
| 12 | `12-steps-diagnose-triage-respond.md` | `diagnoseCI` + `triageCR` + `respondCR` | ~38 tests |
| 13 | `13-steps-remediate-abandon-log.md` | `remediate` + `abandonPR` + `logOutcome` | ~41 tests |
| 14 | `14-engine-runner.md` | `loadConfig` + `createDependencies` + `runWorkflow` | ~24 tests |
| 15 | `15-integration.md` | All integration + cross-cutting tests pass | ~30 tests |

## Verification Strategy

After each phase, run this exact sequence:

```bash
cd llxprt-luther
# 1. Run targeted tests for this phase
bun test test/path/to/specific.test.ts

# 2. Verify no regression
bun test

# 3. Verify code quality
bun run lint
bun run format:check
bun run check:quality
```

Phase is complete when:
- All targeted test files pass (specific tests for this phase)
- Total passing tests ≥ previous count (no regression)
- lint + format + quality checks are clean

## Magnitude Estimate

- **Total source LoC to write:** ~1500-2000 lines across ~20 source files
- **Largest single file:** `gh.ts` (~150 lines), `machine.ts` (~120 lines), `runner.ts` (~150 lines)
- **Average step handler:** 30-50 lines each
- **Engine files:** 40-80 lines each
- **Lib files:** 30-60 lines each

## Execution Tracking

| Phase | Status | Pass Count After | Notes |
|-------|--------|------------------|-------|
| 01 | pending | 73 | Types-only, no test change |
| 02 | pending | ~85 | |
| 03 | pending | ~93 | |
| 04 | pending | ~117 | |
| 05 | pending | ~128 | Some CLI tests may need Phase 14 |
| 06 | pending | ~128 | Production-only, no direct test flip |
| 07 | pending | ~128 | Production-only, no direct test flip |
| 08 | pending | ~193 | Biggest test flip |
| 09 | pending | ~233 | |
| 10 | pending | ~260 | |
| 11 | pending | ~288 | |
| 12 | pending | ~326 | |
| 13 | pending | ~367 | |
| 14 | pending | ~391 | |
| 15 | pending | 424 | All tests green |

Update this table as each phase completes. The pass counts are estimates — actual counts may vary depending on test interdependencies.

## Key Technical Notes

### State Name Mapping
The workflow JSON (`fix-issue.json`) uses camelCase state names (`prWatching`, `diagnosingCI`). The machine tests use UPPERCASE names (`WATCHING`, `DIAGNOSING`). The `machine.ts` implementation must map between these — the machine tests create actors at UPPERCASE state names via `resolveState({ value: state })`.

### XState v5 Patterns
- `setup({ actors: { ... }, guards: { ... } })` to register implementations
- `fromPromise()` wraps each async step handler
- Guards are pure predicates on `context`
- Events carry data via `{ type: "EVENT_NAME", data: { ... } }`

### ProcessRunner vs GhClient
- `GhClient` wraps the `gh` CLI using `ProcessRunner.run()` for all GitHub operations
- Step handlers that run git commands use `ProcessRunner.run()` directly (e.g., `commitPush`)
- Step handlers that need GitHub API calls use `deps.github.*` methods

### Session Context Rules
- **NO session:** planFix, reviewPlan, triageCR, diagnoseCI
- **YES session:** implementFix, fixTests, remediate
- **Session heading per step:** each step appends exactly one section with a specific heading
