# CodeRabbit PR Follow-Through Engine Updates — Technical and Functional Specification

## Purpose

Luther currently relies on a human coordinator to run `gh pr checks NUM --watch --interval 300`, repeat that watch loop, inspect failures, inspect CodeRabbit comments, and decide what to do next. That human-operated loop is error-prone: the coordinator can stop early, miss pending jobs, miss review comments, evaluate stale PR state, or make CI-state decisions from prose instead of authoritative machine data.

This engine update adds deterministic GitHub PR follow-through capabilities so workflows can supervise a generated pull request until it is either green and review feedback has been handled, or the workflow reaches a clear bounded failure state. The LLM remains responsible for judgment and code changes; the engine is responsible for collecting facts, binding those facts to the current PR head, classifying terminal state, enforcing poll limits, deduplicating feedback, and producing structured inputs for the LLM.

## Scope

This specification covers reusable engine functionality that supports PR follow-through workflows. It is not itself the target workflow. The workflow specification in `project-plans/coderabbit/workflowupdates/overview.md` describes how these capabilities are composed for `llxprt-issue-fix-v1`.

In scope:

- Deterministic PR identity capture after PR creation.
- Deterministic PR check polling for up to a configured duration using fixed increments, defaulting to 60 minutes total and 5-minute increments.
- Non-fail-fast check watching by default: continue polling until all current-head checks are terminal or the watch budget is exhausted, then classify all failures together.
- Deterministic classification of PR checks into pending, passing, failing, cancelled, skipped, neutral, unknown, and stale buckets using GitHub CLI/API JSON, not human-readable watch tables.
- Deterministic collection of failed check metadata and bounded logs.
- Deterministic collection of CodeRabbit issue comments, reviews, review comments, and review threads, including readiness/stabilization state.
- Stable machine-readable artifacts for PR identity, PR checks, check failures, CodeRabbit feedback items, feedback state/deduplication, LLM feedback evaluations, remediation plans, remediation results, and marker actions.
- A deterministic aggregation step that separates accepted feedback, rejected feedback with reasons, needs-user-judgment items, current-head CI failures, and stale/incomplete data.
- Deterministic posting of comments/resolution summaries from previously produced structured decisions.
- Step outcomes that workflows can route without LLM interpretation and without relying on `StepOutcome::Abandon` as a routable transition unless an explicit engine behavior change is implemented and tested.

Out of scope:

- The LLM's actual code remediation.
- The LLM's judgment on whether a CodeRabbit item is valid or in scope.
- Replacing GitHub Actions or CodeRabbit.
- Pushing or merging PRs beyond existing `push_changes` behavior.
- Full dynamic DAG fan-out in the workflow engine. If per-item LLM evaluation cannot be represented as native DAG fan-out, the first implementation may use a deterministic executor that invokes the configured LLM command once per item and writes one accepted result per item.

## Functional Requirements

### PR identity capture

The engine must ensure `create_pr` produces a deterministic PR identity artifact before follow-through begins. The artifact must include at least:

- repository owner/name;
- PR number;
- PR URL;
- head ref name;
- head SHA;
- base ref name;
- creation or capture timestamp.

All downstream PR/check/feedback artifacts must repeat the PR number, head ref, head SHA, and base ref they were collected against. If the PR head changes between artifact capture and later use, stale artifacts must not be treated as current. The engine must either refresh current-head artifacts or produce a structured stale-state report that routes to a representable non-success outcome.

### PR check polling

The engine must provide a deterministic PR check watcher that:

- Reads PR identity from `pr.json` and verifies the current PR head ref/SHA before and after polling.
- Polls GitHub PR check state through structured JSON (`gh pr checks --json` or GitHub API), never by parsing table output.
- Supports configurable polling interval, with a default of 300 seconds.
- Supports configurable maximum watch duration or attempt count, with a default of 60 minutes / 12 attempts.
- Writes a structured artifact after every polling cycle.
- Is non-fail-fast by default: a failed check does not stop polling while other current-head checks remain pending and budget remains; after all checks are terminal or budget is exhausted, all current-head failures are classified together.
- Ignores stale check runs from older head SHAs for success/failure decisions and records them in a `stale` bucket for auditability.
- Returns a workflow outcome that distinguishes:
  - all current-head required/visible checks are terminal and successful;
  - one or more current-head terminal failed/cancelled/timed-out checks exist after all checks are terminal or the watch budget is exhausted;
  - one or more current-head checks remain pending after the allowed polling budget;
  - only stale or unbindable check data was available;
  - GitHub state could not be fetched due to authentication, network, or schema errors.

