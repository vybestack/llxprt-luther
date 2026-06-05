# Plan: CodeRabbit PR Follow-Through Engine and Workflow Support

Plan ID: PLAN-20260429-CODERABBIT-PR-FOLLOWUP
Generated: 2026-04-29
Total Phase Entries: 22 (Phase 0.5 plus numbered Phases 01-21). Execution order is exactly P0.5 -> P01 -> P02 -> P03 -> P04 -> P05 -> P06 -> P07 -> P08 -> P09 -> P10 -> P11 -> P12 -> P13 -> P14 -> P15 -> P16 -> P17 -> P18 -> P19 -> P20 -> P21.
Requirements: REQ-PRFU-001 through REQ-PRFU-035

## Critical Reminders

Before implementing ANY phase, ensure you have:

1. Completed preflight verification (Phase 0.5)
2. Defined integration contracts for multi-component features
3. Written integration tests BEFORE unit tests
4. Verified all dependencies and types exist as assumed
5. Preserved the deterministic/LLM boundary: the LLM may judge feedback and write fixes, but it must not decide PR check state, discover failures, discover CodeRabbit comments, or decide loop completion.

## Purpose

Replace the unreliable human post-PR loop with workflow-executed deterministic PR follow-through. The system will watch PR checks for up to one hour in five-minute increments, gather failures and CodeRabbit feedback deterministically, evaluate each feedback item with one accepted validated LLM judgment result, remediate valid items and CI failures, then deterministically comment/resolve feedback with recorded reasons.

This is intentionally ironic: we are creating the plan and workflow support so future Luther runs stop needing a human agent to do the workflow after the workflow creates a PR. I will pretend to enjoy that.

## Source Specifications

- Engine functional/technical spec: `project-plans/coderabbit/engineupdates/overview.md`
- Workflow functional/technical spec: `project-plans/coderabbit/workflowupdates/overview.md`
- EARS requirements: `project-plans/coderabbit/engineupdates/REQUIREMENTS.md`
- Planning guide: `dev-docs/PLAN.md`
- Plan template: `dev-docs/PLAN-TEMPLATE.md`

Conflict-resolution note: this implementation plan is the controlling source for PR follow-through implementation and supersedes the transition sketch in `project-plans/coderabbit/workflowupdates/overview.md` wherever they conflict. The following are explicitly invalid unless this plan is amended with fixture-backed tests that prove the new behavior is safe: any post-PR route to `abandon_and_log`; any workflow-level feedback-evaluator malformed-output self-loop; raw llxprt post-PR remediation without the `PrFollowupRemediationExecutor` artifact/validator wrapper; and generic `push_changes` or `run_tests` reuse for post-PR remediation instead of the dedicated `push_remediation_changes` and `run_post_pr_tests` contracts.


Known superseded source-spec clauses:

| Source spec clause | Superseded by this plan | Required v1 behavior |
|--------------------|-------------------------|----------------------|
| Workflow overview transition sketch routing post-PR fatal/non-success outcomes to `abandon_and_log` | Routing contract and REQ-PRFU-018 sections in this plan | Post-PR fatal/non-success routes to `post_pr_failure_terminal`, which returns fatal and yields overall failure/non-success. |
| Workflow overview/readiness/evaluator workflow self-loops for CodeRabbit readiness or malformed evaluator output | Internal bounded executor policies for `collect_coderabbit_feedback` and `evaluate_coderabbit_feedback` | Readiness observations and evaluator retries are internal to the executor; TOML has no readiness/evaluator self-loop. |
| Raw `run_tests` / `push_changes` reuse in post-PR tail | Dedicated `run_post_pr_tests` and `push_remediation_changes` executor contracts | Post-PR verification and push use dedicated artifact-producing executors only. |
| REQ-PRFU-018 wording or older overview wording implying `abandon_and_log` for bounded post-PR failure | `post_pr_failure_terminal` strategy in this plan | Bounded post-PR non-success writes terminal artifacts and ends through `post_pr_failure_terminal` with failure/non-success. |


## Architecture Summary

### Deterministic engine additions

Add reusable executors or cohesive executor modules for:

- PR identity capture (`github_pr_identity`)
- PR check watching/classification (`github_pr_checks`)
- CI failure/log collection (`github_check_failures`)
- CodeRabbit feedback collection/readiness/state (`github_coderabbit_feedback`)
- Feedback item evaluation runner (`feedback_evaluator`)
- Remediation plan aggregation (`pr_remediation_plan`)
- Remediation llxprt wrapper/failure-artifact bridge (`pr_followup_remediation`)
- Remediation result validation (`pr_remediation_result`)
- Dedicated post-PR local verification (`run_post_pr_tests`)
- Feedback marking/commenting/resolution (`github_feedback_marker`)

### Workflow integration

Update `llxprt-issue-fix-v1` so it does not finish immediately after `create_pr`. It must capture PR identity, watch checks, collect failures/feedback, evaluate feedback, remediate valid items/failures, push, and loop until clean or bounded non-success.


#### Exact post-PR workflow routing contract

Hard gate: before implementation phases begin, `project-plans/coderabbit/analysis/github-api-contract.md` must define exact JSON paths and fixtures for every GitHub API/`gh` field consumed by this routing contract. Implementation phases may not infer response paths from prose or live output; they must consume only documented fixture-backed paths.

The routing contract must use only the current globally valid `StepOutcome` condition strings: `success`, `fixable`, `retryable`, `fatal`, and `abandon`. Workflow TOML must not introduce custom routable outcome strings such as `passed`, `ready`, `clean`, `valid`, `invalid`, `continue`, or `needs_user_judgment`. Richer meanings such as passed checks, readiness, clean plan, invalid evaluation, timeout, unknown, and needs-user-judgment belong inside JSON artifacts and must be converted to routable outcomes by deterministic executor/classifier logic. Although `abandon` remains a valid engine outcome string globally for pre-existing workflows, post-PR executors and the `llxprt-issue-fix-v1` post-PR TOML tail must not return or route `abandon` unless a separate engine change is added with fixture-backed tests proving safe post-PR semantics. P03 stubs must not return `StepOutcome::Abandon`; P16/P17 graph and executor-contract tests must fail if any route reachable from `capture_pr_identity` uses condition `abandon` or if any post-PR executor test returns `StepOutcome::Abandon`.

This plan chooses a new terminal non-success strategy: add a deterministic `post_pr_failure_terminal` executor/step that logs the final failure artifact and returns `StepOutcome::Fatal`. Post-PR timeout, unknown, needs-user-judgment, API failures, iteration caps, and malformed artifacts must route to `post_pr_failure_terminal`, not to a successful `abandon_and_log` step that could make the run finish as `RunOutcome::Success`. Add unit and workflow tests proving `post_pr_failure_terminal` returns `StepOutcome::Fatal` after writing its log artifact and that post-PR non-success paths never report overall success. Graph/fake-runner assertions must expect overall `RunOutcome::Failure { step_id: "post_pr_failure_terminal", ... }` (or the project-equivalent failure payload naming), not an impossible `RunOutcome::Fatal`.

| Step ID | Executor/type | Allowed routable outcomes | Artifact semantic states | Transition contract | Loop/cap | Fatal/non-success route |
|---------|---------------|---------------------------|--------------------------|---------------------|----------|-------------------------|
| `create_pr` | existing PR creation | `success`, existing failure outcomes mapped to `fatal`/`retryable` | existing PR creation state | `success -> capture_pr_identity`; failure outcomes keep existing non-success handling or route to `post_pr_failure_terminal` if inside post-PR tail | none | existing failure route before PR tail |
| `capture_pr_identity` | `github_pr_identity` | `success`, `fatal` | `pr.json:capture_state = captured|fatal` | `success -> post_pr_iteration_guard`; `fatal -> post_pr_failure_terminal` | none | malformed/missing PR identity, missing PR number/URL/head SHA |
| `post_pr_iteration_guard` | deterministic guard | `success`, `fatal` | `post-pr-iteration-guard.json:guard_state = proceed|max_iterations_exceeded|fatal` | `success -> watch_pr_checks`; `fatal -> post_pr_failure_terminal` | `max_post_pr_remediation_iterations = 3` means the initial current-head cycle uses `iteration_index = 0`, then up to 3 remediation head-change cycles may proceed with indexes 1, 2, and 3; index 4 is cap exhaustion | cap exceeded or unreadable guard state are fatal/non-success artifact paths |
| `watch_pr_checks` | `github_pr_checks` | `success`, `fixable`, `fatal` | `pr-check-status.json:overall_state = passed|failed|pending_timeout|unknown|fatal` | `success -> collect_ci_failures`; `fixable -> collect_ci_failures`; `fatal -> collect_ci_failures` always in v1; no direct `watch_pr_checks fatal -> post_pr_failure_terminal` route is allowed | max 12 observations at t=0,5,...,55 minutes with interval 300s; no t=60 observation; fake clock/sleeper in tests | timeout/unknown/API/auth/schema errors record semantic state and return `fatal`; `collect_ci_failures` is the deterministic classifier/bridge that preserves watcher fatal evidence and decides whether downstream CodeRabbit evaluation may continue. Concrete failures are remediable only when all current-head checks are terminal and no persistent pending/unknown exists. |
| `collect_ci_failures` | `github_check_failures` | `success`, `fatal` | `ci-failures.json:collection_state = collected|fatal` | `success -> collect_coderabbit_feedback`; `fatal -> post_pr_failure_terminal` | none | v1 deterministic classifier/bridge. It always reads `pr-check-status.json`, writes `ci-failures.json`, and initializes `failures` plus `pending_or_unknown` arrays. Passed-check input writes empty arrays and returns `success`. API/auth/schema watcher fatal or no trusted current-head checks writes `collection_state=fatal`, `fatal_source`/`watcher_fatal_source`, no invented failures, preserved watcher evidence, and returns `fatal`. Mixed final states collect concrete current-head failed/cancelled/timed_out/action_required checks into `failures`, preserve pending/unknown/stale-untrusted evidence in `pending_or_unknown`, and return `fatal` whenever `pending_or_unknown` is non-empty or any watcher fatal source is present. It returns `success` only when downstream CodeRabbit evaluation may continue: passed checks or concrete all-terminal failures with no pending/unknown/fatal watcher source. |
| `collect_coderabbit_feedback` | `github_coderabbit_feedback` | `success`, `fatal` | `coderabbit-feedback.json:readiness_state = ready|not_ready|timeout|fatal` | `success -> evaluate_coderabbit_feedback`; `fatal -> post_pr_failure_terminal` | collector-internal bounded observations only: max 6 observations, stable ready = 2 identical observations, observation interval default 300s from config key `coderabbit_readiness_observation_interval_seconds`, no workflow self-loop, fake clock/sleeper in tests | not-ready budget exhaustion, timeout, API/auth/schema errors write artifact and return `fatal` |
| `evaluate_coderabbit_feedback` | `feedback_evaluator` | `success`, `fatal` | `feedback-evaluations.json:evaluation_state = complete|incomplete|budget_exhausted|fatal` | `success -> build_remediation_plan`; `fatal -> post_pr_failure_terminal` | internal per-item retries only; no workflow self-loop for malformed LLM output | malformed-output budget exhaustion that leaves any item without an accepted decision records rejected/unevaluated data and returns `fatal` |
| `build_remediation_plan` | `pr_remediation_plan` | `success`, `fixable`, `fatal` | `pr-remediation-plan.json:plan_state = clean|needs_remediation|blocked_needs_user_judgment|fatal` | `success -> mark_coderabbit_feedback`; `fixable -> remediate_pr_followup`; `fatal -> post_pr_failure_terminal` | none | malformed inputs or any needs-user-judgment/timeout/unknown semantic item route fatal/non-success |
| `remediate_pr_followup` | `pr_followup_remediation` wrapper (`PrFollowupRemediationExecutor`) invoking llxprt with the exact remediation prompt/parameter contract | `success`, `fatal` | `pr-remediation-llxprt-run.json:remediation_invocation_state = success|success_without_result|timeout|spawn_failed|retryable_failed|fatal` plus process/log metadata; `pr-remediation-result.json` must still be validated | `success -> validate_remediation_result`; `fatal -> post_pr_failure_terminal` | no llxprt self-loop; validation retry cap is owned by `validate_remediation_result` | Wrapper `success` means only that the validator should run, including cases where llxprt failed but the wrapper wrote a validator-readable failure/result artifact. Wrapper `fatal` means no validator-readable artifact exists or terminal logging must run. TOML must not branch on artifact readability. |
| `mark_coderabbit_feedback` | `github_feedback_marker` | `success`, `fatal` | `pr-feedback-marker-report.json:marker_state = complete|partial|fatal` | `success -> log_completion`; `fatal -> post_pr_failure_terminal`; no other marker outcomes/routes are allowed in v1 | none | partial marker action, marker API failure, idempotency ambiguity, unavailable required resolution, or artifact failure |

| `validate_remediation_result` | `pr_remediation_result` | `success`, `fixable`, `fatal` | `pr-remediation-result.json:validation_state = valid|fixable_malformed|invalid|fatal` plus `validation_retry_index`, `max_validation_retries`, `retry_scope` | `success -> run_post_pr_tests`; `fixable -> remediate_pr_followup`; `fatal -> post_pr_failure_terminal` | deterministic artifact-backed validator retry cap: max 2 fixable remediation-result validation retries per same PR head/plan; reset only on new head after push | incomplete/missing result after cap, malformed non-empty result, head mismatch, invalid schema |
| `run_post_pr_tests` | dedicated `run_post_pr_tests` executor step type registered in the default registry; step ID is also `run_post_pr_tests` | `success`, `fixable`, `fatal` | `post-pr-test-result.json:test_state = passed|failed|fatal` plus `verification_retry_index`, `max_verification_retries`, `retry_scope` | `success -> push_remediation_changes`; `fixable -> remediate_pr_followup`; `fatal -> post_pr_failure_terminal` | deterministic artifact-backed local verification retry cap: max 2 fixable verification-to-remediation retries per same PR head/plan; reset only on new head after push | local verification failures loop to remediation while the artifact-backed cap remains; retry exhaustion or infrastructure failure routes fatal |
| `push_remediation_changes` | dedicated `push_remediation_changes` executor step type (`PushRemediationChangesExecutor`) | `success`, `retryable`, `fatal` | `push-remediation-result.json:push_state = no_change|no_change_excluded_only|pushed|retryable_failed|retry_exhausted|fatal` plus `push_retry_index`, `max_push_retries`, `retry_scope`, `remote_ref`, pre/local/remote/committed head SHAs, deterministic staging/commit metadata, and command/log artifact metadata | `success -> capture_pr_identity`; `retryable/fatal -> post_pr_failure_terminal` unless a dedicated artifact-backed push retry loop is explicitly configured | primary cap is `post_pr_iteration_guard` after successful pushes that change head; retryable push cycles require the executor's deterministic artifact-backed cap and must not rely on engine transition max_iterations | push retry exhaustion, push fatal, command-runner failure, artifact write failure, or unconfigured retryable failure |

| `post_pr_failure_terminal` | new deterministic terminal logger | `fatal` only | `post-pr-failure-terminal.json:terminal_state = fatal` | terminal `StepOutcome::Fatal`, causing overall `RunOutcome::Failure` | none | all bounded non-success post-PR paths |

Critical loop-cap rule for the current engine: post-PR cap semantics must never rely on transition `max_iterations` as the expected behavior. The current runner falls back to global `max_loops` when a transition omits `max_iterations`; if that global cap fires first, the run may abandon before post-PR terminal artifact logging. Therefore every post-PR retry/loop that is part of product semantics must be capped first by deterministic artifact-backed guard/validator executors that can write exhaustion artifacts and return `StepOutcome::Fatal` to `post_pr_failure_terminal`. Every post-PR loop-back transition in TOML must set an explicit high defensive `max_iterations` value; omitting `max_iterations` on a post-PR loop-back is forbidden. Defensive values must be high enough that configured artifact-backed caps always fire first in normal and test fixtures, and tests must assert the artifact-backed cap is the observed cap behavior. No post-PR test may accept `RunOutcome::Abandoned` as the expected result for a semantic cap; expected cap exhaustion is a failure-producing artifact followed by `post_pr_failure_terminal` and fatal/non-success run completion.


Ambiguous branches are prohibited: each routable outcome above maps to exactly one next step for that step. When a semantic state needs more precision than the five `StepOutcome` values, the executor must write that state to its artifact and return the appropriate routable outcome. If a future branch cannot be represented by `success`/`fixable`/`retryable`/`fatal`/`abandon` without ambiguity, add a deterministic classifier executor that reads the artifact and returns one of the allowed outcomes; do not invent a new routable outcome string in TOML.
Mixed check-state collection rule: v1 always routes `watch_pr_checks fatal -> collect_ci_failures`; a direct watcher-fatal terminal route is forbidden in P16/P17 assertions. `collect_ci_failures` is the deterministic classifier/bridge: it preserves `pending_or_unknown`, preserves `fatal_source`/`watcher_fatal_source`, collects concrete terminal current-head failures when available, returns `fatal` for any pending/unknown evidence or watcher fatal source, and returns `success` only when downstream CodeRabbit evaluation may continue. `build_remediation_plan` must never put pending/unknown evidence into `must_fix`.



#### Post-PR iteration cap strategy

Use a deterministic `post_pr_iteration_guard` artifact/step rather than relying on implicit engine loop behavior or mutable in-memory state. The algorithm must be compatible with current `StepContext` limitations: the guard can read current-step config, the current run/artifact directory, and persisted artifacts, but it must not require direct access to the previously executed step.

Exact v1 algorithm:

1. Read the current `pr.json`; reject missing or mismatched binding fields.
2. Read all prior `post-pr-iteration-guard*.json` artifacts for the same `run_id`, `repository_owner`, `repository_name`, and `pr_number`; ignore stale/mismatched artifacts and record their paths in `ignored_stale_artifacts`.
3. If no accepted prior guard exists for this PR/run, write initial entry `iteration_index = 0`, `previous_head_sha = null`, `head_sha = pr.head_sha`, `reason = initial_entry`, `guard_state = proceed`, and return `success`.
4. If the latest accepted guard has the same `head_sha` as the current `pr.json`, write a new observation preserving the same `iteration_index`, set `previous_head_sha` to that same SHA, set `reason = same_head_reentry`, and return `success`. This covers retry/resume and same-head re-entry after a push step that produced no new commit.
5. If the latest accepted guard has a different `head_sha`, treat it as a remediation-push head change, increment `iteration_index` by 1, set `previous_head_sha` to the prior accepted head, set `reason = head_sha_changed_after_remediation_push`, and continue only if `iteration_index <= max_post_pr_remediation_iterations` (with default 3, indexes 1, 2, and 3 are permitted remediation head-change cycles after initial index 0).
6. On resumed runs, if prior artifacts for the same run are present, continue from the latest accepted same-run guard. If this is a new `run_id`, start at `iteration_index = 0` even when the PR/head matches an older run; older-run artifacts may be reported as historical but must not consume the new run's cap.
7. If the incremented `iteration_index` would exceed `max_post_pr_remediation_iterations`, write `guard_state = max_iterations_exceeded`, record the current and previous head SHAs, and return `fatal` so the workflow reaches `post_pr_failure_terminal`.
8. If guard artifacts are unreadable, schema-invalid, or cannot be unambiguously bound to the current PR/run, write `guard_state = fatal` with `reason = unreadable_or_unbindable_guard_state` and return `fatal`.

`post-pr-iteration-guard.json` must record `{schema_version, run_id, repository_owner, repository_name, pr_number, head_ref, head_sha, base_ref, base_sha, iteration_index, max_post_pr_remediation_iterations, previous_head_sha, reason, guard_state, ignored_stale_artifacts, updated_at}`. Tests must cover initial entry at index 0, same-head retry/resume preserving index, same-head after push preserving index, head-change increments to indexes 1/2/3, a fourth remediation head change writing cap exceeded at attempted index 4, stale guard ignored, mismatched binding rejected, and unreadable guard fatal.
#### Post-PR retry and cap artifact semantics



All post-PR caps that affect product behavior are deterministic artifact-backed caps, not engine transition caps. The required cap artifacts are:

| Loop | Artifact family | Required fields | Default max | Scope/reset semantics | Exhaustion behavior |
|------|-----------------|-----------------|-------------|-----------------------|---------------------|
| Remediation-result validation fixable loop (`validate_remediation_result fixable -> remediate_pr_followup`) | `pr-remediation-result.json` history snapshots plus canonical current file | `validation_retry_index`, `max_validation_retries`, `retry_scope`, `plan_artifact_sequence`, `input_head_sha`, `output_head_sha`, binding fields | 2 fixable retries after the first invalid/incomplete validation | Count same `run_id`/PR/current `input_head_sha`/plan artifact sequence. Same-head re-entry increments. A new PR head after successful push resets to 0. A changed remediation plan on the same head resets only when its `plan_artifact_sequence` changes and that change is recorded. | Validator writes `validation_state=invalid` or `fixable_cap_exhausted`, includes failure metadata, returns `StepOutcome::Fatal`, routes to `post_pr_failure_terminal`. |
| Same-head successful-structure but unsuccessful-remediation loop (`validate_remediation_result fixable -> remediate_pr_followup` after a structurally valid result reports no acceptable fix evidence) | `pr-remediation-result.json` history snapshots plus canonical current file | `remediation_attempt_index`, `max_remediation_attempts`, `retry_scope`, `plan_artifact_sequence`, `input_head_sha`, `output_head_sha`, `unsuccessful_statuses`, `no_change_after_remediation`, binding fields | 2 unsuccessful remediation attempts after the first structurally valid but unsuccessful result | Count same `run_id`/PR/current `input_head_sha`/plan artifact sequence. The `retry_scope` is exactly `run_id` + PR binding + `input_head_sha` + `plan_artifact_sequence`. It does not increment `post_pr_iteration_guard` for same-head re-entry. A new PR head after successful push resets to 0. A changed remediation plan on the same head resets only when its `plan_artifact_sequence` changes and that change is recorded. | While the cap remains, validator writes `validation_state=valid_but_unsuccessful`, records the specific unacceptable statuses and `no_change_after_remediation`, returns `StepOutcome::Fixable`, and routes to `remediate_pr_followup`. On exhaustion, validator writes `validation_state=unsuccessful_remediation_cap_exhausted` plus failure metadata and returns `StepOutcome::Fatal` to `post_pr_failure_terminal`. |

| Local verification fixable loop (`run_post_pr_tests fixable -> remediate_pr_followup`) | `post-pr-test-result.json` history snapshots plus canonical current file | `verification_retry_index`, `max_verification_retries`, `retry_scope`, `plan_artifact_sequence`, `remediation_result_artifact_sequence`, `head_sha`, binding fields | 2 fixable verification retries per same head/plan | Count same `run_id`/PR/current `head_sha`/plan artifact sequence. Same-head failed verification increments. A new PR head after successful push resets to 0. | Verifier writes `test_state=failed` with `verification_retry_exhausted=true`, includes failure metadata, returns `StepOutcome::Fatal`, routes to `post_pr_failure_terminal`. |
| Optional push retryable loop, only if the workflow chooses to route `retryable` back to push | `push-remediation-result.json` history snapshots plus canonical current file | `push_retry_index`, `max_push_retries`, `retry_scope`, `head_sha`, `remote_ref`, `push_error_class`, binding fields | 1 retry unless explicitly configured lower/higher | Count same `run_id`/PR/current local `head_sha`/remote ref. Same-head retryable push increments. Any new local head or confirmed successful push resets to 0. | Push executor or push retry guard writes `push_state=retry_exhausted`, includes failure metadata, returns `StepOutcome::Fatal`, routes to `post_pr_failure_terminal`. |

These retry indexes are allocated by scanning accepted same-run history snapshots for the artifact family using binding fields and sequence metadata, not by in-memory counters. Malformed, unbound, or ambiguous retry history is itself a fatal semantic state: write a best-effort artifact with `failure_reason=unreadable_or_unbindable_retry_state`, return `StepOutcome::Fatal`, and route to `post_pr_failure_terminal`. Tests must prove repeated validator-fixable cycles reach `post_pr_failure_terminal` rather than `RunOutcome::Abandoned`; repeated local-verification-fixable cycles reach terminal through `post-pr-test-result.json`; repeated push cycles exceed `post_pr_iteration_guard` or the optional push retry guard and reach terminal; and no post-PR cap test accepts `RunOutcome::Abandoned` as expected.





#### Feedback evaluator retry model

The first version uses internal per-item retries inside `FeedbackEvaluatorExecutor`. Malformed JSON, schema mismatch, unknown decisions, item/body/head mismatches, or missing required reasons are retried within that executor with `max_attempts_per_item = 3`, meaning one initial attempt plus two retries for that item. The workflow must not add an evaluator self-loop for malformed output. `feedback-evaluations.json` must separate `accepted_results`, `rejected_attempts`, `unevaluated_items`, and `budget_exhausted_items`. Budget exhaustion is not an accepted LLM judgment and must not be represented as `accepted_results[*].decision`. If any current item lacks an accepted validated result, the executor writes the rejected/unevaluated/budget-exhausted evidence and returns `fatal` so remediation cannot consume it.

#### Remediation-result enforcement model

The llxprt remediation invocation may succeed as a process while still producing incomplete, invalid, or unsuccessful structured remediation evidence. Therefore workflow routing must send `PrFollowupRemediationExecutor` wrapper `success` to `validate_remediation_result`; the validator is authoritative. Wrapper `success` is a routing signal meaning `validate_remediation_result` should run, not proof that remediation succeeded. The wrapper must also return `success` for timeout/spawn/retryable/fatal command cases when it wrote a validator-readable failure/result artifact for deterministic classification. The wrapper returns `fatal` only when no validator-readable artifact can be produced or terminal logging must run immediately. A non-empty but malformed `pr-remediation-result.json` must never be accepted. If the result is fixably malformed or incomplete and the artifact-backed validation retry cap remains, the validator returns `fixable` and loops back to the wrapper. The loop transition must not rely on engine `max_iterations`; TOML must set an explicit high defensive `max_iterations` on this loop-back, but artifact-backed validation exhaustion must always fire first. If the cap is exhausted or the artifact is semantically invalid, the validator returns `fatal` and routes to `post_pr_failure_terminal`.

