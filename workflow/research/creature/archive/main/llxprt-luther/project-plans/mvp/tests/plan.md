# Test Plan: Luther MVP Behavioral Tests

## Goal

Write failing behavioral tests for every testable requirement in the requirements document. Each test calls real code (which is currently a stub that throws "Not yet implemented"), asserts specific behavioral expectations, and therefore fails. If any test passes, the test is bad — it's testing nothing.

## Methodology

- All tests are behavioral: verify inputs produce expected outputs/effects, not internal method calls.
- No mocks. External dependencies are faked via dependency injection.
- Each test file covers one module or one coherent group of requirements.
- Tests reference requirement IDs in `describe` blocks for traceability.
- Fakes must be functional (return realistic test data) so tests fail because of stub source code, not because fakes throw.

## Excluded Requirements

These sections contain configuration/project meta-requirements that are enforced by tooling, not by behavioral tests:

| Section | Reason |
|---|---|
| 22. Configuration [WORKFLOW] | Config shape — validated at load time, tested via runner/CLI tests |
| 27. Code Quality [PROJECT] | Enforced by Biome, ESLint, check-quality.ts |
| 28. Testing [PROJECT] | Meta-requirements about the tests themselves |
| 30. CI Pipeline [PROJECT] | CI config — enforced by pipeline definition |

Individual excluded requirements within included sections:

| Requirement | Reason |
|---|---|
| REQ-ENG-006 | Design constraint ("JSON defines topology only") — not a testable behavior |
| REQ-ENG-022 | "Run indefinitely" — structural property, not a unit-testable behavior |
| REQ-UI-002 | Enforced by ESLint no-console rule |
| REQ-UI-003 | Design constraint, covered by logger tests indirectly |
| REQ-CLI-006 | Architectural constraint ("no other module writes to console") — enforced by ESLint no-console, not behavioral test |

## Prerequisites: Functional Fakes

Before writing behavioral tests, the three fake implementations must be upgraded from stubs to functional fakes that return realistic test data:

### FakeGithubClient
- `listOpenUnassignedIssues` → returns configurable list of `IssueContext` objects
- `assignIssue` / `unassignIssue` → tracks assignments in internal map
- `addLabel` → tracks labels in internal map
- `createPR` → returns a `PRContext` with configurable PR number
- `closePR` → tracks closed PRs
- `getPRChecks` → returns configurable `PRChecksResult`
- `getCRComments` → returns configurable `CRComment[]`
- `replyCRComment` / `resolveCRThread` → tracks replies and resolutions
- `getWorkflowLogs` → returns configurable log string
- `retriggerWorkflow` → tracks retrigger calls
- `createIssue` → returns configurable `IssueContext`
- Support for configuring errors (throw on specific calls to test error paths)
- `findPRByBranch(owner, repo, branch)` → returns PR or null (crash recovery)
- `getIssueAssignment(owner, repo, issueNumber)` → returns assignee or null
- `getIssueComments(owner, repo, issueNumber)` → returns comments array
- `hasUnresolvedThreads(owner, repo, prNumber)` → returns boolean (for post-flaky-filing branching)

### FakeLlxprtClient
- `invoke` → returns configurable `LlxprtResult` (exit code, stdout, stderr)
- Tracks invocation history (prompts, profiles, working dirs) for assertion
- Support configuring per-call responses (first call returns X, second returns Y)
- Support for configuring errors (throw on specific calls)

### FakeProcess (already exists, needs enhancement)
- Signal handler registration (capture registered signal handlers)
- Signal delivery simulation (emit SIGINT/SIGTERM to registered handlers)
- Process group kill simulation (track kill signals sent to PGIDs)
- Exit code tracking
- Subprocess cwd, env, stdin/stdout/stderr capture per invocation
- Timeout simulation (subprocess exceeds timeout)

### Additional Fakes Needed

**FakeOutputManager** (implements `OutputManager`):
- `writeJson` / `writeText` → stores in memory map
- `readJson` / `readText` → reads from memory map, validates with provided function
- `clean` → clears memory map (but preserves logs/ and state.json per REQ-CROSS-ART-003)
- Tracks all reads/writes for assertion
- Support schema-failure simulation: configure readJson to throw on specific files (malformed JSON, schema mismatch, missing file)
- Support path-scope assertions: verify all reads/writes target ./tmp/

**FakeSessionManager** (implements `SessionManager`):
- `create` → clears content, stores issue
- `appendSection` → appends to in-memory content with deterministic ordering
- `getContent` → returns accumulated content
- `clear` → resets
- Tracks call history for assertion (section names and content in order)
- Support verification: method to check all required sections are present (issue, plan, verdict, implementation notes, PR, loop summaries)

**FakeGitState** (for git operations, crash recovery, and workspace hygiene):
- Tracks current branch, available local branches, available remote branches
- Tracks working directory dirty/clean status
- Tracks committed-but-not-pushed state (crash recovery)
- `branchExists(name)` → boolean (local)
- `remoteBranchExists(name)` → boolean
- `isDirty()` → boolean
- `getCurrentBranch()` → string
- `hasUnpushedCommits()` → boolean
- Configurable non-fast-forward and rebase outcomes
- Supports idempotent delete operations (deleting already-deleted branch is no-op)
- `isDetachedHead()` → boolean (for detached HEAD recovery test)
- Can be set to detached HEAD state to simulate interrupted rebase

**FakeClock** (for sleep/timeout testing):
- Replaces real timers (setTimeout, setInterval)
- `advance(ms)` → trigger all timers that would fire within that window
- Enables testing idle sleep duration and CR wait timeout without real delays

**FakeLoggerSink** (for logger output verification):
- In-memory writable stream that captures all pino output
- `getLines()` → array of parsed JSON log entries
- `getLinesByLevel(level)` → filtered by level number
- `clear()` → reset
- Used by logger tests to verify which messages are emitted at which levels without writing to disk

**FakeRng** (for deterministic jitter testing):
- Replaces Math.random() in the retry utility
- Returns configurable sequence of values
- Enables deterministic assertion on jitter amounts

## Test File Inventory

### Existing files (to be REPLACED — current stubs test "throws NYI" which is not behavioral):

| File | Status |
|---|---|
| test/engine/machine.test.ts | Replace |
| test/engine/persistence.test.ts | Replace |
| test/engine/runner.test.ts | Replace |
| test/steps/scan-issues.test.ts | Replace |
| test/steps/plan-fix.test.ts | Replace |
| test/steps/review-plan.test.ts | Replace |
| test/steps/implement-fix.test.ts | Replace |
| test/steps/run-tests.test.ts | Replace |
| test/steps/submit-pr.test.ts | Replace |
| test/steps/watch-pr-checks.test.ts | Replace |
| test/steps/diagnose-ci.test.ts | Replace |
| test/steps/triage-cr.test.ts | Replace |
| test/steps/remediate.test.ts | Replace |
| test/steps/respond-cr.test.ts | Replace |
| test/steps/commit-push.test.ts | Replace |
| test/steps/log-outcome.test.ts | Replace |
| test/steps/abandon-pr.test.ts | Replace |
| test/integration/happy-path.test.ts | Replace |
| test/integration/abandonment.test.ts | Replace |

