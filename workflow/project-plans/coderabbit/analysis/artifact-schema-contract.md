# PR Follow-Through Artifact Schema Contract

Plan: `PLAN-20260429-CODERABBIT-PR-FOLLOWUP`
Phase: P01 domain model and schemas
Schema version: `1`

This contract defines the Phase 01 domain model and JSON artifact contracts for PR follow-through. It is an analysis/schema contract artifact only: no production Rust structs or workflow TOML are introduced in this phase. Phase 02 must validate every externally sourced field against fixture-backed GitHub API paths and update this document if API evidence requires a rename.

## Superseded source-spec clauses

This contract follows `project-plans/coderabbit/implementation-plan.md` as the controlling source and supersedes older overview vocabulary where it conflicts:

| Older overview vocabulary or clause | Contract vocabulary in this phase |
|---|---|
| `{artifact_dir}` | `artifact_root` only. No `artifact_dir` alias is allowed for PR follow-through executors. |
| `repo` string as primary binding | Split `repository_owner` and `repository_name`; a derived display value may exist but is not a binding substitute. |
| `abandon_and_log` post-PR fatal route | `post_pr_failure_terminal` writes `post-pr-failure-terminal.json` and returns `fatal`. |
| Workflow self-loop for CodeRabbit readiness or malformed evaluator output | Bounded executor-internal readiness/evaluator retry artifacts; TOML must not self-loop those states. |
| Raw `run_tests` / `push_changes` reuse in post-PR tail | Dedicated `run_post_pr_tests` and `push_remediation_changes` artifact contracts. |
| Generic `remediate_pr_feedback` raw `llxprt` step | `PrFollowupRemediationExecutor` wrapper and validator-readable `pr-remediation-result.json`. |

## Artifact root and binding contract

`artifact_root` is a required TOML parameter for each PR follow-through executor. It is expanded using workflow variables, resolved relative to the workflow work directory when relative, created if missing, canonicalized, and then used to initialize `PrFollowupArtifactStore`. Missing, empty, unexpandable, or non-canonicalizable roots are fatal configuration errors.

Canonical current path:

```text
<artifact_root>/pr-followup/current/<run_id>/<repository_owner>/<repository_name>/<pr_number>/<artifact_family>.json
```

History path:

```text
<artifact_root>/pr-followup/history/<run_id>/<repository_owner>/<repository_name>/<pr_number>/<artifact_family>/<artifact_sequence>-<write_sequence>-<producer_step_id>.json
```

Every current-head artifact repeats these binding fields copied from `pr.json`:

| Field | Type | Required | Notes |
|---|---:|---|---|
| `schema_version` | integer | yes | Initial value `1`. |
| `run_id` | string | yes | Workflow run correlation ID. |
| `repository_owner` | string | yes | GitHub owner/organization. |
| `repository_name` | string | yes | GitHub repository name. |
| `pr_number` | integer | yes | Pull request number. |
| `head_ref` | string | yes | PR head branch/ref. |
| `head_sha` | string | yes | Current PR head SHA for artifacts that observe the current head. |
| `base_ref` | string | yes | PR base branch/ref. |
| `base_sha` | string or null | yes | Null only when unavailable in the captured identity; if present in `pr.json`, downstream consumers reject missing or mismatched values. |

Consumers reject absent or mismatched `run_id`, `repository_owner`, `repository_name`, `pr_number`, `head_ref`, `head_sha`, `base_ref`, and non-null `base_sha`. Rejected stale artifacts may be recorded in audit arrays such as `ignored_stale_artifacts`, `stale_checks`, or `source_artifacts`, but must not be consumed as current evidence.

## Artifact store common fields and sequence semantics

Every artifact family includes these persistence fields in both canonical current and history payloads:

| Field | Type | Required | Semantics |
|---|---:|---|---|
| `artifact_sequence` | integer | yes | Monotonic global sequence allocated across all PR follow-through artifact writes for the same `run_id` and PR binding. |
| `write_sequence` | integer | yes | Monotonic sequence within the artifact family for every atomic write. Rewrites of aggregate artifacts increment it. |
| `history_metadata` | object | yes | Metadata for current/history persistence. |
| `history_metadata.canonical_path` | string | yes | Canonical current JSON path. |
| `history_metadata.history_path` | string | yes | Immutable history snapshot JSON path. |
| `history_metadata.artifact_family` | string | yes | File family without `.json`, for example `pr-check-status`. |
| `history_metadata.is_canonical` | boolean | yes | `true` for canonical current payload snapshots; history files preserve the value written by the store. |
| `history_metadata.history_written_at` | string | yes | RFC3339 timestamp from injectable clock. |
| `step_order_index` | integer | yes | TOML step order index used for deterministic terminal ordering. |
| `producer_step_id` | string | yes | Workflow step ID that produced the artifact. |