Validator success semantics are intentionally stricter than schema validity. The canonical `RemediationResultStatus` enum is exactly `fixed | changed | already_satisfied | not_reproduced | not_fixed | skipped | failed`; no prompt, fixture, schema test, validator, artifact table, or marker policy may use a different status vocabulary. Validator-success statuses are `fixed`, `changed`, `already_satisfied`, and `not_reproduced`; unsuccessful-but-structurally-valid statuses are `not_fixed`, `skipped`, and `failed`. For every current `pr-remediation-plan.json.must_fix` item, `validate_remediation_result` may return StepOutcome `success` only when the binding-valid result contains a validator-success status with deterministic evidence: `fixed` or `changed` require changed files, test/log evidence, or code-location evidence tied to the item; `already_satisfied` requires deterministic current-repository/API evidence proving the item was already satisfied at the same input head, such as the cited code path already matching the requested invariant, the failing test passing without a new change, or the referenced thread/check already being resolved; `not_reproduced` requires deterministic reproduction evidence at the same input head, using a configured reproduction command ID or API lookup that records the exact argv/API endpoint, normalized output/status, and reason the failure/comment cannot be reproduced. `already_satisfied` and `not_reproduced` must populate deterministic evidence fields (`evidence.kind`, `evidence.current_head_sha`, and at least one of `evidence.paths`, `evidence.commands`, `evidence.api_lookups`, or `evidence.check_runs`); explanation-only text is invalid. Statuses `not_fixed`, `skipped`, `failed`, missing result entries, empty evidence, or free-form-only explanation are not validator success. A valid structured artifact containing any unsuccessful status is `valid_but_unsuccessful`, not `valid`: the validator records `unsuccessful_statuses`, computes `no_change_after_remediation` from the output head/worktree evidence, applies the same-head unsuccessful-remediation cap above, and returns `fixable` to `remediate_pr_followup` while the cap remains or `fatal` to `post_pr_failure_terminal` when exhausted. If validation succeeds, local verification must run before pushing: `validate_remediation_result success -> run_post_pr_tests -> push_remediation_changes`. Test failures return `fixable` to remediation while the artifact-backed verification retry cap remains; infrastructure/test-runner fatal conditions route to `post_pr_failure_terminal`. If existing llxprt `success_file` support is compatible, the remediation prompt should require the structured result path as the success file, but the validator must still parse and enforce it.

#### Exact `remediate_pr_followup` llxprt prompt and parameter contract

This plan chooses a concrete wrapper approach: `remediate_pr_followup` uses a dedicated `PrFollowupRemediationExecutor` step type (`pr_followup_remediation`) that invokes the existing llxprt command/executor capability, but owns PR follow-through artifact writing and classification. The raw llxprt process result is never the product state. The wrapper must:

- Read only the current, binding-validated `pr-remediation-plan.json`, `pr.json`, `ci-failures.json`, and `feedback-evaluations.json` artifacts from the `PrFollowupArtifactStore`.
- Render an exact TOML-configured remediation prompt that states: fix only `pr-remediation-plan.json.must_fix`; do not fix or rewrite items in `mark_invalid`, `out_of_scope`, or `needs_user_judgment`; do not make unrelated target-repository changes; write `pr-remediation-result.json` at the configured output path; include one result for every `must_fix` CI failure and every valid feedback item; include no free-form-only completion.
- Pass these parameters explicitly: `remediation_plan_path`, `remediation_result_path`, `input_head_sha`, `repository_owner`, `repository_name`, `pr_number`, `head_ref`, `base_ref`, `artifact_root`, and optional `success_file` equal to `remediation_result_path` only if Phase 0.5 confirmed existing llxprt success-file behavior is compatible.
- Require `pr-remediation-result.json` fields: `schema_version`, binding fields, `input_head_sha`, `output_head_sha`, `overall_status`, and `results[]`, where each result has `source_type` (`ci_failure` or `coderabbit_feedback`), `source_id`, `status` using the canonical enum `fixed | changed | already_satisfied | not_reproduced | not_fixed | skipped | failed`, `action`, `evidence`, and optional `evidence_paths`. `evidence` must be structured; for `already_satisfied` and `not_reproduced`, it must include deterministic proof fields (`kind`, `current_head_sha`, and command/API/path/check evidence as applicable). Free-form stdout/stderr may be captured as logs but cannot satisfy completion.
- Write `pr-remediation-llxprt-run.json` for every invocation with bounded stdout/stderr metadata, full log paths when captured, exit status/signal when available, timeout/spawn classification, and artifact binding fields.
- For timeout, spawn failure, fatal llxprt failure, retryable llxprt failure, and success-without-result, write `pr-remediation-llxprt-run.json` plus any validator-readable failure/result artifact through `PrFollowupArtifactStore` using semantic states `timeout`, `spawn_failed`, `fatal`, `retryable_failed`, or `success_without_result`. Return wrapper `success` when that artifact should be classified by `validate_remediation_result`; return wrapper `fatal` only when no validator-readable artifact exists or terminal logging must run. Do not encode artifact-readability branching in TOML.

Fixture assertions must prove the rendered TOML prompt contains the exact required contract, references `pr-remediation-plan.json`, forbids fixing `mark_invalid`, requires one result per CI failure/valid feedback item, requires input/output head SHA fields, requires the canonical status enum (`fixed | changed | already_satisfied | not_reproduced | not_fixed | skipped | failed`), requires structured status/action/evidence fields, requires deterministic evidence for `already_satisfied`/`not_reproduced`, and rejects free-form-only completion.

#### `run_post_pr_tests` contract

`run_post_pr_tests` is a dedicated deterministic executor step type, registered in `ExecutorRegistry::with_defaults()` with the exact step type `run_post_pr_tests`; the workflow step ID is also `run_post_pr_tests`. It must not be generated by the LLM and must not be selected from free-form remediation output. It writes `post-pr-test-result.json` with binding fields plus `commands`; each command records argv or configured command ID, status (`passed`, `failed`, or `fatal`), exit code when available, bounded `stdout`, bounded `stderr`, timestamps from the injectable clock where applicable, and artifact paths for full logs when captured. Only semantic `test_state=passed` may return StepOutcome `success` and reach `push_remediation_changes`. Test failures attributable to current remediation return `fixable` while the artifact-backed verification retry cap remains; retry exhaustion and infrastructure/configuration errors return `fatal`. Do not reuse the issue-specific `run_tests` step for post-PR verification in this plan. Existing `run_tests` remains pre-PR only unless a separate future change explicitly generalizes it with the full `post-pr-test-result.json` artifact contract.

#### `PushRemediationChangesExecutor` commit/stage/push contract

`PushRemediationChangesExecutor` owns deterministic commit creation before push. It must not push an uncommitted working tree or rely on prior llxprt commits. Exact semantics:

1. Read and validate current `pr.json`, `pr-remediation-plan.json`, `pr-remediation-result.json`, and `post-pr-test-result.json`; require local verification passed.
2. Record `pre_push_local_head_sha` from `git rev-parse HEAD`, `pre_push_remote_head_sha` from `git ls-remote` or equivalent current PR head lookup, and `pre_push_pr_head_sha` from `pr.json.head_sha`.
3. Inspect the working tree with porcelain status. Treat no tracked or untracked changes after deterministic exclusions as semantic `no_change`; write `push-remediation-result.json` with `push_state=no_change`, `expected_head_sha=pre_push_local_head_sha`, and return `success` only if the remote PR head is already at that expected head and the current remediation plan has no `must_fix` entries or the binding-valid remediation result contains validator-accepted evidence for every `must_fix` item. Prefer preventing this path in `validate_remediation_result`; this push check is a defensive invariant so a no-change push cannot mask `not_fixed`, `skipped`, `failed`, or explanation-only remediation.
4. Apply deterministic staging exclusions matching the existing workflow exclusions exactly. The exclusion set must be config-driven from the same values used by the issue-fix workflow (for example project memory/state, artifact directories, temporary logs, and other current workflow exclusions); tests must assert the push executor and workflow exclusion lists stay equal. Never stage `.llxprt/` unless the workflow already explicitly permits it.
5. Stage only included paths using safe argv-based git commands. If only excluded changes exist, classify as `no_change_excluded_only`, write excluded path evidence, and verify remote is already at the expected head before returning `success`.
6. Create a commit with deterministic message: `Apply PR follow-through remediation for PR #<pr_number>` plus an optional stable body containing run ID, input head SHA, plan artifact sequence, and remediation result artifact sequence. The commit message must not include untrusted CodeRabbit text.
7. Record `committed_head_sha` after commit. Push the current branch/head ref with safe argv-based git commands.
8. After push, verify the remote PR head is updated to `committed_head_sha` or was already at `committed_head_sha`. Record `post_push_remote_head_sha`, `post_push_local_head_sha`, and `verified_remote_matches_expected=true` before returning `success` with `push_state=pushed`.
9. Classify outcomes as: `no_change`/`no_change_excluded_only` (success only when remote already matches expected local head), `pushed` (success after verified remote update), `retryable_failed` (only for configured transient git/network failures under retry cap), `retry_exhausted`, or `fatal` (staging/commit verification failure, non-deterministic status, unsafe path, remote head mismatch after push, artifact failure).

Required tests must prove successful remediation with a modified working tree stages only allowed paths, creates exactly one deterministic commit, pushes it, records pre/local/remote and committed head SHAs, verifies remote PR head, and only then routes to `capture_pr_identity`. Tests must also cover no-change, excluded-only changes, remote already at expected head, remote mismatch fatal, retryable push with cap, and commit-message shell safety.

`post_pr_failure_terminal` input contract: every post-PR executor that returns `fatal`, `retryable` routed to terminal, or any bounded non-success route must first write a current-run artifact containing `producer_step_id`, semantic state, failure reason, binding fields, `artifact_sequence`, `failure_sequence`, `produced_at` from the injectable clock, and configured `step_order_index`. The terminal executor must not infer failure ordering from filesystem mtimes. It reads the immutable history/snapshot artifacts for the same `run_id`/PR/head, filters to non-success records, and sorts deterministically by `(failure_sequence, artifact_sequence, produced_at, step_order_index, producer_step_id, path)` to select and audit the source failure. If no such artifact exists, it records `failed_step = unknown_current_context_only` and returns fatal. Tests must prove the terminal records the actual failing upstream step for check timeout, CodeRabbit timeout, malformed evaluation, remediation validator fatal, local verification infrastructure fatal, push failure, and marker fatal cases.

#### Pending marker action carry-forward contract

A remediation push can remove CodeRabbit feedback from the current head before `mark_coderabbit_feedback` runs. That does not make already-required comments/resolution actions optional. The workflow must persist marker obligations across remediation pushes/head changes and carry them forward until the marker executor records completion, an explicit policy skip, or fatal/user-judgment state.

The canonical state artifact is `pending-feedback-marker-actions.json` (canonical plus history snapshots through `PrFollowupArtifactStore`). An equivalent section inside an existing canonical state/result artifact is allowed only if it preserves the exact fields and history semantics below and Phase 02 documents the mapping. Each pending action record must include at minimum:

- Original feedback identity: original item ID when present, stable marker key, source surface (`review_thread`, `review_comment`, `issue_comment`, or check summary), source node/comment/thread IDs when available, original body hash, original URL/path/line, original author, and original source head SHA.
- Remediation binding: remediation plan artifact sequence, remediation result artifact sequence, remediation input head SHA, remediation output head SHA, run ID, repository owner/name, PR number, head ref, and base ref/SHA when available.
- Required action/evidence: action kind (`fixed_comment`, `invalid_comment`, `out_of_scope_comment`, optional escalation, `resolve_thread`), deterministic comment template key, action-taken or evaluation reason, evidence paths/URLs from remediation output, and the policy row that made the action required or skipped.
- Marker status: `pending`, `comment_posted_resolution_pending`, `handled_comment_only`, `resolved`, `skipped_by_policy`, `unhandled_needs_user_judgment`, `failed_retryable`, or `failed_fatal`; posted comment IDs/URLs/body hashes, resolved thread IDs, skipped reasons, failure metadata, and timestamps.
- Idempotency keys: separate stable idempotency keys for comment creation and thread resolution. Keys must include action kind, stable marker key, original body hash, `source_head`, `remediation_output_head` when the action claims a fix (or explicit `none` for invalid/out-of-scope/needs-user-judgment actions), deterministic template key/body hash, action body hash, run ID, and repository/PR binding fields needed to avoid collisions. A single ambiguous `head` field is forbidden for carried-forward marker actions.

`build_remediation_plan`/`validate_remediation_result` must create or update pending marker actions for every valid fixed item and every invalid/out-of-scope item that policy requires marking, before any push can make the item disappear from current clean feedback. `mark_coderabbit_feedback` must consume both the current clean `coderabbit-feedback.json`/`feedback-evaluations.json` and pending marker actions from prior remediation history. Current feedback/evaluation data may add new actions or refresh evidence, but it must not erase pending actions solely because the current head is clean or feedback collection returns no matching item. The marker report must record which actions came from current feedback versus carried-forward pending history.

Tests must cover the head-change carry-forward path: head A has CodeRabbit feedback; remediation fixes it and writes pending marker actions with `source_head=A`, `remediation_output_head=B`, body hash, action kind, and run ID; `push_remediation_changes` moves the PR to head B; current CodeRabbit feedback on head B is ready/clean/empty; `mark_coderabbit_feedback` still posts the deterministic fixed comment and resolves the original item/thread using the original item identity and remediation evidence. Retrying the marker step after a partial or completed action must use the local pending state plus remote hidden markers to avoid duplicate comments and duplicate resolution attempts. Additional tests must cover invalid/out-of-scope actions with `remediation_output_head=none`, remote-marker-only resume after a head change, and the same stable item appearing on a later head without colliding with the carried-forward action from the earlier source head.


#### Global artifact persistence contract

Concrete v1 artifact root/path contract: every post-PR executor step must receive a required TOML parameter named `artifact_root`. `artifact_dir` is not accepted as an alias in v1. `artifact_root` must be a non-empty canonical path after engine-supported variable expansion and path normalization; relative values are resolved exactly as existing workflow artifact paths are resolved by Phase 0.5, then canonicalized before constructing `PrFollowupArtifactStore`. Missing, empty, unexpandable, non-canonicalizable, or conflicting artifact-root params are configuration fatal paths: the executor writes a best-effort config/fatal artifact only if a safe root is available from context, otherwise returns the existing engine configuration error path. Canonical current artifacts live directly under `<artifact_root>/pr-followup/current/<run_id>/<repository_owner>/<repository_name>/<pr_number>/`, and immutable history snapshots live under `<artifact_root>/pr-followup/history/<run_id>/<repository_owner>/<repository_name>/<pr_number>/<artifact_family>/<artifact_sequence>-<write_sequence>-<producer_step_id>.json`. Executors must initialize `PrFollowupArtifactStore` from `artifact_root` only; they must not infer roots from individual result paths, working directory, temp directory, or predecessor artifacts. P16/P17 graph tests must assert every post-PR TOML step has exactly one `artifact_root` param, no post-PR step uses `artifact_dir`, every configured artifact/result path is inside the canonical root, and executor unit tests fail if a store is initialized from any source other than `artifact_root`.


Dedicated implementation ownership: Phase 05 is the single artifact-store implementation phase and must be completed before any Phase 06+ PR follow-through executor writes artifacts. `PrFollowupArtifactStore` owns canonical/current files, immutable history snapshots, sequence allocation, atomic writes, canonical/history recovery, binding validation helpers, `ArtifactWriter` behavior, and failure-sequence allocation. Executor-specific modules must not implement independent PR follow-through artifact file writing; they call the store. No later phase may re-own or duplicate `PrFollowupArtifactStore`/`ArtifactWriter` behavior; later phases may only consume or extend typed artifact-family adapters through the store's public API.


Every PR follow-through artifact family must have both: (1) a canonical current file with the stable filename consumed by downstream steps, for example `pr.json` or `feedback-evaluations.json`; and (2) immutable history/snapshot files under a deterministic history directory or name pattern containing the same payload plus sequence metadata. Writers must write JSON to a temporary file in the same filesystem directory, fsync when supported by existing project conventions, and atomically rename it into place for both the history snapshot and canonical current file. Each write must include monotonic `artifact_sequence` and `write_sequence`; failure-producing artifacts also include `failure_sequence`.

Sequence source of truth: on normal execution, `PrFollowupArtifactStore` allocates sequences by scanning accepted same-run history snapshots under the artifact root and taking the next integer after the maximum accepted sequence. If scanning is too expensive or a sidecar is added, the sidecar is only a cache: resume must validate it against history and repair it from history when inconsistent. `artifact_sequence` is global across all PR follow-through artifact families for the same `run_id`; `write_sequence` is per artifact family and increments for every atomic write of that family; `failure_sequence` is global across all failure-producing PR follow-through artifacts for the same `run_id`. History snapshots must record `artifact_family` so per-family `write_sequence` can be validated separately from global `artifact_sequence`. Malformed, duplicate, decreasing, or unbound sequence artifacts are not silently skipped when they are in the current run/PR scope: current consumers reject them as fatal if they affect the family being read, and terminal/audit readers record them as `unreadable_or_unbindable_sequence_artifacts` before routing fatal. Stale older-run or mismatched PR/head artifacts may be ignored only after binding validation records them in ignored/stale lists. Resume tests must cover allocating after canonical-only loss, allocating after history-only recovery, sidecar lower/higher than history, duplicate sequence numbers, malformed history JSON, mismatched binding fields, and preserving global/per-family semantics across process restart.

Current consumers read only the canonical current file for their direct predecessor inputs and must validate binding fields before use. Terminal/audit readers read history snapshots with deterministic ordering and never use filesystem mtimes for semantics. `post_pr_iteration_guard` reads only guard history snapshots plus the canonical current `pr.json`; it must not count unrelated artifact history.

#### Post-PR step ordering source

This plan chooses the TOML-configured ordering approach under current engine constraints. Do not require a `StepContext` engine change for step ordering. Every post-PR workflow step in Phase 17 must define a unique integer `step_order_index` parameter in workflow TOML, beginning at the first post-PR step and increasing monotonically along the primary route. Loop-back steps keep their own fixed configured index; artifact ordering for repeated executions is determined by sequence fields, not by changing step indexes.

Every post-PR executor must read its configured `step_order_index` from step parameters, validate that it is present, and copy it into every canonical and history artifact it writes. Missing, non-integer, duplicate, or non-monotonic configured indexes are workflow/schema errors covered by graph tests. Phase 16 graph tests must enumerate the post-PR step IDs and assert each has exactly one `step_order_index`, indexes are unique, indexes increase along the primary `capture_pr_identity -> ... -> post_pr_failure_terminal` order, and no post-PR artifact contract relies on filesystem mtimes or engine execution order.

#### Post-PR executor error policy

Expected deterministic failures in post-PR executors are data outcomes, not engine errors. API/auth/schema/command failures, timeout budgets, malformed input artifacts, ambiguous current-head binding, missing required upstream artifacts, needs-user-judgment states, iteration-cap exhaustion, local verification infrastructure failures, and marker API failures must write the best available failure-producing artifact through the shared artifact writer and return `Ok(StepOutcome::Fatal)` so workflow routing reaches `post_pr_failure_terminal`.

`Err(EngineError)` is reserved for unrecoverable programming/context errors that prevent the executor from producing even a best-effort artifact, such as invalid executor construction, unavailable artifact root/context, serializer bugs, writer atomic-rename failure after all fallbacks, or impossible invariant violations. Tests must simulate API failures, schema failures, and command failures and prove those route through `post_pr_failure_terminal` with source failure artifacts rather than bubbling as `Err(EngineError)`.
#### Validation enforcement locations

Validation ownership is split deliberately:

- Production executor config validation: each executor validates its own required params at runtime before side effects where possible, writes a fatal/config artifact through `PrFollowupArtifactStore`, and returns `StepOutcome::Fatal` when the artifact can be written. This includes missing/invalid post-PR executor params, missing own artifact paths, missing/invalid own `step_order_index`, malformed own retry caps, unsafe command argv, and unsupported own configuration. Optional `config_loader` schema improvements may catch these earlier, but executor fatal/config artifacts remain required for defense in depth.
- Workflow graph/TOML invariant tests: P16/P17 graph tests enforce cross-step invariants that require the whole workflow graph: unique and monotonic post-PR `step_order_index`, no unresolved placeholders in required post-PR params, every post-PR loop-back transition has explicit high defensive `max_iterations`, no duplicate outcome branches, no custom routable semantic outcomes, no post-PR route to `abandon_and_log`, `generate_pr_description`, or `create_pr`, and `post_pr_failure_terminal` has no outgoing transitions.
- Optional config loader changes: allowed only to improve early diagnostics for missing required params/placeholders/duplicate step ordering; they must not replace executor config artifacts or P16/P17 graph assertions.

Required post-PR params are therefore enforced twice where practical: own-step param shape in production executors, and whole-graph consistency in P16/P17 tests. Missing/invalid own params must be executor fatal/config artifacts whenever artifact writing is possible; graph invariants are never left to live execution discovery.

Exact graph cut assertion: tests must compute the set reachable from `capture_pr_identity` and assert that no reachable path reaches `abandon_and_log`, `generate_pr_description`, or `create_pr`; every fatal or retryable route from a post-PR step targets `post_pr_failure_terminal`; and `post_pr_failure_terminal` has no outgoing transitions.




#### PR identity capture after `create_pr`

`create_pr` must not pass free-form PR text as the source of truth. `capture_pr_identity` must obtain identity by deterministic GitHub data after PR creation, in this fallback order: (1) parse a PR URL/number artifact emitted by `create_pr` if available and verify it with `gh pr view`; (2) query `gh pr view --json number,url,headRefName,headRefOid,baseRefName,baseRefOid,state,isDraft` for the current branch; (3) use REST/GraphQL lookup by head owner/repo/ref. The artifact is accepted only when PR number, URL, head ref, head SHA, and base ref are all present and current.

### Existing integration points

- `src/engine/executor.rs`: `StepExecutor`, `StepContext`, `ExecutorRegistry::with_defaults()`
- `src/engine/executors/mod.rs`: executor modules and test re-exports
- `src/engine/executors/llxprt.rs`: existing LLM command support and artifact writing patterns
- `src/engine/executors/shell.rs`: existing shell command patterns and exit-code mapping
- `src/engine/transition.rs`: `StepOutcome` values and transition matching
- `src/engine/runner.rs`: loop handling and terminal `StepOutcome::Abandon` behavior
- `config/workflows/llxprt-issue-fix-v1.toml`: workflow tail currently routes `create_pr -> log_completion`
- `tests/e2e_workflow_integration.rs`, `tests/smoke_test.rs`, `tests/engine_integration_llxprt_first.rs`: existing integration and workflow graph tests

## Execution Tracker

Create and maintain `project-plans/coderabbit/execution-tracker.md` at implementation time.

| Phase | ID | Status | Started | Completed | Verified | Semantic? | Notes |
|-------|----|--------|---------|-----------|----------|-----------|-------|
| 0.5 | P0.5 | [ ] | - | - | - | N/A | Preflight verification |
| 01 | P01 | [ ] | - | - | - | [ ] | Domain model and schemas |
| 02 | P02 | [ ] | - | - | - | [ ] | Pseudocode |
| 03 | P03 | [ ] | - | - | - | [ ] | Engine stubs |
| 04 | P04 | [ ] | - | - | - | [ ] | Engine integration TDD |
| 05 | P05 | [ ] | - | - | - | [ ] | Dedicated artifact store implementation |
| 06 | P06 | [ ] | - | - | - | [ ] | PR identity/check watch implementation |
| 07 | P07 | [ ] | - | - | - | [ ] | CI failure collection implementation |
| 08 | P08 | [ ] | - | - | - | [ ] | CodeRabbit feedback implementation |
| 09 | P09 | [ ] | - | - | - | [ ] | Feedback evaluation implementation |
| 10 | P10 | [ ] | - | - | - | [ ] | Remediation plan aggregation implementation |
| 11 | P11 | [ ] | - | - | - | [ ] | Remediation result validator and caps implementation |
| 12 | P12 | [ ] | - | - | - | [ ] | PR follow-up llxprt remediation wrapper implementation |
| 13 | P13 | [ ] | - | - | - | [ ] | Post-PR local verification executor implementation |
| 14 | P14 | [ ] | - | - | - | [ ] | Remediation change push executor implementation |
| 15 | P15 | [ ] | - | - | - | [ ] | Feedback marker implementation |
| 16 | P16 | [ ] | - | - | - | [ ] | Workflow integration TDD |
| 17 | P17 | [ ] | - | - | - | [ ] | Workflow TOML/fixture implementation |
| 18 | P18 | [ ] | - | - | - | [ ] | Post-implementation hardening only |
| 19 | P19 | [ ] | - | - | - | [ ] | Security/idempotency hardening |
| 20 | P20 | [ ] | - | - | - | [ ] | Documentation and plan-marker audit |
| 21 | P21 | [ ] | - | - | - | [ ] | Final verification |


---

# Phase 0.5: Preflight Verification

## Phase ID

`PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P0.5`

## Purpose

Verify all assumptions before implementation.

## Requirements Implemented (Expanded)

### REQ-PRFU-019: No-Network Default Tests

**Full Text**: The system shall provide deterministic tests that do not require live GitHub network access by default. Tests shall use fixture GitHub JSON payloads or fake `gh` executables. Tests shall cover check classification, stale head handling, CodeRabbit normalization, evaluation validation, remediation aggregation, and marker idempotency.

**Behavior**:

- GIVEN: PR follow-through code is under test
- WHEN: the test suite runs without GitHub network access
- THEN: deterministic fixtures or fake `gh` commands exercise the behavior

**Why This Matters**: The implementation cannot depend on live GitHub state to prove correctness.

## Implementation Tasks

### Files to Create

- `project-plans/coderabbit/execution-tracker.md`
  - Tracks implementation progress.
- `project-plans/coderabbit/analysis/preflight-verification.md`
  - Records dependency, type/interface, call-path, API, and test-infrastructure verification.
- `project-plans/coderabbit/analysis/final-dry-run-command.json`
  - Records the final non-live dry-run command as an argv array/object that can be executed without shell parsing in final Phase 21.
- `project-plans/coderabbit/analysis/llxprt-remediation-seam.md`
  - Records the exact post-PR llxprt/remediation seam. This plan chooses an explicitly owned `PrFollowupRemediationExecutor` invocation implementation that may reuse helper code from `llxprt.rs`, but owns PR follow-through artifacts, process evidence, changed-path evidence, and routing.
