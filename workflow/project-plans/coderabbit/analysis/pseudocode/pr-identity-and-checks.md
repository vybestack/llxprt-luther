# Pseudocode: PR Identity, Iteration Guard, and Check Watching

Plan: `PLAN-20260429-CODERABBIT-PR-FOLLOWUP`

1. `capture_pr_identity` receives `artifact_root`, repository owner/name, and the PR URL or PR number produced by `create_pr`.
2. Require `artifact_root` to exist after variable expansion, work-dir-relative resolution, directory creation, and canonicalization; otherwise return configuration fatal before downstream work.
3. Query `gh pr view` for number, url, headRefName, headRefOid, baseRefName, baseRefOid, state, isDraft, and id using the argv contract in `github-api-contract.md`.
4. If `gh pr view` lacks any binding field, query the GraphQL PR identity fallback fixture path and merge only documented fields.
5. Reject closed, merged, draft-unacceptable, missing, or mismatched PR identity fields as `capture_state=fatal`.
6. Write `pr.json` through the artifact store with `capture_state=captured`, binding fields, PR node ID, source metadata, and sequence metadata.
7. Return `StepOutcome::Success` only after canonical current and immutable history writes both succeed.
8. `post_pr_iteration_guard` reads current `pr.json` and all same-run guard history snapshots for the same repository and PR.
9. Ignore older-run, different PR, different repository, or stale head artifacts only after recording their paths in `ignored_stale_artifacts`.
10. If no accepted guard exists, write `iteration_index=0`, `previous_head_sha=null`, `reason=initial_entry`, `guard_state=proceed`, and return success.
11. If the latest accepted guard has the same `head_sha`, write a new guard with the same `iteration_index`, `previous_head_sha=head_sha`, `reason=same_head_reentry`, and return success.
12. If the current `head_sha` differs, increment `iteration_index` and set `previous_head_sha` to the prior accepted head.
13. If the incremented index is at most `max_post_pr_remediation_iterations`, write `reason=head_sha_changed_after_remediation_push`, `guard_state=proceed`, and return success.
14. If the incremented index exceeds the cap, write `guard_state=max_iterations_exceeded`, allocate `failure_sequence`, and return fatal so the workflow reaches `post_pr_failure_terminal`.
15. If guard history is unreadable, duplicate-sequenced, or unbindable for the active run, write `guard_state=fatal`, `reason=unreadable_or_unbindable_guard_state`, and return fatal.
16. `watch_pr_checks` reads current `pr.json` and initializes observation budget from `max_attempts=12`, `poll_interval_seconds=300`, and injectable clock/sleeper.
17. For each observation attempt, query structured check status using the documented preferred check command and fallback API order.
18. Normalize every returned check into name, status, conclusion, state bucket, URL, workflow name, run ID, job ID, timestamps, app slug, source, and observed head SHA.
19. Classify checks whose head SHA differs from `pr.json.head_sha` as stale and exclude them from pass/fail decisions.
20. Classify current-head terminal success, neutral, and skipped as passing buckets while recording neutral and skipped separately.
21. Classify failure, startup_failure, timed_out, action_required, and cancelled as terminal failing buckets.
22. Classify queued, requested, waiting, pending, in_progress, and no-conclusion nonterminal checks as pending.
23. Classify missing or unrecognized status/conclusion pairs as unknown.
24. If an API, auth, schema, or artifact write error occurs, write `overall_state=fatal` with `fatal_source` and return fatal after persisting evidence.
25. After each observation, append the observation to `poll_observations` and atomically rewrite `pr-check-status.json` current plus history.
26. If all trusted current-head checks are terminal and no unknown exists, stop without sleeping after that observation.
27. If current-head pending checks remain and budget remains, sleep exactly once between non-final observations.
28. Do not sleep before the first observation and do not sleep after the final budget observation.
29. After polling, compute precedence: fatal, pending_timeout, unknown, failed, passed.
30. If no trusted current-head checks are available and stale data exists, classify as fatal or unknown according to the documented source evidence rather than success.
31. Write final `pr-check-status.json` with `overall_state`, `checks`, `stale_checks`, terminal counts, observation count, and source metadata.
32. Return success for `overall_state=passed`, fixable for `overall_state=failed`, and fatal for `pending_timeout`, `unknown`, or `fatal`.
33. The workflow routes every watch outcome to `collect_ci_failures`, so the watcher artifact is always bridged by the deterministic CI failure classifier.
