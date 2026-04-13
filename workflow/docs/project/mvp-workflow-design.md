# Luther MVP Workflow Design

## 1. Overall Goal

Luther is a self-improving automated software engineering system. It takes GitHub issues, produces fixes, and submits PRs — then measures its own performance and proposes improvements to itself.

### Three horizons

1. **MVP (this doc)**: Take llxprt-code issues → analyze → plan → implement → test → submit PR. A deterministic workflow replaces the existing "luther" GitHub Action with explicit step routing and hard pass/fail gates.

2. **Eval loop**: Measure performance per run (CodeRabbit review count, CI loop count, PR accepted/rejected, time to merge). Store metrics. Surface trends.

3. **Self-improvement**: Luther proposes changes to its own workflow definitions, executor configs, and prompts based on performance data. Human approves or rejects.

## 2. Architecture: Engine vs Workflow

The system has two strictly separated layers:

```
┌─────────────────────────────────────────────────────┐
│                  Workflow Definitions                │
│                                                     │
│  config/workflows/issue-fix-v1.toml    ← MVP        │
│  config/workflows/docs-update-v1.toml  ← future     │
│  config/workflows/dep-audit-v1.toml    ← future     │
│  config/workflow-configs/profile-0.toml             │
│                                                     │
│  These are DATA. They define step graphs,           │
│  transitions, and guard limits. They contain        │
│  zero business logic.                               │
└────────────────────┬────────────────────────────────┘
                     │ loaded by
                     ▼
┌─────────────────────────────────────────────────────┐
│                  Workflow Engine                     │
│                                                     │
│  EngineRunner: sequential step loop                 │
│  ExecutorRegistry: step_type → executor dispatch    │
│  StepContext: work_dir, variables, inter-step state  │
│  Persistence: checkpoints, events, resume           │
│  Monitor: heartbeat, singleton, restart policy      │
│                                                     │
│  The engine is GENERIC. It knows nothing about      │
│  GitHub, issues, PRs, CodeRabbit, or llxprt.        │
│  It executes steps and routes on outcomes.           │
└────────────────────┬────────────────────────────────┘
                     │ dispatches to
                     ▼
┌─────────────────────────────────────────────────────┐
│                  Step Executors                      │
│                                                     │
│  ShellExecutor     ← runs commands, maps exit codes │
│  WriteFileExecutor ← writes files with interpolation│
│  GhExecutor        ← GitHub CLI + JSON parsing (NEW)│
│  GitExecutor       ← git operations (NEW)          │
│  VerifyExecutor    ← build/test/lint → structured  │
│                      failure reports (NEW)          │
│  LlxprtExecutor    ← invokes llxprt-code (NEW)     │
│  EvalExecutor      ← records metrics (NEW)         │
│                                                     │
│  Executors are PLUGGABLE. The workflow definition    │
│  references step_type strings; the registry maps    │
│  them to executors at runtime.                      │
└─────────────────────────────────────────────────────┘
```

### Why this separation matters

- **Workflow definitions are data, not code.** Changing the issue-fix workflow (adding a step, reordering, adjusting guards) is a TOML edit, not a code change.
- **Multiple workflows coexist.** `issue-fix-v1`, `docs-update-v1`, and `self-improve-v1` are different TOML files using the same engine.
- **Workflows are versioned.** `issue-fix-v2` can exist alongside `v1`. Config profiles control which version runs.
- **The engine never hard-codes domain logic.** It doesn't know what "CodeRabbit" is. It knows that step X returned `fixable` and the transition table says to go to step Y.

## 3. The Key Insight: Deterministic Routing Over LLM Judgment

Today's approach (the luther GitHub Action and manual LLxprt sessions) lets the LLM decide both **what to do** and **what to do next**. The LLM analyzes, plans, implements, AND decides whether its own implementation is good enough to proceed.

