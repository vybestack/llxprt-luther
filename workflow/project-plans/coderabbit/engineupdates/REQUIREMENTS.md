# CodeRabbit PR Follow-Through Engine Updates — EARS Requirements

## Metadata

- Feature: CodeRabbit PR Follow-Through Engine Support
- Scope: Engine-level reusable deterministic GitHub PR supervision and feedback artifacts for Luther workflows.
- Related specs:
  - `project-plans/coderabbit/engineupdates/overview.md`
  - `project-plans/coderabbit/workflowupdates/overview.md`

## Ubiquitous Requirements

### REQ-PRFU-001: Deterministic PR Identity Capture

The system shall produce a deterministic PR identity artifact before any PR follow-through step runs.

- The artifact shall include repository owner/name, PR number, PR URL, head ref, head SHA, base ref, and capture timestamp.
- The artifact shall be valid JSON and include `schema_version`.
- Downstream PR follow-through artifacts shall repeat PR number, head ref, head SHA, and base ref.

### REQ-PRFU-002: Current-Head Binding

The system shall bind PR checks, CI failures, CodeRabbit feedback, feedback evaluations, remediation plans, remediation results, and marker reports to the current PR head SHA.

- The system shall not treat stale artifacts or stale check runs from older head SHAs as evidence that the current PR head is clean.
- The system shall report stale data in structured artifacts for auditability.

### REQ-PRFU-003: Structured Artifact Contract

The system shall write machine-readable JSON artifacts for PR identity, check status, CI failures, CodeRabbit feedback, feedback state, feedback evaluations, remediation plans, remediation results, and marker actions.

- Each JSON artifact shall include `schema_version`.
- Each artifact shall include enough identifiers to correlate records across workflow iterations.
- Raw logs may be written as non-JSON files, but their metadata shall be represented in JSON.

### REQ-PRFU-004: Bounded PR Check Watching

The system shall watch PR checks for a configurable bounded duration with a configurable polling interval.

- The default polling interval shall be 300 seconds.
- The default maximum duration shall be 3600 seconds.
- The default maximum attempts shall therefore be 12 bounded polling cycles for the current watch invocation.
- The attempt budget semantics shall be fixture-tested so that the default watch cannot exceed the one-hour budget except for the time required to complete the final in-flight GitHub request.
- The system shall write a status artifact after every polling cycle.

### REQ-PRFU-005: Non-Fail-Fast Check Watching

The system shall not stop check watching on the first failed check while other current-head checks remain pending and the watch budget remains.

- The system shall continue polling until all current-head checks are terminal or the watch budget is exhausted.
- The system shall classify all current-head failures together after polling completes or budget is exhausted.

### REQ-PRFU-006: Check-State Classification

The system shall classify PR checks from structured GitHub data, not from human-readable table output.

- The system shall classify passing, neutral, skipped, failed, cancelled, pending, unknown, and stale states according to a fixture-tested policy.
- Unknown current-head states shall prevent false success.
- Cancelled current-head checks shall be terminal and non-passing unless later proven stale.

### REQ-PRFU-007: CI Failure Collection

The system shall collect deterministic metadata for each failed, cancelled, timed-out, action-required, or unknown current-head check after watch completion.

- The system shall collect check name, state/conclusion, URL, run ID/job ID when available, and a bounded log excerpt when logs are available.
- The system shall record log availability as `available`, `unavailable`, `not_applicable`, or `fetch_failed`.
- Missing logs shall not erase the check failure from the artifact.

### REQ-PRFU-008: CodeRabbit Feedback Collection

The system shall collect CodeRabbit feedback deterministically from GitHub PR issue comments, reviews, inline review comments, and review threads.

- The system shall include only configured CodeRabbit identities by default, with `coderabbitai` and configured bot-account variants observed in fixtures as the default identities.
- The system shall preserve stable IDs, URLs, thread IDs, paths, line numbers, resolution state when available, timestamps, and raw bodies.
- The system shall record whether resolution state is available for each review-thread item.
- The system shall exclude already resolved review threads by default unless the workflow explicitly requests all feedback.
- The system shall write `coderabbit-feedback.json` even when no feedback items are found.

### REQ-PRFU-009: CodeRabbit Readiness and Stabilization

The system shall distinguish “no CodeRabbit feedback exists yet” from “CodeRabbit feedback collection is complete enough to evaluate.”

- The system shall determine readiness using configured CodeRabbit check status, review completion signals, or deterministic feedback stabilization policy.
- Empty feedback shall not be treated as clean until readiness or stabilization criteria have been met for the current PR head.
- If readiness cannot be established within the configured budget, the system shall produce a structured non-success outcome representable by the current workflow schema.
- The default readiness/stabilization budget shall be bounded by workflow loop caps and shall not allow an unbounded retry loop.

