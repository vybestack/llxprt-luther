# Phase 04: Enhanced ShellExecutor -- TDD Tests

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P04`

## Prerequisites

- Required: Phase 03a (Stub Verification) completed
- Verification: `grep -r "@plan:PLAN-20260408-LLXPRT-FIRST.P03" src/`
- Expected files from previous phase: Modified `src/engine/executors/shell.rs` with three stub functions

## Requirements Implemented (Expanded)

### REQ-LF-SHELL-001: JSON output parsing

**Full Text**: Where a step's parameters include `output_format: "json"` and a `context_map`, the ShellExecutor shall parse stdout as JSON and extract fields into named context variables using dot-path notation.
**Behavior**:
- GIVEN: A shell step with `output_format: "json"` and `context_map: {"title": ".title", "count": ".stats.count"}`
- WHEN: The command outputs `{"title": "Bug fix", "stats": {"count": 5}}`
- THEN: Context variables `title` = `"Bug fix"` and `count` = `"5"` are set
**Why This Matters**: Enables `gh --json` output to flow into context variables.

### REQ-LF-SHELL-002: Invalid JSON returns Fatal

**Full Text**: If `output_format: "json"` is specified and stdout is not valid JSON, then the ShellExecutor shall return a `Fatal` outcome with a diagnostic message.
**Behavior**:
- GIVEN: A shell step with `output_format: "json"`
- WHEN: The command outputs `not json at all`
- THEN: ShellExecutor returns `StepOutcome::Fatal`
**Why This Matters**: Fail fast on malformed output rather than silently propagating garbage.

### REQ-LF-SHELL-003: Stdin piping

**Full Text**: Where a step's parameters include a `stdin` field, the ShellExecutor shall pipe the interpolated value of that field to the command's standard input.
**Behavior**:
- GIVEN: A shell step with `command: "cat"` and `stdin: "hello from stdin"`
- WHEN: The step executes
- THEN: stdout contains `"hello from stdin"`
**Why This Matters**: Enables piping prompts to llxprt via stdin.

### REQ-LF-SHELL-004: Stdin from file

**Full Text**: Where a step's parameters include a `stdin_file` field, the ShellExecutor shall read the specified file (relative to work_dir) and pipe its contents to the command's standard input.
**Behavior**:
- GIVEN: A shell step with `command: "cat"` and `stdin_file: "input.txt"`, and `input.txt` contains `"file contents"`
- WHEN: The step executes
- THEN: stdout contains `"file contents"`

### REQ-LF-SHELL-005: Outcome pattern matching

**Full Text**: Where a step's parameters include `outcome_on_stdout`, the ShellExecutor shall scan stdout for the configured string keys and map the first match to the corresponding `StepOutcome` value.
**Behavior**:
- GIVEN: A shell step with `outcome_on_stdout: {"APPROVED": "success", "NEEDS_REVISION": "fixable"}`
- WHEN: Command stdout contains `"The plan is APPROVED"`
- THEN: ShellExecutor returns `StepOutcome::Success`

### REQ-LF-SHELL-006: No match defaults to Success

**Full Text**: When `outcome_on_stdout` is configured and the command exits with code 0 but no configured string is found in stdout, the ShellExecutor shall return `Success` as the default outcome.
**Behavior**:
- GIVEN: A shell step with `outcome_on_stdout` configured
- WHEN: Command exits 0 but stdout contains none of the configured strings
- THEN: ShellExecutor returns `StepOutcome::Success`

### REQ-LF-SHELL-007: Non-zero exit overrides patterns

**Full Text**: If `outcome_on_stdout` is configured and the command exits with a non-zero code, then the ShellExecutor shall return `Fixable` regardless of stdout content, preserving existing exit-code semantics.
**Behavior**:
- GIVEN: A shell step with `outcome_on_stdout: {"APPROVED": "success"}`
- WHEN: Command exits with code 1 and stdout contains `"APPROVED"`
- THEN: ShellExecutor returns `StepOutcome::Fixable` (exit code wins)

### REQ-LF-SHELL-008: Missing stdin_file returns Fatal

**Full Text**: If a `stdin_file` is specified and the file does not exist or cannot be read, then the ShellExecutor shall return a `Fatal` outcome with a diagnostic identifying the missing file.
**Behavior**:
- GIVEN: A shell step with `stdin_file: "nonexistent.txt"`
- WHEN: The file does not exist in work_dir
- THEN: ShellExecutor returns `StepOutcome::Fatal`

### REQ-LF-SHELL-009: Missing dot-path returns Fatal

**Full Text**: If `output_format: "json"` is specified with a `context_map` and a dot-path key does not exist in the parsed JSON, then the ShellExecutor shall return a `Fatal` outcome identifying the missing path and the available top-level keys.
**Behavior**:
- GIVEN: A shell step with `context_map: {"foo": ".nonexistent"}` and command outputs `{"title": "test"}`
- WHEN: The step executes
- THEN: ShellExecutor returns `StepOutcome::Fatal` and context contains error info with available keys

### REQ-LF-SHELL-010: Exit code mapping to outcomes

**Full Text**: Where a step's parameters include an `exit_code_map`, the ShellExecutor shall map specific non-zero exit codes to specific `StepOutcome` values. Unmapped non-zero exit codes still default to `Fixable`. Exit code 0 is never mapped (always Success unless overridden by `outcome_on_stdout`).
**Behavior**:
- GIVEN: A shell step with `exit_code_map: {1: "fatal", 2: "fixable"}`
- WHEN: The command exits with code 1
- THEN: ShellExecutor returns `StepOutcome::Fatal` (mapped via exit_code_map)
- GIVEN: The same step
- WHEN: The command exits with code 3 (unmapped non-zero)
- THEN: ShellExecutor returns `StepOutcome::Fixable` (default non-zero behavior)
**Why This Matters**: Enables steps like `select_issue` to signal `Fatal` via exit code 1 while keeping JSON parsing on the success path only.

## Implementation Tasks

### Files to Create

- `tests/shell_enhanced_tests.rs`
  - MUST include: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P04`
  - MUST include: `/// @requirement:REQ-LF-SHELL-XXX` on every test
  - Tests expect REAL behavior -- they will fail until Phase 05 implementation
  - NO `#[should_panic]` tests
  - NO tests that check for `todo!()` behavior

