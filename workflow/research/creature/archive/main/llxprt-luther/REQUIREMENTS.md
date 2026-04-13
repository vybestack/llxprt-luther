# Luther Test Requirements

Canonical source of truth for all testable behaviors in `llxprt-luther`. Every
requirement has a unique ID, a description of **what** the system does (not how),
and a coverage status indicating whether an existing test covers it.

This document is a pure normative spec. Test infrastructure documentation and
priority recommendations belong in separate files.

**Coverage legend:**

- `[COVERED]` — an existing test exercises this behavior and fully asserts the
  requirement's contract (file name and exact test name cited)
- `[PARTIAL]` — an existing test exercises this behavior but does not fully
  assert every aspect of the requirement's contract (file name and test name
  cited, gap noted)
- `[GAP]` — no test exists yet
- `[BLOCKED]` — requirement cannot be tested because the source file does not
  exist (dependency missing), or the source is specified against the expected
  interface only (test uses an inline stub; source file absent)

**Terminology:**

- **CR comments** — review comments left by CodeRabbit on a PR
- **ABANDONING** — a machine state in which the engine is performing
  abandonment cleanup (close PR, unassign, label)
- **ABANDONED** — a `finalState` outcome value (not a machine state) recorded
  after ABANDONING completes and is logged. ABANDONED is never a node in the
  state machine graph; it exists only as a parameter to `logOutcome` and in
  the outcome log. `finalState` is NOT a field on `WorkflowContext`; it is
  passed as a parameter to `logOutcome` (see §2.15).
- **loopCount** — incremented once per PLANNING entry that follows a REVISE
  event (i.e., one "plan-review cycle"). It is NOT incremented for other loop
  types (CI/CR remediation, test fixes). Plan-review cycles and remediation
  cycles use independent counters.
- **testFixAttempts** — incremented once per FIX_TESTS entry
- **transient file** (OutputManager) — any file written by step handlers during
  a single issue run (plan.md, implementation-notes.md, review JSON, etc.).
  Transient files are removed by `clean()`.
- **persistent file** (OutputManager) — files that survive `clean()`: the JSONL
  outcome log (`logs/outcomes.jsonl`) and the persisted state file. These are
  explicitly excluded from cleanup.
- **events** — string literals emitted by step handler return values (e.g.,
  `ISSUE_FOUND`, `PLAN_READY`, `CHECKS_PASS`). Events are not declared as a
  union type in `types.ts`; they are implicit string constants used by the
  XState machine configuration. A declared event union type may be added in a
  future iteration.
- **finalState** — a string parameter (e.g., `"SUCCESS"`, `"ABANDONED"`)
  passed to the `logOutcome` step handler. It is NOT a field on
  `WorkflowContext` in `types.ts`. Step handlers that need to record an
  outcome receive `finalState` as a call parameter.

**Artifact naming convention:** Requirements reference default artifact names
(e.g., plan output, review output, outcome log). These are the default names
used by the `OutputManager`; the actual paths are determined by the
`OutputManager` implementation and output directory configuration. Requirements
specify behavior in terms of "writes a plan to the output directory" or "appends
a JSONL line to the outcome log" — the concrete file names are defaults, not
hard constraints.

**Session heading names:** The per-step heading names listed in §2 are
prescriptive defaults. Step handlers MUST use the exact heading strings listed
unless a future configuration mechanism overrides them. Tests should assert the
exact strings.

---

## 1. State Machine

The workflow engine is an XState v5 state machine. Each state corresponds to a
step handler. Transitions are driven by events emitted by step handler results.

### 1.1 States

| State | Step Handler | Description |
| --- | --- | --- |
| SCANNING | scan-issues | Poll GitHub for unassigned, unfiltered issues |
| PLANNING | plan-fix | Generate implementation plan via LLxprt |
| REVIEWING | review-plan | Peer-review the plan via LLxprt |
| IMPLEMENTING | implement-fix | Execute the plan via LLxprt |
| TESTING | run-tests | Run project test/lint/format/build suite |
| PUSHING | commit-push | Commit changes and push branch |
| SUBMITTING | submit-pr | Open a pull request on GitHub |
| WATCHING | watch-pr-checks | Poll CI checks and CR comments |
| DIAGNOSING | diagnose-ci | Classify CI failures |
| TRIAGING | triage-cr | Classify CR comments |
| RESPONDING | respond-cr | Reply to and resolve CR threads |
| REMEDIATING | remediate | Apply fixes for CI/CR issues via LLxprt |
| FIX_TESTS | fix-tests | Fix failing tests specifically (`[BLOCKED]` — `src/steps/fix-tests.ts` does not exist) |
| ABANDONING | abandon-pr | Close PR, unassign issue, label |
| LOGGING | log-outcome | Record structured outcome to JSONL |
| SUCCESS | (terminal) | Workflow completed successfully; process exits with code 0 |
| IDLE | (loop entry) | No issues found; sleeps for `idleSleepSeconds`, then transitions back to SCANNING via WAKE |

**Note:** ABANDONED is NOT a machine state. It is a `finalState` value passed
as a parameter to `logOutcome` after the ABANDONING state completes. See
terminology section for details.

### 1.2 Transitions

Each row is a directed edge in the state machine. The WATCHING state emits
exactly one event per evaluation cycle, selected according to the precedence
rules in §1.4. Transition rows for WATCHING specify only the emitted event and
target — the logic for choosing which event is emitted lives exclusively in
§1.4.

| ID | Source | Event | Target | Guard | Coverage |
| --- | --- | --- | --- | --- | --- |
| REQ-SM-001 | SCANNING | ISSUE_FOUND | PLANNING | -- | `[GAP]` |
| REQ-SM-002 | SCANNING | NO_ISSUES | IDLE | -- | `[GAP]` |
| REQ-SM-003 | PLANNING | PLAN_READY | REVIEWING | -- | `[GAP]` |
| REQ-SM-004 | REVIEWING | PLAN_APPROVED | IMPLEMENTING | verdict = APPROVED | `[GAP]` |
| REQ-SM-005 | REVIEWING | PLAN_REVISE | PLANNING | verdict = REVISE, loopCount < maxLoops | `[GAP]` |
| REQ-SM-006 | REVIEWING | PLAN_REVISE | ABANDONING | verdict = REVISE, loopCount >= maxLoops | `[GAP]` |
| REQ-SM-007 | IMPLEMENTING | IMPL_DONE | TESTING | -- | `[GAP]` |
| REQ-SM-008 | TESTING | TESTS_PASS | PUSHING | -- | `[GAP]` |
| REQ-SM-009 | TESTING | TESTS_FAIL | FIX_TESTS | testFixAttempts < maxTestFixAttempts | `[BLOCKED]` — `src/steps/fix-tests.ts` missing |
| REQ-SM-010 | TESTING | TESTS_FAIL | ABANDONING | testFixAttempts >= maxTestFixAttempts | `[GAP]` |
| REQ-SM-011 | FIX_TESTS | FIX_APPLIED | TESTING | -- | `[BLOCKED]` — `src/steps/fix-tests.ts` missing |
| REQ-SM-012 | PUSHING | PUSH_DONE | SUBMITTING | pr is null (first push) | `[GAP]` |
| REQ-SM-013 | PUSHING | PUSH_DONE | WATCHING | pr is not null (subsequent push) | `[GAP]` |
| REQ-SM-014 | SUBMITTING | PR_CREATED | WATCHING | -- | `[GAP]` |
| REQ-SM-015 | WATCHING | CHECKS_PASS | LOGGING | -- (emitted only when §1.4 selects CHECKS_PASS) | `[GAP]` |
| REQ-SM-016 | WATCHING | CHECKS_FAIL | DIAGNOSING | -- (emitted only when §1.4 selects CHECKS_FAIL) | `[GAP]` |
| REQ-SM-017 | WATCHING | CR_COMMENTS | TRIAGING | -- (emitted only when §1.4 selects CR_COMMENTS) | `[GAP]` |
| REQ-SM-018 | DIAGNOSING | DIAG_DONE | REMEDIATING | has PR_RELATED failures | `[GAP]` |
| REQ-SM-019 | DIAGNOSING | DIAG_DONE | WATCHING | only INFRA/FLAKY failures (retrigger and wait) | `[GAP]` |
| REQ-SM-020 | DIAGNOSING | DIAG_DONE | ABANDONING | loopCount >= maxLoops | `[GAP]` |
| REQ-SM-021 | TRIAGING | TRIAGE_DONE | RESPONDING | -- | `[GAP]` |
| REQ-SM-022 | RESPONDING | RESPOND_DONE | REMEDIATING | has IN_SCOPE or OPPORTUNITY items | `[GAP]` |
| REQ-SM-023 | RESPONDING | RESPOND_DONE | WATCHING | only OUT_OF_SCOPE/INVALID items | `[GAP]` |
| REQ-SM-024 | REMEDIATING | REMED_DONE | TESTING | -- | `[GAP]` |
| REQ-SM-025 | ABANDONING | ABANDON_DONE | LOGGING | -- | `[GAP]` |
| REQ-SM-026 | LOGGING | LOG_DONE | SUCCESS | finalState = SUCCESS (passed as parameter to logOutcome) | `[GAP]` |
| REQ-SM-027 | LOGGING | LOG_DONE | IDLE | finalState = ABANDONED (passed as parameter to logOutcome) | `[GAP]` |
| REQ-SM-028 | * (any non-terminal, excluding ABANDONING and LOGGING) | ERROR | ABANDONING | unhandled error in any step (fatal) | `[GAP]` |
| REQ-SM-029 | IDLE | WAKE | SCANNING | after idle sleep timer | `[GAP]` |
| REQ-SM-030 | WATCHING | TIMEOUT | ABANDONING | -- (emitted only when §1.4 selects TIMEOUT) | `[GAP]` |

### 1.3 Recoverable vs Fatal Step Errors

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-SM-ERR-001 | A step that throws a retryable error (e.g., transient network failure) retries per the retry policy before escalating | `[GAP]` |
| REQ-SM-ERR-002 | A step that throws a fatal error (e.g., LLxprt non-zero exit, schema validation failure) transitions directly to ABANDONING without retry | `[GAP]` |
| REQ-SM-ERR-003 | A step that throws an unrecognized error type transitions to ABANDONING (the catch-all from REQ-SM-028) | `[GAP]` |

### 1.4 WATCHING Event Precedence (Canonical)

This section is the single canonical definition of how the WATCHING step
determines which event to emit. WATCHING always emits exactly one event per
evaluation cycle. The event is selected by evaluating conditions in descending
precedence order and emitting the first match.

