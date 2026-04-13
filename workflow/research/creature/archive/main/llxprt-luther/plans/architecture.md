# Luther: Self-Evolving GitHub-Native Harness Agent

## Architecture Plan

### What Luther Is

A locally-run agent that targets a GitHub repository, picks up unassigned issues, attempts fixes, submits PRs, and uses structured feedback from CI, CodeRabbit, and human reviewers to improve itself over time. Luther runs the same loop against its own codebase, creating a recursive self-improvement cycle.

Luther is a **deterministic workflow with agentic steps where judgment is needed** — not an autonomous agent that decides what to do. The workflow skeleton is fixed; the intelligence is inserted at specific points.

### What Luther Is Not

- Not a GitHub Action (runs locally or on dedicated compute, not in runners)
- Not autonomous (operates within a fixed workflow, cannot modify its own eval system or monitor without approval)
- Not a single-model monolith (uses appropriate models per step — fast/cheap for triage, heavy for code generation)

---

## System Architecture

Luther consists of four concurrent systems:

```
┌─────────────────────────────────────────────────────┐
│                     MONITOR                          │
│  Health checks, crash recovery, revert-on-failure,   │
│  transient error detection, resource tracking         │
└──────────────┬──────────────────────┬────────────────┘
               │ watches              │ watches
┌──────────────▼──────────┐ ┌────────▼─────────────────┐
│     OUTER WORKFLOW      │ │   ENTREPRENEURIAL SYSTEM  │
│  Scan → Select → Fix → │ │  Analyze evals → Propose  │
│  Eval → Submit → Loop   │ │  improvements → File      │
│                         │ │  issues on Luther repo    │
└──────────────┬──────────┘ └────────▲─────────────────┘
               │ produces            │ consumes
┌──────────────▼──────────────────────┘────────────────┐
│                   EVAL SYSTEM                         │
│  Metrics collection, classification, retrospectives,  │
│  trend analysis, experience index                     │
└──────────────────────────────────────────────────────┘
```

---

## 1. Outer Workflow

The core execution loop. Deterministic skeleton, agentic steps marked with `[AGENT]`.

### Loop

```
1. SCAN target repo
   - Fetch all open issues (gh api)
   - Filter: unassigned, open, not labeled "luther-skip"
   - Any unassigned issue is a candidate — no priority filtering
     (Luther will eventually fix them all)

2. [AGENT] SELECT an issue to work on
   - Consult experience index: has Luther attempted this before?
     - If yes: what happened? what was tried? why did it fail?
     - Use this context to decide whether to retry or skip for now
   - Check: does this issue have enough information to attempt?
   - Assign the issue to Luther (gh api)

3. [AGENT] PLAN the fix
   - Read relevant code (context-engineered — function signatures
     first, full bodies only where needed)
   - Read related test files
   - Read experience index entries for similar past issues
   - Produce a structured plan (which files to change, what tests
     to add/modify, expected behavior)

4. [AGENT] IMPLEMENT the fix
   - Create a branch
   - Make changes
   - Add/update tests
   - Commit with structured message referencing the issue

5. EVAL the branch (deterministic)
   - Run the project's test suite locally
   - Run linting/formatting checks
   - Run any project-specific checks
   - Compare results against main branch baseline

6. [AGENT] ASSESS results
   - Did tests pass? If not, analyze failures
   - Is this a real improvement or did it break something?
   - Decision: iterate (go to 4), abandon, or submit

7. SUBMIT PR (deterministic)
   - Create PR with structured body (issue ref, what changed, 
     test results, any known limitations)
   - Wait for CI + CodeRabbit

8. [AGENT] RESPOND to feedback
   - Read CI results, CodeRabbit comments
   - For each CodeRabbit issue: evaluate against source code,
     fix if valid, respond explaining action taken
   - Push fixes, wait for re-check
   - Repeat until: all workflows pass AND all CodeRabbit issues
     resolved — or abandon if not converging

9. LOG outcome to experience index
   - Record: issue, approach, result, iterations, 
     failure classifications, time spent, tokens consumed

10. LOOP → go to 1
```

### Issue Assignment

Wide open: any unassigned issue is fair game. No priority gating. Luther will work through them all over time. The eval system tracks what it's good and bad at — the entrepreneurial system uses that data to improve Luther's capabilities rather than filtering which issues it attempts.

If Luther abandons an issue (step 6), it unassigns itself and adds a structured comment explaining why, then labels it "luther-attempted" with detail. It can retry later when the experience index suggests a different approach might work.

