# Phase 03: Enhanced ShellExecutor -- Stub

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P03`

## Prerequisites

- Required: Phase 02a (Pseudocode Verification) completed
- Verification: `grep -r "@plan:PLAN-20260408-LLXPRT-FIRST.P02" .` (N/A -- pseudocode phase has no code markers)
- Preflight verification: Phase 00a MUST be completed
- Expected: `cargo build --all-targets` passes, `cargo test` passes

## Requirements Implemented (Expanded)

This stub phase creates the skeleton for the following requirements. No behavior is implemented yet.

### REQ-LF-SHELL-001: JSON output parsing

**Full Text**: Where a step's parameters include `output_format: "json"` and a `context_map`, the ShellExecutor shall parse stdout as JSON and extract fields into named context variables using dot-path notation.
**Behavior**:
- GIVEN: A shell step with `output_format: "json"` and `context_map: {"title": ".title", "count": ".stats.count"}`
- WHEN: The command outputs `{"title": "Bug fix", "stats": {"count": 5}}`
- THEN: Context variables `title` = `"Bug fix"` and `count` = `"5"` are set
**Why This Matters**: Enables `gh --json` output to flow into context variables for later steps without a dedicated GhExecutor.

### REQ-LF-SHELL-002: Invalid JSON returns Fatal

**Full Text**: If `output_format: "json"` is specified and stdout is not valid JSON, then the ShellExecutor shall return a `Fatal` outcome with a diagnostic message.
**Behavior**:
- GIVEN: A shell step with `output_format: "json"`
- WHEN: The command outputs `not json at all`
- THEN: ShellExecutor returns `StepOutcome::Fatal`

### REQ-LF-SHELL-003: Stdin piping

**Full Text**: Where a step's parameters include a `stdin` field, the ShellExecutor shall pipe the interpolated value of that field to the command's standard input.
**Behavior**:
- GIVEN: A shell step with `command: "cat"` and `stdin: "hello from stdin"`
- WHEN: The step executes
- THEN: stdout contains `"hello from stdin"`
**Why This Matters**: Enables large prompts to be piped to llxprt via stdin instead of command-line arguments.

### REQ-LF-SHELL-004: Stdin from file

**Full Text**: Where a step's parameters include a `stdin_file` field, the ShellExecutor shall read the specified file (relative to work_dir) and pipe its contents to the command's standard input.
**Behavior**:
- GIVEN: A shell step with `command: "cat"` and `stdin_file: "input.txt"`, and `input.txt` exists in work_dir
- WHEN: The step executes
- THEN: stdout contains the contents of `input.txt`

### REQ-LF-SHELL-005: Outcome pattern matching

**Full Text**: Where a step's parameters include `outcome_on_stdout`, the ShellExecutor shall scan stdout for the configured string keys and map the first match to the corresponding `StepOutcome` value.
**Behavior**:
- GIVEN: A shell step with `outcome_on_stdout: {"APPROVED": "success", "NEEDS_REVISION": "fixable"}`
- WHEN: Command stdout contains `"The plan is APPROVED"`
- THEN: ShellExecutor returns `StepOutcome::Success`
**Why This Matters**: Enables LLM evaluation steps to signal pass/fail via specific strings in output.

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
- THEN: ShellExecutor returns `StepOutcome::Fatal` with error info listing available keys

### REQ-LF-SHELL-010: Exit code mapping to outcomes

**Full Text**: Where a step's parameters include an `exit_code_map`, the ShellExecutor shall map specific non-zero exit codes to specific `StepOutcome` values. Unmapped non-zero exit codes still default to `Fixable`. Exit code 0 is never mapped (always Success unless overridden by `outcome_on_stdout`).
**Behavior**:
- GIVEN: A shell step with `exit_code_map: {1: "fatal", 2: "fixable"}`
- WHEN: The command exits with code 1
- THEN: ShellExecutor returns `StepOutcome::Fatal` (mapped via exit_code_map)
- GIVEN: The same step
- WHEN: The command exits with code 3 (unmapped non-zero)
- THEN: ShellExecutor returns `StepOutcome::Fixable` (default non-zero behavior)
**Why This Matters**: Enables steps like `select_issue` to signal `Fatal` via exit code 1 while keeping JSON parsing on the success path only. Avoids the antipattern of mixing `outcome_on_stdout` with `output_format: "json"`.

## Implementation Tasks

### Files to Modify

- `src/engine/executors/shell.rs`
  - Add `exit_code_map: Option<HashMap<i32, String>>` field to the ShellExecutor parameter struct (or as an extracted parameter in `execute()`). This maps specific non-zero exit codes to `StepOutcome` variant names (e.g., `{1: "fatal", 2: "fixable"}`).
  - Add helper function stubs: `extract_dot_path()`, `json_value_to_string()`, `parse_outcome_name()`
  - These stubs use `todo!()` macro for unimplemented logic
  - The main `execute()` method is NOT changed yet -- just the helper functions and the new field are added
  - ADD comment: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P03`
  - ADD comment: `/// @requirement:REQ-LF-SHELL-001,REQ-LF-SHELL-003,REQ-LF-SHELL-005,REQ-LF-SHELL-010`

