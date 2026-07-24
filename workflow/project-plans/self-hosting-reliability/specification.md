# Feature Specification: Self-Hosting Reliability (Recovery Protocol V1)

Plan ID: `PLAN-20260723-SELFHOST-RELIABILITY`
Generated: 2026-07-23

## Purpose

Make Luther self-hosting viable by replacing the separate resume / retry / rewind /
legacy-migration execution paths with a single typed recovery abstraction
(`RecoveryProtocolV1`) backed by immutable, append-only execution capsules
(`ExecutionCapsuleV1`). The goal is a deterministic, interrupt-safe, idempotent
recovery model where a workflow run can be interrupted mid-step and resumed from
exactly the verified workspace state, then carried through to a typed verified
merge — with no direct SQL, no manual git/GitHub mutation, and no duplicate side
effects.

This plan deliberately defers distributed persistence, async engine redesign,
arbitrary legacy exact recovery, and the broader llxprt roadmap (see
"Explicitly Deferred").

## Scope Boundary

Viability is gated, not open-ended. The plan ends when three consecutive mixed
canaries complete with zero invariant violations (see Phase 19). It does **not**
attempt to make every historical run exactly recoverable.

## Architectural Decisions

> The following decisions were corrected during P02 remediation cycles to
> address 13 blocking design corrections (cycle 1, `[C##]`) and 13 refinements
> (cycle 2, `[B##]`). Each is marked inline. See `plan/02-pseudocode.md` for the
> locked numbered pseudocode — the SOLE pseudocode source.

- **Single typed recovery abstraction [C5/C12]**: `RecoveryProtocolV1` replaces
  the separate `ContinuationKind { Resume, Retry, Rewind }` execution paths
  (`src/engine/continuation.rs`) and the recoverable legacy ownership migration
  execution path (`src/engine/continuation/legacy_ownership_migration.rs`). The
  protocol owns all four execution phases — prepare, reserve, execute, finalize
  — and SHALL NOT return `Recovered` before the runner outcome is finalized and
  the finalize transaction commits.
- **Distinct durable recovery epoch [C1]**: recovery fencing uses a distinct
  durable per-run epoch (a dedicated `recovery_epoch` row), NOT
  `MAX(attempt generation)`. The epoch is advanced via a CAS
  (`compare-and-swap`) claim inside a short `IMMEDIATE` transaction, with an
  affected-row check to detect concurrent advances. No synthetic attempt rows
  are appended to bump the epoch.
- **Recovery operations ledger [C2]**: every recovery operation is recorded as
  a durable row in a `recovery_operations` ledger with a stable `operation_id`
  (the idempotency key) derived from exact (run_id, step_id,
  capsule_envelope_digest, source_attempt_id) bindings. Operations transition
  Pending → Completed | Refused | Conflict with a serialized prior outcome. An
  exact completed duplicate returns the prior outcome; a pending duplicate
  reconciles; a conflicting duplicate refuses.
- **Immutable canonical capsule with envelope digest [C8]**: fresh runs persist
  an immutable `ExecutionCapsuleV1` with explicit canonicalization, schema, and
  domain versions and ONE envelope digest computed over ALL replay authority
  fields (run_id, config root encoding, resolved workflow/config bytes, actual
  `LaunchProvenance` canonical digest, base ref). Component digests
  (`workflow_digest`, `config_digest`) are metadata, not authority.
- **Append-only attempts with complete state [C3]**: attempt records are
  immutable, append-only rows carrying the complete `StateSnapshot`, immutable
  attempt/source-parent IDs, step ID/status, capsule schema + envelope digest,
  and snapshot/checkpoint digests. Today the loader selects the newest
  checkpoint **by timestamp** (`ORDER BY timestamp DESC LIMIT 1`); the
  append-only attempt ordering replaces this.
