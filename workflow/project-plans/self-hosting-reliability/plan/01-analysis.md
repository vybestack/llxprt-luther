# Phase 01: Domain Analysis & Integration Map

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P01`

## Prerequisites

- Required: P00A completed with PASS (all preflight checks green).

## Purpose

Establish the domain model for `RecoveryProtocolV1`, `ExecutionCapsuleV1`,
`StepRecoveryPolicy`, and the generation-fenced append-only attempt store. Map
every integration point against the actual current source so implementation
phases modify the right call sites.

## Domain Model

### Current state (what exists today)

- **Recovery verbs**: `ContinuationKind { Resume, Retry{from_failed_step}, Rewind{target} }`
  in `src/engine/continuation.rs`. Three separate planning paths
  (`prepare_continuation`, `select_checkpoint`, `select_rewind_checkpoint`) feed
  one commit (`commit_continuation`) that uses checkpoint-identity fencing inside
  an `IMMEDIATE` transaction.
- **Authorization**: `ResumeAuthorization` (trusted-internal vs operator) +
  `ContinuationRequest.trusted_internal: bool`.
- **Checkpoint storage**: `save_checkpoint_with_conn` does
  `INSERT ... ON CONFLICT(run_id, step_id) DO UPDATE` — row-replace, not
  append-only. The current loader selects the newest checkpoint by timestamp
  (`ORDER BY timestamp DESC LIMIT 1`); `set_resume_point` re-stamps an existing
  row's timestamp so the newest-by-timestamp loader picks it up. Append-only
  attempt ordering (monotonic attempt IDs) is the **future** model introduced by
  this plan (REQ-RP-003), not the current behavior.
- **Launch provenance**: `LaunchProvenance` (canonical serialization +
  `workflow_digest` + `config_digest` + config root) in
  `RunMetadata.launch_provenance` / `RunContext.launch_provenance`. The capsule
  introduces ONE envelope digest over ALL replay authority fields (including the
  `LaunchProvenance` canonical digest); component digests are metadata. [C8]
- **Ownership**: two-phase (`WorkspaceAuthorization` dev/inode, bootstrap +
  durable `.git/luther/workspace-owner`), descriptor-anchored publication.
- **Durable effect pattern**: `legacy_migration_state.rs` records intent →
  completed across a crash boundary. This is the template for a new generalized
  `effect_intents` table (not an overload of the migration-specific schema).
- **Completion**: `RunStatus::Merged` exists, is terminal (`is_terminal()` and
  `TERMINAL_SQL` include it), but has **no current writer** (no `mark_merged`
  method exists; only `mark_completed`). P17 introduces the first writer,
  gated on the typed verified merge artifact.

### Target state (what this plan introduces)

- **One typed recovery entry point**: `RecoveryProtocolV1::recover(request)`
  resolves a `RecoveryStrategy` from the step's `StepRecoveryPolicy` and
  dispatches. The three ContinuationKind execution paths are removed.
- **Immutable capsule**: `ExecutionCapsuleV1` (embeds the resolved workflow
  type + config + ONE envelope digest over ALL replay authority fields + base
  ref) persisted once, immutably. Component digests (workflow_digest,
  config_digest) are metadata. The envelope digest is the single authority,
  not a singular/reused `LaunchProvenance` digest. [C8]
- **Append-only attempts**: monotonic attempt IDs with complete `StateSnapshot`;
  latest selected by (run, attempt). Distinct durable epoch (not
  MAX(generation)) rejects stale epochs via CAS. [C1/C3]
- **Idempotent recovery**: re-issue of the same operation key (run, step,
  capsule, source_attempt) is a verified no-op returning the prior outcome
  (Completed), reconciles (Pending), or refuses (Conflict). [C2]
- **Effect intents**: durable state machine for filesystem/remote effects with
  stable key, binding, canonical payload/digest, expected target/predecessor,
  guarded finalize; reconcile before reissue. [C7]
- **Typed merge**: verified merge artifact + strategy-specific reachability
  proof enum (merge-commit: two ancestry checks; squash: ancestry + content;
  rebase: ancestry + patch) using `git merge-base --is-ancestor` + durable
  `Merged` committed in a single atomic artifact+status transaction. P17
  introduces the first writer of `RunStatus::Merged`. [C10/C11]

### Key data flows

```text
FRESH LAUNCH:
  CLI/daemon → resolve workflow type+config+provenance → build ExecutionCapsuleV1
  → persist capsule (immutable, envelope digest) → claim lease → provision ownership → EngineRunner::run()

