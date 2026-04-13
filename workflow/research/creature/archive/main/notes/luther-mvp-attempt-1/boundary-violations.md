# Luther MVP Attempt 1: Engine/Workflow Boundary Violations — Detailed Evidence

## The Architectural Contract

From `plans/mvp/architecture.md`:

> "The engine is a generic JSON workflow runner. It loads any XState JSON definition, registers implementations, runs transitions, persists state, handles process signals, and provides logging infrastructure. **The engine knows nothing about GitHub, issues, PRs, CodeRabbit, LLxprt, git, or any domain concept.**"
>
> "**The engine never imports from `src/steps/`, `src/lib/gh.ts`, or `src/lib/llxprt.ts`.**"

The engine was supposed to provide hooks; the workflow fills them in:

```
Engine provides:                 Workflow provides:
─────────────────               ──────────────────
setup({ actors, guards })  ←    step handler functions, guard functions
persist(context)           ←    WorkflowContext (engine doesn't inspect it)
resume(stateFile)          →    onResume callback for domain validation
retryWithBackoff(fn, opts) ←    step handler wraps its own calls
logger.child({ module })   ←    each step creates a child logger
onShutdown(callback)       ←    workflow registers cleanup
```

## File-by-File Violation Report

### `src/engine/types.ts` (255 lines)

**What should be here (engine-generic):**
- `ProcessRunner`, `ProcessResult` — generic subprocess abstraction
- `SignalHandler`, `SignalName` — generic signal handling
- `OutputManager` — generic file I/O
- `SessionManager` — possibly generic (append-only document builder)
- `Dependencies` bundle type (but only with generic fields)

**What should NOT be here (workflow-specific):**
- `IssueContext` — GitHub issue domain type
- `PRContext` — GitHub PR domain type
- `CRComment`, `CRTriageEntry`, `CRTriageResult` — CodeRabbit domain types
- `CIDiagnosisEntry`, `CIDiagnosisResult`, `CIFailureClassification` — CI diagnosis domain types
- `ReviewVerdict`, `PlanReview` — plan review domain types
- `CheckStatus`, `PRChecksResult`, `WorkflowRun` — PR checks domain types
- `GithubClient` interface (13 methods, all GitHub-specific)
- `LlxprtClient` interface — LLxprt subprocess domain type
- `LlxprtInvokeParams`, `LlxprtResult` — LLxprt domain types
- `CreatePRParams`, `CreateIssueParams` — GitHub domain types
- `LutherConfig`, `LutherSettings`, `GithubSettings`, `IssueLabels` — Luther config domain types
- `TargetConfig`, `ProfileMap` — target repo domain types
- `WorkflowContext` with `issue`, `pr`, `crTriage`, `ciDiagnosis` fields
- `HistoryEntry` — workflow history tracking

**Violation severity:** ~90% of this file is workflow-specific. Only `ProcessRunner`, `SignalHandler`, `OutputManager`, and the generic portions of `SessionManager` belong in the engine.

### `src/engine/machine.ts` (565 lines)

**What it should do:** Load a JSON definition, validate it structurally (has `id`, `initial`, `states`), and provide a generic `createMachine()` that wires caller-provided actor and guard implementations into the JSON-defined topology.

**What it actually does:** Defines the entire Luther state machine in TypeScript — 17 state constructors, 23 event types, 7 guard implementations, context mutation actions — all hardcoded.

Specific violations:
- `WorkflowEvent` union type: `ISSUE_FOUND`, `PLAN_READY`, `PR_CREATED`, `CR_COMMENTS`, `TRIAGE_DONE`, `DIAG_DONE`, `REMED_DONE`, `ABANDON_DONE` — all workflow domain events
- `createScanningState()`, `createPlanningState()`, `createReviewingState()`, etc. — 15 workflow state factories
- Guards: `underRevisionLimit`, `underTestFixLimit`, `noPR`, `hasPR`, `shouldRemediate`, `hasActionableItems`, `cameFromAbandoning` — all workflow domain logic
- Context mutations: `issue`, `branch`, `pr`, `loopCount`, `testFixAttempts`, `crTriage`, `ciDiagnosis` — all workflow context fields
- `loadWorkflowDefinition()` returns `{ __brand: "MachineConfig" }` — the JSON config is read but ignored
- `createLutherMachine()` parameter is `_config: MachineConfig` (underscore = unused)

**Violation severity:** 100%. The entire file is workflow code. A generic engine's machine.ts would be ~50 lines (load JSON, validate structure, return typed definition).

### `src/engine/runner.ts` (452 lines)

**Violations:**

1. Direct step import:
   ```typescript
   import { scanIssues } from "../steps/scan-issues.js";
   ```
   Architecture: "The engine never imports from src/steps/"