- **Sealed RecoveryAuthority, no trusted_internal bool [C4]**: the public
  `RecoveryRequest` has NO `trusted_internal` bool. A sealed
  `RecoveryAuthority` / `PreparedRecovery` is derived from exact durable state
  and a descriptor-bound `WorkspaceAuthorization`, revalidated inside the CAS
  transaction (TOCTOU guard).
- **Step recovery policy from canonical StepDef [C6]**: `policy_for_step`
  consumes the canonical `StepDef` / explicit declaration plus the exact current
  `SAFE_RERUN_STEPS` classifications (by step_id). Generic `shell`/`write_file`
  step types default to `NonRecoverable` unless a specific canonical step
  explicitly opts into `ContinueWorkspace` via a declared `recovery_policy`.
- **Interrupted-implement path**: an interrupted run resumes via
  `ContinueWorkspace` after exact verification of worktree path, ownership
  (bootstrap + durable `.git/luther/workspace-owner`), base ref, and diagnostic
  state. Authorization is derived from the descriptor-bound
  `WorkspaceAuthorization` (`src/engine/workspace_ownership.rs`), not from a
  `trusted_internal` flag.
- **Legacy evidence is salvage, never exact continuation [C9]**: every run
  WITHOUT a valid pre-execution V1 capsule is salvage-only, regardless of
  provenance or migration source. This includes runs with migrated
  `LaunchProvenance` (sentinel digests) but no V1 capsule. Exact recovery
  refuses; the run record is preserved as immutable salvage lineage.
- **Complete effect-intent state machine [C7]**: filesystem and remote effects
  (git commit/push, GitHub PR/merge) use a durable `effect_intents` state
  machine with a stable unique effect key, operation/attempt/sequence binding,
  canonical payload + digest/version, expected target/predecessor, observed
  result, and Prepared/Completed/Conflict states. Intents are committed before
  the effect; each effect kind has its own external reconciliation logic; the
  finalize is guarded (only transitions from Prepared). This generalizes the
  existing durable migration state machine in
  `src/persistence/legacy_migration_state.rs`.
- **Versioned capsule execution via object-safe adapters [C8]**: the current
  binary loads a versioned `ExecutionCapsuleV1` and executes it through a
  versioned `CapsuleAdapter` that is object-safe (`fn version(&self) -> u32`
  instead of `const VERSION`), enabling `dyn CapsuleAdapter` dispatch.
- **Typed verified merge with strategy-specific proof [C10/C11]**: completion
  requires a `TypedMergeArtifact` (observed merge + strategy-specific
  reachability proof) AND the durable `RunStatus::Merged` state, committed in a
  single short `IMMEDIATE` atomic artifact+status transaction. A status field
  alone never satisfies completion. The merge proof is a strategy-specific enum:
  merge-commit has two ancestry checks; squash and rebase include ancestry plus
  computed expected/observed content or patch evidence. `result_sha` is
  strategy-neutral. The transaction has an explicit allowed predecessor and an
  affected-row check; the normal merge-required flow must NOT first write

### Remediation Cycle 2 Refinements [B1–B13]

- **Exact authority preservation in PreparedRecovery [B1]**: `PreparedRecovery`
  captures the exact run/status/current-step/live-PID/checkpoint/wait/lease
  authority during prepare and reselects/revalidates each inside the reserve
  transaction before any mutation. A change in any dimension aborts reserve.
- **Single caller expected_epoch, single reserve CAS [B2]**: `RecoveryRequest`
  carries the caller's `expected_epoch`. The ONLY compare-and-swap in the
  protocol is the epoch CAS inside the reserve transaction. There is no CAS at
  finalize.
- **Logical request key vs. exact operation ID [B3]**: the normalized logical
  request key (run_id + step_id + capsule lineage) is separated from the exact
  `operation_id`. A `Pending` operation carries a guarded owner-PID/lease claim
  so exactly one process executes or reconciles; an expired lease allows
  adoption.