### Required Code Markers

Every new function in this phase MUST include:

```rust
/// @plan:PLAN-20260408-LLXPRT-FIRST.P03
/// @requirement:REQ-LF-SHELL-XXX
```

### Stub Specifications

1. **`extract_dot_path(json: &serde_json::Value, path: &str) -> Option<&serde_json::Value>`**
   - Corresponds to pseudocode lines 088-097
   - Stub: `todo!()`

2. **`json_value_to_string(value: &serde_json::Value) -> String`**
   - Corresponds to pseudocode lines 099-107
   - Stub: `todo!()`

3. **`parse_outcome_name(name: &str) -> StepOutcome`**
   - Corresponds to pseudocode lines 109-118
   - Stub: `todo!()`

4. **`exit_code_map: Option<HashMap<i32, String>>`** field (on ShellExecutor or extracted from step parameters)
   - Maps specific non-zero exit codes to `StepOutcome` variant names
   - Extracted from `step.parameters["exit_code_map"]` as `HashMap<i32, String>`
   - Used by `execute()` to check exit codes before the default non-zero-to-Fixable fallback
   - In this stub phase: add the field/type definition only; no logic in `execute()` yet

### Constraints

- Do NOT modify the `execute()` method yet
- Do NOT modify any existing tests
- All stubs use `todo!()` macro
- No new test files in this phase

## Verification Commands

### Automated Checks

```bash
# Check plan markers exist
grep -r "@plan:PLAN-20260408-LLXPRT-FIRST.P03" src/ | wc -l
# Expected: 4+ occurrences (one per stub function + exit_code_map field)

# Check for TODO comments (todo!() macro is OK in stubs)
grep -r "// TODO" src/engine/executors/shell.rs
# Expected: No matches (todo!() macro is fine, // TODO comments are not)

# Check for version duplication
find src -name "*shell_v2*" -o -name "*shell_new*" -o -name "*shell_copy*"
# Expected: No matches

# Verify Rust compiles
cargo build --all-targets || exit 1

# Verify existing tests still pass (stubs are not called yet)
cargo test || exit 1

# Verify tests don't EXPECT panic
grep -r "should_panic" tests/
# Expected: No matches
```

### Structural Verification Checklist

- [ ] Previous phase markers present (N/A -- first code phase)
- [ ] All listed files modified
- [ ] Plan markers added to all changes
- [ ] Stubs compile
- [ ] Existing tests still pass
- [ ] No `// TODO` comments (only `todo!()` macro)

## Success Criteria

- `cargo build --all-targets` passes
- `cargo test` passes (all existing tests)
- Three stub functions exist with `todo!()` bodies
- `exit_code_map: Option<HashMap<i32, String>>` field/type defined
- Plan markers present on all new code

## Failure Recovery

If this phase fails:

1. Rollback: `git checkout -- src/engine/executors/shell.rs`
2. Cannot proceed to Phase 04 until fixed

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P03.md`