### New files to create:

| File | Coverage Area |
|---|---|
| test/engine/retry.test.ts | Exponential backoff utility |
| test/engine/signal.test.ts | Graceful shutdown |
| test/lib/logger.test.ts | Pino logger setup |
| test/lib/ui.test.ts | User-facing output |
| test/lib/gh.test.ts | GitHub CLI wrapper |
| test/lib/llxprt.test.ts | LLxprt subprocess |
| test/lib/session.test.ts | Session management |
| test/lib/output.test.ts | Structured output manager |
| test/workflow/loop-limits.test.ts | Loop counter guards |
| test/workflow/git-operations.test.ts | Git operations + hygiene |
| test/workflow/artifact-mgmt.test.ts | Artifact cleanup |
| test/workflow/crash-recovery.test.ts | Crash recovery |
| test/workflow/transient-handling.test.ts | Transient failure workflow usage |
| test/workflow/context-rules.test.ts | Agent context/session rules |
| test/workflow/sleeping.test.ts | Idle sleep behavior |
| test/workflow/post-remediation-testing.test.ts | Post-remediation testing |
| test/workflow/watch-timeout.test.ts | CR wait timeout after CI passes |
| test/workflow/type-contracts.test.ts | §6 Contract completeness (REQ-TYPE-*) |
| test/workflow/retry-policy.test.ts | §7.1 Cross-cutting retry policy (REQ-CROSS-RETRY-*) |
| test/workflow/concurrent-lock.test.ts | §7.5 Concurrent execution prevention (REQ-CROSS-LOCK-*) |
| test/workflow/target-config.test.ts | §7.6 TargetConfig loading/validation (REQ-CROSS-TCONF-*) |
| test/workflow/luther-config-validation.test.ts | §7.7–7.8 LutherConfig validation (REQ-CROSS-LCONF-*) |
| test/workflow/error-taxonomy.test.ts | §7.10 Error classification (REQ-CROSS-ERR-*) |
| test/workflow/context-reset.test.ts | §7.11 Context reset between runs (REQ-CROSS-RESET-*) |
| test/workflow/idempotency.test.ts | §7.12 Idempotent operations (REQ-CROSS-IDEM-*) |
| test/workflow/cancellation.test.ts | §7.13 Cancellation semantics (REQ-CROSS-CANCEL-*) |
| test/workflow/clock-consistency.test.ts | §7.14 Clock source consistency (REQ-CROSS-CLOCK-*) |
| test/workflow/cross-cutting-steps.test.ts | Cross-cutting step behavior (error format, timeout, profile) |
| test/workflow/prompt-construction.test.ts | §7.9 Prompt construction (consolidated in context-rules.test.ts) |
| test/steps/fix-tests.test.ts | Test fixing step |
| test/cli/cli-args.test.ts | CLI argument parsing |
| test/fakes/fake-output.ts | OutputManager fake |
| test/fakes/fake-session.ts | SessionManager fake |
| test/fakes/fake-clock.ts | Timer/sleep fake |
| test/fakes/fake-rng.ts | Deterministic RNG fake |
| test/fakes/fake-git-state.ts | Git operations simulation |
| test/fakes/fake-logger-sink.ts | In-memory pino stream |

## File Count Summary

| Category | Test Files | Fake Files | Total |
|---|---|---|---|
| test/engine/ | 5 | 0 | 5 |
| test/lib/ | 6 | 0 | 6 |
| test/steps/ | 15 | 0 | 15 |
| test/workflow/ | 21 | 0 | 21 |
| test/cli/ | 1 | 0 | 1 |
| test/integration/ | 2 | 0 | 2 |
| test/fakes/ | 0 | 9 | 9 |
| **Total** | **50** | **9** | **59** |

## Creation Order

1. **Fakes first** (tests depend on them):
   - test/fakes/fake-output.ts
   - test/fakes/fake-session.ts
   - test/fakes/fake-clock.ts
   - test/fakes/fake-rng.ts
   - Upgrade test/fakes/fake-github.ts (functional + crash recovery methods)
   - Upgrade test/fakes/fake-llxprt.ts (functional)
   - Upgrade test/fakes/fake-process.ts (signal/kill/subprocess tracking)
   - test/fakes/fake-git-state.ts
   - test/fakes/fake-logger-sink.ts

2. **Engine tests** (foundation — no workflow dependencies):
   - test/engine/machine.test.ts
   - test/engine/persistence.test.ts
   - test/engine/retry.test.ts
   - test/engine/signal.test.ts
   - test/engine/runner.test.ts

3. **Lib tests** (modules used by steps):
   - test/lib/logger.test.ts
   - test/lib/ui.test.ts
   - test/lib/output.test.ts
   - test/lib/session.test.ts
   - test/lib/gh.test.ts
   - test/lib/llxprt.test.ts

4. **Step tests — Phase 1** (pre-PR-submission):
   - test/steps/scan-issues.test.ts
   - test/steps/plan-fix.test.ts
   - test/steps/review-plan.test.ts
   - test/steps/implement-fix.test.ts
   - test/steps/run-tests.test.ts
   - test/steps/submit-pr.test.ts

5. **Step tests — Phase 1b** (test fixing):
   - test/steps/fix-tests.test.ts

6. **Step tests — Phase 2** (post-PR-submission):
   - test/steps/watch-pr-checks.test.ts
   - test/steps/diagnose-ci.test.ts
   - test/steps/triage-cr.test.ts
   - test/steps/remediate.test.ts
   - test/steps/respond-cr.test.ts
   - test/steps/commit-push.test.ts
   - test/steps/log-outcome.test.ts
   - test/steps/abandon-pr.test.ts

7. **Workflow tests** (cross-cutting behaviors):
   - test/workflow/loop-limits.test.ts
   - test/workflow/git-operations.test.ts
   - test/workflow/artifact-mgmt.test.ts
   - test/workflow/crash-recovery.test.ts
   - test/workflow/transient-handling.test.ts
   - test/workflow/context-rules.test.ts
   - test/workflow/sleeping.test.ts
   - test/workflow/post-remediation-testing.test.ts
   - test/workflow/watch-timeout.test.ts
   - test/workflow/type-contracts.test.ts
   - test/workflow/retry-policy.test.ts
   - test/workflow/concurrent-lock.test.ts
   - test/workflow/target-config.test.ts
   - test/workflow/luther-config-validation.test.ts
   - test/workflow/error-taxonomy.test.ts
   - test/workflow/context-reset.test.ts
   - test/workflow/idempotency.test.ts
   - test/workflow/cancellation.test.ts
   - test/workflow/clock-consistency.test.ts
   - test/workflow/cross-cutting-steps.test.ts

8. **CLI tests**:
    - test/cli/cli-args.test.ts