This is the source of most failures:
- LLM says "tests pass" when they don't
- LLM skips verification steps when context gets long
- LLM decides to proceed despite placeholder code
- LLM can't objectively evaluate its own output

**Luther's workflow inverts this.** The LLM does the creative work (analysis, planning, code generation) but the workflow engine controls routing based on **hard outcomes**:

| Decision Point | Old: LLM Decides | New: Workflow Decides |
|---|---|---|
| "Did tests pass?" | LLM reads output, might hallucinate | `cargo test` exit code: 0 → success, non-zero → fixable |
| "Is there placeholder code?" | LLM greps, might miss things | `grep -rn "todo!\|unimplemented!"` exit code check |
| "Did CI pass?" | LLM checks, might misread | `gh pr checks` parsed deterministically |
| "Did CodeRabbit approve?" | LLM reads comments | `gh pr view` + structured comment parsing |
| "Should we retry or abandon?" | LLM vibes | Loop counter vs max_iterations guard |

The LLM is the **hands**. The workflow is the **brain** for routing decisions.

## 4. MVP Workflow: issue-fix-v1

### 4.1 Deterministic vs LLM Split

Most of the workflow is deterministic. Only the creative steps need an LLM.

**Deterministic steps (no LLM, hard pass/fail):**

| Step | What It Does | Tool |
|---|---|---|
| select_issue | Query open issues by label/priority, pick one | `gh issue list` |
| fetch_issue | Get issue details (title, body, labels) | `gh issue view --json` |
| setup_branch | Create worktree, checkout new branch | `git worktree add`, `git checkout -b` |
| verify_local | Run project checks → structured failure report | lint/typecheck/test/format/build (parsed) |
| commit_push | Stage, commit, push | `git add/commit/push` |
| submit_pr | Create PR with title/body | `gh pr create` |
| watch_ci | Poll PR checks until done | `gh pr checks --watch` |
| check_review | Fetch review comments, parse approval status | `gh pr view --json` |
| record_result | Write metrics to store | shell / SQL |
| abandon_and_log | Comment on issue, clean up branch | `gh issue comment`, `git` |

**LLM steps (need llxprt-code):**

| Step | What It Does |
|---|---|
| analyze | Read codebase + issue, produce analysis |
| plan | Generate implementation plan from analysis |
| implement | Write code changes according to plan |
| remediate | Fix failures based on test/CI/review output |
| address_review | Fix code review comments |

That's **10 deterministic steps** and **5 LLM steps**. The LLM does the thinking; everything else is mechanical.

### 4.2 Step Graph

```
 ┌────────────────┐
 │ select_issue    │  gh issue list --label luther --json | pick one
 └──────┬─────────┘
        │ success
        ▼
 ┌────────────────┐
 │ fetch_issue     │  gh issue view N --json title,body,labels
 └──────┬─────────┘
        │ success
        ▼
 ┌────────────────┐
 │ setup_branch    │  git worktree + checkout -b luther/fix-{issue_number}
 └──────┬─────────┘
        │ success
        ▼
 ┌────────────────┐
 │ analyze         │  [LLM] read codebase + issue → analysis doc
 └──────┬─────────┘
        │ success
        ▼
 ┌────────────────┐
 │ plan            │  [LLM] analysis → implementation plan
 └──────┬─────────┘
        │ success
        ▼
 ┌────────────────┐
 │ implement       │  [LLM] plan → write code
 └──────┬─────────┘
        │ success
        ▼
 ┌────────────────┐
 │ verify_local    │  lint + typecheck + test + format + build → parsed
 └──────┬─────────┘
        │ success                          fixable
        ▼                                     │
 ┌────────────────┐                 ┌─────────┴────────┐
 │ commit_push     │                │ remediate [LLM]   │
 └──────┬─────────┘                └─────────┬────────┘
        │ success                            │ success
        ▼                                    ▼
 ┌────────────────┐               (back to verify_local)
 │ submit_pr       │  gh pr create
 └──────┬─────────┘
        │ success
        ▼
 ┌────────────────┐
 │ watch_ci        │  gh pr checks --watch
 └──────┬─────────┘
        │ success                          fixable
        ▼                                     │
 ┌────────────────┐                 ┌─────────┴────────┐
 │ check_review    │                │ remediate [LLM]   │──→ verify_local
 └──────┬─────────┘                └──────────────────┘
        │ success         fixable
        ▼                    │
 ┌────────────────┐  ┌──────┴─────────────┐
 │ record_result   │  │ address_review [LLM]│──→ verify_local
 └────────────────┘  └────────────────────┘

 At any point: fatal → abandon_and_log
 Guard limits exceeded: abandon → abandon_and_log
```