All transition rows in §1.2 for WATCHING are targets only — the event
selection logic is defined here exclusively.

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-SM-PREC-001 | WATCHING emits exactly one event per evaluation cycle | `[GAP]` |
| REQ-SM-PREC-002 | Precedence order (highest to lowest): CHECKS_FAIL > CR_COMMENTS > CHECKS_PASS > TIMEOUT | `[GAP]` |
| REQ-SM-PREC-003 | CHECKS_FAIL: emitted when any CI check has failed status (regardless of CR comments) — transitions to DIAGNOSING | `[GAP]` |
| REQ-SM-PREC-004 | CR_COMMENTS: emitted when all CI checks pass AND unresolved CR comments exist — transitions to TRIAGING | `[GAP]` |
| REQ-SM-PREC-005 | CHECKS_PASS: emitted when all CI checks pass AND no unresolved CR comments exist — transitions to LOGGING | `[GAP]` |
| REQ-SM-PREC-006 | TIMEOUT: emitted when the CR wait timeout is exceeded before conditions settle — transitions to ABANDONING | `[GAP]` |

### 1.5 Guards

Guard conditions used by the state machine. Guard behavior (the boolean
condition) is normative; symbol names used in source code are implementation
details outside the scope of this document.

| ID | Condition | Coverage |
| --- | --- | --- |
| REQ-SM-G001 | `loopCount < maxLoops` (allows plan revision) | `[GAP]` |
| REQ-SM-G002 | `testFixAttempts < maxTestFixAttempts` (allows test fix retry) | `[GAP]` |
| REQ-SM-G003 | `context.pr === null` (first push, route to SUBMITTING) | `[GAP]` |
| REQ-SM-G004 | `context.pr !== null` (subsequent push, route to WATCHING) | `[GAP]` |
| REQ-SM-G005 | `diagnosis.failures.some(f => f.classification === "PR_RELATED")` (has PR-related failures) | `[GAP]` |
| REQ-SM-G006 | `triage has IN_SCOPE or OPPORTUNITY comments` (has actionable comments) | `[GAP]` |
| REQ-SM-G007 | `loopCount >= maxLoops` (loop limit reached, abandon) | `[GAP]` |
| REQ-SM-G008 | `testFixAttempts >= maxTestFixAttempts` (test fix limit reached, abandon) | `[GAP]` |

### 1.6 Context Invariants

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-SM-INV-001 | `context.pr` must be non-null before entering WATCHING (enforced by the SUBMITTING→WATCHING and PUSHING→WATCHING transitions) | `[GAP]` |
| REQ-SM-INV-002 | `context.issue` must be non-null before entering PLANNING | `[GAP]` |
| REQ-SM-INV-003 | `context.branch` must be non-null before entering PUSHING | `[GAP]` |
| REQ-SM-INV-004 | `context.crTriage` must be non-null before entering RESPONDING | `[GAP]` |

### 1.7 Context Mutations

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-SM-CTX-001 | SCANNING sets `context.issue` to the selected issue | `[GAP]` |
| REQ-SM-CTX-002 | PLANNING sets `context.branch` to the computed branch name | `[GAP]` |
| REQ-SM-CTX-003 | SUBMITTING sets `context.pr` to the created PR | `[GAP]` |
| REQ-SM-CTX-004 | `context.loopCount` is incremented by 1 each time the machine enters PLANNING via a PLAN_REVISE event (one plan-review cycle = one increment) | `[GAP]` |
| REQ-SM-CTX-005 | `context.testFixAttempts` is incremented by 1 each time the machine enters FIX_TESTS | `[GAP]` |
| REQ-SM-CTX-006 | TRIAGING sets `context.crTriage` | `[GAP]` |
| REQ-SM-CTX-007 | DIAGNOSING sets `context.ciDiagnosis` | `[GAP]` |
| REQ-SM-CTX-008 | Every state transition appends to `context.history` | `[GAP]` |

### 1.8 Context Reset

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-SM-RST-001 | When starting a new issue (entering SCANNING from IDLE), `issue` is reset to `null` | `[GAP]` |
| REQ-SM-RST-002 | When starting a new issue (entering SCANNING from IDLE), `branch` is reset to `null` | `[GAP]` |
| REQ-SM-RST-003 | When starting a new issue (entering SCANNING from IDLE), `pr` is reset to `null` | `[GAP]` |
| REQ-SM-RST-004 | When starting a new issue (entering SCANNING from IDLE), `loopCount` is reset to `0` | `[GAP]` |
| REQ-SM-RST-005 | When starting a new issue (entering SCANNING from IDLE), `testFixAttempts` is reset to `0` | `[GAP]` |
| REQ-SM-RST-006 | When starting a new issue (entering SCANNING from IDLE), `crTriage` is reset to `null` | `[GAP]` |
| REQ-SM-RST-007 | When starting a new issue (entering SCANNING from IDLE), `ciDiagnosis` is reset to `null` | `[GAP]` |
| REQ-SM-RST-008 | `context.history` is cleared at the start of each new issue run | `[GAP]` |
| REQ-SM-RST-009 | `maxLoops` and `maxTestFixAttempts` retain their configured values across resets | `[GAP]` |

### 1.9 History Policy

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-SM-HIST-001 | Each `HistoryEntry` records `{ state, timestamp, event }` as defined in `types.ts` | `[GAP]` |
| REQ-SM-HIST-002 | History entries are appended in chronological order | `[GAP]` |
| REQ-SM-HIST-003 | History is truncated to the most recent 200 entries to bound serialized state size | `[GAP]` |

### 1.10 Terminal and Loop Semantics

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-SM-TERM-001 | SUCCESS is a terminal state: when reached, the process exits with code 0 | `[GAP]` |
| REQ-SM-TERM-002 | IDLE is a loop-entry state: when reached, the engine sleeps for `idleSleepSeconds` then emits WAKE to transition back to SCANNING | `[GAP]` |
| REQ-SM-TERM-003 | The IDLE→SCANNING transition resets all mutable context (per §1.8) before scanning | `[GAP]` |

### 1.11 Cleanup Error Precedence

Errors during ABANDONING and LOGGING are handled specially to prevent infinite
loops. The precedence rules below are definitive:

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-SM-CLEANUP-001 | If `abandonPR` throws during ABANDONING, the error is logged (pino) but the engine still transitions to LOGGING with `finalState = ABANDONED` (passed as parameter) | `[GAP]` |
| REQ-SM-CLEANUP-002 | If `logOutcome` throws during LOGGING, the error is logged to stderr via pino and the engine transitions to the next state (IDLE or SUCCESS) without retrying | `[GAP]` |
| REQ-SM-CLEANUP-003 | The global ERROR→ABANDONING rule (REQ-SM-028) excludes ABANDONING and LOGGING states — errors in these states never cause a recursive transition to ABANDONING | `[GAP]` |
| REQ-SM-CLEANUP-004 | Precedence order: ABANDONING error → log and continue to LOGGING; LOGGING error → log and continue to IDLE/SUCCESS. No state re-enters itself on error. | `[GAP]` |

---

## 2. Step Handlers

### Cross-cutting step behavior: Session Append

Every step handler that produces output appends a section to the session via
`session.appendSection(heading, content)`. Rather than repeat this in every
step table, this is the canonical requirement:

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-STEP-SESSION-001 | Every step handler that produces a result appends a descriptive section to the session (see per-step heading names below) | `[GAP]` |

Per-step heading names (these are prescriptive — tests MUST assert these exact
strings):

| Step | Section Heading |
| --- | --- |
| scan-issues | "Issue Context" |
| plan-fix | "Plan" |
| review-plan | "Review Verdict" |
| implement-fix | "Implementation Notes" |
| run-tests | "Test Results" |
| commit-push | "Commit" |
| submit-pr | "PR" |
| watch-pr-checks | "PR Checks" |
| diagnose-ci | "CI Diagnosis" |
| triage-cr | "CR Triage" |
| respond-cr | "CR Responses" |
| remediate | "Remediation Notes" |
| fix-tests | "Test Fix Attempt" |
| abandon-pr | "Abandonment" |

### Cross-cutting step behavior: Step Error Format

Rather than repeating error format requirements in every step section, this
single requirement covers all steps that invoke external tools:

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-CROSS-STEP-ERR-001 | When a step handler invokes LLxprt and it exits non-zero, the step throws an Error whose message contains: (a) the step name (e.g., "plan-fix"), and (b) the non-zero exit code | `[GAP]` |

Individual step sections below reference this cross-cutting requirement where
applicable instead of repeating the error format. Step-specific error behaviors
(e.g., partial failure resilience in abandon-pr) remain in their step sections.

### Cross-cutting step behavior: Timeout Propagation

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-STEP-TIMEOUT-001 | Every step that invokes `LlxprtClient.invoke()` passes `timeoutSeconds` from `LutherConfig.luther.crWaitTimeoutSeconds` (or a step-specific override if configured) in the `LlxprtInvokeParams` | `[GAP]` |

### Cross-cutting step behavior: Default Profile Fallback

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-STEP-PROFILE-001 | When a step-specific profile key is absent from `TargetConfig.profiles`, the step falls back to `LutherConfig.luther.defaultProfile` | `[GAP]` |

### 2.1 scan-issues

**Source:** `src/steps/scan-issues.ts`
**Test:** `test/steps/scan-issues.test.ts`

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-STEP-SCAN-001 | Queries GitHub for open issues in the configured `owner/repo` | `[GAP]` |
| REQ-STEP-SCAN-002 | Filters out issues assigned to any user | `[COVERED]` scan-issues.test.ts `"REQ-SCAN-002: filters to unassigned only"` |
| REQ-STEP-SCAN-003 | Filters out issues with the configured `skip` label | `[COVERED]` scan-issues.test.ts `"REQ-SCAN-003: excludes issues with skip label"` |
| REQ-STEP-SCAN-004 | Filters out issues with the configured `attempted` label | `[COVERED]` scan-issues.test.ts `"REQ-SCAN-004: excludes issues with attempted label"` |
| REQ-STEP-SCAN-005 | Returns `{ issue: null }` when no eligible issues remain | `[COVERED]` scan-issues.test.ts `"REQ-SCAN-005: returns null when all filtered out"` |
| REQ-STEP-SCAN-006 | Selects the lowest-numbered eligible issue | `[COVERED]` scan-issues.test.ts `"REQ-SCAN-006: selects oldest in lowest-numbered milestone"` |
| REQ-STEP-SCAN-007 | Assigns the selected issue to the configured `lutherLogin` | `[COVERED]` scan-issues.test.ts `"REQ-SCAN-010: assigns selected issue to luther-bot"` |
| REQ-STEP-SCAN-008 | Writes issue title to the output directory | `[PARTIAL]` scan-issues.test.ts `"REQ-SCAN-011: writes issue context including comments"` — test asserts `output.hasFile("issue-context.md")` and `content.toContain("Issue 42")` (title keyword only); does not verify full title string, body, or number are written |
| REQ-STEP-SCAN-009 | Writes issue body to the output directory | `[GAP]` |
| REQ-STEP-SCAN-010 | Writes issue number to the output directory | `[GAP]` |
| REQ-STEP-SCAN-011 | Initializes the session file with the selected issue | `[GAP]` |

