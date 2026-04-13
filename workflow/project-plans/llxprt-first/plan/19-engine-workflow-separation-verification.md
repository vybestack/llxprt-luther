# Phase 19: Engine/Workflow Separation Verification

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P19`

## Prerequisites

- Required: Phase 18a (E2E Verification) completed
- All previous phases complete: components implemented, TOML files created, E2E tests passing

## Purpose

Final verification that the engine/workflow separation boundary is maintained. This phase creates no new code — it is a verification-only gate that proves REQ-LF-SEP-001 through REQ-LF-SEP-003 by demonstrating the engine compiles and passes all engine-level tests with no workflow config files present.

This is the capstone verification of the plan: the engine is truly generic, and all domain knowledge lives in TOML data files and executor implementations.

## Requirements Implemented (Expanded)

### REQ-LF-SEP-001: No domain-specific code in engine

**Full Text**: The workflow engine shall contain no GitHub-specific, llxprt-specific, or Node/TypeScript-specific code. All domain operations shall be performed by executors dispatched via the registry.
**Behavior**:
- GIVEN: All engine source files in `src/engine/`
- WHEN: Searched for domain-specific terms
- THEN: No GitHub/llxprt/Node/TypeScript terms found (except in comments explaining domain-agnosticism)

### REQ-LF-SEP-002: Engine compiles without workflow files

**Full Text**: The engine shall compile and all engine-level tests shall pass with no workflow definition files present in the config directory.
**Behavior**:
- GIVEN: All files in `config/workflows/` and `config/workflow-configs/` are temporarily removed
- WHEN: `cargo build --all-targets` and `cargo test` are run (excluding E2E tests that load TOML fixtures)
- THEN: Build succeeds and all engine-level tests pass

### REQ-LF-SEP-003: Pure TOML data files

**Full Text**: The workflow type definition and workflow instance config shall be pure TOML data files containing zero Rust code or compiled logic.
**Behavior**:
- GIVEN: The workflow TOML files
- WHEN: Searched for Rust code patterns (fn, impl, struct, let, mod, use, pub)
- THEN: No Rust code found — only TOML key-value pairs and comments

## Verification Steps

### Step 1: Domain Term Search in Engine

Search all engine source files for domain-specific terms. The engine should know nothing about the domain.

```bash
# GitHub-specific terms
grep -rni "github\|gh \|gh_\|pull.request\|pr.create\|issue.list\|milestone" \
  src/engine/runner.rs src/engine/executor.rs src/engine/transition.rs \
  src/engine/instance.rs src/engine/executors/shell.rs src/engine/executors/write_file.rs \
  src/engine/executors/noop.rs
# Expected: no output

# llxprt-specific terms
grep -rni "llxprt\|claude\|profile.load\|\.yolo\|opusthinking\|sonnetthinking\|gpt54x" \
  src/engine/runner.rs src/engine/executor.rs src/engine/transition.rs \
  src/engine/instance.rs src/engine/executors/shell.rs src/engine/executors/write_file.rs \
  src/engine/executors/noop.rs
# Expected: no output

# Node/TypeScript-specific terms (outside VerifyExecutor parsers)
grep -rni "npm\|npx\|vitest\|eslint\|prettier\|tsc\|typescript\|node_modules" \
  src/engine/runner.rs src/engine/executor.rs src/engine/transition.rs \
  src/engine/instance.rs src/engine/executors/shell.rs src/engine/executors/write_file.rs \
  src/engine/executors/noop.rs
# Expected: no output

