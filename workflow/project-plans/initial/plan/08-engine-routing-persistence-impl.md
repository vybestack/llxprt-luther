# Phase 08: Engine Execution, Loop Guard, and Persistence Implementation

## Phase ID
`PLAN-20260404-INITIAL-RUNTIME.P08`

## Prerequisites

- Required: Phase 07 verification completed
- Verification marker required: `project-plans/initial/plan/.completed/P07A.md`
- Preflight verification marker required: `project-plans/initial/plan/.completed/P00A.md`

## Requirements Implemented (Expanded)

### REQ-EARS-ENG-002
**Full Text**: While a workflow instance is executing, the engine shall persist checkpoints and structured events after each step transition.
**Behavior**:
- GIVEN: a workflow instance is executing with steps [A → B → C]
- WHEN: step A completes with outcome `Success`
- THEN: before entering step B, the engine writes a checkpoint row to SQLite with `(run_id, step="A", outcome="success", timestamp)` and appends an event record to the events table
**Why This Matters**: Guarantees deterministic execution, checkpointing, and safe shutdown/resume behavior.

### REQ-EARS-ENG-003
**Full Text**: If a step returns a fatal error condition, then the engine shall route to configured terminal failure handling and write terminal run artifacts.
**Behavior**:
- GIVEN: a workflow instance is executing and step B returns a fatal error outcome
- WHEN: the engine processes the `Fatal` step outcome
- THEN: the engine routes to the configured terminal failure handler, writes terminal run artifacts (final status, error details, timestamp), and does NOT attempt step C or any subsequent step
**Why This Matters**: Guarantees deterministic execution, checkpointing, and safe shutdown/resume behavior.

### REQ-EARS-ENG-004
**Full Text**: When an interrupt/shutdown signal is received, the engine shall persist a resumable checkpoint and exit cleanly.
**Behavior**:
- GIVEN: a workflow instance is mid-execution at step B
- WHEN: a SIGINT/SIGTERM signal is received by the engine process
- THEN: the engine persists a checkpoint with `(current_step="B", status="interrupted")`, exits cleanly (code 0), and a subsequent run with the same `run_id` can resume from step B's checkpoint
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

### REQ-EARS-PERSIST-001
**Full Text**: The system shall persist run metadata, workflow instance identifiers, and state transitions in local durable storage.
**Behavior**:
- GIVEN: a new run is created with `(workflow_type_id="issue-fix-v1", config_id="profile-0", run_id=<uuid>)`
- WHEN: the run is initialized and persisted
- THEN: a SQLite row exists in the `runs` table with all three identifiers, a creation timestamp, and `status = "initialized"`
**Why This Matters**: Creates durable run traceability and prevents silent data-loss failures.

### REQ-EARS-PERSIST-002
**Full Text**: When each step completes, the engine shall append an event record and persist checkpoint data before entering the next step.
**Behavior**:
- GIVEN: a run is executing and step `scan` completes with outcome `success`
- WHEN: the engine persists the step completion
- THEN: the `events` table has a new row with `(run_id, step="scan", outcome="success", timestamp)` AND the `checkpoints` table has a row with `(run_id, last_completed_step="scan", next_step="plan")`
**Why This Matters**: Creates durable run traceability and prevents silent data-loss failures.

### REQ-EARS-PERSIST-003
**Full Text**: The artifact subsystem shall write per-run outputs under deterministic run-scoped directories.
**Behavior**:
- GIVEN: a run with `run_id = "abc-123"` produces output artifacts
- WHEN: the artifact subsystem writes output for this run
- THEN: files are written under `<artifacts_root>/abc-123/` and the directory path is deterministic (same `run_id` always resolves to the same path)
**Why This Matters**: Creates durable run traceability and prevents silent data-loss failures.

### REQ-EARS-PERSIST-004
**Full Text**: If persistence writes fail, then the engine shall raise a structured persistence error and avoid silent continuation.
**Behavior**:
- GIVEN: the SQLite database file is read-only or the disk is full
- WHEN: the engine attempts to write a checkpoint
- THEN: it returns `Err(PersistenceError)` with the underlying IO/SQLite error details, and the engine does NOT silently continue to the next step
**Why This Matters**: Creates durable run traceability and prevents silent data-loss failures.

## Implementation Tasks

### Files to Modify
- `src/engine/runner.rs`
- `src/engine/transition.rs`
- `src/persistence/checkpoint.rs`
- `src/persistence/artifacts.rs`

### Additional Files to Modify
- `src/engine/mod.rs`
- `src/persistence/mod.rs`

### Required Code Markers

Every function/struct/test created in this phase must include markers:

```rust
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
/// @requirement:REQ-EARS-...
```

- Pseudocode reference: `analysis/pseudocode/engine-runner.md` lines 1-12
## Verification Commands

### Automated Checks

```bash
# Plan markers
grep -r "@plan:PLAN-20260404-INITIAL-RUNTIME.P08" src tests project-plans || true

# ALL P07 TDD tests must now pass
cargo test --test engine_execution_integration
cargo test --test persistence_integration
cargo test --test engine_resume_integration

# Verify no stubs remain in engine/persistence impl files
grep -rn "todo!\|unimplemented!" src/engine/ src/persistence/ && echo "FAIL: stubs remain" || echo "OK: no stubs"

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

- [ ] Prerequisite marker exists (`P07A`)
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

Create: `project-plans/initial/plan/.completed/P08.md`

```markdown
Phase: P08
Verdict: PASS|FAIL
Evidence:
- commands run
- test/build outputs
- file list changed
```