- **Durable execution_attempt_id at reserve [B4]**: a durable
  `execution_attempt_id` is allocated at reserve before any effect, recorded in
  an attempt-start row. The immutable outcome snapshot is appended at finalize;
  a durable runner-result field makes the outcome recoverable after an
  execute-before-finalize crash.
- **Insert-or-load exact-binding effect prepare [B5]**: effect prepare is an
  insert-or-load with exact-binding (payload digest + expected target/
  predecessor) comparison. A mismatch transitions the intent to conflict. Merge
  intents are completed/conflicted inside the atomic merge transaction.
- **Retained VerifiedWorkspace anchor [B6]**: `PreparedRecovery` owns a real
  retained `VerifiedWorkspace` (OwnedFd-compatible) anchor obtained via the
  existing `adjudicate_workspace_ownership` kernel. Reserve does a
  descriptor-relative durable marker re-snapshot plus exact descriptor identity
  comparison (TOCTOU guard).
- **StepDef.recovery_policy in capsule milestone [B7]**: the `recovery_policy`
  field is added to `StepDef` (schema + canonicalizer + validation) in the
  capsule milestone (P06–P08) before the protocol. The policy is persisted in
  the canonical workflow bytes so the capsule envelope digest covers it.
- **Actual launch/resume surface mapping [B8]**: capsule persistence is wired
  into the actual launch/resume surfaces — `app/run.rs`, `app/daemon_run.rs`,
  `parent_orchestration/child_workflow.rs`/`child_run.rs`,
  `app/runs/continuation_execution.rs`. A capsule is written by fresh launch
  before the run row is mutated, in a capsule-before-run-row atomic ordering.
- **Framed canonical envelope byte format [B9]**: the envelope digest is
  computed over a framed canonical envelope byte format with fixed supported
  schema/canonicalization/domain/provenance versions and length-prefixed
  authority fields. Adapter dispatch is fail-closed for unsupported versions.
- **No historical capsule backfill [B10]**: historical capsule backfill is
  prohibited. Every run without a capsule written by fresh launch before
  execution is salvage-only forever.
- **Authoritative injected merge verifier [B11]**: the merge verifier takes
  authoritative injected Git/remote interfaces (`MergeGitProbe`,
  `MergeRemoteProbe`) and bound repo/PR/base/head; it computes all evidence
  itself. No ambient shell.
- **Merge artifact DDL + fixed predecessor [B12]**: merge artifact DDL/init,
  exact artifact equality, capsule lookup/join, fixed allowed predecessor
  `ReviewReady`, and modified runner completion semantics (merge-required
  workflows reach `ReviewReady`, not `Completed`; `Merged` only via
  `complete_typed_merge`).
- **Sole pseudocode source [B13]**: `plan/02-pseudocode.md` is the sole
  pseudocode source. Implementation phase docs reference exact component-local
  line numbers only; no phase doc carries alternate conflicting pseudocode.

  Completed.

## Project Structure (new code surfaces)

```text
src/engine/recovery/
  mod.rs                    # RecoveryProtocolV1, RecoveryRequest, RecoveryOutcome
  protocol.rs               # phased dispatch: prepare → reserve → execute → finalize
  policy.rs                 # StepRecoveryPolicy enum + policy_for_step(StepDef)
  capsule.rs                # ExecutionCapsuleV1 + builder + envelope digest
  adapters/
    mod.rs                  # CapsuleAdapter trait (object-safe), version registry
    v1.rs                   # ExecutionCapsuleV1 adapter
  intents.rs                # EffectIntent state machine (filesystem/remote)
  salvage.rs                # Legacy salvage lineage (no exact continuation)
  typed_merge.rs            # TypedMergeArtifact + strategy-specific proof
src/persistence/
  attempts.rs               # append-only attempts (complete StateSnapshot)
  capsule_store.rs          # immutable canonical capsule persistence
  effect_intents.rs         # persisted effect intents state machine table
  recovery_epoch.rs         # distinct durable per-run epoch (CAS-fenced) [C1]
  recovery_operations.rs    # idempotent operation ledger [C2]
```