### 2.2 plan-fix

**Source:** `src/steps/plan-fix.ts`
**Test:** `test/steps/plan-fix.test.ts`

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-STEP-PLAN-001 | Invokes LLxprt with the `planner` profile from TargetConfig | `[COVERED]` plan-fix.test.ts `"REQ-PLAN-001: invokes LLxprt with planner role"` |
| REQ-STEP-PLAN-002 | Prompt includes the issue title, body, and number | `[PARTIAL]` plan-fix.test.ts `"REQ-PLAN-002: prompt includes issue context"` — test asserts `toContain("Fix login crash")` (title only); does not verify body or issue number are present in the prompt |
| REQ-STEP-PLAN-003 | Prompt does NOT include session content (planners see only the issue) | `[COVERED]` plan-fix.test.ts `"REQ-PLAN-003: prompt does NOT include session content"` |
| REQ-STEP-PLAN-004 | Writes LLxprt output to the plan file in the output directory | `[PARTIAL]` plan-fix.test.ts `"REQ-PLAN-004: writes plan to ./tmp/plan.md"` — test asserts `output.hasFile("plan.md")` (existence only); does not verify file content matches LLxprt stdout |
| REQ-STEP-PLAN-005 | Returns `{ planFile }` (the output file path) on success | `[GAP]` |
| REQ-STEP-PLAN-006 | Throws on LLxprt non-zero exit (see REQ-CROSS-STEP-ERR-001) | `[COVERED]` plan-fix.test.ts `"REQ-PLAN-006: throws descriptive error on non-zero exit"` |
| REQ-STEP-PLAN-007 | On a REVISE loop, prompt includes the review feedback from the previous iteration | `[GAP]` |

### 2.3 review-plan

**Source:** `src/steps/review-plan.ts`
**Test:** `test/steps/review-plan.test.ts`

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-STEP-REV-001 | Invokes LLxprt with the `reviewer` profile from TargetConfig | `[COVERED]` review-plan.test.ts `"REQ-REV-001: invokes LLxprt with reviewer role"` |
| REQ-STEP-REV-002 | Prompt includes the plan content and issue context | `[PARTIAL]` review-plan.test.ts `"REQ-REV-002: prompt includes plan and issue context"` — test asserts `toContain("Fix login crash")` (title only); does not verify plan content is present in the prompt |
| REQ-STEP-REV-003 | Prompt does NOT include session content | `[COVERED]` review-plan.test.ts `"REQ-REV-003: prompt does NOT include session"` |
| REQ-STEP-REV-004 | Reads the review result JSON from the output directory after LLxprt completes | `[GAP]` |
| REQ-STEP-REV-005 | Returns `{ review: { verdict, issues } }` on success | `[GAP]` |
| REQ-STEP-REV-006 | When verdict is APPROVED, `revisedPlanFile` is null | `[GAP]` |
| REQ-STEP-REV-007 | When verdict is REVISE, `revisedPlanFile` points to the revised plan | `[GAP]` |
| REQ-STEP-REV-008 | Throws on LLxprt non-zero exit (see REQ-CROSS-STEP-ERR-001) | `[COVERED]` review-plan.test.ts `"REQ-REV-008: throws descriptive error on LLxprt failure"` |
| REQ-STEP-REV-009 | Validates the review result schema (verdict must be APPROVED or REVISE) | `[GAP]` |

### 2.4 implement-fix

**Source:** `src/steps/implement-fix.ts`
**Test:** `test/steps/implement-fix.test.ts`

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-STEP-IMPL-001 | Invokes LLxprt with the `implementer` profile from TargetConfig | `[COVERED]` implement-fix.test.ts `"REQ-IMPL-001: invokes LLxprt with implementer role"` |
| REQ-STEP-IMPL-002 | Prompt includes the plan, issue context, and session content | `[PARTIAL]` implement-fix.test.ts `"REQ-IMPL-002: prompt includes plan, issue, and session"` — test asserts `toContain("Fix login crash")` (title only); does not verify plan content or session content are present in the prompt |
| REQ-STEP-IMPL-003 | LLxprt workingDir is set to the target repo's `localPath` | `[GAP]` |
| REQ-STEP-IMPL-004 | Returns `{ notesFile }` (the output file path) on success | `[GAP]` |
| REQ-STEP-IMPL-005 | Writes implementation notes to the output directory | `[PARTIAL]` implement-fix.test.ts `"REQ-IMPL-005: writes implementation-notes.md"` — test asserts `output.hasFile("implementation-notes.md")` (existence only); does not verify file content matches LLxprt stdout |
| REQ-STEP-IMPL-006 | Throws on LLxprt non-zero exit (see REQ-CROSS-STEP-ERR-001) | `[COVERED]` implement-fix.test.ts `"REQ-IMPL-007: throws descriptive error on non-zero exit"` |

### 2.5 run-tests

**Source:** `src/steps/run-tests.ts`
**Test:** None

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-STEP-TEST-001 | Executes the configured `testCommand` in the target repo directory | `[GAP]` |
| REQ-STEP-TEST-002 | Executes the configured `lintCommand` in the target repo directory | `[GAP]` |
| REQ-STEP-TEST-003 | Executes the configured `formatCommand` in the target repo directory | `[GAP]` |
| REQ-STEP-TEST-004 | Executes the configured `buildCommand` in the target repo directory | `[GAP]` |
| REQ-STEP-TEST-005 | Returns `{ passed: true }` when all commands exit zero | `[GAP]` |
| REQ-STEP-TEST-006 | Returns `{ passed: false, output }` with combined stderr/stdout when any command fails | `[GAP]` |
| REQ-STEP-TEST-007 | Stops execution at the first failing command (does not run build if lint fails) | `[GAP]` |
| REQ-STEP-TEST-008 | Captures and returns both stdout and stderr from failed commands | `[GAP]` |

### 2.6 commit-push

**Source:** `src/steps/commit-push.ts`
**Test:** `test/steps/commit-push.test.ts`

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-STEP-PUSH-001 | Returns a valid commit SHA (hex string) on success | `[COVERED]` commit-push.test.ts `"REQ-PUSH-001: returns valid commit sha on success"` |
| REQ-STEP-PUSH-002 | Stages all changes (`git add -A`) before committing | `[GAP]` |
| REQ-STEP-PUSH-003 | Commits with the provided message | `[GAP]` |
| REQ-STEP-PUSH-004 | Pushes to the remote branch | `[GAP]` |
| REQ-STEP-PUSH-005 | Throws an error containing the step name "commit-push" and the stderr output when git operations fail | `[COVERED]` commit-push.test.ts `"REQ-PUSH-005: throws descriptive error on git failure"` |
| REQ-STEP-PUSH-006 | Force-pushes if the remote branch has diverged | `[GAP]` |

### 2.7 submit-pr

**Source:** `src/steps/submit-pr.ts`
**Test:** `test/steps/submit-pr.test.ts`

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-STEP-SUB-001 | Creates a PR on GitHub with title containing the issue number | `[GAP]` |
| REQ-STEP-SUB-002 | PR body includes "Fixes #N" to auto-close the issue | `[GAP]` |
| REQ-STEP-SUB-003 | PR head branch matches the context branch | `[GAP]` |
| REQ-STEP-SUB-004 | PR base branch is the repo default branch (main/master) | `[GAP]` |
| REQ-STEP-SUB-005 | Returns `{ pr: PRContext }` with the created PR number and branch | `[GAP]` |
| REQ-STEP-SUB-006 | Throws an error containing the step name "submit-pr" and the underlying GitHub error message when PR creation fails | `[COVERED]` submit-pr.test.ts `"REQ-SUB-006: throws descriptive error on gh failure"` |

### 2.8 watch-pr-checks

**Source:** `src/steps/watch-pr-checks.ts`
**Test:** None

Precedence rules for WATCHING events are defined canonically in §1.4. This
section covers step-level behavior only.

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-STEP-WATCH-001 | Polls `getPRChecks` until all checks leave the `pending` state | `[GAP]` |
| REQ-STEP-WATCH-002 | Returns `{ checks: PRChecksResult }` with overall status and individual runs | `[GAP]` |
| REQ-STEP-WATCH-003 | Detects CR comments by calling `getCRComments` | `[GAP]` |
| REQ-STEP-WATCH-004 | Sets `hasCRComments: true` when unresolved CR comments exist | `[GAP]` |
| REQ-STEP-WATCH-005 | Respects the configured `crWaitTimeoutSeconds` for CR comment polling | `[GAP]` |
| REQ-STEP-WATCH-006 | Returns `TIMEOUT` event when CR wait exceeds the timeout | `[GAP]` |
| REQ-STEP-WATCH-007 | CR comment polling begins only after all CI checks have settled (left `pending` state) | `[GAP]` |
| REQ-STEP-WATCH-008 | Event selection follows the precedence rules defined in §1.4 | `[GAP]` |

### 2.9 diagnose-ci