### REQ-PRFU-010: Feedback State and Deduplication

The system shall maintain deterministic feedback state across workflow iterations and resumptions.

- The state shall key feedback by stable GitHub ID plus body hash or update timestamp and current head SHA.
- The state shall record first seen time, last seen time, evaluation status, remediation status, marker/comment status, and resolution status.
- The system shall not repeatedly evaluate or comment on unchanged feedback items that were already handled for the same head SHA.

### REQ-PRFU-011: Per-Item LLM Evaluation Contract

The system shall produce exactly one accepted validated feedback evaluation result per CodeRabbit feedback item for the current item body and head SHA.

- The system may retry malformed LLM output per item within a configured retry cap.
- The final evaluation artifact shall contain exactly one validated result for each input item ID and no extra item IDs.
- Each evaluation shall include item ID or item key, body hash, head SHA, decision, reason, recommended action, accepted timestamp, and attempt count.
- Allowed decisions shall be `valid`, `invalid`, `out_of_scope`, and `needs_user_judgment`.
- Previously accepted unchanged evaluations may be reused from feedback state, but they shall still appear exactly once in the current aggregate evaluation artifact.

### REQ-PRFU-012: Evaluation JSON Validation

The system shall validate every LLM feedback evaluation result against the required schema before using it for remediation or commenting.

- Malformed JSON shall not be treated as a valid evaluation.
- Unknown decisions shall not be silently accepted.
- Mismatched item ID, item key, body hash, or head SHA shall not be accepted.
- Missing reasons shall not be accepted for invalid, out-of-scope, or needs-user-judgment decisions.

### REQ-PRFU-013: Deterministic Remediation Plan Aggregation

The system shall aggregate CI failures and validated feedback evaluations into a deterministic remediation plan.

- The remediation plan shall place CI failures and `valid` feedback items into `must_fix`.
- The remediation plan shall place `invalid` and `out_of_scope` feedback items into `mark_invalid` with reasons.
- The remediation plan shall place `needs_user_judgment`, watch timeouts, unknown persistent check states, and unresolved ambiguity into `needs_user_judgment`.
- The remediation plan shall be clean only when there are no current-head CI failures, no valid feedback items requiring fixes, and no needs-user-judgment items.

### REQ-PRFU-014: Structured Remediation Result

The system shall require a structured remediation result artifact after any PR remediation LLM step.

- The artifact shall record one result per `must_fix` feedback item and one result per CI failure.
- Each result shall include status, action taken, and evidence paths or explanation.
- Each result artifact shall record the input head SHA, output head SHA, overall remediation status, and verification command results when available.
- The system shall validate that every `must_fix` item has a remediation result before any marker action uses the artifact.
- The marker step shall not claim a valid feedback item was fixed unless the remediation result records it as fixed.

### REQ-PRFU-015: Deterministic Feedback Marking

The system shall mark CodeRabbit feedback using structured evaluation and remediation artifacts, not free-form LLM text.

- For fixed valid items, the system shall comment with recorded action taken and resolve the review thread when possible.
- For invalid or out-of-scope items, the system shall comment with the recorded reason and resolve the review thread when policy allows.
- For needs-user-judgment items, the system shall not resolve automatically.
- Comment bodies shall be generated from deterministic templates and structured artifact fields.
- The marker report shall record skipped marker actions and skipped reasons when policy intentionally avoids comment or resolution.

### REQ-PRFU-016: Marker Idempotency

The system shall avoid duplicate feedback comments and duplicate review-thread resolution attempts across retries and resumed runs.

- The marker report shall record posted comment IDs, posted comment URLs, body hashes, resolved thread IDs, and timestamps.
- Before posting, the system shall detect whether the same marker action has already been completed for the same item/body/head SHA.

### REQ-PRFU-017: Shell Safety for GitHub Text

The system shall not interpolate untrusted GitHub text or LLM text directly into shell-quoted `gh` invocations.

- Comment bodies shall be passed through files or process arguments that avoid shell interpretation.
- Raw CodeRabbit bodies shall never be embedded in shell command strings.
- Backticks and command substitutions in review text shall not be executable by the shell.

### REQ-PRFU-018: Workflow-Representable Outcomes

The system shall return step outcomes that can be routed by the current workflow transition schema.

- The system shall not require `StepOutcome::Abandon` to route to cleanup/logging unless a tested engine change makes `abandon` routable.
- Watch timeouts, readiness timeouts, and needs-user-judgment conditions shall map to outcomes that the workflow can handle deterministically.
- The system shall not require ambiguous branching such as one outcome selecting between multiple next steps without an intervening deterministic classifier artifact or a distinct representable outcome.
- Fatal post-PR conditions shall be representable as routes to `abandon_and_log` or another existing terminal failure step under the current workflow engine.

