# Phase 07: VerifyExecutor -- TDD Tests

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P07`

## Prerequisites

- Required: Phase 06a (Stub Verification) completed
- Verification: `grep -r "@plan:PLAN-20260408-LLXPRT-FIRST.P06" src/`
- Expected files: `src/engine/executors/verify.rs` with stub structs and functions

## Requirements Implemented (Expanded)

### REQ-LF-VERIFY-001: Configurable check sequence

**Full Text**: The VerifyExecutor shall run a configurable sequence of verification checks specified in the step's `parameters.checks` array.
**Behavior**:
- GIVEN: A verify step with `checks: ["typecheck", "test"]`
- WHEN: The step executes
- THEN: Both typecheck and test commands are run, results collected for each

### REQ-LF-VERIFY-002: All checks pass returns Success

**Full Text**: When all checks pass, the VerifyExecutor shall return `Success` and set context variable `verify_passed` to `"true"`.
**Behavior**:
- GIVEN: A verify step where all configured checks exit with code 0
- WHEN: The step completes
- THEN: Returns `StepOutcome::Success` and `context.get("verify_passed")` = `"true"`

### REQ-LF-VERIFY-003: Any check fails returns Fixable with report

**Full Text**: When any check fails, the VerifyExecutor shall return `Fixable`, set `verify_passed` to `"false"`, and write a structured failure report to `.luther/verify-report.json` in the working directory.
**Behavior**:
- GIVEN: A verify step where the test check fails
- WHEN: The step completes
- THEN: Returns `StepOutcome::Fixable`, `verify_passed` = `"false"`, and `.luther/verify-report.json` exists

### REQ-LF-VERIFY-004: Summary context variable

**Full Text**: The VerifyExecutor shall set a `verify_summary` context variable containing a human-readable one-line summary of all check results.
**Behavior**:
- GIVEN: Checks lint (pass), typecheck (2 errors), test (3 failed)
- WHEN: Step completes
- THEN: `verify_summary` contains something like `"lint: pass, typecheck: 2 errors, test: 3 errors"`

### REQ-LF-VERIFY-005: Structured error details

**Full Text**: The structured failure report shall contain per-check results with parsed error details including at minimum: file path, line number, and error message.
**Behavior**:
- GIVEN: TypeScript errors in output
- WHEN: Parser runs
- THEN: ErrorRecord has file, line, message populated

### REQ-LF-VERIFY-006: Test failure details

**Full Text**: For test check failures, the failure report shall include test name, file, line, assertion kind, and where available, expected and actual values.
**Behavior**:
- GIVEN: vitest JSON output with failed tests
- WHEN: Parser runs
- THEN: ErrorRecord has test_name, assertion_kind, expected, actual populated

### REQ-LF-VERIFY-007: Parameterized check suite

**Full Text**: The check suite shall be a parameter, not hardcoded.
**Behavior**:
- GIVEN: `checks: ["lint"]` with custom command `check_commands: {"lint": "echo ok"}`
- WHEN: Step executes
- THEN: Only the lint check runs, using the custom command

### REQ-LF-VERIFY-008: Unspawnable command returns Fatal

**Full Text**: If a check command cannot be spawned (binary not found, permission denied), then the VerifyExecutor shall return `Fatal` with a diagnostic.
**Behavior**:
- GIVEN: A check with command `/nonexistent/binary`
- WHEN: Step tries to spawn
- THEN: Returns `StepOutcome::Fatal`

### REQ-LF-VERIFY-009: Per-check-type context variables

**Full Text**: The VerifyExecutor shall set per-check-type context variables containing JSON arrays of structured error records.
**Behavior**:
- GIVEN: Test check fails with 2 failures
- WHEN: Step completes
- THEN: `context.get("test_failures")` is a JSON array string with 2 error records

## Implementation Tasks

### Files to Create

- `tests/verify_executor_tests.rs`
  - MUST include: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P07`
  - MUST include: `/// @requirement:REQ-LF-VERIFY-XXX` on every test

### Test Strategy

Since VerifyExecutor shells out to real commands, tests must use mock commands (scripts that produce known output). Create small shell scripts or use `echo`/`printf` commands that simulate:
- TypeScript error output format
- vitest JSON reporter output
- eslint JSON output
- prettier check output
- Build failure output

Custom `check_commands` parameter enables this: override default commands with echo/printf that produce known output.

### Test List

1. **`test_verify_all_checks_pass_returns_success`** (REQ-LF-VERIFY-002)
   - checks: ["typecheck", "test"] with commands that exit 0
   - Assert outcome is Success, verify_passed is "true"