### Test List

1. **`test_shell_json_output_parsing_extracts_fields_to_context`** (REQ-LF-SHELL-001)
   - Run `echo '{"title":"Bug","number":42}'`, parse JSON, extract via context_map
   - Assert `context.get("issue_title")` = `"Bug"`, `context.get("issue_num")` = `"42"`

2. **`test_shell_json_nested_dot_path_extracts_deep_values`** (REQ-LF-SHELL-001)
   - Run command producing `{"data":{"stats":{"count":7}}}`, extract `.data.stats.count`
   - Assert extracted value = `"7"`

3. **`test_shell_json_invalid_stdout_returns_fatal`** (REQ-LF-SHELL-002)
   - Run `echo "not json"` with `output_format: "json"`
   - Assert outcome is `StepOutcome::Fatal`

4. **`test_shell_json_missing_dot_path_returns_fatal_with_available_keys`** (REQ-LF-SHELL-009)
   - Run command producing `{"title":"test","body":"content"}`, extract `.nonexistent`
   - Assert outcome is `StepOutcome::Fatal`
   - Assert context contains error info mentioning available keys

5. **`test_shell_stdin_pipes_value_to_command`** (REQ-LF-SHELL-003)
   - Run `cat` with `stdin: "piped input"` 
   - Assert stdout contains `"piped input"`

6. **`test_shell_stdin_interpolates_context_variables`** (REQ-LF-SHELL-003)
   - Set context var `greeting` = `"hello"`, use `stdin: "{greeting} world"`
   - Assert stdout contains `"hello world"`

7. **`test_shell_stdin_file_pipes_file_contents`** (REQ-LF-SHELL-004)
   - Create `input.txt` in work_dir with content, run `cat` with `stdin_file: "input.txt"`
   - Assert stdout contains file contents

8. **`test_shell_stdin_file_missing_returns_fatal`** (REQ-LF-SHELL-008)
   - Run with `stdin_file: "does_not_exist.txt"`
   - Assert outcome is `StepOutcome::Fatal`

9. **`test_shell_outcome_on_stdout_maps_matching_string_to_outcome`** (REQ-LF-SHELL-005)
   - Run `echo "PLAN_APPROVED"` with `outcome_on_stdout: {"PLAN_APPROVED": "success"}`
   - Assert outcome is `StepOutcome::Success`