**Source:** `src/steps/diagnose-ci.ts`
**Test:** `test/steps/diagnose-ci.test.ts`

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-STEP-CIDR-001 | Invokes LLxprt with the `ciDiagnose` profile | `[COVERED]` diagnose-ci.test.ts `"REQ-CIDR-001: invokes LLxprt with ci-diagnose role"` |
| REQ-STEP-CIDR-002 | Prompt includes workflow logs and the PR diff | `[PARTIAL]` diagnose-ci.test.ts `"REQ-CIDR-002: prompt includes workflow logs and diff"` — test asserts `toContain("diff")` only; does not verify workflow logs are present in the prompt |
| REQ-STEP-CIDR-003 | Fetches workflow logs via `getWorkflowLogs` for each failed run | `[GAP]` |
| REQ-STEP-CIDR-004 | Validates that classification values are INFRA, FLAKY, or PR_RELATED | `[COVERED]` diagnose-ci.test.ts `"REQ-CIDR-004: rejects invalid classification enum"` |
| REQ-STEP-CIDR-005 | Returns `{ diagnosis: CIDiagnosisResult }` on success | `[GAP]` |
| REQ-STEP-CIDR-006 | Retriggers INFRA-classified workflows via `retriggerWorkflow` | `[COVERED]` diagnose-ci.test.ts `"REQ-CIRT-001: retrigger calls retriggerWorkflow on github"` |
| REQ-STEP-CIDR-007 | Files a new issue for FLAKY-classified failures with the test name | `[COVERED]` diagnose-ci.test.ts `"REQ-FLKY-001: file-flaky creates issue with test name"` |
| REQ-STEP-CIDR-008 | Flaky issue gets the configured `flakyTest` label | `[COVERED]` diagnose-ci.test.ts `"REQ-FLKY-003: flaky issue gets flaky-test label"` |
| REQ-STEP-CIDR-009 | Throws on LLxprt non-zero exit (see REQ-CROSS-STEP-ERR-001) | `[GAP]` |

### 2.10 triage-cr

**Source:** `src/steps/triage-cr.ts`
**Test:** `test/steps/triage-cr.test.ts`

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-STEP-CRTG-001 | Invokes LLxprt with the `crTriage` profile | `[COVERED]` triage-cr.test.ts `"REQ-CRTG-001: invokes LLxprt with cr-triage role"` |
| REQ-STEP-CRTG-002 | Prompt includes all unresolved CR comments and the PR diff | `[GAP]` |
| REQ-STEP-CRTG-003 | Reads the triage result JSON from the output directory after LLxprt completes | `[GAP]` |
| REQ-STEP-CRTG-004 | Validates classification values are IN_SCOPE, OPPORTUNITY, OUT_OF_SCOPE, or INVALID | `[GAP]` |
| REQ-STEP-CRTG-005 | Returns `{ triage: CRTriageResult }` with per-comment classification | `[GAP]` |
| REQ-STEP-CRTG-006 | Throws on LLxprt non-zero exit (see REQ-CROSS-STEP-ERR-001) | `[GAP]` |
| REQ-STEP-CRTG-007 | Only includes unresolved comments (filters out resolved threads) | `[GAP]` |

### 2.11 respond-cr

**Source:** `src/steps/respond-cr.ts`
**Test:** `test/steps/respond-cr.test.ts`

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-STEP-CRSP-001 | Replies to each triaged comment on the PR | `[GAP]` |
| REQ-STEP-CRSP-002 | Reply body includes the triage reasoning | `[COVERED]` respond-cr.test.ts `"REQ-CRSP-002: reply includes reasoning"` |
| REQ-STEP-CRSP-003 | Resolves threads classified as IN_SCOPE or OPPORTUNITY | `[COVERED]` respond-cr.test.ts `"REQ-CRSP-003: resolves IN_SCOPE and OPPORTUNITY"` |
| REQ-STEP-CRSP-004 | Resolves threads classified as INVALID | `[COVERED]` respond-cr.test.ts `"REQ-CRSP-004: resolves INVALID threads"` |
| REQ-STEP-CRSP-005 | Leaves OUT_OF_SCOPE threads unresolved | `[COVERED]` respond-cr.test.ts `"REQ-CRSP-005: leaves OUT_OF_SCOPE unresolved"` |
| REQ-STEP-CRSP-006 | Returns `{ resolvedCount }` with the number of resolved threads | `[COVERED]` respond-cr.test.ts `"REQ-CRSP-006: returns counts and signals PUSHING"` |
| REQ-STEP-CRSP-007 | Returns `{ leftOpenCount }` with the number of unresolved threads | `[COVERED]` respond-cr.test.ts `"REQ-CRSP-006: returns counts and signals PUSHING"` |
| REQ-STEP-CRSP-008 | Reply includes the `plannedAction` for IN_SCOPE comments | `[GAP]` |

### 2.12 remediate

**Source:** `src/steps/remediate.ts`
**Test:** `test/steps/remediate.test.ts`

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-STEP-REMED-001 | Invokes LLxprt with the `remediator` profile | `[COVERED]` remediate.test.ts `"REQ-REMED-001: invokes LLxprt with remediator role"` |
| REQ-STEP-REMED-002 | Prompt includes triage results and CI diagnosis when available | `[PARTIAL]` remediate.test.ts `"REQ-REMED-002: prompt includes triage and diagnosis"` — test asserts `toContain("IN_SCOPE")` only; does not verify CI diagnosis content is present |
| REQ-STEP-REMED-003 | Prompt handles null crTriage and null ciDiagnosis gracefully | `[GAP]` |
| REQ-STEP-REMED-004 | Writes remediation notes to the output directory | `[PARTIAL]` remediate.test.ts `"REQ-REMED-004: writes remediation-notes.md"` — test asserts `output.hasFile("remediation-notes.md")` (existence only); does not verify file content |
| REQ-STEP-REMED-005 | Prompt includes the full session content | `[PARTIAL]` remediate.test.ts `"REQ-REMED-005: includes session in prompt"` — test asserts `toContain("did stuff")` (partial content check); does not verify full session is included |
| REQ-STEP-REMED-006 | Throws on LLxprt non-zero exit (see REQ-CROSS-STEP-ERR-001) | `[COVERED]` remediate.test.ts `"REQ-REMED-006: throws descriptive error on LLxprt failure"` |
| REQ-STEP-REMED-007 | LLxprt workingDir is set to the target repo's `localPath` | `[GAP]` |

### 2.13 fix-tests

**Source:** `src/steps/fix-tests.ts` — **does not exist yet**
**Test:** `test/steps/fix-tests.test.ts` (uses inline stub; source file absent)

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-STEP-TFIX-001 | Invokes LLxprt with the `implementer` profile | `[BLOCKED]` fix-tests.test.ts `"REQ-TFIX-001: invokes LLxprt with implementer role"` — source file absent; test uses inline stub |
| REQ-STEP-TFIX-002 | Prompt includes the full test failure output | `[BLOCKED]` fix-tests.test.ts `"REQ-TFIX-001: prompt includes failure output"` — source file absent; test uses inline stub |
| REQ-STEP-TFIX-003 | Prompt includes the session content (implementation context) | `[BLOCKED]` fix-tests.test.ts `"REQ-TFIX-002: receives luther-session.md as input"` — source file absent; test uses inline stub |
| REQ-STEP-TFIX-004 | Returns `{ fixed: true }` when LLxprt exits zero | `[BLOCKED]` — source file absent |
| REQ-STEP-TFIX-005 | Throws on LLxprt non-zero exit (see REQ-CROSS-STEP-ERR-001) | `[BLOCKED]` fix-tests.test.ts `"throws meaningful error on LLxprt failure"` — source file absent; test uses inline stub |
| REQ-STEP-TFIX-006 | LLxprt workingDir is set to the target repo's `localPath` | `[BLOCKED]` — source file absent |

### 2.14 abandon-pr

**Source:** `src/steps/abandon-pr.ts`
**Test:** `test/steps/abandon-pr.test.ts`

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-STEP-ABAN-001 | Closes the PR with a comment explaining the abandonment reason | `[COVERED]` abandon-pr.test.ts `"REQ-ABAN-001: closes PR with comment"` |
| REQ-STEP-ABAN-002 | Unassigns the issue from the configured `lutherLogin` | `[COVERED]` abandon-pr.test.ts `"REQ-ABAN-002: unassigns issue from luther-bot"` |
| REQ-STEP-ABAN-003 | Adds the configured `attempted` label to the issue | `[COVERED]` abandon-pr.test.ts `"REQ-ABAN-003: adds luther-attempted label"` |
| REQ-STEP-ABAN-004 | Returns `{ abandoned: true }` on completion | `[COVERED]` abandon-pr.test.ts `"REQ-ABAN-001: closes PR with comment"` (asserts `result.abandoned`) |
| REQ-STEP-ABAN-005 | Handles null PR gracefully (skips closePR, still unassigns and labels) | `[COVERED]` abandon-pr.test.ts `"abandons without PR when pr is null"` |
| REQ-STEP-ABAN-006 | closePR failure does not prevent unassign or labeling | `[COVERED]` abandon-pr.test.ts `"REQ-ABAN-006: closePR failure doesn't prevent unassign"` |
| REQ-STEP-ABAN-007 | unassignIssue failure does not prevent labeling | `[COVERED]` abandon-pr.test.ts `"REQ-ABAN-006: unassign failure doesn't prevent label"` |
| REQ-STEP-ABAN-008 | addLabel failure does not prevent completion | `[COVERED]` abandon-pr.test.ts `"REQ-ABAN-006: addLabel failure doesn't prevent completion"` |
| REQ-STEP-ABAN-009 | Handles null issue gracefully (skips unassign and label) | `[GAP]` |
| REQ-STEP-ABAN-010 | Deletes the local working branch after abandonment | `[GAP]` |

### 2.15 log-outcome

**Source:** `src/steps/log-outcome.ts`
**Test:** `test/steps/log-outcome.test.ts`

Note: `finalState` is passed as a parameter to `logOutcome` (e.g.,
`logOutcome({ deps, context, finalState: "SUCCESS" })`). It is NOT read from
`WorkflowContext`. See terminology section.

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-STEP-LOG-001 | Returns `{ logged: true }` on successful write | `[COVERED]` log-outcome.test.ts `"REQ-LOG-002: logging always executes"` |
| REQ-STEP-LOG-002 | Appends a JSONL line to the outcome log (does not overwrite) | `[PARTIAL]` log-outcome.test.ts `"REQ-LOG-004: appends to existing JSONL file"` — test asserts `toContain("previous")` (verifies prior content survives); does not verify the new entry is valid JSONL or that exactly one line was appended |
| REQ-STEP-LOG-003 | Outcome entry includes event, issue number, PR number, and timestamp | `[GAP]` |
| REQ-STEP-LOG-004 | Success outcome records `kind: "pr_submitted"` with loop count | `[GAP]` |
| REQ-STEP-LOG-005 | Success outcome records `kind: "pr_merged"` with PR number | `[GAP]` |
| REQ-STEP-LOG-006 | ABANDONED outcome records `kind: "pr_abandoned"` with reason | `[GAP]` |
| REQ-STEP-LOG-007 | Skip outcome records `kind: "issue_skipped"` with reason | `[GAP]` |
| REQ-STEP-LOG-008 | Error outcome records `kind: "error"` with message and step | `[GAP]` |
| REQ-STEP-LOG-009 | Write failure does not throw (returns `{ logged: false }`) | `[COVERED]` log-outcome.test.ts `"REQ-LOG-012: write failure does not throw"` |
| REQ-STEP-LOG-010 | Final state (passed as parameter) is recorded in the outcome entry | `[PARTIAL]` log-outcome.test.ts `"REQ-SUCC-001: success outcome records final state"` — test asserts `toContain("SUCCESS")` (string presence only); does not verify the JSONL structure or field name |
| REQ-STEP-LOG-011 | Unassigns the issue from `lutherLogin` on success | `[COVERED]` log-outcome.test.ts `"REQ-SUCC-002: unassigns issue on success"` |
| REQ-STEP-LOG-012 | Cleans the session after logging | `[GAP]` |
| REQ-STEP-LOG-013 | Cleans the output directory after logging | `[GAP]` |

