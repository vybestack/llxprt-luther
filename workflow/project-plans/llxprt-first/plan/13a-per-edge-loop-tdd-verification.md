# Phase 13a: Per-edge Loop Limits -- TDD Verification

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P13a`

## Prerequisites

- Required: Phase 13 completed

## Verification Commands

```bash
# Plan markers
grep -c "@plan:PLAN-20260408-LLXPRT-FIRST.P13" tests/per_edge_loop_tests.rs
# Expected: 10+

# Requirement markers
grep -c "@requirement:REQ-LF-LOOP" tests/per_edge_loop_tests.rs
# Expected: 10+

# No reverse testing
grep "should_panic" tests/per_edge_loop_tests.rs
# Expected: no output

# Compile
cargo build --all-targets

# Tests fail (Red phase of TDD)
cargo test --test per_edge_loop_tests 2>&1 | grep "test result"
# Expected: most tests fail (per-edge loop enforcement not implemented yet)

# Existing tests still pass
cargo test --test executor_unit_tests
cargo test --test engine_execution_integration
cargo test --test namespaced_context_tests
```

### Semantic Verification

- [ ] Tests construct real EngineRunner instances with real workflows
- [ ] Tests verify RunOutcome variants (Success vs Abandoned), not internal counts
- [ ] Tests for independent loops verify both loops can iterate without interfering
- [ ] Checkpoint roundtrip test actually saves and loads from database
- [ ] Tests check the abandoned reason message content for edge identification
- [ ] Forward-only workflow test confirms no false-positive loop detection

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P13a.md`