10. **`test_shell_outcome_on_stdout_fixable_mapping`** (REQ-LF-SHELL-005)
    - Run `echo "PLAN_NEEDS_REVISION"` with `outcome_on_stdout: {"PLAN_NEEDS_REVISION": "fixable"}`
    - Assert outcome is `StepOutcome::Fixable`

11. **`test_shell_outcome_on_stdout_no_match_defaults_to_success`** (REQ-LF-SHELL-006)
    - Run `echo "random output"` with `outcome_on_stdout: {"MAGIC": "fixable"}`
    - Assert outcome is `StepOutcome::Success`

12. **`test_shell_outcome_on_stdout_nonzero_exit_returns_fixable_regardless`** (REQ-LF-SHELL-007)
    - Run `echo "APPROVED" && exit 1` with `outcome_on_stdout: {"APPROVED": "success"}`
    - Assert outcome is `StepOutcome::Fixable` (exit code takes precedence)

13. **`test_shell_json_array_value_stored_as_json_string`** (REQ-LF-SHELL-001)
    - Run command producing `{"labels":["bug","urgent"]}`, extract `.labels`
    - Assert extracted value is a JSON array string `["bug","urgent"]`

14. **`test_shell_without_new_params_works_as_before`** (backward compatibility)
    - Run `echo hello` with just `command` param (no output_format, no stdin, no outcome_on_stdout)
    - Assert `StepOutcome::Success` and stdout captured -- same as existing behavior

15. **`test_shell_exit_code_map_maps_nonzero_exit_to_specified_outcome`** (REQ-LF-SHELL-010)
    - Run `exit 1` with `exit_code_map: {1: "fatal"}`
    - Assert outcome is `StepOutcome::Fatal`

16. **`test_shell_exit_code_map_unmapped_nonzero_defaults_to_fixable`** (REQ-LF-SHELL-010)
    - Run `exit 3` with `exit_code_map: {1: "fatal", 2: "fixable"}`
    - Assert outcome is `StepOutcome::Fixable` (code 3 not in map, default behavior)

17. **`test_shell_exit_code_map_zero_exit_ignores_map`** (REQ-LF-SHELL-010)
    - Run `echo ok` (exit 0) with `exit_code_map: {0: "fatal"}`
    - Assert outcome is `StepOutcome::Success` (exit code 0 is never mapped)

### Required Code Markers

Every test MUST include:

```rust
/// @plan:PLAN-20260408-LLXPRT-FIRST.P04
/// @requirement:REQ-LF-SHELL-XXX
#[test]
fn test_name() { ... }
```

## Verification Commands

### Automated Checks

```bash
# Check plan markers exist
grep -r "@plan:PLAN-20260408-LLXPRT-FIRST.P04" tests/ | wc -l
# Expected: 17+ occurrences

# Check requirements covered
grep -r "@requirement:REQ-LF-SHELL" tests/shell_enhanced_tests.rs | wc -l
# Expected: 17+ occurrences

# Check for reverse testing
grep -r "should_panic" tests/shell_enhanced_tests.rs
# Expected: No matches

# Compile check (tests should compile but may fail)
cargo build --all-targets

# Run tests -- most should FAIL (Red phase of TDD)
cargo test --test shell_enhanced_tests 2>&1 | tail -20
# Expected: Multiple test failures (assertion errors or todo!() panics)

# Existing tests should still pass
cargo test --test executor_unit_tests
cargo test --test hello_world_workflow_integration
```

### Structural Verification Checklist

- [ ] Phase 03 markers present in source
- [ ] Test file created: `tests/shell_enhanced_tests.rs`
- [ ] All 17 tests have plan markers
- [ ] All tests have requirement markers
- [ ] No `#[should_panic]` tests
- [ ] Tests compile
- [ ] Tests fail with assertion errors (not compile errors)
- [ ] Existing tests unaffected

## Success Criteria

- 17 behavioral tests written
- All tests tagged with plan and requirement markers
- Tests fail naturally (assertion errors or todo!() panics from stubs)
- No reverse testing patterns
- Existing tests still pass

## Failure Recovery

If this phase fails:

1. Rollback: `rm tests/shell_enhanced_tests.rs`
2. Verify: `cargo test` still passes
3. Re-run Phase 04

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P04.md`