- `project-plans/coderabbit/analysis/expected-failing-tests.json`
  - Machine-readable manifest of intentionally failing future-phase tests. Each entry must include the exact test binary, exact concrete test name or explicitly enumerated test names for any filter, owner phase, requirement ID, introduced/removal phases, expected failure mode/assertion, artifact or fixture involved, and required failure group. Allowed groups are exactly: `graph/fake E2E`, `artifact store`, `GitHub API`, `evaluator`, `remediation validator`, `marker/idempotency`, and `shell safety`. The manifest validator must reject vague entries: missing group, group outside the allowed set, owner phase that does not match the phase owning the test, missing exact test name(s), broad filters without enumerated names, vague expected failure text such as `fails`, `not implemented`, `TBD`, or `future`, and entries without a concrete assertion substring/error matcher.


- `project-plans/coderabbit/.completed/P0.5`
  - Completion marker written only after preflight findings are recorded. Later analysis-only phases use completion markers as prerequisites instead of implementation code-marker greps.


### Files to Inspect

- `Cargo.toml`
  - Verify `serde`, `serde_json`, `chrono`, `tokio`, and error-handling dependencies.
- `src/engine/executor.rs`
  - Verify executor registration and `StepContext` interfaces.
- `src/engine/transition.rs`
  - Verify `StepOutcome` variants and string names.
- `src/engine/runner.rs`
  - Verify terminal `StepOutcome::Abandon` behavior and loop-cap semantics.
- `src/engine/executors/llxprt.rs`
  - Verify artifact writing and command invocation patterns.
- `config/workflows/llxprt-issue-fix-v1.toml`
  - Verify current PR tail and transitions.
- `tests/e2e_workflow_integration.rs`, `tests/smoke_test.rs`, `tests/engine_integration_llxprt_first.rs`
  - Verify test extension points.

## Verification Commands

```bash
cargo tree -p serde_json
cargo tree -p chrono
grep -R "pub trait StepExecutor" src/engine/executor.rs
grep -R "pub enum StepOutcome" src/engine/transition.rs
test -f project-plans/coderabbit/.completed/P0.5

grep -R "StepOutcome::Abandon" src/engine/runner.rs
grep -n "from = \"create_pr\"" config/workflows/llxprt-issue-fix-v1.toml
cargo test --test e2e_workflow_integration -- --list
```

## Expanded Preflight Checklist

Record all findings in `project-plans/coderabbit/analysis/preflight-verification.md` before P01:

| Area | Exact verification required | Blocking if missing? |
|------|-----------------------------|----------------------|
| Current workflow schema | Confirm transition syntax, step outcome names, `max_iterations` semantics, and whether multiple transitions from one outcome are rejected or ordered. | Yes, routing contract must match actual schema. |
| Current `llxprt` capabilities | Confirm artifact input/output support, `success_file` behavior, failure outcome names, prompt templating, and whether result files can be required without source changes outside planned phases. Record the exact post-PR remediation seam in `llxprt-remediation-seam.md`: this plan owns a separate `PrFollowupRemediationExecutor` invocation implementation that may reuse argv construction/process-runner helpers from `llxprt.rs`, but must own PR follow-up artifacts and routing. | Yes, remediation enforcement depends on this. |
| Post-PR llxprt invocation evidence contract | Confirm the invocation implementation can return or persist all fields required before routing: argv, exit status or signal, timeout/spawn class, bounded stdout/stderr, full stdout/stderr/log paths, success-file presence, changed-path evidence, and enough context to write `pr-remediation-llxprt-run.json` plus validator-readable failure artifacts. If existing `llxprt.rs` helpers cannot expose those fields, Phase 12 must implement the missing owned runner wrapper instead of depending on opaque llxprt results. | Yes for P12. |
| Fixture regeneration | Identify the canonical TOML-to-JSON fixture regeneration command and confirm generated fixture diffs are deterministic. Record the source-of-truth paths exactly: production TOML `config/workflows/llxprt-issue-fix-v1.toml`, fixture TOML `tests/fixtures/workflows/valid/llxprt-issue-fix-v1.toml`, and fixture JSON `tests/fixtures/workflows/valid/llxprt-issue-fix-v1.json`. Record the exact regeneration command as argv in `project-plans/coderabbit/analysis/fixture-regeneration-command.json`; if the current canonical command remains `python3 junk/regen_fixtures.py`, record it there rather than relying on prose. | Yes for P17. |
| `gh` JSON capabilities | Run live `gh` inspection only as reconnaissance to populate `github-api-contract.md` and checked-in fixtures. Implementation must consume pinned fixture-backed fields or fake `gh` outputs only. If the local installed `gh` output differs from the contract/fixtures, block implementation and update the contract plus fixtures before code consumes the new shape. Record installed `gh` version used for reconnaissance. | Yes for API contract. |
| GitHub permissions and auth scopes | Record installed `gh` version, authenticated account/token type, and required scopes/permissions for GraphQL review-thread reads, `resolveReviewThread`, comment creation, paginated comments/reviews, pushing to the PR branch, and Actions jobs/log downloads. Define permission-denied behavior as artifact-backed fatal/non-success with `permission_denied`, `operation`, `required_scope_or_permission`, `account_login`, `token_type`, and safe command/query metadata fields. Add fixture-backed auth-failure cases for each GitHub surface before implementation consumes it. | Yes for P02/P04 and all GitHub executors. |


| Mock registry setup | Confirm existing test support for fake executors, fake command runners, temporary artifact directories, and no-network test defaults. | Yes for P04/P16/P18. |
| Clock/sleeper abstraction | Confirm whether an injectable clock/sleeper already exists; if absent, plan a minimal test-only/injectable abstraction so one-hour watch tests execute without real sleeps. | Yes for P04/P06. |
| Command-runner injection seam | Confirm or design the trait/seam used by GitHub executors, marker actions, and post-PR tests so tests can use fake `gh`/shell runners without network or real pushes. | Yes for P03/P04. |
| Artifact root and path convention | Confirm `StepContext` exposes or can derive an artifact root; document exact canonical/current and history/snapshot paths relative to that root, including temp-file placement. | Yes for P01/P03. |
| Feedback evaluator LLM adapter | Confirm existing LLM invocation support can send one `FeedbackEvaluationRequest` per item and capture raw JSON response; otherwise design an adapter compatible with existing llxprt/command patterns. | Yes for P04/P09. |
| TOML nested params | Confirm workflow schema supports new nested params for PR follow-through executors, including post-PR test commands, budgets, bot identities, and artifact options; if not, update schema tests before workflow implementation. | Yes for P16/P17. |
| Duplicate transition ambiguity | Confirm parser/validator behavior for duplicate transitions from the same step/outcome. For this feature, add workflow-specific graph tests for `llxprt-issue-fix-v1` that fail on duplicate outcome branches in the post-PR tail; do not promise global workflow-engine duplicate-transition validation unless a separate global validation phase is explicitly added. | Yes for P04/P16. |
| GitHub API field inventory | Produce preliminary inventory of every GitHub/`gh` JSON field and JSON path needed by schema field tables before P01 finalizes schema names that depend on external API payloads. | Yes for P01/P02. |
| Final dry-run CLI syntax | Record the canonical dry-run command syntax for this repository and write it in argv-safe form to `project-plans/coderabbit/analysis/final-dry-run-command.json` (or an equivalent newline-delimited argv file with no shell parsing). Final verification must execute the recorded command file rather than an assumed invocation or commented placeholder. | Yes for P21. |
| Pending marker carry-forward | Confirm the artifact/state location for `pending-feedback-marker-actions.json` or its exact equivalent section, including history snapshot rules and how marker actions survive remediation pushes/head changes. | Yes for P04/P09/P10/P16. |
| Expected failing test manifest | Initialize `project-plans/coderabbit/analysis/expected-failing-tests.json`. P04/P16 TDD phases add intentional future-phase failures; every implementation phase removes entries it makes pass and proves remaining failures match the manifest. | Yes for P04 through P21. |

| StepOutcome routing | Confirm `StepOutcome::Abandon` is terminal/non-routable in the current runner; all post-PR bounded non-success routes must use representable outcomes and end at `post_pr_failure_terminal`, which returns `fatal` after logging. | Yes for P16/P17. |

- `project-plans/coderabbit/.completed/P0.5` exists only after the documented assumptions are complete.


## Success Criteria

- All assumptions are documented.
- Any blocking mismatch is reflected in a plan update before implementation.
- The plan explicitly avoids requiring routable `StepOutcome::Abandon`; post-PR non-success routes end through `post_pr_failure_terminal` returning `fatal`.

- Preflight confirms the exact workflow routing contract can be represented without ambiguous branches.
- Preflight records exact production/fixture workflow paths, the argv-safe fixture regeneration command, and the normalized production-vs-fixture comparison test that P17/P21 must run.
- Preflight records the exact `PrFollowupRemediationExecutor` llxprt/remediation seam and confirms the required process evidence fields can be captured before routing.
- Live `gh` output is treated only as reconnaissance; implementation is blocked unless all consumed fields are pinned in `github-api-contract.md` and checked-in fixtures/fake `gh` outputs.
- Preflight records `gh` version, account/token type, required GitHub scopes/permissions, and artifact fields plus fixture-backed behavior for permission-denied failures across GraphQL, REST, comments, review threads, pushing, and Actions logs.

- The expected-failing test manifest exists and is ready for P04/P16 TDD ownership.


## Failure Recovery

If preflight finds incompatible assumptions, update this plan before starting P01.

---

# Phase 01: Domain Model and Artifact Schemas

## Phase ID

`PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P01`

## Prerequisites

- Required: Phase 0.5 completed
- Verification: `test -f project-plans/coderabbit/.completed/P0.5 && test -f project-plans/coderabbit/analysis/preflight-verification.md`

- Hard gate: preliminary GitHub API field inventory exists in `project-plans/coderabbit/analysis/preflight-verification.md` or `project-plans/coderabbit/analysis/github-api-field-inventory.md` before schema field tables are finalized.


## Requirements Implemented (Expanded)

### REQ-PRFU-001: Deterministic PR Identity Capture

**Full Text**: The system shall produce a deterministic PR identity artifact before any PR follow-through step runs. The artifact shall include repository owner/name, PR number, PR URL, head ref, head SHA, base ref, base SHA when available, and capture timestamp. The artifact shall be valid JSON and include `schema_version`. Every downstream PR follow-through artifact schema shall repeat the binding fields `run_id`, `repository_owner`, `repository_name`, `pr_number`, `head_ref`, `head_sha`, `base_ref`, and `base_sha` where available, and consumers shall reject missing or mismatched binding fields before using the artifact.

**Behavior**:

- GIVEN: a PR exists
- WHEN: PR follow-through begins
- THEN: PR identity is captured as structured JSON and downstream artifacts bind to it

**Why This Matters**: The workflow must not use stale or ambiguous PR state.

### REQ-PRFU-002: Current-Head Binding

**Full Text**: The system shall bind PR checks, CI failures, CodeRabbit feedback, feedback evaluations, remediation plans, remediation results, and marker reports to the current PR head SHA. The system shall not treat stale artifacts or stale check runs from older head SHAs as evidence that the current PR head is clean. The system shall report stale data in structured artifacts for auditability.

**Behavior**:

- GIVEN: a PR head changes after remediation
- WHEN: prior artifacts are read
- THEN: stale artifacts are ignored for current success and recorded as stale

**Why This Matters**: CI and review comments are only meaningful for the commit they apply to.

### REQ-PRFU-003: Structured Artifact Contract

**Full Text**: The system shall write machine-readable JSON artifacts for PR identity, check status, CI failures, CodeRabbit feedback, feedback state, feedback evaluations, remediation plans, remediation results, and marker actions. Each JSON artifact shall include `schema_version`. Each artifact shall include enough identifiers to correlate records across workflow iterations. Raw logs may be written as non-JSON files, but their metadata shall be represented in JSON.
### Common metadata required on every PR follow-through artifact family

Every artifact family in Phase 01 tables must include the common persistence metadata from the global sequence/failure contracts:

| Field | Type | Required | Notes |
|-------|------|----------|-------|
| `artifact_sequence` | integer | yes | Monotonic global sequence allocated by `PrFollowupArtifactStore` across all PR follow-through artifact writes for the same `run_id`, regardless of artifact family. |
| `write_sequence` | integer | yes | Monotonic per-artifact-family sequence for each atomic write of that family; included in canonical and history payloads. |
| `history_metadata` | object | yes | Contains at minimum `canonical_path`, `history_path`, `artifact_family`, `is_canonical`, and `history_written_at` from the injectable clock. |
| `step_order_index` | integer | yes | Copied from TOML step configuration for deterministic terminal ordering. |

Every failure-producing artifact family must additionally include the failure metadata below whenever its semantic state is non-success/fatal/fixable/retryable or it is eligible as terminal source evidence:

| Field | Type | Required | Notes |
|-------|------|----------|-------|
| `producer_step_id` | string | yes | Workflow step ID that produced the failure evidence. |
| `semantic_state` | string | yes | Artifact-specific semantic state, such as `pending_timeout`, `fatal`, `blocked_needs_user_judgment`, or `failed`. |
| `failure_reason` | string | yes | Deterministic reason suitable for terminal audit. |
| `failure_sequence` | integer | yes | Monotonic global sequence across all failure-producing PR follow-through artifacts for the same `run_id`; terminal sorting uses this before timestamps. |
| `produced_at` | string | yes | RFC3339 timestamp from the injectable clock. |
| `step_order_index` | integer | yes | Repeated here because terminal ordering requires it for failure-producing records. |

P01 fixture tables and examples must show these fields for all artifact families; P04 schema rejection tests must fail when any required common field or failure field is missing.



**Behavior**:

- GIVEN: any deterministic PR follow-through step runs
- WHEN: it produces output
- THEN: the output is a schema-versioned JSON artifact

**Why This Matters**: Later steps must consume facts deterministically.

## Required Artifact Schema Field Tables

Phase 01 is compile-safe analysis/fixtures/schema-contract TDD only, and its schema tables are provisional until Phase 02 validates them against the exact GitHub API contract: it must define artifact field tables, schema-contract documentation, and JSON fixtures. It must not add Rust tests that import missing production structs, and it must not add production Rust schema structs under `src/`. Schema checks in this phase are limited to fixture/document validation that can compile without production types, for example JSON validity, required-field presence in fixtures, and contract-document consistency. Production schema stubs move to Phase 03; behavioral Rust schema tests that import those stubs move to Phase 04 and must compile then fail behaviorally against incomplete validation. Field names are not final until Phase 02 maps each field to an API-contract source or marks it internally produced; Phase 02 must update `artifact-schema-contract.md` or fail completion if any field remains unvalidated, TBD, or provisional.

### `pr.json`

| Field | Type | Required | Notes |
|-------|------|----------|-------|
| `schema_version` | integer | yes | Initial value `1`; all PR follow-through artifacts use integer schema versions consistently. |
| `run_id` | string | yes | Workflow/run correlation ID. |
| `repository_owner` | string | yes | GitHub owner. |
| `repository_name` | string | yes | GitHub repo. |
| `pr_number` | integer | yes | Captured after `create_pr`. |
| `pr_url` | string | yes | Canonical HTML URL. |
| `head_ref` | string | yes | PR head branch/ref. |
| `head_sha` | string | yes | Current head commit SHA. |
| `base_ref` | string | yes | Base branch. |
| `base_sha` | string | no | Include when available. |
| `captured_at` | string | yes | RFC3339 timestamp from injectable clock. |
| `source` | string | yes | `create_pr_artifact`, `gh_pr_view`, `graphql`, or `rest`. |

### `pr-check-status.json`

| Field | Type | Required | Notes |
|-------|------|----------|-------|
| `schema_version` | integer | yes | Same integer schema version convention as `pr.json`. |
| `run_id`, `repository_owner`, `repository_name`, `pr_number`, `head_ref`, `head_sha`, `base_ref`, `base_sha` | mixed | yes except `base_sha` when unavailable | Binding fields copied from `pr.json`; consumers reject missing or mismatched required binding fields. |
| `poll_attempts` | integer | yes | Number of completed polls. |
| `max_attempts` | integer | yes | Default 12. |
| `poll_interval_seconds` | integer | yes | Default 300. |
| `max_duration_seconds` | integer | yes | Default 3600. |
| `overall_state` | string | yes | `passed`, `failed`, `pending_timeout`, `unknown`, or `fatal`. |
| `checks` | array | yes | Current-head check records. |
| `stale_checks` | array | yes | Older-head check records ignored for success. |
| `observed_at` | string | yes | RFC3339. |

Each check record must include `name`, `status`, `conclusion`, `state`, `url`, `workflow_name`, `run_id`, `job_id`, `started_at`, `completed_at`, `head_sha`, and `source`.

Watch budget semantics are observation-count based for v1: the default 12-attempt watch performs observations at t=0, t=5, ..., t=55 minutes, sleeps only between observations, and does not perform an additional t=60 observation. The one-hour duration is the enclosing budget for those 12 observations plus only the already-started final GitHub request/classification; tests and docs must consistently use this t=0..55 model.

### `ci-failures.json`

| Field | Type | Required | Notes |
|-------|------|----------|-------|
| `schema_version` | integer | yes | Same integer schema version convention as `pr.json`. |
| `run_id`, `repository_owner`, `repository_name`, `pr_number`, `head_ref`, `head_sha`, `base_ref`, `base_sha` | mixed | yes except `base_sha` when unavailable | Binding fields copied from `pr.json`; consumers reject missing or mismatched required binding fields. |
| `failures` | array | yes | Only concrete current-head terminal failed/cancelled/timed_out/action_required checks. Unknown, stale-only, schema-unbindable, and pending-after-budget records are excluded. |
| `pending_or_unknown` | array | yes | Pending-after-budget, unknown, stale-only, and schema-unbindable records that are not concrete must-fix failures. These route to needs-user-judgment/fatal artifact paths, not remediation `must_fix`. |
| `log_artifacts` | array | yes | Metadata for raw log excerpt files. |


### PR check watcher write semantics

The watcher shall use a single aggregate `pr-check-status.json` artifact containing a `poll_observations` array. After every polling cycle, including intermediate pending cycles and the final terminal/timeout cycle, it must rewrite the aggregate artifact atomically with all observations collected so far. Each `poll_observations[*]` entry must include `attempt_number`, `observed_at`, `head_sha`, `current_head_checks`, `stale_checks`, `classification`, `terminal_counts`, and `write_sequence`. Tests must use a fake artifact writer to assert one write per completed poll, monotonically increasing `write_sequence`, and that the final artifact is the last aggregate write rather than the only write.

### Deterministic CodeRabbit readiness truth table

The v1 readiness policy must be defined as an explicit truth table in `coderabbit-feedback.md` and fixture-tested. Signal inputs are:

| Signal | Required fields | Current-head predicate | Ready/stable predicate |
|--------|-----------------|------------------------|------------------------|
| Bot identity | `author_login`, `author_type`, configured bot-login list | signal author matches `coderabbitai[bot]`, `coderabbit[bot]`, or configured login | at least one current-head CodeRabbit signal observed |
| Check signal | `name`, `app.slug`, `status`, `conclusion`, `head_sha`, `completed_at` | `head_sha == pr.head_sha`; stale check signals ignored | ready only when status is terminal and conclusion is `success` or equivalent completed-neutral per documented fixture |
| Review signal | `review_id`, `state`, `author_login`, `commit_id`, `submitted_at` | `commit_id == pr.head_sha`; stale review signals ignored | ready when CodeRabbit review state is `COMMENTED`, `CHANGES_REQUESTED`, or other documented terminal review state; `PENDING`/missing is not ready |
| Comment signal | `comment_id`, `thread_id`, `author_login`, `commit_id` or `original_commit_id`, `body`, `updated_at`, `resolved`, `outdated` | current when commit binding equals `pr.head_sha` and `outdated != true`; stale/outdated ignored for readiness and item set | contributes normalized item only after ready signal exists |
| Hidden marker signal | comment body marker | current when marker contains matching `run_id`/repo/PR/head/body hash | used only for idempotency, never as readiness |

Exact hidden marker namespace is HTML comment syntax: `<!-- luther-pr-followup marker_key=<stable_marker_key> source_head=<source_head_sha> remediation_output_head=<remediation_output_head_sha_or_none> body=<body_hash> action=<action_kind> run_id=<run_id> -->`. Do not use visible `@plan` marker text in PR comments. `source_head` is the head SHA where the feedback/action obligation originated. `remediation_output_head` is the pushed remediation result head for fixed actions, or `none` for invalid/out-of-scope/needs-user-judgment actions that do not have remediation output. The hidden marker must not use a single ambiguous `head` field.

Readiness is `ready` only after two consecutive observations have: a current-head ready/completed CodeRabbit signal, the same normalized current-head feedback item set hash, and no current-head in-progress CodeRabbit signal. Any current-head feedback item addition, removal, body hash change, resolution-state change, or ready-signal-to-in-progress change resets `stable_observation_count` to 1 for the new observation hash. Stale signals are recorded in observations but ignored for current readiness. Fixture cases must cover every truth-table row, current-head vs stale-head binding, check-ready with zero comments, terminal review states, pending review/check states, exact marker parsing, feedback changes resetting stability, stale signals ignored, and non-CodeRabbit bot noise.

Failure records must include `failure_id`, `check_name`, `state`, `conclusion`, `url`, `run_id`, `job_id`, `log_status`, `log_excerpt_path`, and `collection_error`. Policy tests must prove only `failures` records can become remediation `must_fix`; `pending_or_unknown` records always become `needs_user_judgment` and route to the fatal/non-success terminal path.

### `coderabbit-feedback.json` and `coderabbit-feedback-state.json`

| Field | Type | Required | Notes |
|-------|------|----------|-------|
| `schema_version` | integer | yes | Same integer schema version convention as `pr.json`. |
| `run_id`, `repository_owner`, `repository_name`, `pr_number`, `head_ref`, `head_sha`, `base_ref`, `base_sha` | mixed | yes except `base_sha` when unavailable | Binding fields copied from `pr.json`; consumers reject missing or mismatched required binding fields. |
| `readiness_state` | string | yes | `ready`, `not_ready`, `timeout`, or `fatal`. |
| `stable_observation_count` | integer | yes | First version requires 2 identical ready observations. |
| `observations` | array | yes | Observation snapshots with timestamps and source signals. |
| `items` | array | yes | Normalized feedback items. |
| `state_entries` | array | state artifact | Prior accepted evaluation/marker state keyed by stable item key/body hash/head SHA. |

Feedback items must include `item_id`, `stable_marker_key`, `thread_id`, `comment_id`, `review_id`, `author_login`, `author_association`, `bot_identity`, `path`, `line`, `side`, `body`, `body_hash`, `url`, `created_at`, `updated_at`, `resolved`, `outdated`, `source`, and `raw_node_id`.
Observation records must include `observed_at`, `signals_seen`, `bot_identities_matched`, `observation_hash`, `budget_used`, `budget_remaining`, `items_count`, and `outcome_reason`.
Readiness observation looping is owned inside the collector executor; workflow TOML must not self-loop `collect_coderabbit_feedback`. Tests must use a fake clock/sleeper and must prove not-ready budget exhaustion writes `readiness_state=timeout` (or another non-ready semantic state) and returns StepOutcome `fatal` to `post_pr_failure_terminal`.




### `feedback-evaluations.json`

| Field | Type | Required | Notes |
|-------|------|----------|-------|
| `schema_version` | integer | yes | Same integer schema version convention as `pr.json`. |
| `run_id`, `repository_owner`, `repository_name`, `pr_number`, `head_ref`, `head_sha`, `base_ref`, `base_sha` | mixed | yes except `base_sha` when unavailable | Binding fields copied from `pr.json`; consumers reject missing or mismatched required binding fields. |
| `items_seen` | integer | yes | Must equal current input item count. |
| `accepted_results` | array | yes | Only validated LLM judgments or reused prior accepted judgments. |
| `rejected_attempts` | array | yes | Malformed/mismatched LLM outputs for audit. |
| `unevaluated_items` | array | yes | Items that did not receive an accepted result for any non-budget reason. |
| `budget_exhausted_items` | array | yes | Items whose malformed-output retry budget was exhausted. |
| `max_attempts_per_item` | integer | yes | First version: 3. |
| `evaluation_state` | string | yes | `complete`, `incomplete`, `budget_exhausted`, or `fatal`; only `complete` can return StepOutcome `success`. |

Accepted result records must include `item_id`, `stable_marker_key`, `body_hash`, `head_sha`, `decision`, `reason`, `recommended_action`, `accepted_at`, `attempt_count`, and `source` (`new` or `reused`). Decisions are only `valid`, `invalid`, `out_of_scope`, or `needs_user_judgment`; budget exhaustion is recorded only in `budget_exhausted_items`, never as an accepted decision. Remediation may consume only `accepted_results` with `decision=valid`. Reusable evaluations come only from binding-validated `coderabbit-feedback-state.json.state_entries[*].accepted_evaluation` records or the equivalent Phase 02-documented accepted-evaluation history index; raw prior `feedback-evaluations.json` output is not reusable unless it has first been persisted into that state/history source. Ambiguous, malformed, duplicate, or mismatched prior state for the same stable marker key/body hash/head SHA is fatal rather than silently ignored or re-evaluated.


### `pr-remediation-plan.json`, `pr-remediation-result.json`, `post-pr-test-result.json`, `push-remediation-result.json`, `pr-feedback-marker-report.json`, `post-pr-iteration-guard.json`, `post-pr-failure-terminal.json`

| Artifact | Required fields | Notes |
|----------|-----------------|-------|
| `pr-remediation-plan.json` | `schema_version`, `run_id`, `repository_owner`, `repository_name`, `pr_number`, `head_ref`, `head_sha`, `base_ref`, `base_sha`, `must_fix`, `mark_invalid`, `needs_user_judgment`, `pending_or_unknown`, `plan_state` | `plan_state` is `clean`, `needs_remediation`, `blocked_needs_user_judgment`, or `fatal`; routing still uses only `success`, `fixable`, or `fatal`. |
| `pr-remediation-result.json` | `schema_version`, `run_id`, `repository_owner`, `repository_name`, `pr_number`, `head_ref`, `input_head_sha`, `output_head_sha`, `base_ref`, `base_sha`, `overall_status`, `results`, `verification_commands`, `success_file_path`, `validation_state` | One result per `must_fix` item and CI failure; non-empty malformed artifacts are rejected. |
| `post-pr-test-result.json` | `schema_version`, `run_id`, `repository_owner`, `repository_name`, `pr_number`, `head_ref`, `head_sha`, `base_ref`, `base_sha`, `test_state`, `commands`, `verification_retry_index`, `max_verification_retries`, `retry_scope`, `plan_artifact_sequence`, `remediation_result_artifact_sequence`, `verification_retry_exhausted` | Written only by the dedicated `run_post_pr_tests` executor; local verification failures are artifact-capped and exhaustion routes fatal. |
| `push-remediation-result.json` | `schema_version`, `run_id`, `repository_owner`, `repository_name`, `pr_number`, `head_ref`, `head_sha`, `base_ref`, `base_sha`, `push_state`, `push_retry_index`, `max_push_retries`, `retry_scope`, `remote_ref`, `pre_push_local_head_sha`, `pre_push_remote_head_sha`, `pre_push_pr_head_sha`, `committed_head_sha`, `post_push_local_head_sha`, `post_push_remote_head_sha`, `expected_head_sha`, `verified_remote_matches_expected`, `staged_paths`, `excluded_paths`, `commit_message`, `push_error_class`, `commands`, `stdout_artifact_path`, `stderr_artifact_path` | Always written by dedicated `PushRemediationChangesExecutor` for no-change, excluded-only, pushed, retryable, retry-exhausted, and fatal push outcomes; retry exhaustion routes fatal through this push artifact. |