These are additive surfaces; existing modules are modified only in the
migration/deprecation phase (Phase 15).

## Technical Environment

- **Type**: Extension to the existing workflow engine (`luther-workflow` crate).
- **Runtime**: Synchronous engine core (`EngineRunner`), tokio app around it.
- **Persistence**: SQLite (`rusqlite` 0.34 bundled), already a dependency.
- **No new crate dependencies.** Reuses `serde`, `serde_json`, `sha2`, `uuid`,
  `chrono`, `rusqlite`.

## Integration Points (MANDATORY — verified by source inspection)

### Existing code that will be replaced/unified by `RecoveryProtocolV1`

- `src/engine/continuation.rs` — `ContinuationKind`, `ContinuationRequest`,
  `prepare_continuation`, `ContinuationPlan`. The three variants (Resume /
  Retry / Rewind) collapse into one `RecoveryProtocolV1::recover(request)` with
  a `RecoveryStrategy` resolved from the step's `StepRecoveryPolicy`. The
  `trusted_internal: bool` field on `ContinuationRequest` is REMOVED [C4];
  authorization is derived from the descriptor-bound `WorkspaceAuthorization`.
- `src/engine/continuation/commit.rs` — `commit_continuation(conn, request,
  bound_identity)`. Its `TransactionBehavior::Immediate` + checkpoint-identity
  binding becomes the epoch CAS + operation reserve inside the protocol's
  reserve phase [C1/C2].
- `src/engine/continuation/selection.rs` — `select_checkpoint`,
  `select_rewind_checkpoint`. Selection moves under `RecoveryProtocolV1`.
- `src/engine/continuation/authorization.rs` — `ResumeAuthorization`,
  `checkpoint_is_authorized`. Authorization becomes descriptor-bound
  (`WorkspaceAuthorization` from `workspace_ownership`), not a `trusted_internal`
  bool [C4].
- `src/engine/continuation/resume_authorization.rs` —
  `prepare_resume_authorization`, `PreparedResume`, `ResumeAuthorizationError`.
  The `trusted_internal: bool` flag is replaced by the sealed
  `RecoveryAuthority` carrying a descriptor-bound `WorkspaceAuthorization` [C4].

### Existing code that will execute versioned capsules via adapters

- `src/engine/runner.rs` — `EngineRunner::run()` (public) and
  `resume_from_checkpoint()` (**private** today). External launch/resume surfaces
  reconstruct the run context then call `EngineRunner::run`; the private resume
  method, when surfaced, loads the persisted `ExecutionCapsuleV1` and dispatches
  through the adapter instead of reconstructing the instance ad hoc.
- `src/engine/runner.rs` — `RunContext.launch_provenance` (already a
  `LaunchProvenance` field). The capsule embeds and supersedes the ad-hoc
  provenance field as the single launch authority.
- **[B8]** Actual launch/resume surfaces that construct and persist
  `ExecutionCapsuleV1` at fresh launch (capsule-before-run-row atomic ordering)
  and load it on resume:
  - `src/app/run.rs` — `handle_run_command` →
    `create_durable_runner_with_provenance` → `EngineRunner::with_db_path_for_launch`
    (inserts the initial `Starting` row). Capsule is built + persisted BEFORE
    `runner.run()`.
  - `src/app/daemon_run.rs` — daemon launch path: records `LaunchProvenance`
    via `LaunchProvenance::from_resolved`, then
    `create_durable_runner_with_provenance`. Capsule is built + persisted
    BEFORE `run_daemon_runner`.
  - `src/engine/executors/parent_orchestration/child_workflow.rs` /
    `child_run.rs` — `launch_child_workflow` (fresh) and
    `resume_child_workflow` (resume via `prepare_child_resume_readonly`).
    Capsule is built + persisted before the child runner executes; resume
    loads the capsule.
  - `src/app/runs/continuation_execution.rs` — `reconstruct_runner` /
    `reconstruct_runner_with_config`: resume path. Loads the persisted capsule
    instead of reconstructing the instance ad hoc.
