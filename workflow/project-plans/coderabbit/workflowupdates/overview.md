# CodeRabbit PR Follow-Through Workflow Updates — Technical and Functional Specification

## Purpose

The `llxprt-issue-fix-v1` workflow currently ends after creating a pull request. Human coordination is then required to watch GitHub Actions, inspect failures, evaluate CodeRabbit feedback, fix accepted issues, push changes, and repeat until the PR is green. This specification updates the workflow so that post-PR follow-through is part of the workflow itself, using deterministic engine steps for facts and bounded LLM steps for judgment/remediation.

This workflow update depends on the engine capabilities specified in `project-plans/coderabbit/engineupdates/overview.md`. The engine support exists to make this workflow reliable; the workflow itself is the user-visible behavior.

## User-facing behavior

After Luther creates a PR, the workflow must:

1. Capture deterministic PR identity: number, URL, head ref, head SHA, and base ref.
2. Watch GitHub PR checks for the captured current head deterministically for up to one hour, polling every five minutes by default.
3. Use non-fail-fast watching by default: if some checks fail while others are still pending, keep polling until all current-head checks are terminal or the watch budget is exhausted, then classify all failures together.
4. Ignore stale checks from older head SHAs for pass/fail decisions while reporting them in artifacts.
5. If current-head checks fail, collect the failed check metadata/logs deterministically and route them to remediation.
6. Collect CodeRabbit feedback deterministically from PR comments, reviews, review comments, and review threads only after CodeRabbit is ready/stable for the current head, or keep looping/report a bounded failure if readiness/stability is not reached.
7. Evaluate each current CodeRabbit feedback item with exactly one accepted bounded LLM judgment result, retrying only malformed output for that item.
8. Pass valid CodeRabbit items and current-head CI failures to the remediation step.
9. Require remediation to write a structured result per accepted feedback item and per CI failure.
10. Deterministically comment on CodeRabbit items with the action taken or the reason an item is invalid/out of scope, and resolve threads when API support and policy allow.
11. Push remediation changes and repeat the watch/feedback loop until all checks pass and all actionable CodeRabbit items are handled, or until configured loop caps are reached.
12. Complete only when the PR is green for the current head, CodeRabbit feedback is ready/stable and clean, marker actions are complete or intentionally skipped with recorded reasons, and no needs-human conditions remain.

## Scope

In scope:

- Updating `llxprt-issue-fix-v1.toml` so PR follow-through happens after `create_pr` and before `log_completion`.
- Adding workflow variables for PR polling interval, total polling budget, CodeRabbit author identities, CodeRabbit readiness/stabilization policy, and maximum PR remediation iterations.
- Ensuring `create_pr` or an immediate deterministic follow-up step writes a PR identity artifact with number, URL, head ref, head SHA, and base ref.
- Adding deterministic steps for watch, failure collection, feedback collection/state, remediation-plan aggregation, feedback marking, and stale/current-head validation.
- Adding bounded LLM steps for per-item feedback evaluation and remediation.
- Adding remediation-result validation so marker comments can truthfully describe action taken.
- Adding feedback deduplication/idempotency state so loops/resumes do not duplicate comments or repeated evaluations.
- Updating fixture TOML/JSON and workflow graph tests.

Out of scope:

- Merging PRs.
- Manual intervention while the workflow is running, except for explicit needs-human outcomes represented by current engine outcomes or by a planned tested engine change.
- Letting the LLM decide whether checks are complete or failed.
- Letting the LLM scrape GitHub comments itself.
- Making this feature specific to issue #1748. The #1748 smoke exposed the gap, but this workflow behavior must be reusable for future Luther PRs.
- Implementing all low-level GitHub API query details in this overview; those belong in the detailed engine/workflow implementation plan.

## Existing workflow integration

Current tail:

```text
push_changes
→ generate_pr_description
→ create_pr
→ log_completion
```

Required semantic tail:

```text
push_changes
→ generate_pr_description
→ create_pr
→ capture_pr_identity
→ watch_pr_checks
→ collect_check_failures_if_needed
→ collect_coderabbit_feedback
→ evaluate_coderabbit_feedback
→ build_pr_remediation_plan
→ remediate_pr_feedback
→ validate_pr_remediation_result
→ run_tests
→ push_changes
→ capture_pr_identity
→ watch_pr_checks
→ ...
→ mark_pr_feedback
→ log_completion
```

The exact step names may differ, but the semantics must preserve these boundaries:

