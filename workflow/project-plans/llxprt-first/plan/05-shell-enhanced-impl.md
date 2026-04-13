# Phase 05: Enhanced ShellExecutor -- Implementation

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P05`

## Prerequisites

- Required: Phase 04a (TDD Verification) completed
- Verification: `grep -r "@plan:PLAN-20260408-LLXPRT-FIRST.P04" tests/`
- Expected files: `tests/shell_enhanced_tests.rs` with 17 failing tests
- All existing tests pass

## Requirements Implemented (Expanded)

All REQ-LF-SHELL-001 through REQ-LF-SHELL-010. See Phase 04 for full expansion.

## Implementation Tasks

### Files to Modify

- `src/engine/executors/shell.rs`
  - Implement `extract_dot_path()` (from pseudocode lines 088-097)
    - Split dot_path on `.`, walk JSON value tree
    - Return None if any segment missing
  - Implement `json_value_to_string()` (from pseudocode lines 099-107)
    - String -> return inner string
    - Number/Bool -> to_string()
    - Array/Object -> serde_json::to_string()
    - Null -> empty string
  - Implement `parse_outcome_name()` (from pseudocode lines 109-118)
    - Match lowercase name to StepOutcome variant
  - Modify `execute()` method (from pseudocode lines 001-086):
    - Add stdin handling before spawn (lines 006-018, 024-033)
    - Change Command spawn to use `child` pattern for stdin piping (lines 028-035)
    - Add exit_code_map evaluation: extract `exit_code_map` from params as `Option<HashMap<i32, String>>`; on non-zero exit, check map first — if code is mapped, return the mapped StepOutcome via `parse_outcome_name()`; otherwise fall through to default Fixable
    - Add outcome_on_stdout scanning BEFORE JSON parsing (lines 048-058) — if outcome_on_stdout matches a non-Success outcome, return immediately without JSON parsing
    - Add JSON output parsing AFTER outcome determination (lines 060-078) — only reached if outcome_on_stdout didn't match
    - Preserve existing behavior when new params not present
  - ADD markers: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P05`

### Implementation Details (Pseudocode Reference)

#### `execute()` method rewrite:

**Lines 006-018 (stdin setup):**
```
Extract optional "stdin" and "stdin_file" from params
If stdin: interpolate string, store for piping
If stdin_file: resolve path relative to work_dir, read contents
If file missing or unreadable: set diagnostic in context, return Ok(Fatal)
```

**Lines 020-035 (command spawn with stdin):**
```
Build Command with sh -c
If stdin_data is Some: set cmd.stdin(Stdio::piped)
Spawn as child process (not .output())
If stdin_data: write to child.stdin, drop stdin handle
Wait for output with child.wait_with_output()
```

**Lines 048-058 (outcome determination — BEFORE JSON parsing):**
```
If exit code != 0:
  If exit_code_map is Some AND contains this exit code:
    Convert mapped outcome name to StepOutcome via parse_outcome_name()
    Return Ok(mapped_outcome)
  Else (unmapped non-zero):
    Return Ok(Fixable) -- existing default behavior
If outcome_on_stdout present: scan stdout for each pattern string
  First match: convert outcome name to StepOutcome, return it immediately
  (This short-circuits JSON parsing — crucial for steps that use both
   outcome_on_stdout and output_format, where the fatal-path stdout
   should not be parsed for context_map extraction)
  No match: fall through to JSON parsing
```

**Why exit_code_map is checked before outcome_on_stdout**: Exit code mapping applies only to non-zero exits. When exit code is 0, `outcome_on_stdout` and JSON parsing proceed as normal. When exit code is non-zero, `exit_code_map` provides specific mapping (e.g., code 1 → Fatal) while unmapped codes fall back to the existing Fixable default. This means `outcome_on_stdout` is never evaluated for non-zero exits, consistent with REQ-LF-SHELL-007.

