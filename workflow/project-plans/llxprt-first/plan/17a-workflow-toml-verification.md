# Phase 17a: Workflow TOML -- Verification

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P17a`

## Prerequisites

- Required: Phase 17 completed

## Verification Commands

```bash
# Files exist
test -f config/workflows/llxprt-issue-fix-v1.toml && echo "OK" || echo "MISSING"
test -f config/workflow-configs/llxprt-code.toml && echo "OK" || echo "MISSING"
test -f tests/fixtures/workflows/valid/llxprt-issue-fix-v1.toml && echo "OK" || echo "MISSING"
test -f tests/fixtures/workflow-configs/valid/llxprt-code.toml && echo "OK" || echo "MISSING"

# Plan markers
grep "@plan:PLAN-20260408-LLXPRT-FIRST.P17" config/workflows/llxprt-issue-fix-v1.toml
grep "@plan:PLAN-20260408-LLXPRT-FIRST.P17" config/workflow-configs/llxprt-code.toml

# No hardcoded profiles in workflow type
grep -i "opusthinking\|gpt54xhigh\|sonnetthinking" config/workflows/llxprt-issue-fix-v1.toml
# Expected: no output

# Step count
grep -c 'step_id = ' config/workflows/llxprt-issue-fix-v1.toml
# Expected: 15

# Transition count
grep -c '^\[\[transitions\]\]' config/workflows/llxprt-issue-fix-v1.toml
# Expected: 25+

# Per-edge limits
grep -c "max_iterations" config/workflows/llxprt-issue-fix-v1.toml
# Expected: 3

# Verify executor step
grep 'step_type = "verify"' config/workflows/llxprt-issue-fix-v1.toml
# Expected: 1 match

# Separate push and PR steps (REQ-LF-PR-004)
grep 'step_id = "push_changes"' config/workflows/llxprt-issue-fix-v1.toml
grep 'step_id = "create_pr"' config/workflows/llxprt-issue-fix-v1.toml
# Expected: both found as separate steps

# Config variables section
grep "\[variables\]" config/workflow-configs/llxprt-code.toml
# Expected: found

# TOML parseable
cargo build --all-targets
cargo test
```

### Semantic Verification

- [ ] I read through all 15 steps and verified each step's parameters make sense for its purpose
- [ ] I verified the transition table: every non-terminal step has at least one outgoing transition
- [ ] I verified the transition table: every non-terminal step has a `fatal` → `abandon_and_log` transition
- [ ] I verified the loop-back transitions have `max_iterations` set correctly
- [ ] I verified `select_issue` uses `context_map` to extract `issue_number`
- [ ] I verified LLM steps use `stdin` for prompts and reference `.luther/*.md` files (REQ-LF-DATA-003)
- [ ] I verified `fetch_issue` step writes issue body and comments directly to `.luther/` files via shell I/O (REQ-LF-DATA-002)
- [ ] I verified `outcome_on_stdout` is used for `evaluate_plan` and `evaluate_impl` steps
- [ ] I verified `abandon_and_log` removes label, unassigns user, comments on issue (REQ-LF-FAIL-002..004)
- [ ] I verified test fixture copies are identical to config files

### Requirements Traceability

- [ ] REQ-LF-PROF-001: Workflow type uses `{profile_*}` variables, not concrete names
- [ ] REQ-LF-PROF-002: Config maps `profile_*` to concrete profile names
- [ ] REQ-LF-DATA-001: issue_number, branch_name, issue_title flow via context variables
- [ ] REQ-LF-DATA-002: Issue body, plan, verify report written to `.luther/` files
- [ ] REQ-LF-DATA-003: LLM prompts say "read .luther/issue.md", not `{issue_body}`
- [ ] REQ-LF-ISSUE-001..004: select_issue step covers milestone ordering, assignment, labeling
- [ ] REQ-LF-FETCH-001..004: fetch_issue step covers retrieval, file writing, context vars
- [ ] REQ-LF-WS-001..004: setup_workspace step covers checkout, branch creation, .luther dir
- [ ] REQ-LF-PLAN-001..005: create_plan/evaluate_plan steps and loop with limit 5
- [ ] REQ-LF-IMPL-001..003: implement/evaluate_impl steps and routing
- [ ] REQ-LF-TEST-001..003: run_tests (verify executor) and remediate loop with limit 5
- [ ] REQ-LF-PR-001..004: push_changes, generate_pr_description, create_pr as separate steps
- [ ] REQ-LF-FAIL-001..005: Fatal routes to abandon_and_log; abandon step does cleanup
- [ ] REQ-LF-SCOPE-001: Workflow ends at create_pr → log_completion
- [ ] REQ-LF-SEP-003: TOML files contain zero Rust code

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P17a.md`
