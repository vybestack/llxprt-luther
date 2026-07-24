# Phase P08B — Atomic Fresh-Launch Capsule Persistence (M2 Closure)

Phase: `PLAN-20260723-SELFHOST-RELIABILITY.P08B`
Milestone: M2 closure

## Problem

P08A left M2 unchecked: `persist_capsule_v1` has no production fresh-launch
caller. The capsule store can insert an immutable capsule, but no production
launch surface atomically inserts **both** the initial `Starting`
`RunMetadata` row and the immutable `ExecutionCapsuleV1` in a single SQLite
transaction. This leaves two orphaned-state windows:

1. A run row is inserted, then the process crashes before the capsule is
   persisted: the run exists without its immutable launch authority.
2. A capsule is inserted, then the run insert fails: an immutable capsule
   exists for a run that is not in the registry.

## Goal

Add a single, cohesive transaction API that inserts the initial `Starting`
`RunMetadata` and the immutable `ExecutionCapsuleV1` in **one** SQLite
`IMMEDIATE` transaction. Wire every production fresh-launch caller (CLI
`run`, daemon launch, parent-orchestration child launch) to build the V1
capsule from the exact resolved post-override workflow/config/config-root/
provenance/base-ref and pass it into that one API.

### Requirements (numbered pseudocode)

1. `persist_launch_atomically(conn, metadata, capsule)`:
   - `BEGIN IMMEDIATE`
   - `INSERT OR FAIL` the `Starting` `RunMetadata` (run collision → rollback)
   - `persist_capsule_v1` (envelope digest verified; capsule collision → rollback)
   - `COMMIT`
   - Any failure → `ROLLBACK`; neither row is durable.

2. The capsule is built **before** persistence from the exact resolved
   post-override `WorkflowType`, `WorkflowConfig`, canonical config root,
   `LaunchProvenance`, and resolved `base_ref`. No historical/backfill path.

3. `EngineRunner::with_db_path_for_launch` accepts a capsule input and uses
   the atomic launch persistence API. No separate `persist_initial_run` /
   `persist_capsule_v1` calls on the fresh-launch path.

4. Resume constructors (`with_db_path_and_context`) are **unchanged**: they
   re-verify provenance and load the existing capsule, never insert one.

5. No ambient transaction commit; no compensating cleanup; no separate
   persistence calls. The transaction owns the atomic pair.

6. Failures (run collision, capsule collision, constraint/envelope failure)
   cause full rollback and leave neither a run nor a capsule.

## Tests (P08B)

- `successful_atomic_pair_inserts_run_and_capsule`
- `run_collision_rolls_back_capsule`
- `capsule_collision_rolls_back_run`
- `injected_capsule_failure_rolls_back_run`
- `constraint_failure_rolls_back_run` (envelope-digest verification failure)
- Fresh-launch caller tests: CLI, daemon, child each build a capsule and
  persist atomically; collision leaves no run and no capsule.

## Verification (P08B / P08C)

- Focused tests green.
- Full locked library/integration suite green.
- Strict workspace/all-target/all-feature Clippy (`-D warnings`).
- `cargo fmt --check`.
- Changed-file complexity/source-size within lizard limits.
- `git diff --check`.

## Completion

M2 is checked when the production fresh-launch wiring and all gates pass.