2. Hardcoded agent state knowledge:
   ```typescript
   const AGENT_STATES = new Set([
     "PLANNING", "REVIEWING", "IMPLEMENTING", "FIX_TESTS",
     "DIAGNOSING", "TRIAGING", "REMEDIATING",
   ]);
   ```
   A generic engine wouldn't know which states are "agent" states.

3. Domain event knowledge in `getDefaultEvent()`:
   ```typescript
   SCANNING: () => ({ type: "ISSUE_FOUND", data: { issue: context.issue } }),
   SUBMITTING: () => ({ type: "PR_CREATED", data: { pr: context.pr } }),
   WATCHING: () => ({ type: "CHECKS_PASS" }),
   ```
   The engine hardcodes which event to send for each workflow state on resume.

4. Domain knowledge in `registerShutdown()`:
   ```typescript
   const { lutherLogin } = deps.config.github;
   const issueNum = state.context.issue?.number ?? 0;
   await deps.github.unassignIssue(owner, name, issueNum, lutherLogin);
   ```
   Engine performing GitHub issue unassignment during shutdown.

5. Step dispatch imports:
   ```typescript
   import { dispatchAbandoning, dispatchLogging, dispatchStep, mapStepResultToEvent }
     from "./step-dispatch.js";
   ```
   These are workflow-specific dispatch functions.

6. Test-environment detection:
   ```typescript
   const output = deps.output as OutputManager & { files?: Map<string, string> };
   if (output.files instanceof Map) {
   ```
   Production code inspecting test fake internals.

**Violation severity:** ~80%. Config loading/validation and the basic loop structure are engine concerns. Everything else is workflow-specific.

### `src/engine/step-dispatch.ts` (268 lines)

```typescript
import { abandonPR } from "../steps/abandon-pr.js";
import { commitPush } from "../steps/commit-push.js";
import { diagnoseCI } from "../steps/diagnose-ci.js";
import { fixTests } from "../steps/fix-tests.js";
import { implementFix } from "../steps/implement-fix.js";
import { logOutcome } from "../steps/log-outcome.js";
import { planFix } from "../steps/plan-fix.js";
import { remediate } from "../steps/remediate.js";
import { respondCR } from "../steps/respond-cr.js";
import { reviewPlan } from "../steps/review-plan.js";
import { runTests } from "../steps/run-tests.js";
import { scanIssues } from "../steps/scan-issues.js";
import { submitPR } from "../steps/submit-pr.js";
import { triageCR } from "../steps/triage-cr.js";
import { watchPRChecks } from "../steps/watch-pr-checks.js";
```

**Violation severity:** 100%. This file should not exist in the engine. It's the workflow's step registration — the engine should accept step handlers as configuration, not import them.

## What a Correct Engine Would Look Like

A truly generic engine would expose approximately:

```typescript
// engine/types.ts — ONLY generic types
interface EngineConfig {
  workflowPath: string;
  statePath: string;
  schemaVersion: string;
}

interface StepHandler {
  (context: unknown): Promise<StepResult>;
}

interface GuardHandler {
  (context: unknown): boolean;
}

interface WorkflowRegistration {
  actors: Record<string, StepHandler>;
  guards: Record<string, GuardHandler>;
  onResume?: (context: unknown) => Promise<boolean>;
  onShutdown?: (context: unknown) => Promise<void>;
}

// engine/machine.ts — ONLY generic loading
function loadDefinition(path: string): JsonMachineConfig;
function createMachine(def: JsonMachineConfig, reg: WorkflowRegistration): AnyStateMachine;

// engine/runner.ts — ONLY generic loop
function runEngine(config: EngineConfig, registration: WorkflowRegistration): Promise<void>;

// engine/persistence.ts — unchanged (already generic)
// engine/retry.ts — unchanged (already generic)
```

Then the workflow layer would provide:

```typescript
// workflow/types.ts — ALL domain types (IssueContext, PRContext, etc.)
// workflow/machine-config.ts — registration of actors and guards
// workflow/step-dispatch.ts — dispatch table mapping states to step handlers
// workflow/runner.ts — calls engine with workflow-specific registration
```

## Import Direction Rules (for attempt 2)

```
ALLOWED:
  workflow/ → engine/     (workflow uses engine)
  workflow/ → steps/      (workflow dispatches steps)
  workflow/ → lib/        (workflow uses lib services)
  steps/   → lib/        (steps use lib services)
  steps/   → engine/types (steps use generic types like ProcessRunner)

FORBIDDEN:
  engine/  → workflow/    (engine must not know about workflow)
  engine/  → steps/       (engine must not know about steps)
  engine/  → lib/gh.ts    (engine must not know about GitHub)
  engine/  → lib/llxprt.ts (engine must not know about LLxprt)
```

These rules must be enforced by lint, not by documentation.
