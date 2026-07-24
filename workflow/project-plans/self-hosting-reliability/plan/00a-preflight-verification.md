# Phase 00A: Preflight Verification

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P00A`

## Purpose

Verify ALL assumptions against the actual current source before writing any
code. Every type, function, and call path cited in this plan must be confirmed
to exist where the plan says it does.

## Dependency Verification

| Dependency | Verification Command | Status |
|------------|----------------------|--------|
| rusqlite 0.34 (bundled) | `grep '^rusqlite' Cargo.toml` | VERIFIED — `rusqlite = { version = "0.34", features = ["bundled", "chrono", "uuid"] }` |
| serde / serde_json | `grep '^serde' Cargo.toml` | VERIFIED — `serde = { version = "1", features = ["derive"] }`, `serde_json = "1"` |
| sha2 | `grep '^sha2' Cargo.toml` | VERIFIED — `sha2 = "0.10"` |
| uuid (v4, serde) | `grep '^uuid' Cargo.toml` | VERIFIED — `uuid = { version = "1.0", features = ["v4", "serde"] }` |
| chrono (serde) | `grep '^chrono' Cargo.toml` | VERIFIED — `chrono = { version = "0.4", features = ["serde"] }` |
| tempfile, rstest (dev) | `grep -E 'tempfile\|rstest' Cargo.toml` | VERIFIED — `rstest = "0.25"`, `tempfile = "3"` |

No new crate dependencies are introduced by this plan. SQLite `RETURNING` is
already used in the codebase (`src/persistence/leases.rs`) and is supported by
the rusqlite 0.34 bundled SQLite.

## Type/Interface Verification (against actual source)

| Type Name | Plan-Assumed Location | Expected Shape | Status |
|-----------|----------------------|----------------|--------|
| `EngineRunner` | `src/engine/runner.rs` | struct with `run()` (pub), `execute_step()` (pub); `resume_from_checkpoint()` exists but is **private** | VERIFIED — `run`/`execute_step` public; `resume_from_checkpoint` is private (no `pub`) |
| `StepOutcome` | `src/engine/transition.rs` | enum: Success, Retryable, Fatal, Fixable, Abandon, Wait | VERIFIED — all six variants present |
| `Checkpoint` | `src/persistence/checkpoint.rs` | `{ run_id, step_id, state_snapshot: StateSnapshot, timestamp }` | VERIFIED |
| `StateSnapshot` | `src/persistence/checkpoint.rs` | `{ retry_count, loop_count, edge_loop_counts, context, status }` | VERIFIED |
| `RunStatus` | `src/persistence/run_metadata.rs` | enum incl. `Merged` (terminal); **no writer exists** | VERIFIED — `Merged` present, `is_terminal()` true, in `TERMINAL_SQL`; no `mark_merged` method (only `mark_completed`) |
| `RunMetadata` | `src/persistence/run_metadata.rs` | run row with `failure_cleanup`, status, `launch_provenance: Option<LaunchProvenance>` | VERIFIED |
| `LaunchProvenance` | `src/persistence/launch_provenance.rs` | canonical serialization + `workflow_digest` + `config_digest` + config root | VERIFIED — **dual digest** (`workflow_digest: String`, `config_digest: String`), not singular |
| `WorkspaceAuthorization` | `src/engine/workspace_ownership.rs` | opaque dev/inode, constructed via capture only | VERIFIED |
| `ContinuationKind` | `src/engine/continuation.rs` | enum: Resume, Retry{from_failed_step}, Rewind{target} | VERIFIED |
| `ContinuationRequest` | `src/engine/continuation.rs` | `{ run_id, kind, force, trusted_internal }` | VERIFIED |
| `ResumeAuthorization` | `src/engine/continuation/authorization.rs` | trusted-internal vs operator authority | VERIFIED — 4 variants: Operator, OperatorCurrentWait, OperatorCommittedCheckpoint, TrustedInternalWait |
| `LeaseStatus` | `src/persistence/leases.rs` | enum: Pending, Claimed, Running, … | VERIFIED |
| `MigrationStatus` | `src/persistence/legacy_migration_state.rs` | enum: Pending, Completed (durable state machine) | VERIFIED — Pending, Completed |

## Call Path Verification

| Function | Expected Caller / Location | Evidence Required | Status |
|----------|---------------------------|-------------------|--------|
| `save_checkpoint_with_conn` | `src/engine/runner.rs::interrupt_at_step` + `handle_interrupt` | `ON CONFLICT DO UPDATE` in checkpoint.rs | VERIFIED — `INSERT ... ON CONFLICT(run_id, step_id) DO UPDATE` (row-replace, NOT append-only) |
| `load_checkpoint_with_conn` | `src/engine/runner.rs::resume_from_checkpoint` | call present; selects newest by timestamp | VERIFIED — `ORDER BY timestamp DESC LIMIT 1` (newest-by-timestamp; append-only attempt ordering is the future REQ-RP-003) |
| `commit_continuation` | CLI runs resume/retry/rewind via main.rs | grep in src/cli, src/main.rs | VERIFIED — `TransactionBehavior::Immediate` in commit.rs |
| `verify_workspace_ownership` | `src/engine/runner.rs` (failure cleanup guard) | grep in runner + workspace_ownership.rs | VERIFIED |
| `set_resume_point` | `src/engine/continuation/commit.rs` | `CHECKPOINT_STATUS_READY_TO_RESUME` | VERIFIED — re-stamps an existing row's timestamp + status so the newest-by-timestamp loader selects it (history preserved) |

## Test Infrastructure Verification

| Component | Verification | Status |
|-----------|--------------|--------|
| Integration test pattern | `ls tests/*.rs` and `cargo test --list` | VERIFIED (54 integration test files) |
| Existing tests pass | `cargo test --locked` exits 0 | VERIFIED — detached full suite completed with status 0; library suite: 1,346 passed; every integration and documentation test summary passed; 8 tests ignored by their suites |
| Lint policy | `CLIPPY_CONF_DIR=.github/clippy cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | VERIFIED — status 0; denied complexity and source-structure lints remain enforced |

## Blocking Issues Found

Source inspection completed. The following **blocking corrections** were
identified and **resolved** in the plan documents (no production source was
modified):

1. **Checkpoint behavior correction**: the current loader selects newest **by
   timestamp**, not by attempt ID. Append-only attempt ordering is the future
   REQ-RP-003. Corrected in `specification.md`, `01-analysis.md`,
   `00-overview.md`.
2. **`RunStatus::Merged` has no writer**: the variant is terminal but no code
   path sets it. P17 introduces the first writer. Corrected in `01-analysis.md`,
   `17-typed-verified-merge.md`, `execution-tracker.md`.
3. **`EngineRunner::resume_from_checkpoint` is private**: external launch/resume
   surfaces reconstruct the run context then call `EngineRunner::run`, rather
   than calling the private resume method directly. Corrected in
   `01-analysis.md`, `12-capsule-adapter-wiring-stub.md`,
   `14-capsule-adapter-wiring-impl.md`.
4. **Capsule digest is capsule-scoped, not singular**: `LaunchProvenance`
   carries separate `workflow_digest` + `config_digest`. The capsule uses a
   capsule-scoped `CapsuleDigest { workflow_digest, config_digest }`, not a
   singular/reused provenance digest. Corrected in `02-pseudocode.md`,
   `06-execution-capsule-stub.md`, `08-execution-capsule-impl.md`.
5. **Merge reachability proof must use correct ancestry per strategy**:
   `git merge-base --is-ancestor` with the exact recorded ancestor/descendant,
   not an assumption that the merged SHA is an ancestor of the final head.
   Corrected in `02-pseudocode.md`, `17-typed-verified-merge.md`,
   `17a-typed-verified-merge-verification.md`.

All blocking corrections were resolved at the documentation level. The runtime
verification gates also passed, so P00A is complete.

## Decision Log

1. Untyped and legacy step types default to `NonRecoverable` and fail closed.
2. Generalized effects use a new `effect_intents` table; migration-specific
   state remains single-purpose.
3. Capsules use their own canonical digest while embedding provenance's
   `workflow_digest` and `config_digest`.
4. Merge reachability uses a typed, strategy-aware
   `git merge-base --is-ancestor` proof.
5. Durable append-only attempts and generation fencing precede protocol
   implementation; no temporary in-memory facade is permitted.
6. SQLite inserts use `RETURNING`, already supported and used by this project.

## Verification Result

P00A **passes**. Dependencies, types, interfaces, and call paths were verified
against source. The detached full `cargo test --locked` run exited 0, strict
all-target/all-feature Clippy exited 0, formatting passed, and `git diff
--check` passed. P01 may proceed.

## Verification Gate

- [x] All dependencies verified (no new crates needed).
- [x] All types match the plan assumptions (with documented corrections).
- [x] All call paths exist.
- [x] Test infrastructure ready.
- [x] Full `cargo test --locked` passes (status 0; library: 1,346 passed).
- [x] Strict project Clippy passes with `-D warnings`.
- [x] Formatting and `git diff --check` pass.
