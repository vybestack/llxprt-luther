# Luther MVP Attempt 1: Post-Mortem

## What Was Attempted

Build the Luther MVP — an autonomous GitHub issue-fixing agent — using a stub-first, test-driven approach where:

1. Architecture and requirements were written first (~260 requirements in EARS format)
2. All production code was created as stubs throwing "Not yet implemented"
3. ~424 behavioral tests were written as aspirational specs against the stubs
4. A subagent (`fallbacktypescriptcoder`) was dispatched to implement production code to make the tests pass, one phase at a time across 15 phases

The architecture defined a hard boundary: a **generic reusable engine** (loads any JSON workflow, runs transitions, persists state, handles signals) and a **workflow layer** (the Luther-specific fix-issue state machine, step handlers, domain types, GitHub/LLxprt integration).

## Final State

- **414/414 tests passing**, 0 stubs remaining
- All lint, format, and quality checks green (15/15 custom quality patterns pass)
- 15 step handlers implemented, XState v5 state machine with 17 states, full integration tests
- ~3,500 lines of production code across 25 source files

## What Failed: The Engine/Workflow Boundary

The central architectural principle — "the engine knows nothing about GitHub, issues, PRs, CodeRabbit, LLxprt, git, or any domain concept" — was comprehensively violated.

### The Violation in Numbers

| Engine file | Lines | Generic? |
|---|---|---|
| `persistence.ts` | 64 | Yes — clean |
| `retry.ts` | 64 | Yes — clean |
| `types.ts` | 255 | No — ~90% workflow domain types |
| `machine.ts` | 565 | No — 100% Luther state machine |
| `runner.ts` | 452 | No — imports steps, knows domain events |
| `step-dispatch.ts` | 268 | No — 100% workflow dispatch table |
| **Total** | **1,668** | **128 lines generic, 1,540 lines workflow** |

### Specific Violations

1. **`engine/types.ts`** contains `IssueContext`, `PRContext`, `CRComment`, `CRTriageEntry`, `CIDiagnosisResult`, `PlanReview`, `GithubClient`, `LlxprtClient`, `LutherConfig`, `WorkflowContext` — all workflow-specific domain types living in the engine.

2. **`engine/machine.ts`** hardcodes the entire Luther state machine in TypeScript. `loadWorkflowDefinition()` reads a JSON file but returns a dummy branded object. `createLutherMachine()` ignores the config parameter and defines states inline. The JSON definition file exists but is decorative.

3. **`engine/runner.ts`** imports `scanIssues` from `src/steps/` (architecture explicitly says "the engine never imports from src/steps/"). Contains hardcoded `AGENT_STATES` set, `getDefaultEvent()` lookup table with workflow-specific events, and `registerShutdown()` that reaches into `deps.config.github.lutherLogin`.

4. **`engine/step-dispatch.ts`** imports all 15 step handlers and hardcodes which handler runs in which state. This is 100% workflow code living in the engine directory.

### Additional Implementation Gaps

Beyond the boundary violation, there were functional gaps:

- **No infinite runner loop** (REQ-ENG-022) — processes one issue and exits
- **Zero `retryWithBackoff` usage** — retry.ts implemented and tested but never wired into any step handler
- **No git workspace hygiene** — no branch creation, no workspace cleanup
- **Resume path uses canned events** — doesn't re-execute steps on crash recovery
- **Machine recreated on every transition** — reads JSON from disk per state change

## Why It Failed: Root Cause Analysis

### 1. The Boundary Was Broken Before Implementation Started

The initial stub file structure placed all domain types in `engine/types.ts`, the machine stub in `engine/machine.ts`, and the runner stub in `engine/runner.ts`. The tests were written to import from these paths. When the subagent implemented production code, it had to put workflow logic in those files because that's where the tests pointed.

**The tests never tested the boundary.** There was no test saying "engine/machine.ts must not reference SCANNING." No lint rule preventing engine/ from importing steps/. The boundary existed only as prose in the architecture document, not as a mechanically enforced constraint.

### 2. "Make Tests Pass" Optimizes for Behavior, Not Structure

The subagent was given one objective: make the tests pass. It did exactly that. It never asked "should this code live here?" or "does this import violate the architecture?" The tests defined correct *behavior* but not correct *structure*. Any constraint not expressed as a failing test or a lint error was invisible to the implementation agent.

This is the fundamental limitation of test-driven subagent implementation: **tests verify what code does, not where code lives or how it's organized.** Architectural boundaries are structural constraints, not behavioral ones.

### 3. The Tests Themselves Conflated Engine and Workflow

Tests in `test/engine/machine.test.ts` tested Luther-specific transitions (`SCANNING → PLANNING`, `ISSUE_FOUND` events, `underTestFixLimit` guards). Tests in `test/engine/runner.test.ts` tested config validation for `lutherLogin` and `issueLabels`. These are workflow behaviors being tested through engine paths, baking in the boundary violation from the test side.

### 4. No Incremental Boundary Verification

The 15-phase plan verified each phase by running `bun test && bun run lint && bun run check:quality`. None of these checked structural constraints like "engine/ must not import from steps/" or "engine/types.ts must not define IssueContext." The quality check script (check-quality.ts) catches code smells but not architectural violations.

## What Succeeded

Despite the boundary failure, several things worked well:

### The Step Handlers Are Clean
All 15 step handlers in `src/steps/` are properly separated, use DI through the Dependencies interface, produce typed output, and don't reach across boundaries. If the engine/workflow split were fixed, the steps could remain as-is.

### The DI Architecture Works
`ProcessRunner`, `SignalHandler`, `OutputManager`, `SessionManager` — these interfaces enabled full testing with fakes. No mocks anywhere. The fakes (`FakeGithubClient`, `FakeLlxprtClient`, `FakeProcessRunner`, etc.) are genuine in-memory implementations, not mock theater.

### The Quality Infrastructure Is Valuable
- 15 custom quality patterns catching LLM-specific code smells
- ESLint with `noInlineConfig: true`, mock API bans, function size limits
- Biome formatting
- All reusable for attempt 2

### The Behavioral Specs Define Real Contracts
The 424 tests define what Luther should actually do. The assertions are correct even if the code is in the wrong files. Many tests (step handler tests, persistence tests, retry tests) would survive a reorganization with only import path changes.

### Subagent Implementation Was Efficient
15 phases, 414 tests, ~3,500 lines of production code, all passing. The subagent produced real implementations, not fakes. Step handlers actually build prompts, invoke subprocesses, parse structured output, manage sessions. The code would work against real GitHub repos (modulo the gaps noted above).

## Lessons Learned

### 1. Enforce Architecture Mechanically, Not Documentarily

This is directly from the harness research (notes/harness/overview.md, Principle 7): "Enforce architecture mechanically, not through instructions." The architecture document said the engine must not import from steps/. No lint rule enforced it. The boundary was prose, so it was ignored.

**For attempt 2:** Add ESLint `no-restricted-imports` rules or custom lint rules that prevent `engine/` from importing `steps/` or `lib/`. Add a quality check pattern that scans for domain terms in engine files.

### 2. Tests Must Test Structure, Not Just Behavior

Behavioral tests are necessary but not sufficient. Structural tests — "this module must not depend on that module," "these files must not contain these patterns" — are needed to enforce architectural decisions.

**For attempt 2:** Add structural tests (dependency direction tests) alongside behavioral tests. Or use the quality check script to enforce import boundaries.

### 3. File Structure Pre-Commits Architecture

Where you put the stubs determines where the implementation lands. If the stub for the state machine is in `engine/machine.ts`, that's where the subagent will put the implementation. The file structure IS the architecture as far as the implementing agent is concerned.

**For attempt 2:** Get the file structure right before writing stubs. If the engine and workflow must be separate, create separate directories with separate stub files from the start.

### 4. The Subagent Needs Architectural Context, Not Just Tests

The fallbacktypescriptcoder subagent was given phase instructions like "implement machine.ts to make these tests pass." It was never told "engine/ is generic, workflow/ is specific, these are the import rules." The architecture document existed but wasn't part of the subagent prompt.

**For attempt 2:** Include architectural constraints explicitly in subagent prompts. Or better, encode them as lint rules the subagent will encounter as errors.

### 5. Stub-First TDD Works for Behavior, Not for Boundaries

The approach successfully produced correct behavioral implementations. Every step handler does what it should. The state machine transitions correctly. The retry logic works. What it didn't produce was correct code organization. TDD drives implementation through red→green→refactor, but the "refactor" step (where structural cleanup happens) never occurred because the subagent's job was done once tests turned green.

### 6. Review Cycles Caught Code Quality, Not Architecture

Four review→remediate cycles improved test quality from C- to A-, catching weak assertions, duplication, missing coverage, dead code. But no review cycle flagged "the engine imports step handlers" because the reviewer wasn't checking for architectural boundary violations — it was reviewing test quality.

## Connection to Research

This experience validates several findings from the research notes:

- **Harness Principle 7** (harness/overview.md): "Enforce architecture mechanically, not through instructions. Agents replicate patterns that already exist — including bad ones."
- **Harness Principle 2**: "Design the workflow first, then embed intelligence." We designed the workflow but didn't mechanically enforce its boundaries.
- **Self-evolving agents** (evolve/overview.md, §Key Principle 2): "Empirical validation replaces formal proofs." Our tests were the empirical validation, and they didn't validate structure.
- **PLAN.md Integration Analysis**: The llxprt planning system emphasizes "what existing code will USE this feature?" The engine was supposed to be used by any workflow, but because nothing enforced that contract, it became Luther-specific.

## What to Preserve for Attempt 2

### Keep
- `eslint.config.ts` — lint rules, mock bans, complexity limits
- `biome.json` — formatting config
- `tsconfig.json` — strict TypeScript config
- `scripts/check-quality.ts` and `scripts/quality-patterns.ts` — custom quality checks
- `package.json`, `bun.lock`, `bunfig.toml` — project setup
- `RULES.md` — testing philosophy
- `plans/` and `project-plans/` — architecture and requirements documents
- `.gitignore`

### Delete
- `src/` — all production code (boundary violation makes it unsalvageable as-is)
- `test/` — all tests (import paths and some test designs bake in the wrong structure)

### Salvageable Ideas (for reference, not copy-paste)
- The step handler implementations are correct logic — rewrite in correct location
- The state machine topology (states, events, guards) is correct — extract to JSON properly
- The DI interface design (ProcessRunner, SignalHandler, etc.) is sound
- The fake implementations are well-designed
- The retry and persistence implementations are genuinely generic
