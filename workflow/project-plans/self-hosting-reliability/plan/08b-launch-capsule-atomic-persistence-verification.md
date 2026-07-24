# Phase P08B Verification Results — Atomic Fresh-Launch Capsule Persistence

Phase verified: `PLAN-20260723-SELFHOST-RELIABILITY.P08B`
Completed: 2026-07-23
Verdict: PASS — M2 closed

## What was implemented

- **`persist_launch_atomically`** (`persistence/capsule_store.rs`): a single,
  cohesive transaction API that inserts both the initial `Starting`
  `RunMetadata` (via `INSERT OR FAIL`) and the immutable `ExecutionCapsuleV1`
  (via `persist_capsule_v1`) in one SQLite `IMMEDIATE` transaction. Any failure
  (run collision, capsule collision, constraint violation, envelope-digest
  verification failure, or generic DB error) causes full rollback: neither row
  is durable.
- **`LaunchPersistenceOutcome`** and **`LaunchPersistenceError`**: typed
  outcome/error enums distinguishing `RunCollision`, `CapsuleCollision`, and
  `Database` failures.
- **`EngineRunner::with_db_path_for_launch`** signature change: now accepts an
  `ExecutionCapsuleV1` and calls `persist_launch_with_capsule` instead of the
  old `persist_initial_run_for_launch`.
- **All production fresh-launch callers updated**:
  - CLI (`create_durable_runner_with_provenance` in `app/run.rs`): builds the
    capsule from the resolved workflow/config/config-root/provenance/base-ref
    and passes it.
  - Daemon (`launch_daemon_workflow` in `app/daemon_run.rs`): passes
    `config_root` to `create_durable_runner_with_provenance`.
  - Child (`launch_child_workflow` in
    `engine/executors/parent_orchestration/child_workflow.rs`): builds the
    capsule from the resolved workflow/config/config-root/provenance/base-ref.
- **`open_initialized_connection`** (`engine/runner/support.rs`): initializes
  the capsule table so the runner's own connection can atomically persist the
  capsule at fresh launch.

## Requirements satisfied

1. ✅ Capsule exists before any workflow execution/effects (built and persisted
   in the constructor, before `run()`).
2. ✅ Duplicate run ID or capsule causes full rollback (`RunCollision` /
   `CapsuleCollision` variants).
3. ✅ Capsule insert failure leaves no run (transaction rollback).
4. ✅ Run insert failure leaves no capsule (transaction rollback).
5. ✅ No historical/backfill path (capsule is always freshly built).
6. ✅ Resume constructors unchanged (`with_db_path_and_context`).
7. ✅ No ambient transaction commit (single explicit `tx.commit()`).
8. ✅ No separate persistence calls (one API call, one transaction).

## Tests

| Test | Verifies |
|------|----------|
| `successful_atomic_pair_inserts_run_and_capsule` | Both rows persisted and queryable. |
| `run_collision_rolls_back_capsule` | Pre-existing run → `RunCollision`, no capsule left. |
| `capsule_collision_rolls_back_run` | Pre-existing capsule → `CapsuleCollision`, no run left. |
| `injected_capsule_failure_rolls_back_run` | Tampered envelope digest → failure, no run or capsule left. |
| `constraint_capsule_failure_rolls_back_run` | Run inserted then capsule PK constraint fires → `CapsuleCollision`, run rolled back. |
| `fresh_launch_caller_persists_run_and_capsule_atomically` | `with_db_path_for_launch` persists both rows. |
| `fresh_launch_caller_collision_leaves_no_capsule` | `with_db_path_for_launch` collision leaves no capsule. |

All 7 tests pass with real SQLite.

## Full verification gates

- **Focused library tests** (daemon launcher, capsule store, sqlite,
  construction, runner tests): 72 passed.
- **Full library suite**: 1,353 passed; 0 failed.
- **Integration suite** (capsule, epoch/operations/attempts, effect intents,
  continuation, persistence, engine execution, engine resume, daemon
  scheduler, run registry, hello world, llxprt preflight, token validation,
  per-edge loop): all passed.
- **Strict Clippy** (`--all-targets --all-features -- -D warnings`): passed.
- **`cargo fmt --check`**: passed.
- **Changed-file complexity/source-size**: all changed files within lizard
  thresholds (CCN ≤ 25, function ≤ 80 lines, file ≤ 1000 lines).
- **`git diff --check`**: no whitespace errors.

## M2 status

**Milestone 2 (P06–P08B) is complete.** The `ExecutionCapsuleV1` with one
envelope digest over all replay authority fields [C8] is persisted immutably at
fresh launch via `persist_launch_atomically`. Adapter is object-safe
(`fn version(&self)`) [C8]. Duplicate run ID or capsule causes full rollback.
