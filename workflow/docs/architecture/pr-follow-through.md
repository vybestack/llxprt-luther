# PR Follow-Through Architecture

@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P20
@requirement:REQ-PRFU-001,REQ-PRFU-002,REQ-PRFU-003,REQ-PRFU-004,REQ-PRFU-009,REQ-PRFU-011,REQ-PRFU-014,REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-018,REQ-PRFU-020,REQ-PRFU-020A,REQ-PRFU-033,REQ-PRFU-034,REQ-PRFU-035

This document summarizes the CodeRabbit PR follow-through architecture added by `PLAN-20260429-CODERABBIT-PR-FOLLOWUP`. It is the operator-facing traceability map for the deterministic engine executors, the workflow tail, and the artifact families used to prove why a run completed or failed.

## Deterministic boundaries

This section documents deterministic boundaries.

@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P20
@requirement:REQ-PRFU-002,REQ-PRFU-003,REQ-PRFU-018,REQ-PRFU-033,REQ-PRFU-034,REQ-PRFU-035

The deterministic boundary owns every fact-gathering, classification, routing, and completion decision:

- PR identity capture and current-head binding for repository owner/name, PR number, head ref, head SHA, base ref, and run identity.
- PR check observation, stale/current filtering, check-state classification, pending/unknown timeout detection, and failure collection from structured GitHub data.
- CI failure metadata and bounded log collection, including explicit `available`, `unavailable`, `not_applicable`, and `fetch_failed` log status.
- CodeRabbit feedback discovery from GitHub issue comments, reviews, review comments, and review threads, including readiness, stabilization, deduplication, and state reuse.
- Strict validation that each current CodeRabbit feedback item has exactly one accepted validated feedback evaluation for the current item key/body hash/head SHA.
- Remediation plan aggregation from current-head CI failures plus accepted feedback evaluations.
- Remediation result validation, local post-PR verification, push result validation, feedback marker idempotency, and terminal failure source selection.
- Workflow routing decisions and loop-cap exhaustion. The LLM is never asked whether checks passed, whether CodeRabbit is ready, whether a loop is complete, or whether a terminal failure is safe to ignore.

## LLM boundaries

@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P20
@requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-014,REQ-PRFU-033,REQ-PRFU-034

The LLM boundary is limited to judgment and remediation content:

- `feedback_evaluator` invokes the configured LLM once per feedback item that lacks a reusable accepted evaluation. The accepted output is strict JSON for that single item only.
- The LLM may decide whether a CodeRabbit item is `valid`, `invalid`, `out_of_scope`, or `needs_user_judgment`, and must provide a reason plus recommended action.
- `PrFollowupRemediationExecutor` invokes llxprt for accepted `must_fix` work only: current-head CI failures and valid CodeRabbit items from the deterministic plan.
- The remediation LLM may edit code and describe actions taken, but its result is not trusted until `pr-remediation-result.json` passes deterministic validation.
- The marker executor comments and resolves from structured artifacts; it does not invent action text from free-form LLM logs.

## Artifact root and path model

@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P20
@requirement:REQ-PRFU-002,REQ-PRFU-003

PR follow-through executors use `artifact_root` as the configured storage root. The path contract is no artifact_dir: the legacy `{artifact_dir}` name is not a PR follow-through executor contract and must not be used as an alias.

Canonical current JSON path:

```text
<artifact_root>/pr-followup/current/<run_id>/<repository_owner>/<repository_name>/<pr_number>/<artifact_family>.json
```

Immutable history JSON path:

```text
<artifact_root>/pr-followup/history/<run_id>/<repository_owner>/<repository_name>/<pr_number>/<artifact_family>/<artifact_sequence>-<write_sequence>-<producer_step_id>.json
```

Raw logs may live beside the JSON artifacts, but JSON metadata records their paths, availability, bounded excerpts, command metadata, and binding fields.

## Common artifact schemas and correlation fields

@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P20
@requirement:REQ-PRFU-001,REQ-PRFU-002,REQ-PRFU-003

Every machine-readable PR follow-through artifact includes enough identifiers to trace records across workflow iterations:

- Schema and run identity: `schema_version`, `run_id`, and workflow/run-specific artifact location metadata.
- PR identity: `repository_owner`, `repository_name`, `pr_number`, `head_ref`, `head_sha`, `base_ref`, and `base_sha` when known.
- Source/output head fields where relevant: `input_head_sha`, `output_head_sha`, `source_head_sha`, `remediation_output_head_sha`, local/remote/pre/post push head SHA fields, and stale/current binding arrays.
- Persistence ordering: global `artifact_sequence`, per-family `write_sequence`, and failure-only `failure_sequence`.
- Producer ordering: `producer_step_id`, `step_order_index`, `history_metadata.canonical_path`, `history_metadata.history_path`, `history_metadata.artifact_family`, and `history_metadata.history_written_at`.
- Item correlation: `item_id`, `stable_marker_key`, `body_hash`, `thread_id`, `comment_id`, `review_id`, check IDs, CI `failure_id`, plan `source_id`, marker `action_id`, and marker `idempotency_key`.
- Current/stale binding behavior: consumers reject missing or mismatched run/PR/head fields for current decisions, may record rejected paths in stale/audit arrays, and never infer current success from stale-only evidence.

Terminal source ordering is deterministic: `failure_sequence`, then `artifact_sequence`, then `write_sequence`, then `step_order_index`; filesystem mtimes are not part of failure selection.

## Artifact schemas

@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P20
@requirement:REQ-PRFU-003,REQ-PRFU-014,REQ-PRFU-015,REQ-PRFU-016

### `pr.json` — PR identity

Records `capture_state`, `pr_url`, `source`, `source_pr_node_id`, captured PR binding fields, and source head repository information. Downstream current-head artifacts copy and validate its binding fields.

### `post-pr-iteration-guard.json` — remediation iteration cap

Records `iteration_index`, `max_post_pr_remediation_iterations`, `previous_head_sha`, current `head_sha`, `reason`, `guard_state`, `ignored_stale_artifacts`, and `updated_at`. It enforces the post-PR remediation cap with an artifact-backed current-head sequence rather than engine loop memory.

### `pr-check-status.json` — check status

Records `poll_attempts`, `max_attempts`, `poll_interval_seconds`, `max_duration_seconds`, `overall_state`, `poll_observations`, final trusted current-head `checks`, `stale_checks`, `observed_at`, and `fatal_source`. Observations are written at t=0,5,...,55 minutes for the default 12 observations; there is no t=60 observation.

### `ci-failures.json` — CI failures

Records `collection_state`, concrete current-head `failures`, non-remediable `pending_or_unknown`, `watcher_fatal_source`, collection `fatal_source`, `log_artifacts`, and `source_check_status_artifact_sequence`. Failed, cancelled, timed-out, and action-required checks become concrete failures only when current-head and terminal; pending/unknown/stale evidence never becomes remediation `must_fix`.

### `coderabbit-feedback.json` — CodeRabbit feedback

Records `readiness_state`, `stable_observation_count`, `required_stable_observations`, `max_observations`, `observation_interval_seconds`, `observations`, normalized `items`, `included_bot_identities`, and `feedback_item_set_hash`. Items preserve IDs, thread/comment/review fields, path/line, body, `body_hash`, URL, timestamps, resolution availability, source, raw node ID, and commit SHA.

### `coderabbit-feedback-state.json` — feedback state

Records `state_entries`, `state_index_hash`, and `superseded_entries`. Entries are keyed by stable marker key, body hash, and head SHA, and track first/last seen timestamps, evaluation status, remediation status, marker status, resolution status, superseded/stale flags, and reuse eligibility.

### `feedback-evaluations.json` — feedback evaluations

Records `items_seen`, `accepted_results`, `rejected_attempts`, `unevaluated_items`, `budget_exhausted_items`, `max_attempts_per_item`, `evaluation_state`, and `reused_results_count`. When complete, `accepted_results` contains exactly one accepted validated feedback evaluation per current item/body/head, with decision, reason, recommended action, accepted timestamp, attempt count, source, and reuse state.

### `pr-remediation-plan.json` — remediation plan

