# Phase 08: VerifyExecutor -- Implementation

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P08`

## Prerequisites

- Required: Phase 07a (TDD Verification) completed
- Verification: `grep -r "@plan:PLAN-20260408-LLXPRT-FIRST.P07" tests/`
- Expected files: `tests/verify_executor_tests.rs` with 14 failing tests

## Requirements Implemented (Expanded)

All REQ-LF-VERIFY-001 through REQ-LF-VERIFY-009. See Phase 07 for full expansion.

## Implementation Tasks

### Files to Modify

- `src/engine/executors/verify.rs`
  - Implement `VerifyExecutor::execute()` (from pseudocode lines 033-110)
    - Extract checks array and optional check_commands from params
    - Iterate checks, resolve commands, spawn, capture output
    - Parse output per check type
    - Build report, write to .luther/verify-report.json
    - Set context variables (verify_passed, verify_summary, per-type errors)
    - Return Success if all pass, Fixable if any fail, Fatal if spawn fails
  - Implement `resolve_check_command()` (from pseudocode lines 112-127)
    - Check custom_commands first, fall back to Node/TypeScript defaults
  - Implement `parse_check_output()` (from pseudocode lines 129-140)
    - Dispatch to type-specific parser based on check_type
  - Implement `parse_typescript_errors()` (from pseudocode lines 142-155)
    - Regex: `^(.+)\((\d+),(\d+)\): error (TS\d+): (.+)$`
    - Extract file, line, column, message
    - Fallback: raw output as single ErrorRecord
  - Implement `parse_test_results()` (from pseudocode lines 157-179)
    - Try JSON parse (vitest --reporter=json format)
    - Extract testResults[].assertionResults[] for failures
    - Fallback: raw output
  - Implement `parse_lint_errors()` (from pseudocode lines 181-199)
    - Try JSON parse (eslint --format json)
    - Extract filePath, messages[].line, column, message
    - Fallback: raw output
  - Implement `parse_format_errors()` (from pseudocode lines 201-217)
    - Parse prettier check output (lines with file paths)
    - Fallback: raw output wrapped in structured record (see fallback spec below)
  - Implement `parse_build_errors()` (from pseudocode lines 219-226)
    - Delegate to typescript parser, fallback to raw output wrapped in structured record
  - Implement `build_summary()` (from pseudocode lines 228-235)
    - Format: "check: pass" or "check: N errors"
  - ADD markers: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P08`

### Raw Output Fallback Specification (REQ-LF-VERIFY-005, REQ-LF-VERIFY-006)

When a check's output cannot be parsed by the type-specific parser (unrecognized format, empty output, garbled output), the fallback behavior MUST wrap the raw output in a structured `ErrorRecord` that preserves minimum machine-readable context:

```rust
// Fallback ErrorRecord when parsing fails
ErrorRecord {
    file: None,
    line: None,
    column: None,
    message: raw_stderr_or_stdout.trim().to_string(),  // Full raw output as the message
    severity: Some("error".to_string()),
    test_name: None,
    assertion_kind: None,
    expected: None,
    actual: None,
}
```

Additionally, the `CheckResult` for an unparseable check MUST always contain:
- `check_type`: the check that was run (e.g., `"lint"`, `"test"`)
- `exit_code`: the actual exit code from the command
- `raw_stdout`: complete stdout
- `raw_stderr`: complete stderr
- `passed: false` (since the check exited non-zero)
- `errors`: a `Vec` with at least one `ErrorRecord` containing the raw output as `message`

This means: even when parsing fails completely, the report still has a structured record per failed check with the check type, exit code, and raw text. The LLM remediation step can work with this raw data — it just doesn't get per-file/per-line details.

The test `test_verify_unparseable_output_produces_raw_error_record` (Phase 07, test #14) validates this behavior.

### Constraints

- Do NOT modify any test files
- All 14 tests from Phase 07 must pass
- All existing tests must still pass
- No `todo!()`, `unimplemented!()`, `println!()`, or `dbg!()` in final code

## Verification Commands

```bash
# All verify tests pass
cargo test --test verify_executor_tests || exit 1
# Expected: 14 passed, 0 failed

# Full test suite
cargo test || exit 1

# No test modifications
git diff tests/verify_executor_tests.rs | head -5
# Expected: no output

# No debug code
grep -rn "println!\|dbg!\|todo!\|unimplemented!" src/engine/executors/verify.rs
# Expected: No matches

# Plan markers
grep -c "@plan:PLAN-20260408-LLXPRT-FIRST.P08" src/engine/executors/verify.rs
# Expected: 1+

# Clippy
cargo clippy -- -D warnings
```

### Deferred Implementation Detection

```bash
grep -rn "todo!\|unimplemented!" src/engine/executors/verify.rs
# Expected: No matches

grep -rn "// TODO\|// FIXME\|placeholder\|not yet" src/engine/executors/verify.rs
# Expected: No matches
```

### Semantic Verification

- [ ] VerifyExecutor runs real shell commands for each check
- [ ] Report file contains per-check results with parsed errors
- [ ] TypeScript error parser extracts file/line/message from real format
- [ ] Test result parser handles vitest JSON reporter format
- [ ] Lint parser handles eslint JSON format
- [ ] Format parser extracts file paths from prettier output
- [ ] Unknown/unparseable output falls back to structured ErrorRecord with check_type, exit_code, and raw text as message
- [ ] Fallback ErrorRecord has severity = "error" and the raw stdout/stderr as the message
- [ ] CheckResult always has check_type, exit_code, raw_stdout, raw_stderr even when parsing fails
- [ ] Fatal returned when command cannot be spawned

## Success Criteria

- All 14 verify executor tests pass
- All existing tests pass
- No deferred implementation
- Clippy passes

## Failure Recovery

1. Rollback: `git checkout -- src/engine/executors/verify.rs`
2. Verify: `cargo test` passes

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P08.md`