- `src/main.rs` / `src/cli/` — CLI verbs dispatch to the above app surfaces.

### Existing code that becomes append-only / epoch-fenced

- `src/persistence/checkpoint.rs` — `save_checkpoint_with_conn`
  (`INSERT ... ON CONFLICT DO UPDATE`) becomes append-only insert keyed by
  attempt ID. Today `load_checkpoint_with_conn` selects the newest checkpoint
  **by timestamp** (`ORDER BY timestamp DESC LIMIT 1`); `set_resume_point`
  re-stamps an existing row's timestamp so the newest-by-timestamp loader picks
  it up. The append-only attempt ordering (REQ-RP-003) replaces this. Attempt
  rows carry the complete `StateSnapshot` and capsule binding [C3].
- `src/persistence/run_metadata.rs` — `RunStatus::Merged` (terminal) is the
  durable completion state reached only through typed verified merge. It exists
  and is terminal today, but has **no current writer**; P17 introduces the first,
  in an atomic artifact+status transaction with explicit predecessor [C11/B12].
  **[B12]** A new non-terminal `RunStatus::ReviewReady` variant is added: a
  merge-required workflow reaches `ReviewReady` (not `Completed`) after all steps
  complete; `Merged` is reached only via `complete_typed_merge`. The fixed
  allowed predecessor for `ReviewReady → Merged` is enforced in the transaction.
- `src/persistence/launch_provenance.rs` — `LaunchProvenance` carries separate
  `workflow_digest` and `config_digest`. The capsule has ONE envelope digest over
  all replay authority fields (including the `LaunchProvenance` canonical digest);
  the component digests are metadata [C8].
- `src/persistence/legacy_migration_state.rs` — durable intent→complete state
  machine pattern is generalized into a new `effect_intents` table with a
  complete state machine (stable key, binding, reconcile, guarded finalize) [C7].

### Existing safety surfaces that MUST NOT be weakened

- `src/engine/workspace_ownership.rs` — `WorkspaceAuthorization`,
  `verify_workspace_ownership`, bootstrap + durable marker verification.
  **[B6]** The protocol consumes the consolidated ownership kernel
  `adjudicate_workspace_ownership(workspace: &Path, run_id: &str) ->
  OwnershipVerdict`, whose `Owned(VerifiedWorkspace)` variant yields a retained
  anchor (OwnedFd-compatible) and `VerifiedWorkspace::authorization() ->
  WorkspaceAuthorization`. `WorkspaceAuthorization { dev, ino }` is opaque and
  constructible only inside the module.
- `src/engine/workspace_ownership/durable_publication.rs` — descriptor-anchored,
  `O_NOFOLLOW` publication.
- `src/persistence/leases.rs` — `LeaseStatus`, conditional lease transitions.
- `src/engine/runner.rs` — ownership-denied terminal guard, failure cleanup
  provenance, per-edge loop limits.
- CI, PR-binding, and review safety (carried through Phase 17).

### User access points

- `luther run` (fresh launch) — persists `ExecutionCapsuleV1`.
- `luther-workflow runs resume|retry|rewind` — unified under
  `RecoveryProtocolV1` (CLI verbs remain as operator-facing names that select a
  `RecoveryStrategy`).

### Migration requirements

- Existing `checkpoints` rows are preserved (salvage lineage); new runs use the
  append-only attempt table. No in-place destructive migration of live data.
- `LaunchProvenance`-bearing rows are upgraded to capsule-backed; runs without a
  valid pre-execution V1 capsule (including migrated-provenance rows with
  sentinel digests) remain salvage-only [C9].

