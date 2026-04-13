# Pseudocode: Repository Workspace and Branch Preparation

1. Read repository policy from workflow config (`repo`, `workspace`, `branch`).
2. Resolve working directory path based on `workspace.strategy`.
3. For `shared`, reuse configured workspace path.
4. For `per-run`, derive workspace path from template plus `run_id/config_id`.
5. Ensure checkout exists (clone/fetch as configured).
6. Checkout configured base branch.
7. Derive run branch name from `branch.name_template`.
8. If branch exists, switch to it.
9. If branch missing and `create_if_missing=true`, create and switch.
10. If `force_reset=true`, hard-reset run branch to base branch.
11. If any checkout/fetch/branch operation fails, return structured init failure and abort run.
12. Return prepared workspace context to engine initialization.