`artifact_sequence` is global, not per family. `write_sequence` is per family. Terminal source ordering uses `failure_sequence` first, then `artifact_sequence`, then `write_sequence`, then `step_order_index`; it must not use filesystem mtimes.

## Failure artifact contract

Any artifact written in a non-success semantic state, any terminal source evidence, and any artifact that reports fixable/retryable/fatal behavior includes:

| Field | Type | Required | Notes |
|---|---:|---|---|
| `semantic_state` | string | yes | Artifact-specific state such as `pending_timeout`, `fatal`, `blocked_needs_user_judgment`, `failed`, or `fixable_cap_exhausted`. |
| `failure_reason` | string | yes | Deterministic machine-readable reason. |
| `failure_sequence` | integer | yes | Monotonic global sequence across failure-producing PR follow-through artifacts for the same `run_id`. |
| `produced_at` | string | yes | RFC3339 timestamp from injectable clock. |
| `failure_details` | object | yes | Safe structured details; must not contain unbounded logs. |

Failure artifacts still include all binding and common persistence fields. Raw logs may be separate files but are referenced through bounded metadata in JSON.

## Check-state precedence

`pr-check-status.json.overall_state` is computed by this precedence order for current-head observations:

1. `fatal`: GitHub/API/auth/schema failure, unbindable current-head state, missing trusted current-head check data where required, or artifact-write failure.
2. `pending_timeout`: any current-head pending check remains after the watch budget.
3. `unknown`: any current-head unknown state remains after the watch budget or final terminal classification.
4. `failed`: one or more current-head terminal `failure`, `startup_failure`, `timed_out`, `action_required`, or `cancelled` checks exist, and no pending/unknown/fatal condition has higher precedence.
5. `passed`: all trusted current-head visible/required checks are terminal and in passing buckets (`success`, `neutral`, `skipped`) with neutral/skipped recorded separately.

Stale checks never create `passed` or `failed` current-head evidence. Stale-only data is `fatal` or produces `pending_or_unknown` through `ci-failures.json` and then terminal non-success.

## Artifact field tables

### `pr.json` — PR identity/current-head binding

| Field | Type | Required | Notes |
|---|---:|---|---|
| Common binding/common fields | mixed | yes | Includes `schema_version`, `run_id`, repository fields, PR fields, sequence fields. |
| `pr_url` | string | yes | Canonical pull request HTML URL. |
| `capture_state` | enum | yes | `captured` or `fatal`. |
| `captured_at` | string | yes | RFC3339 timestamp. |
| `source` | enum | yes | `create_pr_artifact`, `gh_pr_view`, `graphql`, or `rest`. |
| `source_pr_node_id` | string or null | yes | GitHub PR node ID when available. |
| `source_head_repository_owner` | string or null | yes | Fork owner if available. |
| `source_head_repository_name` | string or null | yes | Fork repo if available. |

### `post-pr-iteration-guard.json` — artifact-backed remediation-loop cap

| Field | Type | Required | Notes |
|---|---:|---|---|
| Common binding/common fields | mixed | yes | Bound to current `pr.json`. |
| `iteration_index` | integer | yes | Initial current-head cycle is `0`; remediation head-change cycles are `1..max`. |
| `max_post_pr_remediation_iterations` | integer | yes | Default `3`. |
| `previous_head_sha` | string or null | yes | Null for initial entry. |
| `reason` | enum | yes | `initial_entry`, `same_head_reentry`, `head_sha_changed_after_remediation_push`, `max_iterations_exceeded`, or `unreadable_or_unbindable_guard_state`. |
| `guard_state` | enum | yes | `proceed`, `max_iterations_exceeded`, or `fatal`. |
| `ignored_stale_artifacts` | array | yes | History/current paths ignored due to run/PR/head mismatch or staleness. |
| `updated_at` | string | yes | RFC3339. |

### `pr-check-status.json` — check watch status