| `pr-feedback-marker-report.json` | `schema_version`, `run_id`, `repository_owner`, `repository_name`, `pr_number`, `head_ref`, `head_sha`, `base_ref`, `base_sha`, `marker_actions`, `skipped_actions`, `remote_marker_comments_seen`, `resolved_threads`, `posted_comments`, `marker_state` | Supports local and remote idempotency. |
| `post-pr-iteration-guard.json` | `schema_version`, `run_id`, `repository_owner`, `repository_name`, `pr_number`, `head_ref`, `head_sha`, `base_ref`, `base_sha`, `iteration_index`, `max_post_pr_remediation_iterations`, `previous_head_sha`, `reason`, `guard_state`, `ignored_stale_artifacts`, `updated_at` | Deterministic cap for current workflow engine semantics; cap exhaustion returns StepOutcome `fatal`. Records stale/mismatched ignored guard paths explicitly. |
| `post-pr-failure-terminal.json` | `schema_version`, `run_id`, `repository_owner`, `repository_name`, `pr_number`, `head_ref`, `head_sha`, `base_ref`, `base_sha`, `failed_step`, `failure_reason`, `source_artifacts`, `terminal_state`, `logged_at`, `source_failure_sequence`, `source_artifact_sequence`, `source_write_sequence`, `source_producer_step_id`, `source_step_order_index`, `source_artifact_path`, `source_history_path`, `selected_source_reason` | Written by `post_pr_failure_terminal`, which returns StepOutcome `fatal` after logging and records the exact selected source failure artifact without using mtimes. |

All artifacts in this table use integer `schema_version` with initial value `1` and must also include the common `artifact_sequence`, `write_sequence`, `history_metadata`, and `step_order_index` fields. Failure-producing states in these artifacts must also include `producer_step_id`, `semantic_state`, `failure_reason`, `failure_sequence`, and `produced_at`. `post-pr-iteration-guard.json` must include `ignored_stale_artifacts`. Retry/cap artifacts must include the retry index and max fields listed above. `post-pr-failure-terminal.json` must include the required source/sequence fields listed above.




## Implementation Tasks


Downstream schema tests must include table-driven missing/mismatched binding cases for every artifact family: absent `run_id`, wrong `repository_owner`, wrong `repository_name`, wrong `pr_number`, wrong `head_ref`, wrong `head_sha`, wrong `base_ref`, and missing or mismatched `base_sha` when the upstream PR identity provided it. A stale artifact may be reported for audit only; it must not be consumed as current evidence.

### Files to Create

- `tests/fixtures/github_pr/`
  - Fixture JSON examples for PR identity, check status, CodeRabbit feedback, evaluations, remediation plans, remediation results, marker reports, current canonical files, and immutable history/snapshot files with sequence fields.
- `project-plans/coderabbit/analysis/artifact-schema-contract.md`
  - Contract tables and current/history persistence rules that do not require Rust production structs to compile.
- `project-plans/coderabbit/.completed/P01`
  - Completion marker written after fixture examples and schema-contract documentation are complete.


### Files to Modify

- None under `src/` in this phase. Any source schema implementation before Phase 03 stubs violates TDD ordering.

## Verification Commands

```bash
test -d tests/fixtures/github_pr
test -f project-plans/coderabbit/analysis/artifact-schema-contract.md
test -f project-plans/coderabbit/.completed/P01

python3 -m json.tool tests/fixtures/github_pr/pr.json >/dev/null
# Optional if added in this phase: cargo test --test github_pr_fixture_contract_tests
# Expected during P01: commands compile/pass without importing missing production Rust structs; behavioral schema tests are deferred to P04.
```

## Success Criteria

- Fixture examples and schema-contract documentation exist before production schema code.
- Any P01 tests compile without production PR follow-through structs and validate only fixtures/contracts.
- No production `src/` schema or executor behavior is implemented in this phase; behavioral schema validation tests are added after Phase 03 stubs exist.



---

# Phase 02: Pseudocode for Deterministic PR Follow-Through

## Phase ID

`PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P02`

## Prerequisites

- Required: Phase 01 completed
- Verification: `test -f project-plans/coderabbit/.completed/P01 && test -d tests/fixtures/github_pr && test -f project-plans/coderabbit/analysis/artifact-schema-contract.md`


## Requirements Implemented (Expanded)

### REQ-PRFU-033: No LLM CI-State Decisions

**Full Text**: The system shall never ask the LLM to decide whether PR checks are complete, pending, failed, or passed.

**Behavior**:

- GIVEN: a workflow needs PR check status
- WHEN: status is needed
- THEN: deterministic GitHub data and classification pseudocode determine the result

**Why This Matters**: The user specifically wants the runner, not the agent, to own the watch loop.

### REQ-PRFU-034: No LLM Feedback Discovery

**Full Text**: The system shall never rely on the LLM to discover CodeRabbit comments, review threads, or failed check logs.

**Behavior**:

- GIVEN: CodeRabbit or CI feedback exists
- WHEN: the workflow collects it
- THEN: deterministic GitHub APIs and artifact parsing find it

**Why This Matters**: The LLM should judge items, not scrape or miss them.

## Implementation Tasks

### Files to Create

- `project-plans/coderabbit/analysis/domain-model.md`
  - Domain model for PR identity, check state, CodeRabbit feedback, evaluation, remediation, marker state.
- `project-plans/coderabbit/analysis/pseudocode/pr-identity-and-checks.md`
  - Numbered pseudocode for PR identity and check watching.
- `project-plans/coderabbit/analysis/pseudocode/ci-failures.md`
  - Numbered pseudocode for failure collection.
- `project-plans/coderabbit/analysis/pseudocode/coderabbit-feedback.md`
  - Numbered pseudocode for feedback collection/readiness/state.
- `project-plans/coderabbit/analysis/pseudocode/feedback-evaluation.md`
  - Numbered pseudocode for one accepted validated evaluation per item.
- `project-plans/coderabbit/analysis/pseudocode/remediation-and-marker.md`
  - Numbered pseudocode for remediation plan/result validation and marker idempotency.
- `project-plans/coderabbit/analysis/github-api-contract.md`
  - Exact GitHub REST/GraphQL/`gh` API contract for PR identity, checks, logs, CodeRabbit review/thread/comment discovery, marker comments, and review-thread resolution.
- `project-plans/coderabbit/.completed/P02`
  - Completion marker written after pseudocode line ranges and API contract fixtures are complete. Phase 03 uses this marker plus API-contract checks as its analysis prerequisite rather than relying on implementation code-marker greps.


## Required GitHub API Contract Document

`github-api-contract.md` must be created from the preflight field inventory before schema finalization is treated as complete. It must link every consumed JSON path to at least one fixture and at least one planned test assertion. It must include, at minimum:

| Operation | Required query/command | Required fields | Pagination | Fallback order | Resolution/mutation expectation |
|-----------|------------------------|-----------------|------------|----------------|---------------------------------|
| PR identity | `gh pr view --json number,url,headRefName,headRefOid,baseRefName,baseRefOid,state,isDraft` plus GraphQL/REST by head ref when needed | number, URL, head ref/SHA, base ref/SHA, state | not applicable | create_pr artifact verification, current branch `gh pr view`, GraphQL, REST | read-only |
| Check status | Prefer structured `gh pr checks --json` when fields are sufficient; otherwise REST check-runs/statuses for current `head_sha` | check name, status, conclusion, URL, run/job IDs, timestamps, head SHA | all pages until exhausted or cap; fixtures must include page-2 check/check-run data | `gh pr checks --json`, REST checks, REST statuses | read-only |
| Logs | REST Actions jobs/logs by run/job ID; bounded excerpts persisted as files | log URL/status/path/error | all workflow job pages/log surfaces until exhausted; fixtures must include page-2 job/log data | job log, run log, unavailable | read-only |
| CodeRabbit feedback | GraphQL review threads/comments and REST review comments/issues comments as needed | node IDs, thread IDs, comment IDs, author, body, path/line, resolved/outdated, URL, timestamps | cursor/page until exhausted; fixtures must include page-2-only feedback/comment/thread data | GraphQL reviewThreads, REST review comments, REST issue comments | read-only |
| Marker comments | REST or GraphQL comment creation using file/argument body passing | created comment ID/URL/body hash | all remote marker comment surfaces page until exhausted; fixtures must include page-2-only remote markers | existing remote marker detection, create comment | create only when no local or remote marker exists |
| Pending marker actions | `pending-feedback-marker-actions.json` canonical/history state or documented exact equivalent | original item id/key/body hash/source head, remediation input/output head, action/evidence, marker status, comment/resolution idempotency keys | history snapshots scanned until exhausted under artifact root | current clean feedback/evaluations, pending marker history, remote marker comments | carried-forward actions survive head changes until handled/skipped/fatal |

## Hard Gate Before Implementation

Before Phase 03 or any later implementation phase may begin, the implementer must replace every `@pseudocode lines X-Y` placeholder with exact line ranges from the numbered pseudocode files, and `github-api-contract.md` must include exact JSON paths plus fixtures for each GitHub/`gh` response consumed by implementation. This is a hard gate: no Rust implementation, test implementation beyond the pseudocode/TDD scaffolding, TOML implementation, or fixture regeneration may proceed while any PR follow-through code marker contains `@pseudocode lines X-Y`, `@pseudocode TBD`, another placeholder, or while API contract JSON paths/fixtures are missing.

The GitHub API contract gate is machine-checkable, not grep-only. Phase 02 is a mini-TDD harness: add negative fixtures first, make `github_api_contract_tests` fail for missing fields/fixtures/assertions/permissions, then implement the validator until those negative and positive fixtures prove the contract. Phase 02 must add both a checked-in test target named `github_api_contract_tests` and a binary validator executable by the exact command `cargo run --bin github_api_contract_validator -- project-plans/coderabbit/analysis/github-api-contract.md tests/fixtures/github_api_contract`; alternatively, Phase 02 may write a checked-in argv file at `project-plans/coderabbit/analysis/github-api-contract-validator-command.json`, and every Phase 03+ verification block must execute that argv file. The command is mandatory, not an optional/commented equivalent. The validator parses `project-plans/coderabbit/analysis/github-api-contract.md` and fails unless every consumed field, JSON path, fallback branch, pagination surface, readiness truth-table row, marker discovery surface, mutation response path, and permission-denied path names: (1) the exact source command/query/mutation, (2) fixture filename(s), (3) fixture JSON pointer(s), and (4) at least one assertion/test name proving the path/fallback/pagination/readiness/permission row is exercised. The validator/test must fail on missing fixtures, missing assertions, duplicate or unknown fixture pointers, page-2 requirements without page-2 assertions, fallback rows without both primary-failure and fallback-success fixtures, permission-denied rows without required scope/permission metadata assertions, readiness rows without expected artifact states, or any `TBD`, `TODO`, `json_path TBD`, `fixture TBD`, `assertion TBD`, `@pseudocode lines X-Y`, or `@pseudocode TBD` placeholder in API contracts, artifact contracts, pseudocode, PR follow-through source, or PR follow-through tests. Phase 03 and Phase 05+ prerequisites must run this validator/test in addition to explicit negative placeholder greps.

All code-touching implementation phases (P03 and P05 through P19) must have a completion gate requiring: the previous phase `.completed` marker exists; every new or materially modified production function, private helper, test function, struct, enum, trait, impl block, and module-level constant involved in PR follow-through includes adjacent `@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.Pxx`, `@requirement`, and exact `@pseudocode lines <start>-<end>` references, except purely mechanical re-exports that do not implement behavior. The gate must use positive and negative commands: a positive marker audit proving all touched PR follow-through items have the three markers, and explicit negative `! grep` checks proving no `@pseudocode TBD`, `@pseudocode lines X-Y`, `TODO API`, unallowlisted `TBD`, or placeholder text remains in touched PR follow-through code/tests/contracts. Completion markers must cite exact pseudocode line references used by the phase.

`project-plans/coderabbit/analysis/expected-failing-tests.json` is the source of truth for intentionally failing future-phase tests introduced by P04/P16. Each manifest entry must include `test_binary`, `test_name` (or an explicit `test_names` array when a filter is used), `test_filter`, `group`, `owner_phase`, `requirement_id`, `introduced_in_phase`, `removal_required_by_phase`, `expected_failure_mode`, `expected_assertion`, and `artifact_or_fixture`. The allowed `group` values are exactly `graph/fake E2E`, `artifact store`, `GitHub API`, `evaluator`, `remediation validator`, `marker/idempotency`, and `shell safety`; P04 must populate the manifest matrix across these groups for every expected-failing test it introduces and name the owner phase for removing each entry. The manifest validator must reject broad filters such as `post_pr`, `ci_failure`, or `shell_safety` unless the concrete matched test names are enumerated in the entry, and must also reject vague entries: missing/unknown group, missing exact owner phase, missing exact test, `expected_failure_mode` or `expected_assertion` containing only generic text such as `fails`, `not implemented`, `future`, `TBD`, or `TODO`, or missing concrete artifact/fixture. P04 must run `cargo test -- --list` and assert every manifest entry names an existing test in the named binary. Each phase must remove entries for tests it makes pass, verify no unexpected failures beyond the manifest, and update its completion marker with the manifest diff. P21 requires the manifest to exist and be empty. No phase may claim the full suite is green before P21.


| Thread resolution | GraphQL `resolveReviewThread` when thread ID is available | thread ID, mutation response ID, error | not applicable | GraphQL resolution, record unsupported/unavailable | resolve only after deterministic policy allows |

The document must specify exact `gh api` invocations or GraphQL query/mutation names, response JSON paths, page-size/cursor handling, retryable vs fatal API errors, and how every command avoids shell interpolation of GitHub text. For acceptance, every JSON path must list fixture filename(s), fixture JSON pointer(s), and the test name that asserts the path is parsed. It must also specify that marker resolution mutations are attempted only after a local+remote idempotency check.

Pagination/fallback acceptance is explicit: fixtures and tests must cover page-2 data affecting artifacts or routing for PR checks/check-runs, workflow jobs/logs, review threads/comments, issue comments, and remote marker comments. At least one test per surface must fail if only page 1 is consumed. Thread-resolution fixtures must also cover a GraphQL-lacks-comment capability/field gap and assert the REST fallback supplies the comment creation path without shell interpolation.


The first-version CodeRabbit readiness policy must be documented in `coderabbit-feedback.md` and implemented from these signals: configured CodeRabbit bot identity, current-head review/comment/check metadata, ready/completed summary signal, normalized feedback item set, and two identical consecutive observations. Budgets are fixed for v1 at `max_readiness_observations = 6`, `stable_observation_count_required = 2`, and `coderabbit_readiness_observation_interval_seconds = 300` by default; tests use fake sleepers and no real sleeps. First observation is immediate with no pre-sleep; sleep exactly between non-final observations; no sleep occurs after a ready/fatal/final-budget observation. The readiness artifact must record `readiness_state`, `signals_seen`, `bot_identities_matched`, `stable_observation_count`, `observation_hash`, `budget_used`, `budget_remaining`, `items_count`, `outcome_reason`, and `observation_interval_seconds`.

CodeRabbit readiness must include a table-driven truth table in `coderabbit-feedback.md` before P08 can complete. Every row must name source precedence between check-run signals, review state, and review/comment signals; whether in-progress check state overrides a ready review/comment; behavior for a missing CodeRabbit check; stale ready signal when current-head feedback exists; empty feedback with current-head ready check; absent CodeRabbit signal; configured disabled/unsupported behavior; and partial API failure surfaces. Each row must name a checked-in fixture and expected `coderabbit-feedback.json` artifact state. P08 is blocked unless every row has a fixture and expected artifact state. Fixture cases must include absent bot, in-progress bot, ready with no feedback, delayed feedback after ready, changing feedback reset, stable ready, timeout, API fatal, API partial failures, unsupported/disabled configuration, and non-CodeRabbit bot noise.



## Verification Commands

```bash
test -f project-plans/coderabbit/analysis/domain-model.md
grep -R "^[0-9][0-9]*\." project-plans/coderabbit/analysis/pseudocode/*.md | wc -l
test -f project-plans/coderabbit/.completed/P02

# Expected: at least 60 numbered pseudocode lines
cargo test --test github_api_contract_tests -- github_api_contract && cargo run --bin github_api_contract_validator -- project-plans/coderabbit/analysis/github-api-contract.md tests/fixtures/github_api_contract

! grep -R "TBD\|TODO\|provisional\|json_path: TBD\|fixture TBD\|assertion TBD\|@pseudocode lines X-Y\|@pseudocode TBD" project-plans/coderabbit/analysis/artifact-schema-contract.md project-plans/coderabbit/analysis/github-api-contract.md project-plans/coderabbit/analysis/pseudocode

```

## Success Criteria

- Pseudocode has line numbers.
- `github-api-contract.md` validates every externally sourced artifact field and links it to fixture-backed JSON paths.
- `artifact-schema-contract.md` has been updated to reflect validated Phase 02 API field names; no TBD/provisional API fields remain before `.completed/P02` is written.

- Later implementation phases can cite specific pseudocode line ranges.

---

# Phase 03: Engine Executor Stubs

## Phase ID

`PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03`

## Prerequisites

- Required: Phase 02 completed
- Verification: `test -f project-plans/coderabbit/.completed/P02 && grep -R "^[0-9][0-9]*\." project-plans/coderabbit/analysis/pseudocode/*.md`
- Hard gate: `! grep -R "@pseudocode lines X-Y\|@pseudocode TBD\|@pseudocode placeholder" src tests/analysis tests project-plans/coderabbit/analysis 2>/dev/null`
- Hard gate: execute `cargo test --test github_api_contract_tests -- github_api_contract && cargo run --bin github_api_contract_validator -- project-plans/coderabbit/analysis/github-api-contract.md tests/fixtures/github_api_contract`, unless Phase 02 recorded a checked-in argv file at `project-plans/coderabbit/analysis/github-api-contract-validator-command.json`; if that argv file exists, execute exactly that recorded argv in every P03+ verification block. The gate proves every consumed field/path/fallback/pagination/readiness row has fixture + assertion coverage and forbids TBD/TODO/json-path placeholders.


## Requirements Implemented (Expanded)

### REQ-PRFU-020: Integration Reachability

**Full Text**: The system shall register new PR follow-through executors with the default executor registry and make them reachable from workflow TOML. The `llxprt-issue-fix-v1` workflow shall be able to invoke the executors after PR creation. Workflow graph tests shall prove the PR does not complete immediately after `create_pr`. Workflow graph tests shall prove `create_pr` cannot transition directly to `log_completion` for the PR follow-through path. Workflow graph tests shall prove post-remediation verification and push can loop back to PR identity capture with configured loop caps.


**Behavior**:

- GIVEN: a workflow references PR follow-through step types
- WHEN: the engine dispatches those step types
- THEN: registered executors are found

**Why This Matters**: A feature that is not registered in the executor registry is unreachable.

## Implementation Tasks

### Files to Modify

- `src/engine/executors/pr_followup_artifacts.rs`
  - Add mandatory `PrFollowupArtifactStore`/`ArtifactWriter` shells and signatures used by every PR follow-through executor before any executor-specific file writes are implemented. Phase 03 must define constructor shapes, trait boundaries, canonical/history path API signatures, sequence allocator API signatures, binding validator API signatures, and injectable filesystem/clock seams, but it must not implement writer behavior beyond compile-safe placeholders. Direct file writes in PR follow-through executors are prohibited except through this store once behavior is implemented in Phase 05.
  - Do not add direct writer behavior tests in P03. Direct `PrFollowupArtifactStore` behavioral tests are owned by Phase 04 and must fail against these shells/stubs until Phase 05 implements the writer behavior.

- `src/engine/executors/pr_followup_types.rs`
  - Add shared artifact schema stubs, binding fields, artifact sequence types, and common validation entry-point types.
- `src/engine/executors/github_pr.rs`
  - Add only PR identity capture, PR check watching, and CI failure collection stub executors plus shared GitHub command-runner seams for PR/check/CI APIs.
- `src/engine/executors/github_feedback.rs`
  - Add only CodeRabbit feedback collector and feedback marker stub executors plus marker parser/discovery types.
- `src/engine/executors/feedback_eval.rs`
  - Add only feedback evaluation request/response types, evaluator stub, and LLM invocation adapter stubs.
- `src/engine/executors/pr_remediation.rs`
  - Add remediation plan/result validator, `RunPostPrTestsExecutor`, `PushRemediationChangesExecutor`, post-PR iteration guard, and failure terminal stubs.
- Module ownership is fixed for the plan unless a later implementation note records a one-to-one equivalent split: `github_pr.rs` owns only PR identity/checks/CI; `github_feedback.rs` owns CodeRabbit collection/marker; `feedback_eval.rs` owns evaluator/LLM adapter; `pr_remediation.rs` owns remediation plan/result validator/post-PR tests/push/terminal/iteration guard; `pr_followup_artifacts.rs` owns artifact store/writer behavior; `pr_followup_types.rs` owns shared types. Do not move CodeRabbit/marker behavior into `github_pr.rs`, evaluator behavior into GitHub modules, artifact writer behavior into remediation modules, or PR/check/CI behavior into feedback modules.

- Stub executor structs and exact default step-type registrations are mandatory. Phase 03 must add all routed post-PR executors/stubs listed below and register every step type in `ExecutorRegistry::with_defaults()`:
  - `GithubPrIdentityExecutor` registered as `github_pr_identity` for step ID `capture_pr_identity`.
  - `PostPrIterationGuardExecutor` registered as `post_pr_iteration_guard` for step ID `post_pr_iteration_guard`.
  - `GithubPrChecksExecutor` registered as `github_pr_checks` for step ID `watch_pr_checks`.
  - `GithubCheckFailuresExecutor` registered as `github_check_failures` for step ID `collect_ci_failures`.
  - `GithubCodeRabbitFeedbackExecutor` registered as `github_coderabbit_feedback` for step ID `collect_coderabbit_feedback`.
  - `FeedbackEvaluatorExecutor` registered as `feedback_evaluator` for step ID `evaluate_coderabbit_feedback`.
  - `PrRemediationPlanExecutor` registered as `pr_remediation_plan` for step ID `build_remediation_plan`.
  - `PrRemediationResultExecutor` registered as `pr_remediation_result` for step ID `validate_remediation_result`.
  - `RunPostPrTestsExecutor` registered as `run_post_pr_tests` for step ID `run_post_pr_tests`; an exact equivalent name is allowed only if the step type remains exactly `run_post_pr_tests` and P03 notes document the struct name mapping.
  - `PushRemediationChangesExecutor` registered as `push_remediation_changes` for step ID `push_remediation_changes`; this dedicated executor owns `push-remediation-result.json` and must not be replaced by generic shell for structured push artifact creation.
  - `GithubFeedbackMarkerExecutor` registered as `github_feedback_marker` for step ID `mark_coderabbit_feedback`.
  - `PostPrFailureTerminalExecutor` registered as `post_pr_failure_terminal` for step ID `post_pr_failure_terminal`.
  - `PrFollowupRemediationExecutor` registered as `pr_followup_remediation` for step ID `remediate_pr_followup`; it wraps/invokes existing llxprt support but owns PR follow-through failure artifacts and result-path classification. `push_remediation_changes` must dispatch through the dedicated `push_remediation_changes` step type.
  - Stub methods may return deterministic placeholder errors or default non-success results, but must compile and must follow the post-PR error policy: expected deterministic failures write best-effort artifacts when the artifact writer behavior exists and return `Ok(StepOutcome::Fatal)` rather than `Err(EngineError)`. P03 stubs must not return `StepOutcome::Abandon`; abandon is globally valid but intentionally unused by post-PR executor stubs and TOML until a separately tested engine change exists.
  - Add minimal production artifact schema structs now, after Phase 01 compile-safe fixture contracts exist: `PrIdentity`, `PrCheckStatus`, `CiFailures`, `CodeRabbitFeedback`, `FeedbackState`, `FeedbackEvaluations`, `PrRemediationPlan`, `PrRemediationResult`, `PostPrTestResult`, `PushRemediationResult`, `FeedbackMarkerReport`, `PostPrIterationGuard`, and `PostPrFailureTerminal`.
  - Add registry introspection tests that construct `ExecutorRegistry::with_defaults()` and assert lookup succeeds for every planned post-PR step type: `github_pr_identity`, `post_pr_iteration_guard`, `github_pr_checks`, `github_check_failures`, `github_coderabbit_feedback`, `feedback_evaluator`, `pr_remediation_plan`, `pr_followup_remediation` for `remediate_pr_followup`, `pr_remediation_result`, `run_post_pr_tests`, `push_remediation_changes`, `github_feedback_marker`, and `post_pr_failure_terminal`. These P03 tests must use an introspection helper such as `contains_step_type(step_type: &str) -> bool` or `registered_step_types() -> BTreeSet<String>` added to `ExecutorRegistry` rather than dispatching or executing stub executors. Behavioral dispatch/execution tests belong to P04/P05+ after artifact writer behavior exists; P03 stubs must not need a working artifact writer.

  - Stub executors must return only current `StepOutcome` values (`success`, `fixable`, `retryable`, `fatal`, `abandon`) and must not expose custom routable semantic outcomes; richer semantic placeholders belong only in artifact fields.
  - MUST include phase and requirement markers.

- `src/engine/executors/mod.rs`
  - Re-export executor structs and shared PR follow-through types.

- `src/engine/time.rs` or `src/engine/executors/pr_followup_time.rs`
  - Define concrete fake-clock/sleeper abstraction: a trait such as `ClockSleeper` with `now()` and `sleep(duration)`; production implementation delegates to system time/Tokio or std sleep per existing async/sync style; fake implementation stores virtual time and recorded sleep durations. Executor constructors accept the abstraction, while `ExecutorRegistry::with_defaults()` installs the production implementation. Tests must assert virtual time advances and no real sleeps occur.