The watcher must make completion decisions from current check state only. It must not rely on elapsed-duration assumptions such as “this is probably stuck.”

### Check-state classification policy

The engine must define a fixture-tested classification table for GitHub states/conclusions. Required policy:

- Passing: `success`, plus `neutral` and `skipped` only when GitHub reports them as terminal conclusions for the current head. These must be recorded separately from true success.
- Failing: `failure`, `startup_failure`, `timed_out`, `action_required`, and equivalent terminal failure conclusions.
- Cancelled: `cancelled` is terminal and non-passing. It should be remediated or reported with other failures unless the workflow later proves it is stale.
- Pending: queued, requested, waiting, in_progress, pending, and no conclusion with a nonterminal status.
- Unknown: any unrecognized state/conclusion or missing status/conclusion pair. Unknown prevents false success. If unknown persists after the watch budget, route as a structured fatal or needs-human condition using an outcome representable by the current workflow schema.
- Stale: any check whose head SHA/ref does not match the current PR identity. Stale checks are ignored for success/failure classification, but must be reported.

Missing logs must not erase a failure. If logs cannot be fetched, `ci-failures.json` must include the failure metadata, `log_available: false`, and a deterministic `log_error` reason.

### Check failure collection

The engine must provide deterministic collection of CI failure details:

- Input: the structured check status artifact bound to the current PR head.
- For every current-head failed/cancelled/timed-out/action-required check, collect stable identifiers, name, state, conclusion, URL, run ID/job ID when available, and a bounded log excerpt.
- Store raw logs under the artifact directory when available.
- Store a machine-readable `ci-failures.json` file with enough detail for an LLM remediation step.
- Avoid treating pending jobs as failed unless the watch budget is exhausted; exhausted pending jobs become a timeout/needs-human artifact, not invented failures.
- Preserve stale/unknown check reports separately so they can be surfaced without being remediated as current failures.

### CodeRabbit feedback collection and readiness

The engine must collect CodeRabbit feedback deterministically:

- Query PR issue comments, PR reviews, PR review comments, and review threads through GitHub REST and GraphQL APIs.
- Include only comments authored by CodeRabbit identities configured by workflow, defaulting to `coderabbitai` and the bot account variants observed in fixtures.
- Preserve stable IDs, URLs, paths, line numbers, thread IDs, resolution state when available, created/updated timestamps, raw bodies, and the PR head ref/SHA observed at collection time.
- Normalize comments into feedback items where possible:
  - review thread = one item;
  - inline PR review comment without thread = one item;
  - issue comment review summary may be represented as one or more items if deterministic parsing can split it safely, otherwise as one summary item.
- Exclude already resolved threads unless workflow explicitly requests all feedback.
- Write `coderabbit-feedback.json` and `coderabbit-feedback-state.json`.

Empty feedback must not mean clean until CodeRabbit has reached a ready/stable state. The collector must record readiness using deterministic signals, for example:

- CodeRabbit has posted a completed review/status marker for the current head; or
- the configured CodeRabbit check/run for the current head is terminal; and
- feedback has stabilized across the configured number of consecutive polls or collection passes.

If CodeRabbit has not completed for the current head, or if feedback changes during stabilization, the collector must route to a representable non-clean outcome and record `ready: false` or `stable: false`. The workflow may then continue watching/collecting within its loop budget.

### GitHub API expectations and fallback behavior

The implementation must prefer structured GitHub APIs:

- REST or GraphQL for PR identity and checks when `gh pr checks --json` does not expose all needed fields.
- GraphQL review threads for thread IDs and resolution state.
- GraphQL mutations for resolving review threads where available.
- REST review comments/issues APIs as fallback for comments that are not available through GraphQL.

If review-thread resolution state cannot be fetched, the item must still be collected with `resolution_state_available: false`; marker behavior must not claim resolution unless a resolution mutation succeeds. If a thread cannot be resolved through API permissions or availability, the marker must record a nonfatal per-item action result and leave the thread unresolved unless workflow policy treats that as fatal.

### Feedback state, deduplication, and idempotency