| Field | Type | Required | Notes |
|---|---:|---|---|
| Common binding/common fields | mixed | yes | Bound to current `pr.json`. |
| `poll_attempts` | integer | yes | Completed observations. Default max is 12. |
| `max_attempts` | integer | yes | Default `12`, observations at t=0,5,...,55 minutes. |
| `poll_interval_seconds` | integer | yes | Default `300`. |
| `max_duration_seconds` | integer | yes | Default `3600`. |
| `overall_state` | enum | yes | `passed`, `failed`, `pending_timeout`, `unknown`, or `fatal`. |
| `poll_observations` | array | yes | Aggregate observation history; rewritten atomically after every poll. |
| `checks` | array | yes | Final trusted current-head check records. |
| `stale_checks` | array | yes | Older-head records ignored for current decisions. |
| `observed_at` | string | yes | Final observation timestamp. |
| `fatal_source` | string or null | yes | Deterministic source such as `auth`, `api`, `schema`, `artifact_write`, `stale_only`, or null. |

Each `poll_observations[]` entry contains `attempt_number`, `observed_at`, `head_sha`, `current_head_checks`, `stale_checks`, `classification`, `terminal_counts`, and `write_sequence`. Each check contains `check_id`, `name`, `status`, `conclusion`, `state`, `bucket`, `url`, `workflow_name`, `run_id`, `job_id`, `started_at`, `completed_at`, `head_sha`, `app_slug`, and `source`.

### `ci-failures.json` — CI failure collection

| Field | Type | Required | Notes |
|---|---:|---|---|
| Common binding/common fields | mixed | yes | Bound to current `pr.json`. |
| `collection_state` | enum | yes | `collected` or `fatal`. |
| `failures` | array | yes | Concrete terminal current-head failures only. |
| `pending_or_unknown` | array | yes | Pending-after-budget, unknown, stale-only, schema-unbindable, or watcher-fatal evidence that must not become remediation `must_fix`. |
| `watcher_fatal_source` | string or null | yes | Copied from `pr-check-status.json.fatal_source` when present. |
| `fatal_source` | string or null | yes | Collection fatal classifier source. |
| `log_artifacts` | array | yes | Bounded raw log metadata. |
| `source_check_status_artifact_sequence` | integer | yes | `artifact_sequence` of consumed `pr-check-status.json`. |

`failures[]` records include `failure_id`, `check_id`, `check_name`, `state`, `conclusion`, `url`, `run_id`, `job_id`, `log_status`, `log_excerpt`, `log_excerpt_path`, `raw_log_path`, and `collection_error`. `log_status` is `available`, `unavailable`, `not_applicable`, or `fetch_failed`. `pending_or_unknown` records always route to terminal non-success/needs-user-judgment; they are never placed in `pr-remediation-plan.must_fix`.

### `coderabbit-feedback.json` — CodeRabbit feedback and readiness

| Field | Type | Required | Notes |
|---|---:|---|---|
| Common binding/common fields | mixed | yes | Bound to current `pr.json`. |
| `readiness_state` | enum | yes | `ready`, `not_ready`, `timeout`, or `fatal`. |
| `stable_observation_count` | integer | yes | Ready requires two identical ready observations in v1. |
| `required_stable_observations` | integer | yes | Default `2`. |
| `max_observations` | integer | yes | Default `6`. |
| `observation_interval_seconds` | integer | yes | From `coderabbit_readiness_observation_interval_seconds`, default `300`. |
| `observations` | array | yes | Readiness/stability observations. |
| `items` | array | yes | Normalized unresolved current-head feedback items. |
| `included_bot_identities` | array | yes | Configured identities used for filtering. |
| `feedback_item_set_hash` | string or null | yes | Current normalized item-set hash. |

Feedback items include `item_id`, `stable_marker_key`, `thread_id`, `comment_id`, `review_id`, `author_login`, `author_association`, `bot_identity`, `path`, `line`, `side`, `body`, `body_hash`, `url`, `created_at`, `updated_at`, `resolved`, `outdated`, `resolution_state_available`, `source`, `raw_node_id`, and `commit_sha`. Observations include `observed_at`, `signals_seen`, `bot_identities_matched`, `observation_hash`, `budget_used`, `budget_remaining`, `items_count`, `outcome_reason`, `current_head_ready_signal_seen`, `in_progress_signal_seen`, and `stale_signals`.

### `coderabbit-feedback-state.json` — feedback state/reuse/idempotency source

| Field | Type | Required | Notes |
|---|---:|---|---|
| Common binding/common fields | mixed | yes | Bound to current `pr.json`. |
| `state_entries` | array | yes | Entries keyed by `stable_marker_key`, `body_hash`, and `head_sha`. |
| `state_index_hash` | string | yes | Deterministic hash of current accepted state index. |
| `superseded_entries` | array | yes | Prior body/head entries retained for audit. |

