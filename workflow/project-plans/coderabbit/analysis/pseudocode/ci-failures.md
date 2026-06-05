# Pseudocode: CI Failure Collection

Plan: `PLAN-20260429-CODERABBIT-PR-FOLLOWUP`

1. `collect_ci_failures` reads current `pr.json` and current `pr-check-status.json` through the artifact store.
2. Validate repository, PR number, head ref, head SHA, base ref, base SHA, schema version, artifact sequence, and producer metadata.
3. If watcher binding is stale or untrusted, write `collection_state=fatal` with source artifact details and return fatal.
4. Initialize output arrays `failures=[]`, `pending_or_unknown=[]`, and `log_artifacts=[]`.
5. If `pr-check-status.json.fatal_source` is present, copy it into `watcher_fatal_source` and add watcher evidence to `pending_or_unknown`.
6. For every final current-head failed, cancelled, timed-out, action-required, or startup-failure check, create a deterministic failure ID from check ID/name/head SHA.
7. Preserve failure check name, conclusion, state, URL, run ID, job ID, workflow name, and source check status artifact sequence.
8. When run ID and job ID are available, fetch Actions job log metadata and logs using the documented log API path.
9. If a check maps only to a workflow run, fetch run jobs with pagination and identify matching jobs before fetching logs.
10. If logs are available, store full raw logs under the artifact store log area and place bounded excerpts in JSON.
11. If logs are unavailable because the source is not Actions, record `log_status=not_applicable` without deleting the failure.
12. If logs cannot be fetched because of permission, expiry, rate limit, or transport failure, record `log_status=fetch_failed` or `unavailable` with deterministic error class.
13. For every pending timeout, unknown, stale-only, schema-unbindable, or watcher-fatal record, add a `pending_or_unknown` entry with source, reason, and raw evidence path.
14. Do not put pending, unknown, stale-only, or watcher-fatal evidence into `failures` or downstream `must_fix`.
15. If `pending_or_unknown` is non-empty, write `collection_state=fatal`, allocate `failure_sequence`, and return fatal.
16. If watcher fatal source is present, write `collection_state=fatal`, preserve watcher evidence, allocate `failure_sequence`, and return fatal.
17. If checks passed with no failures, write `collection_state=collected`, empty arrays, source artifact sequence, and return success.
18. If all current-head checks are terminal and concrete failures exist with no pending, unknown, stale-only, or watcher-fatal evidence, write `collection_state=collected` and return success.
19. On API auth failure, write permission-denied metadata fields required by the API contract and return fatal.
20. On artifact write failure after best-effort fatal evidence cannot be written, return engine error according to existing artifact-store error policy.
21. Downstream `build_remediation_plan` may consume only `collection_state=collected` artifacts and only the `failures` array for CI `must_fix` items.