# Note: VerifyExecutor (verify.rs) IS allowed to have Node/TypeScript terms because its
# parsers handle specific output formats. The executor is domain-aware by design.
# But the engine (runner, executor registry, transition resolver) must not be.
```

### Step 2: Compile and Test Without Workflow Files

Temporarily move workflow files aside and verify the engine still works.

```bash
# Move workflow files to a temp location
mkdir -p /tmp/luther-sep-test
cp -r config/workflows config/workflow-configs /tmp/luther-sep-test/
rm -rf config/workflows/* config/workflow-configs/*

# Verify build succeeds
cargo build --all-targets
# Expected: success

# Run all engine-level tests (exclude E2E and config binding tests that need TOML files)
cargo test --test executor_unit_tests
cargo test --test engine_execution_integration
cargo test --test engine_resume_integration
cargo test --test persistence_integration
cargo test --test engine_integration_llxprt_first
cargo test --test per_edge_loop_tests
cargo test --test namespaced_context_tests
cargo test --test shell_enhanced_tests
cargo test --test verify_executor_tests
# Expected: all pass (these use programmatic workflow construction, not config files)

# Verify E2E tests that load TOML fixtures would fail (since we removed the files)
# This is expected and confirms those tests actually depend on the TOML files
cargo test --test e2e_workflow_integration 2>&1 | grep "FAILED\|error\|panicked"
# Expected: failures (TOML fixtures are gone)

# Config binding tests should also fail
cargo test --test config_binding_integration 2>&1 | grep "FAILED\|error\|panicked"
# Expected: failures (config files are gone)

# Restore workflow files
cp -r /tmp/luther-sep-test/workflows config/
cp -r /tmp/luther-sep-test/workflow-configs config/
rm -rf /tmp/luther-sep-test

# Verify everything passes again with files restored
cargo test
# Expected: all pass
```

### Step 3: TOML Files Contain No Rust Code

```bash
# Check workflow type TOML for Rust code patterns
grep -n "^fn \|^pub \|^impl \|^struct \|^enum \|^mod \|^use \|^let \|^const \|^static " \
  config/workflows/llxprt-issue-fix-v1.toml
# Expected: no output

# Check workflow config TOML for Rust code patterns
grep -n "^fn \|^pub \|^impl \|^struct \|^enum \|^mod \|^use \|^let \|^const \|^static " \
  config/workflow-configs/llxprt-code.toml
# Expected: no output

# Verify TOML files are valid TOML (not embedded Rust)
python3 -c "import tomllib; tomllib.load(open('config/workflows/llxprt-issue-fix-v1.toml', 'rb'))" 2>&1
# Expected: no error (valid TOML)

python3 -c "import tomllib; tomllib.load(open('config/workflow-configs/llxprt-code.toml', 'rb'))" 2>&1
# Expected: no error (valid TOML)
```

### Step 4: Final Full Test Suite

```bash
# Complete test suite with all files in place
cargo test 2>&1 | grep "test result"
# Expected: 0 failures, total test count includes all new tests

# Clippy
cargo clippy -- -D warnings
# Expected: no warnings

# Check total test count (should have grown significantly from baseline)
cargo test 2>&1 | tail -1
# Expected: ~190+ tests passing (144 baseline + ~46 new)
```

## Verification Checklist

### REQ-LF-SEP-001: No domain code in engine

- [ ] No GitHub terms in engine source (`src/engine/`)
- [ ] No llxprt terms in engine source
- [ ] No Node/TypeScript terms in engine source (excluding VerifyExecutor)
- [ ] Engine runner contains no workflow-specific routing logic
- [ ] Transition resolver is purely outcome-based (no domain-aware branching)

### REQ-LF-SEP-002: Engine compiles without workflow files

- [ ] `cargo build --all-targets` succeeds with empty `config/workflows/` and `config/workflow-configs/`
- [ ] All engine-level tests pass without workflow files
- [ ] E2E tests correctly fail without workflow files (proves they depend on the TOML)
- [ ] Restoring files makes all tests pass again

### REQ-LF-SEP-003: Pure TOML data files

- [ ] `llxprt-issue-fix-v1.toml` contains no Rust code constructs
- [ ] `llxprt-code.toml` contains no Rust code constructs
- [ ] Both files are valid TOML parseable by standard TOML parsers
- [ ] All workflow logic (steps, transitions, parameters) is declarative data

### Cross-cutting verification

- [ ] VerifyExecutor is the only executor with domain-aware code (Node/TypeScript parsers)
- [ ] VerifyExecutor's check suite is parameterized, not hardcoded in the engine
- [ ] Profile names appear only in TOML config files, never in Rust source
- [ ] `target_repo` and `assignee` appear only in TOML config files
- [ ] The engine's public API (EngineRunner, ExecutorRegistry, StepContext) has no domain parameters

## Success Criteria

- All 4 verification steps pass
- All checklist items confirmed
- REQ-LF-SEP-001 through REQ-LF-SEP-003 fully satisfied
- Complete test suite passes with all files present

## Failure Recovery

This phase has no code changes — it's verification only. If verification fails:

1. Identify the domain leakage or separation violation
2. Fix the offending code in the appropriate source file
3. Re-run verification
4. Ensure `config/workflows/` and `config/workflow-configs/` are restored if they were moved

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P19.md`