State entries include `item_id`, `stable_marker_key`, `body_hash`, `head_sha`, `first_seen_at`, `last_seen_at`, `evaluation_status` (`accepted`, `rejected`, `unevaluated`, `budget_exhausted`, or `reused`), `accepted_evaluation`, `remediation_status`, `marker_status`, `resolution_status`, `superseded`, `stale`, and `reuse_eligible`.

### `feedback-evaluations.json` — accepted/rejected/unevaluated evaluation state

| Field | Type | Required | Notes |
|---|---:|---|---|
| Common binding/common fields | mixed | yes | Bound to current `pr.json`. |
| `items_seen` | integer | yes | Must equal current feedback input count. |
| `accepted_results` | array | yes | Exactly one per item when `evaluation_state=complete`. |
| `rejected_attempts` | array | yes | Malformed/mismatched/unknown-decision LLM outputs. |
| `unevaluated_items` | array | yes | Items with no accepted result for non-budget reasons. |
| `budget_exhausted_items` | array | yes | Items whose malformed-output budget was exhausted. |
| `max_attempts_per_item` | integer | yes | Default `3`. |
| `evaluation_state` | enum | yes | `complete`, `incomplete`, `budget_exhausted`, or `fatal`. |
| `reused_results_count` | integer | yes | Count of accepted results reused from feedback state. |

Accepted results include `item_id`, `stable_marker_key`, `body_hash`, `head_sha`, `decision`, `reason`, `recommended_action`, `accepted_at`, `attempt_count`, `source`, and `reuse_state`. `decision` is exactly `valid`, `invalid`, `out_of_scope`, or `needs_user_judgment`; budget exhaustion is never an accepted decision. `source` is `new` or `reused`. `reuse_state` is `not_reused`, `reused_from_state`, or `not_reuse_eligible`. Rejected attempts include `attempt_number`, `item_id`, `raw_response_artifact_path`, `reject_reason`, `parsed_decision`, and `observed_head_sha`.

### `pr-remediation-plan.json` — deterministic remediation plan

| Field | Type | Required | Notes |
|---|---:|---|---|
| Common binding/common fields | mixed | yes | Bound to current `pr.json`. |
| `plan_state` | enum | yes | `clean`, `needs_remediation`, `blocked_needs_user_judgment`, or `fatal`. |
| `must_fix` | array | yes | CI failures and `valid` feedback only. |
| `mark_invalid` | array | yes | `invalid` and `out_of_scope` feedback with reasons. |
| `needs_user_judgment` | array | yes | Feedback or PR state requiring human judgment. |
| `pending_or_unknown` | array | yes | Copied non-remediable CI/check uncertainty. |
| `source_artifacts` | array | yes | Artifact sequences consumed to build plan. |

Each plan item includes `source_type` (`ci_failure` or `coderabbit_feedback`), `source_id`, `stable_marker_key` when applicable, `reason`, `recommended_action`, `input_head_sha`, and `source_artifact_sequence`. Pending/unknown check evidence is never in `must_fix`.

### `pr-remediation-llxprt-run.json` — remediation wrapper invocation

| Field | Type | Required | Notes |
|---|---:|---|---|
| Common binding/common fields | mixed | yes | Bound to plan input head. |
| `remediation_invocation_state` | enum | yes | `success`, `success_without_result`, `timeout`, `spawn_failed`, `retryable_failed`, or `fatal`. |
| `remediation_plan_path` | string | yes | Input plan path. |
| `remediation_result_path` | string | yes | Expected result path. |
| `argv` | array | yes | Safe argv list; no shell interpolation of GitHub text. |
| `exit_code` | integer or null | yes | Process exit code when available. |
| `signal` | integer or null | yes | Signal when available. |
| `stdout_artifact_path` | string or null | yes | Full stdout path if captured. |
| `stderr_artifact_path` | string or null | yes | Full stderr path if captured. |
| `bounded_stdout` | string | yes | Bounded excerpt. |
| `bounded_stderr` | string | yes | Bounded excerpt. |
| `validator_readable_result_written` | boolean | yes | Wrapper `success` means validator should run, not remediation success. |

### `pr-remediation-result.json` — remediation result/status enum

