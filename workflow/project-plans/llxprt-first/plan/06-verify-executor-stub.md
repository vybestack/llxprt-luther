# Phase 06: VerifyExecutor -- Stub

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P06`

## Prerequisites

- Required: Phase 05a (Enhanced ShellExecutor Verification) completed
- Verification: `grep -r "@plan:PLAN-20260408-LLXPRT-FIRST.P05" src/`
- Expected: All tests pass, ShellExecutor enhancements working

## Requirements Implemented (Expanded)

This stub phase creates the skeleton for REQ-LF-VERIFY-001 through REQ-LF-VERIFY-009.

### REQ-LF-VERIFY-001: Configurable check sequence

**Full Text**: The VerifyExecutor shall run a configurable sequence of verification checks specified in the step's `parameters.checks` array.
**Behavior**:
- GIVEN: A verify step with `checks: ["typecheck", "test"]`
- WHEN: The step executes
- THEN: Both typecheck and test commands are run, results collected for each
**Why This Matters**: Different projects need different verification suites.

### REQ-LF-VERIFY-002: All checks pass returns Success

**Full Text**: When all checks pass, the VerifyExecutor shall return `Success` and set context variable `verify_passed` to `"true"`.
**Behavior**:
- GIVEN: A verify step where all configured checks exit with code 0
- WHEN: The step completes
- THEN: Returns `StepOutcome::Success` and `context.get("verify_passed")` = `"true"`

### REQ-LF-VERIFY-003: Any check fails returns Fixable with report

**Full Text**: When any check fails, the VerifyExecutor shall return `Fixable`, set `verify_passed` to `"false"`, and write a structured failure report to `.luther/verify-report.json` in the working directory.
**Behavior**:
- GIVEN: A verify step where the test check fails (non-zero exit)
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
- WHEN: Parser runs successfully
- THEN: ErrorRecord has file, line, message populated
- GIVEN: Output that cannot be parsed by any type-specific parser
- WHEN: Fallback behavior triggers
- THEN: ErrorRecord has message containing raw output, check_type and exit_code are still recorded

### REQ-LF-VERIFY-006: Test failure details

**Full Text**: For test check failures, the failure report shall include test name, file, line, assertion kind, and where available, expected and actual values.
**Behavior**:
- GIVEN: vitest JSON output with failed tests
- WHEN: Parser runs
- THEN: ErrorRecord has test_name, assertion_kind, expected, actual populated

### REQ-LF-VERIFY-007: Check suite is parameterized

**Full Text**: The check suite shall be a parameter, not hardcoded. The VerifyExecutor shall support at minimum: `lint`, `typecheck`, `test`, `format`, and `build` check types for Node/TypeScript projects.
**Behavior**:
- GIVEN: `checks: ["lint"]` with custom command `check_commands: {"lint": "echo ok"}`
- WHEN: Step executes
- THEN: Only the lint check runs, using the custom command
**Why This Matters**: The engine must not hardcode any project type.

### REQ-LF-VERIFY-008: Unspawnable command returns Fatal

**Full Text**: If a check command cannot be spawned (binary not found, permission denied), then the VerifyExecutor shall return `Fatal` with a diagnostic identifying the failed check and command.
**Behavior**:
- GIVEN: A check with command `/nonexistent/binary`
- WHEN: Step tries to spawn
- THEN: Returns `StepOutcome::Fatal`

### REQ-LF-VERIFY-009: Per-check-type context variables

**Full Text**: The VerifyExecutor shall set per-check-type context variables (`test_failures`, `build_errors`, `type_errors`, `lint_errors`) containing JSON arrays of structured error records.
**Behavior**:
- GIVEN: Test check fails with 2 failures
- WHEN: Step completes
- THEN: `context.get("test_failures")` is a JSON array string with 2 error records

## Implementation Tasks

### Files to Create

- `src/engine/executors/verify.rs`
  - MUST include: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P06`
  - MUST include: `/// @requirement:REQ-LF-VERIFY-001` through `REQ-LF-VERIFY-009`
  - Create structs: `CheckResult`, `ErrorRecord`, `VerifyReport`, `VerifyExecutor`
  - Implement `StepExecutor` trait for `VerifyExecutor` with `todo!()` body
  - Create stub functions: `resolve_check_command()`, `parse_check_output()`, `parse_typescript_errors()`, `parse_test_results()`, `parse_lint_errors()`, `parse_format_errors()`, `parse_build_errors()`, `build_summary()`
  - All function bodies: `todo!()`
  - Structs should have proper fields with `serde::Serialize` derives (for JSON report output)

### Files to Modify

- `src/engine/executors/mod.rs`
  - Add `pub mod verify;`
  - Add `pub use verify::VerifyExecutor;`
  - ADD marker: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P06`

### Stub Struct Specifications (from pseudocode lines 001-028)

```rust
#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub check_type: String,
    pub passed: bool,
    pub exit_code: i32,
    pub errors: Vec<ErrorRecord>,
    pub raw_stdout: String,
    pub raw_stderr: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ErrorRecord {
    pub file: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub message: String,
    pub severity: Option<String>,
    pub test_name: Option<String>,
    pub assertion_kind: Option<String>,
    pub expected: Option<String>,
    pub actual: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifyReport {
    pub passed: bool,
    pub summary: String,
    pub checks: Vec<CheckResult>,
}
```

### Required Code Markers

```rust
/// @plan:PLAN-20260408-LLXPRT-FIRST.P06
/// @requirement:REQ-LF-VERIFY-XXX
```

## Verification Commands

```bash
# Check plan markers
grep -r "@plan:PLAN-20260408-LLXPRT-FIRST.P06" src/ | wc -l
# Expected: 10+ (structs + functions)

# Module registered
grep "pub mod verify" src/engine/executors/mod.rs
# Expected: found

# Compiles
cargo build --all-targets

# Existing tests still pass
cargo test
```

### Structural Verification

- [ ] `verify.rs` created in `src/engine/executors/`
- [ ] Module registered in `mod.rs`
- [ ] Structs have Serialize derive
- [ ] StepExecutor trait implemented (with todo!() body)
- [ ] All stub functions present with todo!()

## Success Criteria

- `cargo build --all-targets` passes
- `cargo test` passes (all existing tests)
- New module compiles and is reachable

## Failure Recovery

1. Rollback: `rm src/engine/executors/verify.rs`
2. Rollback: `git checkout -- src/engine/executors/mod.rs`

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P06.md`