### 4.3 Step Details

#### Deterministic Steps

##### select_issue
- **step_type**: `gh`
- **what it does**: `gh issue list --repo {target_repo} --label "luther" --state open --json number,title,labels --limit 10` then pick the highest-priority unassigned issue
- **outcome mapping**: found issue → success (sets `{issue_number}`), no eligible issues → fatal (nothing to do)
- **context output**: `{issue_number}`
- **selection criteria** (configurable in workflow config):
  - Label filter (e.g., `luther`, `bug`, `good-first-issue`)
  - Exclude already-assigned issues
  - Priority ordering: by label weight, then by age (oldest first)

##### fetch_issue
- **step_type**: `gh`
- **what it does**: `gh issue view {issue_number} --repo {target_repo} --json title,body,labels,assignees,comments`
- **outcome mapping**: exit 0 → success, non-zero → fatal
- **context output**: `{issue_title}`, `{issue_body}`, `{issue_labels}`, `{issue_comments}`

##### setup_branch
- **step_type**: `git`
- **what it does**:
  1. `git fetch origin` — ensure up to date
  2. `git checkout -b luther/fix-{issue_number} origin/{base_branch}` — create branch from base
  3. Validate clean state
- **outcome mapping**: branch created → success, conflict/error → fatal
- **context output**: `{branch_name}`

##### verify_local
- **step_type**: `verify` (dedicated executor — not a generic shell step)
- **what it does**: Runs a configurable sequence of verification commands, parses structured output from each, and produces a machine-readable failure report.
- **commands run** (configurable per-project in workflow config):
  - MVP: llxprt-code (Node/TypeScript) — lint → typecheck → test → format → build → smoke run
  - Later: luther-workflow (Rust) — `cargo build --all-targets` → `cargo test` → `cargo clippy -- -D warnings` → placeholder grep
  - The check suite is a config parameter, not hardcoded in the executor — Rust support comes when Luther works on itself
- **outcome mapping**: all pass → success, any fail → fixable
- **context output** (structured, not raw stderr):
  - `{verify_passed}` — boolean, did everything pass
  - `{verify_summary}` — "lint: pass, typecheck: 2 errors, test: 3 failed, format: pass, build: pass"
  - `{build_errors}` — JSON array of `{file, line, message}` (empty if build passed)
  - `{test_failures}` — JSON array of `{test_name, file, line, assertion, expected, actual}` 
  - `{lint_errors}` — JSON array of `{file, line, rule, message}`
  - `{type_errors}` — JSON array of `{file, line, code, message}`
- **why a dedicated executor**: The remediate/LLM step gets a *structured* failure report — not raw compiler/tsc output to parse. "Test `reads file content` at src/tools/__tests__/read-file.test.ts:23 failed: expected `Hello, World!` got `Hello, World`" is vastly more actionable for the LLM than 200 lines of test runner output. The workflow can also use the structured data to make routing decisions (e.g., type error vs test failure vs lint warning).
- **this is the key deterministic gate** — no LLM judgment anywhere in this step

##### commit_push
- **step_type**: `git`
- **what it does**: `git add -A && git commit -m "{commit_message}" && git push -u origin {branch_name}`
- **outcome mapping**: exit 0 → success, non-zero → fatal