| Field | Type | Required | Notes |
|---|---:|---|---|
| Common binding/common fields | mixed | yes | Uses `input_head_sha` for plan binding and `output_head_sha` for post-remediation state. |
| `input_head_sha` | string | yes | Must match plan `head_sha`. |
| `output_head_sha` | string | yes | Git head after remediation attempt. |
| `overall_status` | enum | yes | `fixed`, `changed`, `already_satisfied`, `not_reproduced`, `not_fixed`, `skipped`, or `failed` summarized across results. |
| `results` | array | yes | One result per `must_fix` item. |
| `verification_commands` | array | yes | Commands/evidence produced by remediation when available. |
| `success_file_path` | string or null | yes | Present only if configured and compatible. |
| `validation_state` | enum | yes | `valid`, `fixable_malformed`, `invalid`, `fatal`, `valid_but_unsuccessful`, `fixable_cap_exhausted`, or `unsuccessful_remediation_cap_exhausted`. |
| `validation_retry_index` | integer | yes | Artifact-backed retry index. |
| `max_validation_retries` | integer | yes | Default `2`. |
| `remediation_attempt_index` | integer | yes | Same-head unsuccessful-structure retry index. |
| `max_remediation_attempts` | integer | yes | Default `2`. |
| `retry_scope` | object | yes | `run_id` + PR binding + input head + plan artifact sequence. |
| `plan_artifact_sequence` | integer | yes | Consumed plan sequence. |
| `unsuccessful_statuses` | array | yes | Any `not_fixed`, `skipped`, or `failed` statuses. |
| `no_change_after_remediation` | boolean | yes | Validator invariant. |

Canonical `results[].status` enum is exactly `fixed`, `changed`, `already_satisfied`, `not_reproduced`, `not_fixed`, `skipped`, or `failed`. Each result includes `source_type` (`ci_failure` or `coderabbit_feedback`), `source_id`, `status`, `action`, `evidence`, and `evidence_paths`. `already_satisfied` and `not_reproduced` require deterministic `evidence.kind`, `evidence.current_head_sha`, and at least one of `evidence.paths`, `evidence.commands`, `evidence.api_lookups`, or `evidence.check_runs`; explanation-only evidence is invalid.

### `post-pr-test-result.json` — dedicated post-PR verification

| Field | Type | Required | Notes |
|---|---:|---|---|
| Common binding/common fields | mixed | yes | Bound to current post-remediation head. |
| `test_state` | enum | yes | `passed`, `failed`, or `fatal`. |
| `commands` | array | yes | Configured command IDs or argv, statuses, bounded output, full log paths. |
| `verification_retry_index` | integer | yes | Artifact-backed retry index. |
| `max_verification_retries` | integer | yes | Default `2`. |
| `retry_scope` | object | yes | Same run/PR/head/plan scope. |
| `plan_artifact_sequence` | integer | yes | Consumed plan. |
| `remediation_result_artifact_sequence` | integer | yes | Consumed remediation result. |
| `verification_retry_exhausted` | boolean | yes | Exhaustion returns fatal. |

### `push-remediation-result.json` — dedicated push contract

| Field | Type | Required | Notes |
|---|---:|---|---|
| Common binding/common fields | mixed | yes | Bound to local/pr head at push time. |
| `push_state` | enum | yes | `no_change`, `no_change_excluded_only`, `pushed`, `retryable_failed`, `retry_exhausted`, or `fatal`. |
| `push_retry_index` | integer | yes | Artifact-backed push retry index. |
| `max_push_retries` | integer | yes | Default `1` if retry loop configured. |
| `retry_scope` | object | yes | Same run/PR/local head/remote ref scope. |
| `remote_ref` | string | yes | PR branch ref. |
| `pre_push_local_head_sha` | string | yes | Local head before staging/commit/push. |
| `pre_push_remote_head_sha` | string | yes | Remote PR branch head before push. |
| `pre_push_pr_head_sha` | string | yes | `pr.json.head_sha`. |
| `committed_head_sha` | string or null | yes | Commit created by executor when changes exist. |
| `post_push_local_head_sha` | string | yes | Local head after push attempt. |
| `post_push_remote_head_sha` | string or null | yes | Remote head after push attempt when known. |
| `expected_head_sha` | string | yes | Expected remote head. |
| `verified_remote_matches_expected` | boolean | yes | Required for pushed/no-change success. |
| `staged_paths`, `excluded_paths` | arrays | yes | Deterministic staging metadata. |
| `commit_message` | string or null | yes | Commit message when created. |
| `push_error_class` | string or null | yes | Retryable/fatal class. |
| `commands` | array | yes | Safe argv command metadata. |
| `stdout_artifact_path`, `stderr_artifact_path` | string or null | yes | Full logs when captured. |

