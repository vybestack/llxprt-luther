# Pseudocode: Remediation Plan, Validation, Push, Marker, and Terminal

Plan: `PLAN-20260429-CODERABBIT-PR-FOLLOWUP`

1. `build_remediation_plan` reads current PR identity, check status, CI failures, feedback, feedback state, and feedback evaluations.
2. Validate all consumed current-head artifacts by binding fields and artifact sequences before reading semantic arrays.
3. If any input artifact is missing, stale, malformed, or unbindable, write `plan_state=fatal`, allocate failure sequence, and return fatal.
4. If CI failure collection contains `pending_or_unknown`, copy that evidence to `pending_or_unknown`, set `plan_state=blocked_needs_user_judgment`, and return fatal.
5. Add each concrete CI failure from `ci-failures.json.failures` to `must_fix` with source type `ci_failure`.
6. Add each accepted CodeRabbit evaluation with `decision=valid` to `must_fix` with source type `coderabbit_feedback`.
7. Add accepted `invalid` and `out_of_scope` evaluations to `mark_invalid` with deterministic reasons and marker obligations.
8. Add accepted `needs_user_judgment` evaluations to `needs_user_judgment` and return fatal after writing the plan.
9. Create or update `pending-feedback-marker-actions.json` for every invalid, out-of-scope, fixed-valid, or resolution-required action that must survive head changes.
10. If `must_fix` is empty and no user-judgment blockers exist, write `plan_state=clean` and return success.
11. If `must_fix` is non-empty and no blockers exist, write `plan_state=needs_remediation` and return fixable.
12. `remediate_pr_followup` reads only current binding-valid PR, CI failure, feedback evaluation, and remediation plan artifacts.
13. Render the configured remediation prompt requiring fixes only for `must_fix`, no changes for `mark_invalid` or `needs_user_judgment`, and structured `pr-remediation-result.json` output.
14. Pass remediation plan path, result path, input head SHA, repository fields, PR number, head/base refs, artifact root, and optional success file as explicit argv-safe parameters.
15. Write `pr-remediation-llxprt-run.json` for every invocation with argv, exit status, signal, timeout/spawn class, bounded logs, full log paths, and validator-readable-result flag.
16. Return wrapper success when a validator-readable result or failure artifact exists, even if llxprt reported timeout, spawn failure, retryable failure, or success without complete remediation.
17. Return wrapper fatal only when no validator-readable artifact can be produced or terminal logging must run immediately.
18. `validate_remediation_result` reads current plan, remediation run, remediation result, git head state, and same-scope validation history.
19. Validate schema version, binding fields, input head SHA, output head SHA, plan artifact sequence, result count, canonical status enum, and structured evidence.
20. Require one result for every `must_fix` item and no unbound extra source IDs.
21. Treat `fixed`, `changed`, `already_satisfied`, and `not_reproduced` as validator-success statuses only with deterministic evidence tied to the same input head.
22. Require `already_satisfied` and `not_reproduced` evidence to include kind, current head SHA, and at least one path, command, API lookup, or check-run proof.
23. Treat `not_fixed`, `skipped`, and `failed` as structurally valid but unsuccessful statuses.
24. If result is missing or fixably malformed and validation retry cap remains, write `validation_state=fixable_malformed`, increment retry index, and return fixable.
25. If validation retry cap is exhausted, write `validation_state=fixable_cap_exhausted`, allocate failure sequence, and return fatal.
26. If structurally valid result is unsuccessful and remediation attempt cap remains, write `validation_state=valid_but_unsuccessful` and return fixable.
27. If unsuccessful remediation cap is exhausted, write `validation_state=unsuccessful_remediation_cap_exhausted`, allocate failure sequence, and return fatal.
28. If every `must_fix` item has validator-success evidence, write `validation_state=valid`, update pending marker actions for fixed items, and return success.
29. `run_post_pr_tests` runs only configured post-PR verification commands using argv-safe command runner contracts.
30. Record each command ID or argv, status, exit code, bounded stdout/stderr, full log paths, and injectable-clock timestamps.
31. If all configured commands pass, write `test_state=passed` and return success.
32. If tests fail and verification retry cap remains, write `test_state=failed`, increment retry index, and return fixable.
33. If verification cap is exhausted or infrastructure fails, write `test_state=fatal` or failed-exhausted evidence, allocate failure sequence, and return fatal.
34. `push_remediation_changes` validates PR, plan, remediation result, and post-PR test artifacts before inspecting git state.
35. Record local head, remote PR head, PR artifact head, target ref, and deterministic status before staging.
36. Stage only included paths after applying workflow-equivalent exclusions, never staging project memory unless explicitly permitted by workflow configuration.
37. If no included changes exist, return success only when remote already matches expected local head and validator-success evidence covers all `must_fix` items.
38. If included changes exist, create one deterministic commit and push using safe argv commands.
39. Verify remote PR head equals committed head before returning success with `push_state=pushed`.
40. Classify remote mismatch, unsafe paths, command-runner failures, or artifact failures as retryable or fatal according to the push contract and artifact-backed cap.
41. `mark_coderabbit_feedback` reads current clean feedback/evaluations plus pending marker actions carried forward from prior heads.
42. Discover existing remote marker comments through documented paginated surfaces before posting new comments.
43. For each pending action, compute separate idempotency keys for comment creation and thread resolution using action kind, stable marker key, source head, remediation output head, body hash, template key, run ID, repository, and PR.
44. If a local or remote completed marker exists for the idempotency key, record `already_completed_local` or `already_completed_remote` and avoid duplicate comment or resolution.
45. Post deterministic comments using file body or GraphQL variables, never shell interpolation of GitHub or LLM text.
46. Resolve review threads only after local and remote idempotency checks pass and the policy row requires resolution.
47. Record posted comment IDs, URLs, body hashes, resolved thread IDs, skipped reasons, API operation metadata, and failure classes.
48. If all required marker actions complete or are intentionally skipped by policy, write `marker_state=complete` and return success.
49. If any required marker action is partial, ambiguous, or failed, write `marker_state=partial` or `fatal`, allocate failure sequence, and return fatal.
50. `post_pr_failure_terminal` scans same-run PR follow-through history for non-success artifacts with failure sequence metadata.
51. Sort candidate failures by failure sequence, artifact sequence, produced timestamp, step order index, producer step ID, and path.
52. Select the first sorted failure source, or record `unknown_current_context_only` if no usable source exists.
53. Write `post-pr-failure-terminal.json` with selected source metadata and return fatal as the only terminal outcome.