Records `plan_state`, `must_fix`, `mark_invalid`, `needs_user_judgment`, `pending_or_unknown`, and consumed `source_artifacts`. Only current-head CI failures and valid feedback enter `must_fix`; invalid/out-of-scope feedback enters `mark_invalid`; unresolved ambiguity, unknowns, timeouts, and `needs_user_judgment` items route to non-success.

### `pr-remediation-llxprt-run.json` — remediation invocation

Records `remediation_invocation_state`, plan and expected result paths, safe `argv`, process exit metadata, stdout/stderr artifact paths, bounded output excerpts, and `validator_readable_result_written`. Wrapper success means a validator-readable artifact exists or should be checked; it does not mean remediation succeeded.

### `pr-remediation-result.json` — remediation results

Records `input_head_sha`, `output_head_sha`, `overall_status`, one `results[]` entry per `must_fix` item, `verification_commands`, `success_file_path`, `validation_state`, `validation_retry_index`, `max_validation_retries`, `remediation_attempt_index`, `max_remediation_attempts`, `retry_scope`, `plan_artifact_sequence`, `unsuccessful_statuses`, and `no_change_after_remediation`. Result statuses are structured (`fixed`, `changed`, `already_satisfied`, `not_reproduced`, `not_fixed`, `skipped`, `failed`) and require deterministic evidence for already-satisfied/not-reproduced claims.

### `post-pr-test-result.json` — post-PR tests

Records `test_state`, configured command IDs or argv, command statuses, bounded outputs, full log paths, `verification_retry_index`, `max_verification_retries`, `retry_scope`, `plan_artifact_sequence`, `remediation_result_artifact_sequence`, and `verification_retry_exhausted`.

### `push-remediation-result.json` — push results

Records `push_state`, retry scope/counters, `remote_ref`, `pre_push_local_head_sha`, `pre_push_remote_head_sha`, `pre_push_pr_head_sha`, `committed_head_sha`, `post_push_local_head_sha`, `post_push_remote_head_sha`, `expected_head_sha`, `verified_remote_matches_expected`, staged/excluded paths, commit message, push error class, safe command metadata, and stdout/stderr artifact paths.

### `pending-feedback-marker-actions.json` — marker action queue

Records `pending_actions`, `carry_forward_from_artifact_sequence`, and marker policy. Each action carries `action_id`, `action_kind`, `item_id`, `stable_marker_key`, `source_head_sha`, `remediation_output_head_sha`, `body_hash`, `idempotency_key`, comment body artifact path/template, resolution requirement, status, and reason.

### `pr-feedback-marker-report.json` — marker actions

Records `marker_state`, `marker_actions`, `skipped_actions`, `remote_marker_comments_seen`, `resolved_threads`, `posted_comments`, and `pending_feedback_marker_actions_artifact_sequence`. Marker actions record hidden marker data, body hashes, source/remediation heads, posted comment IDs/URLs, resolution attempts/results, API operation/error class, completion time, and idempotency state.

### `post-pr-failure-terminal.json` — terminal failure artifact

Records `terminal_state=fatal`, `failed_step`, `failure_reason`, candidate `source_artifacts`, selected source failure/artifact/write sequences, selected source producer step/order, current and history source paths, selection reason, and `logged_at`. All bounded post-PR non-success paths route here before overall workflow failure.

## Workflow routing overview

@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P20
@requirement:REQ-PRFU-018,REQ-PRFU-020,REQ-PRFU-020A,REQ-PRFU-035

The workflow tail after `create_pr` is deterministic and uses only globally valid routable outcomes: `success`, `fixable`, `retryable`, and `fatal` for the post-PR path. The post-PR TOML does not introduce custom semantic route outcomes such as `passed`, `ready`, `clean`, `valid`, `invalid`, `continue`, or `needs_user_judgment`, and it does not use post-PR `abandon` routes.

Routing summary:

1. `create_pr success -> capture_pr_identity`.
2. `capture_pr_identity success -> post_pr_iteration_guard`; fatal routes to `post_pr_failure_terminal`.
3. `post_pr_iteration_guard success -> watch_pr_checks`; cap exhaustion/fatal routes to `post_pr_failure_terminal`.
4. `watch_pr_checks` routes all outcomes to `collect_ci_failures` so the CI collector can preserve watcher evidence and classify whether downstream CodeRabbit evaluation may continue.
5. `collect_ci_failures success -> collect_coderabbit_feedback`; fatal routes to `post_pr_failure_terminal`.
6. `collect_coderabbit_feedback success -> evaluate_coderabbit_feedback`; fatal routes to `post_pr_failure_terminal`.
7. `evaluate_coderabbit_feedback success -> build_remediation_plan`; fatal routes to `post_pr_failure_terminal`.
8. `build_remediation_plan success -> mark_coderabbit_feedback`; `fixable -> remediate_pr_followup`; fatal routes to `post_pr_failure_terminal`.
9. `remediate_pr_followup success -> validate_remediation_result`; fatal routes to `post_pr_failure_terminal`.
10. `validate_remediation_result success -> run_post_pr_tests`; `fixable -> remediate_pr_followup`; fatal routes to `post_pr_failure_terminal`.
11. `run_post_pr_tests success -> push_remediation_changes`; `fixable -> remediate_pr_followup`; fatal routes to `post_pr_failure_terminal`.
12. `push_remediation_changes success -> capture_pr_identity`; retryable/fatal routes to `post_pr_failure_terminal` unless a dedicated artifact-backed push retry loop is configured.
13. `mark_coderabbit_feedback success -> log_completion`; fatal routes to `post_pr_failure_terminal`.
14. `post_pr_failure_terminal` returns only `fatal`, producing overall non-success instead of cleanup success.

The remediation validation/local-test/push loop is therefore artifact-backed: remediation writes structured results, validation proves coverage, local tests must pass, push records head movement/no-change state, and the next iteration recaptures PR identity before watching the new head.

## Operational behavior

@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P20
@requirement:REQ-PRFU-004,REQ-PRFU-009,REQ-PRFU-011,REQ-PRFU-014,REQ-PRFU-015,REQ-PRFU-016

- Check watching performs 12 observations at t=0,5,...,55 minutes by default with `poll_interval_seconds=300` and `max_duration_seconds=3600`. The watcher writes a status artifact after every polling cycle and never fails fast while current-head checks remain pending and budget remains.
- CodeRabbit readiness requires deterministic readiness/stabilization evidence. Empty feedback is clean only after readiness and stabilization have been established for the current PR head. The v1 readiness collector uses bounded internal observations, a default maximum of 6, and stable-ready requirement of two identical ready observations.
- Feedback evaluation requires exactly one accepted validated feedback evaluation per current item/body/head. Malformed LLM output is captured in rejected attempts and retried only within the per-item cap; budget exhaustion is terminal non-success rather than fabricated evaluation.
- Structured remediation result requirements are enforced before marker actions: every `must_fix` CI failure and valid feedback item must have one bound structured result with status, action, and deterministic evidence paths or explanation; fixed feedback cannot be claimed without validator-approved remediation evidence.
- Marker comments and review-thread resolution are idempotent. Local marker reports and remote hidden marker comments are checked before posting or resolving. Idempotency keys bind stable marker key, source head SHA, remediation output head, body hash, action kind, and run identity. Ambiguous idempotency, partial marker action where policy requires completion, API failure, or unavailable required resolution routes to `post_pr_failure_terminal`.

## Traceability map

@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P20
@requirement:REQ-PRFU-001,REQ-PRFU-002,REQ-PRFU-003,REQ-PRFU-018,REQ-PRFU-020,REQ-PRFU-035

- Engine implementation files: `src/engine/executors/pr_followup_artifacts.rs`, `src/engine/executors/pr_followup_types.rs`, `src/engine/executors/github_pr.rs`, `src/engine/executors/github_feedback.rs`, `src/engine/executors/feedback_eval.rs`, `src/engine/executors/pr_remediation.rs`, and executor registration in `src/engine/executors/mod.rs`.
- Workflow configuration: `config/workflows/llxprt-issue-fix-v1.toml` and generated/fixture workflow TOML/JSON under `tests/fixtures/workflows/`.
- Tests: `tests/github_pr_followup_executor_tests.rs`, `tests/pr_followup_workflow_integration.rs`, `tests/e2e_workflow_integration.rs`, `tests/workflow_shell_safety_tests.rs`, `tests/pr_followup_marker_audit_tests.rs`, and GitHub API contract tests/fixtures.
- Planning contracts: `project-plans/coderabbit/implementation-plan.md`, `project-plans/coderabbit/engineupdates/REQUIREMENTS.md`, `project-plans/coderabbit/analysis/artifact-schema-contract.md`, and `project-plans/coderabbit/analysis/github-api-contract.md`.

