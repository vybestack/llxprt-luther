# Phase 08: engine/machine.ts — State Machine Wiring

## Scope

Implement `loadWorkflowDefinition` and `createLutherMachine` in `src/engine/machine.ts`.

## Dependencies

- Phase 01 (types fixup — AbandonPROutput.reason)

## Requirements Covered

- REQ-ENG-001: Load workflow definition from JSON
- REQ-ENG-002: Validate definition against expected type
- REQ-ENG-003: Exit with error on invalid definition
- REQ-ENG-004: Register step handlers via `setup()` actors
- REQ-ENG-005: Register guards via `setup()` guards
- REQ-ENG-006: JSON defines topology; executable logic in TypeScript

## Critical Design Decision: JSON vs. TypeScript Machine

The workflow JSON (`fix-issue.json`) uses XState's `invoke` pattern with `onDone`/`onError` callbacks. However, the machine tests send explicit events like `WAKE`, `ISSUE_FOUND`, `PLAN_READY`, `TESTS_FAIL`, etc. via `actor.send({ type: "EVENT" })`.

**The tests define the contract.** The machine must be an XState v5 state machine that:
1. Uses UPPERCASE state names: `IDLE`, `SCANNING`, `PLANNING`, `REVIEWING`, `IMPLEMENTING`, `TESTING`, `FIX_TESTS`, `PUSHING`, `SUBMITTING`, `WATCHING`, `DIAGNOSING`, `TRIAGING`, `RESPONDING`, `REMEDIATING`, `ABANDONING`, `LOGGING`, `SUCCESS`
2. Accepts explicit events: `WAKE`, `ISSUE_FOUND`, `NO_ISSUES`, `PLAN_READY`, `PLAN_APPROVED`, `PLAN_REVISE`, `IMPL_DONE`, `TESTS_PASS`, `TESTS_FAIL`, `FIX_APPLIED`, `PUSH_DONE`, `PR_CREATED`, `CHECKS_PASS`, `CHECKS_FAIL`, `CR_COMMENTS`, `TIMEOUT`, `TRIAGE_DONE`, `DIAG_DONE`, `RESPOND_DONE`, `REMED_DONE`, `ABANDON_DONE`, `LOG_DONE`, `ERROR`
3. Uses guards that inspect `WorkflowContext`
4. Has context mutations via `assign()` actions
5. `SUCCESS` is a terminal state (`type: "final"`)

The JSON file is a design artifact — `loadWorkflowDefinition` validates it but `createLutherMachine` builds the actual XState machine programmatically using the TypeScript `setup()` API.

## loadWorkflowDefinition(path)

### From test expectations:
- `loadWorkflowDefinition("workflow.json")` → returns `MachineConfig` with `__brand: "MachineConfig"`
- Invalid/missing file → throws
- Malformed JSON → throws

### Implementation:
1. Resolve path relative to `src/workflows/` (or use a known path resolution strategy)
2. Read the JSON file
3. Validate it has required structure (`id`, `initial`, `states`)
4. Return a branded `MachineConfig` object: `{ ...parsed, __brand: "MachineConfig" as const }`

The actual state machine topology is defined in TypeScript, not loaded from JSON. The JSON serves as a metadata/validation reference.

## createLutherMachine(config, deps)

### Full State Machine Definition

Build using XState v5 `setup()` API:

```typescript
import { setup, assign, type AnyStateMachine } from "xstate";

export function createLutherMachine(config: MachineConfig, deps: Dependencies): AnyStateMachine {
  return setup({
    types: {} as {
      context: WorkflowContext;
      events: /* union of all event types */;
    },
    guards: { /* all guard implementations */ },
    actions: { /* context mutation actions */ },
  }).createMachine({
    id: "fix-issue",
    initial: "IDLE",
    context: { /* initial context */ },
    states: { /* all states */ },
  });
}
```

### States, Events, and Transitions (from tests)

| From | Event | Guard | To | Context Mutation |
|------|-------|-------|----|------------------|
| IDLE | WAKE | — | SCANNING | Reset all: issue→null, branch→null, pr→null, loopCount→0, testFixAttempts→0, crTriage→null, ciDiagnosis→null, history→[]. Retain: maxLoops, maxTestFixAttempts |
| SCANNING | ISSUE_FOUND | — | PLANNING | Set `issue` from event data |
| SCANNING | NO_ISSUES | — | IDLE | — |
| PLANNING | PLAN_READY | — | REVIEWING | Set `branch` from event data; increment `loopCount` |
| REVIEWING | PLAN_APPROVED | — | IMPLEMENTING | — |
| REVIEWING | PLAN_REVISE | loopCount < maxLoops | PLANNING | — |
| REVIEWING | PLAN_REVISE | loopCount >= maxLoops | ABANDONING | — |
| IMPLEMENTING | IMPL_DONE | — | TESTING | — |
| TESTING | TESTS_PASS | — | PUSHING | — |
| TESTING | TESTS_FAIL | testFixAttempts < maxTestFixAttempts | FIX_TESTS | Increment `testFixAttempts` |
| TESTING | TESTS_FAIL | testFixAttempts >= maxTestFixAttempts | ABANDONING | — |
| FIX_TESTS | FIX_APPLIED | — | TESTING | — |
| PUSHING | PUSH_DONE | pr === null | SUBMITTING | — |
| PUSHING | PUSH_DONE | pr !== null | WATCHING | — |
| SUBMITTING | PR_CREATED | — | WATCHING | Set `pr` from event data |
| WATCHING | CHECKS_PASS | — | LOGGING | — |
| WATCHING | CHECKS_FAIL | — | DIAGNOSING | — |
| WATCHING | CR_COMMENTS | — | TRIAGING | — |
| WATCHING | TIMEOUT | — | ABANDONING | — |
| DIAGNOSING | DIAG_DONE | nextAction=retry_ci (guard) | RETRYING? / WATCHING | Set `ciDiagnosis` from event data |
| TRIAGING | TRIAGE_DONE | — | RESPONDING | Set `crTriage` from event data |
| RESPONDING | RESPOND_DONE | hasActionableItems=true (guard) | REMEDIATING | — |
| RESPONDING | RESPOND_DONE | hasActionableItems=false | ??? | — |
| REMEDIATING | REMED_DONE | — | TESTING | — |
| ABANDONING | ABANDON_DONE | — | LOGGING | — |
| LOGGING | LOG_DONE | came from ABANDONING (guard) | IDLE | — |
| LOGGING | LOG_DONE | else | SUCCESS | — |
| All active | ERROR | — | ABANDONING | — |
| SUCCESS | — | — | (final) | — |