#### 2.15.1 Outcome JSONL Format

The outcome log is a JSONL file (one JSON object per line). Each line describes
a completed workflow run. There is no `OutcomeEvent` type in `types.ts` — these
requirements describe the serialized JSONL format, not a TypeScript contract.

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-STEP-LOG-FMT-001 | Each JSONL line contains a `timestamp` field with an ISO 8601 string | `[GAP]` |
| REQ-STEP-LOG-FMT-002 | Each JSONL line contains an `event` field (non-empty string identifying the outcome type) | `[GAP]` |
| REQ-STEP-LOG-FMT-003 | Each JSONL line contains a `data` object with a `kind` field discriminator (one of: `pr_submitted`, `pr_merged`, `pr_abandoned`, `issue_skipped`, `error`) | `[GAP]` |
| REQ-STEP-LOG-FMT-004 | Each JSONL line contains an `issue` field: the issue number (positive integer) or null | `[GAP]` |
| REQ-STEP-LOG-FMT-005 | Each JSONL line contains a `pr` field: the PR number (positive integer) or null | `[GAP]` |

---

## 3. Libraries

### 3.1 SessionManager (`src/lib/session.ts`)

**Implementation:** `FileSessionManager`
**Test:** `test/lib/session.test.ts`

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-LIB-SESSION-001 | `create(issue)` produces a file containing the issue number, title, and body | `[PARTIAL]` session.test.ts `"REQ-SESS-001: creates luther-session.md file"` — test asserts `toContain("42")` (number only); does not verify title or body are present |
| REQ-LIB-SESSION-002 | `appendSection(heading, content)` adds a markdown section to the session file | `[COVERED]` session.test.ts `"REQ-SESS-002: appendSection adds content"` |
| REQ-LIB-SESSION-003 | Multiple `appendSection` calls accumulate in order | `[COVERED]` session.test.ts `"REQ-SESS-002: multiple appendSection calls accumulate"` |
| REQ-LIB-SESSION-004 | Sections are ordered chronologically (Plan before Implementation before PR) | `[PARTIAL]` session.test.ts `"REQ-SESS-002: per-step append produces ordered sections"` — test verifies string position (`indexOf("Plan") < indexOf("Implementation")`) which confirms append order, but does not verify timestamp-based chronological ordering |
| REQ-LIB-SESSION-005 | `create()` replaces all previous content (fresh start) | `[COVERED]` session.test.ts `"REQ-SESS-004: create replaces previous content"` |
| REQ-LIB-SESSION-006 | `getContent()` returns the full session as a single string | `[PARTIAL]` session.test.ts `"REQ-SESS-003: session contains all sections after full flow"` — test asserts `toContain` for individual section headings; does not verify the return type is a single string or that all content is present verbatim |
| REQ-LIB-SESSION-007 | `clear()` removes all session content | `[GAP]` |
| REQ-LIB-SESSION-008 | `clear()` on I/O error throws (does not swallow the error) | `[GAP]` |
| REQ-LIB-SESSION-009 | Session file is written to the configured `sessionDir` | `[GAP]` |
| REQ-LIB-SESSION-010 | Creates the session directory if it does not exist | `[GAP]` |

### 3.2 OutputManager (`src/lib/output.ts`)

**Implementation:** `FileOutputManager`
**Test:** `test/lib/output.test.ts`

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-LIB-OUTPUT-001 | `writeJson` / `readJson` round-trips data through the output directory | `[COVERED]` output.test.ts `"REQ-OUT-001: round-trips JSON within ./tmp/"` |
| REQ-LIB-OUTPUT-002 | `writeJson` produces valid parseable JSON | `[COVERED]` output.test.ts `"REQ-OUT-002: writeJson produces valid parseable JSON"` |
| REQ-LIB-OUTPUT-003 | `readJson` applies the validator function and returns the typed result | `[COVERED]` output.test.ts `"REQ-OUT-003: readJson applies validator and returns typed result"` |
| REQ-LIB-OUTPUT-004 | `readJson` throws when the validator rejects the data | `[COVERED]` output.test.ts `"REQ-OUT-003: readJson throws when validator rejects"` |
| REQ-LIB-OUTPUT-005 | `readJson` throws on a missing file | `[COVERED]` output.test.ts `"REQ-OUT-004: readJson throws on missing file"` |
| REQ-LIB-OUTPUT-006 | `readJson` throws on malformed JSON | `[COVERED]` output.test.ts `"REQ-OUT-004: readJson throws on malformed JSON"` |
| REQ-LIB-OUTPUT-007 | `clean()` removes transient files (step handler outputs written during an issue run) from the output directory | `[PARTIAL]` output.test.ts `"REQ-OUT-001: clean removes transient files"` — test verifies a transient file is removed; does not verify persistent files (outcome log, state) are preserved |
| REQ-LIB-OUTPUT-008 | `writeText` / `readText` round-trips string content | `[GAP]` |
| REQ-LIB-OUTPUT-009 | `clean()` preserves persistent files: the JSONL outcome log and persisted state | `[GAP]` |
| REQ-LIB-OUTPUT-010 | Creates the output directory if it does not exist | `[GAP]` |

### 3.3 Logger (`src/lib/logger.ts`)

**Implementation:** `createLogger`, `createChildLogger`
**Test:** `test/lib/logger.test.ts`

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-LIB-LOG-001 | Default log level is `info` | `[COVERED]` logger.test.ts `"REQ-LOG-020: creates a pino logger at info level"` |
| REQ-LIB-LOG-002 | `--debug` flag sets log level to `debug` | `[COVERED]` logger.test.ts `"REQ-LOG-024: --debug enables debug output"` |
| REQ-LIB-LOG-003 | `--verbose` flag sets log level to `trace` | `[COVERED]` logger.test.ts `"REQ-LOG-025: --verbose enables trace output"` |
| REQ-LIB-LOG-004 | All five pino levels (trace, debug, info, warn, error) produce output | `[COVERED]` logger.test.ts `"REQ-LOG-021: all five pino levels produce output"` |
| REQ-LIB-LOG-005 | Child logger includes the `module` field in every log entry | `[COVERED]` logger.test.ts `"REQ-LOG-027: child includes module field in output"` |
| REQ-LIB-LOG-006 | Log directory is created if it does not exist | `[GAP]` |
| REQ-LIB-LOG-007 | Warn and above are also written to stdout (fd 1) | `[GAP]` |

### 3.4 LlxprtClient (`src/lib/llxprt.ts`)

**Implementation:** `LlxprtSubprocess`
**Test:** None (tested indirectly through step handlers with `FakeLlxprtClient`)

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-LIB-LLXPRT-001 | Spawns the configured `llxprtBinary` as a subprocess | `[GAP]` |
| REQ-LIB-LLXPRT-002 | Passes the `profile` as a CLI argument | `[GAP]` |
| REQ-LIB-LLXPRT-003 | Passes the `prompt` via stdin or file argument | `[GAP]` |
| REQ-LIB-LLXPRT-004 | Sets the subprocess working directory to `workingDir` | `[GAP]` |
| REQ-LIB-LLXPRT-005 | Captures stdout and stderr from the subprocess | `[GAP]` |
| REQ-LIB-LLXPRT-006 | Returns `{ exitCode, stdout, stderr }` | `[GAP]` |
| REQ-LIB-LLXPRT-007 | When `timeoutSeconds` is defined, kills the subprocess if it exceeds the timeout | `[GAP]` |
| REQ-LIB-LLXPRT-008 | When `timeoutSeconds` is undefined, no timeout is applied (subprocess runs indefinitely) | `[GAP]` |
| REQ-LIB-LLXPRT-009 | Throws an error containing the binary path when the binary is not found on PATH | `[GAP]` |

### 3.5 GithubClient (`src/lib/gh.ts`)

**Implementation:** `GhClient`
**Test:** None (tested indirectly through step handlers with `FakeGithubClient`)

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-LIB-GH-001 | `listOpenUnassignedIssues` returns issues from the GitHub API | `[GAP]` |
| REQ-LIB-GH-002 | `assignIssue` assigns a user to the issue via the GitHub API | `[GAP]` |
| REQ-LIB-GH-003 | `unassignIssue` removes a user from the issue via the GitHub API | `[GAP]` |
| REQ-LIB-GH-004 | `addLabel` adds a label to the issue via the GitHub API | `[GAP]` |
| REQ-LIB-GH-005 | `createPR` creates a pull request and returns the PR context | `[GAP]` |
| REQ-LIB-GH-006 | `closePR` closes the PR with a comment | `[GAP]` |
| REQ-LIB-GH-007 | `getPRChecks` returns the status of all workflow runs for the PR | `[GAP]` |
| REQ-LIB-GH-008 | `getCRComments` returns all CR comments for the PR | `[GAP]` |
| REQ-LIB-GH-009 | `replyCRComment` posts a reply to a review thread | `[GAP]` |
| REQ-LIB-GH-010 | `resolveCRThread` marks a review thread as resolved | `[GAP]` |
| REQ-LIB-GH-011 | `getWorkflowLogs` returns the log text for a workflow run | `[GAP]` |
| REQ-LIB-GH-012 | `retriggerWorkflow` re-runs a workflow via the GitHub API | `[GAP]` |
| REQ-LIB-GH-013 | `createIssue` creates a new issue and returns the issue context | `[GAP]` |
| REQ-LIB-GH-014 | `listOpenUnassignedIssues` populates `IssueContext.labels` from the GitHub API response | `[GAP]` |
| REQ-LIB-GH-015 | When `gh` CLI produces output that does not match the expected JSON schema, throws an error containing the raw output (first 500 characters) in the error message | `[GAP]` |
| REQ-LIB-GH-016 | All GithubClient methods that invoke `gh` CLI wrap subprocess errors in an Error whose message contains the `gh` stderr output | `[GAP]` |

