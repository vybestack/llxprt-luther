# Phase 00: Plan Overview & Specification Lock

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P00`

## Purpose

Lock the specification (`../specification.md`) as the single source of truth for
this plan. Confirm the phase sequence, milestone gates, and review-cycle cap
before any implementation phase begins.

## Specification Lock

- The specification at `../specification.md` is the authoritative artifact.
- Any change to requirements (REQ-RP-001 … REQ-RP-010, REQ-QUAL-001,
  REQ-QUAL-002) requires updating the specification and the execution tracker
  in the same change.
- The viability gate (9 stages) and qualification gate (3 canaries + zero
  prohibited escapes) are fixed.
- P00 is **complete and spec-locked**. Resolved decisions (below) are fixed for
  the remainder of the plan.

## Resolved Decisions (locked at P00A; corrected at P02 remediation cycle 1)

These decisions were verified against source during P00A and are now fixed,
except where corrected during P02 remediation cycle 1 (marked **[CORRECTED]**):

1. **Untyped/legacy step default**: `NonRecoverable` (fail closed). **[CORRECTED
   at C6]**: generic `shell`/`write_file` step types default to `NonRecoverable`
   unless a specific canonical step explicitly opts into `ContinueWorkspace` via
   a declared `recovery_policy`. Policy consumes the canonical `StepDef` +
   `SAFE_RERUN_STEPS` (by step_id).
2. **Effect intents**: new generalized `effect_intents` table (not a reuse of
   the migration-specific `legacy_migration_state.rs` schema). **[CORRECTED at
   C7]**: complete effect-intent state machine with stable unique key,
   operation/attempt/sequence binding, canonical payload + digest/version,
   expected target/predecessor, observed result, Prepared/Completed/Conflict,
   guarded finalize.
3. **Capsule digest**: **[CORRECTED at C8]** ONE envelope digest over ALL
   replay authority fields (run_id, config_root_encoding, resolved bytes,
   launch_provenance_digest, base_ref). Component digests (workflow_digest,
   config_digest) are metadata, NOT authority. The adapter is object-safe via
   `fn version(&self)`.
4. **Typed merge reachability proof**: uses `git merge-base --is-ancestor` and
   records the exact tested ancestor/descendant per strategy (merge-commit,
   squash, rebase) — it does NOT assume the merged SHA is an ancestor of the
   final head. **[CORRECTED at C10/C11]**: strategy-specific proof enum
   (merge-commit: two ancestry checks; squash: ancestry + content; rebase:
   ancestry + patch). External verification then short IMMEDIATE atomic
   artifact+status transaction with affected-row check.
5. **SQLite idioms**: use `RETURNING` (already used in `leases.rs` under
   rusqlite 0.34) where applicable.
6. **Recovery epoch** **[CORRECTED at C1]**: distinct durable per-run epoch,
   NOT `MAX(attempt generation)`, advanced via CAS with affected-row check.
7. **Operation ledger** **[CORRECTED at C2]**: `recovery_operations` ledger
   with stable idempotency key, Pending/Completed/Refused/Conflict, serialized
   prior outcome.
8. **Sealed authority** **[CORRECTED at C4]**: `RecoveryRequest` has no
   `trusted_internal` bool; sealed `RecoveryAuthority` derived from exact
   durable state + descriptor-bound `WorkspaceAuthorization`.
9. **Phased protocol** **[CORRECTED at C5/C12]**: prepare (no tx) → reserve
   (short IMMEDIATE tx) → execute (no tx) → finalize (short IMMEDIATE tx);
   cannot return `Recovered` before finalize.

## Source-Verified Corrections (applied at P00A)

- The current checkpoint loader selects the newest checkpoint **by timestamp**
  (`ORDER BY timestamp DESC LIMIT 1`); `set_resume_point` re-stamps an existing
  row's timestamp. Append-only attempt ordering (monotonic IDs) is the **future**
  model (REQ-RP-003), not the current behavior.
- `RunStatus::Merged` exists, is terminal, but has **no current writer**; P17
  introduces the first writer, gated on the typed artifact.
- `EngineRunner::resume_from_checkpoint` is **private**; external launch/resume
  surfaces reconstruct the run context then call `EngineRunner::run`, rather
  than calling `resume_from_checkpoint` directly.

## Phase Sequence (no gaps)

```
P00 (this) → P00A → P01 → P01A → P02 → P02A
  → M1: P03–P05 (durable epoch + operations ledger + append-only attempts + effect intents)
  → M2: P06–P08 (ExecutionCapsuleV1, envelope digest, object-safe adapter)
  → M3: P09–P11 (RecoveryProtocolV1, phased model, consuming durable store directly)
  → P12–P14 (capsule adapter wiring)
  → P15 (migration & deprecation)
  → P16 (deterministic failpoint matrix)
  → P17 (typed verified merge + strategy-specific proof + atomic artifact+status tx)
  → P18 (canary harness, 3 consecutive mixed)
  → P19 (qualification metrics & gate)
```

## P0 Prerequisite Milestones

Three P0 prerequisites are bounded as separate milestone phases, ordered so the
durable persistence substrate exists before the capsule and protocol layers that
consume it — avoiding any temporary in-memory persistence facade:

- **M1 (P03–P05)**: durable epoch (distinct row, CAS) + operations ledger +
  append-only attempt rows (complete StateSnapshot) + effect-intent state machine
  — durable from the start.
- **M2 (P06–P08)**: `ExecutionCapsuleV1` (ONE envelope digest over all replay
  authority fields; object-safe adapter) — immutable canonical capsule persisted
  at fresh launch.
- **M3 (P09–P11)**: `RecoveryProtocolV1` — the single typed recovery abstraction
  consuming the durable store directly (no facade), using the phased model
  (prepare → reserve → execute → finalize).

These gates must all pass before any post-M3 phase begins.

## Explicitly Deferred (out of scope for this plan)

- Distributed persistence.
- Async engine redesign.
- Arbitrary legacy exact recovery (legacy = salvage lineage only).
- Broader llxprt roadmap.

## Verification Gate

- [x] `../specification.md` exists and is internally consistent.
- [x] `../execution-tracker.md` lists all 20 phases in sequence.
- [x] Three milestone gates are enumerated (M1 append-only, M2 capsule, M3 protocol).
- [x] Review-cycle cap (two cycles per review type) is recorded.
- [x] Resolved decisions are locked.
- [x] Source-verified corrections applied.
