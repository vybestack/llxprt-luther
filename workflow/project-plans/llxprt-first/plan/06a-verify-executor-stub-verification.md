# Phase 06a: VerifyExecutor -- Stub Verification

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P06a`

## Prerequisites

- Required: Phase 06 completed

## Verification Commands

```bash
# File exists
test -f src/engine/executors/verify.rs && echo "OK" || echo "MISSING"

# Module registered
grep "pub mod verify" src/engine/executors/mod.rs
grep "pub use verify::VerifyExecutor" src/engine/executors/mod.rs

# Plan markers
grep -c "@plan:PLAN-20260408-LLXPRT-FIRST.P06" src/engine/executors/verify.rs
# Expected: 10+

# Structs exist
grep "pub struct CheckResult" src/engine/executors/verify.rs
grep "pub struct ErrorRecord" src/engine/executors/verify.rs
grep "pub struct VerifyReport" src/engine/executors/verify.rs
grep "pub struct VerifyExecutor" src/engine/executors/verify.rs

# StepExecutor implemented
grep "impl StepExecutor for VerifyExecutor" src/engine/executors/verify.rs

# Stubs use todo!()
grep -c "todo!()" src/engine/executors/verify.rs
# Expected: 8+ (execute + 7 helper functions)

# Compiles
cargo build --all-targets

# Existing tests pass
cargo test
```

### Semantic Verification

- [ ] VerifyExecutor implements StepExecutor trait
- [ ] CheckResult has all fields from pseudocode (check_type, passed, exit_code, errors, raw_stdout, raw_stderr)
- [ ] ErrorRecord has test-specific fields (test_name, assertion_kind, expected, actual)
- [ ] Structs derive Serialize for JSON report generation
- [ ] No existing code modified beyond mod.rs

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P06.md`