9. **Integration tests** (last — depend on everything):
   - test/integration/happy-path.test.ts
   - test/integration/abandonment.test.ts

## Verification

After all tests are written:

```
cd llxprt-luther
bun test
```

Expected result: **0 pass, N fail** (where N is the total test case count, ~300+).

If any test passes, it's a bad test — it's not testing real behavior. Investigate and fix.

Lint and format must still pass:
```
bun run lint
bun run format:check
bun run check:quality
```

## Detailed Test Specifications

See companion files:
- [engine-tests.md](./engine-tests.md) — Engine layer: machine, persistence, retry, signals, runner
- [lib-tests.md](./lib-tests.md) — Lib modules: logger, ui, gh, llxprt, session, output
- [step-tests-phase1.md](./step-tests-phase1.md) — Steps: scan, plan, review, implement, run-tests, submit, fix-tests
- [step-tests-phase2.md](./step-tests-phase2.md) — Steps: watch, diagnose, triage, remediate, respond, push, log, abandon
- [workflow-tests.md](./workflow-tests.md) — Cross-cutting: loops, git, artifacts, crash recovery, transient, context, CLI, integration, type contracts, config validation, retry policy, concurrency, error taxonomy, resets, idempotency, cancellation, clock consistency

---

## REQ-ID → Test File Traceability Matrix

Every canonical REQ-ID from REQUIREMENTS.md mapped to its test file. IDs not listed here are excluded (see Excluded Requirements above).

### §1 State Machine (REQ-SM-*)

| REQ-ID | Test File | Status |
|---|---|---|
| REQ-SM-001 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-002 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-003 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-004 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-005 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-006 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-007 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-008 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-009 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-010 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-011 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-012 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-013 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-014 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-015 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-016 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-017 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-018 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-019 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-020 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-021 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-022 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-023 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-024 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-025 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-026 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-027 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-028 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-029 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-030 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-ERR-001 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-ERR-002 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-ERR-003 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-PREC-001 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-PREC-002 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-PREC-003 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-PREC-004 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-PREC-005 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-PREC-006 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-G001 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-G002 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-G003 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-G004 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-G005 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-G006 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-G007 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-G008 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-INV-001 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-INV-002 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-INV-003 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-INV-004 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-CTX-001 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-CTX-002 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-CTX-003 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-CTX-004 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-CTX-005 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-CTX-006 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-CTX-007 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-CTX-008 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-RST-001 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-RST-002 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-RST-003 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-RST-004 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-RST-005 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-RST-006 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-RST-007 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-RST-008 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-RST-009 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-HIST-001 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-HIST-002 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-HIST-003 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-TERM-001 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-TERM-002 | engine-tests.md → machine.test.ts + workflow-tests.md → sleeping.test.ts | Spec [OK] |
| REQ-SM-TERM-003 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-CLEANUP-001 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-CLEANUP-002 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-CLEANUP-003 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-SM-CLEANUP-004 | engine-tests.md → machine.test.ts | Spec [OK] |

### §2 Step Handlers (REQ-STEP-*, REQ-CROSS-STEP-ERR-001)