### 3.6 UI (`src/lib/ui.ts`)

**Implementation:** `printFatal`, `printInfo`, `printWarn`
**Test:** None

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-LIB-UI-001 | `printFatal` writes a prefixed error message to stderr | `[GAP]` |
| REQ-LIB-UI-002 | `printInfo` writes a message to stdout | `[GAP]` |
| REQ-LIB-UI-003 | `printWarn` writes a warning to stderr | `[GAP]` |

---

## 4. Engine

### 4.1 Persistence (`src/engine/persistence.ts`)

**Test:** `test/engine/persistence.test.ts`

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-ENG-001 | `saveState` serializes the full `PersistedState` to a JSON file at the given path | `[COVERED]` persistence.test.ts `"REQ-ENG-010: serializes state to the given path"` |
| REQ-ENG-002 | `saveState` throws when the target directory does not exist | `[COVERED]` persistence.test.ts `"write to nonexistent directory throws"` |
| REQ-ENG-003 | `saveState` / `loadState` round-trips preserve all context values | `[COVERED]` persistence.test.ts `"REQ-ENG-010: round-trip preserves context values"` |
| REQ-ENG-004 | `loadState` deserializes and returns the persisted state | `[COVERED]` persistence.test.ts `"REQ-ENG-011: deserializes persisted state"` |
| REQ-ENG-005 | `loadState` returns null when the file does not exist | `[COVERED]` persistence.test.ts `"REQ-ENG-012: returns null when file does not exist"` |
| REQ-ENG-006 | `loadState` returns null for corrupt (unparseable) JSON | `[COVERED]` persistence.test.ts `"REQ-ENG-013: returns null for corrupt JSON"` |
| REQ-ENG-007 | `loadState` returns null for truncated JSON | `[COVERED]` persistence.test.ts `"REQ-ENG-013: returns null for truncated JSON"` |
| REQ-ENG-008 | `loadState` returns null when schema version does not match | `[COVERED]` persistence.test.ts `"REQ-SCHEMA-002: returns null for mismatched schema version"` |
| REQ-ENG-009 | `loadState` deletes the stale file on schema version mismatch | `[COVERED]` persistence.test.ts `"REQ-ENG-016: schema mismatch deletes the stale file"` |
| REQ-ENG-010 | `clearState` deletes the state file from disk | `[COVERED]` persistence.test.ts `"deletes the state file from disk"` |
| REQ-ENG-011 | `clearState` is a no-op when the file does not exist | `[GAP]` |
| REQ-ENG-012 | Persisted state includes a schema version field | `[GAP]` |

### 4.2 Machine Config (`src/engine/machine.ts`)

**Test:** `test/engine/machine.test.ts`

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-ENG-020 | `loadWorkflowDefinition` rejects JSON without required `states` field | `[COVERED]` machine.test.ts `"REQ-ENG-002: rejects JSON with missing required states field"` |
| REQ-ENG-021 | `loadWorkflowDefinition` throws for a nonexistent file | `[COVERED]` machine.test.ts `"REQ-ENG-003: throws human-readable message for nonexistent file"` |
| REQ-ENG-022 | `loadWorkflowDefinition` throws for invalid schema | `[COVERED]` machine.test.ts `"REQ-ENG-003: throws message identifying invalid field for bad schema"` |
| REQ-ENG-023 | `loadWorkflowDefinition` returns a branded `MachineConfig` on valid input | `[GAP]` |
| REQ-ENG-024 | `createLutherMachine` returns a valid XState machine from config and deps | `[GAP]` |
| REQ-ENG-025 | `createLutherMachine` wires each state to its step handler | `[GAP]` |

### 4.3 Runner (`src/engine/runner.ts`)

**Test:** `test/engine/runner.test.ts`

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-ENG-030 | `loadConfig` throws on a missing config file | `[COVERED]` runner.test.ts `"REQ-ENG-020: throws on missing config file"` |
| REQ-ENG-031 | `loadConfig` validates the config schema | `[GAP]` |
| REQ-ENG-032 | `loadConfig` returns a valid `LutherConfig` on success | `[GAP]` |
| REQ-ENG-033 | `createDependencies` wires all real implementations into a `Dependencies` bundle | `[GAP]` |
| REQ-ENG-034 | `runWorkflow` creates the machine, restores state if available, and starts execution | `[GAP]` |
| REQ-ENG-035 | `runWorkflow` persists state after each transition | `[GAP]` |
| REQ-ENG-036 | `runWorkflow` clears persisted state on terminal states (SUCCESS) and on transition from LOGGING to IDLE | `[GAP]` |
| REQ-ENG-037 | `runWorkflow` resumes from the persisted state on restart | `[GAP]` |

### 4.4 Runner Lifecycle

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-ENG-LIFE-001 | Runner startup sequence: load config → create dependencies → restore persisted state → run workflow | `[GAP]` |
| REQ-ENG-LIFE-002 | Runner shutdown sequence: persist current state → cleanup resources → exit | `[GAP]` |
| REQ-ENG-LIFE-003 | `createDependencies` validates that all dependency fields are non-null before returning the bundle | `[GAP]` |

### 4.5 Resume Semantics

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-ENG-RESUME-001 | On restart with a persisted state file, the engine resumes at the persisted state (not from SCANNING) | `[GAP]` |
| REQ-ENG-RESUME-002 | If the crashed step had already pushed commits, resuming does not re-push duplicates (PUSHING is idempotent or skipped) | `[GAP]` |
| REQ-ENG-RESUME-003 | If the crashed step had already created a PR, resuming does not create a duplicate PR (SUBMITTING checks for existing PR) | `[GAP]` |
| REQ-ENG-RESUME-004 | If the crashed step had already assigned the issue, resuming does not fail on re-assignment | `[GAP]` |
| REQ-ENG-RESUME-005 | Persisted `context.history` is restored so that the resumed run has full history | `[GAP]` |

### 4.6 Retry (`retryWithBackoff`)

**Test:** `test/engine/retry.test.ts`

Note: `retryWithBackoff` is not yet exported from any source file. The test
file uses an inline stub.

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-ENG-RETRY-001 | Retries exactly `maxRetries` times before throwing | `[BLOCKED]` retry.test.ts `"REQ-TRANS-005: retries exactly maxRetries times then throws"` — source not yet exported; test uses inline stub |
| REQ-ENG-RETRY-002 | Returns the result on a successful retry | `[BLOCKED]` retry.test.ts `"REQ-TRANS-005: returns result on successful retry"` — source not yet exported; test uses inline stub |
| REQ-ENG-RETRY-003 | Initial delay is approximately 1 second | `[BLOCKED]` retry.test.ts `"REQ-TRANS-005A: initial delay is approximately 1 second"` — source not yet exported; test uses inline stub |
| REQ-ENG-RETRY-004 | Delays grow exponentially between retries | `[BLOCKED]` retry.test.ts `"REQ-TRANS-005A: delays grow exponentially"` — source not yet exported; test uses inline stub |
| REQ-ENG-RETRY-005 | Delay is capped at `maxDelay` | `[BLOCKED]` retry.test.ts `"REQ-TRANS-005A: delay capped at 120 seconds"` — source not yet exported; test uses inline stub |
| REQ-ENG-RETRY-006 | Jitter is applied using the provided RNG | `[BLOCKED]` retry.test.ts `"REQ-TRANS-005A: FakeRng at 0.5 produces mid-range jitter"` — source not yet exported; test uses inline stub |
| REQ-ENG-RETRY-007 | All parameters (initialDelay, multiplier, maxDelay, jitter, maxRetries) are configurable | `[BLOCKED]` retry.test.ts `"REQ-TRANS-005A: all parameters configurable"` — source not yet exported; test uses inline stub |
| REQ-ENG-RETRY-008 | Non-retryable errors (e.g., 4xx HTTP) are not retried | `[GAP]` |

### 4.7 Signal Handling

**Test:** None (FakeSignalHandler exists in `test/fakes/fake-process.ts` but no tests use it)

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-ENG-SIG-001 | First SIGINT triggers graceful shutdown (finish current step, persist state, exit) | `[GAP]` |
| REQ-ENG-SIG-002 | Second SIGINT within 5 seconds of the first triggers immediate exit (process.exit) with no additional cleanup | `[GAP]` |
| REQ-ENG-SIG-003 | SIGTERM triggers graceful shutdown (same behavior as first SIGINT) | `[GAP]` |
| REQ-ENG-SIG-004 | Graceful shutdown persists the current state before exiting | `[GAP]` |
| REQ-ENG-SIG-005 | Graceful shutdown unassigns the issue if one was claimed | `[GAP]` |

---

## 5. CLI

**Source:** `src/index.ts`
**Test:** None

### 5.1 Entry Point

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-CLI-001 | `main()` calls `loadConfig` and `runWorkflow` | `[GAP]` |
| REQ-CLI-002 | Unhandled errors are caught, printed via `printFatal`, and exit with code 1 | `[GAP]` |
| REQ-CLI-003 | `--config` flag specifies an alternate config file path | `[GAP]` |
| REQ-CLI-004 | `--debug` flag enables debug logging | `[GAP]` |
| REQ-CLI-005 | `--verbose` flag enables trace logging | `[GAP]` |
| REQ-CLI-006 | `--dry-run` flag runs scanning only without making changes | `[GAP]` |
| REQ-CLI-007 | No arguments uses default config path | `[GAP]` |

### 5.2 Exit Codes

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-CLI-EXIT-001 | Process exits with code 0 on successful workflow completion (SUCCESS terminal state) | `[GAP]` |
| REQ-CLI-EXIT-002 | Process exits with code 1 on unhandled error | `[GAP]` |

---

## 6. Contract Completeness (`types.ts`)

These requirements verify that domain types defined in `src/engine/types.ts`
are correctly validated, constructed, and consumed across the codebase.

### 6.1 WorkflowContext

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-TYPE-CTX-001 | `WorkflowContext.history` entries conform to the `HistoryEntry` schema: `{ state: string, timestamp: string, event: string }` | `[GAP]` |
| REQ-TYPE-CTX-002 | `WorkflowContext` initial state has all nullable fields set to null and counters set to 0 | `[GAP]` |
| REQ-TYPE-CTX-003 | `WorkflowContext.maxLoops` and `maxTestFixAttempts` are populated from `LutherConfig` | `[GAP]` |

