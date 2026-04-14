# Phase 20: CLI Production Wiring

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P20`

## Prerequisites

- Required: Phase 18a (E2E Verification) completed
- Verification: All graph routing and live integration tests pass
- Expected: Workflow TOML files exist in `config/workflows/` and `config/workflow-configs/`

## Purpose

Make `luther-workflow run --workflow-type X --config Y` resolve workflow types and configs from the production `config/` directory instead of the hardcoded `tests/fixtures` path. This is the bridge between "tested engine" and "usable tool."

Currently, `src/main.rs` hardcodes `tests/fixtures` as the fixture root:
```rust
let fixture_root = std::path::PathBuf::from("tests/fixtures");
```

This phase replaces that with a resolution strategy:
1. Look in `config/` relative to the current working directory (production use)
2. Support `--config-dir` CLI flag to override the root (for testing and custom setups)

This is generic engine infrastructure — it knows nothing about which workflows or configs exist. It just resolves files from the configured directory.

## Requirements Implemented

This phase doesn't map to a specific REQ-LF-* requirement — it fills an infrastructure gap. Without it, the engine can't actually be run against the production workflow files.

## Implementation Tasks

### Files to Modify

- `src/cli/mod.rs` — Add `--config-dir` to `RunArgs`
  - New field: `config_dir: Option<PathBuf>` with `#[arg(long, value_name = "DIR")]`
  - Description: "Directory containing workflows/ and workflow-configs/ subdirectories"
  - ADD marker: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P20`

- `src/main.rs` — `handle_run_command()`
  - Replace hardcoded `tests/fixtures` with resolution logic:
    ```rust
    let config_root = args.config_dir
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from("config"));
    ```
  - Use `config_root` as the fixture_root for `resolve_workflow_type()` and `resolve_workflow_config()`
  - Also update the resolve functions — currently they look in `{root}/workflows/valid/` and `{root}/workflow-configs/valid/`. For production, the structure should be:
    - `config/workflows/{id}.toml` (no `valid/` subdirectory — that's a test convention)
    - `config/workflow-configs/{id}.toml`
  - ADD marker: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P20`

- `src/workflow/config_loader.rs` — `resolve_workflow_type()` and `resolve_workflow_config()`
  - Currently looks in `{fixture_root}/workflows/valid/` and `{fixture_root}/workflow-configs/valid/`
  - Change to look in `{root}/workflows/` first, fall back to `{root}/workflows/valid/` (backward compat for test fixtures)
  - Same for workflow-configs: `{root}/workflow-configs/` first, `{root}/workflow-configs/valid/` fallback
  - This way both `config/workflows/llxprt-issue-fix-v1.toml` and `tests/fixtures/workflows/valid/test-workflow.toml` resolve correctly
  - ADD marker: `/// @plan:PLAN-20260408-LLXPRT-FIRST.P20`

### Files to Create

- `tests/cli_config_resolution_integration.rs` — Tests for the new resolution behavior
  - MUST include `@plan:PLAN-20260408-LLXPRT-FIRST.P20`

### Test List (4 tests)

1. **`test_resolve_workflow_type_from_config_dir`**
   - Use a temp dir with `workflows/test.toml`
   - Call `resolve_workflow_type("test", &temp_dir)`
   - Assert: resolves successfully (no `valid/` subdirectory needed)

2. **`test_resolve_workflow_type_from_valid_subdir`**
   - Use a temp dir with `workflows/valid/test.toml`
   - Call `resolve_workflow_type("test", &temp_dir)`
   - Assert: resolves successfully (backward compat)

3. **`test_resolve_workflow_config_from_config_dir`**
   - Same as #1 but for `workflow-configs/test.toml`

4. **`test_resolve_production_workflow_from_config`**
   - Call `resolve_workflow_type("llxprt-issue-fix-v1", &PathBuf::from("config"))`
   - Assert: resolves successfully, workflow_type_id matches
   - Call `resolve_workflow_config("llxprt-code", &PathBuf::from("config"))`
   - Assert: resolves successfully, config_id matches

### Constraints

- No domain-specific logic in the resolution code — it just finds files by ID
- Existing tests that use `tests/fixtures` as the root must continue to work
- The `config/` default is relative to cwd, not compiled in

## Verification Commands

```bash
# Plan markers
grep -c "@plan:PLAN-20260408-LLXPRT-FIRST.P20" src/cli/mod.rs src/main.rs src/workflow/config_loader.rs tests/cli_config_resolution_integration.rs
# Expected: 1+ per file

# --config-dir flag exists
cargo run -- run --help 2>&1 | grep "config-dir"
# Expected: found

# Resolution tests pass
cargo test --test cli_config_resolution_integration 2>&1 | grep "test result"
# Expected: 4 passed

# Existing tests still pass
cargo test 2>&1 | grep "test result"
# Expected: 0 failures

# Can resolve production workflows
cargo run -- run --workflow-type llxprt-issue-fix-v1 --config llxprt-code --dry-run
# Expected: prints step list and "Dry run complete"

cargo clippy -- -D warnings
```

### Structural Verification

- [ ] `--config-dir` flag added to CLI
- [ ] Default resolution uses `config/` relative to cwd
- [ ] `config_loader.rs` checks `{root}/workflows/` before `{root}/workflows/valid/`
- [ ] Existing test fixtures still resolve via `valid/` subdirectory fallback
- [ ] `luther-workflow run --dry-run` works against production config files
- [ ] No domain-specific terms in the resolution code

## Success Criteria

- `luther-workflow run --workflow-type llxprt-issue-fix-v1 --config llxprt-code --dry-run` works
- All existing tests pass
- 4 new resolution tests pass
- Production and test fixture paths both resolve

## Failure Recovery

1. Rollback: `git checkout -- src/cli/mod.rs src/main.rs src/workflow/config_loader.rs`
2. Delete: `rm tests/cli_config_resolution_integration.rs`
3. Verify: `cargo test`

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P20.md`
