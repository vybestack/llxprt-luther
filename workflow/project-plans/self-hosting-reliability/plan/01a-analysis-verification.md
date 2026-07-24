# Phase 01A: Analysis Verification

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P01A`

## Prerequisites

- Required: P01 completed.

## Purpose

Verify the domain analysis and integration map are grounded in the actual
source, with no assumed symbols.

## Verification Commands

```bash
# Every file cited in the integration map exists
for f in \
  src/engine/continuation.rs \
  src/engine/continuation/commit.rs \
  src/engine/continuation/selection.rs \
  src/engine/continuation/authorization.rs \
  src/engine/continuation/resume_authorization.rs \
  src/engine/runner.rs \
  src/persistence/checkpoint.rs \
  src/persistence/run_metadata.rs \
  src/persistence/launch_provenance.rs \
  src/persistence/leases.rs \
  src/persistence/legacy_migration_state.rs \
  src/engine/workspace_ownership.rs \
  src/engine/workspace_ownership/durable_publication.rs; do
  test -f "workflow/$f" || echo "MISSING: $f"
done

# Every key symbol cited exists
grep -rn "pub enum ContinuationKind" workflow/src/engine/continuation.rs
grep -rn "pub struct ContinuationRequest" workflow/src/engine/continuation.rs
grep -rn "pub fn commit_continuation" workflow/src/engine/continuation/commit.rs
grep -rn "pub enum RunStatus" workflow/src/persistence/run_metadata.rs
grep -rn "Merged" workflow/src/persistence/run_metadata.rs
grep -rn "pub struct WorkspaceAuthorization" workflow/src/engine/workspace_ownership.rs
grep -rn "ON CONFLICT" workflow/src/persistence/checkpoint.rs
grep -rn "pub struct LaunchProvenance" workflow/src/persistence/launch_provenance.rs
grep -rn "trusted_internal" workflow/src/engine/continuation.rs
```

## Structural Verification Checklist

- [x] All cited files exist (no MISSING output).
- [x] All cited symbols exist (grep returns matches).
- [x] No new files cited as "existing" that do not exist.
- [x] The current checkpoint storage is confirmed row-replace (`ON CONFLICT`), and the loader selects newest **by timestamp** (not by attempt ID).
- [x] `RunStatus::Merged` confirmed terminal in source AND confirmed to have **no current writer** (no `mark_merged`).
- [x] `EngineRunner::resume_from_checkpoint` confirmed **private** (external paths reconstruct then `run`).
- [x] `LaunchProvenance` confirmed to carry separate `workflow_digest` + `config_digest` (dual digest, not singular).
- [x] `WorkspaceAuthorization` confirmed opaque (private dev/ino fields).
- [x] `ContinuationRequest.trusted_internal` confirmed present.

## Semantic Verification Checklist

1. Does the analysis explain HOW each of the three current recovery verbs maps
   onto `RecoveryProtocolV1`? **YES** — CLI-facing selectors build one typed
   request and delegate to the protocol.
2. Does the analysis identify the exact row-replace SQL that becomes
   append-only? **YES** — `ON CONFLICT(run_id, step_id) DO UPDATE` is the
   current replacement point.
3. Does the analysis preserve the ownership-denied terminal guard? **YES** — it
   remains an explicit non-weakened safety surface.
4. Does the analysis identify where the capsule is loaded on resume? **YES** —
   capsule-backed context reconstruction feeds `EngineRunner::run`; the private
   `resume_from_checkpoint` remains internal.

## Blocking Issues Found

None. P01A passed. Three non-blocking clarifications were incorporated into
P01: step-ID safety and step-type policy are separate dimensions; runtime
`ResumeAuthorization` remains independently required; and all
`set_resume_point` compatibility callers were enumerated.

## Failure Recovery

If this phase fails:

1. Update `01-analysis.md` to correct the cited symbols.

## Verdict

**PASS** — every formal recovery requirement maps to a verified existing or
explicitly net-new integration point, and all semantic verification questions
passed.
2. Re-run the verification commands.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P01A.md`