2. **`test_verify_any_check_fails_returns_fixable`** (REQ-LF-VERIFY-003)
   - checks: ["typecheck", "test"] where test command exits 1
   - Assert outcome is Fixable, verify_passed is "false"

3. **`test_verify_writes_report_file_on_failure`** (REQ-LF-VERIFY-003)
   - Run with a failing check
   - Assert `.luther/verify-report.json` exists in work_dir
   - Assert file parses as valid JSON
   - Assert report has `passed: false`

4. **`test_verify_report_contains_per_check_results`** (REQ-LF-VERIFY-005)
   - Run with 2 checks (one pass, one fail)
   - Parse report JSON
   - Assert report has 2 check entries with correct pass/fail status

5. **`test_verify_summary_contains_all_check_statuses`** (REQ-LF-VERIFY-004)
   - Run with lint (pass) and typecheck (fail)
   - Assert verify_summary contains "lint: pass" and "typecheck:"

6. **`test_verify_typescript_error_parser_extracts_file_and_line`** (REQ-LF-VERIFY-005)
   - Use custom command that echoes TypeScript-format error: `src/foo.ts(10,5): error TS2322: message`
   - Assert ErrorRecord has file="src/foo.ts", line=10, message contains "TS2322"

7. **`test_verify_test_parser_extracts_test_name_and_failure`** (REQ-LF-VERIFY-006)
   - Use custom command that echoes vitest JSON with a failed test
   - Assert ErrorRecord has test_name, message populated

8. **`test_verify_lint_parser_extracts_eslint_json`** (REQ-LF-VERIFY-005)
   - Use custom command that echoes eslint JSON format
   - Assert ErrorRecord has file, line, message from eslint output

9. **`test_verify_custom_check_commands_override_defaults`** (REQ-LF-VERIFY-007)
   - Provide check_commands: {"lint": "echo custom_lint"} 
   - Assert the custom command was used (check raw_stdout)

10. **`test_verify_unspawnable_command_returns_fatal`** (REQ-LF-VERIFY-008)
    - Use check_commands with a nonexistent binary
    - Assert outcome is Fatal

11. **`test_verify_sets_per_check_type_context_variables`** (REQ-LF-VERIFY-009)
    - Run with failing test check
    - Assert context has "test_failures" set to a JSON array string

12. **`test_verify_runs_only_configured_checks`** (REQ-LF-VERIFY-001)
    - Run with checks: ["lint"] only
    - Assert only lint check ran (report has 1 entry)

13. **`test_verify_empty_checks_array_returns_success`** (REQ-LF-VERIFY-001, edge case)
    - Run with checks: []
    - Assert outcome is Success, verify_passed is "true"

14. **`test_verify_unparseable_output_produces_raw_error_record`** (REQ-LF-VERIFY-005, REQ-LF-VERIFY-006, robustness)
    - Use custom command that outputs garbage (not matching any parser format) and exits non-zero
    - Assert the `CheckResult` has `check_type` set (e.g., `"lint"`), `exit_code` is non-zero, `raw_stderr` or `raw_stdout` contains the raw output
    - Assert `errors` Vec contains at least one `ErrorRecord` with `message` containing the raw output text
    - Assert the `ErrorRecord.severity` is `Some("error")`
    - This validates that even when parsing fails, the minimum structured data (check type, exit code, raw text) is always present

### Required Code Markers

```rust
/// @plan:PLAN-20260408-LLXPRT-FIRST.P07
/// @requirement:REQ-LF-VERIFY-XXX
#[test]
fn test_name() { ... }
```

## Verification Commands

```bash
# Plan markers
grep -c "@plan:PLAN-20260408-LLXPRT-FIRST.P07" tests/verify_executor_tests.rs
# Expected: 14+

# Requirement coverage
grep -c "@requirement:REQ-LF-VERIFY" tests/verify_executor_tests.rs
# Expected: 14+

# No reverse testing
grep "should_panic" tests/verify_executor_tests.rs
# Expected: no output

# Compile
cargo build --all-targets

# Tests fail (Red phase)
cargo test --test verify_executor_tests 2>&1 | grep "test result"
# Expected: failures > 0

# Existing tests pass
cargo test --test executor_unit_tests
cargo test --test shell_enhanced_tests
```

## Success Criteria

- 14 behavioral tests written
- All tests tagged with plan and requirement markers
- Tests fail naturally (todo!() panics from stubs)
- Existing tests still pass

## Failure Recovery

1. Rollback: `rm tests/verify_executor_tests.rs`
2. Verify: `cargo test` passes

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P07.md`