**Lines 060-078 (JSON parsing — AFTER outcome determination):**
```
Check params for "output_format" == "json"
Parse stdout as serde_json::Value
If parse fails: set json_parse_error in context, return Ok(Fatal)
If context_map present: iterate entries, extract each dot-path
If any path missing: set json_path_error with available top-level keys, return Ok(Fatal)
Set each extracted value in context
Return Ok(Success)
```

**Why this order matters**: Steps like `select_issue` use both `outcome_on_stdout` (to detect fatal conditions) and `output_format: "json"` + `context_map` (to extract data on success). By evaluating `outcome_on_stdout` first, the executor can return Fatal immediately when the stdout contains a fatal marker, without attempting JSON parsing or `context_map` extraction. This avoids the contradiction where `output_format: "json"` would fail on non-JSON fatal output, or where `context_map` would try to extract fields that only exist on the success path.


### Constraints

- Do NOT modify any test files
- Do NOT create new files (only modify `shell.rs`)
- All 14 tests from Phase 04 must pass
- All existing tests must still pass
- No `todo!()`, `unimplemented!()`, `println!()`, or `dbg!()` in final code

## Verification Commands

### Automated Checks

```bash
# All enhanced shell tests pass
cargo test --test shell_enhanced_tests || exit 1
# Expected: 17 tests pass, 0 failures

# All existing tests still pass
cargo test || exit 1

# No test modifications
git diff tests/shell_enhanced_tests.rs | head -5
# Expected: no output (tests unchanged)

# No debug code
grep -rn "println!\|dbg!\|todo!\|unimplemented!" src/engine/executors/shell.rs
# Expected: No matches

# Plan markers
grep -c "@plan:PLAN-20260408-LLXPRT-FIRST.P05" src/engine/executors/shell.rs
# Expected: 1+ occurrences

# No duplicate files
find src -name "*shell_v2*" -o -name "*shell_new*"
# Expected: No matches

# Clippy clean
cargo clippy -- -D warnings
```

### Deferred Implementation Detection

```bash
grep -rn "todo!\|unimplemented!" src/engine/executors/shell.rs
# Expected: No matches

grep -rn "// TODO\|// FIXME\|// HACK\|placeholder\|not yet" src/engine/executors/shell.rs
# Expected: No matches

grep -rn "fn .* \{\s*\}" src/engine/executors/shell.rs
# Expected: No empty function bodies
```

### Semantic Verification Checklist

1. **Does the code DO what the requirements say?**
   - [ ] JSON parsing: command stdout parsed, fields extracted via dot-path
   - [ ] Stdin piping: value piped to command's stdin
   - [ ] Stdin file: file contents piped to stdin
   - [ ] Outcome patterns: stdout scanned for configured strings
   - [ ] Non-zero exit: returns Fixable regardless of patterns (when no exit_code_map)
   - [ ] Exit code mapping: mapped non-zero exit codes return the specified StepOutcome
   - [ ] Exit code mapping: unmapped non-zero exit codes default to Fixable
   - [ ] Exit code mapping: exit code 0 ignores the map entirely
   - [ ] No match: defaults to Success
   - [ ] Missing file: returns Fatal
   - [ ] Invalid JSON: returns Fatal
   - [ ] Missing path: returns Fatal with available keys

2. **Is this REAL implementation, not placeholder?**
   - [ ] Deferred implementation detection passed
   - [ ] No empty function bodies

3. **Would the tests FAIL if implementation was removed?**
   - [ ] Tests verify actual context values, not just that code ran

4. **Backward compatibility preserved?**
   - [ ] Existing tests pass unchanged
   - [ ] Commands without new params work exactly as before

## Success Criteria

- All 17 enhanced shell tests pass
- All existing tests pass
- No todo!() or debug code in implementation
- Clippy passes
- Plan and requirement markers present

## Failure Recovery

If this phase fails:

1. Rollback: `git checkout -- src/engine/executors/shell.rs`
2. Verify: `cargo test --test executor_unit_tests` still passes
3. Re-run Phase 05

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P05.md`
