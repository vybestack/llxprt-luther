# Phase 01: Domain Analysis and Boundary Definition

## Phase ID
`PLAN-20260404-INITIAL-RUNTIME.P01`

## Prerequisites

- Required: Phase 0.5 preflight completed
- Verification marker required: `project-plans/initial/plan/.completed/P00A.md`
- Preflight verification marker required: `project-plans/initial/plan/.completed/P00A.md`

## Requirements Implemented (Expanded)

### REQ-EARS-ARCH-001
**Full Text**: The runtime platform shall separate monitor responsibilities, engine responsibilities, workflow type definitions, and workflow instance configuration.
**Behavior**:
- GIVEN: `src/` contains separate `monitor/`, `engine/`, `workflow/`, and `persistence/` module trees
- WHEN: examining module dependency graph (`use` / `mod` statements across all four modules)
- THEN: each layer depends only downward (monitor→engine, engine→workflow+persistence), with no circular or upward imports
**Why This Matters**: Keeps the runtime evolvable and prevents the boundary collapse seen in the previous attempt.

### REQ-EARS-ARCH-002
**Full Text**: The engine shall not embed workflow-domain-specific policy logic that belongs in workflow type/config definitions.
**Behavior**:
- GIVEN: the engine module (`src/engine/`) is fully implemented
- WHEN: searching engine source files for domain-specific step names (e.g., `scan`, `plan`, `implement`, `commit`) or hardcoded workflow logic
- THEN: zero matches for hardcoded step names or domain policy; all step definitions and routing rules are loaded from workflow TOML, not compiled into Rust
**Why This Matters**: Keeps the runtime evolvable and prevents the boundary collapse seen in the previous attempt.

### REQ-EARS-ARCH-003
**Full Text**: The monitor shall supervise engine lifecycle without depending on workflow step semantics.
**Behavior**:
- GIVEN: the monitor module (`src/monitor/`) is fully implemented
- WHEN: examining monitor imports and public API surface
- THEN: monitor references only engine lifecycle types (start/stop/status/health), never workflow step types, transition types, or config schema structs
**Why This Matters**: Keeps the runtime evolvable and prevents the boundary collapse seen in the previous attempt.

### REQ-EARS-ARCH-004
**Full Text**: The engine shall instantiate workflow execution from `(workflow_type_id, config_id, run_id)`.
**Behavior**:
- GIVEN: a valid workflow type TOML and config TOML exist on disk
- WHEN: the engine creates a new workflow run
- THEN: the resulting `WorkflowRunRef` contains three non-empty identifiers: `workflow_type_id`, `config_id`, and `run_id` (UUID)
**Why This Matters**: Keeps the runtime evolvable and prevents the boundary collapse seen in the previous attempt.

### REQ-EARS-ARCH-005
**Full Text**: Where multi-instance execution is disabled, the monitor shall enforce a single active workflow instance while preserving type/config identifiers in persisted metadata.
**Behavior**:
- GIVEN: multi-instance execution is disabled (MVP default) and one run is already active
- WHEN: a second workflow run is requested for the same scope
- THEN: the monitor rejects the second run with an error that includes the active run's `workflow_type_id` and `config_id`
**Why This Matters**: Keeps the runtime evolvable and prevents the boundary collapse seen in the previous attempt.

### REQ-EARS-SCALE-001
**Full Text**: While MVP single-instance mode is enabled, the monitor shall run exactly one active workflow instance loop.
**Behavior**:
- GIVEN: MVP single-instance mode is active (the default)
- WHEN: the monitor starts and enters its run loop
- THEN: exactly one engine instance loop runs at a time; the monitor never spawns concurrent engine processes
**Why This Matters**: Keeps MVP single-instance while preserving future multi-profile extensibility.

### REQ-EARS-SCALE-002
**Full Text**: The persisted run model shall include workflow type and config identifiers so later multi-instance scheduling can be added without schema redesign.
**Behavior**:
- GIVEN: a completed run has been persisted to SQLite
- WHEN: querying the `runs` table schema and row data
- THEN: columns `workflow_type_id` and `config_id` exist, are non-null, and contain the values used to create the run
**Why This Matters**: Keeps MVP single-instance while preserving future multi-profile extensibility.

### REQ-EARS-SCALE-003
**Full Text**: Where multiple workflow instance profiles are configured, the monitor shall be able to select a configured instance by ID without changing workflow type code.
**Behavior**:
- GIVEN: multiple workflow config profiles exist on disk (e.g., `profile-0.toml`, `profile-1.toml`)
- WHEN: the monitor is started with `--config-id profile-1`
- THEN: it loads and executes using `profile-1` configuration, without requiring changes to the workflow type definition or Rust source code
**Why This Matters**: Keeps MVP single-instance while preserving future multi-profile extensibility.

## Implementation Tasks

### Files to Create
- `project-plans/initial/analysis/domain-model.md`
- `project-plans/initial/analysis/integration-touchpoints.md`

### Files to Modify
- `project-plans/initial/execution-tracker.md`

### Required Code Markers

Every function/struct/test created in this phase must include markers:

```rust
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P01
/// @requirement:REQ-EARS-...
```

## Verification Commands

### Automated Checks

```bash
grep -r "@plan:PLAN-20260404-INITIAL-RUNTIME.P01" src tests project-plans || true
cargo build --all-targets
cargo test
```

### Structural Verification Checklist

- [ ] Prerequisite marker exists (`P00A`)
- [ ] All listed files created/modified
- [ ] Plan + requirement markers present
- [ ] `cargo build` passes
- [ ] `cargo test` status matches phase expectation

### Semantic Verification Checklist

- [ ] Behavioral outcomes match the requirement text
- [ ] Tests would fail if implementation were removed
- [ ] Feature is reachable through planned call paths
- [ ] No production code changes; analysis artifacts are complete and internally consistent.

## Success Criteria

- Required artifacts and code changes are complete.
- Verification checklist is fully satisfied.
- Evidence is written to completion marker file.

## Failure Recovery

1. Revert files changed only by this phase as needed.
2. Fix issues discovered by verification.
3. Re-run this phase; do not proceed until PASS.

## Phase Completion Marker

Create: `project-plans/initial/plan/.completed/P01.md`

```markdown
Phase: P01
Verdict: PASS|FAIL
Evidence:
- commands run
- test/build outputs
- file list changed
```