| REQ-ID | Test File | Status |
|---|---|---|
| REQ-STEP-SESSION-001 | step-tests-phase1.md + step-tests-phase2.md (per-step) | Spec [OK] |
| REQ-CROSS-STEP-ERR-001 | workflow-tests.md → cross-cutting-steps.test.ts | Spec [OK] |
| REQ-STEP-TIMEOUT-001 | workflow-tests.md → cross-cutting-steps.test.ts | Spec [OK] |
| REQ-STEP-PROFILE-001 | workflow-tests.md → cross-cutting-steps.test.ts + context-rules.test.ts | Spec [OK] |
| REQ-STEP-SCAN-001 | step-tests-phase1.md → scan-issues.test.ts | Spec [OK] |
| REQ-STEP-SCAN-002 | step-tests-phase1.md → scan-issues.test.ts | Spec [OK] |
| REQ-STEP-SCAN-003 | step-tests-phase1.md → scan-issues.test.ts | Spec [OK] |
| REQ-STEP-SCAN-004 | step-tests-phase1.md → scan-issues.test.ts | Spec [OK] |
| REQ-STEP-SCAN-005 | step-tests-phase1.md → scan-issues.test.ts | Spec [OK] |
| REQ-STEP-SCAN-006 | step-tests-phase1.md → scan-issues.test.ts | Spec [OK] |
| REQ-STEP-SCAN-007 | step-tests-phase1.md → scan-issues.test.ts | Spec [OK] |
| REQ-STEP-SCAN-008 | step-tests-phase1.md → scan-issues.test.ts | Spec [OK] |
| REQ-STEP-SCAN-009 | step-tests-phase1.md → scan-issues.test.ts | Spec [OK] |
| REQ-STEP-SCAN-010 | step-tests-phase1.md → scan-issues.test.ts | Spec [OK] |
| REQ-STEP-SCAN-011 | step-tests-phase1.md → scan-issues.test.ts | Spec [OK] |
| REQ-STEP-PLAN-001 | step-tests-phase1.md → plan-fix.test.ts | Spec [OK] |
| REQ-STEP-PLAN-002 | step-tests-phase1.md → plan-fix.test.ts | Spec [OK] |
| REQ-STEP-PLAN-003 | step-tests-phase1.md → plan-fix.test.ts | Spec [OK] |
| REQ-STEP-PLAN-004 | step-tests-phase1.md → plan-fix.test.ts | Spec [OK] |
| REQ-STEP-PLAN-005 | step-tests-phase1.md → plan-fix.test.ts | Spec [OK] |
| REQ-STEP-PLAN-006 | step-tests-phase1.md → plan-fix.test.ts | Spec [OK] |
| REQ-STEP-PLAN-007 | step-tests-phase1.md → plan-fix.test.ts | Spec [OK] |
| REQ-STEP-REV-001 | step-tests-phase1.md → review-plan.test.ts | Spec [OK] |
| REQ-STEP-REV-002 | step-tests-phase1.md → review-plan.test.ts | Spec [OK] |
| REQ-STEP-REV-003 | step-tests-phase1.md → review-plan.test.ts | Spec [OK] |
| REQ-STEP-REV-004 | step-tests-phase1.md → review-plan.test.ts | Spec [OK] |
| REQ-STEP-REV-005 | step-tests-phase1.md → review-plan.test.ts | Spec [OK] |
| REQ-STEP-REV-006 | step-tests-phase1.md → review-plan.test.ts | Spec [OK] |
| REQ-STEP-REV-007 | step-tests-phase1.md → review-plan.test.ts | Spec [OK] |
| REQ-STEP-REV-008 | step-tests-phase1.md → review-plan.test.ts | Spec [OK] |
| REQ-STEP-REV-009 | step-tests-phase1.md → review-plan.test.ts | Spec [OK] |
| REQ-STEP-IMPL-001 | step-tests-phase1.md → implement-fix.test.ts | Spec [OK] |
| REQ-STEP-IMPL-002 | step-tests-phase1.md → implement-fix.test.ts | Spec [OK] |
| REQ-STEP-IMPL-003 | step-tests-phase1.md → implement-fix.test.ts | Spec [OK] |
| REQ-STEP-IMPL-004 | step-tests-phase1.md → implement-fix.test.ts | Spec [OK] |
| REQ-STEP-IMPL-005 | step-tests-phase1.md → implement-fix.test.ts | Spec [OK] |
| REQ-STEP-IMPL-006 | step-tests-phase1.md → implement-fix.test.ts | Spec [OK] |
| REQ-STEP-TEST-001 | step-tests-phase1.md → run-tests.test.ts | Spec [OK] |
| REQ-STEP-TEST-002 | step-tests-phase1.md → run-tests.test.ts | Spec [OK] |
| REQ-STEP-TEST-003 | step-tests-phase1.md → run-tests.test.ts | Spec [OK] |
| REQ-STEP-TEST-004 | step-tests-phase1.md → run-tests.test.ts | Spec [OK] |
| REQ-STEP-TEST-005 | step-tests-phase1.md → run-tests.test.ts | Spec [OK] |
| REQ-STEP-TEST-006 | step-tests-phase1.md → run-tests.test.ts | Spec [OK] |
| REQ-STEP-TEST-007 | step-tests-phase1.md → run-tests.test.ts | Spec [OK] |
| REQ-STEP-TEST-008 | step-tests-phase1.md → run-tests.test.ts | Spec [OK] |
| REQ-STEP-PUSH-001 | step-tests-phase2.md → commit-push.test.ts | Spec [OK] |
| REQ-STEP-PUSH-002 | step-tests-phase2.md → commit-push.test.ts | Spec [OK] |
| REQ-STEP-PUSH-003 | step-tests-phase2.md → commit-push.test.ts | Spec [OK] |
| REQ-STEP-PUSH-004 | step-tests-phase2.md → commit-push.test.ts | Spec [OK] |
| REQ-STEP-PUSH-005 | step-tests-phase2.md → commit-push.test.ts | Spec [OK] |
| REQ-STEP-PUSH-006 | step-tests-phase2.md → commit-push.test.ts | Spec [OK] |
| REQ-STEP-SUB-001 | step-tests-phase1.md → submit-pr.test.ts | Spec [OK] |
| REQ-STEP-SUB-002 | step-tests-phase1.md → submit-pr.test.ts | Spec [OK] |
| REQ-STEP-SUB-003 | step-tests-phase1.md → submit-pr.test.ts | Spec [OK] |
| REQ-STEP-SUB-004 | step-tests-phase1.md → submit-pr.test.ts | Spec [OK] |
| REQ-STEP-SUB-005 | step-tests-phase1.md → submit-pr.test.ts | Spec [OK] |
| REQ-STEP-SUB-006 | step-tests-phase1.md → submit-pr.test.ts | Spec [OK] |
| REQ-STEP-WATCH-001 | step-tests-phase2.md → watch-pr-checks.test.ts | Spec [OK] |
| REQ-STEP-WATCH-002 | step-tests-phase2.md → watch-pr-checks.test.ts | Spec [OK] |
| REQ-STEP-WATCH-003 | step-tests-phase2.md → watch-pr-checks.test.ts | Spec [OK] |
| REQ-STEP-WATCH-004 | step-tests-phase2.md → watch-pr-checks.test.ts | Spec [OK] |
| REQ-STEP-WATCH-005 | step-tests-phase2.md → watch-pr-checks.test.ts + workflow-tests.md → watch-timeout.test.ts | Spec [OK] |
| REQ-STEP-WATCH-006 | step-tests-phase2.md → watch-pr-checks.test.ts + workflow-tests.md → watch-timeout.test.ts | Spec [OK] |
| REQ-STEP-WATCH-007 | step-tests-phase2.md → watch-pr-checks.test.ts | Spec [OK] |
| REQ-STEP-WATCH-008 | step-tests-phase2.md → watch-pr-checks.test.ts | Spec [OK] |
| REQ-STEP-CIDR-001 | step-tests-phase2.md → diagnose-ci.test.ts | Spec [OK] |
| REQ-STEP-CIDR-002 | step-tests-phase2.md → diagnose-ci.test.ts | Spec [OK] |
| REQ-STEP-CIDR-003 | step-tests-phase2.md → diagnose-ci.test.ts | Spec [OK] |
| REQ-STEP-CIDR-004 | step-tests-phase2.md → diagnose-ci.test.ts | Spec [OK] |
| REQ-STEP-CIDR-005 | step-tests-phase2.md → diagnose-ci.test.ts | Spec [OK] |
| REQ-STEP-CIDR-006 | step-tests-phase2.md → diagnose-ci.test.ts | Spec [OK] |
| REQ-STEP-CIDR-007 | step-tests-phase2.md → diagnose-ci.test.ts | Spec [OK] |
| REQ-STEP-CIDR-008 | step-tests-phase2.md → diagnose-ci.test.ts | Spec [OK] |
| REQ-STEP-CIDR-009 | step-tests-phase2.md → diagnose-ci.test.ts | Spec [OK] |
| REQ-STEP-CRTG-001 | step-tests-phase2.md → triage-cr.test.ts | Spec [OK] |
| REQ-STEP-CRTG-002 | step-tests-phase2.md → triage-cr.test.ts | Spec [OK] |
| REQ-STEP-CRTG-003 | step-tests-phase2.md → triage-cr.test.ts | Spec [OK] |
| REQ-STEP-CRTG-004 | step-tests-phase2.md → triage-cr.test.ts | Spec [OK] |
| REQ-STEP-CRTG-005 | step-tests-phase2.md → triage-cr.test.ts | Spec [OK] |
| REQ-STEP-CRTG-006 | step-tests-phase2.md → triage-cr.test.ts | Spec [OK] |
| REQ-STEP-CRTG-007 | step-tests-phase2.md → triage-cr.test.ts | Spec [OK] |
| REQ-STEP-CRSP-001 | step-tests-phase2.md → respond-cr.test.ts | Spec [OK] |
| REQ-STEP-CRSP-002 | step-tests-phase2.md → respond-cr.test.ts | Spec [OK] |
| REQ-STEP-CRSP-003 | step-tests-phase2.md → respond-cr.test.ts | Spec [OK] |
| REQ-STEP-CRSP-004 | step-tests-phase2.md → respond-cr.test.ts | Spec [OK] |
| REQ-STEP-CRSP-005 | step-tests-phase2.md → respond-cr.test.ts | Spec [OK] |
| REQ-STEP-CRSP-006 | step-tests-phase2.md → respond-cr.test.ts | Spec [OK] |
| REQ-STEP-CRSP-007 | step-tests-phase2.md → respond-cr.test.ts | Spec [OK] |
| REQ-STEP-CRSP-008 | step-tests-phase2.md → respond-cr.test.ts | Spec [OK] |
| REQ-STEP-REMED-001 | step-tests-phase2.md → remediate.test.ts | Spec [OK] |
| REQ-STEP-REMED-002 | step-tests-phase2.md → remediate.test.ts | Spec [OK] |
| REQ-STEP-REMED-003 | step-tests-phase2.md → remediate.test.ts | Spec [OK] |
| REQ-STEP-REMED-004 | step-tests-phase2.md → remediate.test.ts | Spec [OK] |
| REQ-STEP-REMED-005 | step-tests-phase2.md → remediate.test.ts | Spec [OK] |
| REQ-STEP-REMED-006 | step-tests-phase2.md → remediate.test.ts | Spec [OK] |
| REQ-STEP-REMED-007 | step-tests-phase2.md → remediate.test.ts | Spec [OK] |
| REQ-STEP-TFIX-001 | step-tests-phase1.md → fix-tests.test.ts | Spec [OK] (BLOCKED) |
| REQ-STEP-TFIX-002 | step-tests-phase1.md → fix-tests.test.ts | Spec [OK] (BLOCKED) |
| REQ-STEP-TFIX-003 | step-tests-phase1.md → fix-tests.test.ts | Spec [OK] (BLOCKED) |
| REQ-STEP-TFIX-004 | step-tests-phase1.md → fix-tests.test.ts | Spec [OK] (BLOCKED) |
| REQ-STEP-TFIX-005 | step-tests-phase1.md → fix-tests.test.ts | Spec [OK] (BLOCKED) |
| REQ-STEP-TFIX-006 | step-tests-phase1.md → fix-tests.test.ts | Spec [OK] (BLOCKED) |
| REQ-STEP-ABAN-001 | step-tests-phase2.md → abandon-pr.test.ts | Spec [OK] |
| REQ-STEP-ABAN-002 | step-tests-phase2.md → abandon-pr.test.ts | Spec [OK] |
| REQ-STEP-ABAN-003 | step-tests-phase2.md → abandon-pr.test.ts | Spec [OK] |
| REQ-STEP-ABAN-004 | step-tests-phase2.md → abandon-pr.test.ts | Spec [OK] |
| REQ-STEP-ABAN-005 | step-tests-phase2.md → abandon-pr.test.ts | Spec [OK] |
| REQ-STEP-ABAN-006 | step-tests-phase2.md → abandon-pr.test.ts | Spec [OK] |
| REQ-STEP-ABAN-007 | step-tests-phase2.md → abandon-pr.test.ts | Spec [OK] |
| REQ-STEP-ABAN-008 | step-tests-phase2.md → abandon-pr.test.ts | Spec [OK] |
| REQ-STEP-ABAN-009 | step-tests-phase2.md → abandon-pr.test.ts | Spec [OK] |
| REQ-STEP-ABAN-010 | step-tests-phase2.md → abandon-pr.test.ts | Spec [OK] |
| REQ-STEP-LOG-001 | step-tests-phase2.md → log-outcome.test.ts | Spec [OK] |
| REQ-STEP-LOG-002 | step-tests-phase2.md → log-outcome.test.ts | Spec [OK] |
| REQ-STEP-LOG-003 | step-tests-phase2.md → log-outcome.test.ts | Spec [OK] |
| REQ-STEP-LOG-004 | step-tests-phase2.md → log-outcome.test.ts | Spec [OK] |
| REQ-STEP-LOG-005 | step-tests-phase2.md → log-outcome.test.ts | Spec [OK] |
| REQ-STEP-LOG-006 | step-tests-phase2.md → log-outcome.test.ts | Spec [OK] |
| REQ-STEP-LOG-007 | step-tests-phase2.md → log-outcome.test.ts | Spec [OK] |
| REQ-STEP-LOG-008 | step-tests-phase2.md → log-outcome.test.ts | Spec [OK] |
| REQ-STEP-LOG-009 | step-tests-phase2.md → log-outcome.test.ts | Spec [OK] |
| REQ-STEP-LOG-010 | step-tests-phase2.md → log-outcome.test.ts | Spec [OK] |
| REQ-STEP-LOG-011 | step-tests-phase2.md → log-outcome.test.ts | Spec [OK] |
| REQ-STEP-LOG-012 | step-tests-phase2.md → log-outcome.test.ts | Spec [OK] |
| REQ-STEP-LOG-013 | step-tests-phase2.md → log-outcome.test.ts | Spec [OK] |
| REQ-STEP-LOG-FMT-001 | step-tests-phase2.md → log-outcome.test.ts | Spec [OK] |
| REQ-STEP-LOG-FMT-002 | step-tests-phase2.md → log-outcome.test.ts | Spec [OK] |
| REQ-STEP-LOG-FMT-003 | step-tests-phase2.md → log-outcome.test.ts | Spec [OK] |
| REQ-STEP-LOG-FMT-004 | step-tests-phase2.md → log-outcome.test.ts | Spec [OK] |
| REQ-STEP-LOG-FMT-005 | step-tests-phase2.md → log-outcome.test.ts | Spec [OK] |

