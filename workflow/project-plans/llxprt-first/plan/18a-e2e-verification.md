# Phase 18a: End-to-End Workflow -- Verification

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P18a`

## Prerequisites

- Required: Phase 18 completed

## Verification Commands

```bash
# Plan markers
grep -c "@plan:PLAN-20260408-LLXPRT-FIRST.P18" tests/e2e_workflow_integration.rs
# Expected: 12+

# Requirement coverage
grep -c "@requirement:REQ-LF" tests/e2e_workflow_integration.rs
# Expected: 12+

# All E2E tests pass
cargo test --test e2e_workflow_integration 2>&1 | grep "test result"
# Expected: 12 passed, 0 failed

# Full test suite
cargo test 2>&1 | grep "test result"
# Expected: 0 failures

# Clippy
cargo clippy -- -D warnings

# No external tools invoked
grep -c "Command::new\|process::Command" tests/e2e_workflow_integration.rs
# Expected: 0
```

### Semantic Verification

- [ ] Happy path test visits all 15 steps in the correct order
- [ ] Plan loop test: evaluate_plan returns Fixable twice → create_plan called 3 times → evaluate_plan returns Success → proceeds to implement
- [ ] Plan loop limit test: evaluate_plan always Fixable → Abandoned after 5 back-edges
- [ ] Test/remediate loop test: run_tests Fixable → remediate → run_tests again → eventually Success
- [ ] Test/remediate limit test: always Fixable → Abandoned after 5
- [ ] Impl evaluation loop test: evaluate_impl Fixable → implement → eventually Success
- [ ] Fatal routing test: Fatal at any step → engine follows `condition = "fatal"` transition to `abandon_and_log` → `abandon_and_log` executes → workflow terminates
- [ ] TOML loading tests: workflow type and config load and parse from fixtures without errors
- [ ] Graph completeness test: every non-terminal step has fatal route and at least one normal route
- [ ] Config variables test: variables from `[variables]` section accessible during step execution

### Requirements Coverage Matrix

| Requirement Group | Test(s) | Covered? |
|---|---|---|
| REQ-LF-ISSUE-001..004 | test_happy_path, test_fatal_at_select_issue | [ ] |
| REQ-LF-FETCH-001..004 | test_happy_path, test_fatal_at_any_step | [ ] |
| REQ-LF-WS-001..004 | test_happy_path, test_fatal_at_any_step | [ ] |
| REQ-LF-PLAN-001..005 | test_plan_loop_*, test_happy_path | [ ] |
| REQ-LF-IMPL-001..003 | test_impl_evaluation_loop, test_happy_path | [ ] |
| REQ-LF-TEST-001..003 | test_test_remediation_loop_*, test_happy_path | [ ] |
| REQ-LF-PR-001..004 | test_happy_path | [ ] |
| REQ-LF-FAIL-001..005 | test_fatal_at_*, test_plan_loop_exceeds, test_test_remediation_exceeds | [ ] |
| REQ-LF-PROF-001..004 | test_config_variables_injected, test_workflow_config_loads | [ ] |
| REQ-LF-DATA-001..003 | test_workflow_type_loads (verifies step params reference files) | [ ] |
| REQ-LF-SEP-003 | test_workflow_type_loads, test_workflow_config_loads | [ ] |
| REQ-LF-SCOPE-001..002 | test_happy_path (terminates at log_completion, not CI watching) | [ ] |

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P18a.md`