- `src/engine/executor.rs`
  - Register the new executor step types in `ExecutorRegistry::with_defaults()`.

## Verification Commands

```bash
cargo build
cargo test --test github_api_contract_tests -- github_api_contract && cargo run --bin github_api_contract_validator -- project-plans/coderabbit/analysis/github-api-contract.md tests/fixtures/github_api_contract
grep -R "github_pr_checks" src/engine | wc -l
grep -R "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03" src/engine | wc -l
cargo test --test github_pr_followup_executor_tests -- registry_registers_all_post_pr_step_types_by_introspection
cargo test --test pr_followup_marker_audit_tests -- p03_markers_cover_all_touched_items
! grep -R "@pseudocode lines X-Y\|@pseudocode TBD\|@pseudocode placeholder\|TODO API\|json_path TBD\|fixture TBD\|assertion TBD" src/engine tests project-plans/coderabbit/analysis
```

## Success Criteria

- Build succeeds.
- New step types are registered and default-registry introspection tests cover every planned post-PR step type without executing stubs or requiring artifact writer behavior.
- Shared `PrFollowupArtifactStore`/`ArtifactWriter` shells/signatures exist before executor implementations depend on them; behavioral writer tests are deferred to P04 and implementation to P05.
- No workflow changes yet.


---

# Phase 04: Engine Integration TDD

## Phase ID

`PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P04`

## Prerequisites

- Required: Phase 03 completed
- Verification: `grep -R "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03" src/engine`

## Requirements Implemented (Expanded)

### REQ-PRFU-004: Bounded PR Check Watching

**Full Text**: The system shall watch PR checks for a configurable bounded duration with a configurable polling interval. The default polling interval shall be 300 seconds. The default maximum duration shall be 3600 seconds. The default maximum attempts shall therefore be 12 bounded observations for the current watch invocation, scheduled at t=0,5,...,55 minutes with no t=60 observation. The attempt budget semantics shall be fixture-tested so that the default watch cannot exceed the one-hour budget except for the time required to complete the final in-flight GitHub request/classification started at t=55. The system shall write a status artifact after every polling cycle.

**Behavior**:

- GIVEN: checks remain pending
- WHEN: the watcher uses defaults
- THEN: it performs at most 12 bounded polling cycles and reports timeout rather than success

**Why This Matters**: The requested one-hour loop must be enforceable by tests.

### REQ-PRFU-005: Non-Fail-Fast Check Watching

**Full Text**: The system shall not stop check watching on the first failed check while other current-head checks remain pending and the watch budget remains. The system shall continue polling until all current-head checks are terminal or the watch budget is exhausted. The system shall classify all current-head failures together after polling completes or budget is exhausted.

**Behavior**:

- GIVEN: one check fails while another is pending
- WHEN: budget remains
- THEN: the watcher continues and later reports all failures together

**Why This Matters**: We need all failures, not just the first one.

### REQ-PRFU-006, REQ-PRFU-007, REQ-PRFU-008, REQ-PRFU-009, REQ-PRFU-010, REQ-PRFU-011, REQ-PRFU-012, REQ-PRFU-013, REQ-PRFU-014, REQ-PRFU-015, REQ-PRFU-016

**Full Text**: This phase writes failing behavioral tests proving current-head check classification, stale-head rejection, CI failure/log collection, bounded CodeRabbit readiness, feedback state deduplication, evaluation validation, remediation planning/result validation, marker idempotency, shell safety, and non-success terminal handling. The tests shall use deterministic fixtures or fake `gh`/LLM executors, shall not require live GitHub network access, shall prove only `success`, `fixable`, `retryable`, `fatal`, and `abandon` are used as routable outcomes, and shall prove timeout/unknown/needs-user-judgment/API-failure paths do not complete as workflow success.

**Behavior**:

- GIVEN: fixture GitHub/LLM/remediation artifacts
- WHEN: deterministic executors process them
Phase 04 intentionally owns the core workflow graph/routing TDD before engine behavior implementation begins in Phase 05. Even though full workflow integration TDD remains in P16, P04 must add the minimal graph tests that protect implementation sequencing: no direct `create_pr -> log_completion`, no post-PR route to `abandon_and_log`, only current `StepOutcome` route values, `post_pr_failure_terminal` terminal fatal behavior, no duplicate outcome branches in the post-PR tail, and reachability of `validate_remediation_result -> run_post_pr_tests -> push_remediation_changes -> capture_pr_identity`. These are workflow-specific tests for `llxprt-issue-fix-v1` and its planned fixture; they do not require or promise global duplicate-transition validation for every workflow.


- THEN: outputs match expected JSON and outcomes

**Why This Matters**: Tests must lock the deterministic contract before implementation.

## Implementation Tasks

### Files to Create

- `tests/github_pr_followup_executor_tests.rs`
  - Behavioral integration tests using fixture JSON and fake command runners.
  - Tests must fail naturally against stubs.

### Files to Modify

- `tests/e2e_workflow_integration.rs`
  - Add the initial post-PR workflow graph tests in this phase, before workflow TOML implementation, so they compile and fail against the current `create_pr -> log_completion` TOML. Tests must assert the future routing contract for `watch_pr_checks success -> collect_ci_failures`, `watch_pr_checks fixable -> collect_ci_failures`, and `collect_ci_failures success -> collect_coderabbit_feedback`.

- Possibly `tests/common` or helper modules if existing patterns support fake commands.
- Graph reachability tests must compute reachability from `create_pr` success into the post-PR tail, not only from `capture_pr_identity`. The positive assertion is `create_pr success -> capture_pr_identity -> ... -> post_pr_failure_terminal/log_completion` through the post-PR contract, with no direct `create_pr -> log_completion`. Negative fixtures must include duplicate `create_pr success` transitions, one to `capture_pr_identity` and one to `log_completion`, and the test must fail that ambiguity before any downstream reachability assertion can pass.


## Required Test Cases

- PR identity capture uses post-`create_pr` deterministic GitHub verification and rejects missing PR number/URL/head SHA.
- Default watch budget is 12 observations at t=0,5,...,55 minutes for 300-second interval / 3600-second max duration using an injectable fake clock/sleeper; tests must not actually sleep.
- Non-fail-fast watcher continues after early failure while another check is pending.
- CI `failed` is distinct from `pending_timeout`, `unknown`, and watcher `fatal`; `failures` contains only concrete terminal failed/cancelled/timed_out/action_required checks, while `pending_or_unknown` contains pending-after-budget, unknown, stale-only, and schema-unbindable records. Mixed-state precedence is mandatory: failed+pending after budget and failed+unknown after budget are terminal/non-remediable, but they must first run failure collection so deterministic failure metadata/log excerpts are captured. Fatal watcher artifacts with API/auth/schema failures or no trusted current-head checks are not concrete check failures: the collector must write `ci-failures.json` referencing the watcher fatal source, invent no failures, and return `fatal`. Required tests: failed+pending collects concrete failure logs/metadata, preserves pending in `pending_or_unknown`, then reaches `post_pr_failure_terminal`; failed+unknown collects concrete failure logs/metadata, preserves unknown in `pending_or_unknown`, then reaches `post_pr_failure_terminal`; watcher fatal/API/auth/schema/no-trusted-checks writes collector fatal referencing the watcher source with `failures=[]`; all-terminal with one failure reaches remediation/fixable; all-terminal acceptable checks reach the clean success path.
- Unknown state prevents success.
- Stale head SHA checks are ignored for current-head success/failure and reported as stale.
- CI failure collection preserves missing-log failures, separately records pending/unknown timeout items, writes an empty `ci-failures.json` with `failures=[]` and `pending_or_unknown=[]` after passed checks, writes a fatal collector artifact that references the watcher fatal source when the watcher artifact is fatal without trusted concrete checks, and returns `fatal` after collection when `pending_or_unknown` is non-empty or the watcher fatal source is present so remediation is skipped.
- CodeRabbit feedback collector does not treat empty feedback as clean before readiness/stabilization.
- CodeRabbit readiness fixtures cover the exact truth table: no bot signal, in-progress bot signal, current-head ready check signal, stale ready check ignored, terminal review states, pending review/check states, ready summary with zero comments, ready summary with delayed comments, exact HTML marker parsing, two identical stable observations, changing feedback that resets stability, stale signals ignored, timeout, API fatal, and non-CodeRabbit bot noise.
- First-version bot identities include `coderabbitai[bot]`, `coderabbit[bot]`, and configurable additional logins; fixtures must include non-CodeRabbit bots to prove filtering.
- CodeRabbit normalization preserves thread IDs, comment IDs, path, line, author, URL, resolution state.
- Feedback state deduplicates unchanged body/hash/head and re-evaluates changed feedback.
- Phase 04 adds the Rust behavioral schema tests that import Phase 03 production stubs; they must compile and fail behaviorally against incomplete stub validation, including missing/mismatched binding fields, missing `artifact_sequence`, `write_sequence`, required `history_metadata`, failure metadata, source/sequence fields on `post-pr-failure-terminal.json`, `ignored_stale_artifacts` on `post-pr-iteration-guard.json`, and current/history sequence validation.
- Feedback evaluator invokes exactly one `FeedbackEvaluationRequest` per current feedback item per attempt; each request contains one item only and includes item ID, stable marker key, body hash, and head SHA. Validation rejects arrays, responses containing multiple item IDs, extra item IDs, wrong body hash, wrong head SHA, unknown decisions, malformed JSON, and missing reasons; internal per-item retries stop at 3 attempts without workflow self-loop.
- Fake LLM tests prove a batch-style response is rejected, and unchanged reused evaluations are emitted exactly once in `accepted_results` without re-invoking the LLM adapter.
- Remediation plan routes only concrete CI `failures` and feedback `accepted_results` with `decision=valid` to `must_fix`; invalid/out-of-scope goes to `mark_invalid`; user-judgment plus `pending_or_unknown` timeout/unknown/stale-only/schema-unbindable evidence goes to `needs_user_judgment` and the fatal/non-success artifact path.
- Pagination/fallback tests for GitHub surfaces are owned by P04 as TDD and made pass in their implementation phases: page-2-only PR checks/check-runs affect `pr-check-status.json`; page-2-only workflow jobs/logs affect `ci-failures.json` log artifacts/routing; page-2-only review threads/comments and issue comments affect `coderabbit-feedback.json`; page-2-only remote marker comments affect marker idempotency; and GraphQL-lacks-comment fixtures force the REST fallback path for comment creation/resolution support.
- Pending marker carry-forward tests are owned by P04/P16 TDD: head A feedback is remediated, pending marker actions are persisted, push changes PR head to B, current head B feedback is ready/clean/empty, and the marker still comments/resolves the original item without duplicating on retry.

- Remediation result validation requires every `must_fix` item to use the canonical status enum `fixed | changed | already_satisfied | not_reproduced | not_fixed | skipped | failed` and to have validator-acceptable evidence before success: `fixed`/`changed`, or tightly scoped `already_satisfied`/`not_reproduced` with deterministic evidence fields. Structured `not_fixed`, `skipped`, `failed`, missing, or explanation-only results are valid-but-unsuccessful and must not route to push success; they loop to llxprt only while the same-head unsuccessful-remediation artifact cap remains.
- P04 owns tests before implementation proving: a valid structured result with `not_fixed` for a CodeRabbit `must_fix` item does not reach `push_remediation_changes` success; a valid structured result with `skipped` or `failed` for a CI failure does not reach `push_remediation_changes` success; `already_satisfied` and `not_reproduced` are accepted only with deterministic evidence fields; repeated same-head no-change remediation attempts exhaust the artifact-backed `remediation_attempt_index`/`max_remediation_attempts` cap and reach `post_pr_failure_terminal` with `RunOutcome::Failure { step_id: "post_pr_failure_terminal", ... }`, never `RunOutcome::Abandoned`; and `post_pr_iteration_guard` same-head preservation is safe because this same-head remediation cap handles the loop.

- Remediation result validation requires every `must_fix` item to have a result and loops fixable validator failures to llxprt at most 2 times using artifact-backed retry metadata, never transition-cap semantics.
- Core workflow graph/routing tests are written before engine behavior implementation in `tests/e2e_workflow_integration.rs`: no direct `create_pr -> log_completion`; no post-PR route to `abandon_and_log`; route outcomes are limited to current `StepOutcome` values (`success`, `fixable`, `retryable`, `fatal`, `abandon`); `post_pr_failure_terminal` is terminal and returns fatal/non-success; workflow-specific duplicate outcome branches in the post-PR tail are rejected by tests; `watch_pr_checks` success and fixable both route to `collect_ci_failures`; `collect_ci_failures` success routes to `collect_coderabbit_feedback`; and `validate_remediation_result -> run_post_pr_tests -> push_remediation_changes -> capture_pr_identity` is reachable.

- Schema/binding validation rejects every downstream artifact with missing or mismatched `run_id`, `repository_owner`, `repository_name`, `pr_number`, `head_ref`, `head_sha`, `base_ref`, or available `base_sha`.
- PR check watcher fake writer proves `pr-check-status.json` is rewritten after every polling cycle with aggregate `poll_observations`.
- Fake sleeper polling tests prove first poll is immediate at t=0, there are exactly 12 max observations by default at t=0,5,...,55 minutes, sleeps occur only between attempts, no sleep occurs after the final poll, no t=60 observation occurs, 12 pending polls record exactly 11 sleeps of 300 seconds, virtual time advances by 3300 seconds before the 12th poll starts, and the final in-flight request response is still consumed/classified.
- Fake sleeper CodeRabbit readiness tests prove the first observation is immediate, `coderabbit_readiness_observation_interval_seconds` defaults to 300 seconds and is configurable, sleeps occur only between non-final observations, no sleep occurs after stable-ready/fatal/final-budget observations, six not-ready observations record exactly five sleeps by default, and changing feedback resets stability without resetting the observation budget.

- Error-policy tests simulate GitHub API/auth failures, schema validation failures, and command-runner failures in post-PR executors and assert they write best-effort failure artifacts, return `Ok(StepOutcome::Fatal)`, and route through `post_pr_failure_terminal`; only unrecoverable context/programming failures may return `Err(EngineError)`.
- Direct `PrFollowupArtifactStore`/`ArtifactWriter` tests cover canonical/history path calculation, temp-file path placement in the same directory, atomic rename writes, artifact/write sequence allocation, failure sequence allocation, binding validation helper behavior, and resume allocation from history/sidecar state including canonical-only loss, history-only recovery, sidecar lower/higher than history, duplicate sequence numbers, malformed history JSON, mismatched binding fields, and global artifact/failure versus per-family write sequence semantics. Include explicit interleaved-family test `artifact_store_allocates_global_artifact_and_failure_sequences_with_per_family_write_sequences_for_interleaved_families`.


- Marker idempotency avoids duplicate comments for the same stable marker key/body/head using both local `pr-feedback-marker-report.json` and remote marker comments.
- Shell safety tests prove raw CodeRabbit body and LLM/output text are written to files/arguments and not shell-interpolated. P04 must introduce the shared malicious-text fixtures (`backticks`, `$()`, quotes, newlines, here-doc delimiters) and required shell-safety test names for GitHub API runners, feedback evaluator adapter, llxprt wrapper prompt/result handling, marker comment creation, post-PR tests, and push commands; implementation phases P06/P07/P08/P09/P12/P13/P14/P15 make their owned filters pass. These required shell-safety tests are not deferred to P19.
- Table-driven policy tests must follow the integration-first tests and cover classification, stale binding, CodeRabbit normalization, evaluation validation, remediation aggregation, and idempotency key generation at unit granularity. These unit tests may reuse integration fixtures but must assert individual policy functions directly.
- Post-PR terminal tests must explicitly distinguish old pre-PR `abandon_and_log` behavior from new post-PR `post_pr_failure_terminal` behavior: existing pre-PR tests may continue proving `abandon_and_log` semantics, while post-PR tests must prove non-success is not masked as `RunOutcome::Success`. Mock-runner tests must assert the final result is `RunOutcome::Failure` (or the project equivalent fatal/non-success run outcome) at `post_pr_failure_terminal`.



## Phase-Gated Test Matrix and Verification Commands

Phase 04 creates broad integration tests, but later phases must use filtered gates until the relevant implementation exists. `cargo test` for the whole crate is allowed to fail between Phase 04 and final Phase 21 only because intentionally failing future-phase tests are listed in `project-plans/coderabbit/analysis/expected-failing-tests.json`; each phase must still pass its owned filtered tests before completion, remove manifest entries it makes pass, verify no unexpected failures beyond the manifest, and never claim `cargo test --quiet`, the full suite, or CI is green until P21. Direct expected-failing `cargo test` invocations are diagnostic only and must not appear as normal pass gates. P04 and P16 must add and run an explicit expected-failure verifier command (checked-in script/binary) such as `python3 project-plans/coderabbit/analysis/verify-expected-failing-tests.py --manifest project-plans/coderabbit/analysis/expected-failing-tests.json -- cargo test ...` or an equivalent `cargo run --bin expected_failing_tests_verifier -- ...`. The verifier must run the expected-failing workflow graph/fake integration tests, capture stdout/stderr/status, and fail unless: every manifest-named test exists in `cargo test -- --list`; filters expand only to the exact enumerated test names; failures are assertion/behavior failures matching `expected_assertion`/`expected_failure_mode`; there are no compile failures, panics from missing stubs, harness errors, or infrastructure errors; and there are no unexpected passing/failing tests outside the manifest.

| Phase completion gate | Required filtered tests expected to pass | Broad tests allowed to fail? |
|-----------------------|------------------------------------------|------------------------------|
| P04 | list/representative TDD tests compile and fail behaviorally; manifest is populated with exact future-phase failures | Yes, only manifest-listed failures. |
| P05 | artifact-store filters, sequence/recovery/binding tests, and interleaved-family tests pass | Yes, only manifest-listed later filters. |
| P06 | all P05 filters plus `-- pr_identity`, `-- pr_checks`, `-- check_classification`, `-- github_pr_command_runner_shell_safety` | Yes. |
| P07 | all P06 filters plus `-- ci_failure`, `-- ci_failure_passed_checks_writes_empty_artifact`, `-- ci_log_collection_shell_safety` | Yes. |
| P08 | all P07 filters plus `-- coderabbit`, `-- coderabbit_api_shell_safety`; every readiness truth-table row has fixture and expected artifact state | Yes. |
| P09 | all P08 filters plus `-- feedback_evaluation`, `-- feedback_evaluator_command_shell_safety` | Yes. |
| P10 | remediation plan filters and status-vocabulary grep tests pass | Yes. |
| P11 | remediation result validator/status/cap/evidence filters pass | Yes. |
| P12 | remediation wrapper and prompt-contract filters pass | Yes. |
| P13 | `run_post_pr_tests` and post-PR test shell-safety filters pass | Yes. |
| P14 | `push_remediation_changes` and post-PR push shell-safety filters pass | Yes. |
| P15 | marker, idempotency, carry-forward, and shell-safety filters pass | Workflow TOML graph tests may still fail until P17. |
| P16 | workflow graph and fake E2E tests compile and fail against current TOML only; required coverage is non-ignored | Yes, P16 is TDD for TOML. |
| P17 | workflow graph, dry-run step list, fake E2E, and production-vs-fixture TOML/JSON comparison tests pass | Remaining unrelated crate tests only if documented outside this feature. |
| P21 | `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --quiet`, `cargo build --release --quiet`, required e2e/fake workflow tests, production-vs-fixture comparison, preflight-recorded final dry-run argv, and empty expected-failing manifest all pass | No. |



```bash
cargo test --test github_pr_followup_executor_tests -- --list
cargo test --test e2e_workflow_integration -- post_pr --list
cargo test -- --list
python3 project-plans/coderabbit/analysis/validate-expected-failing-tests.py project-plans/coderabbit/analysis/expected-failing-tests.json

grep -R "@requirement:REQ-PRFU" tests/github_pr_followup_executor_tests.rs | wc -l
cargo test --test github_pr_followup_executor_tests -- pr_checks_page2_data_affects_status
cargo test --test github_pr_followup_executor_tests -- pending_marker_actions_carry_forward_across_head_change
cargo test --test e2e_workflow_integration -- post_pr
# Expected during P04: representative tests above compile and fail by assertions/behavior against stubs, not compile errors. Capture the failure excerpts in `project-plans/coderabbit/.completed/P04` or linked completion evidence.

```

## Success Criteria

- Tests are behavioral and do not expect stubs/panics.
- Representative tests have actually been executed against stubs, not merely listed.
- Tests fail for missing behavior, not compile errors; the completion marker or linked evidence includes captured failure output and explicitly identifies assertion/behavior failures.
- The phase matrix is followed so broad future-phase tests are not treated as a failure before their implementation phase. Completion evidence names intentionally failing future-phase tests and owner phases, proves existing pre-feature tests still pass or documents unrelated failures, and never claims full-suite green until P21.
- The expected-failure verifier has run the expected-failing graph/fake integration tests and proven the actual failures exactly match `expected-failing-tests.json` by existing test name and expected assertion/behavior failure, with no compile failures or unexpected failures.


---

# Phase 05: Dedicated Artifact Store Implementation

## Phase ID

`PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05`

## Prerequisites

- Required: Phase 04 completed
- Verification: `test -f tests/github_pr_followup_executor_tests.rs && cargo test --test github_api_contract_tests -- github_api_contract && cargo run --bin github_api_contract_validator -- project-plans/coderabbit/analysis/github-api-contract.md tests/fixtures/github_api_contract`
- Hard gate: exact `@pseudocode lines <start>-<end>` ranges from P02 must be present before implementation; placeholders such as `@pseudocode lines X-Y` are forbidden.
- Pseudocode: exact lines from `project-plans/coderabbit/analysis/pseudocode/remediation-and-marker.md` for artifact persistence and binding validation.

## Implementation Tasks

- Implement the only dedicated `PrFollowupArtifactStore`/`ArtifactWriter` behavior in `src/engine/executors/pr_followup_artifacts.rs` before PR identity/check, CI, feedback, evaluation, remediation, verification, push, or marker executors write PR follow-through artifacts.
- Own canonical/current paths, immutable history/snapshot paths, temp-file placement in the same directory, atomic writes, canonical/history recovery, JSON serialization validation, binding validation helpers, global `artifact_sequence`, per-family `write_sequence`, and global `failure_sequence` allocation.
- Recovery must scan accepted same-run history snapshots, repair or ignore sidecar/cache state only after validation, recover after canonical-only or history-only loss, and reject malformed, duplicate, decreasing, or unbound current-run sequence data that affects a consumed family.
- Add and make pass interleaved-family tests, including `artifact_store_allocates_global_artifact_and_failure_sequences_with_per_family_write_sequences_for_interleaved_families`.
- From completion of this phase onward, all PR follow-through executors write artifacts only through `PrFollowupArtifactStore`. Later phases may add typed artifact-family payloads and call store APIs, but must not implement independent artifact writing, sequence allocation, canonical/history recovery, or `ArtifactWriter` behavior.

### Required Code Markers

Every new or materially modified production function, private helper, test function, struct, enum, trait, impl block, and module-level constant involved in artifact-store behavior must include adjacent markers; purely mechanical re-exports are exempt:

```rust
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-004
/// @pseudocode lines <exact-start>-<exact-end>
```

## Verification Commands

```bash
cargo test --test github_api_contract_tests -- github_api_contract && cargo run --bin github_api_contract_validator -- project-plans/coderabbit/analysis/github-api-contract.md tests/fixtures/github_api_contract
cargo test --test github_pr_followup_executor_tests -- artifact_store
cargo test --test github_pr_followup_executor_tests -- artifact_store_allocates_global_artifact_and_failure_sequences_with_per_family_write_sequences_for_interleaved_families
cargo test --test pr_followup_marker_audit_tests -- p05_markers_cover_all_touched_items
! grep -R "@pseudocode lines X-Y\|@pseudocode TBD\|@pseudocode placeholder\|TODO API\|json_path TBD\|fixture TBD\|assertion TBD" src/engine/executors/pr_followup_artifacts.rs tests project-plans/coderabbit/analysis
! grep -R "todo!\|unimplemented!" src/engine/executors/pr_followup_artifacts.rs
```

## Success Criteria

- Artifact store tests for canonical/history writes, sequence allocation, failure sequences, recovery, binding validation, and interleaved families pass.
- `expected-failing-tests.json` removes artifact-store tests made pass and contains only later-phase expected failures.
- Completion marker cites exact `@pseudocode` line ranges and contains no TBD placeholders.

---

# Phase 06: PR Identity and Check Watch Implementation

## Phase ID

`PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06`

## Prerequisites

- Required: Phase 05 completed
- Verification: `test -f project-plans/coderabbit/.completed/P05 && grep -R "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05" src/engine && cargo test --test github_api_contract_tests -- github_api_contract && cargo run --bin github_api_contract_validator -- project-plans/coderabbit/analysis/github-api-contract.md tests/fixtures/github_api_contract`
- Hard gate: exact `@pseudocode lines <start>-<end>` ranges from P02 must be present before implementation; placeholders such as `@pseudocode lines X-Y` are forbidden.
- Pseudocode: `project-plans/coderabbit/analysis/pseudocode/pr-identity-and-checks.md`

## Requirements Implemented (Expanded)

### REQ-PRFU-001, REQ-PRFU-002, REQ-PRFU-004, REQ-PRFU-005, REQ-PRFU-006, REQ-PRFU-021, REQ-PRFU-022, REQ-PRFU-027, REQ-PRFU-030, REQ-PRFU-033

**Full Text**: This phase implements deterministic PR identity capture and current-head-bound check watching. The PR artifact must include repository owner/name, PR number/URL, head ref/SHA, base ref/SHA when available, capture timestamp, source, and integer `schema_version`. The check watcher must poll structured GitHub data for the current head only, ignore stale checks for success/failure while reporting them, use the default 12 observations at 300-second intervals within a 3600-second budget (t=0,5,...,55 minutes; no t=60 observation), continue polling after early failures while other checks remain pending, classify semantic states as passed/failed/pending_timeout/unknown/fatal inside `pr-check-status.json`, and return only StepOutcome `success`, `fixable`, or `fatal`. The LLM must not decide CI state.

**Behavior**:

- GIVEN: structured PR/check data
- WHEN: identity/check executors run
- THEN: they produce current-head-bound JSON artifacts and routable outcomes without LLM involvement

