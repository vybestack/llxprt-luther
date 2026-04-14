# Phase 20a: CLI Production Wiring — Verification

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P20a`

## Prerequisites

- Required: Phase 20 (CLI Production Wiring) completed

## Verification Commands

```bash
# Plan markers
grep -c "@plan:PLAN-20260408-LLXPRT-FIRST.P20" src/cli/mod.rs src/main.rs src/workflow/config_loader.rs
# Expected: 1+ per file

# CLI flag documented
cargo run -- run --help 2>&1 | grep "config-dir"
# Expected: found

# Dry run against production config
cargo run -- run --workflow-type llxprt-issue-fix-v1 --config llxprt-code --dry-run 2>&1
# Expected: prints 14 steps and "Dry run complete"

# Resolution tests
cargo test --test cli_config_resolution_integration 2>&1 | grep "test result"
# Expected: 4 passed, 0 failed

# Full test suite
cargo test 2>&1 | grep "test result"
# Expected: 0 failures

cargo clippy -- -D warnings
```

## Checklist

- [ ] `--config-dir` flag present in `--help` output
- [ ] Dry run against production TOML prints all 14 steps
- [ ] Resolution tests pass (4 tests)
- [ ] Existing test suite still passes
- [ ] No hardcoded fixture paths remain in `main.rs`

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P20a.md`