The engine must maintain deterministic feedback state artifacts to support loops and resumes:

- A stable feedback item key derived from immutable GitHub identifiers when available (`thread_id`, `comment_id`, `review_id`) and a deterministic content hash fallback.
- `coderabbit-feedback-state.json` containing discovered item keys, first/last seen timestamps, head SHA, body hash, resolution state, prior evaluation status, prior marker status, and superseded/stale flags.
- Deduplication so the same CodeRabbit item is evaluated at most once per unchanged body/head combination.
- Idempotent marker behavior so duplicate comments are not posted across loop iterations or workflow resumes. Marker comments should include or be associated with a stable marker key, and the marker must inspect prior marker actions before posting.
- Re-evaluation when an item body changes, when a previously resolved item reopens, or when the PR head changes and the item is still current.

### Per-item feedback evaluation support

The engine should support one LLM evaluation per collected feedback item. Because current workflow topology is static, the implementation may choose one of these approaches:

1. Add a reusable deterministic executor that loops over feedback items and invokes the configured LLM command once per unevaluated item, requiring strict JSON output for each evaluation; or
2. Add minimal workflow-engine support for iterating a step over a JSON array.

Either approach must produce the same external contract: `feedback-evaluations.json`, containing exactly one accepted validated evaluation result per current input item/body/head combination. Previously accepted unchanged evaluations may be reused from state; they still must appear once in the current aggregate artifact.

Allowed decisions:

- `valid` — should be fixed in this PR.
- `invalid` — factually wrong or already satisfied.
- `out_of_scope` — not appropriate for this PR.
- `needs_user_judgment` — cannot be safely decided automatically.

The per-item LLM contract is:

- Invoke the LLM independently for each item that lacks a reusable accepted evaluation.
- Accept only strict JSON matching the schema for that one item.
- Validate that the response item ID/key, body hash, head SHA, decision enum, reason, and recommended action match the requested item contract.
- Retry malformed output per item up to the configured retry limit.
- Do not ask the LLM to evaluate multiple items in one accepted response.
- Do not create a second accepted result for an item once a valid result has been recorded for the same item/body/head.

Malformed output is `Fixable` while per-item retry budget remains and `Fatal` after retry exhaustion unless a planned engine change introduces a more specific representable outcome.

### Remediation-plan aggregation

The engine must provide deterministic aggregation from:

- `pr.json`;
- `pr-check-status.json`;
- `ci-failures.json`;
- `coderabbit-feedback.json`;
- `coderabbit-feedback-state.json`;
- `feedback-evaluations.json`.

The output `pr-remediation-plan.json` must contain:

- `must_fix`: current-head CI failures plus feedback items evaluated as valid.
- `mark_invalid`: feedback items evaluated invalid or out of scope, with reasons.
- `needs_user_judgment`: feedback items requiring human judgment, watch timeouts, unstable CodeRabbit state after budget, stale-only check state, or persistent unknown check state.
- `clean`: true only when all current-head checks are passing/acceptable terminal states, CodeRabbit is ready and stable, there are no CI failures, no valid feedback items, no unmarked invalid/out-of-scope items, and no needs-user-judgment items.

The aggregator must not produce ambiguous workflow outcomes. If current engine transitions cannot route `Abandon`, needs-human/watch-timeout conditions must map to `Fatal` or `Fixable` with explicit artifact fields, or the implementation plan must include an engine behavior change that makes `StepOutcome::Abandon` routable with tests.

### Structured remediation result

The remediation step must produce a structured result artifact consumed by the marker and aggregator, for example `pr-remediation-result.json`. The engine must validate that the artifact covers every `must_fix` entry:

- Per CI failure: identifier, attempted action, files changed or reason no change was needed, verification command/result when available, and status (`fixed`, `not_reproduced`, `not_fixed`, `needs_user_judgment`).
- Per valid CodeRabbit item: item key, attempted action, files changed or reason no change was needed, verification command/result when available, and status.
- Overall remediation status and current head SHA after changes.

The marker must not invent “action taken” text from free-form logs. It must comment from this structured artifact plus the evaluation reasons.

### Deterministic commenting and resolution

The engine must support deterministic comment/resolution actions from structured artifacts:

- For valid fixed items: comment with action taken from `pr-remediation-result.json` and resolve thread when possible.
- For invalid/out-of-scope items: comment with the recorded evaluation reason and resolve thread when policy allows.
- For needs-user-judgment items: do not resolve automatically; write a clear artifact and route using `Fixable` or `Fatal` unless routable `Abandon` is explicitly implemented.
- Record one marker result per item in `pr-feedback-marker-report.json`, including action attempted, comment URL/ID, resolution attempted, resolution result, skipped reason, and idempotency key.
- Comment body generation must be template-driven and file-based to avoid shell-escaping problems.
- Backticks must not be placed unescaped into shell-quoted `gh` invocations.

## Technical Design

### New or updated modules

Recommended module layout:

```text
src/engine/executors/github_pr.rs
src/engine/executors/github_feedback.rs
src/engine/executors/feedback_eval.rs
src/engine/executors/pr_remediation.rs
```

Alternative: one `github_pr` executor with multiple modes. Prefer separate executors if they remain cohesive and testable.

Expected executor registrations in `ExecutorRegistry::with_defaults()`:

- `github_pr_identity` or an updated `create_pr` that writes `pr.json`
- `github_pr_checks`
- `github_check_failures`
- `github_coderabbit_feedback`
- `feedback_evaluator`
- `pr_remediation_plan`
- `github_feedback_marker`

### Existing integration points

- `src/engine/executor.rs`
  - `StepExecutor` remains the execution contract.
  - `StepContext` provides interpolation values and stores output paths/IDs for later steps.
  - `ExecutorRegistry::with_defaults()` must register new executors so workflow TOML can use them.

- `src/engine/transition.rs`
  - Current engine behavior treats `StepOutcome::Abandon` as terminal rather than a normal routable transition. These specs must not require transitions from `abandon` unless the engine implementation plan explicitly changes that behavior and adds tests.
  - Existing outcomes are enough if outcomes are mapped as:
    - `Success`: clean/current state, or deterministic artifact generation completed when the next step can inspect contents.
    - `Fixable`: actionable CI failures, valid feedback, unstable CodeRabbit state that should loop within budget, malformed per-item evaluation while retry budget remains, or bounded remediation needed.
    - `Retryable`: checks or feedback readiness should continue polling, if the engine elects to expose retryable directly and the workflow schema can route it.
    - `Fatal`: GitHub/API/auth/schema errors, exhausted watch/feedback budget, needs-user-judgment, persistent unknown state, stale-only state, or malformed evaluation after retry exhaustion when no routable abandon support exists.
  - If a more expressive outcome or routable `Abandon` is needed, the implementation plan must include engine changes, transition tests, and workflow graph tests.

- `src/engine/executors/shell.rs` and `src/engine/executors/verify.rs`
  - These can be used for early prototypes, but long-term behavior should not depend on fragile shell parsing.

- `config/workflows/llxprt-issue-fix-v1.toml`
  - Consumes these executors in post-PR follow-through.

### Artifact contracts

All engine-generated artifacts must be valid JSON unless explicitly a raw log. JSON files must include a `schema_version` field and must bind to `repo`, `pr_number`, `head_ref`, `head_sha`, and `base_ref` unless the artifact is explicitly repository-global.

#### `pr.json`

```json
{
  "schema_version": 1,
  "repo": "owner/name",
  "pr_number": 1910,
  "url": "https://github.com/owner/name/pull/1910",
  "head_ref": "luther/issue-1234",
  "head_sha": "abc123",
  "base_ref": "main",
  "captured_at": "2026-04-29T17:00:00Z"
}
```

#### `pr-check-status.json`

```json
{
  "schema_version": 1,
  "repo": "owner/name",
  "pr_number": 1910,
  "head_ref": "luther/issue-1234",
  "head_sha": "abc123",
  "base_ref": "main",
  "poll_started_at": "2026-04-29T17:00:00Z",
  "poll_finished_at": "2026-04-29T18:00:00Z",
  "attempts": 12,
  "interval_seconds": 300,
  "complete": true,
  "passed": false,
  "budget_exhausted": false,
  "pending": [],
  "failed": [
    {
      "id": "check-run-123",
      "name": "Lint (Javascript)",
      "state": "completed",
      "conclusion": "failure",
      "bucket": "fail",
      "head_sha": "abc123",
      "url": "https://github.com/...",
      "description": "..."
    }
  ],
  "cancelled": [],
  "skipped": [],
  "neutral": [],
  "passed_checks": [],
  "unknown": [],
  "stale": []
}
```