### Convergence Detection

If step 8 loops more than N times (configurable, default 3) without all checks passing, Luther abandons the PR. This is a failure — logged, classified, and fed to the eval system. The PR is closed with a structured comment, not left dangling.

---

## 2. Eval System

Collects metrics, classifies outcomes, maintains the experience index, and runs retrospectives.

### Metrics (Collected Per Cycle)

**Volume metrics:**
- Issues attempted today/this week
- PRs submitted today/this week
- PRs merged vs. rejected vs. abandoned

**Efficiency metrics:**
- Commits-after-submission to clean PR (lower is better)
- Time from issue assignment to PR submission
- Token cost per issue (total tokens consumed across all steps)
- Abandonment rate (issues started but not completed)

**Quality metrics:**
- First-attempt CI pass rate
- CodeRabbit issue count per PR (lower is better)
- Regression rate (PRs that break existing tests)

**Self-improvement metrics (when Luther targets its own repo):**
- Did the last self-modification improve any of the above metrics?
- Did it make anything worse?
- Any change that doesn't improve things is classified as neutral or failure
  — there are no "lateral moves." If it didn't measurably help, it didn't help.

### Outcome Classification

Fixed taxonomy, not freeform. Every outcome gets classified:

**Loop reasons (why did a PR need additional commits):**
```
test_failure        — existing or new tests failed
lint_failure        — formatting/linting violations
coderabbit_issue    — valid CodeRabbit feedback requiring changes
build_failure       — compilation/build errors
merge_conflict      — branch conflicts with main
type_error          — type checking failures
missing_tests       — reviewer/CR requested additional test coverage
```

**Rejection/abandonment reasons:**
```
wrong_approach      — fundamental approach was incorrect
incomplete_fix      — fix was partial, didn't fully resolve the issue
regression          — fix broke something else
out_of_scope        — PR changed things beyond the issue
insufficient_tests  — not enough test coverage for the change
style_violation     — didn't match project conventions
non_convergent      — couldn't get to clean PR within loop limit
issue_unclear       — issue didn't have enough information to fix
```

**Transient/system failure reasons (separate category — NOT counted as Luther failures):**
```
api_unavailable     — GitHub API, LLM API, or other service down
rate_limited        — hit API rate limits
resource_exhausted  — out of memory, disk, tokens budget
network_error       — connectivity issues
service_timeout     — external service timed out
infra_config_error  — environment/config problem, not Luther's fault
```

Transient failures trigger a backoff-and-retry, not an abandonment. The eval system tracks them separately so they don't pollute Luther's quality metrics. If transient failures persist beyond a threshold (configurable), the monitor escalates.

### Retrospectives

After each eval cycle (daily or configurable), the eval system produces a structured retrospective:

```
[AGENT] Analyze the last cycle's outcomes:
- What patterns emerge in the failure classifications?
- Are certain types of issues consistently failing? Why?
- Did loop counts increase or decrease? Why?
- Are transient failures masking real problems?
- Did any self-modifications correlate with metric changes?
- What specifically caused each failure or regression?
```

The retrospective is structured data (not prose), appended to the experience index. It identifies **root causes**, not just symptoms. "test_failure" is a symptom; "Luther doesn't check for null returns from database queries" is a root cause.

### Experience Index

The git history and issue/PR history ARE the archive — the complete, immutable record. But they're not digestible for an LLM context window. The experience index is a **structured, queryable summary** that sits alongside the git history, not instead of it.

Format: a local file or lightweight store (JSON/SQLite) with entries like:

```json
{
  "issue": "target-repo#142",
  "type": "bug_fix",
  "area": "auth/session_handling", 
  "attempted": "2026-04-15",
  "approach": "Added null check before session.refresh() call",
  "outcome": "merged",
  "loops": 1,
  "tokens": 45000,
  "failure_reasons": [],
  "lessons": "Session handling code requires null checks on all refresh paths",
  "related_issues": ["target-repo#98", "target-repo#115"],
  "branch": "luther/fix-142",
  "pr": "target-repo#156"
}
```

For failed/abandoned attempts:

```json
{
  "issue": "target-repo#87",
  "type": "feature",
  "area": "api/pagination",
  "attempted": "2026-04-12",
  "approach": "Tried cursor-based pagination replacement",
  "outcome": "abandoned",
  "loops": 3,
  "tokens": 120000,
  "failure_reasons": ["non_convergent", "regression"],
  "lessons": "Pagination changes cascade to 12 consumer endpoints — need to update all consumers, not just the API layer",
  "related_issues": [],
  "branch": "luther/fix-87",
  "pr": null,
  "retrospective": "Root cause: underestimated scope. The fix touched api/paginate.py but consumers in cli/, web/, and sdk/ all hardcode offset-based params."
}
```

