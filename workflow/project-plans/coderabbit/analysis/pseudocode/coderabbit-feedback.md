# Pseudocode: CodeRabbit Feedback Collection and Readiness

Plan: `PLAN-20260429-CODERABBIT-PR-FOLLOWUP`

1. `collect_coderabbit_feedback` reads current `pr.json`, current feedback state, and optional pending marker actions.
2. Load configured CodeRabbit identities, defaulting to `coderabbitai`, `coderabbitai[bot]`, and fixture-observed bot variants.
3. Initialize readiness budget with `max_observations=6`, `required_stable_observations=2`, and `observation_interval_seconds=300` unless configured otherwise.
4. For each observation, query GraphQL review threads first using the documented cursor contract.
5. Query REST review comments and issue comments using documented pagination to cover surfaces GraphQL does not expose.
6. Query check-run or review-status readiness signals for configured CodeRabbit identities on the current head.
7. Normalize review threads into feedback items with thread ID, comment ID, author, body, body hash, path, line, side, URL, timestamps, resolved state, outdated state, and commit SHA.
8. Normalize REST review comments without usable thread identity into inline feedback items with stable fallback keys and `resolution_state_available=false`.
9. Normalize issue comments or check summaries as summary items unless deterministic splitting is explicitly supported by a later contract.
10. Filter out non-CodeRabbit authors and record them as noise only in observation metadata.
11. Exclude already resolved review threads by default while retaining enough state for idempotency and audit.
12. Mark items whose commit SHA or observed head differs from `pr.json.head_sha` as stale unless the contract explicitly identifies them as current-head summary feedback.
13. Build `stable_marker_key` from thread ID, comment ID, review ID, or a deterministic source/body/path/hash fallback.
14. Compute `feedback_item_set_hash` from sorted current unresolved item keys and body hashes.
15. Detect ready signals from current-head CodeRabbit completed checks, completed reviews, or completed summary comments according to the truth table in the API contract.
16. Treat in-progress CodeRabbit check or review signals as overriding ready review/comment signals for the same current head.
17. Treat missing CodeRabbit check as allowed only when another documented current-head ready signal exists and feedback stabilizes.
18. Increment stable observation count only when ready is true and the normalized item-set hash matches the immediately previous ready observation.
19. Reset stable observation count when feedback changes, current-head signals change materially, or readiness becomes false.
20. After each observation, write `coderabbit-feedback.json` and `coderabbit-feedback-state.json` with observations, items, state entries, and budget counters.
21. If stable ready count reaches the required count, write `readiness_state=ready` and return success.
22. If an observation has API auth, schema, partial required-surface failure, or unsupported configuration fatality, write `readiness_state=fatal`, allocate failure sequence, and return fatal.
23. If the observation budget is exhausted before stable readiness, write `readiness_state=timeout`, allocate failure sequence, and return fatal.
24. If budget remains and readiness is not stable, sleep exactly once between non-final observations.
25. Do not sleep before the first observation and do not sleep after a ready, fatal, timeout, or final-budget observation.
26. Update feedback state entries with first seen, last seen, evaluation status, marker status, resolution status, superseded status, stale flag, and reuse eligibility.
27. Preserve prior accepted evaluation references only when repository, PR, stable marker key, body hash, and head SHA match exactly.
28. Treat ambiguous or duplicate prior accepted state for the same stable marker key/body hash/head SHA as fatal for evaluator consumption.
29. Current clean or empty feedback is clean only when `readiness_state=ready` and stable observation criteria are met.
