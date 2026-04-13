# Luther MVP Attempt 1: Salvageable Assets

## Salvageable Logic (Rewrite in Correct Location, Don't Copy-Paste)

### State Machine Topology
The 17 states, 23 events, and 7 guards are architecturally correct. The topology just needs to live in `workflows/fix-issue.json` (actually loaded) and `workflow/machine-setup.ts` (registration), not hardcoded in `engine/machine.ts`.

States: IDLE, SCANNING, PLANNING, REVIEWING, IMPLEMENTING, TESTING, FIX_TESTS, PUSHING, SUBMITTING, WATCHING, DIAGNOSING, TRIAGING, RESPONDING, REMEDIATING, ABANDONING, LOGGING, SUCCESS

Key transitions:
- REVIEWING → PLANNING (PLAN_REVISE + underRevisionLimit) or → ABANDONING
- TESTING → FIX_TESTS (TESTS_FAIL + underTestFixLimit) or → ABANDONING
- PUSHING → SUBMITTING (noPR) or → WATCHING (hasPR)
- DIAGNOSING → REMEDIATING (shouldRemediate) or → WATCHING
- RESPONDING → REMEDIATING (hasActionableItems) or → PUSHING
- LOGGING → IDLE (cameFromAbandoning) or → SUCCESS

### Step Handler Patterns
Each step handler follows a clean pattern that works:
1. Extract deps and domain inputs
2. Build prompt from context (for agent steps)
3. Invoke llxprt or processRunner
4. Parse structured output
5. Write artifacts to output manager
6. Append to session
7. Return typed result

The implementations are correct for: scanIssues, planFix, reviewPlan, implementFix, runTests, fixTests, commitPush, submitPR, watchPRChecks, diagnoseCI, triageCR, respondCR, remediate, abandonPR, logOutcome.

### Generic Engine Components
These two files are genuinely reusable as-is:
- `persistence.ts` (64 lines) — state serialization with schema versioning
- `retry.ts` (64 lines) — exponential backoff with jitter, injectable sleep/rng

### DI Interface Design
These interfaces are well-designed:
- `ProcessRunner { run(command, cwd): Promise<ProcessResult> }`
- `SignalHandler { onShutdown(callback): void }`
- `OutputManager { readJson, readText, writeJson, writeText, clean }`
- `SessionManager { create, appendSection, getContent, clear }`

### Fake Implementations
The test fakes are well-designed behavioral implementations:
- `FakeGithubClient` — in-memory issues, assignments, PRs, comments, PR checks sequences
- `FakeLlxprtClient` — response queue with exit code, stdout, stderr
- `FakeProcessRunner` — result queue with command/cwd recording
- `FakeSignalHandler` — callback registration with triggerShutdown()
- `FakeOutputManager` — in-memory file map with hasFile(), fileContent(), errorOnFile()
- `FakeSessionManager` — in-memory sections with hasSectionWithHeading()

### Lib Layer Implementations
These are correct implementations that just need correct placement:
- `GhClient` — real `gh` CLI command construction with JSON parsing
- `LlxprtSubprocess` — real subprocess invocation with shell escaping
- `FileOutputManager` — real file I/O with directory creation
- `FileSessionManager` — real session file management

### Quality Infrastructure
Fully reusable, no changes needed:
- `eslint.config.ts` — mock bans, complexity limits, no inline config
- `biome.json` — formatting
- `scripts/check-quality.ts` — 15 custom pattern checks
- `scripts/quality-patterns.ts` — pattern definitions
- `tsconfig.json` — strict mode
- `RULES.md` — testing philosophy

## What NOT to Salvage

### The Engine/Workflow Entanglement
Do not try to refactor the existing code. The entanglement is structural — every file in `engine/` (except persistence.ts and retry.ts) would need a near-complete rewrite to separate concerns. Starting fresh with correct file structure is faster and less error-prone than untangling.

### The Tests as Written
The tests import from `src/engine/machine.ts`, `src/engine/runner.ts`, etc. and test workflow behavior through engine paths. Even if the production code is reorganized, the tests would need import path changes AND some tests conflate engine and workflow concerns (e.g., testing Luther-specific state transitions in engine/machine.test.ts). Better to rewrite tests with correct imports and correct boundary awareness.

### The JSON Workflow Definition
`src/workflows/fix-issue.json` exists but was never actually used by the implementation. It may not match the hardcoded TypeScript machine. Don't trust it — regenerate from the architecture document.

## Functional Gaps to Address in Attempt 2

These were missing from the attempt 1 implementation entirely:

1. **Infinite runner loop** — REQ-ENG-022 says run indefinitely; implementation runs once
2. **retryWithBackoff wiring** — retry.ts exists but no step handler uses it
3. **Git workspace hygiene** — no branch creation, no workspace cleanup, no stale branch removal
4. **Proper crash recovery** — resume path sends canned events instead of re-executing steps
5. **JSON-driven machine** — architecture says load from JSON; implementation hardcodes in TypeScript
6. **Polling intervals** — watchPRChecks uses 100ms; architecture says 300s
