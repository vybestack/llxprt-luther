# Phase 04: Behavioral TDD for Config Resolution and Validation

## Phase ID
`PLAN-20260404-INITIAL-RUNTIME.P04`

## Prerequisites

- Required: Phase 03 verification completed
- Verification marker required: `project-plans/initial/plan/.completed/P03A.md`
- Preflight verification marker required: `project-plans/initial/plan/.completed/P00A.md`

## Requirements Implemented (Expanded)

### REQ-EARS-WF-004
**Full Text**: When a workflow run is requested, the engine shall resolve workflow type and workflow config files by configured identifiers.
**Behavior**:
- GIVEN: fixture files exist at `tests/fixtures/workflows/valid/issue-fix-v1.toml` and `tests/fixtures/workflow-configs/valid/profile-0.toml`
- WHEN: `resolve_workflow("issue-fix-v1", "profile-0", &fixture_root)` is called
- THEN: it returns `Ok(WorkflowInstance)` containing the parsed workflow type (with steps and transitions) and the parsed config (with runtime parameters and repository settings)
**Why This Matters**: Preserves declarative workflow behavior and keeps policy outside the Rust engine core.

### REQ-EARS-WF-005
**Full Text**: If workflow type or workflow config validation fails, then the engine shall reject run startup and emit structured validation errors.
**Behavior**:
- GIVEN: a malformed workflow type fixture exists (e.g., missing required `[steps]` table)
- WHEN: attempting to resolve/validate this workflow type
- THEN: it returns `Err(ValidationError)` with a structured error that names the missing field and the source file path
**Why This Matters**: Preserves declarative workflow behavior and keeps policy outside the Rust engine core.

### REQ-EARS-ENG-001
**Full Text**: When an engine run starts, the engine shall bind workflow type and workflow config into a concrete workflow instance.
**Behavior**:
- GIVEN: valid `WorkflowType` and `WorkflowConfig` have been resolved from disk
- WHEN: the engine creates a new run
- THEN: a `WorkflowInstance` is produced containing the merged type+config, a generated `run_id` (UUID), and initial state set to the first step defined in the workflow type's step list
**Why This Matters**: Guarantees deterministic execution, checkpointing, and safe shutdown/resume behavior.

### REQ-EARS-PERSIST-001
**Full Text**: The system shall persist run metadata, workflow instance identifiers, and state transitions in local durable storage.
**Behavior**:
- GIVEN: a new run is created with `(workflow_type_id="issue-fix-v1", config_id="profile-0", run_id=<uuid>)`
- WHEN: the run is initialized and persisted
- THEN: a SQLite row exists in the `runs` table with all three identifiers, a creation timestamp, and `status = "initialized"`
**Why This Matters**: Creates durable run traceability and prevents silent data-loss failures.

### REQ-EARS-SCALE-002
**Full Text**: The persisted run model shall include workflow type and config identifiers so later multi-instance scheduling can be added without schema redesign.
**Behavior**:
- GIVEN: a completed run has been persisted to SQLite
- WHEN: querying the `runs` table schema and row data
- THEN: columns `workflow_type_id` and `config_id` exist, are non-null, and contain the values used to create the run
**Why This Matters**: Keeps MVP single-instance while preserving future multi-profile extensibility.

## Implementation Tasks

### Files to Create
- `tests/config_binding_integration.rs`
- `tests/config_binding_json_parity_integration.rs`
- `tests/fixtures/workflows/valid/issue-fix-v1.toml`
- `tests/fixtures/workflows/valid/issue-fix-v1.json`
- `tests/fixtures/workflow-configs/valid/profile-0.toml`
- `tests/fixtures/workflow-configs/valid/profile-0.json`

### Files to Modify
- (none required)

### Required Code Markers

Every function/struct/test created in this phase must include markers:

```rust
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P04
/// @requirement:REQ-EARS-...
```

## Verification Commands

### Automated Checks

```bash
# Plan markers
grep -r "@plan:PLAN-20260404-INITIAL-RUNTIME.P04" src tests project-plans || true

# Test file existence
test -f tests/config_binding_integration.rs && echo "OK: config test" || echo "FAIL: config test missing"
test -f tests/config_binding_json_parity_integration.rs && echo "OK: JSON parity test" || echo "FAIL: JSON parity test missing"

# Fixture files
test -f tests/fixtures/workflows/valid/issue-fix-v1.toml && echo "OK: TOML fixture" || echo "FAIL: TOML fixture missing"
test -f tests/fixtures/workflows/valid/issue-fix-v1.json && echo "OK: JSON fixture" || echo "FAIL: JSON fixture missing"
test -f tests/fixtures/workflow-configs/valid/profile-0.toml && echo "OK: config TOML fixture" || echo "FAIL: missing"
test -f tests/fixtures/workflow-configs/valid/profile-0.json && echo "OK: config JSON fixture" || echo "FAIL: missing"

# Test count (expect >= 3 test functions per file)
echo "config_binding tests: $(grep -c '#\[test\]\|#\[rstest\]' tests/config_binding_integration.rs)"
echo "json_parity tests: $(grep -c '#\[test\]\|#\[rstest\]' tests/config_binding_json_parity_integration.rs)"

# Build compiles, tests expected to FAIL (TDD red phase)
cargo build --all-targets
cargo test 2>&1 | tail -20
echo "NOTE: test failures are EXPECTED in this TDD phase"
```

### Structural Verification Checklist

- [ ] Prerequisite marker exists (`P03A`)
- [ ] All listed files created/modified
- [ ] Plan + requirement markers present
- [ ] `cargo build` passes
- [ ] `cargo test` status matches phase expectation

### Semantic Verification Checklist

- [ ] Behavioral outcomes match the requirement text
- [ ] Tests would fail if implementation were removed
- [ ] Feature is reachable through planned call paths
- [ ] Behavioral tests are added first and fail naturally before implementation.

## Success Criteria

- Required artifacts and code changes are complete.
- Verification checklist is fully satisfied.
- Evidence is written to completion marker file.

## Failure Recovery

1. Revert files changed only by this phase as needed.
2. Fix issues discovered by verification.
3. Re-run this phase; do not proceed until PASS.

## Phase Completion Marker

Create: `project-plans/initial/plan/.completed/P04.md`

```markdown
Phase: P04
Verdict: PASS|FAIL
Evidence:
- commands run
- test/build outputs
- file list changed
```
