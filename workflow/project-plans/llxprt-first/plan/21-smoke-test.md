# Phase 21: End-to-End Smoke Test

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P21`

## Prerequisites

- Required: Phase 20a (CLI Production Wiring Verification) completed
- Verification: `cargo run -- run --workflow-type llxprt-issue-fix-v1 --config llxprt-code --dry-run` works
- Required: `gh` CLI authenticated, network access

## Purpose

Run the actual workflow engine against the real GitHub repo for the first few steps to prove the system works end-to-end. This is not an automated test — it's a manual verification script that exercises the real execution path:

1. **select_issue** — queries real milestones, picks a real issue
2. **setup_workspace** — clones or fetches the real repo into work_dir
3. **fetch_issue** — retrieves real issue body and comments, writes to `.luther/` files

The test stops after `fetch_issue` (does NOT invoke llxprt, run tests, or create PRs). This validates the entire stack: CLI → config resolution → engine → ShellExecutor → real `gh`/`git` commands → context variable flow → file I/O.

## Implementation Tasks

### Files to Create

- `tests/smoke_test.rs` — Automated smoke test (`#[ignore]`, requires `gh` auth + network)
  - MUST include `@plan:PLAN-20260408-LLXPRT-FIRST.P21`

### Smoke Test Design

The test loads the real workflow TOML and config from fixtures, creates a temp work_dir, and runs the engine with a `SmokeTestExecutor` that:
- For `select_issue`, `setup_workspace`, `fetch_issue`: delegates to the real `ShellExecutor` (actually runs commands)
- For all other steps: returns `Fatal` to stop execution after `fetch_issue`

This way the test runs exactly 3 real steps and then stops cleanly via the fatal transition to `abandon_and_log` (which also uses the real executor to run cleanup commands against the issue).

**Alternative approach**: If wiring a hybrid executor is too complex, the test can directly run the shell commands from the loaded TOML step parameters, interpolating variables from the loaded config. This is simpler but doesn't exercise the engine's dispatch loop.

The preferred approach is the hybrid executor because it validates the full stack.

### Test List (2 tests, both `#[ignore]`)

1. **`test_smoke_select_and_fetch`**
   - Load workflow type from `config/workflows/llxprt-issue-fix-v1.toml` (via config resolution, not hardcoded path)
   - Load config from `config/workflow-configs/llxprt-code.toml`
   - Override `work_dir` in config variables to a fresh `TempDir`
   - Create `WorkflowInstance`, create `EngineRunner` with the hybrid executor
   - Run the engine
   - Assert: run progresses past `select_issue` (the executor was called, `issue_number` is in context)
   - Assert: `{work_dir}/.git/` exists (workspace was set up)
   - Assert: `{work_dir}/.luther/issue.md` exists and is non-empty (issue was fetched)
   - Assert: `{work_dir}/.luther/issue-raw.json` exists and is valid JSON
   - Cleanup: the issue was assigned and labeled during `select_issue`; the `abandon_and_log` step should unassign and remove the label. Verify this happened by checking the issue state afterward, or accept that cleanup is best-effort.

2. **`test_smoke_dry_run_prints_all_steps`**
   - Run: `cargo run -- run --workflow-type llxprt-issue-fix-v1 --config llxprt-code --dry-run`
   - Capture stdout
   - Assert: output contains all 14 step_ids
   - Assert: output contains "Dry run complete"
   - This test exercises the CLI → config resolution → workflow loading path without any external tool calls

### Key Design Constraints

- **All config comes from TOML** — the test loads config from the real workflow files via the resolution system. No hardcoded repo names, profile names, or paths in the Rust source.
- **`#[ignore]` on both tests** — they require `gh` auth, network access, and modify real GitHub state (issue assignment). Run explicitly: `cargo test --test smoke_test -- --ignored`
- **Temp directory for work_dir** — never use a hardcoded path. Override the config's `work_dir` value with a `TempDir` for test isolation.
- **Cleanup is important** — the test assigns an issue and adds a label. The `abandon_and_log` step should clean this up. If the test crashes before cleanup, the issue will be left assigned — this is acceptable for a smoke test but should be noted in test documentation.

## Verification Commands

```bash
# Plan markers
grep -c "@plan:PLAN-20260408-LLXPRT-FIRST.P21" tests/smoke_test.rs
# Expected: 2+

# Smoke tests are ignored by default
cargo test --test smoke_test 2>&1 | grep "test result"
# Expected: 0 passed, 0 failed, 2 ignored

# Run smoke tests (requires gh auth + network)
cargo test --test smoke_test -- --ignored 2>&1 | grep "test result"
# Expected: 2 passed, 0 failed

# Dry run test can also be verified manually:
cargo run -- run --workflow-type llxprt-issue-fix-v1 --config llxprt-code --dry-run

cargo clippy -- -D warnings
```

### Verification Checklist

- [ ] `tests/smoke_test.rs` created with 2 tests
- [ ] Both tests have `#[ignore]` attribute
- [ ] Tests load config from TOML files via resolution (no hardcoded values)
- [ ] `test_smoke_select_and_fetch` exercises real `gh` and `git` commands
- [ ] `test_smoke_select_and_fetch` verifies files created in work_dir
- [ ] `test_smoke_dry_run_prints_all_steps` exercises CLI → config → workflow loading
- [ ] Work_dir uses TempDir for isolation
- [ ] Cleanup (unassign/unlabel) happens via abandon_and_log or manual cleanup

## Success Criteria

- Both smoke tests pass when run with `--ignored`
- The engine successfully selects an issue, sets up a workspace, and fetches issue data
- Files are created in the correct locations
- The full CLI dry-run path works

## Failure Recovery

1. Rollback: `rm tests/smoke_test.rs`
2. Manual cleanup: if test left an issue assigned, unassign it via `gh issue edit --remove-assignee`
3. Verify: `cargo test` still passes

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P21.md`
