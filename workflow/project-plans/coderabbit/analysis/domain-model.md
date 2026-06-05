# Phase 02 Domain Model

Plan: `PLAN-20260429-CODERABBIT-PR-FOLLOWUP`
Phase: `P02`

This document names the deterministic entities used by the Phase 02 pseudocode and GitHub API contract. Production Rust structs are introduced later; this is an analysis-only contract.

## Entity summary

| Entity | Source of truth | Current-head binding | Produced by | Consumed by |
|---|---|---|---|---|
| PR identity | `pr.json` | `repository_owner`, `repository_name`, `pr_number`, `head_ref`, `head_sha`, `base_ref`, `base_sha` | `capture_pr_identity` | every post-PR step |
| Iteration guard | `post-pr-iteration-guard.json` | PR identity plus `iteration_index` | `post_pr_iteration_guard` | workflow routing and terminal auditing |
| Check status | `pr-check-status.json` | PR identity plus check `head_sha` | `watch_pr_checks` | `collect_ci_failures` |
| CI failures | `ci-failures.json` | PR identity plus source check sequence | `collect_ci_failures` | `build_remediation_plan`, remediation prompt |
| CodeRabbit feedback | `coderabbit-feedback.json` | PR identity plus feedback item `commit_sha` or observed head | `collect_coderabbit_feedback` | `evaluate_coderabbit_feedback`, marker |
| Feedback state | `coderabbit-feedback-state.json` | stable marker key, body hash, head SHA | `collect_coderabbit_feedback`, evaluator, marker | evaluator reuse and marker idempotency |
| Feedback evaluations | `feedback-evaluations.json` | item ID, stable marker key, body hash, head SHA | `evaluate_coderabbit_feedback` | `build_remediation_plan`, marker |
| Remediation plan | `pr-remediation-plan.json` | current input head SHA | `build_remediation_plan` | remediation wrapper and validator |
| Remediation run | `pr-remediation-llxprt-run.json` | input head SHA and plan artifact sequence | `remediate_pr_followup` | validator and terminal |
| Remediation result | `pr-remediation-result.json` | input and output head SHAs | `remediate_pr_followup`, `validate_remediation_result` | tests, push, marker |
| Post-PR tests | `post-pr-test-result.json` | current local head SHA and plan sequence | `run_post_pr_tests` | push and terminal |
| Push result | `push-remediation-result.json` | local, remote, and PR head SHAs | `push_remediation_changes` | capture loop and terminal |
| Pending marker actions | `pending-feedback-marker-actions.json` | source head and remediation output head | plan builder and validator | marker executor |
| Marker report | `pr-feedback-marker-report.json` | marker execution head | `mark_coderabbit_feedback` | completion and terminal |
| Terminal report | `post-pr-failure-terminal.json` | best available PR binding | `post_pr_failure_terminal` | run failure evidence |

## State vocabulary

| Domain | Artifact state field | Values |
|---|---|---|
| PR identity | `capture_state` | `captured`, `fatal` |
| Iteration guard | `guard_state` | `proceed`, `max_iterations_exceeded`, `fatal` |
| Checks | `overall_state` | `passed`, `failed`, `pending_timeout`, `unknown`, `fatal` |
| CI failure collection | `collection_state` | `collected`, `fatal` |
| CodeRabbit readiness | `readiness_state` | `ready`, `not_ready`, `timeout`, `fatal` |
| Evaluation | `evaluation_state` | `complete`, `incomplete`, `budget_exhausted`, `fatal` |
| Remediation plan | `plan_state` | `clean`, `needs_remediation`, `blocked_needs_user_judgment`, `fatal` |
| Remediation invocation | `remediation_invocation_state` | `success`, `success_without_result`, `timeout`, `spawn_failed`, `retryable_failed`, `fatal` |
| Remediation validation | `validation_state` | `valid`, `fixable_malformed`, `invalid`, `fatal`, `valid_but_unsuccessful`, `fixable_cap_exhausted`, `unsuccessful_remediation_cap_exhausted` |
| Post-PR tests | `test_state` | `passed`, `failed`, `fatal` |
| Push | `push_state` | `no_change`, `no_change_excluded_only`, `pushed`, `retryable_failed`, `retry_exhausted`, `fatal` |
| Marker | `marker_state` | `complete`, `partial`, `fatal` |
| Terminal | `terminal_state` | `fatal` |

## Deterministic boundaries

- GitHub, Actions, review, and marker data is discovered only through fixture-backed API paths in `github-api-contract.md`.
- PR check state and CodeRabbit readiness are never delegated to the LLM.
- The LLM judges one feedback item at a time and writes remediation changes, but deterministic validators decide whether the structured result can route onward.
- Artifact-backed retry and iteration caps own product behavior. Workflow transition loop caps are only defensive.