- `capture_pr_identity` is deterministic and writes `pr.json` with number, URL, head ref, head SHA, and base ref.
- `watch_pr_checks` is deterministic and binds check state to the current PR head SHA/ref.
- `collect_check_failures` is deterministic and keeps missing logs explicit rather than dropping failures.
- `collect_coderabbit_feedback` is deterministic and records readiness, stabilization, deduplication, and resolution state.
- `evaluate_coderabbit_feedback` performs one LLM judgment per current unevaluated feedback item and produces strict JSON; previously accepted unchanged evaluations may be reused from state but appear exactly once in the aggregate artifact.
- `build_pr_remediation_plan` is deterministic and emits no ambiguous routing state.
- `remediate_pr_feedback` is an LLM implementation step that consumes only accepted feedback and CI failures.
- `validate_pr_remediation_result` is deterministic and requires per-item/per-failure remediation status.
- `mark_pr_feedback` is deterministic and idempotent.

## Proposed workflow steps

### `capture_pr_identity`

Type: updated `create_pr`, `github_pr_identity`, or equivalent deterministic executor.

Inputs:

- `{target_repo}`
- PR URL/number from `create_pr` output, if already known
- output path `{artifact_dir}/pr.json`

Outputs:

- `pr.json` containing `number`, `url`, `headRef`, `headSha`, and `baseRef`.

Outcomes:

- `success`: PR identity artifact written and head SHA/ref captured.
- `fatal`: PR identity could not be determined from structured GitHub data.

### `watch_pr_checks`

Type: `github_pr_checks` or equivalent deterministic executor.

Inputs:

- `{artifact_dir}/pr.json`
- `{target_repo}`
- `interval_seconds = 300`
- `max_duration_seconds = 3600`
- output path `{artifact_dir}/pr-check-status.json`

Outcomes:

- `success`: all current-head checks are terminal and passing or acceptable terminal states under the classification policy.
- `fixable`: one or more current-head checks are terminal failed/cancelled/timed-out/action-required after all checks are terminal or the watch budget is exhausted.
- `fatal`: GitHub/API/auth/schema failure, watch budget exhausted with checks still pending, stale-only state, or persistent unknown state, unless the implementation plan adds tested routable `abandon`/timeout semantics.

Important: `fixable` means “remediation needed,” not “the watch command failed.” The watcher must not fail fast on the first failed check while other current-head checks are still pending.

### `collect_check_failures`

Type: `github_check_failures` or equivalent deterministic executor.

Inputs:

- `{artifact_dir}/pr.json`
- `{artifact_dir}/pr-check-status.json`
- `{target_repo}`
- output path `{artifact_dir}/ci-failures.json`
- raw log directory `{artifact_dir}/ci-logs/`
- log excerpt byte limit

Outcomes:

- `success`: failure artifact written; may contain zero failures only when there are no current-head failed checks.
- `fatal`: failure details could not be collected due to unrecoverable error.

Missing logs are recorded per failure with `log_available: false` and `log_error`; they do not remove the failure from remediation.

### `collect_coderabbit_feedback`

Type: `github_coderabbit_feedback` or equivalent deterministic executor.

Inputs:

- `{artifact_dir}/pr.json`
- `{target_repo}`
- author identities, default `coderabbitai` plus configured bot-account variants
- include unresolved threads only by default
- readiness/stabilization settings
- output path `{artifact_dir}/coderabbit-feedback.json`
- state path `{artifact_dir}/coderabbit-feedback-state.json`

Behavior:

- Query issue comments, PR reviews, PR review comments, and review threads through GitHub REST/GraphQL APIs.
- Prefer GraphQL review threads for thread IDs/resolution state and use REST fallbacks when GraphQL does not expose a comment.
- Bind all feedback to the PR head ref/SHA observed at collection time.
- Deduplicate items by stable GitHub IDs or deterministic content hash fallback.
- Report `ready` and `stable` separately from `items`.

Outcomes:

- `success`: feedback artifact written and CodeRabbit is ready/stable; items may be empty only in this state.
- `fixable`: CodeRabbit is not ready/stable but workflow budget remains and the step should loop or be retried under configured caps.
- `fatal`: GitHub/API/auth/schema failure, readiness/stabilization budget exhausted, or unresolvable stale state unless tested routable `abandon` semantics are added.

### `evaluate_coderabbit_feedback`

Type: `feedback_evaluator` or equivalent one-call-per-item evaluator.

Inputs:

- `{artifact_dir}/pr.json`
- `{artifact_dir}/coderabbit-feedback.json`
- `{artifact_dir}/coderabbit-feedback-state.json`
- issue markdown
- plan markdown
- PR diff or bounded relevant excerpts
- output path `{artifact_dir}/feedback-evaluations.json`

