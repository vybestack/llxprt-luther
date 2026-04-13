# Pseudocode: Engine Runner and Routing

1. Load resumable checkpoint if run resumes; otherwise start at entry step.
2. Execute current step through adapter boundary.
3. Collect structured step outcome (`success`, `retryable`, `fatal`, etc.).
4. Append structured event for executed step and outcome.
5. Persist checkpoint after event append.
6. Evaluate transition table using structured outcome (never unstructured log matching).
7. Apply retry and loop counters from config guard limits.
8. If loop/retry limit exceeded, transition to configured abandonment/terminal outcome.
9. If fatal error outcome, route to terminal failure handling.
10. If shutdown signal received, persist resumable checkpoint and exit cleanly.
11. Otherwise advance to next step and repeat.
12. On terminal success/failure, write terminal artifacts and final run state.