### 6.2 WorkflowRun Fidelity

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-TYPE-RUN-001 | `WorkflowRun.name` is a non-empty string corresponding to the workflow name from the GitHub Actions API response | `[GAP]` |
| REQ-TYPE-RUN-002 | `WorkflowRun.status` is one of `"pass"`, `"fail"`, or `"pending"`, mapped from the GitHub API check run conclusion/status | `[GAP]` |
| REQ-TYPE-RUN-003 | `WorkflowRun.runId` is a positive integer corresponding to the GitHub Actions run ID from the API response | `[GAP]` |

### 6.3 CRComment

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-TYPE-CR-001 | `CRComment.threadId` is a non-empty string uniquely identifying the review thread | `[GAP]` |
| REQ-TYPE-CR-002 | `CRComment.path` is a file path relative to the repo root | `[GAP]` |
| REQ-TYPE-CR-003 | `CRComment.line` is a positive integer | `[GAP]` |
| REQ-TYPE-CR-004 | `CRComment.body` is a non-empty string containing the review comment text | `[GAP]` |
| REQ-TYPE-CR-005 | `CRComment.resolved` is a boolean indicating whether the thread has been resolved | `[GAP]` |

### 6.4 CreatePRParams Validation

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-TYPE-PR-001 | `CreatePRParams.title` is a non-empty string | `[GAP]` |
| REQ-TYPE-PR-002 | `CreatePRParams.body` is a non-empty string | `[GAP]` |
| REQ-TYPE-PR-003 | `CreatePRParams.head` is a non-empty branch name | `[GAP]` |
| REQ-TYPE-PR-004 | `CreatePRParams.base` is a non-empty branch name | `[GAP]` |
| REQ-TYPE-PR-005 | `CreatePRParams.owner` and `repo` match the configured target repo | `[GAP]` |

### 6.5 CreateIssueParams Validation

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-TYPE-ISSUE-001 | `CreateIssueParams.title` is a non-empty string | `[GAP]` |
| REQ-TYPE-ISSUE-002 | `CreateIssueParams.body` is a non-empty string | `[GAP]` |
| REQ-TYPE-ISSUE-003 | `CreateIssueParams.labels` is a readonly array of strings (may be empty) | `[GAP]` |

---

## 7. Cross-Cutting Concerns

### 7.1 Retry Policy

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-CROSS-RETRY-001 | GitHub API calls are retried on transient failures (5xx, network errors) using `retryWithBackoff` | `[GAP]` |
| REQ-CROSS-RETRY-002 | LLxprt invocations are NOT retried (each call is expensive) | `[GAP]` |
| REQ-CROSS-RETRY-003 | Git operations are retried on lock-file contention | `[GAP]` |

### 7.2 Artifact Lifecycle

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-CROSS-ART-001 | Step outputs are written to the output directory managed by `OutputManager` | `[GAP]` |
| REQ-CROSS-ART-002 | The output directory is cleaned between issue runs | `[GAP]` |
| REQ-CROSS-ART-003 | JSONL outcome log survives cleanup | `[GAP]` |
| REQ-CROSS-ART-004 | Persisted state file survives cleanup | `[GAP]` |

### 7.3 Git Hygiene

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-CROSS-GIT-001 | Working branch is named `luther/fix-{issueNumber}` | `[GAP]` |
| REQ-CROSS-GIT-002 | Branch is created from the latest `main` (or default branch) | `[GAP]` |
| REQ-CROSS-GIT-003 | Working branch is deleted locally after successful merge | `[GAP]` |
| REQ-CROSS-GIT-004 | Dirty working directory is detected and aborted before starting a new issue | `[GAP]` |

### 7.4 Loop Limits and Convergence Detection

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-CROSS-LOOP-001 | Plan revise loops are bounded by `LutherConfig.luther.maxLoops` | `[GAP]` |
| REQ-CROSS-LOOP-002 | Test fix attempts are bounded by `LutherConfig.luther.maxTestFixAttempts` | `[GAP]` |
| REQ-CROSS-LOOP-003 | Exceeding any loop limit transitions to ABANDONING | `[GAP]` |
| REQ-CROSS-LOOP-004 | Loop counts are persisted across restarts | `[GAP]` |

### 7.5 Concurrent Execution Prevention

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-CROSS-LOCK-001 | Only one Luther instance can run against a given repo at a time | `[GAP]` |
| REQ-CROSS-LOCK-002 | A lock file is created on startup and removed on exit | `[GAP]` |
| REQ-CROSS-LOCK-003 | Stale lock files (from crashed instances) are detected and overridden | `[GAP]` |

### 7.6 TargetConfig

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-CROSS-TCONF-001 | TargetConfig is loaded from the target repo (e.g., `.luther.json`) | `[GAP]` |
| REQ-CROSS-TCONF-002 | Missing TargetConfig file throws an error containing the expected file path | `[GAP]` |
| REQ-CROSS-TCONF-003 | All profile names in `TargetConfig.profiles` are validated to be non-empty strings | `[GAP]` |
| REQ-CROSS-TCONF-004 | All command strings in TargetConfig (`testCommand`, `lintCommand`, `formatCommand`, `buildCommand`) are validated to be non-empty strings | `[GAP]` |

### 7.7 LutherConfig Validation

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-CROSS-LCONF-001 | `LutherConfig.luther.maxLoops` is validated as a positive integer (> 0, ≤ 100) | `[GAP]` |
| REQ-CROSS-LCONF-002 | `LutherConfig.luther.maxTestFixAttempts` is validated as a positive integer (> 0, ≤ 20) | `[GAP]` |
| REQ-CROSS-LCONF-003 | `LutherConfig.luther.idleSleepSeconds` is validated as a positive integer (> 0, ≤ 86400) | `[GAP]` |
| REQ-CROSS-LCONF-004 | `LutherConfig.targetRepo.localPath` is validated as a non-empty string that is an absolute path | `[GAP]` |
| REQ-CROSS-LCONF-005 | `LutherConfig.targetRepo.owner` is validated as a non-empty string | `[GAP]` |
| REQ-CROSS-LCONF-006 | `LutherConfig.targetRepo.name` is validated as a non-empty string | `[GAP]` |
| REQ-CROSS-LCONF-007 | `LutherConfig.github.lutherLogin` is validated as a non-empty string | `[GAP]` |
| REQ-CROSS-LCONF-008 | `LutherConfig.luther.crWaitTimeoutSeconds` is validated as a positive integer (> 0, ≤ 7200) | `[GAP]` |
| REQ-CROSS-LCONF-009 | `LutherConfig.luther.llxprtBinary` is validated as a non-empty string | `[GAP]` |
| REQ-CROSS-LCONF-010 | `LutherConfig.luther.defaultProfile` is validated as a non-empty string | `[GAP]` |
| REQ-CROSS-LCONF-011 | `LutherConfig.github.issueLabels.attempted` is validated as a non-empty string | `[GAP]` |
| REQ-CROSS-LCONF-012 | `LutherConfig.github.issueLabels.skip` is validated as a non-empty string | `[GAP]` |
| REQ-CROSS-LCONF-013 | `LutherConfig.github.issueLabels.flakyTest` is validated as a non-empty string | `[GAP]` |
| REQ-CROSS-LCONF-014 | Validation errors include the field name and the invalid value | `[GAP]` |

### 7.8 LutherConfig Negative-Path Validation

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-CROSS-LCONF-NEG-001 | `maxLoops = 0` is rejected as invalid (must be > 0) | `[GAP]` |
| REQ-CROSS-LCONF-NEG-002 | `maxLoops = -1` is rejected as invalid (must be > 0) | `[GAP]` |
| REQ-CROSS-LCONF-NEG-003 | `localPath = ""` (empty string) is rejected as invalid | `[GAP]` |
| REQ-CROSS-LCONF-NEG-004 | `owner = ""` (empty string) is rejected as invalid | `[GAP]` |
| REQ-CROSS-LCONF-NEG-005 | `name = ""` (empty string for repo name) is rejected as invalid | `[GAP]` |
| REQ-CROSS-LCONF-NEG-006 | `maxTestFixAttempts = 0` is rejected as invalid (must be > 0) | `[GAP]` |
| REQ-CROSS-LCONF-NEG-007 | `maxTestFixAttempts = -1` is rejected as invalid (must be > 0) | `[GAP]` |

### 7.9 Prompt Construction

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-CROSS-PROMPT-001 | Prompts are assembled as plain text with clearly delimited sections (issue context, plan, session, etc.) | `[GAP]` |
| REQ-CROSS-PROMPT-002 | Special characters in issue titles/bodies and CR comment bodies are not double-escaped when injected into prompts | `[GAP]` |
| REQ-CROSS-PROMPT-003 | Session content appended to prompts is truncated to a configurable maximum token/character count to avoid exceeding LLM context limits | `[GAP]` |
| REQ-CROSS-PROMPT-004 | Truncation preserves the most recent sections (newest content is retained, oldest is dropped) | `[GAP]` |

### 7.10 Error Taxonomy

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-CROSS-ERR-001 | Tool timeout errors (LLxprt subprocess killed by `timeoutSeconds`) are classified as fatal for the current step | `[GAP]` |
| REQ-CROSS-ERR-002 | Logical failures (e.g., LLxprt exits non-zero, schema validation failure) are classified as fatal for the current step | `[GAP]` |
| REQ-CROSS-ERR-003 | Parse failures (e.g., malformed JSON from LLxprt output) are classified as fatal for the current step | `[GAP]` |
| REQ-CROSS-ERR-004 | Transient network errors (GitHub API 5xx, DNS failure) are classified as retryable | `[GAP]` |
| REQ-CROSS-ERR-005 | Permanent API errors (GitHub 4xx, auth failure) are classified as fatal | `[GAP]` |

### 7.11 Context Reset Between Issue Runs

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-CROSS-RESET-001 | Session is cleared between issue runs (`session.clear()` called before `session.create()`) | `[GAP]` |
| REQ-CROSS-RESET-002 | Output directory is cleaned between issue runs (`output.clean()`) | `[GAP]` |
| REQ-CROSS-RESET-003 | Persisted state is cleared on transition from LOGGING to IDLE (no stale state carries over) | `[GAP]` |

### 7.12 Idempotency