#### `ci-failures.json`

```json
{
  "schema_version": 1,
  "repo": "owner/name",
  "pr_number": 1910,
  "head_ref": "luther/issue-1234",
  "head_sha": "abc123",
  "base_ref": "main",
  "failures": [
    {
      "id": "ci-1",
      "check_id": "check-run-123",
      "check_name": "Lint (Javascript)",
      "state": "completed",
      "conclusion": "failure",
      "url": "https://github.com/...",
      "log_available": true,
      "log_error": null,
      "log_excerpt": "...",
      "raw_log_path": "ci/Lint-Javascript.log"
    }
  ],
  "stale": [],
  "unknown": []
}
```

#### `coderabbit-feedback.json`

```json
{
  "schema_version": 1,
  "repo": "owner/name",
  "pr_number": 1910,
  "head_ref": "luther/issue-1234",
  "head_sha": "abc123",
  "base_ref": "main",
  "ready": true,
  "stable": true,
  "items": [
    {
      "id": "cr-thread-1",
      "item_key": "thread:PRRT_...",
      "body_hash": "sha256:...",
      "source": "review_thread",
      "thread_id": "PRRT_...",
      "comment_id": "PRRC_...",
      "author": "coderabbitai",
      "path": "packages/core/src/core/file.ts",
      "line": 122,
      "url": "https://github.com/...",
      "is_resolved": false,
      "resolution_state_available": true,
      "body": "..."
    }
  ]
}
```

#### `coderabbit-feedback-state.json`

```json
{
  "schema_version": 1,
  "repo": "owner/name",
  "pr_number": 1910,
  "head_ref": "luther/issue-1234",
  "head_sha": "abc123",
  "base_ref": "main",
  "ready": true,
  "stable": true,
  "items": [
    {
      "item_key": "thread:PRRT_...",
      "body_hash": "sha256:...",
      "first_seen_at": "2026-04-29T17:10:00Z",
      "last_seen_at": "2026-04-29T17:20:00Z",
      "evaluation_status": "accepted",
      "marker_status": "not_marked",
      "superseded": false,
      "stale": false
    }
  ]
}
```

#### `feedback-evaluations.json`

```json
{
  "schema_version": 1,
  "repo": "owner/name",
  "pr_number": 1910,
  "head_ref": "luther/issue-1234",
  "head_sha": "abc123",
  "base_ref": "main",
  "items": [
    {
      "item_key": "thread:PRRT_...",
      "body_hash": "sha256:...",
      "decision": "valid",
      "reason": "The requested regression is in scope because it covers the same allowedFunctionNames semantics changed by this PR.",
      "recommended_action": "Add a malformed non-array allowedFunctionNames regression test.",
      "accepted_at": "2026-04-29T17:30:00Z",
      "attempts": 1
    }
  ]
}
```

#### `pr-remediation-plan.json`

```json
{
  "schema_version": 1,
  "repo": "owner/name",
  "pr_number": 1910,
  "head_ref": "luther/issue-1234",
  "head_sha": "abc123",
  "base_ref": "main",
  "clean": false,
  "must_fix": [],
  "mark_invalid": [],
  "needs_user_judgment": []
}
```

#### `pr-remediation-result.json`

```json
{
  "schema_version": 1,
  "repo": "owner/name",
  "pr_number": 1910,
  "input_head_sha": "abc123",
  "output_head_sha": "def456",
  "overall_status": "fixed",
  "ci_results": [],
  "feedback_results": [
    {
      "item_key": "thread:PRRT_...",
      "status": "fixed",
      "action_taken": "Added the requested regression coverage.",
      "files_changed": ["packages/core/src/core/file.test.ts"],
      "verification": [
        {
          "command": "npm test -- packages/core/src/core/file.test.ts",
          "status": "passed"
        }
      ]
    }
  ]
}
```

#### `pr-feedback-marker-report.json`

```json
{
  "schema_version": 1,
  "repo": "owner/name",
  "pr_number": 1910,
  "head_ref": "luther/issue-1234",
  "head_sha": "def456",
  "actions": [
    {
      "item_key": "thread:PRRT_...",
      "idempotency_key": "marker:thread:PRRT_...:sha256:...",
      "comment_posted": true,
      "comment_url": "https://github.com/...",
      "resolution_attempted": true,
      "resolved": true,
      "skipped_reason": null
    }
  ]
}
```