### `pending-feedback-marker-actions.json` — pending marker action queue

| Field | Type | Required | Notes |
|---|---:|---|---|
| Common binding/common fields | mixed | yes | Bound to the source obligation head. |
| `pending_actions` | array | yes | Actions awaiting marker completion. |
| `carry_forward_from_artifact_sequence` | integer or null | yes | Prior pending action source when resumed/carried forward. |
| `marker_policy` | object | yes | Comment/resolve policy. |

Each pending action includes `action_id`, `action_kind` (`comment_fixed`, `comment_invalid`, `comment_out_of_scope`, `comment_needs_user_judgment`, `resolve_thread`, or `skip`), `item_id`, `stable_marker_key`, `source_head_sha`, `remediation_output_head_sha`, `body_hash`, `idempotency_key`, `comment_body_template_id`, `comment_body_artifact_path`, `resolution_required`, `status` (`pending`, `completed`, `skipped`, or `failed`), and `reason`.

### `pr-feedback-marker-report.json` — marker report/idempotency

| Field | Type | Required | Notes |
|---|---:|---|---|
| Common binding/common fields | mixed | yes | Bound to current marker execution head. |
| `marker_state` | enum | yes | `complete`, `partial`, or `fatal`. |
| `marker_actions` | array | yes | Attempted actions and outcomes. |
| `skipped_actions` | array | yes | Intentional skips with reasons. |
| `remote_marker_comments_seen` | array | yes | Existing hidden markers discovered remotely. |
| `resolved_threads` | array | yes | Resolution successes. |
| `posted_comments` | array | yes | Posted comment IDs/URLs/body hashes. |
| `pending_feedback_marker_actions_artifact_sequence` | integer or null | yes | Queue consumed by marker. |

Marker actions include `action_id`, `action_kind`, `item_id`, `stable_marker_key`, `idempotency_key`, `hidden_marker`, `body_hash`, `source_head_sha`, `remediation_output_head_sha`, `comment_posted`, `comment_id`, `comment_url`, `resolution_attempted`, `resolved`, `skipped_reason`, `api_operation`, `api_error_class`, `completed_at`, and `idempotency_state` (`new_action`, `already_completed_local`, `already_completed_remote`, or `ambiguous`). Ambiguous idempotency is fatal.

### `post-pr-failure-terminal.json` — terminal non-success log

| Field | Type | Required | Notes |
|---|---:|---|---|
| Common binding/common fields | mixed | yes | Best-effort binding from current `pr.json`; if identity is unavailable, failure details record why. |
| `terminal_state` | enum | yes | `fatal`. |
| `failed_step` | string | yes | Step that caused terminal route. |
| `failure_reason` | string | yes | Deterministic terminal reason. |
| `source_artifacts` | array | yes | Candidate source failure artifacts. |
| `source_failure_sequence` | integer | yes | Selected source failure sequence. |
| `source_artifact_sequence` | integer | yes | Selected source artifact sequence. |
| `source_write_sequence` | integer | yes | Selected source write sequence. |
| `source_producer_step_id` | string | yes | Selected source producer step. |
| `source_step_order_index` | integer | yes | Selected source order. |
| `source_artifact_path` | string | yes | Current selected source path. |
| `source_history_path` | string | yes | History selected source path. |
| `selected_source_reason` | string | yes | Why this source was chosen deterministically. |
| `logged_at` | string | yes | RFC3339. |

## Fixture examples

Phase 01 JSON examples live under `tests/fixtures/github_pr/`. They are intentionally contract fixtures, not GitHub API fixtures. They must parse with `python3 -m json.tool` and expose the required terms used by P04 tests: `schema_version`, `run_id`, `repository_owner`, `repository_name`, `pr_number`, `head_sha`, `artifact_sequence`, `write_sequence`, `failure_sequence`, `pending-feedback-marker-actions`, `pr-remediation-result`, and `coderabbit-feedback-state`.

## P04 test obligations derived from this contract

P04 schema/contract tests must reject missing required common fields, missing binding fields, missing failure fields for non-success states, mismatched current-head bindings, stale-only success, unknown/pending evidence in `must_fix`, malformed evaluator decisions, duplicate accepted evaluations, invalid remediation status values, missing structured evidence for `already_satisfied`/`not_reproduced`, marker idempotency ambiguity, non-monotonic sequence numbers, missing history metadata, and terminal source selection by mtime rather than sequence metadata.