Externally-mutating operations must be safe to retry or re-execute without
creating duplicate side effects. This is critical for resume after crash (§4.5)
and for retry logic (§4.6).

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-CROSS-IDEM-001 | `assignIssue` is idempotent: assigning an already-assigned issue to the same user does not error | `[GAP]` |
| REQ-CROSS-IDEM-002 | `unassignIssue` is idempotent: unassigning a user who is not assigned does not error | `[GAP]` |
| REQ-CROSS-IDEM-003 | `addLabel` is idempotent: adding a label that already exists on the issue does not error | `[GAP]` |
| REQ-CROSS-IDEM-004 | `submitPR` checks for an existing open PR on the same branch before creating a new one; if one exists, it returns the existing PR instead of creating a duplicate | `[GAP]` |

### 7.13 Cancellation Semantics

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-CROSS-CANCEL-001 | On SIGINT during an in-flight LLxprt subprocess, the subprocess is killed (SIGTERM) before the engine persists state and exits | `[GAP]` |
| REQ-CROSS-CANCEL-002 | On SIGINT during an in-flight `gh` CLI subprocess, the subprocess is killed (SIGTERM) before the engine persists state and exits | `[GAP]` |
| REQ-CROSS-CANCEL-003 | Subprocess cleanup on cancellation is best-effort: if the kill signal fails, the engine still proceeds to persist state and exit | `[GAP]` |

### 7.14 Clock Source Consistency

| ID | Requirement | Coverage |
| --- | --- | --- |
| REQ-CROSS-CLOCK-001 | All timestamps written to `HistoryEntry`, outcome JSONL, and persisted state use the same clock source (e.g., `Date.now()` or a clock abstraction) | `[GAP]` |
| REQ-CROSS-CLOCK-002 | All timestamps are formatted as ISO 8601 UTC strings | `[GAP]` |

---

## 8. Legacy-to-Canonical REQ-ID Mapping

Test assertion strings use legacy IDs that predate the canonical REQ-ID scheme
in this document. The legacy IDs in test descriptions are retained for backward
compatibility. This table is the definitive mapping.

| Legacy ID (in test descriptions) | Canonical ID (this document) | Test File |
| --- | --- | --- |
| REQ-SCAN-002 | REQ-STEP-SCAN-002 | scan-issues.test.ts |
| REQ-SCAN-003 | REQ-STEP-SCAN-003 | scan-issues.test.ts |
| REQ-SCAN-004 | REQ-STEP-SCAN-004 | scan-issues.test.ts |
| REQ-SCAN-005 | REQ-STEP-SCAN-005 | scan-issues.test.ts |
| REQ-SCAN-006 | REQ-STEP-SCAN-006 | scan-issues.test.ts |
| REQ-SCAN-010 | REQ-STEP-SCAN-007 | scan-issues.test.ts |
| REQ-SCAN-011 | REQ-STEP-SCAN-008 | scan-issues.test.ts |
| REQ-PLAN-001 | REQ-STEP-PLAN-001 | plan-fix.test.ts |
| REQ-PLAN-002 | REQ-STEP-PLAN-002 | plan-fix.test.ts |
| REQ-PLAN-003 | REQ-STEP-PLAN-003 | plan-fix.test.ts |
| REQ-PLAN-004 | REQ-STEP-PLAN-004 | plan-fix.test.ts |
| REQ-PLAN-006 | REQ-STEP-PLAN-006 | plan-fix.test.ts |
| REQ-REV-001 | REQ-STEP-REV-001 | review-plan.test.ts |
| REQ-REV-002 | REQ-STEP-REV-002 | review-plan.test.ts |
| REQ-REV-003 | REQ-STEP-REV-003 | review-plan.test.ts |
| REQ-REV-008 | REQ-STEP-REV-008 | review-plan.test.ts |
| REQ-IMPL-001 | REQ-STEP-IMPL-001 | implement-fix.test.ts |
| REQ-IMPL-002 | REQ-STEP-IMPL-002 | implement-fix.test.ts |
| REQ-IMPL-005 | REQ-STEP-IMPL-005 | implement-fix.test.ts |
| REQ-IMPL-007 | REQ-STEP-IMPL-006 | implement-fix.test.ts |
| REQ-PUSH-001 | REQ-STEP-PUSH-001 | commit-push.test.ts |
| REQ-PUSH-005 | REQ-STEP-PUSH-005 | commit-push.test.ts |
| REQ-SUB-006 | REQ-STEP-SUB-006 | submit-pr.test.ts |
| REQ-CIDR-001 | REQ-STEP-CIDR-001 | diagnose-ci.test.ts |
| REQ-CIDR-002 | REQ-STEP-CIDR-002 | diagnose-ci.test.ts |
| REQ-CIDR-004 | REQ-STEP-CIDR-004 | diagnose-ci.test.ts |
| REQ-CIRT-001 | REQ-STEP-CIDR-006 | diagnose-ci.test.ts |
| REQ-FLKY-001 | REQ-STEP-CIDR-007 | diagnose-ci.test.ts |
| REQ-FLKY-003 | REQ-STEP-CIDR-008 | diagnose-ci.test.ts |
| REQ-CRTG-001 | REQ-STEP-CRTG-001 | triage-cr.test.ts |
| REQ-CRSP-002 | REQ-STEP-CRSP-002 | respond-cr.test.ts |
| REQ-CRSP-003 | REQ-STEP-CRSP-003 | respond-cr.test.ts |
| REQ-CRSP-004 | REQ-STEP-CRSP-004 | respond-cr.test.ts |
| REQ-CRSP-005 | REQ-STEP-CRSP-005 | respond-cr.test.ts |
| REQ-CRSP-006 | REQ-STEP-CRSP-006, REQ-STEP-CRSP-007 | respond-cr.test.ts |
| REQ-REMED-001 | REQ-STEP-REMED-001 | remediate.test.ts |
| REQ-REMED-002 | REQ-STEP-REMED-002 | remediate.test.ts |
| REQ-REMED-004 | REQ-STEP-REMED-004 | remediate.test.ts |
| REQ-REMED-005 | REQ-STEP-REMED-005 | remediate.test.ts |
| REQ-REMED-006 | REQ-STEP-REMED-006 | remediate.test.ts |
| REQ-TFIX-001 | REQ-STEP-TFIX-001, REQ-STEP-TFIX-002 | fix-tests.test.ts |
| REQ-TFIX-002 | REQ-STEP-TFIX-003 | fix-tests.test.ts |
| REQ-ABAN-001 | REQ-STEP-ABAN-001, REQ-STEP-ABAN-004 | abandon-pr.test.ts |
| REQ-ABAN-002 | REQ-STEP-ABAN-002 | abandon-pr.test.ts |
| REQ-ABAN-003 | REQ-STEP-ABAN-003 | abandon-pr.test.ts |
| REQ-ABAN-006 | REQ-STEP-ABAN-006, REQ-STEP-ABAN-007, REQ-STEP-ABAN-008 | abandon-pr.test.ts |
| REQ-LOG-002 | REQ-STEP-LOG-001 | log-outcome.test.ts |
| REQ-LOG-004 | REQ-STEP-LOG-002 | log-outcome.test.ts |
| REQ-LOG-012 | REQ-STEP-LOG-009 | log-outcome.test.ts |
| REQ-SUCC-001 | REQ-STEP-LOG-010 | log-outcome.test.ts |
| REQ-SUCC-002 | REQ-STEP-LOG-011 | log-outcome.test.ts |
| REQ-SESS-001 | REQ-LIB-SESSION-001 | session.test.ts |
| REQ-SESS-002 | REQ-LIB-SESSION-002, REQ-LIB-SESSION-003, REQ-LIB-SESSION-004 | session.test.ts |
| REQ-SESS-003 | REQ-LIB-SESSION-006 | session.test.ts |
| REQ-SESS-004 | REQ-LIB-SESSION-005 | session.test.ts |
| REQ-OUT-001 | REQ-LIB-OUTPUT-001, REQ-LIB-OUTPUT-007 | output.test.ts |
| REQ-OUT-002 | REQ-LIB-OUTPUT-002 | output.test.ts |
| REQ-OUT-003 | REQ-LIB-OUTPUT-003, REQ-LIB-OUTPUT-004 | output.test.ts |
| REQ-OUT-004 | REQ-LIB-OUTPUT-005, REQ-LIB-OUTPUT-006 | output.test.ts |
| REQ-LOG-020 | REQ-LIB-LOG-001 | logger.test.ts |
| REQ-LOG-021 | REQ-LIB-LOG-004 | logger.test.ts |
| REQ-LOG-024 | REQ-LIB-LOG-002 | logger.test.ts |
| REQ-LOG-025 | REQ-LIB-LOG-003 | logger.test.ts |
| REQ-LOG-027 | REQ-LIB-LOG-005 | logger.test.ts |
| REQ-ENG-002 | REQ-ENG-020 | machine.test.ts |
| REQ-ENG-003 | REQ-ENG-021, REQ-ENG-022 | machine.test.ts |
| REQ-ENG-010 | REQ-ENG-001, REQ-ENG-003 | persistence.test.ts |
| REQ-ENG-011 | REQ-ENG-004 | persistence.test.ts |
| REQ-ENG-012 | REQ-ENG-005 | persistence.test.ts |
| REQ-ENG-013 | REQ-ENG-006, REQ-ENG-007 | persistence.test.ts |
| REQ-ENG-014 | (supplementary — supports REQ-ENG-004) | persistence.test.ts |
| REQ-ENG-015 | (supplementary — supports REQ-ENG-003) | persistence.test.ts |
| REQ-ENG-016 | REQ-ENG-009 | persistence.test.ts |
| REQ-ENG-020 | REQ-ENG-030 | runner.test.ts |
| REQ-SCHEMA-002 | REQ-ENG-008 | persistence.test.ts |
| REQ-TRANS-005 | REQ-ENG-RETRY-001, REQ-ENG-RETRY-002 | retry.test.ts |
| REQ-TRANS-005A | REQ-ENG-RETRY-003 through REQ-ENG-RETRY-007 | retry.test.ts |

---

## 9. Coverage Summary

### By Layer

| Layer | Total REQs | Covered | Partial | Gaps | Blocked |
| --- | --- | --- | --- | --- | --- |
| State Machine (§1) | 78 | 0 | 0 | 76 | 2 |
| Step Handlers (§2) | 130 | 42 | 12 | 70 | 6 |
| Libraries (§3) | 55 | 14 | 4 | 37 | 0 |
| Engine (§4) | 47 | 14 | 0 | 26 | 7 |
| CLI (§5) | 9 | 0 | 0 | 9 | 0 |
| Contract Completeness (§6) | 19 | 0 | 0 | 19 | 0 |
| Cross-Cutting (§7) | 64 | 0 | 0 | 64 | 0 |
| **Total** | **402** | **70** | **16** | **301** | **15** |