### §3 Libraries (REQ-LIB-*)

| REQ-ID | Test File | Status |
|---|---|---|
| REQ-LIB-SESSION-001 | lib-tests.md → session.test.ts | Spec [OK] |
| REQ-LIB-SESSION-002 | lib-tests.md → session.test.ts | Spec [OK] |
| REQ-LIB-SESSION-003 | lib-tests.md → session.test.ts | Spec [OK] |
| REQ-LIB-SESSION-004 | lib-tests.md → session.test.ts | Spec [OK] |
| REQ-LIB-SESSION-005 | lib-tests.md → session.test.ts | Spec [OK] |
| REQ-LIB-SESSION-006 | lib-tests.md → session.test.ts | Spec [OK] |
| REQ-LIB-SESSION-007 | lib-tests.md → session.test.ts | Spec [OK] |
| REQ-LIB-SESSION-008 | lib-tests.md → session.test.ts | Spec [OK] |
| REQ-LIB-SESSION-009 | lib-tests.md → session.test.ts | Spec [OK] |
| REQ-LIB-SESSION-010 | lib-tests.md → session.test.ts | Spec [OK] |
| REQ-LIB-OUTPUT-001 | lib-tests.md → output.test.ts | Spec [OK] |
| REQ-LIB-OUTPUT-002 | lib-tests.md → output.test.ts | Spec [OK] |
| REQ-LIB-OUTPUT-003 | lib-tests.md → output.test.ts | Spec [OK] |
| REQ-LIB-OUTPUT-004 | lib-tests.md → output.test.ts | Spec [OK] |
| REQ-LIB-OUTPUT-005 | lib-tests.md → output.test.ts | Spec [OK] |
| REQ-LIB-OUTPUT-006 | lib-tests.md → output.test.ts | Spec [OK] |
| REQ-LIB-OUTPUT-007 | lib-tests.md → output.test.ts | Spec [OK] |
| REQ-LIB-OUTPUT-008 | lib-tests.md → output.test.ts | Spec [OK] |
| REQ-LIB-OUTPUT-009 | lib-tests.md → output.test.ts | Spec [OK] |
| REQ-LIB-OUTPUT-010 | lib-tests.md → output.test.ts | Spec [OK] |
| REQ-LIB-LOG-001 | lib-tests.md → logger.test.ts | Spec [OK] |
| REQ-LIB-LOG-002 | lib-tests.md → logger.test.ts | Spec [OK] |
| REQ-LIB-LOG-003 | lib-tests.md → logger.test.ts | Spec [OK] |
| REQ-LIB-LOG-004 | lib-tests.md → logger.test.ts | Spec [OK] |
| REQ-LIB-LOG-005 | lib-tests.md → logger.test.ts | Spec [OK] |
| REQ-LIB-LOG-006 | lib-tests.md → logger.test.ts | Spec [OK] |
| REQ-LIB-LOG-007 | lib-tests.md → logger.test.ts | Spec [OK] |
| REQ-LIB-LLXPRT-001 | lib-tests.md → llxprt.test.ts | Spec [OK] |
| REQ-LIB-LLXPRT-002 | lib-tests.md → llxprt.test.ts | Spec [OK] |
| REQ-LIB-LLXPRT-003 | lib-tests.md → llxprt.test.ts | Spec [OK] |
| REQ-LIB-LLXPRT-004 | lib-tests.md → llxprt.test.ts | Spec [OK] |
| REQ-LIB-LLXPRT-005 | lib-tests.md → llxprt.test.ts | Spec [OK] |
| REQ-LIB-LLXPRT-006 | lib-tests.md → llxprt.test.ts | Spec [OK] |
| REQ-LIB-LLXPRT-007 | lib-tests.md → llxprt.test.ts | Spec [OK] |
| REQ-LIB-LLXPRT-008 | lib-tests.md → llxprt.test.ts | Spec [OK] |
| REQ-LIB-LLXPRT-009 | lib-tests.md → llxprt.test.ts | Spec [OK] |
| REQ-LIB-GH-001 | lib-tests.md → gh.test.ts | Spec [OK] |
| REQ-LIB-GH-002 | lib-tests.md → gh.test.ts | Spec [OK] |
| REQ-LIB-GH-003 | lib-tests.md → gh.test.ts | Spec [OK] |
| REQ-LIB-GH-004 | lib-tests.md → gh.test.ts | Spec [OK] |
| REQ-LIB-GH-005 | lib-tests.md → gh.test.ts | Spec [OK] |
| REQ-LIB-GH-006 | lib-tests.md → gh.test.ts | Spec [OK] |
| REQ-LIB-GH-007 | lib-tests.md → gh.test.ts | Spec [OK] |
| REQ-LIB-GH-008 | lib-tests.md → gh.test.ts | Spec [OK] |
| REQ-LIB-GH-009 | lib-tests.md → gh.test.ts | Spec [OK] |
| REQ-LIB-GH-010 | lib-tests.md → gh.test.ts | Spec [OK] |
| REQ-LIB-GH-011 | lib-tests.md → gh.test.ts | Spec [OK] |
| REQ-LIB-GH-012 | lib-tests.md → gh.test.ts | Spec [OK] |
| REQ-LIB-GH-013 | lib-tests.md → gh.test.ts | Spec [OK] |
| REQ-LIB-GH-014 | lib-tests.md → gh.test.ts | Spec [OK] |
| REQ-LIB-GH-015 | lib-tests.md → gh.test.ts | Spec [OK] |
| REQ-LIB-GH-016 | lib-tests.md → gh.test.ts | Spec [OK] |
| REQ-LIB-UI-001 | lib-tests.md → ui.test.ts | Spec [OK] |
| REQ-LIB-UI-002 | lib-tests.md → ui.test.ts | Spec [OK] |
| REQ-LIB-UI-003 | lib-tests.md → ui.test.ts | Spec [OK] |