**Why This Matters**: This replaces the human `gh pr checks --watch --interval 300` loop.

## Implementation Tasks

### Files to Modify

- `src/engine/executors/github_pr.rs`
  - Implement PR identity capture and check classification/watch logic following pseudocode lines from P02.
  - Use `std::process::Command` with args or an injectable command runner; do not shell-parse table output.
  - Implement an injectable clock/sleeper so watch tests advance virtual time and never sleep for the one-hour budget.
  - Classify concrete current-head terminal failures separately from `pending_timeout` and `unknown`; only concrete failures flow to `ci-failures.failures`, while pending/unknown timeout evidence flows to `pending_or_unknown` and fatal/non-success artifact paths.
  - Enforce mixed-state precedence after the observation budget: any current-head pending check makes the aggregate `overall_state=pending_timeout`, and any persistent unknown/schema-unbindable current-head check makes the aggregate `overall_state=unknown`, even if other checks failed. If concrete current-head failures are also present, route through failure collection before terminal; concrete failures return a remediable path only when every current-head check is terminal and no persistent pending/unknown exists.
  - Make the manifest-listed P04 table-driven tests pass for failed+pending collection-then-terminal, failed+unknown collection-then-terminal, all-terminal failure remediation/fixable, and all-terminal acceptable clean success.

  - Return StepOutcome `success` for semantic `passed`, `fixable` for concrete semantic `failed`, and `fatal` for semantic `pending_timeout`, `unknown`, or `fatal`; do not return custom routable outcomes.
  - Pending/unknown timeout evidence must route through the non-success terminal path, not through remediation as accepted work.
  - Write `pr-check-status.json` after every completed polling cycle as an aggregate artifact with `poll_observations`; tests must use a fake writer to prove write-after-each-poll semantics.
  - Write `pr.json` and `pr-check-status.json` only through the shared `PrFollowupArtifactStore`/`ArtifactWriter`; direct ad hoc file writes are prohibited.
  - Make P04 pagination tests pass for PR check/check-run surfaces: page-2-only current-head check data must be consumed, affect `pr-check-status.json`, and change routing/classification when page 1 alone would be incomplete or clean.
  - Implement exact PR check polling schedule: perform the first poll immediately at virtual time `t=0` with no pre-poll sleep; after each non-terminal poll except the final allowed attempt, sleep exactly `poll_interval_seconds` before the next poll; perform at most `max_attempts=12` observations by default at t=0,5,...,55 minutes; do not perform a t=60 observation; do not sleep after the final poll; for 12 pending polls with a 300-second interval, fake virtual time advances by exactly 3300 seconds of sleeps before the 12th poll starts. The maximum-duration budget is a guard around this observation schedule and permits only the final in-flight GitHub request/classification already started at t=55 to complete. Tests must assert recorded sleep count is 11 for 12 attempts, every sleep is 300 seconds, no sleep occurs after attempt 12, no t=60 request is started, and final classification is based on the last in-flight request response.

### Required Code Markers

Every new or materially modified production function, private helper, test function, struct, enum, trait, impl block, and module-level constant involved in PR identity/check behavior must include adjacent markers; purely mechanical re-exports are exempt:

```rust
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-004
/// @pseudocode lines <exact-start>-<exact-end>
```

## Verification Commands

```bash
cargo test --test github_api_contract_tests -- github_api_contract && cargo run --bin github_api_contract_validator -- project-plans/coderabbit/analysis/github-api-contract.md tests/fixtures/github_api_contract
cargo test --test github_pr_followup_executor_tests -- pr_identity
cargo test --test github_pr_followup_executor_tests -- pr_checks
cargo test --test github_pr_followup_executor_tests -- check_classification
cargo test --test github_pr_followup_executor_tests -- github_pr_command_runner_shell_safety
cargo test --test pr_followup_marker_audit_tests -- p06_markers_cover_all_touched_items
! grep -R "@pseudocode lines X-Y\|@pseudocode TBD\|@pseudocode placeholder\|TODO API\|json_path TBD\|fixture TBD\|assertion TBD" src/engine/executors/github_pr.rs tests project-plans/coderabbit/analysis
! grep -R "todo!\|unimplemented!" src/engine/executors/github_pr.rs
```

## Success Criteria

- PR identity and check tests pass.
- Watch budget and non-fail-fast behavior are tested.
- Shell-safety gate passes for PR identity/check command runner construction: no untrusted PR/check text is shell-interpolated; fake runner captures argv/file inputs only.
- Unknown/stale/pending cannot produce false success.

---

# Phase 07: CI Failure Collection Implementation

## Phase ID

`PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07`

## Prerequisites

- Required: Phase 06 completed
- Verification: `test -f project-plans/coderabbit/.completed/P06 && grep -R "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06" src/engine && cargo test --test github_api_contract_tests -- github_api_contract && cargo run --bin github_api_contract_validator -- project-plans/coderabbit/analysis/github-api-contract.md tests/fixtures/github_api_contract`
- Pseudocode: `project-plans/coderabbit/analysis/pseudocode/ci-failures.md`

## Requirements Implemented (Expanded)

### REQ-PRFU-007: CI Failure Collection

**Full Text**: The system shall collect deterministic metadata for each concrete failed, cancelled, timed-out, or action-required current-head check after watch completion. Unknown, pending-after-budget, stale-only, and schema-unbindable check records shall be collected under `pending_or_unknown`, not `failures`. The system shall collect check name, state/conclusion, URL, run ID/job ID when available, and a bounded log excerpt when logs are available. The system shall record log availability as `available`, `unavailable`, `not_applicable`, or `fetch_failed`. Missing logs shall not erase the check failure from the artifact.

**Behavior**:

- GIVEN: check status contains failed and cancelled checks
- WHEN: failure collection runs
- THEN: every failure appears in `ci-failures.json` with log status metadata

**Why This Matters**: The remediation LLM needs deterministic failure inputs.

## Implementation Tasks

### Files to Modify

- `src/engine/executors/github_pr.rs`
  - Implement CI failure collection from `pr-check-status.json`.
  - Collect logs through structured run/job IDs when available; record unavailable/fetch_failed when not.
  - When `pr-check-status.json.overall_state=passed`, still write `ci-failures.json` with `collection_state=collected`, `failures=[]`, `pending_or_unknown=[]`, and `log_artifacts=[]`, then return StepOutcome `success`. When `overall_state=pending_timeout` or `unknown`, collect concrete failure metadata/logs if present, preserve all pending/unknown records in `pending_or_unknown`, write `collection_state=collected`, and return StepOutcome `fatal` to `post_pr_failure_terminal`; do not put pending/unknown evidence in `failures` or remediation `must_fix`. When `overall_state=fatal` because the watcher recorded API/auth/schema errors or no trusted current-head checks, write `ci-failures.json` with `collection_state=fatal`, `failures=[]`, no invented failure names/logs, `watcher_fatal_source`/source artifact reference, safe error metadata copied from the watcher artifact, and return StepOutcome `fatal`.
  - Make P04 pagination tests pass for workflow jobs/logs: page-2-only jobs/log metadata must be collected, affect `ci-failures.json` and log artifacts, and cannot be silently dropped.
  - Fetch logs through the injected command/API runner with explicit argv and bounded artifact files; never build shell strings from check names, URLs, or log text.

## Verification Commands

```bash
cargo test --test github_api_contract_tests -- github_api_contract && cargo run --bin github_api_contract_validator -- project-plans/coderabbit/analysis/github-api-contract.md tests/fixtures/github_api_contract
cargo test --test github_pr_followup_executor_tests -- ci_failure
grep -R "@requirement:REQ-PRFU-007" src/engine tests | wc -l
cargo test --test github_pr_followup_executor_tests -- ci_failure_passed_checks_writes_empty_artifact
cargo test --test github_pr_followup_executor_tests -- ci_failure_collector_watcher_fatal_writes_source_reference_without_invented_failures
cargo test --test github_pr_followup_executor_tests -- ci_failure_collector_pending_unknown_collects_only_concrete_failures_and_preserves_pending_or_unknown

cargo test --test github_pr_followup_executor_tests -- ci_log_collection_shell_safety
cargo test --test pr_followup_marker_audit_tests -- p07_markers_cover_all_touched_items
! grep -R "@pseudocode lines X-Y\|@pseudocode TBD\|@pseudocode placeholder\|TODO API\|json_path TBD\|fixture TBD\|assertion TBD" src/engine/executors/github_pr.rs tests project-plans/coderabbit/analysis
```

## Success Criteria

- Missing logs do not remove failures.
- Watcher fatal/API/auth/schema/no-trusted-check paths produce a collector fatal artifact that references the watcher source and invents no failures; pending_timeout/unknown with concrete failures collects only concrete failures and preserves pending/unknown evidence while returning fatal.

- Passed-check paths still produce a current-head `ci-failures.json` with empty `failures` and `pending_or_unknown` arrays.
- Shell-safety gate passes for CI log collection command construction and log artifact handling.
- Failure artifact is current-head-bound.

---

# Phase 08: CodeRabbit Feedback Collection Implementation

## Phase ID

`PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08`

## Prerequisites

- Required: Phase 07 completed
- Verification: `test -f project-plans/coderabbit/.completed/P07 && grep -R "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07" src/engine && cargo test --test github_api_contract_tests -- github_api_contract && cargo run --bin github_api_contract_validator -- project-plans/coderabbit/analysis/github-api-contract.md tests/fixtures/github_api_contract`
- Pseudocode: `project-plans/coderabbit/analysis/pseudocode/coderabbit-feedback.md`

## Requirements Implemented (Expanded)

### REQ-PRFU-008, REQ-PRFU-009, REQ-PRFU-010, REQ-PRFU-024, REQ-PRFU-034

**Full Text**: This phase implements deterministic CodeRabbit feedback collection, readiness/stabilization, feedback state, deduplication, and no LLM feedback discovery. The collector must discover review threads/comments through GitHub APIs or structured fake fixtures, filter configured CodeRabbit bot identities (`coderabbitai[bot]`, `coderabbit[bot]`, plus configuration), normalize current-head feedback with stable keys and body hashes, require a ready/completed signal plus two identical consecutive observations before returning StepOutcome `success`, use a collector-internal observation budget of 6 with fake clock/sleeper in tests, and return StepOutcome `fatal` with a non-success readiness artifact for not-ready budget exhaustion, timeout, API/auth/schema errors, or other ambiguity. Workflow TOML must not self-loop this collector.

**Behavior**:

- GIVEN: GitHub review/comment fixtures for CodeRabbit
- WHEN: collection runs
- THEN: current-head feedback items and readiness/state artifacts are produced deterministically

**Why This Matters**: Empty feedback is only clean when CodeRabbit is ready/stable, not when comments simply have not arrived yet.

## Implementation Tasks

### Files to Modify

- `src/engine/executors/github_feedback.rs`
  - Implement CodeRabbit feedback normalization and feedback state updates.
  - Add GraphQL/REST command construction with no shell interpolation.
  - Implement first-version readiness/stabilization policy exactly from the truth table in `coderabbit-feedback.md`: CodeRabbit bot identity observed from configured logins (`coderabbitai[bot]`, `coderabbit[bot]`, plus config); a current-head ready/completed signal from review/summary/check metadata; all feedback observations bound to current head; stale signals recorded but ignored; current-head feedback changes reset stability; two consecutive identical normalized observations required for semantic `ready`; max 6 readiness observations per invocation inside the collector; observation interval from `coderabbit_readiness_observation_interval_seconds` defaulting to 300; first observation immediate with no pre-sleep; sleep only between non-final observations; no final sleep; no workflow self-loop; fake clock/sleeper in tests; timeout/not-ready budget exhaustion writes `readiness_state=timeout` and returns StepOutcome `fatal`; fatal API/auth/schema errors write `readiness_state=fatal` and return StepOutcome `fatal`.
  - Make P04 pagination/fallback tests pass for review threads/comments, issue comments, and remote marker discovery: page-2-only CodeRabbit feedback and remote marker comments must affect `coderabbit-feedback.json`/marker idempotency, and GraphQL-lacks-comment fixtures must use the REST fallback rather than dropping comment capability.
  - Preserve fallback behavior when resolution state is unavailable.
  - State and implement CodeRabbit v1 scope explicitly: if no observable CodeRabbit ready/completed signal exists after the observation budget, write non-ready evidence and route `fatal`; repositories without CodeRabbit enabled require explicit config disable/alternate readiness support if implemented, otherwise they are out of scope for this workflow.
  - Define marker remote discovery surfaces before marker implementation: GraphQL review thread comments, REST review comments, and REST issue comments as supported by the API contract; all discovery must paginate until exhausted or documented cap. The marker parser accepts only the exact hidden HTML marker namespace, extracts marker key/head/body hash/run ID, rejects malformed markers, treats duplicate identical remote markers as already complete with audit evidence, treats conflicting duplicate markers as ambiguity/fatal, and resolves local-vs-remote conflict by trusting matching remote completed actions for idempotency while recording stale/mismatched local artifacts as ignored.

## Verification Commands

```bash
cargo test --test github_api_contract_tests -- github_api_contract && cargo run --bin github_api_contract_validator -- project-plans/coderabbit/analysis/github-api-contract.md tests/fixtures/github_api_contract
cargo test --test github_pr_followup_executor_tests -- coderabbit
cargo test --test github_pr_followup_executor_tests -- coderabbit_api_shell_safety
grep -R "@requirement:REQ-PRFU-009" src/engine tests | wc -l
cargo test --test pr_followup_marker_audit_tests -- p08_markers_cover_all_touched_items
! grep -R "@pseudocode lines X-Y\|@pseudocode TBD\|@pseudocode placeholder\|TODO API\|json_path TBD\|fixture TBD\|assertion TBD" src/engine/executors/github_feedback.rs tests project-plans/coderabbit/analysis
```

## Success Criteria

- Feedback readiness/stabilization is bounded.
- Empty-not-ready cannot return clean success.
- Shell-safety gate passes for CodeRabbit API/GraphQL/REST command construction and untrusted review text handling.
- Unchanged feedback is deduplicated by stable ID/body hash/head SHA.

---

# Phase 09: Feedback Evaluation Runner Implementation

## Phase ID

`PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09`

## Prerequisites

- Required: Phase 08 completed
- Verification: `test -f project-plans/coderabbit/.completed/P08 && grep -R "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08" src/engine && cargo test --test github_api_contract_tests -- github_api_contract && cargo run --bin github_api_contract_validator -- project-plans/coderabbit/analysis/github-api-contract.md tests/fixtures/github_api_contract`
- Pseudocode: `project-plans/coderabbit/analysis/pseudocode/feedback-evaluation.md`

## Requirements Implemented (Expanded)

### REQ-PRFU-011: Per-Item LLM Evaluation Contract

**Full Text**: The system shall produce exactly one accepted validated feedback evaluation result per CodeRabbit feedback item for the current item body and head SHA when evaluation succeeds. The system may retry malformed LLM output per item within a configured retry cap. The final evaluation artifact shall separate `accepted_results`, `rejected_attempts`, `unevaluated_items`, and `budget_exhausted_items`; budget exhaustion is not an accepted LLM judgment. Accepted evaluations shall include item ID or item key, body hash, head SHA, decision, reason, recommended action, accepted timestamp, and attempt count. Allowed accepted decisions shall be `valid`, `invalid`, `out_of_scope`, and `needs_user_judgment`. Previously accepted unchanged evaluations may be reused from feedback state, but they shall still appear exactly once in `accepted_results`. Remediation may consume only `accepted_results` with `decision=valid`.

**Behavior**:

- GIVEN: three CodeRabbit feedback items
- WHEN: evaluation runs
- THEN: the artifact contains exactly three accepted validated evaluations, or records rejected/unevaluated/budget-exhausted items and returns non-success without feeding remediation

**Why This Matters**: The LLM can judge feedback, but the engine must ensure shape, identity, and completeness.

### REQ-PRFU-012: Evaluation JSON Validation

**Full Text**: The system shall validate every LLM feedback evaluation result against the required schema before using it for remediation or commenting. Malformed JSON shall not be treated as a valid evaluation. Unknown decisions shall not be silently accepted. Mismatched item ID, item key, body hash, or head SHA shall not be accepted. Missing reasons shall not be accepted for invalid, out-of-scope, or needs-user-judgment decisions.

**Behavior**:

- GIVEN: malformed or mismatched LLM output
- WHEN: validation runs
- THEN: the result is rejected and retried or routed non-success

**Why This Matters**: Malformed LLM output must not corrupt the deterministic pipeline.

## Implementation Tasks

### Files to Modify

- `src/engine/executors/feedback_eval.rs`
  - Implement per-item prompt creation, invocation adapter, JSON validation, retry cap, evaluation reuse, and output aggregation.
  - Write `feedback-evaluations.json` with separate `accepted_results`, `rejected_attempts`, `unevaluated_items`, and `budget_exhausted_items`; never encode budget exhaustion as an accepted decision.
  - Return StepOutcome `success` only when every current item has exactly one accepted validated result; otherwise return StepOutcome `fatal` after writing the audit artifact.
  - Invoke exactly one `FeedbackEvaluationRequest` per item per attempt; a request must contain one item only and must include the expected item ID, stable marker key, body hash, head SHA, and repository/PR binding.
  - Reject response arrays, batch responses, responses with multiple/extra item IDs, wrong item ID, wrong stable marker key, wrong body hash, wrong head SHA, unknown decisions, malformed JSON, and missing required reasons.
  - Reuse prior accepted unchanged evaluations without invoking the LLM adapter and emit each reused evaluation exactly once in `accepted_results`. The only reusable source is binding-validated `coderabbit-feedback-state.json.state_entries[*].accepted_evaluation` or the Phase 02-documented accepted-evaluation history index keyed by stable marker key, body hash, head SHA, repository, and PR. After accepting a newly validated LLM evaluation, atomically persist it back to that state/history source before returning success so the next same-head/body run can reuse it. Ambiguous, malformed, duplicate, stale, or unbindable prior state for a current item is fatal and must be recorded in `feedback-evaluations.json` rather than falling through to a fresh LLM call.
  - Retry malformed or mismatched LLM output internally per item with `max_attempts_per_item = 3` (initial attempt plus two retries); do not add or require a workflow self-loop for evaluator retries.

## Verification Commands

```bash
cargo test --test github_api_contract_tests -- github_api_contract && cargo run --bin github_api_contract_validator -- project-plans/coderabbit/analysis/github-api-contract.md tests/fixtures/github_api_contract
cargo test --test github_pr_followup_executor_tests -- feedback_evaluation
cargo test --test github_pr_followup_executor_tests -- feedback_evaluator_command_shell_safety
grep -R "@requirement:REQ-PRFU-011" src/engine tests | wc -l
cargo test --test pr_followup_marker_audit_tests -- p09_markers_cover_all_touched_items
! grep -R "@pseudocode lines X-Y\|@pseudocode TBD\|@pseudocode placeholder\|TODO API\|json_path TBD\|fixture TBD\|assertion TBD" src/engine/executors/feedback_eval.rs tests project-plans/coderabbit/analysis
```

## Success Criteria

- Exactly one accepted result per current item.
- Malformed output behavior is fixture-tested.
- Feedback evaluation tests cover first-run LLM invocation and persistence to feedback state/history; same head/body reuse without any LLM call; changed body or changed head forcing re-evaluation and state update; and ambiguous/malformed prior state producing fatal output rather than reuse or silent overwrite.
- Shell-safety gate passes for any evaluator adapter command/process invocation; feedback bodies and LLM text are passed by structured input/file/argv, not shell-interpolated strings.
- Reused evaluations still appear once in aggregate output.

---

# Phase 10: Remediation Plan Aggregation Implementation

## Phase ID

`PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10`

## Prerequisites

- Required: Phase 09 completed
- Verification: `test -f project-plans/coderabbit/.completed/P09 && grep -R "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09" src/engine && cargo test --test github_api_contract_tests -- github_api_contract && cargo run --bin github_api_contract_validator -- project-plans/coderabbit/analysis/github-api-contract.md tests/fixtures/github_api_contract`
- Pseudocode: exact remediation plan aggregation lines from `project-plans/coderabbit/analysis/pseudocode/remediation-and-marker.md`.

## Requirements Implemented (Expanded)

### REQ-PRFU-013: Deterministic Remediation Plan Aggregation

**Full Text**: The system shall aggregate CI failures and accepted validated feedback evaluations into a deterministic `pr-remediation-plan.json`. The remediation plan shall place CI failures and only `feedback-evaluations.json.accepted_results` items with `decision=valid` into `must_fix`. Accepted `invalid` and `out_of_scope` feedback items go into `mark_invalid`; accepted `needs_user_judgment`, watch timeouts, unknown persistent check states, unevaluated items, budget-exhausted items, and unresolved ambiguity go into `needs_user_judgment` and return StepOutcome `fatal` through the non-success terminal route.

## Implementation Tasks

- Implement `PrRemediationPlanExecutor` in `src/engine/executors/pr_remediation.rs` using only binding-valid `pr.json`, `ci-failures.json`, `coderabbit-feedback.json`, and `feedback-evaluations.json` read through `PrFollowupArtifactStore`.
- Route semantic `clean` as StepOutcome `success`, semantic `needs_remediation` as StepOutcome `fixable`, and semantic `blocked_needs_user_judgment`/`fatal` as StepOutcome `fatal`.
- Ensure `pending_or_unknown` evidence and accepted `needs_user_judgment` evaluations never become `must_fix`.
- Before returning `success` for a clean plan, create durable `pending-feedback-marker-actions.json` entries for every accepted `invalid` or `out_of_scope` feedback evaluation in `mark_invalid`, even when there are no `must_fix` items and no remediation output will be produced. These actions must be written through `PrFollowupArtifactStore` with canonical/history snapshots, source binding, deterministic marker keys, action kind, reason/evidence fields, and `remediation_output_head=none`. This keeps P10 responsible for invalid/out-of-scope clean-plan marker work; P11 remains responsible for valid fixed items after remediation and must not be required before invalid/out-of-scope marker actions exist.

- The implementation plan's canonical remediation-result status enum supersedes older overview examples. Add grep/schema tests preventing `needs_user_judgment` from appearing as a remediation result status while still allowing it as an evaluation decision and remediation-plan state.

### Required Code Markers

Every new or materially modified production function, private helper, test function, struct, enum, trait, impl block, and module-level constant involved in remediation plan aggregation must include adjacent P10 markers; purely mechanical re-exports are exempt.


## Verification Commands

```bash
cargo test --test github_api_contract_tests -- github_api_contract && cargo run --bin github_api_contract_validator -- project-plans/coderabbit/analysis/github-api-contract.md tests/fixtures/github_api_contract
cargo test --test github_pr_followup_executor_tests -- remediation_plan
cargo test --test github_pr_followup_executor_tests -- remediation_plan_invalid_out_of_scope_only_writes_pending_marker_actions_and_returns_success
cargo test --test github_pr_followup_executor_tests -- pending_marker_actions_invalid_out_of_scope_have_no_remediation_output_head_and_do_not_duplicate_on_retry

cargo test --test github_pr_followup_executor_tests -- remediation_plan_needs_user_judgment_returns_fatal
cargo test --test github_pr_followup_executor_tests -- remediation_result_status_enum_rejects_needs_user_judgment
cargo test --test pr_followup_marker_audit_tests -- p10_markers_cover_all_touched_items
! grep -R "@pseudocode lines X-Y\|@pseudocode TBD\|@pseudocode placeholder\|TODO API\|json_path TBD\|fixture TBD\|assertion TBD" src/engine/executors/pr_remediation.rs tests project-plans/coderabbit/analysis
```
- A plan containing only invalid/out-of-scope feedback is a successful clean plan only after durable pending marker actions are persisted with `remediation_output_head=none`; marker consumption and retry/resume idempotency have tests proving no duplicate actions are created.


## Success Criteria

- Clean plans are clean only under REQ-PRFU-013.
- Plan artifacts are written through `PrFollowupArtifactStore` with canonical/history snapshots and binding fields.
- The expected-failing manifest removes plan tests made pass and contains only later-phase expected failures.

---

# Phase 11: Remediation Result Validator and Caps Implementation

## Phase ID

`PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P11`

## Prerequisites

- Required: Phase 10 completed
- Verification: `test -f project-plans/coderabbit/.completed/P10 && grep -R "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10" src/engine`
- Pseudocode: exact remediation result validation/cap lines from `project-plans/coderabbit/analysis/pseudocode/remediation-and-marker.md`.

## Requirements Implemented (Expanded)

### REQ-PRFU-014: Structured Remediation Result

**Full Text**: The system shall require a structured `pr-remediation-result.json` artifact after any PR remediation LLM step. The canonical result status enum is exactly `fixed | changed | already_satisfied | not_reproduced | not_fixed | skipped | failed`. `needs_user_judgment` is not a remediation result status; it remains valid only as evaluation/plan state. A malformed non-empty remediation result is rejected. Structurally valid `not_fixed`, `skipped`, or `failed` entries are valid-but-unsuccessful and loop only while artifact-backed caps remain.

## Implementation Tasks

- Implement `PrRemediationResultExecutor` validation, binding checks, same-head validator retry caps, same-head unsuccessful/no-change remediation caps, and pending marker action creation/update for fixed and invalid/out-of-scope items before push can change PR head.
- Accept validator success only when every `must_fix` item has `fixed`, `changed`, or deterministic-evidence-backed `already_satisfied`/`not_reproduced` tied to the same input head.
- Reject unknown statuses, including `needs_user_judgment`, in result artifacts, prompts, fixtures, and schema tests.
- Exhausted caps write failure artifacts and route to `post_pr_failure_terminal` as fatal/non-success, never `RunOutcome::Abandoned`.

## Verification Commands

```bash
cargo test --test pr_followup_marker_audit_tests -- p11_markers_cover_all_touched_items
! grep -R "@pseudocode lines X-Y\|@pseudocode TBD\|@pseudocode placeholder\|TODO API\|json_path TBD\|fixture TBD\|assertion TBD" src/engine tests project-plans/coderabbit/analysis
cargo test --test github_pr_followup_executor_tests -- remediation_result
cargo test --test github_pr_followup_executor_tests -- remediation_validator
cargo test --test github_pr_followup_executor_tests -- remediation_validator_status_enum
cargo test --test github_pr_followup_executor_tests -- remediation_validator_rejects_unknown_status_outside_canonical_enum
cargo test --test github_pr_followup_executor_tests -- remediation_validator_same_head_no_change_attempt_cap_reaches_post_pr_failure_terminal_failure_not_abandoned
```

## Success Criteria