LLM prompt policy:

- Treat all CodeRabbit severity labels equally.
- Do not reject an issue just because it is labelled minor/nit/code quality.
- Accept small in-scope improvements, especially regression tests.
- Reject factual mistakes with evidence.
- Reject scope expansions outside the PR unless they are test coverage for the changed behavior.
- Return strict JSON only for the single item being evaluated.

Evaluation contract:

- Invoke the LLM once per current feedback item that lacks a reusable accepted evaluation for the same item key/body hash/head SHA.
- Accept exactly one validated result per item key/body hash/head SHA.
- Validate item key, body hash, head SHA, decision enum, reason, and recommended action.
- Retry malformed output per item up to the configured retry limit.
- Reuse previously accepted unchanged evaluations across loops/resumes, but include them once in the current aggregate artifact.

Outcomes:

- `success`: every current feedback item has exactly one accepted valid evaluation.
- `fixable`: evaluator output malformed for at least one item but retry budget remains.
- `fatal`: repeated malformed output, missing required artifacts, or mismatched/stale item identity.

### `build_pr_remediation_plan`

Type: `pr_remediation_plan` deterministic executor.

Inputs:

- `{artifact_dir}/pr.json`
- `{artifact_dir}/pr-check-status.json`
- `{artifact_dir}/ci-failures.json`
- `{artifact_dir}/coderabbit-feedback.json`
- `{artifact_dir}/coderabbit-feedback-state.json`
- `{artifact_dir}/feedback-evaluations.json`
- output path `{artifact_dir}/pr-remediation-plan.json`

Outcomes:

- `success`: remediation plan is clean; no code changes are required and marker can run for any pending invalid/out-of-scope comments.
- `fixable`: there are current-head CI failures or valid CodeRabbit items to fix.
- `fatal`: malformed/missing inputs, needs-user-judgment items, watch timeout, CodeRabbit readiness/stabilization exhausted, persistent unknown check state, or stale-only state unless a tested routable `abandon` outcome is implemented.

The outcome mapping must be representable by the current workflow transition schema. Do not specify transitions that depend on `StepOutcome::Abandon` unless the engine implementation plan explicitly changes terminal abandon behavior with tests.

### `remediate_pr_feedback`

Type: `llxprt`.

Inputs:

- `{artifact_dir}/pr.json`
- `{artifact_dir}/pr-remediation-plan.json`
- `{artifact_dir}/ci-failures.json`
- `{artifact_dir}/feedback-evaluations.json`
- issue, plan, PR diff
- stdout/stderr artifact files
- output path `{artifact_dir}/pr-remediation-result.json`
- `success_on_diff = true`

Prompt requirements:

- Fix only `must_fix` items and CI failures from the remediation plan.
- Do not fix invalid/out-of-scope items.
- Do not broaden the PR beyond the original issue and accepted feedback.
- Run local targeted verification when possible.
- Write structured results for every CI failure and valid CodeRabbit item, including action taken or reason no code change was needed.
- Leave diagnostic artifacts for failures.

Outcomes:

- `success`: remediation changed the target repository or produced structured no-change results, and local verification will run next.
- `fixable`: LLxprt attempted but produced no acceptable diff/result artifact or declared incomplete while retry budget remains.
- `fatal`: system/runtime error, stale input head, or malformed/missing remediation result after retry exhaustion.

### `validate_pr_remediation_result`

Type: deterministic validator, possibly part of `pr_remediation_plan` or `github_feedback_marker`.

Inputs:

- `{artifact_dir}/pr-remediation-plan.json`
- `{artifact_dir}/pr-remediation-result.json`
- current git/head state when available

Outcomes:

- `success`: every `must_fix` CI failure and valid feedback item has a structured remediation result.
- `fixable`: result is incomplete or malformed but remediation retry budget remains.
- `fatal`: result remains incomplete/malformed after retry exhaustion or input head mismatch cannot be recovered.

### `mark_pr_feedback`

Type: `github_feedback_marker` deterministic executor.

Inputs:

- `{artifact_dir}/pr.json`
- `{artifact_dir}/coderabbit-feedback.json`
- `{artifact_dir}/coderabbit-feedback-state.json`
- `{artifact_dir}/feedback-evaluations.json`
- `{artifact_dir}/pr-remediation-plan.json`
- `{artifact_dir}/pr-remediation-result.json`
- output path `{artifact_dir}/pr-feedback-marker-report.json`

Behavior:

- For each valid fixed item, comment with action taken from the remediation result and resolve the review thread when possible.
- For each invalid/out-of-scope item, comment with recorded reason and resolve when policy allows.
- For needs-user-judgment items, do not resolve; route using current representable outcomes unless tested routable `abandon` semantics are added.
- Use GraphQL mutations for review-thread resolution where available; record REST/GraphQL fallback behavior when resolution state or mutation support is unavailable.
- Use idempotency keys and existing marker state to avoid duplicate comments across loops/resumes.
- Use file-based comment bodies or GraphQL mutations; do not interpolate raw review text into shell commands.

Outcomes:

- `success`: comments/resolution actions completed, already completed, or intentionally skipped with recorded nonfatal reasons.
- `fatal`: GitHub/API/auth/schema failure, non-idempotent marker ambiguity, or unresolved needs-human condition unless tested routable `abandon` semantics are added.

## Loop structure

Recommended representable transitions:

```text
create_pr success → capture_pr_identity
capture_pr_identity success → watch_pr_checks
watch_pr_checks success → collect_coderabbit_feedback
watch_pr_checks fixable → collect_check_failures
watch_pr_checks fatal → abandon_and_log
collect_check_failures success → collect_coderabbit_feedback
collect_check_failures fatal → abandon_and_log
collect_coderabbit_feedback success → evaluate_coderabbit_feedback
collect_coderabbit_feedback fixable → watch_pr_checks_or_collect_coderabbit_feedback_retry (bounded)
collect_coderabbit_feedback fatal → abandon_and_log
evaluate_coderabbit_feedback success → build_pr_remediation_plan
evaluate_coderabbit_feedback fixable → evaluate_coderabbit_feedback (bounded per-item retry)
evaluate_coderabbit_feedback fatal → abandon_and_log
build_pr_remediation_plan success → mark_pr_feedback
build_pr_remediation_plan fixable → remediate_pr_feedback
build_pr_remediation_plan fatal → abandon_and_log
remediate_pr_feedback success → validate_pr_remediation_result
remediate_pr_feedback fixable → remediate_pr_feedback (bounded)
remediate_pr_feedback fatal → abandon_and_log
validate_pr_remediation_result success → run_tests
validate_pr_remediation_result fixable → remediate_pr_feedback (bounded)
validate_pr_remediation_result fatal → abandon_and_log
run_tests success → push_changes
run_tests fixable/fatal → abandon_and_log or existing workflow behavior
push_changes success → capture_pr_identity (bounded PR feedback loop)
mark_pr_feedback success → log_completion
mark_pr_feedback fatal → abandon_and_log
```

No transition may require ambiguous branching such as `success → A or B`. If a step needs to choose between two next steps, it must expose a deterministic representable outcome (`success`, `fixable`, `fatal`, or explicitly tested additional behavior) or write an artifact consumed by a deterministic classifier step that exposes such an outcome.

The workflow must include loop caps:

- PR check watch budget: default 60 minutes / 5-minute increments.
- CodeRabbit readiness/stabilization budget, aligned with the PR follow-through loop budget and never treating empty-not-ready feedback as clean.
- PR remediation iterations after PR creation: default maximum 3.
- Per-remediation LLxprt retry/self-loop: default maximum 2.
- CodeRabbit feedback evaluation retry for malformed output: default maximum 2 per item.
- Marker idempotency checks on every loop/resume.

## Data dependencies

The workflow must capture PR identity from `create_pr` into context and `pr.json`. If `gh pr create` output is not stable enough, `create_pr` should write a JSON artifact using structured `gh pr view` or GitHub API data after creation.

Required artifact paths:

```text
{artifact_dir}/pr.json
{artifact_dir}/pr-check-status.json
{artifact_dir}/ci-failures.json
{artifact_dir}/ci-logs/
{artifact_dir}/coderabbit-feedback.json
{artifact_dir}/coderabbit-feedback-state.json
{artifact_dir}/feedback-evaluations.json
{artifact_dir}/pr-remediation-plan.json
{artifact_dir}/pr-remediation-result.json
{artifact_dir}/pr-feedback-marker-report.json
{artifact_dir}/pr-remediation-stdout.txt
{artifact_dir}/pr-remediation-stderr.txt
```

Every PR/check/feedback/remediation/marker artifact must include or reference the same PR number, head ref, head SHA, and base ref unless it intentionally records stale state. Artifacts collected for an older head SHA must be ignored for current success/failure decisions and reported as stale.

## Determinism boundaries

Deterministic:

- PR identity discovery and current-head validation.
- Check watch and classification, including skipped/neutral/cancelled/unknown/stale behavior.
- CI failure collection, including missing-log semantics.
- CodeRabbit feedback collection, readiness/stabilization, deduplication, and state persistence.
- Aggregating valid/invalid/user-judgment items from LLM JSON decisions.
- Validating exactly one accepted evaluation per feedback item/body/head.
- Validating remediation-result coverage.
- Commenting/resolving with recorded reasons and idempotency keys.
- Loop completion decisions.

LLM:

- Whether a CodeRabbit feedback item is valid/in-scope.
- Reason and recommended action for each feedback item.
- Code changes to fix valid feedback and CI failures.
- Structured description of remediation action taken, subject to deterministic validation.

## GitHub API expectations

Workflow implementation depends on the engine using structured GitHub data:

- PR identity and current head must come from `gh pr view --json` or REST/GraphQL equivalents, not scraped text.
- Check state must come from `gh pr checks --json`, check-runs/status APIs, or GraphQL equivalents with head SHA binding.
- Review-thread collection and resolution should use GraphQL where possible.
- Issue comments, review comments, and PR reviews may use REST fallbacks.
- If resolution state or mutations are unavailable, the workflow must record that fallback state and avoid claiming a thread was resolved.

## Testing strategy

Workflow tests must verify:

- The post-PR steps exist in `llxprt-issue-fix-v1`.
- `create_pr` no longer transitions directly to `log_completion`.
- PR identity capture is reachable after `create_pr` and writes number, URL, head ref, head SHA, and base ref.
- `watch_pr_checks` is reachable after PR identity capture.
- Check watch is non-fail-fast and binds state to current head SHA/ref.
- Failed/cancelled/timed-out current-head checks route to failure collection and remediation.
- Pending-after-budget, unknown-after-budget, stale-only, and needs-human conditions route through outcomes supported by the current engine, or through an explicitly planned tested engine change.
- Passing checks route to CodeRabbit collection/evaluation only after current-head validation.
- Empty CodeRabbit feedback routes to clean only when readiness/stabilization criteria are satisfied.
- CodeRabbit readiness/stabilization retry/failure routes are bounded and representable.
- Each current feedback item receives exactly one accepted evaluation, with per-item malformed-output retry behavior.
- Valid feedback routes to remediation; clean feedback routes to marking/completion.
- Remediation result validation requires coverage for every accepted feedback item and CI failure.
- Marker behavior is idempotent and records per-item action/resolution results.
- Fatal outcomes from post-PR deterministic steps route to `abandon_and_log` or another existing terminal failure step.
- Loop caps exist on PR remediation, watch cycles, feedback readiness, and evaluation retries.
- JSON fixtures mirror TOML fixtures.

## Plan compliance gaps to cover in future detailed plans

This overview is not the complete implementation plan. Future plan artifacts must include:

- The exact TOML transition encoding with no ambiguous branch notation.
- The chosen representation for needs-human/watch-timeout outcomes under current `StepOutcome::Abandon` terminal behavior, or the explicit engine behavior change and tests that make abandon routable.
- The exact CodeRabbit readiness/stabilization criteria and loop counters.
- The exact GitHub REST/GraphQL queries and fallback order.
- The exact schemas for feedback state, remediation result, and marker report artifacts.
- The exact idempotency strategy for marker comments and evaluation reuse.
- Fixture updates that prove stale head SHA artifacts are ignored rather than treated as clean or failed current state.

## Acceptance Criteria

- The workflow does not complete immediately after PR creation.
- The workflow captures deterministic PR identity with number, URL, head ref, head SHA, and base ref.
- The workflow can supervise current-head PR checks without a human `gh pr checks --watch` loop.
- Check completion/failure state is determined by structured GitHub data and watched non-fail-fast until all checks are terminal or the one-hour budget is exhausted.
- Pending, stale, unknown, cancelled, skipped, neutral, and missing-log states follow an explicit classification policy.
- CI failures and CodeRabbit comments become structured artifacts bound to the current PR head.
- Empty CodeRabbit feedback is considered clean only after CodeRabbit readiness/stabilization criteria are satisfied.
- Every current CodeRabbit item receives exactly one accepted recorded LLM evaluation with a reason.
- Valid items and CI failures are sent to a remediation step.
- Remediation writes structured per-feedback-item and per-CI-failure results.
- Invalid/out-of-scope CodeRabbit items are commented with recorded reasons, and valid fixed items are commented with recorded actions taken.
- Marker actions are idempotent across loops/resumes and record per-item outcomes.
- The workflow loops after remediation until checks are green and feedback is clean, or bounded failure conditions are reached through transitions representable by the current engine schema.