## Recoverable external-wait state and operator continuation

@plan:PLAN-20260623-LUTHER-CONTINUATION

A PR-check pending timeout is a *recoverable* condition: CI was still running when
the watch window closed, and the checks may later go green. Such timeouts are no
longer mapped to an irreversible terminal failure.

- `watch_pr_checks` maps the typed check classification as `Passed -> success`,
  `Failed -> fixable`, genuine `Unknown`/`Fatal` -> `fatal`, and
  `PendingTimeout -> wait` (`StepOutcome::Wait`). The workflow intentionally
  defines no `wait` transition for `watch_pr_checks`, so a wait outcome **pauses**
  the run at `watch_pr_checks` with a resumable checkpoint
  (`StateSnapshot.status = "waiting"`) and records the non-terminal
  `RunStatus::WaitingForChecks` instead of routing the timeout to
  `post_pr_failure_terminal`. The `pr-check-status` artifact still records
  `overall_state = pending_timeout`.
- The engine surfaces this as `RunOutcome::WaitingExternal { step_id, reason }`;
  the process exits 0 with a hint to resume.

Operators continue a paused or failed run from the CLI without editing SQLite:

- `luther-workflow runs checkpoints RUN_ID [--json]` — list every per-step
  checkpoint with step id, status, timestamp, loop count, retry count, and a
  brief context summary.
- `luther-workflow runs resume RUN_ID [--force]` — resume the newest resumable
  checkpoint (`waiting`/`interrupted`/`ready_to_resume`); for a terminal `Failed`
  run, the checkpoint immediately before the recorded terminal step.
- `luther-workflow runs retry RUN_ID --from-failed-step [--force]` — re-run the
  failed external-wait step (the `watch_pr_checks` pending-timeout case) using the
  prior checkpoint context.
- `luther-workflow runs rewind RUN_ID (--to-step STEP | --to-checkpoint ID) [--force]`
  — set the resume point to a selected earlier checkpoint by step id or by
  `step_id@timestamp` identity.

Continuation is validated before any state changes (run id exists, checkpoint
exists, workflow type/config still resolve and match, issue/PR identity is
recoverable, workspace is present, and the selected step is in the safe-to-rerun
whitelist — `watch_pr_checks`, `collect_ci_failures`,
`collect_coderabbit_feedback`, `capture_pr_identity`, `post_pr_iteration_guard`).
Implementation/remediation steps are rejected unless `--force`. Failed validation
refuses the operation with per-check diagnostics rather than corrupting state.

Continuation preserves history (the event log is append-only and the terminal
checkpoint row is retained with its older timestamp) and writes auditable
artifacts under the run's artifact root: `continuation-request.json`,
`continuation-validation.json`, `checkpoint-selection.json`, and
`resume-result.json` / `retry-result.json`. A successful continuation reopens the
run to `RunStatus::Running` and appends a continuation event so monitor and
`runs show` reflect the resumed state.

- Engine/persistence implementation files: `src/engine/continuation.rs`,
  `src/engine/transition.rs` (`StepOutcome::Wait`), `src/engine/runner.rs`
  (`RunOutcome::WaitingExternal`), `src/persistence/checkpoint.rs`
  (`set_resume_point`/`get_checkpoint_for_step`/`load_checkpoint_before_step`),
  `src/persistence/run_metadata.rs` (`RunStatus::is_resumable`/`reopen`), and the
  `runs` subcommands in `src/cli/mod.rs` + `src/main.rs`.
- Tests: `tests/continuation_integration.rs`, `tests/engine_resume_integration.rs`,
  and the recoverable-wait assertions in `tests/github_pr_followup_executor_tests.rs`
  and `tests/pr_followup_replay_e2e_tests.rs`.