##### submit_pr
- **step_type**: `gh`
- **what it does**: `gh pr create --repo {target_repo} --title "{pr_title}" --body "{pr_body}" --base {base_branch}`
- **outcome mapping**: PR created → success, error → fatal
- **context output**: `{pr_number}`, `{pr_url}`
- **note**: PR title must include issue reference (e.g., "Fix foo (Fixes #{issue_number})")

##### watch_ci
- **step_type**: `gh`
- **what it does**: `gh pr checks {pr_number} --repo {target_repo} --watch --interval 300`
- **outcome mapping**: all green → success, any failure → fixable
- **context output**: `{ci_status}`, `{ci_failures}` (parsed failure names + logs)

##### check_review
- **step_type**: `gh`
- **what it does**:
  1. `gh pr view {pr_number} --repo {target_repo} --json reviews,comments`
  2. Parse CodeRabbit comments (filter bot comments by author)
  3. Parse human review status (approved / changes_requested / pending)
- **outcome mapping**:
  - Approved with no unresolved comments → success
  - Changes requested or unresolved CodeRabbit comments → fixable
  - No reviews yet → fixable (wait/retry)
- **context output**: `{review_status}`, `{review_comments}` (structured: author, body, resolved)

##### record_result
- **step_type**: `shell`
- **what it does**: Write run metrics to persistent store (SQLite or JSON)
- **context inputs**: `{run_id}`, `{issue_number}`, `{pr_number}`, outcome, loop counts, timing
- **outcome mapping**: always success (logging failure shouldn't kill the run)

##### abandon_and_log
- **step_type**: `gh` + `git`
- **what it does**:
  1. `gh issue comment {issue_number} --body "Luther failed: {abandon_reason}"`
  2. Clean up branch if configured
  3. Record failure metrics
- **outcome mapping**: always terminal

#### LLM Steps

##### analyze
- **step_type**: `llxprt`
- **what it does**: Invokes llxprt-code in `{work_dir}` with goal: "Analyze issue #{issue_number}: {issue_title}" + issue body as context
- **outcome mapping**: llxprt exits 0 → success, non-zero → fixable
- **context output**: `{analysis}` (captured from llxprt output or written file)

##### plan
- **step_type**: `llxprt`
- **what it does**: Invokes llxprt-code with the analysis, asks for concrete implementation plan
- **outcome mapping**: llxprt exits 0 → success, non-zero → fixable
- **context output**: `{plan}`

##### implement
- **step_type**: `llxprt`
- **what it does**: Invokes llxprt-code with the plan, asks it to write the code changes
- **outcome mapping**: llxprt exits 0 → success, non-zero → fixable
- **context output**: modified files in work_dir

##### remediate
- **step_type**: `llxprt`
- **what it does**: Invokes llxprt-code with structured failure data from verify_local (`{test_failures}`, `{build_errors}`, `{type_errors}`, `{lint_errors}`) or CI (`{ci_failures}`), asks it to fix the specific problems identified
- **key advantage**: The LLM doesn't parse raw test/compiler output — it gets pre-parsed structured reports like "test `reads file content` at src/tools/__tests__/read-file.test.ts:23 failed: expected `Hello, World!` got `Hello, World`"
- **outcome mapping**: llxprt exits → success (flows back to verify_local)
- **guard**: bounded by max_iterations — if remediation loops N times → abandon

##### address_review
- **step_type**: `llxprt`
- **what it does**: Invokes llxprt-code with `{review_comments}`, asks it to address each comment
- **outcome mapping**: exits → success (flows back to verify_local → commit_push → watch_ci)
- **guard**: bounded by max_iterations

### 4.4 What This Replaces

The existing "luther" GitHub Action in llxprt-code does roughly the same work but:
- The LLM decides routing (it reads test output and decides whether to continue)
- There are no hard loop guards (it can spin forever or give up too early)
- There's no checkpoint/resume (if it crashes, start over)
- There's no metric collection (no eval data)
- There's no separation between the workflow shape and the execution engine

## 5. New Executors Required

The engine currently has `shell`, `write_file`, and `noop`. The MVP needs three new executors, in priority order:

### 5.1 GhExecutor (P0 — most steps depend on this)

Wraps the `gh` CLI with structured JSON output parsing and context variable extraction.

Why not just use ShellExecutor? Because:
- `gh` commands return JSON (`--json` flag) that needs to be parsed into context variables
- Issue selection needs filtering/sorting logic beyond "run command, check exit code"
- Review comment parsing needs structured extraction (author, body, resolved status)
- PR creation needs to capture the PR number from output

```toml
[[steps]]
step_id = "fetch_issue"
step_type = "gh"

[steps.parameters]
command = "issue_view"
repo = "{target_repo}"
issue_number = "{issue_number}"
json_fields = ["title", "body", "labels", "assignees", "comments"]
# Executor parses JSON output and sets context variables:
#   {issue_title}, {issue_body}, {issue_labels}, etc.
```

```toml
[[steps]]
step_id = "select_issue"
step_type = "gh"

[steps.parameters]
command = "issue_list"
repo = "{target_repo}"
labels = ["luther"]
state = "open"
sort = "created"
limit = 10
# Executor picks first unassigned issue, sets {issue_number}
```

```toml
[[steps]]
step_id = "check_review"
step_type = "gh"

[steps.parameters]
command = "pr_review_status"
repo = "{target_repo}"
pr_number = "{pr_number}"
# Executor fetches reviews + comments, determines:
#   - approval status (approved / changes_requested / pending)
#   - unresolved CodeRabbit comments
#   - sets {review_status}, {review_comments}
#   - returns success if approved, fixable if not
```

**Supported `gh` subcommands for MVP**:
- `issue_list` — list and filter issues
- `issue_view` — fetch issue details
- `issue_comment` — post a comment
- `pr_create` — create a PR
- `pr_checks` — watch CI status
- `pr_review_status` — fetch reviews and determine approval

### 5.2 GitExecutor (P0 — workspace setup and commit/push)

Wraps git operations with proper error handling and context extraction.

```toml
[[steps]]
step_id = "setup_branch"
step_type = "git"

[steps.parameters]
command = "setup_branch"
base_branch = "{base_branch}"
branch_name = "luther/fix-{issue_number}"
# Executor: fetch, create branch, validate clean state
# Sets {branch_name}
```

```toml
[[steps]]
step_id = "commit_push"
step_type = "git"

[steps.parameters]
command = "commit_push"
message = "{commit_message}"
branch = "{branch_name}"
# Executor: git add -A, commit, push
# Sets {commit_sha}
```

**Supported git subcommands for MVP**:
- `setup_branch` — fetch + create branch from base
- `commit_push` — add + commit + push
- `cleanup_branch` — delete local + remote branch

### 5.3 VerifyExecutor (P0 — the deterministic quality gate)

Runs a sequence of verification commands and parses their output into structured failure reports. This is the step that replaces "LLM, did the tests pass?" with hard data.

The VerifyExecutor is **target-project-aware** — the checks it runs depend on the project being worked on. The workflow config declares which check suite to use, and the executor knows how to run and parse each one.

For the MVP target (llxprt-code, a Node/TypeScript project):

```toml
[[steps]]
step_id = "verify_local"
step_type = "verify"

[steps.parameters]
checks = ["lint", "typecheck", "test", "format", "build", "smoke"]
# Each check has built-in knowledge of how to run and parse:
#   lint:       eslint (or project lint script) → parse lint errors
#   typecheck:  tsc --noEmit → parse type errors (file, line, code, message)
#   test:       vitest/jest --reporter=json → parse test results
#   format:     prettier --check → parse unformatted files
#   build:      npm run build / tsc → parse build errors
#   smoke:      run llxprt in non-interactive mode → verify it starts and exits cleanly
```

**Output parsing — what the LLM receives vs today:**

Today (raw stderr dumped to LLM):
```
src/tools/read-file.ts(42,5): error TS2322: Type 'string' is not
assignable to type 'number'.
...200 more lines of tsc output...
```

With VerifyExecutor (structured JSON in context):
```json
{
  "check": "typecheck",
  "passed": false,
  "errors": [
    {
      "file": "src/tools/read-file.ts",
      "line": 42,
      "column": 5,
      "code": "TS2322",
      "message": "Type 'string' is not assignable to type 'number'"
    }
  ]
}
```

For test failures, most Node test runners support JSON output (`vitest --reporter=json`, `jest --json`):
```json
{
  "check": "test",
  "passed": false,
  "failures": [
    {
      "test_name": "read-file tool > reads file content",
      "file": "src/tools/__tests__/read-file.test.ts",
      "line": 23,
      "kind": "assertion_failed",
      "expected": "Hello, World!",
      "actual": "Hello, World",
      "message": "expected 'Hello, World' to equal 'Hello, World!'"
    }
  ],
  "summary": { "passed": 142, "failed": 2, "skipped": 0 }
}
```

**Why this matters**: The remediate step doesn't waste LLM tokens figuring out *what failed*. It gets a precise, pre-parsed report and can focus entirely on *how to fix it*. It also means the workflow can make smarter routing decisions — type errors might be handled differently from test failures from lint warnings.

**Configurable checks**: The workflow config controls which checks run. The executor just needs parsers for each check type. This also means Luther can work on *different* projects — a Rust project would use `cargo build/test/clippy`, a Python project would use `pytest/mypy/ruff`. The workflow TOML stays the same; the check suite is a config parameter.

### 5.4 LlxprtExecutor (P0 — the creative steps)

Invokes llxprt-code as a subprocess with a structured prompt and captures its output.

```toml
[[steps]]
step_id = "analyze"
step_type = "llxprt"

[steps.parameters]
goal = "Analyze issue #{issue_number}: {issue_title}\n\n{issue_body}"
context_files = ["src/**/*.rs", "tests/**/*.rs"]
output_format = "markdown"
max_tokens = 50000
```

**Key design questions**:
- How does llxprt-code get invoked? CLI subprocess (`llxprt --goal "..."`)? Or as a library?
- How do we pass it the codebase context? Working directory? Explicit file list?
- How do we capture structured output vs free-form text?
- How do we map llxprt's exit/output to StepOutcome?

### 5.5 EvalExecutor (P2 — horizon 2, not MVP-critical)

Records performance metrics to a local store. Could be deferred — a shell step writing to a JSON file works for MVP.

## 6. Context Passing Between Steps

Steps need to pass data forward. The current `StepContext` supports this via `set(key, value)` / `get(key)` with `{key}` interpolation in parameters.

For the MVP workflow, the critical context chain is:

```
fetch_issue  →  {issue_number}, {issue_title}, {issue_body}, {issue_labels}
analyze      →  {analysis}
plan         →  {plan}
implement    →  (modifies files in work_dir, no explicit context var needed)
verify_local →  {verify_stdout}, {verify_stderr} (failure details for remediate)
commit_push  →  {commit_sha}
submit_pr    →  {pr_number}, {pr_url}
watch_ci     →  {ci_status}, {ci_failures}
check_review →  {review_comments}
record_result→  (terminal, writes metrics)
```

### Current limitation

Today, `{stdout}` and `{stderr}` are overwritten by every shell step. For the MVP workflow, we need either:
- **Namespaced outputs**: `{step_id.stdout}` (e.g., `{fetch_issue.stdout}`)
- **Explicit context setting**: the executor parses output and sets named variables
- **Both**: executor sets named vars from structured output, raw stdout available as `{step_id.stdout}`

The `LlxprtExecutor` especially needs structured output capture — we can't just dump the entire LLM response into `{stdout}` and hope interpolation works.

## 7. What Exists Today vs What Needs Building

### Exists (engine layer — generic, workflow-agnostic)
| Component | Status | Notes |
|---|---|---|
| WorkflowType schema + TOML/JSON loading | [OK] Done | |
| WorkflowConfig schema + loading | [OK] Done | |
| EngineRunner (step loop, transition resolution) | [OK] Done | |
| ExecutorRegistry (dispatch) | [OK] Done | |
| StepContext + interpolation | [OK] Done | Needs namespaced outputs |
| ShellExecutor | [OK] Done | |
| WriteFileExecutor | [OK] Done | |
| Checkpoint/resume persistence | [OK] Done | |
| Event logging | [OK] Done | |
| Monitor + heartbeat + IPC | [OK] Done | |
| CLI (run/status/service) | [OK] Done | Needs config path flexibility |

### Needs building (executor + workflow layer)
| Component | Priority | Scope |
|---|---|---|
| GhExecutor | P0 — MVP blocker | issue list/view/comment, pr create/checks/reviews, JSON parsing, context extraction |
| GitExecutor | P0 — MVP blocker | fetch, branch create, commit, push, cleanup |
| VerifyExecutor | P0 — MVP blocker | Run build/test/clippy/grep, parse output into structured failure reports |
| LlxprtExecutor | P0 — MVP blocker | Invoke llxprt-code, capture structured output |
| Namespaced context outputs | P0 — MVP blocker | `{step_id.stdout}`, `{step_id.variable}` |
| issue-fix-v1.toml (real version) | P0 — MVP blocker | Replace placeholder step_types with real ones |
| profile-0.toml (real version) | P0 — MVP blocker | Real config with target repo, labels, base branch |
| Config path resolution | P1 — needed for real runs | CLI resolves from config dir, not just fixtures |
| Eval metrics storage | P2 — horizon 2 | SQLite table for per-run performance data |
| Self-improvement workflow | P3 — horizon 3 | Separate workflow type that reads eval data |

## 8. Eval Metrics (Horizon 2)

Every completed run records:

| Metric | Source | Why It Matters |
|---|---|---|
| `remediation_loops` | engine loop counter | Fewer = better code generation |
| `ci_failures` | watch_ci step count | Fewer = better verification |
| `coderabbit_comments` | check_review step | Fewer = cleaner code |
| `review_rounds` | check_review loop count | Fewer = better first attempt |
| `pr_accepted` | terminal outcome | Binary success metric |
| `pr_rejected` | terminal outcome | Binary failure metric |
| `total_duration_secs` | run timestamps | Efficiency metric |
| `llxprt_invocations` | LlxprtExecutor count | Cost/efficiency metric |
| `lines_changed` | git diff stat | Scope metric |

These feed into horizon 3: Luther reads its own eval data and proposes workflow/prompt changes.

## 9. Separation of Concerns Summary

| Layer | What It Knows | What It Doesn't Know |
|---|---|---|
| **Workflow definition** (TOML) | Step names, transitions, guard limits | How steps execute, what llxprt is |
| **Engine** (Rust) | How to run steps, route on outcomes, persist state | GitHub, issues, PRs, CodeRabbit |
| **Executors** (Rust) | How to run one step type (shell, llxprt, etc.) | The workflow graph, what comes next |
| **llxprt-code** (external) | How to analyze/plan/implement code | The workflow it's embedded in |

No layer reaches into another. The workflow definition could be swapped (issue-fix-v2, docs-update-v1) without changing the engine. The engine could swap executors (different LLM, different CI system) without changing the workflow. llxprt-code doesn't know it's inside a workflow — it just gets a goal and a codebase.
