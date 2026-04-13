# Phase 01: Domain Analysis

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P01`

## Prerequisites

- Required: Phase 00a (Preflight Verification) completed
- All blocking issues from preflight addressed
- `cargo build --all-targets` passes
- `cargo test` passes

## Purpose

Analyze the existing codebase to understand every integration point, data flow, and modification required. This phase produces no code — only analysis artifacts that inform pseudocode and implementation.

## Analysis Tasks

### 1. ShellExecutor Enhancement Analysis

**Current behavior** (`src/engine/executors/shell.rs`):
- Extracts `command` from params JSON
- Interpolates command string via `interpolate_string()`
- Spawns `sh -c <interpolated_command>` with stdout/stderr piped
- Captures stdout, stderr, exit_code into context (flat keys: `"stdout"`, `"stderr"`, `"exit_code"`)
- Returns `Success` on exit 0, `Fixable` on non-zero

**New behavior needed**:
1. **JSON output parsing** (REQ-LF-SHELL-001, REQ-LF-SHELL-002, REQ-LF-SHELL-009):
   - After capturing stdout, check for `output_format: "json"` in params
   - Parse stdout as JSON via `serde_json::from_str`
   - If parse fails, return `Fatal` outcome (not error — EngineError is for infra failures, Fatal is for step failures)
   - Extract fields from parsed JSON using `context_map` dot-path notation
   - Set extracted values into context
   - If a dot-path doesn't resolve, return `Fatal` with diagnostic

2. **Stdin piping** (REQ-LF-SHELL-003, REQ-LF-SHELL-004, REQ-LF-SHELL-008):
   - Before spawning command, check for `stdin` or `stdin_file` in params
   - If `stdin`: interpolate value, pipe to command's stdin
   - If `stdin_file`: resolve path relative to work_dir, read file contents, pipe to stdin
   - If `stdin_file` and file doesn't exist, return `Fatal`
   - Change `Command` setup: use `Stdio::piped()` for stdin when needed, write to child's stdin before waiting

3. **Outcome pattern matching** (REQ-LF-SHELL-005, REQ-LF-SHELL-006, REQ-LF-SHELL-007):
   - After capturing stdout and exit code, check for `outcome_on_stdout` in params
   - If non-zero exit code, return `Fixable` regardless (existing behavior preserved)
   - If exit 0 and `outcome_on_stdout` configured, scan stdout for each key
   - Map first match to corresponding `StepOutcome` variant — return immediately (before JSON parsing)
   - If no match and exit 0, fall through to JSON parsing (if configured) then default to `Success`
   - **Note**: `outcome_on_stdout` is evaluated BEFORE `output_format`/`context_map` processing. This allows steps to use both features: `outcome_on_stdout` catches fatal/fixable conditions, while `output_format: "json"` + `context_map` extracts data on the success path.

**Integration points**:
- `execute()` method signature unchanged — no callers affected
- Params JSON gains new optional fields — backward compatible
- Context variable names unchanged — downstream steps unaffected

### 2. VerifyExecutor Analysis

**This is a new executor** — no existing code to modify.

**Behavior** (REQ-LF-VERIFY-001 through REQ-LF-VERIFY-009):
- Receives `checks` array from step params: `["lint", "typecheck", "test", "format", "build"]`
- For each check, runs a predefined command based on check type and project type
- Captures output and parses it with check-type-specific parsers
- Produces per-check pass/fail with structured error records
- Aggregates into a summary and writes `.luther/verify-report.json`
- Sets context variables: `verify_passed`, `verify_summary`, `test_failures`, `build_errors`, `type_errors`, `lint_errors`

**Commands per check type (Node/TypeScript MVP)**:
- `lint`: `npm run lint -- --format json 2>&1` (or `npx eslint --format json .`)
- `typecheck`: `npx tsc --noEmit 2>&1`
- `test`: `npx vitest run --reporter=json 2>&1` (fallback: `npm test -- --json`)
- `format`: `npx prettier --check . 2>&1`
- `build`: `npm run build 2>&1`

**Parser strategy**: Each check type has a parser function `fn parse_<type>_output(stdout: &str, stderr: &str, exit_code: i32) -> CheckResult`. Parsers extract structured errors where possible, fall back to raw output otherwise. Parsers NEVER cause Fatal — unparseable output produces a generic error record with the raw text.

**Check commands must be configurable**: The step params should allow overriding the default command per check type:
```toml
[steps.parameters]
checks = ["lint", "typecheck", "test"]