This serves as RAG context. When Luther considers an issue, it queries the experience index for:
- Prior attempts on this exact issue
- Prior attempts on similar issues (same area, same type)
- Patterns from recent retrospectives

This prevents "I want to do something we already tried." If the index shows a prior attempt failed with a specific root cause, Luther either addresses that root cause or skips the issue.

The experience index is append-only. Entries are never modified or deleted. New attempts on the same issue create new entries, linked to prior ones.

---

## 3. Entrepreneurial System

Analyzes eval data and proposes improvements to Luther itself. Runs on a configurable cadence (e.g., after every N eval cycles, or weekly).

### Process

```
1. [AGENT] ANALYZE recent eval data
   - Pull metrics, classifications, retrospectives from eval system
   - Identify patterns:
     - What failure types are most common?
     - What issue types have lowest success rate?
     - Where is Luther spending the most tokens for the least return?
     - Did previous self-modifications help, hurt, or do nothing?

2. [AGENT] PROPOSE improvements
   - Generate specific, scoped proposals:
     GOOD: "Add a pre-check step that runs affected tests before 
            making changes, to establish a baseline"
     BAD:  "Improve code quality" (too vague)
     BAD:  "Rewrite the planning system" (too broad)
   - Each proposal must state:
     - What specific metric it targets
     - What the expected improvement is
     - What could go wrong

3. COMPARE against experience index
   - Has Luther tried this before? What happened?
   - If a similar improvement was tried and classified as 
     neutral/failure, the proposal must explain what's different
     this time

4. EVALUATE previous self-modifications
   - For each recent self-modification: did it improve the 
     targeted metric?
   - Any modification that didn't measurably improve things is
     classified as neutral or failure — no free passes
   - Why did it succeed or fail? Root cause analysis.

5. FILE issues on Luther's own repo
   - Each approved proposal becomes a GitHub issue on Luther's repo
   - Structured format: problem, proposed solution, target metric,
     expected impact, risks, prior attempts if any

6. Luther's own outer workflow picks up these issues
   - Same loop: select → plan → implement → eval → submit → respond
   - PRs to Luther's own repo go through the same CI/review process
   - The monitor watches for regressions after self-modifications
```

### Constraints

- **Eval modifications require human approval.** Luther can suggest new evals or modifications to existing ones by filing issues labeled "eval-proposal." These are not auto-assigned. A human reviews and approves before they enter the eval system. This is the modification frontier — Luther cannot change how it's measured.
- **Monitor modifications require human approval.** Same mechanism. Luther cannot weaken its own safety guardrails.
- **Outer workflow structure is frozen.** Luther can modify *how* it performs each step (better prompts, different context assembly, improved tool use) but cannot add or remove steps from the outer workflow without human approval.
- **One self-modification at a time.** To enable attribution, Luther should not stack multiple self-modifications in the same eval period. Change one thing, measure the effect, then decide on the next change. This matches the SMAS principle of minimal necessary modification.

### What Luther CAN Modify Without Approval

- Its own prompts and instructions for each agentic step
- Its context assembly strategy (what information to include/exclude per step)
- Its tool implementations (how it reads files, searches code, etc.)
- Its retry and error handling logic
- Its issue selection heuristics
- Its branch naming, commit message, and PR body templates

### What Luther CANNOT Modify Without Approval

- The eval system's metrics definitions and computation
- The monitor's health checks and revert logic
- The outer workflow's step sequence
- The experience index schema
- The modification frontier itself
- The convergence/abandonment thresholds

---

## 4. Monitor

Watches Luther's health, detects failures, handles transient issues, and reverts bad self-modifications.

### Health Checks (Continuous)

```
1. IS LUTHER RUNNING?
   - Process alive check
   - Last activity timestamp (if no activity for N minutes, alert)
   
2. IS LUTHER MAKING PROGRESS?
   - Has it completed at least one outer loop iteration recently?
   - Is it stuck in an infinite loop on one issue?
   - Is it consuming tokens but producing no PRs or abandonment decisions?

3. ARE EXTERNAL SERVICES AVAILABLE?
   - GitHub API responsive?
   - LLM API responsive?
   - Target repo accessible?
   - Can Luther push branches?
```

### Transient Failure Handling