### §4 Engine (REQ-ENG-*)

| REQ-ID | Test File | Status |
|---|---|---|
| REQ-ENG-001 | engine-tests.md → persistence.test.ts | Spec [OK] |
| REQ-ENG-002 | engine-tests.md → persistence.test.ts | Spec [OK] |
| REQ-ENG-003 | engine-tests.md → persistence.test.ts | Spec [OK] |
| REQ-ENG-004 | engine-tests.md → persistence.test.ts | Spec [OK] |
| REQ-ENG-005 | engine-tests.md → persistence.test.ts | Spec [OK] |
| REQ-ENG-006 | Excluded — design constraint | N/A |
| REQ-ENG-007 | engine-tests.md → persistence.test.ts | Spec [OK] |
| REQ-ENG-008 | engine-tests.md → persistence.test.ts | Spec [OK] |
| REQ-ENG-009 | engine-tests.md → persistence.test.ts | Spec [OK] |
| REQ-ENG-010 | engine-tests.md → persistence.test.ts | Spec [OK] |
| REQ-ENG-011 | engine-tests.md → persistence.test.ts | Spec [OK] |
| REQ-ENG-012 | engine-tests.md → persistence.test.ts | Spec [OK] |
| REQ-ENG-020 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-ENG-021 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-ENG-022 | Excluded — structural property | N/A |
| REQ-ENG-023 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-ENG-024 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-ENG-025 | engine-tests.md → machine.test.ts | Spec [OK] |
| REQ-ENG-030 | engine-tests.md → runner.test.ts | Spec [OK] |
| REQ-ENG-031 | engine-tests.md → runner.test.ts | Spec [OK] |
| REQ-ENG-032 | engine-tests.md → runner.test.ts | Spec [OK] |
| REQ-ENG-033 | engine-tests.md → runner.test.ts | Spec [OK] |
| REQ-ENG-034 | engine-tests.md → runner.test.ts | Spec [OK] |
| REQ-ENG-035 | engine-tests.md → runner.test.ts | Spec [OK] |
| REQ-ENG-036 | engine-tests.md → runner.test.ts | Spec [OK] |
| REQ-ENG-037 | engine-tests.md → runner.test.ts | Spec [OK] |
| REQ-ENG-LIFE-001 | engine-tests.md → runner.test.ts | Spec [OK] |
| REQ-ENG-LIFE-002 | engine-tests.md → runner.test.ts | Spec [OK] |
| REQ-ENG-LIFE-003 | engine-tests.md → runner.test.ts | Spec [OK] |
| REQ-ENG-RESUME-001 | engine-tests.md → runner.test.ts + workflow-tests.md → crash-recovery.test.ts | Spec [OK] |
| REQ-ENG-RESUME-002 | engine-tests.md → runner.test.ts | Spec [OK] |
| REQ-ENG-RESUME-003 | engine-tests.md → runner.test.ts + workflow-tests.md → crash-recovery.test.ts | Spec [OK] |
| REQ-ENG-RESUME-004 | engine-tests.md → runner.test.ts + workflow-tests.md → crash-recovery.test.ts | Spec [OK] |
| REQ-ENG-RESUME-005 | engine-tests.md → runner.test.ts + workflow-tests.md → crash-recovery.test.ts | Spec [OK] |
| REQ-ENG-RETRY-001 | engine-tests.md → retry.test.ts | Spec [OK] (BLOCKED) |
| REQ-ENG-RETRY-002 | engine-tests.md → retry.test.ts | Spec [OK] (BLOCKED) |
| REQ-ENG-RETRY-003 | engine-tests.md → retry.test.ts | Spec [OK] (BLOCKED) |
| REQ-ENG-RETRY-004 | engine-tests.md → retry.test.ts | Spec [OK] (BLOCKED) |
| REQ-ENG-RETRY-005 | engine-tests.md → retry.test.ts | Spec [OK] (BLOCKED) |
| REQ-ENG-RETRY-006 | engine-tests.md → retry.test.ts | Spec [OK] (BLOCKED) |
| REQ-ENG-RETRY-007 | engine-tests.md → retry.test.ts | Spec [OK] (BLOCKED) |
| REQ-ENG-RETRY-008 | engine-tests.md → retry.test.ts | Spec [OK] |
| REQ-ENG-SIG-001 | engine-tests.md → signal.test.ts | Spec [OK] |
| REQ-ENG-SIG-002 | engine-tests.md → signal.test.ts | Spec [OK] |
| REQ-ENG-SIG-003 | engine-tests.md → signal.test.ts | Spec [OK] |
| REQ-ENG-SIG-004 | engine-tests.md → signal.test.ts | Spec [OK] |
| REQ-ENG-SIG-005 | engine-tests.md → signal.test.ts | Spec [OK] |