- Marker cannot consume incomplete remediation result.
- `not_fixed`, `skipped`, `failed`, missing, unknown-status, or explanation-only results cannot reach push success.
- The status vocabulary gate rejects `needs_user_judgment` as a remediation result status.
- Completion marker cites exact pseudocode lines and updates `expected-failing-tests.json`.

---

# Phase 12: PR Follow-up Llxprt Remediation Wrapper Implementation

## Phase ID

`PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12`

## Prerequisites

- Required: Phase 11 completed
- Verification: `test -f project-plans/coderabbit/.completed/P11 && test -f project-plans/coderabbit/analysis/llxprt-remediation-seam.md`
- Pseudocode: exact llxprt remediation wrapper lines from `project-plans/coderabbit/analysis/pseudocode/remediation-and-marker.md`.

## Implementation Tasks

- Implement `PrFollowupRemediationExecutor` as the only post-PR llxprt invocation path. It owns a separate invocation implementation for PR follow-through, even when it reuses helper functions from `src/engine/executors/llxprt.rs`.
- The invocation result must expose argv, exit status or signal, timeout/spawn class, bounded stdout/stderr, full stdout/stderr/log paths, success-file presence, changed-path evidence, artifact binding fields, and enough metadata to write `pr-remediation-llxprt-run.json` plus validator-readable failure/result artifacts before routing.
- Add a concrete invocation seam for PR follow-through, for example a `PrFollowupLlxprtCommandRunner` trait returning an owned `LlxprtInvocationResult` (or equivalently named owned result type). The owned result must include process evidence (`argv`, working directory, exit status or signal, timeout/spawn classification, stdout/stderr excerpts, full log paths, success-file/result-file evidence, and changed-path evidence) independent of `StepOutcome`. Tests must inject fake owned results and must fail if the implementation simply calls `LlxprtExecutor::execute` and infers product success/failure from its `StepOutcome` without capturing the process evidence needed for `pr-remediation-llxprt-run.json` and validator-readable artifacts. Reusing helper functions from `llxprt.rs` is allowed only below this seam when the owned result remains complete.

- Render the exact remediation prompt contract: fix only `pr-remediation-plan.json.must_fix`; do not fix `mark_invalid`, `out_of_scope`, or `needs_user_judgment`; write `pr-remediation-result.json`; use only canonical statuses `fixed | changed | already_satisfied | not_reproduced | not_fixed | skipped | failed`; include structured evidence; no free-form-only completion.
- For timeout, spawn failure, fatal llxprt failure, retryable llxprt failure, and success-without-result, write `pr-remediation-llxprt-run.json` plus any validator-readable failure/result artifact before returning. Return wrapper `success` when the validator should classify the artifact; return wrapper `fatal` only when no validator-readable artifact exists or terminal logging must run.

## Verification Commands

```bash
cargo test --test pr_followup_marker_audit_tests -- p12_markers_cover_all_touched_items
! grep -R "@pseudocode lines X-Y\|@pseudocode TBD\|@pseudocode placeholder\|TODO API\|json_path TBD\|fixture TBD\|assertion TBD" src/engine tests project-plans/coderabbit/analysis
cargo test --test github_pr_followup_executor_tests -- pr_followup_remediation_wrapper
cargo test --test github_pr_followup_executor_tests -- remediate_pr_followup_prompt_contract
cargo test --test github_pr_followup_executor_tests -- pr_followup_llxprt_wrapper_uses_owned_invocation_result_not_step_outcome_inference

```

## Success Criteria
- A concrete owned invocation-result/command-runner seam exists and tests prove process evidence comes from that seam rather than from `LlxprtExecutor::execute` `StepOutcome` inference.


- `pr-remediation-llxprt-run.json` records process evidence and changed-path evidence for success and failure classes.
- Wrapper routing never depends on raw llxprt process success as product success.
- Completion marker cites exact pseudocode lines and updates `expected-failing-tests.json`.

---

# Phase 13: Post-PR Local Verification Executor Implementation

## Phase ID

`PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13`

## Prerequisites

- Required: Phase 12 completed
- Verification: `test -f project-plans/coderabbit/.completed/P12 && grep -R "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12" src/engine`
- Pseudocode: exact post-PR test executor lines from `project-plans/coderabbit/analysis/pseudocode/remediation-and-marker.md`.

## Implementation Tasks

- Implement `RunPostPrTestsExecutor` with argv/command-ID configuration only, injected safe command runner, bounded stdout/stderr, full log artifacts, binding validation, `post-pr-test-result.json`, and artifact-backed verification retry caps.
- Missing commands, empty argv, shell-string-only commands, unrecognized command IDs, unsafe working directories, malformed retry caps, and missing own params are fatal/config artifacts where possible.

## Verification Commands

```bash
cargo test --test pr_followup_marker_audit_tests -- p13_markers_cover_all_touched_items
! grep -R "@pseudocode lines X-Y\|@pseudocode TBD\|@pseudocode placeholder\|TODO API\|json_path TBD\|fixture TBD\|assertion TBD" src/engine tests project-plans/coderabbit/analysis
cargo test --test github_pr_followup_executor_tests -- run_post_pr_tests
cargo test --test github_pr_followup_executor_tests -- post_pr_tests_and_push_shell_safety
```

## Success Criteria

- Local verification success is required before push.
- Test failures loop only while artifact-backed caps remain; infrastructure/configuration failures route fatal.
- Completion marker cites exact pseudocode lines and updates `expected-failing-tests.json`.

---

# Phase 14: Remediation Change Push Executor Implementation

## Phase ID

`PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14`

## Prerequisites

- Required: Phase 13 completed
- Verification: `test -f project-plans/coderabbit/.completed/P13 && grep -R "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13" src/engine`
- Pseudocode: exact push executor lines from `project-plans/coderabbit/analysis/pseudocode/remediation-and-marker.md`.

## Implementation Tasks

- Implement `PushRemediationChangesExecutor` with explicit step type `push_remediation_changes`, injected safe command runner, deterministic working-tree inspection, deterministic staging exclusions, deterministic commit creation, push command construction, bounded stdout/stderr, full logs, binding validation, remote-head verification, and `push-remediation-result.json` canonical/history writes.
- `no_change` and `no_change_excluded_only` are success only after verifying remote PR head already matches expected local head and defensively confirming acceptable `must_fix` evidence. Successful commit+push is success only after remote PR head equals committed head.

## Verification Commands

```bash
cargo test --test pr_followup_marker_audit_tests -- p14_markers_cover_all_touched_items
! grep -R "@pseudocode lines X-Y\|@pseudocode TBD\|@pseudocode placeholder\|TODO API\|json_path TBD\|fixture TBD\|assertion TBD" src/engine tests project-plans/coderabbit/analysis
cargo test --test github_pr_followup_executor_tests -- push_remediation_changes
cargo test --test github_pr_followup_executor_tests -- post_pr_tests_and_push_shell_safety
```

## Success Criteria

- Successful remediation creates and pushes a deterministic commit before routing back to `capture_pr_identity`.
- No-change and excluded-only paths verify remote head and cannot mask unsuccessful remediation.
- Completion marker cites exact pseudocode lines and updates `expected-failing-tests.json`.

---


---

# Phase 15: Feedback Marker Implementation

## Phase ID

`PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15`

## Prerequisites

- Required: Phase 14 completed
- Verification: `grep -R "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14" src/engine`
- Pseudocode: `project-plans/coderabbit/analysis/pseudocode/remediation-and-marker.md`

## Requirements Implemented (Expanded)

### REQ-PRFU-015: Deterministic Feedback Marking

**Full Text**: The system shall mark CodeRabbit feedback using structured evaluation, `pr-remediation-result.json`, and `pr-feedback-marker-report.json` artifacts, not free-form LLM text. For fixed valid items, the system shall comment with recorded action taken and resolve the review thread when possible. For invalid or out-of-scope items, the system shall comment with the recorded reason and resolve the review thread when policy allows. For needs-user-judgment items, the system shall not resolve automatically and shall route non-success rather than pretending completion. Comment bodies shall be generated from deterministic templates and structured artifact fields. The marker report shall record skipped marker actions and skipped reasons when policy intentionally avoids comment or resolution.

**Behavior**:

- GIVEN: marker input artifacts
- WHEN: marker runs
- THEN: comments/resolution attempts are made from deterministic templates and recorded

**Why This Matters**: Feedback handling should be auditable and not invented by the LLM.

### REQ-PRFU-016: Marker Idempotency

**Full Text**: The system shall avoid duplicate feedback comments and duplicate review-thread resolution attempts across retries and resumed runs. `pr-feedback-marker-report.json` shall record posted comment IDs, posted comment URLs, body hashes, resolved thread IDs, and timestamps. Before posting, the system shall detect whether the same marker action has already been completed for the same item/body/head SHA.

**Behavior**:

- GIVEN: marker step is retried
- WHEN: the same item/body/head was already marked
- THEN: no duplicate comment is posted

**Why This Matters**: Workflow retries must not spam PRs.

### REQ-PRFU-017: Shell Safety for GitHub Text

**Full Text**: The system shall not interpolate untrusted GitHub text or LLM text directly into shell-quoted `gh` invocations. Comment bodies shall be passed through files or process arguments that avoid shell interpretation. Raw CodeRabbit bodies shall never be embedded in shell command strings. Backticks and command substitutions in review text shall not be executable by the shell.

**Behavior**:


- GIVEN: CodeRabbit text contains shell metacharacters
- WHEN: marker comments
- THEN: the text cannot be executed by the shell

**Why This Matters**: GitHub text is untrusted input.

## Implementation Tasks

### Files to Modify

  - Consume P10-created invalid/out-of-scope pending marker actions even when there is no `pr-remediation-result.json`; these actions have `remediation_output_head=none` and must not require remediation output. Valid/fixed marker actions after remediation remain P11/P15 responsibility and require validator-approved remediation evidence.

- `src/engine/executors/github_feedback.rs` and shared `pr_followup_types.rs`
  - Implement marker templates, idempotency checks, GraphQL/REST command invocations, fallback recording, marker parser, local/remote conflict policy, and `pr-feedback-marker-report.json` writing.
  - Marker idempotency must use both local `pr-feedback-marker-report.json` artifacts and remote marker comments. Each deterministic marker comment must include a stable hidden HTML comment marker using syntax `<!-- luther-pr-followup marker_key=<stable_marker_key> source_head=<source_head_sha> remediation_output_head=<remediation_output_head_sha_or_none> body=<body_hash> action=<action_kind> run_id=<run_id> -->` so retries and resumed runs can discover already-completed actions even if local artifacts are missing. Do not use visible `@plan` marker text in GitHub comments, and do not emit a single ambiguous `head` marker field.


  - Consume `pending-feedback-marker-actions.json` (or the Phase 02 documented equivalent) in addition to current `coderabbit-feedback.json` and `feedback-evaluations.json`. Pending actions from prior remediation history remain authoritative even when current-head feedback is ready/clean/empty after a remediation push.
  - Update each pending action status atomically in canonical/history state after comment and resolution attempts; keep comment and resolution idempotency keys separate so a retry after comment success/resolution failure does not repost the comment.


Carry-forward tests are mandatory in this phase if not already passing from P04: head A feedback fixed by remediation and pushed to head B must still be commented/resolved from pending marker actions even when current head B feedback is empty/clean, and a retry must not duplicate the original comment or resolution attempt. Tests must also prove invalid/out-of-scope actions use `remediation_output_head=none` and do not require remediation output, remote-marker-only resume works after a head change, and the same stable item on a later head does not collide with an earlier carried-forward marker because `source_head`, `remediation_output_head`, body hash, action, and run ID are part of the marker/idempotency key.

### REQ-PRFU-026 Marker Partial-Failure Policy

Marker implementation must use this deterministic policy table; it is not optional or left to LLM judgment:

| Item class | Comment required? | Resolution required? | If resolution unavailable | If comment succeeds but resolution fails | Partial handled state | Retry/idempotency requirement |
|------------|-------------------|----------------------|---------------------------|------------------------------------------|-----------------------|-------------------------------|
| valid + fixed by `pr-remediation-result.json` | yes, with action/evidence | yes when a resolvable review thread ID exists | record `resolution_unavailable`, keep marker state `partial`, return `fatal` unless config explicitly marks resolution unsupported for that source | record posted marker comment ID/body hash, do not repost comment on retry, retry resolution only, return `fatal` through terminal if cap/exhausted | `comment_posted_resolution_pending` | Remote hidden marker plus local report suppresses duplicate comments; retry resumes at resolution. |
| invalid | yes, with deterministic reason from accepted evaluation | yes when thread ID exists and policy allows resolving invalid findings | record `resolution_unavailable`; if no resolvable thread exists, state may be `handled_comment_only`; if mutation is expected but unavailable, return `fatal` | do not repost comment; retry resolution only | `comment_posted_resolution_pending` or `handled_comment_only` | Comment and resolution actions have separate idempotency keys. |
| out_of_scope | yes, with deterministic out-of-scope reason | yes only when policy explicitly allows resolving out-of-scope findings; otherwise skip with reason | record `resolution_skipped_by_policy` and may be fully handled after comment | if resolution was required, same retry behavior as invalid; if skipped by policy, no failure | `handled_comment_only` or `comment_posted_resolution_pending` | Skipped resolution is persisted so retries do not attempt it unless policy changes. |
| needs_user_judgment | optional escalation comment only when configured; never claim fixed | no automatic resolution | record `resolution_skipped_needs_user_judgment` | not applicable unless escalation comment fails | `unhandled_needs_user_judgment` | Routes fatal/non-success before clean completion; retries must not duplicate escalation comments. |

Marker report fields must distinguish `posted_comments`, `resolved_threads`, `skipped_actions`, `partial_actions`, `retryable_actions`, and `failed_actions`. Marker success is allowed only when every required action is complete or explicitly skipped by policy. A comment-posted/resolution-failed sequence is partial, not success; retries must use hidden remote markers and local report state to avoid duplicate comments while retrying only unresolved actions. Tests must cover every table row, unavailable resolution, mutation failure after successful comment, retry after partial state, remote-marker-only resume, local-report-only resume, conflicting remote markers, and marker fatal routing through `post_pr_failure_terminal`.


```bash
cargo test --test github_pr_followup_executor_tests -- marker_consumes_invalid_out_of_scope_pending_actions_with_no_remediation_output_head
cargo test --test github_pr_followup_executor_tests -- marker_retry_resume_does_not_duplicate_invalid_out_of_scope_pending_actions
```


## Verification Commands

```bash
cargo test --test pr_followup_marker_audit_tests -- p15_markers_cover_all_touched_items
! grep -R "@pseudocode lines X-Y\|@pseudocode TBD\|@pseudocode placeholder\|TODO API\|json_path TBD\|fixture TBD\|assertion TBD" src/engine tests project-plans/coderabbit/analysis
cargo test --test github_pr_followup_executor_tests -- marker
cargo test --test github_pr_followup_executor_tests -- shell_safety
grep -R "@requirement:REQ-PRFU-017" src/engine tests | wc -l
```

## Success Criteria

- Marker tests pass.
- Duplicate marker actions are prevented.
- Shell safety test passes.

---

# Phase 16: Workflow Integration TDD

## Phase ID

`PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16`

## Prerequisites

- Required: Phase 15 completed
- Verification: `grep -R "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15" src/engine`

## Requirements Implemented (Expanded)

### REQ-PRFU-018: Workflow-Representable Outcomes

**Full Text**: The system shall return only step outcomes that can be routed by the current workflow transition schema: `success`, `fixable`, `retryable`, `fatal`, and `abandon`. Workflow TOML must not use custom routable outcomes such as `passed`, `ready`, `clean`, `valid`, `invalid`, `continue`, or `needs_user_judgment`; those richer states must live inside artifacts and be classified deterministically. Watch timeouts, readiness timeouts, unknown states, API failures, malformed artifacts, and needs-user-judgment conditions shall route to `post_pr_failure_terminal`, whose executor writes a terminal artifact and returns `fatal` so the run cannot finish as `RunOutcome::Success` through a successful cleanup/logging step. The system shall not require ambiguous branching such as one outcome selecting between multiple next steps without an intervening deterministic classifier artifact.

**Behavior**:

- GIVEN: post-PR step outcomes
- WHEN: transitions resolve
- THEN: every outcome has a representable deterministic route

**Why This Matters**: The current engine cannot route invented semantic outcomes, and successful logging must not mask post-PR failure as overall success.

### REQ-PRFU-020 and REQ-PRFU-020A

**Full Text**: Graph and fake end-to-end tests must prove executor reachability, no direct `create_pr -> log_completion`, allowed StepOutcome-only routing (`success`, `fixable`, `retryable`, `fatal`, `abandon`), no custom routable semantic outcomes, post-remediation loop back through PR identity capture with artifact-backed caps, `validate_remediation_result success -> run_post_pr_tests -> push_remediation_changes`, local verification failure looping back to remediation with artifact-backed caps, and all bounded non-success paths reaching `post_pr_failure_terminal` with overall fatal/non-success completion.

**Behavior**:

- GIVEN: the workflow TOML
- WHEN: graph tests inspect transitions
- THEN: post-PR follow-through is reachable and bounded

**Why This Matters**: The workflow must actually use the engine support.


  - Add the exact named graph cut assertion: compute all steps reachable from `capture_pr_identity` and assert the reachable set does not include `abandon_and_log`, `generate_pr_description`, or `create_pr`; assert every fatal/retryable post-PR route targets `post_pr_failure_terminal`; and assert `post_pr_failure_terminal` has no outgoing transitions.

## Implementation Tasks

### Files to Modify

- `tests/e2e_workflow_integration.rs`
  - Complete and update the Phase 04 graph tests for exact post-PR step IDs, outcomes, transitions, fatal routes, evaluator no-self-loop behavior, validator fixable loop, post-PR guard cap, and no ambiguous duplicate outcome branches after all executors exist. Phase 04 owns the first compiling/failing graph TDD against the current TOML; Phase 16 expands those tests for full workflow integration before TOML implementation. Require TOML invariants that `post_pr_failure_terminal` has no outgoing transitions, no post-PR step routes to `abandon_and_log`, and every post-PR non-success route targets `post_pr_failure_terminal`. Modify the existing e2e fatal-cleanup/abandon tests, rather than leaving them broad, so every `fatal -> abandon_and_log` expectation is explicitly scoped to pre-PR-only behavior and cannot match any step reachable from `capture_pr_identity`.
  - Add graph and fake-runner tests proving no post-PR reachable path reaches `abandon_and_log`; every post-PR fatal ends as `RunOutcome::Failure { step_id: "post_pr_failure_terminal", ... }` (or project-equivalent failure payload); and a negative graph test fails if any post-PR fatal route can reach successful cleanup/logging (`abandon_and_log`, `log_completion`, or equivalent success-returning cleanup).
  - Add a guard test that enumerates all post-PR step IDs from the routing contract and fails if any route to `abandon_and_log`, even indirectly through a duplicate fatal/retryable transition.
  - Add a named duplicate-transition test group such as `post_pr_reachable_transitions_are_unique_by_from_and_effective_condition`. It must compute every transition reachable from `create_pr` success into the post-PR tail and every step reachable from `capture_pr_identity`, normalize each transition to `(from_step_id, effective_condition)` where a missing/omitted condition is treated as `success`, and fail if any pair appears more than once. Include negative fixtures containing duplicate `create_pr success` transitions to both `log_completion` and `capture_pr_identity`, duplicate `watch_pr_checks fatal` transitions, and duplicate `build_remediation_plan success` transitions to prove the test catches explicit duplicate conditions and missing-condition-as-success duplicates.
  - Add P16 graph/executor-contract assertions that `abandon` is not used in the post-PR tail: no route reachable from `capture_pr_identity` may have condition `abandon`, and fake post-PR executor tests must fail if any post-PR executor returns `StepOutcome::Abandon`. Existing pre-PR abandon behavior may remain tested separately.

  - Add graph tests asserting every post-PR step has a TOML-configured `step_order_index`, indexes are unique, and indexes are monotonic along the primary post-PR route.
  - Add graph tests asserting every post-PR step has exactly one TOML-configured `artifact_root`, no post-PR step uses `artifact_dir`, configured artifact/result paths are within `artifact_root`, and every post-PR step has a TOML-configured `step_order_index`, with indexes unique and monotonic along the primary post-PR route.


  - Add TOML-level routing assertions for the P17 step contract: `run_post_pr_tests`, `remediate_pr_followup`, and `push_remediation_changes` exist; `watch_pr_checks success -> collect_ci_failures`; `watch_pr_checks fixable -> collect_ci_failures`; `watch_pr_checks fatal -> collect_ci_failures` with no optional direct terminal route; `collect_ci_failures success -> collect_coderabbit_feedback`; `collect_ci_failures fatal -> post_pr_failure_terminal`; `build_remediation_plan success -> mark_coderabbit_feedback`; `mark_coderabbit_feedback success -> log_completion`; `mark_coderabbit_feedback fatal -> post_pr_failure_terminal`; no other marker outcomes/routes exist unless this plan is explicitly expanded; `remediate_pr_followup success -> validate_remediation_result`; `remediate_pr_followup fatal -> post_pr_failure_terminal`; no `remediate_pr_followup retryable` route exists unless the wrapper outcome contract is explicitly expanded in this plan; `validate_remediation_result success -> run_post_pr_tests`; `run_post_pr_tests success -> push_remediation_changes`; `run_post_pr_tests fixable -> remediate_pr_followup`; `push_remediation_changes success -> capture_pr_identity`; all fatal/retryable post-PR failures route to `post_pr_failure_terminal`; no post-PR route points to `generate_pr_description` or `create_pr`. Graph/unit tests must prove wrapper `success` is used for validator-readable failure artifacts and wrapper `fatal` is used only for terminal paths.
- `tests/smoke_test.rs`
  - Update expected step list and transition expectations.
- `tests/pr_followup_workflow_integration.rs`
  - Add fake end-to-end workflow tests before TOML implementation using fake GitHub/gh fixtures and fake llxprt executor outputs.
  - Scenarios must include clean success, all-terminal CI-failed remediation, failed+pending collection-then-terminal, failed+unknown collection-then-terminal, CodeRabbit-valid remediation, empty-not-ready fatal, unknown-timeout fatal without concrete failures, malformed non-empty remediation result rejected, local verification failure looping to remediation, invalid/out-of-scope-only feedback where `build_remediation_plan success -> mark_coderabbit_feedback -> log_completion`, marker partial failure reaching `post_pr_failure_terminal` with `RunOutcome::Failure`, and marker retry idempotency.
  - Include the pending-marker carry-forward scenario: head A feedback fixed and pending marker actions written; remediation push changes to head B; current feedback on B is ready/clean/empty; marker still comments/resolves the original item; retry/resume does not duplicate comment or resolution.

  - Use fake clock/sleeper for readiness/check budgets; no test may wait for real five-minute intervals.



## Verification Commands

```bash
cargo test --test pr_followup_marker_audit_tests -- p16_markers_cover_all_touched_items
! grep -R "@pseudocode lines X-Y\|@pseudocode TBD\|@pseudocode placeholder\|TODO API\|json_path TBD\|fixture TBD\|assertion TBD" src/engine tests project-plans/coderabbit/analysis
cargo test --test e2e_workflow_integration -- post_pr --list
cargo test --test e2e_workflow_integration -- llxprt_dry_run_step_list
python3 project-plans/coderabbit/analysis/validate-expected-failing-tests.py project-plans/coderabbit/analysis/expected-failing-tests.json
python3 project-plans/coderabbit/analysis/verify-expected-failing-tests.py --manifest project-plans/coderabbit/analysis/expected-failing-tests.json --test-binary e2e_workflow_integration --test-binary pr_followup_workflow_integration
# Expected during P16 TDD: required dry-run/step-list and fake E2E coverage must be non-ignored and listed, then the expected-failure verifier must run the expected-failing graph/fake integration tests and prove failures exactly match the manifest. Do not use direct failing `cargo test --test e2e_workflow_integration -- post_pr` or `cargo test --test pr_followup_workflow_integration` as normal pass gates before P17 updates TOML. Smoke tests are not acceptance gates; any remaining ignored smoke coverage is optional/manual dry-run-only or derives expectations from the same e2e contract fixture, and must be invoked explicitly with --ignored if retained.

```

## Success Criteria

- The P16 expected-failure verifier proves the current-TOML graph/fake E2E failures exactly match `expected-failing-tests.json`: actual tests exist, exact names are enumerated, failures are expected assertion/behavior failures, and there are no compile failures or unexpected failures.

- Tests fail naturally against the current `create_pr -> log_completion` workflow.
- Tests prove the exact routing contract table in this plan is represented by the current schema.
- Tests prove malformed feedback-evaluator output is handled inside the evaluator executor and does not require a workflow self-loop.
- Tests prove `PrFollowupRemediationExecutor` wrapper success always routes to deterministic remediation-result validation, including validator-readable failure artifacts from timeout/spawn/retryable/fatal command cases; wrapper fatal routes directly to `post_pr_failure_terminal`; validator `fixable` loops to the wrapper while the artifact-backed retry cap remains; validator `fatal` routes to `post_pr_failure_terminal`; and a malformed non-empty `pr-remediation-result.json` cannot be accepted.
- Tests prove `validate_remediation_result success -> run_post_pr_tests -> push_remediation_changes -> capture_pr_identity`, and local verification failures loop to remediation while artifact-backed caps remain. The successful remediation scenario must assert `push_remediation_changes` created/pushed the deterministic commit and verified the remote PR head before `capture_pr_identity` runs.
- Tests prove duplicate post-PR transitions are rejected by grouping every transition reachable from `create_pr` success and from `capture_pr_identity` by `(from, effective_condition)` with missing condition normalized to `success`; negative fixtures cover duplicate `create_pr success` routes to both `log_completion` and `capture_pr_identity`, duplicate `watch_pr_checks fatal`, and duplicate `build_remediation_plan success` routes. Tests also prove no post-PR route uses `abandon` and no fake post-PR executor returns `StepOutcome::Abandon`.

- Tests prove the post-PR tail does not route to `generate_pr_description`, `create_pr`, or `abandon_and_log`; only `push_remediation_changes success` may loop back to `capture_pr_identity` for the same PR follow-through cycle. Existing fatal-cleanup e2e tests are migrated so `fatal -> abandon_and_log` is asserted only for pre-PR steps; graph/fake-runner tests assert no path reachable from `capture_pr_identity` reaches `abandon_and_log`, every post-PR fatal terminates as `RunOutcome::Failure { step_id: "post_pr_failure_terminal", ... }`, and a negative graph fixture proves a post-PR fatal route to successful cleanup would fail the suite. The new post-PR guard test enumerates every post-PR step ID.
- Tests prove pending marker actions are carried forward across remediation head changes: original head A feedback still receives the required comment/resolution after head B is clean/empty, and retry/resume is idempotent. Fake E2E tests also prove invalid/out-of-scope-only feedback causes marker success followed by `log_completion`, while marker partial failure reaches `post_pr_failure_terminal` and overall `RunOutcome::Failure`.