INTERRUPT:
  signal → interrupt_at_step → append attempt row (interrupted, complete StateSnapshot) → record diagnostics

RECOVER:
  RecoveryProtocolV1::recover(request)
  → prepare (no tx): load capsule (immutable) → resolve policy (StepDef + SAFE_RERUN_STEPS)
  → reserve (short IMMEDIATE tx): epoch CAS + operation ledger reconciliation + WorkspaceAuth revalidation
  → execute (no tx): ContinueWorkspace exact-verify worktree/ownership/base/diagnostic → runner
  → finalize (short IMMEDIATE tx): append attempt + finalize operation (guarded) → Recovered

EFFECT:
  before commit/push/merge → prepare_effect → issue → reconcile-on-recovery → guarded finalize

COMPLETE:
  external verification (no tx) → typed verified merge artifact + strategy-specific proof
  → atomic artifact+status transaction → durable RunStatus::Merged
```

## Integration Map (exact files + symbols, verified by source inspection)

### Files that will be created (additive)

| File | Surface |
|------|---------|
| `src/engine/recovery/mod.rs` | `RecoveryProtocolV1`, `RecoveryRequest` (no trusted_internal), `RecoveryOutcome`, re-exports |
| `src/engine/recovery/protocol/mod.rs` | `recover()` dispatch, phased model (prepare/reserve/execute/finalize), epoch CAS, operation ledger reconciliation |
| `src/engine/recovery/protocol/prepare.rs` | prepare phase (no tx): load capsule, resolve policy |
| `src/engine/recovery/protocol/reserve.rs` | reserve phase (IMMEDIATE tx): epoch CAS + operation ledger |
| `src/engine/recovery/protocol/execute.rs` | execute phase (no tx): ContinueWorkspace exact verify |
| `src/engine/recovery/protocol/finalize.rs` | finalize phase (IMMEDIATE tx): guarded finalize |
| `src/engine/recovery/protocol/executor.rs` | injected truthful executor |
| `src/engine/recovery/policy.rs` | `StepRecoveryPolicy` enum + `policy_for_step` consuming StepDef + SAFE_RERUN_STEPS |
| `src/engine/recovery/capsule.rs` | `ExecutionCapsuleV1`, builder, ONE envelope digest over all replay authority fields |
| `src/engine/recovery/adapters/mod.rs` | object-safe `CapsuleAdapter` trait (`fn version(&self)`), version registry |
| `src/engine/recovery/adapters/v1.rs` | V1 adapter |
| `src/engine/recovery/intents.rs` | `EffectIntent` durable state machine + reconcile |
| `src/engine/recovery/salvage.rs` | Legacy salvage lineage (no V1 capsule = salvage-only) |
| `src/engine/recovery/typed_merge/mod.rs` | typed verified merge + strategy-specific proof + atomic artifact+status tx |
| `src/persistence/recovery_epoch.rs` | distinct durable epoch + CAS claim |
| `src/persistence/recovery_operations.rs` | idempotent operation ledger |
| `src/persistence/attempts.rs` | append-only attempt IDs with complete StateSnapshot |
| `src/persistence/capsule_store.rs` | immutable capsule persistence |
| `src/persistence/effect_intents.rs` | persisted effect intents state machine table |

### Files that will be modified (M3/P15 — non-destructive to live data)

| File | Change | Why |
|------|--------|-----|
| `src/persistence/checkpoint.rs` | `save_checkpoint_with_conn` → append insert with complete StateSnapshot; `load_checkpoint_with_conn` → latest-by-attempt; `set_resume_point` → epoch-fenced CAS | REQ-RP-003, REQ-RP-004 |
| `src/engine/runner.rs` | `resume_from_checkpoint()` (private today) loads capsule via adapter when surfaced; external launch/resume paths reconstruct the capsule-backed context then call `EngineRunner::run` | REQ-RP-009 |
| `src/engine/continuation.rs` | `ContinuationKind`/`ContinuationRequest` become thin CLI-facing selectors that build a `RecoveryRequest` (no `trusted_internal` bool); the three execution paths delegate to `RecoveryProtocolV1` | REQ-RP-001 |
| `src/engine/continuation/commit.rs` | epoch CAS inside `commit_continuation` (the transactional host becomes the protocol's reserve phase) | REQ-RP-004 |
| `src/engine/mod.rs` | `pub mod recovery;` + re-exports | wiring |
| `src/main.rs` / `src/cli/` | fresh launch persists capsule; resume surfaces load capsule | REQ-RP-002, REQ-RP-009 |

### Safety surfaces NOT weakened (verified unchanged in behavior)

| File | Invariant preserved |
|------|---------------------|
| `src/engine/workspace_ownership.rs` | exact dev/inode authorization; bootstrap + durable marker verification |
| `src/engine/workspace_ownership/durable_publication.rs` | `O_NOFOLLOW` descriptor-anchored publication |
| `src/persistence/leases.rs` | conditional lease transitions, lease authority anchored to issue |
| `src/engine/runner.rs` | ownership-denied terminal guard; failure cleanup provenance; per-edge loop limits |
| CI / PR-binding / review | carried through P17 |

## Resolved Decisions (locked at P00A; implemented from P03 onward)

- **`StepRecoveryPolicy` default for untyped/legacy step types**: **NonRecoverable**
  (fail closed). Implemented in `policy_for_step_type`'s catch-all arm.
- **Effect-intents table**: **new generalized `effect_intents` table** (not a
  reuse of the migration-specific `legacy_migration_state.rs` schema), avoiding
  overloading migration-specific semantics.

### Policy and caller interactions verified in P01A

- `StepRecoveryPolicy` and authorization remain orthogonal: policy selects the
  executor's recovery semantics, while `ResumeAuthorization` proves the exact
  runtime wait/checkpoint/workspace authority. The protocol requires both; a
  permissive result from either dimension cannot override refusal by the other.
- The existing `SAFE_RERUN_STEPS` is step-ID based. Migration preserves those
  exact classifications while translating them into typed policies; it does
  not infer safety solely from a generic executor `step_type`.
- `set_resume_point` is also called by `src/persistence/wait_state.rs` and
  `src/daemon/poller/apply.rs`. Its compatibility shim retains the current
  signature while routing new V1 runs to epoch-fenced append-only state;
  legacy checkpoint rows remain available for salvage reads.
- **Capsule digest**: **ONE envelope digest** over ALL replay authority fields
  (run_id, config root encoding, resolved bytes, LaunchProvenance canonical
  digest, base ref). Component digests (workflow_digest, config_digest) are
  metadata, NOT authority. [C8]
- **Typed merge reachability proof**: uses **`git merge-base --is-ancestor`**
  and records the **exact tested ancestor/descendant** according to merge
  strategy (merge-commit: head+base → merge commit; squash: base → squash commit
  + content digest; rebase: base → final head + content/patch equivalence). This
  avoids the direction error of assuming the merged SHA is an ancestor of the
  final head.
- **SQLite idioms**: use **`RETURNING`** (already used in `leases.rs` under
  rusqlite 0.34) for single-statement read-back where applicable.

## Verification Gate

- [x] Integration map cites only files/symbols confirmed in P00A.
- [x] Every REQ-RP-* requirement maps to at least one integration point.
- [x] Safety surfaces are listed as preserved, not weakened.
- [x] Resolved decisions are locked (no scope expansion).