## Atomicity Boundary (corrected)

- The protocol's IMMEDIATE transactions cover **database rows only**. They do
  NOT provide atomicity over external systems (git, GitHub). External
  verification (reachability, observed merge state) happens BEFORE the
  transaction; the transaction commits only the DB-side artifact + status. The
  gap between external observation and DB commit is bridged by the
  effect-intent state machine and idempotent retry, NOT by assumed cross-system
  atomicity [C5/C11].
- The epoch CAS uses an affected-row check to detect concurrent DB-level races.
  It does NOT prevent two processes from observing the same external state;
  that is handled by the operation ledger's idempotent reconciliation.

## Formal Requirements

### REQ-RP-001: Single typed recovery abstraction
The system SHALL provide exactly one typed recovery entry point,
`RecoveryProtocolV1::recover(request)`, that handles all resume/retry/rewind/
migration execution. Separate execution paths for these verbs SHALL be removed.
The protocol SHALL own all four execution phases — prepare, reserve, execute,
finalize — and SHALL NOT return `Recovered` before the runner outcome is
finalized and the finalize transaction commits [C5/C12]. `PreparedRecovery`
SHALL preserve the exact run/status/current-step/live-PID/checkpoint/wait/lease
authority captured during prepare and SHALL reselect/revalidate each inside the
reserve transaction before any mutation [B1].

### REQ-RP-002: Immutable canonical capsule with envelope digest
Fresh runs SHALL persist an immutable `ExecutionCapsuleV1` containing explicit
canonicalization, schema, domain, and provenance versions, the canonical resolved
workflow bytes, canonical resolved config bytes, the actual `LaunchProvenance`
canonical digest, and base ref, before any step executes. The capsule SHALL have
ONE envelope digest computed over a **framed canonical envelope byte format**
(length-prefixed authority fields with fixed-width version header) covering ALL
replay authority fields [C8/B9]. Component digests (workflow/config) SHALL be
metadata, not authority. The capsule SHALL be immutable after write [C8].
Adapter dispatch SHALL be fail-closed for unsupported schema/canonicalization/
domain/provenance versions [B9]. Historical capsule backfill SHALL be prohibited;
every run without a capsule written by fresh launch before execution SHALL be
salvage-only [B10].

### REQ-RP-003: Append-only immutable attempt IDs with complete state
Attempt records and checkpoints SHALL be append-only rows identified by
monotonic attempt IDs. Each attempt row SHALL include the complete
`StateSnapshot`, immutable attempt/source-parent IDs, operation binding, step
ID/status, capsule schema version + envelope digest, snapshot/checkpoint digests,
and a durable runner-result field [C3/B4]. Existing checkpoint rows SHALL NOT be
mutated in place; the latest attempt is selected by (run_id, attempt) ordering.
**[B4]** A durable `execution_attempt_id` SHALL be allocated at reserve (before
any effect) and recorded in an attempt-start row. The immutable outcome snapshot
SHALL be appended at finalize; the runner-result field SHALL make the outcome
recoverable after an execute-before-finalize crash.

### REQ-RP-004: Epoch-fenced idempotent recovery with operation ledger
The system SHALL maintain a distinct durable per-run recovery epoch (not derived
from `MAX(attempt generation)`) advanced via a CAS claim inside a short
`IMMEDIATE` transaction, with an affected-row check [C1]. **[B2]** The
`RecoveryRequest` SHALL carry the caller's `expected_epoch`, and the epoch CAS
at reserve SHALL be the ONLY compare-and-swap in the protocol; there SHALL be no
CAS at finalize. Every recovery operation SHALL be recorded in a
`recovery_operations` ledger with a stable `operation_id` (durable row identity)
derived from exact (run_id, step_id, capsule_envelope_digest, source_attempt_id)
bindings [C2]. **[B3]** A normalized `logical_request_key` (run_id + step_id +
capsule lineage) SHALL be separated from the exact `operation_id` for
uniqueness/conflict detection. A `Pending` operation SHALL carry a guarded
owner-PID/lease claim so exactly one process executes or reconciles; an expired
lease SHALL allow adoption. Operations SHALL transition Pending → Completed |
Refused | Conflict with a serialized prior outcome. An exact completed duplicate
SHALL return the prior outcome; a pending duplicate SHALL reconcile; a
conflicting duplicate SHALL refuse. No synthetic attempt rows SHALL be appended
to bump the epoch.

