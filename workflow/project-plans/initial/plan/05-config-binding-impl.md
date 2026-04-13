# Phase 05: Config Resolution and Binding Implementation

## Phase ID
`PLAN-20260404-INITIAL-RUNTIME.P05`

## Prerequisites

- Required: Phase 04 verification completed
- Verification marker required: `project-plans/initial/plan/.completed/P04A.md`
- Preflight verification marker required: `project-plans/initial/plan/.completed/P00A.md`

## Requirements Implemented (Expanded)

### REQ-EARS-ARCH-004
**Full Text**: The engine shall instantiate workflow execution from `(workflow_type_id, config_id, run_id)`.
**Behavior**:
- GIVEN: a valid workflow type TOML and config TOML exist on disk
- WHEN: the engine creates a new workflow run
- THEN: the resulting `WorkflowRunRef` contains three non-empty identifiers: `workflow_type_id`, `config_id`, and `run_id` (UUID)
**Why This Matters**: Keeps the runtime evolvable and prevents the boundary collapse seen in the previous attempt.

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
- `src/workflow/config_loader.rs`
- `src/engine/instance.rs`
- `src/persistence/run_metadata.rs`
- `src/persistence/sqlite.rs`

### Files to Modify
- `src/workflow/mod.rs`
- `src/engine/mod.rs`
- `src/persistence/mod.rs`
- `Cargo.toml`

## Required dependency additions in this phase

- `uuid`

### Required Code Markers

Every function/struct/test created in this phase must include markers:

```rust
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
/// @requirement:REQ-EARS-...
```

- Pseudocode reference: `analysis/pseudocode/config-loading.md` lines 1-12
## Verification Commands

### Automated Checks

```bash
# Plan markers
grep -r "@plan:PLAN-20260404-INITIAL-RUNTIME.P05" src tests project-plans || true

# Implementation files exist
test -f src/workflow/config_loader.rs && echo "OK" || echo "FAIL: config_loader.rs missing"
test -f src/engine/instance.rs && echo "OK" || echo "FAIL: instance.rs missing"
test -f src/persistence/run_metadata.rs && echo "OK" || echo "FAIL: run_metadata.rs missing"
test -f src/persistence/sqlite.rs && echo "OK" || echo "FAIL: sqlite.rs missing"

# uuid dependency added
grep -q 'uuid' Cargo.toml || echo "FAIL: uuid not in Cargo.toml"

# Key implementation types
grep -q "pub fn resolve_workflow" src/workflow/config_loader.rs || echo "FAIL: resolve_workflow not found"
grep -q "pub struct WorkflowInstance" src/engine/instance.rs || echo "FAIL: WorkflowInstance not found"
grep -q "WorkflowRunRef" src/persistence/run_metadata.rs || echo "FAIL: WorkflowRunRef not found"

# ALL P04 TDD tests must pass
cargo test --test config_binding_integration
cargo test --test config_binding_json_parity_integration

cargo build --all-targets
cargo test
```


### Deferred Implementation Detection (MANDATORY)

```bash
grep -rn "todo!\|unimplemented!" src tests
# Expected: no matches in implementation targets

grep -rn "// TODO\|// FIXME\|// HACK" src tests
# Expected: no matches in implementation targets
```
### Structural Verification Checklist

- [ ] Prerequisite marker exists (`P04A`)
- [ ] All listed files created/modified
- [ ] Plan + requirement markers present
- [ ] `cargo build` passes
- [ ] `cargo test` status matches phase expectation

### Semantic Verification Checklist

- [ ] Behavioral outcomes match the requirement text
- [ ] Tests would fail if implementation were removed
- [ ] Feature is reachable through planned call paths
- [ ] All preceding TDD tests pass without weakening tests or adding placeholders.

## Success Criteria

- Required artifacts and code changes are complete.
- Verification checklist is fully satisfied.
- Evidence is written to completion marker file.

## Failure Recovery

1. Revert files changed only by this phase as needed.
2. Fix issues discovered by verification.
3. Re-run this phase; do not proceed until PASS.

## Phase Completion Marker

Create: `project-plans/initial/plan/.completed/P05.md`

```markdown
Phase: P05
Verdict: PASS|FAIL
Evidence:
- commands run
- test/build outputs
- file list changed
```
