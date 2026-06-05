# Pseudocode: Feedback Evaluation

Plan: `PLAN-20260429-CODERABBIT-PR-FOLLOWUP`

1. `evaluate_coderabbit_feedback` reads current `pr.json`, `coderabbit-feedback.json`, and `coderabbit-feedback-state.json` through the artifact store.
2. Validate all input artifacts share repository, PR number, head ref, head SHA, base ref, and current artifact sequences.
3. If `coderabbit-feedback.json.readiness_state` is not `ready`, write `evaluation_state=fatal` with source artifact evidence and return fatal.
4. Build a deterministic sorted list of current feedback items by source surface, stable marker key, body hash, and item ID.
5. For each item, look up exactly matching accepted evaluation state by repository, PR, stable marker key, body hash, and head SHA.
6. If exactly one binding-valid accepted evaluation exists, append it to `accepted_results` with `source=reused` and do not invoke the LLM for that item.
7. If duplicate, stale, malformed, or unbindable accepted evaluation state exists for the current key, write fatal evidence and return fatal.
8. For an unevaluated item, build a one-item evaluation request containing item ID, stable marker key, body hash, head SHA, body, URL/path context, issue/plan context, and allowed decision enum.
9. Invoke the configured LLM command or adapter for that single item only, using argv-safe parameters and bounded raw response capture.
10. Parse the LLM response as strict JSON for one item and reject free-form-only output.
11. Validate response item ID, stable marker key, body hash, head SHA, decision enum, reason requirements, and recommended action.
12. Accepted decisions are exactly `valid`, `invalid`, `out_of_scope`, or `needs_user_judgment`.
13. Reject responses that contain extra item IDs, missing reason for non-valid decisions, unknown decisions, or body/head mismatches.
14. Record each rejected attempt with attempt number, reject reason, parsed decision when available, raw response path, and observed head SHA.
15. Retry malformed or rejected output until `max_attempts_per_item=3` attempts are exhausted for that item.
16. Once a valid response is accepted, append it once to `accepted_results` with `source=new`, `attempt_count`, accepted timestamp, reason, and recommended action.
17. Persist each newly accepted evaluation into `coderabbit-feedback-state.json` before returning success, so a same-head rerun can reuse it.
18. If any current item exhausts its malformed-output budget, record it in `budget_exhausted_items` and do not fabricate an accepted result.
19. If any current item remains unevaluated for non-budget reasons, record it in `unevaluated_items`.
20. If every current item has exactly one accepted result and there are no rejected terminal gaps, write `evaluation_state=complete` and return success.
21. If any current item lacks an accepted result after retries, write `evaluation_state=budget_exhausted` or `fatal`, allocate failure sequence, and return fatal.
22. Do not return fixable to the workflow for malformed evaluator output; retries are internal to this executor in v1.
23. Downstream remediation may consume only `accepted_results` with `decision=valid` and must ignore rejected, budget-exhausted, and unevaluated records except as fatal evidence.