- Tests prove `post_pr_failure_terminal` has no outgoing transitions and mock-runner execution ends with fatal/non-success `RunOutcome::Failure` at that step. Tests must include repeated validator-fixable, repeated local-verification-fixable, and repeated push/head-change cap scenarios; every post-PR loop-back transition has an explicit high defensive `max_iterations`; artifact-backed caps fire before engine transition caps or global `max_loops`; and all expected cap outcomes are terminal fatal/non-success through `post_pr_failure_terminal`, never `RunOutcome::Abandoned`.
- Dry-run workflow coverage moves to `tests/e2e_workflow_integration.rs` as non-ignored step-list/dry-run assertions for `llxprt-issue-fix-v1`; the required dry-run step list is a non-ignored e2e gate. Smoke tests are not acceptance gates and must not silently skip or substitute for this coverage. If an ignored smoke test remains, it must be manual dry-run-only coverage invoked explicitly with `--ignored`, or it must derive its expected step list from the same e2e contract fixture; no live GitHub smoke is part of required verification.


- Tests do not require live GitHub network access.

---

# Phase 17: Workflow TOML and Fixture Implementation

## Phase ID

`PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P17`

## Prerequisites

- Required: Phase 16 completed
- Verification: `grep -R "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16" tests`

## Requirements Implemented (Expanded)

### REQ-PRFU-020: Integration Reachability

**Full Text**: The system shall register new PR follow-through executors with the default executor registry and make them reachable from workflow TOML. The `llxprt-issue-fix-v1` workflow shall be able to invoke the executors after PR creation. Workflow graph tests shall prove the PR does not complete immediately after `create_pr`. Workflow graph tests shall prove `create_pr` cannot transition directly to `log_completion` for the PR follow-through path. Workflow graph tests shall prove post-remediation verification and push can loop back to PR identity capture with configured loop caps.

**Behavior**:

- GIVEN: `llxprt-issue-fix-v1` opens a PR
- WHEN: `create_pr` succeeds
- THEN: follow-through begins instead of completing immediately

**Why This Matters**: This is the user-visible workflow improvement.

## Implementation Tasks


### Phase 17 Workflow Config Variable Plan

This plan chooses explicit production and fixture TOML parameter updates rather than unresolved placeholders. Phase 17 must add literal or schema-validated TOML params for every new post-PR value in both production and fixture workflow configs:

| Parameter group | Required TOML params |
|-----------------|----------------------|
| Watch/readiness budgets | `max_check_watch_attempts=12`, `check_watch_interval_seconds=300`, `max_check_watch_duration_seconds=3600`, `max_readiness_observations=6`, `coderabbit_readiness_observation_interval_seconds=300`, `stable_observation_count_required=2` |
| Iteration/retry budgets | `max_post_pr_remediation_iterations=3`, `max_validation_retries=2`, `max_verification_retries=2`, optional `max_push_retries=1` if push retry loop is enabled, plus explicit high defensive transition `max_iterations` values on every post-PR loop-back |
| CodeRabbit identities | default bot logins `coderabbitai[bot]` and `coderabbit[bot]`, plus an explicit configurable list field for additional logins |
| Artifact/result paths | required `artifact_root` on every post-PR step; canonical paths for `pr-remediation-plan.json`, `pr-remediation-result.json`, `post-pr-test-result.json`, `push-remediation-result.json`, `pr-feedback-marker-report.json`, and `post-pr-failure-terminal.json`, all inside `artifact_root`; `artifact_dir` is forbidden in v1 |
| Remediation prompt | exact prompt/template params described in the `remediate_pr_followup` contract, including plan path, result path, input head SHA variable, and optional compatible `success_file` |
| Post-PR test commands | static argv/command-ID list for post-remediation verification; no shell-string-only commands and no LLM-selected commands |
| Staging exclusions | deterministic exclusion values shared with current issue-fix workflow staging rules and consumed by `PushRemediationChangesExecutor` |
| Step ordering | unique integer `step_order_index` on every post-PR step |

P17 tests must fail on any unresolved placeholder in post-PR params, including `${...}` values not supported by the workflow engine, `TODO`, `TBD`, empty strings for required params, missing `artifact_root`, any `artifact_dir` alias, artifact/result paths outside the canonical `artifact_root`, missing budget fields, missing CodeRabbit bot identities, missing post-PR test commands, missing staging exclusions, missing prompt/result paths, and any post-PR loop-back transition that omits explicit defensive `max_iterations`. Fixture TOML and generated JSON must mirror production TOML exactly for these params.

### Files to Modify

- `config/workflows/llxprt-issue-fix-v1.toml`
  - Add post-PR steps and transitions exactly matching the routing contract in this plan using only current engine outcome strings and, for the post-PR tail, only `success`, `fixable`, `retryable`, and `fatal` as routable outcomes. `abandon` remains globally valid for other workflows but must not be used by any post-PR route in this workflow. `watch_pr_checks` success, fixable, and fatal all route to `collect_ci_failures`; direct `watch_pr_checks fatal -> post_pr_failure_terminal` is forbidden. `collect_ci_failures` success routes to `collect_coderabbit_feedback`, and fatal routes to `post_pr_failure_terminal`. `build_remediation_plan success -> mark_coderabbit_feedback`; marker success routes only to `log_completion`; marker fatal routes only to `post_pr_failure_terminal`.
  - Configure a unique integer `step_order_index` on every post-PR step; indexes must be monotonic along the primary route and copied into every artifact written by that step. Do not require an engine `StepContext` ordering change.
  - Configure required `artifact_root` on every post-PR step and ensure every artifact/result path is under that root. Do not configure or accept `artifact_dir` in production or fixture TOML. Executors must construct `PrFollowupArtifactStore` from `artifact_root` only.


  - Remove direct `create_pr -> log_completion` transition.
  - Add deterministic `post_pr_iteration_guard` and the retry-index fields described above rather than relying on implicit engine loop behavior or transition `max_iterations` for remediation-cycle caps. Because the current runner uses global `max_loops` when transition `max_iterations` is omitted, every post-PR loop-back transition must set an explicit high defensive `max_iterations` value that exceeds the artifact-backed cap path exercised by tests.
  - Do not add a workflow self-loop on `collect_coderabbit_feedback`; readiness observations and caps are collector-internal with fake clock/sleeper tests.
  - Do not add an evaluator malformed-output self-loop.
  - Configure `remediate_pr_followup` with the dedicated `pr_followup_remediation` wrapper step type, not raw `llxprt`. Route wrapper `success` to `validate_remediation_result` and wrapper `fatal` to `post_pr_failure_terminal`; wrapper `success` covers validator-readable failure/result artifacts as well as process success. Do not express artifact-readability branching in TOML. Configure validator fixable loop to the wrapper with artifact-backed retry fields and an explicit high defensive `max_iterations`; do not rely on transition `max_iterations` for this cap; use llxprt `success_file` for `pr-remediation-result.json` only if preflight confirmed compatibility.
   - Route `validate_remediation_result` success to `run_post_pr_tests`, then route `run_post_pr_tests` success to `push_remediation_changes`; local verification failures route back to remediation with artifact-backed caps. `run_post_pr_tests` must be configured/static TOML or deterministic executor configuration, not generated by the LLM, and must write `post-pr-test-result.json` with commands/status/stdout/stderr.
   - P17 step contract: `run_post_pr_tests` is a post-remediation verifier that consumes `pr-remediation-result.json`, returns `success` only for passed local verification, returns `fixable` for fixable test failures, and returns `fatal` for infrastructure/configuration failure; `remediate_pr_followup` is the only llxprt remediation step in the post-PR tail and must write/require `pr-remediation-result.json`; `push_remediation_changes` pushes only after validation and local tests pass, then routes to `capture_pr_identity` so the guard can observe the new head.
   - Add TOML/fixture assertions proving no post-PR transition routes to `generate_pr_description` or `create_pr`; post-PR remediation cycles must remain within `remediate_pr_followup -> validate_remediation_result -> run_post_pr_tests -> push_remediation_changes -> capture_pr_identity`.
   - Preserve the P16 duplicate-transition invariant: every post-PR reachable transition is unique by `(from, effective_condition)` after normalizing a missing condition to `success`; no duplicate explicit condition or implicit-success condition may remain in production or fixture TOML.

  - Configure `push_remediation_changes` with the dedicated `push_remediation_changes` executor type, not a generic shell type, so `push-remediation-result.json` is produced deterministically by `PushRemediationChangesExecutor`.
  - Ensure all fatal post-PR outcomes route to `post_pr_failure_terminal`, which returns StepOutcome `fatal`; do not route bounded non-success through successful `abandon_and_log` and do not rely on routing `StepOutcome::Abandon`.



- `tests/fixtures/workflows/valid/llxprt-issue-fix-v1.toml`
  - Mirror production TOML.

- `tests/fixtures/workflows/valid/llxprt-issue-fix-v1.json`
  - Regenerate from TOML using the exact argv recorded in `project-plans/coderabbit/analysis/fixture-regeneration-command.json`. Current known source-of-truth paths are production TOML `config/workflows/llxprt-issue-fix-v1.toml`, fixture TOML `tests/fixtures/workflows/valid/llxprt-issue-fix-v1.toml`, and fixture JSON `tests/fixtures/workflows/valid/llxprt-issue-fix-v1.json`. If preflight confirms `python3 junk/regen_fixtures.py` as canonical, the recorded argv must be `["python3", "junk/regen_fixtures.py"]`; otherwise use the recorded command. Do not hardcode or invent a different script during P17.


### Verification Commands

```bash
cargo test --test pr_followup_marker_audit_tests -- p17_markers_cover_all_touched_items
! grep -R "@pseudocode lines X-Y\|@pseudocode TBD\|@pseudocode placeholder\|TODO API\|json_path TBD\|fixture TBD\|assertion TBD" src/engine tests project-plans/coderabbit/analysis
cp config/workflows/llxprt-issue-fix-v1.toml tests/fixtures/workflows/valid/llxprt-issue-fix-v1.toml
python3 - <<'PY'
import json, subprocess
from pathlib import Path
cmd = json.loads(Path('project-plans/coderabbit/analysis/fixture-regeneration-command.json').read_text())
argv = cmd['argv'] if isinstance(cmd, dict) else cmd
if not isinstance(argv, list) or not argv or not all(isinstance(part, str) and part for part in argv):
    raise SystemExit('fixture regeneration command must be a non-empty argv string list')
subprocess.run(argv, check=True)
PY
cargo test --test e2e_workflow_integration -- post_pr
cargo test --test e2e_workflow_integration -- post_pr_reachable_transitions_are_unique_by_from_and_effective_condition
cargo test --test e2e_workflow_integration -- post_pr_reachable_graph_does_not_use_abandon_condition

cargo test --test e2e_workflow_integration -- llxprt_dry_run_step_list
cargo test --test e2e_workflow_integration -- post_pr_params_have_no_unresolved_placeholders
cargo test --test e2e_workflow_integration -- post_pr_steps_require_artifact_root
cargo test --test e2e_workflow_integration -- post_pr_steps_forbid_artifact_dir_alias

cargo test --test e2e_workflow_integration -- production_and_fixture_llxprt_issue_fix_v1_are_equivalent
cargo test --test pr_followup_workflow_integration


```

## Success Criteria

- Production and fixture TOML have no post-PR `abandon` routes and no duplicate reachable transitions by `(from, effective_condition)`; missing transition condition is tested as `success`.

- Workflow graph tests pass.
- `cargo test --test pr_followup_workflow_integration` passes, proving workflow TOML/fixture implementation cannot complete without the fake end-to-end PR follow-up scenarios.

- Fixture TOML/JSON mirror production TOML, proven by a normalized comparison test that parses `config/workflows/llxprt-issue-fix-v1.toml`, `tests/fixtures/workflows/valid/llxprt-issue-fix-v1.toml`, and `tests/fixtures/workflows/valid/llxprt-issue-fix-v1.json`, normalizes ordering/format-only differences, and fails on any semantic drift.
- Post-PR params contain no unresolved placeholders and include literal/schema-validated budgets, CodeRabbit identities, readiness intervals, explicit defensive loop-back max iterations, staging exclusions, prompt/result paths, and post-PR test commands. Tests assert artifact-backed caps fire before defensive transition caps/global `max_loops`.

---

# Phase 18: Post-Implementation Hardening Only

## Phase ID

`PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P18`

## Prerequisites

- Required: Phase 17 completed
- Verification: `grep -R "capture_pr_identity" config/workflows/llxprt-issue-fix-v1.toml`
- Required earlier coverage: all fake-GitHub workflow end-to-end scenarios are owned exclusively by Phase 16 and were written before TOML implementation.


## Requirements Implemented (Expanded)

### REQ-PRFU-035: No False Clean Completion

**Full Text**: The system shall not complete PR follow-through successfully unless current-head checks are passing, CodeRabbit readiness/stabilization has been established, actionable feedback has been handled, local post-remediation verification has passed before push, and marker actions required by policy have completed or are unnecessary. Phase 18 may add hardening assertions around already-existing tests only. It must not own or introduce the required fake-GitHub end-to-end scenario coverage; those scenarios belong exclusively to Phase 16.

**Behavior**:

- GIVEN: fake GitHub scenarios for pending checks, failed checks, valid CodeRabbit feedback, and clean PR state
- WHEN: the workflow runs through fake executors/commands
- THEN: it completes only for the clean scenario

**Why This Matters**: End-to-end behavior must prove the feature actually works.

## Implementation Tasks

### Files to Create or Modify

- `tests/pr_followup_workflow_integration.rs`
  - Add only hardening assertions that reuse or extend Phase 16-owned fixtures, using fixture scripts and temporary artifact directories. Do not add new fake workflow end-to-end scenario ownership in this phase.
  - Do not defer the core clean, CI-failed, CodeRabbit-valid, empty-not-ready, unknown-timeout, malformed remediation-result, local-verification-failure, terminal-failure, or marker-retry scenarios from Phase 16 to this phase.


## Verification Commands

```bash
cargo test --test pr_followup_marker_audit_tests -- p18_markers_cover_all_touched_items
! grep -R "@pseudocode lines X-Y\|@pseudocode TBD\|@pseudocode placeholder\|TODO API\|json_path TBD\|fixture TBD\|assertion TBD" src/engine tests project-plans/coderabbit/analysis
cargo test --test pr_followup_workflow_integration
```

## Success Criteria

- Phase 16 fake end-to-end scenarios remain passing after TOML implementation.
- Phase 18 adds no first-owned fake workflow end-to-end scenario coverage; required routing, terminal failure, readiness, remediation validation, local verification, and marker retry behavior are already covered by Phase 16.


---

# Phase 19: Security and Idempotency Audit Only

## Phase ID

`PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P19`

## Prerequisites

- Required: Phase 18 completed
- Verification: `grep -R "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P18" tests`

## Requirements Implemented (Expanded)

### REQ-PRFU-016 and REQ-PRFU-017

**Full Text**: Marker actions must be idempotent across retries/resumed runs and untrusted GitHub/LLM text must not be shell-interpolated. Required shell-safety and idempotency behavior is owned by earlier implementation phases: P04 introduces the shared adversarial fixtures and expected-failing tests, P06/P07/P08/P09/P12/P13/P14/P15 make their owned command/marker filters pass, and P16/P17 enforce workflow-level safety. P19 is audit-only: it reruns the already-existing tests and grep checks to confirm coverage remains intact, but it must not introduce new required behavior or first-owned failing tests.

**Behavior**:

- GIVEN: retry/resume and malicious review text cases
- WHEN: marker/commenting runs
- THEN: no duplicate comments are posted and shell metacharacters cannot execute

**Why This Matters**: Automated PR commenting must be safe and non-spammy.

## Implementation Tasks

### Files to Modify

- `src/engine/executors/github_pr.rs`
  - Audit only PR identity/check/CI `Command` invocations owned by this module. Do not add new behavior in P19; if coverage or behavior is missing, update the earliest owner phase (P04 for TDD fixture/test ownership, P06/P07 for implementation) before P19 can complete.
- `src/engine/executors/github_feedback.rs`
  - Audit CodeRabbit collection and marker command invocations.
  - Confirm already-existing tests ensure comment bodies are file-based or argument-based without shell command interpretation.
  - Confirm already-existing tests ensure `pr-feedback-marker-report.json` state is read before actions.
  - Confirm already-existing tests ensure remote marker comments are queried before actions and matched by stable marker key, head SHA, and body hash.
- `src/engine/executors/feedback_eval.rs`
  - Audit LLM adapter command/process invocation for safe argument/file passing.
- `src/engine/executors/pr_remediation.rs`
  - Audit remediation validator, `run_post_pr_tests`, and `push_remediation_changes` command invocations and structured artifact writes.
- `src/engine/executors/pr_followup_artifacts.rs` and `src/engine/executors/pr_followup_types.rs`
  - Audit artifact path construction, binding validation, and shared type serialization/deserialization safety.

- `tests/github_pr_followup_executor_tests.rs`, `tests/workflow_shell_safety_tests.rs`, and workflow graph tests
  - Audit and rerun only already-existing adversarial text/idempotency/security tests. Do not add P19-owned required test cases. If the audit finds a missing shell-safety/idempotency case, stop P19 and return to the earliest insufficient phase: P04 for missing expected-failing coverage/fixtures, P06/P07/P08/P09/P12/P13/P14/P15 for missing executor behavior, or P16/P17 for missing workflow/TOML assertions. After that earlier phase is corrected, rerun P19 as an audit.


## Verification Commands

```bash
cargo test --test pr_followup_marker_audit_tests -- p19_markers_cover_all_touched_items
! grep -R "@pseudocode lines X-Y\|@pseudocode TBD\|@pseudocode placeholder\|TODO API\|json_path TBD\|fixture TBD\|assertion TBD" src/engine tests project-plans/coderabbit/analysis
cargo test --test github_pr_followup_executor_tests -- idempotency
cargo test --test github_pr_followup_executor_tests -- shell_safety
cargo test --test workflow_shell_safety_tests -- production_and_fixture_workflows_use_safe_body_handling
cargo test --test workflow_shell_safety_tests -- coderabbit_text_metacharacters_cannot_execute
cargo test --test workflow_shell_safety_tests -- static_command_allowlist_is_machine_checked
! grep -R "gh .*--body \\\"" src/engine/executors tests config/workflows --include="*.rs" --include="*.toml" --include="*.json"
grep -R -e "--body-file\|api .*--input\|safe_body_file" src/engine/executors tests config/workflows --include="*.rs" --include="*.toml" --include="*.json"

```

## Success Criteria

- Security/idempotency tests pass.
- No raw CodeRabbit body interpolation into shell commands.
- Shell-safety verification is mandatory and command-backed for production and fixture workflow TOML/JSON: unsafe `gh --body` patterns are absent; dynamic/untrusted text uses `--body-file`, safe API input files, or safe argv/file handling; adversarial CodeRabbit text with backticks, `$()`, quotes, newlines, and here-doc delimiters cannot execute; and the static allowlist is checked by tests.

- P19 adds no new required behavior and no P19-owned expected-failing manifest entries; audit gaps are remediated by returning to the earliest insufficient owner phase.

  - Add a shell safety audit note separating new PR follow-through GitHub text paths from pre-existing workflow shell interpolation. The new PR follow-through path must prove comment bodies, CodeRabbit text, and LLM text are passed via files or safe process arguments. Pre-existing workflow shell interpolation discovered during audit must be documented separately as existing risk unless directly touched by this plan; do not conflate it with the new PR follow-through safety gate.

---

# Phase 20: Documentation and Traceability Audit

## Phase ID

`PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P20`

## Prerequisites

- Required: Phase 19 completed
- Verification: `grep -R "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P19" src tests`

## Requirements Implemented (Expanded)

### REQ-PRFU-003: Structured Artifact Contract

**Full Text**: The system shall write machine-readable JSON artifacts for PR identity, check status, CI failures, CodeRabbit feedback, feedback state, feedback evaluations, remediation plans, remediation results, and marker actions. Each JSON artifact shall include `schema_version`. Each artifact shall include enough identifiers to correlate records across workflow iterations. Raw logs may be written as non-JSON files, but their metadata shall be represented in JSON.

**Behavior**:

- GIVEN: users need to inspect PR follow-through
- WHEN: a run completes or fails
- THEN: artifact schemas and paths are documented

**Why This Matters**: Operators need deterministic artifacts for debugging.

## Implementation Tasks

### Files to Create or Modify

- `docs/architecture/pr-follow-through.md`
  - Document engine/workflow behavior and artifact schemas.
- `project-plans/coderabbit/execution-tracker.md`
  - Update phase status.

## Verification Commands

```bash
grep -R "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP" src tests config docs project-plans/coderabbit | wc -l

grep -R "@requirement:REQ-PRFU" src tests | wc -l
```

## Success Criteria

- Documentation describes deterministic/LLM boundaries.
- Plan/requirement markers are present in implementation and tests.

---

# Phase 21: Final Verification

## Phase ID

`PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P21`

## Prerequisites

- Required: Phase 20 completed
- Verification: `grep -R "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P20" docs project-plans/coderabbit`

## Requirements Implemented (Expanded)

### REQ-PRFU-035: No False Clean Completion

**Full Text**: The system shall not complete PR follow-through successfully unless current-head checks are passing, CodeRabbit readiness/stabilization has been established, actionable feedback has been handled, local post-remediation verification has passed before push, marker actions required by policy have completed or are unnecessary, and all timeout/unknown/needs-user-judgment/API-failure paths have routed to fatal/non-success rather than successful cleanup.

**Behavior**:

- GIVEN: the complete implementation
- WHEN: all tests and verification run
- THEN: false-clean scenarios are rejected and clean scenarios pass

**Why This Matters**: This is the end-to-end safety property.

## Verification Commands

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --quiet
cargo build --release --quiet
cargo test --test pr_followup_workflow_integration
cargo test --test e2e_workflow_integration -- llxprt_dry_run_step_list
cargo test --test e2e_workflow_integration -- production_and_fixture_llxprt_issue_fix_v1_are_equivalent

python3 - <<'PY'
import json, subprocess
from pathlib import Path
command_file = Path('project-plans/coderabbit/analysis/final-dry-run-command.json')
data = json.loads(command_file.read_text())
argv = data['argv'] if isinstance(data, dict) else data
if not isinstance(argv, list) or not argv or not all(isinstance(part, str) and part for part in argv):
    raise SystemExit('final dry-run command must be a non-empty argv string list')
subprocess.run(argv, check=True)
PY

```

## Deferred Implementation Detection

```bash
python3 - <<'PY'
import json
from pathlib import Path
manifest = Path('project-plans/coderabbit/analysis/expected-failing-tests.json')
data = json.loads(manifest.read_text())
if data not in ([], {'tests': []}):
    raise SystemExit('expected-failing-tests.json must be empty before P21 completion')
PY

! grep -rn "todo!\|unimplemented!" src/engine/executors src/engine/executor.rs tests --include="*.rs"
! grep -rn -E "(// TODO|// FIXME|// HACK|placeholder|not yet|will be|@pseudocode lines X-Y|@pseudocode TBD|TODO API|json_path TBD|fixture TBD|assertion TBD)" src/engine/executors src/engine/executor.rs tests project-plans/coderabbit/analysis --include="*.rs" --include="*.md" --include="*.json"
```

Expected: both explicit negative checks return success because there are no matches in implemented PR follow-through code/tests/contracts outside documented allowlists.

## Semantic Verification Checklist

- [ ] PR checks are watched from structured current-head state.
- [ ] Watch defaults are one hour and five-minute increments.
- [ ] Watcher is non-fail-fast by default.
- [ ] CI failures are deterministic artifacts.
- [ ] CodeRabbit readiness/stabilization prevents empty premature clean completion.
- [ ] Each feedback item has exactly one accepted validated evaluation result from a one-item request, or a reused evaluation emitted once without LLM re-invocation.
- [ ] Valid feedback and CI failures feed remediation.
- [ ] Marker comments use structured reasons/action taken.
- [ ] Marker actions are idempotent using both local `pr-feedback-marker-report.json` artifacts and remote marker comments with stable marker keys.
- [ ] Pending/unknown check timeouts are separated from concrete CI failures and are not blindly routed as must-fix.
- [ ] The evaluator uses internal per-item retries only; workflow has no evaluator malformed-output self-loop.
- [ ] Pending marker actions survive remediation pushes/head changes: a fixed item from original head A is still commented/resolved after clean head B, and marker retries do not duplicate actions.
- [ ] Page-2 GitHub data is consumed for checks/check-runs, workflow jobs/logs, review threads/comments, issue comments, and remote marker comments; GraphQL-lacks-comment fixtures use the REST fallback.

- [ ] llxprt remediation success is enforced by deterministic validator routing.

- [ ] Workflow TOML uses only routable StepOutcome values: `success`, `fixable`, `retryable`, `fatal`, and `abandon`.
- [ ] Bounded non-success paths reach `post_pr_failure_terminal`, which has no outgoing transitions, and do not report `RunOutcome::Success` through successful cleanup or `RunOutcome::Abandoned` through engine transition caps.
- [ ] Terminal/audit ordering uses artifact/failure sequence fields and injectable-clock timestamps, never filesystem mtimes.
- [ ] Validated remediation is followed by dedicated `run_post_pr_tests` local verification before push.
- [ ] Malformed non-empty `pr-remediation-result.json` is rejected.

- [ ] Workflow does not complete immediately after PR creation.

## Success Criteria

- All verification commands return expected results.
- No phases skipped.
- No false-clean test scenarios pass incorrectly.
- Phase 21 executes the preflight-recorded argv-safe dry-run command from `project-plans/coderabbit/analysis/final-dry-run-command.json`; there is no commented placeholder dry-run command. Required non-ignored e2e dry-run/step-list coverage and normalized production-vs-fixture comparison pass; ignored smoke tests are optional and cannot substitute for these gates.

- The feature is reachable through `llxprt-issue-fix-v1`.

## Failure Recovery

If final verification fails:

1. Do not merge or claim success.
2. Identify the failing phase requirement.
3. Return to the earliest phase whose tests/specs are insufficient.
4. Do not patch around failing tests by weakening requirements.
