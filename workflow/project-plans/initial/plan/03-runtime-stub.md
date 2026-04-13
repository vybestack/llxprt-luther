# Phase 03: Config and Schema Harness Stub

## Phase ID
`PLAN-20260404-INITIAL-RUNTIME.P03`

## Prerequisites

- Required: Phase 02 verification completed
- Verification marker required: `project-plans/initial/plan/.completed/P02A.md`
- Preflight verification marker required: `project-plans/initial/plan/.completed/P00A.md`

## Requirements Implemented (Expanded)

### REQ-EARS-WF-001
**Full Text**: The system shall treat workflow topology as external declarative data, separate from Rust implementation code.
**Behavior**:
- GIVEN: a workflow type TOML file defines `[[steps]]`, `[[transitions]]`, and `[guards]`
- WHEN: parsing the TOML file into a `WorkflowType` struct
- THEN: the parsed struct's step count, transition edges, and guard references match the TOML content exactly, with no hardcoded steps in Rust source
**Why This Matters**: Preserves declarative workflow behavior and keeps policy outside the Rust engine core.

### REQ-EARS-WF-002
**Full Text**: The system shall support TOML as the primary format for workflow type and instance configuration.
**Behavior**:
- GIVEN: a valid workflow type TOML file exists at `config/workflows/issue-fix-v1.toml`
- WHEN: calling the config loader with the TOML file path
- THEN: parsing succeeds and returns a `WorkflowType` with all fields (steps, transitions, guards) populated from the TOML
**Why This Matters**: Preserves declarative workflow behavior and keeps policy outside the Rust engine core.

### REQ-EARS-WF-003
**Full Text**: Where JSON input support is enabled, the system shall accept semantically equivalent JSON representations of workflow type and instance configuration.
**Behavior**:
- GIVEN: a JSON file exists that is semantically equivalent to the reference TOML workflow type
- WHEN: parsing both the TOML and JSON versions via the config loader
- THEN: the resulting `WorkflowType` structs are equal (`assert_eq!`): same steps, same transitions, same guards, same parameters
**Why This Matters**: Preserves declarative workflow behavior and keeps policy outside the Rust engine core.

### REQ-EARS-WF-006
**Full Text**: The workflow type definition shall include step topology, transitions, and guard references.
**Behavior**:
- GIVEN: a workflow type TOML file contains `[[steps]]`, `[[transitions]]`, and `[guards]` sections
- WHEN: deserializing the file into `WorkflowType`
- THEN: `workflow_type.steps` is non-empty, `workflow_type.transitions` maps step outputs to next steps, and `workflow_type.guards` contains the referenced guard configurations
**Why This Matters**: Preserves declarative workflow behavior and keeps policy outside the Rust engine core.

### REQ-EARS-WF-007
**Full Text**: The workflow instance config shall include runtime parameters, guard limits, adapter settings, and repository workspace/branch settings.
**Behavior**:
- GIVEN: a workflow config TOML file contains `[runtime]`, `[guards]`, `[adapters]`, and `[repository]` sections
- WHEN: deserializing the file into `WorkflowConfig`
- THEN: all four sections are populated: `runtime` has timeout/retry values, `guards` has limit values, `adapters` has adapter-specific settings, and `repository` has workspace strategy and branch configuration
**Why This Matters**: Preserves declarative workflow behavior and keeps policy outside the Rust engine core.

### REQ-EARS-ARCH-004
**Full Text**: The engine shall instantiate workflow execution from `(workflow_type_id, config_id, run_id)`.
**Behavior**:
- GIVEN: a valid workflow type TOML and config TOML exist on disk
- WHEN: the engine creates a new workflow run
- THEN: the resulting `WorkflowRunRef` contains three non-empty identifiers: `workflow_type_id`, `config_id`, and `run_id` (UUID)
**Why This Matters**: Keeps the runtime evolvable and prevents the boundary collapse seen in the previous attempt.

## Implementation Tasks

### Files to Create
- `src/workflow/mod.rs`
- `src/workflow/schema.rs`
- `src/engine/dagrs_runtime.rs`
- `config/workflows/issue-fix-v1.toml`
- `config/workflow-configs/profile-0.toml`
- `tests/fixtures/workflows/issue-fix-v1.toml`
- `tests/fixtures/workflow-configs/profile-0.toml`

### Files to Modify
- `src/lib.rs`
- `Cargo.toml`

## Required dependency additions in this phase

- `dagrs`
- `toml`

### Required Code Markers

Every function/struct/test created in this phase must include markers:

```rust
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
/// @requirement:REQ-EARS-...
```

## Verification Commands

### Automated Checks

```bash
# Plan markers
grep -r "@plan:PLAN-20260404-INITIAL-RUNTIME.P03" src tests project-plans || true

# File existence
test -f src/workflow/mod.rs && echo "OK: workflow/mod.rs" || echo "FAIL: workflow/mod.rs missing"
test -f src/workflow/schema.rs && echo "OK: schema.rs" || echo "FAIL: schema.rs missing"
test -f src/engine/dagrs_runtime.rs && echo "OK: dagrs_runtime.rs" || echo "FAIL: dagrs_runtime.rs missing"
test -f config/workflows/issue-fix-v1.toml && echo "OK: workflow TOML" || echo "FAIL: workflow TOML missing"
test -f config/workflow-configs/profile-0.toml && echo "OK: config TOML" || echo "FAIL: config TOML missing"

# Key types compile
grep -q "pub struct WorkflowType" src/workflow/schema.rs || echo "FAIL: WorkflowType not found"
grep -q "pub struct WorkflowConfig" src/workflow/schema.rs || echo "FAIL: WorkflowConfig not found"

# Dependencies added
grep -q 'dagrs' Cargo.toml || echo "FAIL: dagrs not in Cargo.toml"
grep -q 'toml' Cargo.toml || echo "FAIL: toml not in Cargo.toml"

# Build
cargo build --all-targets
cargo test
```

### Structural Verification Checklist

- [ ] Prerequisite marker exists (`P02A`)
- [ ] All listed files created/modified
- [ ] Plan + requirement markers present
- [ ] `cargo build` passes
- [ ] `cargo test` status matches phase expectation

### Semantic Verification Checklist

- [ ] Behavioral outcomes match the requirement text
- [ ] Tests would fail if implementation were removed
- [ ] Feature is reachable through planned call paths
- [ ] Code compiles and exposes seams for next TDD phase; stubs are allowed in this phase only.

## Success Criteria

- Required artifacts and code changes are complete.
- Verification checklist is fully satisfied.
- Evidence is written to completion marker file.

## Failure Recovery

1. Revert files changed only by this phase as needed.
2. Fix issues discovered by verification.
3. Re-run this phase; do not proceed until PASS.

## Phase Completion Marker

Create: `project-plans/initial/plan/.completed/P03.md`

```markdown
Phase: P03
Verdict: PASS|FAIL
Evidence:
- commands run
- test/build outputs
- file list changed
```