### REQ-PRFU-019: No-Network Default Tests

The system shall provide deterministic tests that do not require live GitHub network access by default.

- Tests shall use fixture GitHub JSON payloads or fake `gh` executables.
- Tests shall cover check classification, stale head handling, CodeRabbit normalization, evaluation validation, remediation aggregation, and marker idempotency.

### REQ-PRFU-020: Integration Reachability

The system shall register new PR follow-through executors with the default executor registry and make them reachable from workflow TOML.

- The `llxprt-issue-fix-v1` workflow shall be able to invoke the executors after PR creation.
- Workflow graph tests shall prove the PR does not complete immediately after `create_pr`.
- Workflow graph tests shall prove `create_pr` cannot transition directly to `log_completion` for the PR follow-through path.
- Workflow graph tests shall prove post-remediation verification and push can loop back to PR identity capture with configured loop caps.

### REQ-PRFU-020A: Workflow Loop Caps

The system shall expose deterministic configuration and artifact state needed for the workflow to enforce post-PR loop caps.

- The default maximum PR remediation iterations after PR creation shall be 3.
- The default maximum remediation LLM retry/self-loop count shall be 2.
- The default maximum malformed feedback-evaluation retry count shall be 2 per item.
- Loop-cap exhaustion shall produce structured non-success outcomes representable by the current workflow schema.

## Event-Driven Requirements

### REQ-PRFU-021: When PR Head Changes During Follow-Through

When the PR head SHA changes during follow-through, the system shall treat prior current-head-bound check, feedback, evaluation, remediation, and marker artifacts as stale unless explicitly refreshed for the new head.

### REQ-PRFU-022: When Checks Remain Pending After Budget

When current-head checks remain pending after the configured watch budget, the system shall write a structured timeout report and route to a non-success outcome representable by the workflow.

### REQ-PRFU-023: When GitHub API Authentication Fails

When GitHub API or CLI authentication fails, the system shall return a fatal outcome and write diagnostic details without invoking the LLM for guesswork.

### REQ-PRFU-024: When CodeRabbit Feedback Changes

When a CodeRabbit feedback item's body, update timestamp, or thread state changes, the system shall treat it as a new evaluation candidate for the current head while retaining prior state for auditability.

### REQ-PRFU-025: When LLM Feedback Evaluation Is Malformed

When an LLM feedback evaluation response is malformed, the system shall retry that item within the configured cap or return a non-success outcome without fabricating an evaluation.

### REQ-PRFU-026: When Comment Posting Succeeds but Thread Resolution Fails

When feedback comment posting succeeds but review-thread resolution fails, the system shall record both facts in the marker report and return a non-success outcome only if policy requires thread resolution for completion.

## State-Driven Requirements

### REQ-PRFU-027: While Checks Are Pending

While any current-head check is pending and watch budget remains, the system shall continue polling rather than routing to remediation or completion.

### REQ-PRFU-028: While Valid Feedback Is Unfixed

While any feedback item evaluated as valid lacks a fixed remediation result, the system shall not mark that feedback item resolved or complete the workflow as clean.

### REQ-PRFU-029: While Needs-User-Judgment Items Exist

While any current-head feedback item or PR follow-through condition requires user judgment, the system shall not complete the PR follow-through workflow as successful.

### REQ-PRFU-030: While Stale Data Exists

While only stale check or feedback data exists for a PR, the system shall refresh current-head data or route to a non-success state; it shall not infer success from stale data.

## Optional Requirements

### REQ-PRFU-031: Optional Fail-Fast Check Watching

Where configured explicitly, the system may support fail-fast check watching, but the default shall remain non-fail-fast.

### REQ-PRFU-032: Optional External CI Log Fallback

Where a failed check has no GitHub Actions log, the system may store only check metadata and URL, provided the log status is explicit in the CI failure artifact.

## Unwanted Behavior Requirements

### REQ-PRFU-033: No LLM CI-State Decisions

The system shall never ask the LLM to decide whether PR checks are complete, pending, failed, or passed.

### REQ-PRFU-034: No LLM Feedback Discovery

The system shall never rely on the LLM to discover CodeRabbit comments, review threads, or failed check logs.

### REQ-PRFU-035: No False Clean Completion

The system shall not complete PR follow-through successfully unless current-head checks are passing, CodeRabbit readiness/stabilization has been established, actionable feedback has been handled, and marker actions required by policy have completed or are unnecessary.