### §5 CLI (REQ-CLI-*)

| REQ-ID | Test File | Status |
|---|---|---|
| REQ-CLI-001 | workflow-tests.md → cli-args.test.ts | Spec [OK] |
| REQ-CLI-002 | workflow-tests.md → cli-args.test.ts | Spec [OK] |
| REQ-CLI-003 | workflow-tests.md → cli-args.test.ts | Spec [OK] |
| REQ-CLI-004 | workflow-tests.md → cli-args.test.ts | Spec [OK] |
| REQ-CLI-005 | workflow-tests.md → cli-args.test.ts | Spec [OK] |
| REQ-CLI-006 | workflow-tests.md → cli-args.test.ts | Spec [OK] |
| REQ-CLI-007 | workflow-tests.md → cli-args.test.ts | Spec [OK] |
| REQ-CLI-EXIT-001 | workflow-tests.md → cli-args.test.ts | Spec [OK] |
| REQ-CLI-EXIT-002 | workflow-tests.md → cli-args.test.ts | Spec [OK] |

### §6 Contract Completeness (REQ-TYPE-*)

| REQ-ID | Test File | Status |
|---|---|---|
| REQ-TYPE-CTX-001 | workflow-tests.md → type-contracts.test.ts | Spec [OK] |
| REQ-TYPE-CTX-002 | workflow-tests.md → type-contracts.test.ts | Spec [OK] |
| REQ-TYPE-CTX-003 | workflow-tests.md → type-contracts.test.ts | Spec [OK] |
| REQ-TYPE-RUN-001 | workflow-tests.md → type-contracts.test.ts | Spec [OK] |
| REQ-TYPE-RUN-002 | workflow-tests.md → type-contracts.test.ts | Spec [OK] |
| REQ-TYPE-RUN-003 | workflow-tests.md → type-contracts.test.ts | Spec [OK] |
| REQ-TYPE-CR-001 | workflow-tests.md → type-contracts.test.ts | Spec [OK] |
| REQ-TYPE-CR-002 | workflow-tests.md → type-contracts.test.ts | Spec [OK] |
| REQ-TYPE-CR-003 | workflow-tests.md → type-contracts.test.ts | Spec [OK] |
| REQ-TYPE-CR-004 | workflow-tests.md → type-contracts.test.ts | Spec [OK] |
| REQ-TYPE-CR-005 | workflow-tests.md → type-contracts.test.ts | Spec [OK] |
| REQ-TYPE-PR-001 | workflow-tests.md → type-contracts.test.ts | Spec [OK] |
| REQ-TYPE-PR-002 | workflow-tests.md → type-contracts.test.ts | Spec [OK] |
| REQ-TYPE-PR-003 | workflow-tests.md → type-contracts.test.ts | Spec [OK] |
| REQ-TYPE-PR-004 | workflow-tests.md → type-contracts.test.ts | Spec [OK] |
| REQ-TYPE-PR-005 | workflow-tests.md → type-contracts.test.ts | Spec [OK] |
| REQ-TYPE-ISSUE-001 | workflow-tests.md → type-contracts.test.ts | Spec [OK] |
| REQ-TYPE-ISSUE-002 | workflow-tests.md → type-contracts.test.ts | Spec [OK] |
| REQ-TYPE-ISSUE-003 | workflow-tests.md → type-contracts.test.ts | Spec [OK] |

### §7 Cross-Cutting (REQ-CROSS-*)