### Error handling

- Authentication or missing `gh`: fatal.
- GitHub API rate limit: retryable if within polling budget, fatal after budget exhausted unless a routable timeout outcome is explicitly implemented.
- Malformed JSON from `gh`: fatal.
- Unknown check states: represented in `unknown`; unknown terminal/nonterminal classification must be conservative and prevent false success.
- Stale check data: represented in `stale`; stale checks are ignored for current-head success/failure, but stale-only state prevents success.
- CodeRabbit unavailable or no comments: success with empty feedback only when readiness and stabilization criteria are satisfied for the current head.
- CodeRabbit not ready or unstable: non-clean structured outcome; loop if budget remains, otherwise route as fatal/needs-human unless routable abandon support is implemented.
- LLM evaluation malformed: fixable if per-item retry budget remains, fatal otherwise.
- Missing logs for failed checks: not fatal by itself; record `log_available: false` and continue.

### Testing strategy

Tests must be deterministic and not require live GitHub network access by default.

Recommended tests:

- Unit tests for PR identity artifact capture and downstream head SHA/ref binding.
- Unit tests for check-state classification from fixture JSON, including skipped, neutral, cancelled, unknown, missing conclusion, stale, and missing logs.
- Unit tests for non-fail-fast watch behavior: continue polling while any current-head check is pending, then classify all failures after terminal state or budget exhaustion.
- Unit tests for watch-budget calculation: 60 minutes / 300 seconds = 12 attempts.
- Unit tests for pending/failing/passing/unknown/stale transitions.
- Unit tests for CodeRabbit readiness and stabilization, including empty-but-not-ready feedback.
- Unit tests for CodeRabbit normalization from fixture GraphQL/REST payloads and GraphQL fallback behavior.
- Unit tests for feedback deduplication/idempotency across loops and resumes.
- Unit tests for per-item feedback-evaluation JSON validation and retry behavior.
- Unit tests validating remediation-result coverage for every `must_fix` item.
- Integration tests with fake `gh` script in PATH producing fixture JSON and fake logs.
- Workflow graph tests proving post-PR steps are reachable, representable by current transition schema, and loop caps exist.
- If routable `StepOutcome::Abandon` is implemented, transition tests proving it is no longer terminal for configured edges.

### Security and shell safety

- Prefer `std::process::Command` arguments over shell strings in executors.
- If comments are posted through `gh`, write body to a temporary file and use `--body-file` where available.
- Never interpolate untrusted review text into shell commands.
- Bound log excerpt sizes to avoid context blowups.

## Plan compliance gaps to cover in future detailed plans

This overview intentionally describes contracts and behavior, not the full implementation plan. Future plan artifacts must spell out:

- The exact transition/outcome mapping chosen for needs-human/watch-timeout conditions, including whether `StepOutcome::Abandon` behavior changes.
- The exact GitHub REST/GraphQL queries and fixture payloads.
- The exact CodeRabbit readiness/stabilization signals used by the first implementation.
- The exact idempotency key format and marker-comment discovery policy.
- The exact remediation-result schema validation rules and prompt templates.
- Migration of `create_pr` to a deterministic `pr.json` artifact without changing unrelated workflow behavior.

## Acceptance Criteria

- A workflow can deterministically watch current-head PR checks for up to one hour in 5-minute increments without human intervention.
- Pending, unknown, stale-only, or CodeRabbit-not-ready state cannot be mistaken for success.
- Failed/cancelled/timed-out current-head checks produce structured artifacts for remediation, with missing logs represented explicitly.
- CodeRabbit feedback is collected deterministically, readiness/stabilization is recorded, and empty feedback is clean only after readiness/stabilization criteria are satisfied.
- Each current CodeRabbit item/body/head combination receives exactly one accepted validated LLM evaluation result in the evaluation artifact.
- Valid feedback and current-head CI failures are passed to remediation; invalid/out-of-scope feedback is carried with reasons for deterministic commenting.
- Remediation produces a structured per-item/per-failure result artifact.
- Commenting/resolution behavior is driven by artifacts, uses idempotency state, and records action outcomes.
- The feature is reachable from `llxprt-issue-fix-v1` after `create_pr` using transitions representable by the current workflow engine, or includes an explicit tested engine change for new routing behavior.