Not all failures are Luther's fault. The monitor distinguishes:

**Transient failures (retry with backoff):**
- API rate limits → exponential backoff, resume where left off
- Network errors → retry after delay
- Service timeouts → retry with extended timeout
- LLM API errors (500s, overloaded) → backoff and retry
- GitHub API outages → pause loop, resume when available

**Resource constraints (pause and alert):**
- Token budget exhausted → pause, alert, wait for budget reset or human intervention
- Disk full → pause, alert
- Memory pressure → pause current issue, restart with lighter context

**Transient failures are logged separately in the eval system** and do not count against Luther's quality metrics. But if transient failures become chronic (e.g., rate limiting every cycle), the monitor flags this as a systemic issue requiring human attention.

### Regression Detection After Self-Modification

When Luther modifies its own code (via the entrepreneurial system's issues):

```
1. Record Luther's baseline metrics before the modification
2. After modification is applied, run Luther for a configurable 
   eval period (e.g., N issues or T hours)
3. Compare post-modification metrics to baseline
4. If metrics regressed:
   a. Log the regression with root cause analysis
      [AGENT] Why did this change make things worse? 
      Analyze the specific failures that occurred post-change 
      vs. pre-change patterns
   b. Revert the change (git revert on Luther's repo)
   c. Restart Luther from the reverted state
   d. File a retrospective entry in the experience index:
      what was tried, why it failed, root cause
5. If metrics are neutral (no improvement):
   a. Classify as neutral/failure (no free passes)
   b. Revert the change
   c. Log why it didn't help
6. If metrics improved:
   a. Keep the change
   b. Log the improvement with attribution
```

### Crash Recovery

```
1. Luther crashes →
2. Monitor detects (process exit, no heartbeat)
3. Check: was this the first crash since last self-modification?
   - YES → likely caused by the self-modification. Revert it, restart.
   - NO → check logs for root cause
4. Check: is it a transient issue?
   - API down, resource exhaustion, etc. → wait, retry
5. Check: is it a config issue?
   - Missing env vars, bad credentials, repo access → alert human
6. None of the above → alert human with full diagnostic context
```

---

## Modification Frontier

Explicit boundary between what Luther can and cannot change autonomously.

| Component | Luther Can Modify | Requires Human Approval |
|---|---|---|
| Target repo code | [OK] (via PRs) | Merge decision is human's |
| Luther's own prompts/tools | [OK] (via self-PRs) | Monitor watches for regression |
| Luther's context assembly | [OK] | Monitor watches for regression |
| Luther's error handling | [OK] | Monitor watches for regression |
| Eval metrics & computation | [ERROR] | Must file "eval-proposal" issue |
| Monitor logic | [ERROR] | Must file issue, human applies |
| Outer workflow steps | [ERROR] | Must file issue, human applies |
| Experience index schema | [ERROR] | Must file issue, human applies |
| Abandonment thresholds | [ERROR] | Must file issue, human applies |
| Modification frontier | [ERROR] | Never self-modifiable |

---

## Experience Index: The Digestible Archive

Git history + issue/PR history = the complete, immutable archive. This is the equivalent of the DGM's agent archive. It's authoritative and tamper-proof.

The experience index = a structured, queryable summary that makes the archive useful to an LLM within a context window. It's a derived artifact, not a replacement.

### What It Contains

- One entry per issue attempt (success, failure, or abandonment)
- One entry per self-modification attempt (with before/after metrics)
- Retrospective summaries (daily or per-cycle)
- Links back to the actual git commits, PRs, and issues (the authoritative source)

### How It's Used

1. **Before attempting an issue:** query for prior attempts on this issue or similar issues. Include relevant entries in the agentic context for step 3 (PLAN).
2. **Before proposing a self-modification:** query for prior self-modifications in the same area. The entrepreneurial system must acknowledge prior attempts and explain what's different.
3. **During retrospectives:** aggregate recent entries to identify patterns.

### Implementation

Lightweight local store — SQLite or JSON lines file. Not a heavy vector database. The experience index is small enough (thousands of entries, not millions) that keyword search + recency weighting is sufficient. If it grows large enough to need embeddings, that's a good problem to have later.

The index file lives in Luther's own repo, version-controlled alongside Luther's code. It's append-only within a run, but the file itself is committed periodically so the git history captures the index's evolution too.

---

## The Recursive Loop

Luther runs two instances of the same outer workflow:

**Instance 1: Luther → Target Repo**
- Picks up issues from the target repo
- Submits PRs to the target repo
- Eval system measures PR quality, acceptance rate, efficiency

**Instance 2: Luther → Luther's Own Repo**
- Picks up issues from Luther's repo (filed by the entrepreneurial system)
- Submits PRs to Luther's repo
- Eval system measures whether self-modifications improved Instance 1's metrics
- Monitor watches for regressions and reverts bad changes

The entrepreneurial system bridges the two: it reads Instance 1's eval data and files issues for Instance 2 to work on.

```
Instance 1 (target repo work)
    │ produces metrics
    ▼
Eval System
    │ feeds data to
    ▼
Entrepreneurial System
    │ files issues on Luther's repo
    ▼
Instance 2 (self-improvement work)
    │ modifies Luther's code
    ▼
Monitor (watches for regressions)
    │ if regression: revert + restart
    │ if improvement: keep + log
    ▼
Instance 1 (now running improved Luther)
```

---

## Bootstrapping Plan

### Phase 0: Manual Setup
- Luther's codebase exists with the outer workflow implemented
- Eval system collects metrics but entrepreneurial system is disabled
- Hand-written skill documents (2-3 focused ones): how to read the target repo, where tests are, PR conventions, coding style
- Monitor runs but only does health checks

### Phase 1: Target Repo Only
- Luther runs Instance 1 against the target repo
- Picks up issues, attempts fixes, submits PRs
- Eval system collects data
- Human reviews PRs normally
- No self-modification — just establishing baseline metrics and filling the experience index

### Phase 2: Entrepreneurial System Enabled
- Entrepreneurial system starts analyzing eval data
- Files improvement issues on Luther's repo
- Human reviews and applies these manually (not Luther)
- This validates the entrepreneurial system's judgment before giving it autonomy

### Phase 3: Self-Modification Enabled
- Instance 2 starts running: Luther works on its own issues
- Monitor actively watches for regressions
- Self-modification PRs still require human merge approval initially
- Eval system tracks self-modification effectiveness

### Phase 4: Increased Autonomy
- Self-modification PRs can be auto-merged if:
  - CI passes
  - CodeRabbit has no unresolved issues
  - Monitor's regression detection doesn't trigger
- Human reviews become periodic audits rather than per-PR gates
- The modification frontier remains enforced

---

## Open Design Decisions

1. **Cadence:** How often does the entrepreneurial system run? After every N issues? Daily? Triggered by metric thresholds?
2. **Eval period for self-modifications:** How many issues/how much time before deciding if a self-modification helped?
3. **Multiple target repos:** Can one Luther instance work on multiple target repos simultaneously, or one at a time?
4. **Model selection:** Which models for which steps? Fast/cheap for triage and classification, heavy for code generation and planning?
5. **Token budgets:** Per-issue token limit? Per-day token limit? How to handle budget exhaustion?
6. **Concurrency:** Can Luther work on multiple issues simultaneously (parallel branches) or strictly serial?
7. **Human escalation:** When does Luther ask for help instead of abandoning an issue?

---

## Research Grounding

This architecture is grounded in the following research findings:

| Design Decision | Research Basis |
|---|---|
| Deterministic workflow + agentic steps | InfoWorld/CodeRabbit: hybrid architecture achieves 88.8% GCR |
| Fixed outcome taxonomy | ACE paper: structured bullets +10.6% vs. unstructured |
| Experience index (not just raw history) | MemInsight: +34% recall with semantic metadata augmentation |
| Modification frontier | SMAS: agents cannot expand their own modification boundary |
| Neutral = failure for self-modifications | DGM/GEA: only measurable improvement counts |
| Revert-on-regression | SMAS: Lyapunov stability — every change must measurably improve |
| Separate proposer and evaluator | ACE Reflector role: removing it significantly degrades performance |
| One modification at a time | SMAS: minimal necessary modification, regularization term |
| Eval modifications require approval | DGM safety: agents hack reward functions when they can |
| Append-only experience index | SMAS: immutable audit trail requirement |
| Archive = git history | DGM: maintaining diverse agent archive enables breakthroughs |
| Phased bootstrapping | Self-Improving Coding Agent: baseline must be functional first |
| Transient failure separation | Outcome-Oriented Eval: measure what matters, exclude noise |
| Retrospectives with root cause | SE-Agent: trajectory analysis (revision/recombination/refinement) |
| Skill docs hand-curated | SkillsBench: human-curated +16.2pp, self-generated -1.3pp |
| Focused context per step | Distracting Effect: irrelevant context degrades performance 6-11pp |
