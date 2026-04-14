# Phase 21a: Smoke Test — Verification

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P21a`

## Prerequisites

- Required: Phase 21 (Smoke Test) completed

## Verification Commands

```bash
# Plan markers
grep -c "@plan:PLAN-20260408-LLXPRT-FIRST.P21" tests/smoke_test.rs
# Expected: 2+

# Tests are ignored by default
cargo test --test smoke_test 2>&1 | grep "test result"
# Expected: 0 passed, 0 failed, 2 ignored

# Full smoke run (requires gh auth + network)
cargo test --test smoke_test -- --ignored 2>&1
# Expected: 2 passed, 0 failed

# Full test suite (smoke tests excluded by default)
cargo test 2>&1 | grep "test result"
# Expected: 0 failures

cargo clippy -- -D warnings
```

## Checklist

- [ ] `tests/smoke_test.rs` exists with 2 `#[ignore]` tests
- [ ] No hardcoded repo names, profile names, or paths in test source
- [ ] Dry run test passes without network
- [ ] Real smoke test creates workspace, fetches issue, writes files
- [ ] Full test suite still passes

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P21a.md`
