# Phase 18a: End-to-End Workflow — Verification

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P18a`

## Prerequisites

- Required: Phase 18 completed

## Verification Commands

```bash
# Plan markers — graph routing tests
grep -c "@plan:PLAN-20260408-LLXPRT-FIRST.P18" tests/e2e_workflow_integration.rs
# Expected: 13+

# Plan markers — live tests
grep -c "@plan:PLAN-20260408-LLXPRT-FIRST.P18" tests/live_workflow_integration.rs
# Expected: 6+

# Graph routing tests pass
cargo test --test e2e_workflow_integration 2>&1 | grep "test result"
# Expected: 13 passed, 0 failed

# Live tests ignored by default
cargo test --test live_workflow_integration 2>&1 | grep "test result"
# Expected: 0 passed, 0 failed, 6 ignored

# Live tests pass when explicitly run (requires gh auth + network)
cargo test --test live_workflow_integration -- --ignored 2>&1 | grep "test result"
# Expected: 6 passed, 0 failed

# No hardcoded repo/profile names in live test source
grep -n "vybestack\|llxprt-code\|acoliver\|opusthinking\|deepthinker\|typescriptexpert" tests/live_workflow_integration.rs
# Expected: no output (all values from TOML config)

# Full test suite (live tests excluded)
cargo test 2>&1 | grep "test result"
# Expected: 0 failures

cargo clippy -- -D warnings
```

### Semantic Verification — Graph Routing Tests

- [ ] Happy path visits all 14 steps in correct order
- [ ] Plan loop: evaluate_plan Fixable twice → create_plan called 3 times → then Success
- [ ] Plan loop limit: always Fixable → Abandoned after 5
- [ ] Test/remediate loop: run_tests Fixable → remediate → run_tests → Success
- [ ] Test/remediate limit: always Fixable → Abandoned after 5
- [ ] Impl evaluation loop: evaluate_impl Fixable → implement → Success
- [ ] Fatal routing: Fatal at any step → `abandon_and_log` via transition table
- [ ] TOML loading: workflow type and config parse from fixtures
- [ ] Graph completeness: every non-terminal step has fatal route
- [ ] Config variables: `[variables]` values accessible during step execution
- [ ] Run metadata: completion recorded in SQLite

### Semantic Verification — Live Integration Tests

- [ ] Can list real issues from repo (non-empty JSON array)
- [ ] Can list real milestones (at least 1)
- [ ] Can fetch real issue details (title, body, comments, url present)
- [ ] fetch_issue writes .luther/issue.md (non-empty file)
- [ ] Workspace setup creates clone with correct branch and .luther/ directory
- [ ] Workspace setup works on re-run (fetch+reset path)
- [ ] All repo-specific values come from TOML config, not Rust source

### Requirements Coverage Matrix

| Requirement Group | Graph Tests | Live Tests | Covered? |
|---|---|---|---|
| REQ-LF-ISSUE-001..004 | happy_path, fatal_at_select_issue | can_list_issues, can_list_milestones | [ ] |
| REQ-LF-FETCH-001..004 | happy_path, fatal_at_any_step | can_fetch_issue_details, fetch_writes_issue_files | [ ] |
| REQ-LF-WS-001..004 | happy_path | workspace_setup_creates_clone, workspace_reuses_existing | [ ] |
| REQ-LF-PLAN-001..005 | plan_loop_*, happy_path | — | [ ] |
| REQ-LF-IMPL-001..003 | impl_evaluation_loop, happy_path | — | [ ] |
| REQ-LF-TEST-001..003 | test_remediation_loop_*, happy_path | — | [ ] |
| REQ-LF-PR-001..004 | happy_path | — | [ ] |
| REQ-LF-FAIL-001..005 | fatal_at_*, loop_exceeds_* | — | [ ] |
| REQ-LF-PROF-001..004 | config_variables_injected, config_loads | — | [ ] |
| REQ-LF-DATA-001..003 | workflow_type_loads | fetch_writes_issue_files | [ ] |
| REQ-LF-SEP-003 | workflow_type_loads, config_loads | — | [ ] |

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P18a.md`