[steps.parameters.check_commands]
lint = "npm run lint -- --format json"
test = "npx vitest run --reporter=json"
```

**Integration points**:
- New file: `src/engine/executors/verify.rs`
- Register in `src/engine/executors/mod.rs` 
- Register in `ExecutorRegistry::with_defaults()` in `src/engine/executor.rs`

### 3. Namespaced Context Analysis

**Current behavior** (`src/engine/executor.rs`):
- `StepContext.variables`: flat `HashMap<String, String>`
- `set(key, value)`: inserts `key` → `value`
- `get(key)`: looks up `key` directly
- `interpolate_string()`: collects all keys from context + built-ins, sorts by length descending, replaces `{key}` patterns

**New behavior** (REQ-LF-CTX-001 through REQ-LF-CTX-004):
- `set(key, value)` when called during step execution stores as `step_id.key`
- `get("step_id.variable")` → direct lookup of namespaced key
- `get("variable")` → search all namespaced keys `*.variable`, return most-recently-set
- `interpolate_string()` must handle both `{step_id.variable}` and `{variable}` forms
- Built-ins `work_dir` and `run_id` always resolvable without namespace

**Implementation approach**:
- Add `current_step_id: Option<String>` to `StepContext`
- Add `step_order: Vec<String>` to track execution order
- `set(key, value)`:
  - If `current_step_id` is `Some(step_id)`: store as `"step_id.key"` in HashMap
  - Also store as `"key"` (overwriting) for backward compat during transition
- `get(key)`:
  - If key contains `.`: direct HashMap lookup
  - If key doesn't contain `.`: direct HashMap lookup (gets most-recently-set value)
  - Built-ins: check `work_dir`, `run_id` first
- `set_current_step_id(step_id)`: called by EngineRunner before executing each step
- `interpolate_string()`: no change to algorithm — the key resolution logic is in `get()`, and the function already iterates all keys

**Wait — simpler approach**: Store all values as `"step_id.key"`. For unnamespaced lookup, also store under bare `"key"` (last-write-wins). The HashMap naturally handles this with O(1) lookups. No ordering needed.

**Change to EngineRunner**:
- In `execute_step()`, before calling `self.registry.dispatch()`, call `self.context.set_current_step_id(step_id)` 
- This tells the context which step is currently executing, so `set()` can namespace correctly

**Backward compatibility**:
- Existing tests call `ctx.set("stdout", value)` directly — this should still work
- If `current_step_id` is `None`, `set()` stores under bare key only (no namespace prefix)
- Existing `{stdout}` interpolation continues to work because bare key is always set

### 4. Per-edge Loop Limits Analysis

**Current behavior** (`src/engine/runner.rs`):
- `EngineRunner` has `loop_count: u32` and `max_loops: u32`
- `is_loop_back()` checks if next step index ≤ current step index
- If loop_back detected: `loop_count += 1`, check against `max_loops`
- `max_loops` comes from `config.guard_limits.max_iterations.unwrap_or(10)`

**New behavior** (REQ-LF-LOOP-001 through REQ-LF-LOOP-005):
- `TransitionDef` gains `max_iterations: Option<u32>`
- Runner tracks `edge_loop_counts: HashMap<String, u32>` keyed by `"from:to"`
- When a transition is taken:
  - Look up the `TransitionDef` for this from/to/condition
  - If that transition has `max_iterations`, check `edge_loop_counts["from:to"]` against it
  - If that transition has no `max_iterations`, check against global `max_loops` fallback
  - Increment `edge_loop_counts["from:to"]`
- If limit exceeded, return `RunOutcome::Abandoned`
- `StateSnapshot` stores per-edge counts for checkpoint persistence

**Changes to TransitionDef**:
- `src/workflow/schema.rs`: Add `#[serde(default)] pub max_iterations: Option<u32>`
- `src/engine/transition.rs`: Add same field to the local `TransitionDef`
  - OR: eliminate the duplicate `TransitionDef` in transition.rs and use only schema.rs version
  - The duplicate exists because transition.rs was written before schema.rs deserialization was added
  - **Recommendation**: Keep both for now, add field to both. Deduplication is out of scope.

**Changes to EngineRunner**:
- Replace `loop_count: u32` with `edge_loop_counts: HashMap<String, u32>`
- Keep `max_loops: u32` as the global fallback
- Modify the loop detection in `run()`:
  - After resolving `next_step`, look up the matching `TransitionDef`
  - Compute edge key: `format!("{}:{}", current_step_id, next_step_id)`
  - Get per-edge limit: `transition_def.max_iterations.unwrap_or(self.max_loops)`
  - Increment and check

**Changes to StateSnapshot**:
- Add `edge_loop_counts: HashMap<String, u32>` (serialized in the `context` JSON blob)
- On checkpoint save: serialize edge counts into the context blob
- On checkpoint load: deserialize edge counts from context blob
- Keep `loop_count: u32` field for backward compat (set to sum of edge counts)

**Changes to existing tests**:
- Tests that check `runner.loop_count()` will need updating
- `create_checkpoint()` in runner.rs will need to include edge counts
- Tests constructing `TransitionDef` directly will need the new field

### 5. Workflow TOML Analysis

**Current workflow TOML schema** (`config/workflows/issue-fix-v1.toml`):
- Uses `workflow_type_id`, `[[steps]]` with `step_id`, `step_type`, `description`, `[steps.parameters]`
- Uses `[[transitions]]` with `from`, `to`, `condition`
- Uses `[guards]`

**New workflow TOML** (`config/workflows/llxprt-issue-fix-v1.toml`):
- Same schema + new fields on transitions: `max_iterations`
- New step_types: `"shell"` (with new params), `"verify"`
- New step parameters: `output_format`, `context_map`, `stdin`, `stdin_file`, `outcome_on_stdout`, `checks`
- Steps: ~15 steps covering the full issue-fix flow
- Transitions: ~25 transitions including loop-back edges

**Workflow config** (`config/workflow-configs/llxprt-code.toml`):
- Same schema as existing configs
- Additional variables section for profile mappings and repo config
- These go into context at run start via config loading

### 6. Integration Verification

**The test**: if you delete every file in `config/workflows/` and `config/workflow-configs/`, the engine should still compile and all engine-level tests should still pass.

**Current**: [OK] — engine tests use programmatic workflow construction, not config files

**After changes**: Must remain true. The new TOML files are data, not code. Engine tests must continue to use programmatic construction.

## Output Artifacts

This phase produces this analysis document. No code changes. The analysis informs Phase 02 (Pseudocode).

## Verification Commands

```bash
# No code changes — just verify nothing broke
cargo build --all-targets
cargo test
```

## Success Criteria

- All five component analyses complete with specific file paths, line ranges, and change descriptions
- Integration points identified for every modification
- Backward compatibility strategy documented for each change
- No blocking issues remain unresolved

## Failure Recovery

This phase has no code changes. No rollback needed.