### Guards (from machine-topology tests)

```typescript
guards: {
  // REQ-SM-G001: allows plan revision
  underRevisionLimit: ({ context }) => context.loopCount < context.maxLoops,
  // REQ-SM-G002: allows test fix retry
  underTestFixLimit: ({ context }) => context.testFixAttempts < context.maxTestFixAttempts,
  // REQ-SM-G003: routes PUSH_DONE to SUBMITTING when no PR yet
  noPR: ({ context }) => context.pr === null,
  // REQ-SM-G004: routes PUSH_DONE to WATCHING when PR exists
  hasPR: ({ context }) => context.pr !== null,
  // REQ-SM-G005: routes DIAG_DONE based on nextAction
  shouldRemediate: ({ event }) => event.data?.nextAction === "remediate",
  // REQ-SM-G006: routes RESPOND_DONE based on hasActionableItems
  hasActionableItems: ({ event }) => event.data?.hasActionableItems === true,
  // LOG_DONE routing
  cameFromAbandoning: ({ context }) =>
    context.history.some(h => h.state === "ABANDONING"),
}
```

### Context Mutations (via assign)

Every transition appends to history (REQ-SM-CTX-008):
```typescript
const appendHistory = assign({
  history: ({ context, event }) => {
    const entry = { state: /* current state */, timestamp: new Date().toISOString(), event: event.type };
    const newHistory = [...context.history, entry];
    return newHistory.slice(-200); // REQ-SM-HIST-003: truncate to 200
  }
});
```

Event-specific mutations:
- `ISSUE_FOUND`: `assign({ issue: ({ event }) => event.data.issue })`
- `PLAN_READY`: `assign({ branch: ({ event }) => event.data.branch })`
- `PR_CREATED`: `assign({ pr: ({ event }) => event.data.pr })`
- `TRIAGE_DONE`: `assign({ crTriage: ({ event }) => event.data.triage })`
- `DIAG_DONE`: `assign({ ciDiagnosis: ({ event }) => event.data.diagnosis })`
- Entering REVIEWING: `assign({ loopCount: ({ context }) => context.loopCount + 1 })`
- Entering FIX_TESTS: `assign({ testFixAttempts: ({ context }) => context.testFixAttempts + 1 })`
- `WAKE`: Reset all nullable fields, counters to 0, history to empty

### Initial Context
```typescript
context: {
  issue: null,
  branch: null,
  pr: null,
  loopCount: 0,
  maxLoops: 10,
  testFixAttempts: 0,
  maxTestFixAttempts: 3,
  crTriage: null,
  ciDiagnosis: null,
  history: [],
}
```

### SUCCESS Terminal State
```typescript
SUCCESS: { type: "final" }
```
Test: `actor.getSnapshot().status === "done"` (REQ-SM-TERM-001)

### ERROR Handling
Every active state (SCANNING through REMEDIATING — 12 states) must accept `ERROR` event and transition to ABANDONING (REQ-SM-CLEANUP-003).

### Helper: State Name ↔ JSON Name Mapping
The workflow JSON uses camelCase names. The machine uses UPPERCASE. If `loadWorkflowDefinition` is used for anything beyond validation, maintain a mapping. But since the machine is defined programmatically, this is just documentation:

| Machine State | JSON State |
|--------------|------------|
| IDLE | idle |
| SCANNING | scanning |
| PLANNING | planning |
| REVIEWING | reviewing |
| IMPLEMENTING | implementing |
| TESTING | localTesting |
| FIX_TESTS | fixingTests |
| PUSHING | pushing |
| SUBMITTING | submitting |
| WATCHING | prWatching |
| DIAGNOSING | diagnosingCI |
| TRIAGING | triagingCR |
| RESPONDING | respondingCR |
| REMEDIATING | remediating |
| ABANDONING | abandoning |
| LOGGING | (not in JSON — dedicated state for outcome logging) |
| SUCCESS | succeeded |

## Test Files

- `test/engine/machine.test.ts` — 30+ tests: loadWorkflowDefinition, createLutherMachine, all transitions, guards, error handling, WATCHING precedence
- `test/engine/machine-topology.test.ts` — 40+ tests: guards at boundaries, context invariants, mutations, resets, history policy, terminal semantics, cleanup/error precedence

## Verification

```bash
cd llxprt-luther
bun test test/engine/machine.test.ts
bun test test/engine/machine-topology.test.ts
bun test  # confirm no regression
bun run lint
bun run format:check
bun run check:quality
```

Expected: ~70 new tests pass (these are the biggest test files).

## Magnitude

~200-300 lines. This is the largest single file. Expect to extract helper functions to stay under 50 lines per function. Consider splitting:
- `createStates()` → returns the states config object
- `createGuards()` → returns the guards map
- `createActions()` → returns the assign actions