### REQ-RP-005: Step recovery policy from canonical StepDef
Each step SHALL resolve a `StepRecoveryPolicy` variant from the canonical
`StepDef` / explicit declaration plus the exact current `SAFE_RERUN_STEPS`
classifications (by step_id). Generic `shell`/`write_file` step types SHALL
default to `NonRecoverable` unless a specific canonical step explicitly opts
into `ContinueWorkspace` via a declared `recovery_policy` [C6]. **[B7]** The
`recovery_policy` field SHALL be added to `StepDef` (schema + canonicalizer +
validation) in the capsule milestone (P06–P08) before the protocol, and SHALL be
persisted in the canonical workflow bytes so the capsule envelope digest covers
it. The protocol SHALL select its strategy from this policy. The public
`RecoveryRequest` SHALL NOT carry a `trusted_internal` bool [C4]; authorization
is derived from the sealed `RecoveryAuthority` carrying a descriptor-bound
`WorkspaceAuthorization`.

### REQ-RP-006: Interrupted-implement exact verification
An interrupted run SHALL resume via `ContinueWorkspace` only after exact
verification of: worktree path, ownership (bootstrap + durable marker), base
ref, and diagnostic state. A mismatch on any dimension SHALL refuse recovery.
**[B6]** `PreparedRecovery` SHALL own a real retained `VerifiedWorkspace`
(OwnedFd-compatible) anchor obtained via the existing
`adjudicate_workspace_ownership` kernel. The descriptor-bound
`WorkspaceAuthorization` SHALL be revalidated inside the CAS transaction via a
descriptor-relative marker re-snapshot plus exact descriptor identity comparison
(TOCTOU guard) [C4/B6].

### REQ-RP-007: Legacy salvage, never exact continuation
Every run WITHOUT a valid pre-execution V1 capsule SHALL be salvage-only,
regardless of provenance or migration source (including runs with migrated
`LaunchProvenance` sentinel digests but no V1 capsule) [C9/B10]. It SHALL NEVER
be presented as an exact continuation of a fresh V1 run. **[B10]** Historical
capsule backfill SHALL be prohibited; a run without a capsule written by fresh
launch before execution SHALL remain salvage-only forever. Salvage lineage SHALL
be immutable and append-only.

### REQ-RP-008: Complete effect-intent state machine
Filesystem and remote effects SHALL use a durable `effect_intents` state
machine with: a stable unique effect key, operation/attempt/sequence binding,
canonical payload + digest/version, expected target/predecessor, observed
result, and Prepared/Completed/Conflict states [C7]. Intents SHALL be committed
before the effect is issued. **[B5]** Effect prepare SHALL be an insert-or-load
with exact-binding (payload digest + expected target/predecessor) comparison; a
mismatch SHALL transition the intent to conflict. Merge intents SHALL be
completed/conflicted inside the atomic merge transaction (P17). Each effect kind
(commit/push/open_pr/merge) SHALL have its own external reconciliation logic.
The finalize SHALL be guarded (only transitions from Prepared, with an
affected-row check). Call-site phases for commit/push/open_pr/merge SHALL be
wired.

### REQ-RP-009: Versioned capsule execution via object-safe adapters
The current binary SHALL load a versioned `ExecutionCapsuleV1` and execute it
through a versioned `CapsuleAdapter`. The adapter trait SHALL be object-safe
(`fn version(&self) -> u32` instead of `const VERSION`) [C8]. **[B9]** Adapter
dispatch SHALL be fail-closed for unsupported schema versions. Capsule schema
evolution SHALL be isolated from the engine core.

