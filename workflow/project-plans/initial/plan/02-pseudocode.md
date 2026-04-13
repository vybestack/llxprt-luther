# Phase 02: Pseudocode and Integration Blueprint

## Phase ID
`PLAN-20260404-INITIAL-RUNTIME.P02`

## Prerequisites

- Required: Phase 01 verification completed
- Verification marker required: `project-plans/initial/plan/.completed/P01A.md`
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

### REQ-EARS-ROUTE-001
**Full Text**: The engine shall route transitions using structured step outcomes rather than string-matching unstructured logs.
**Behavior**:
- GIVEN: a workflow type defines transitions `{step: "build", on_success: "test", on_failure: "diagnose"}`
- WHEN: the `build` step completes with outcome `StepOutcome::Success`
- THEN: the engine routes to `test` by matching the structured `StepOutcome` enum variant against the transition table (not by parsing log strings)
**Why This Matters**: Ensures branching/loops are explicit and bounded instead of ad hoc control flow.

### REQ-EARS-ROUTE-002
**Full Text**: While in remediation-capable states, the engine shall permit configured loop-back transitions to prior execution states.
**Behavior**:
- GIVEN: a workflow type defines transition `{step: "diagnose", on_fixable: "implement"}` and the loop counter is below the configured limit
- WHEN: the `diagnose` step returns `StepOutcome::Fixable`
- THEN: the engine transitions back to `implement`, increments the loop counter for this remediation cycle, and the loop counter value is persisted
**Why This Matters**: Ensures branching/loops are explicit and bounded instead of ad hoc control flow.

### REQ-EARS-ROUTE-003
**Full Text**: If configured loop limits are reached, then the engine shall route to configured abandonment/terminal logging outcomes.
**Behavior**:
- GIVEN: a workflow config sets `guards.max_remediation_loops = 3` and the loop counter has reached 3
- WHEN: the `diagnose` step returns `StepOutcome::Fixable` again
- THEN: the engine routes to the configured abandonment step (e.g., `abandon_and_log`) instead of looping back, writes a terminal record with reason `loop_limit_exceeded`, and does not increment the counter
**Why This Matters**: Ensures branching/loops are explicit and bounded instead of ad hoc control flow.

### REQ-EARS-ROUTE-004
**Full Text**: The engine shall enforce retry and loop guardrails from workflow config.
**Behavior**:
- GIVEN: a workflow config defines `guards.max_retries = 2` and `guards.max_remediation_loops = 3`
- WHEN: the engine initializes a new run from this config
- THEN: the engine loads both limits into its guard state and checks them before every retry or loop-back transition during execution
**Why This Matters**: Ensures branching/loops are explicit and bounded instead of ad hoc control flow.

## Implementation Tasks

### Files to Create
- `project-plans/initial/analysis/pseudocode/config-loading.md`
- `project-plans/initial/analysis/pseudocode/engine-runner.md`
- `project-plans/initial/analysis/pseudocode/monitor-loop.md`
- `project-plans/initial/analysis/pseudocode/repository-prep.md`

### Files to Modify
- `project-plans/initial/execution-tracker.md`

### Required Code Markers

Every function/struct/test created in this phase must include markers:

```rust
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P02
/// @requirement:REQ-EARS-...
```

## Verification Commands

### Automated Checks

```bash
grep -r "@plan:PLAN-20260404-INITIAL-RUNTIME.P02" src tests project-plans || true
cargo build --all-targets
cargo test
```

### Structural Verification Checklist

- [ ] Prerequisite marker exists (`P01A`)
- [ ] All listed files created/modified
- [ ] Plan + requirement markers present
- [ ] `cargo build` passes
- [ ] `cargo test` status matches phase expectation

### Semantic Verification Checklist

- [ ] Behavioral outcomes match the requirement text
- [ ] Tests would fail if implementation were removed
- [ ] Feature is reachable through planned call paths
- [ ] Pseudocode steps are numbered and sufficient for implementation phases to cite line ranges.

## Success Criteria

- Required artifacts and code changes are complete.
- Verification checklist is fully satisfied.
- Evidence is written to completion marker file.

## Failure Recovery

1. Revert files changed only by this phase as needed.
2. Fix issues discovered by verification.
3. Re-run this phase; do not proceed until PASS.

## Phase Completion Marker

Create: `project-plans/initial/plan/.completed/P02.md`

```markdown
Phase: P02
Verdict: PASS|FAIL
Evidence:
- commands run
- test/build outputs
- file list changed
```