| REQ-ID | Test File | Status |
|---|---|---|
| REQ-CROSS-RETRY-001 | workflow-tests.md → retry-policy.test.ts + transient-handling.test.ts | Spec [OK] |
| REQ-CROSS-RETRY-002 | workflow-tests.md → retry-policy.test.ts + transient-handling.test.ts | Spec [OK] |
| REQ-CROSS-RETRY-003 | workflow-tests.md → retry-policy.test.ts + transient-handling.test.ts | Spec [OK] |
| REQ-CROSS-ART-001 | workflow-tests.md → artifact-mgmt.test.ts | Spec [OK] |
| REQ-CROSS-ART-002 | workflow-tests.md → artifact-mgmt.test.ts | Spec [OK] |
| REQ-CROSS-ART-003 | workflow-tests.md → artifact-mgmt.test.ts | Spec [OK] |
| REQ-CROSS-ART-004 | workflow-tests.md → artifact-mgmt.test.ts | Spec [OK] |
| REQ-CROSS-GIT-001 | workflow-tests.md → git-operations.test.ts | Spec [OK] |
| REQ-CROSS-GIT-002 | workflow-tests.md → git-operations.test.ts | Spec [OK] |
| REQ-CROSS-GIT-003 | workflow-tests.md → git-operations.test.ts | Spec [OK] |
| REQ-CROSS-GIT-004 | workflow-tests.md → git-operations.test.ts | Spec [OK] |
| REQ-CROSS-LOOP-001 | workflow-tests.md → loop-limits.test.ts | Spec [OK] |
| REQ-CROSS-LOOP-002 | workflow-tests.md → loop-limits.test.ts | Spec [OK] |
| REQ-CROSS-LOOP-003 | workflow-tests.md → loop-limits.test.ts | Spec [OK] |
| REQ-CROSS-LOOP-004 | workflow-tests.md → loop-limits.test.ts | Spec [OK] |
| REQ-CROSS-LOCK-001 | workflow-tests.md → concurrent-lock.test.ts | Spec [OK] |
| REQ-CROSS-LOCK-002 | workflow-tests.md → concurrent-lock.test.ts | Spec [OK] |
| REQ-CROSS-LOCK-003 | workflow-tests.md → concurrent-lock.test.ts | Spec [OK] |
| REQ-CROSS-TCONF-001 | workflow-tests.md → target-config.test.ts | Spec [OK] |
| REQ-CROSS-TCONF-002 | workflow-tests.md → target-config.test.ts | Spec [OK] |
| REQ-CROSS-TCONF-003 | workflow-tests.md → target-config.test.ts | Spec [OK] |
| REQ-CROSS-TCONF-004 | workflow-tests.md → target-config.test.ts | Spec [OK] |
| REQ-CROSS-LCONF-001 | workflow-tests.md → luther-config-validation.test.ts | Spec [OK] |
| REQ-CROSS-LCONF-002 | workflow-tests.md → luther-config-validation.test.ts | Spec [OK] |
| REQ-CROSS-LCONF-003 | workflow-tests.md → luther-config-validation.test.ts | Spec [OK] |
| REQ-CROSS-LCONF-004 | workflow-tests.md → luther-config-validation.test.ts | Spec [OK] |
| REQ-CROSS-LCONF-005 | workflow-tests.md → luther-config-validation.test.ts | Spec [OK] |
| REQ-CROSS-LCONF-006 | workflow-tests.md → luther-config-validation.test.ts | Spec [OK] |
| REQ-CROSS-LCONF-007 | workflow-tests.md → luther-config-validation.test.ts | Spec [OK] |
| REQ-CROSS-LCONF-008 | workflow-tests.md → luther-config-validation.test.ts | Spec [OK] |
| REQ-CROSS-LCONF-009 | workflow-tests.md → luther-config-validation.test.ts | Spec [OK] |
| REQ-CROSS-LCONF-010 | workflow-tests.md → luther-config-validation.test.ts | Spec [OK] |
| REQ-CROSS-LCONF-011 | workflow-tests.md → luther-config-validation.test.ts | Spec [OK] |
| REQ-CROSS-LCONF-012 | workflow-tests.md → luther-config-validation.test.ts | Spec [OK] |
| REQ-CROSS-LCONF-013 | workflow-tests.md → luther-config-validation.test.ts | Spec [OK] |
| REQ-CROSS-LCONF-014 | workflow-tests.md → luther-config-validation.test.ts | Spec [OK] |
| REQ-CROSS-LCONF-NEG-001 | workflow-tests.md → luther-config-validation.test.ts | Spec [OK] |
| REQ-CROSS-LCONF-NEG-002 | workflow-tests.md → luther-config-validation.test.ts | Spec [OK] |
| REQ-CROSS-LCONF-NEG-003 | workflow-tests.md → luther-config-validation.test.ts | Spec [OK] |
| REQ-CROSS-LCONF-NEG-004 | workflow-tests.md → luther-config-validation.test.ts | Spec [OK] |
| REQ-CROSS-LCONF-NEG-005 | workflow-tests.md → luther-config-validation.test.ts | Spec [OK] |
| REQ-CROSS-LCONF-NEG-006 | workflow-tests.md → luther-config-validation.test.ts | Spec [OK] |
| REQ-CROSS-LCONF-NEG-007 | workflow-tests.md → luther-config-validation.test.ts | Spec [OK] |
| REQ-CROSS-PROMPT-001 | workflow-tests.md → context-rules.test.ts | Spec [OK] |
| REQ-CROSS-PROMPT-002 | workflow-tests.md → context-rules.test.ts | Spec [OK] |
| REQ-CROSS-PROMPT-003 | workflow-tests.md → context-rules.test.ts | Spec [OK] |
| REQ-CROSS-PROMPT-004 | workflow-tests.md → context-rules.test.ts | Spec [OK] |
| REQ-CROSS-ERR-001 | workflow-tests.md → error-taxonomy.test.ts | Spec [OK] |
| REQ-CROSS-ERR-002 | workflow-tests.md → error-taxonomy.test.ts | Spec [OK] |
| REQ-CROSS-ERR-003 | workflow-tests.md → error-taxonomy.test.ts | Spec [OK] |
| REQ-CROSS-ERR-004 | workflow-tests.md → error-taxonomy.test.ts | Spec [OK] |
| REQ-CROSS-ERR-005 | workflow-tests.md → error-taxonomy.test.ts | Spec [OK] |
| REQ-CROSS-RESET-001 | workflow-tests.md → context-reset.test.ts | Spec [OK] |
| REQ-CROSS-RESET-002 | workflow-tests.md → context-reset.test.ts | Spec [OK] |
| REQ-CROSS-RESET-003 | workflow-tests.md → context-reset.test.ts | Spec [OK] |
| REQ-CROSS-IDEM-001 | workflow-tests.md → idempotency.test.ts | Spec [OK] |
| REQ-CROSS-IDEM-002 | workflow-tests.md → idempotency.test.ts | Spec [OK] |
| REQ-CROSS-IDEM-003 | workflow-tests.md → idempotency.test.ts | Spec [OK] |
| REQ-CROSS-IDEM-004 | workflow-tests.md → idempotency.test.ts | Spec [OK] |
| REQ-CROSS-CANCEL-001 | workflow-tests.md → cancellation.test.ts | Spec [OK] |
| REQ-CROSS-CANCEL-002 | workflow-tests.md → cancellation.test.ts | Spec [OK] |
| REQ-CROSS-CANCEL-003 | workflow-tests.md → cancellation.test.ts | Spec [OK] |
| REQ-CROSS-CLOCK-001 | workflow-tests.md → clock-consistency.test.ts | Spec [OK] |
| REQ-CROSS-CLOCK-002 | workflow-tests.md → clock-consistency.test.ts | Spec [OK] |

### Coverage Summary

| Section | Total REQ-IDs | Spec'd | Excluded | Blocked |
|---|---|---|---|---|
| §1 State Machine | 78 | 78 | 0 | 2 (REQ-SM-009, REQ-SM-011: fix-tests.ts missing) |
| §2 Step Handlers | 130 | 130 | 0 | 6 (REQ-STEP-TFIX-*: fix-tests.ts missing) |
| §3 Libraries | 55 | 55 | 0 | 0 |
| §4 Engine | 47 | 45 | 2 | 7 (REQ-ENG-RETRY-001..007: inline stub) |
| §5 CLI | 9 | 9 | 0 | 0 |
| §6 Contract | 19 | 19 | 0 | 0 |
| §7 Cross-Cutting | 64 | 64 | 0 | 0 |
| **Total** | **402** | **400** | **2** | **15** |

All 400 testable requirements have a behavioral Given/When/Then test spec in this plan. 2 are excluded (REQ-ENG-006, REQ-ENG-022) as non-behavioral design constraints. 15 are BLOCKED (source file does not exist) but have specs ready for when the source is created.