### REQ-RP-010: Typed verified merge with strategy-specific proof
Completion SHALL require a typed, verified merge artifact AND the durable
`RunStatus::Merged` state, committed in a single short `IMMEDIATE` atomic
artifact+status transaction with an explicit allowed predecessor and an
affected-row check [C11]. A status field alone SHALL NOT satisfy completion.
**[B11]** The merge verifier SHALL take authoritative injected Git/remote
interfaces (`MergeGitProbe`, `MergeRemoteProbe`) and bound repo/PR/base/head; it
SHALL compute all evidence itself (no ambient shell). **[B12]** A `merge_artifacts`
DDL SHALL be defined with `run_id` PRIMARY KEY, capsule envelope digest join key,
and exact artifact equality on conflict; the fixed allowed predecessor for the
`ReviewReady → Merged` transition SHALL be `RunStatus::ReviewReady`; runner
completion semantics SHALL be modified so merge-required workflows reach
`ReviewReady` (not `Completed`). The merge proof SHALL be a strategy-specific
enum [C10]:
- **merge-commit**: TWO ancestry checks — head and base are ancestors of the
  merge commit.
- **squash**: ancestry (base is ancestor of squash commit) PLUS computed
  expected/observed content digest match.
- **rebase**: ancestry (base is ancestor of final head) PLUS computed
  expected/observed patch-id equivalence.
The `result_sha` SHALL be strategy-neutral. The proof SHALL NOT assume the
merged SHA is an ancestor of the final head. External verification SHALL happen
before the transaction; the normal merge-required flow SHALL NOT first write
Completed.

### REQ-QUAL-001: Three consecutive mixed canaries
Qualification SHALL require three consecutive mixed canaries completing the full
viability gate with zero invariant violations.

### REQ-QUAL-002: Zero prohibited escapes
Qualification SHALL require zero occurrences of: direct SQL outside the
persistence layer, historical binary/config dependency, manual git/GitHub
mutation, duplicate effects, or invariant violations.

## Viability Gate Stages

A run passes the viability gate when it traverses all stages:

1. **Fresh launch** — capsule persisted, lease claimed, ownership provisioned.
2. **Deterministic interruption after worktree delta** — run interrupted after a
   workspace-mutating step produces a worktree delta.
3. **Supported recover** — recovery dispatched via `RecoveryProtocolV1`.
4. **Exact working-tree verification** — `ContinueWorkspace` verifies worktree,
   ownership, base ref, diagnostics.
5. **Allowlist staging** — `git add` only allowlisted paths.
6. **Commit/push** — committed and pushed via persisted intent.
7. **PR binding** — run bound to a PR with verified identity.
8. **Stable final-head CI/review** — CI and review pass on final head.
9. **Typed merge and reachability proof** — typed verified merge artifact +
   per-strategy reachability proof (`git merge-base --is-ancestor` with the
   exact recorded ancestor/descendant) + durable `Merged`.

## Explicitly Deferred

- **Distributed persistence** — single SQLite process only.
- **Async engine redesign** — `EngineRunner` stays synchronous; tokio wraps it.
- **Arbitrary legacy exact recovery** — only V1-capsule runs recover exactly.
- **Broader llxprt roadmap** — out of scope.

## Constraints

- No new crate dependencies.
- No weakening of ownership, provenance, lease, checkpoint, CI, scope, or review
  safety.
- Review-cycle cap: no more than two cycles of each review/remediation type per
  phase (see execution-tracker.md).
- No direct SQL outside the persistence layer (REQ-QUAL-002).
- No placeholder branches in locked pseudocode; every match arm is concrete.
- No claims of DB/remote atomicity; external verification is separate from
  IMMEDIATE transactions [C5/C11].
