# Phase 03A: Epoch + Operations Ledger + Append-Only Stub Verification

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P03A`

## Prerequisites

- Required: P03 completed.

## Verification Commands

```bash
cargo build --all-targets || exit 1
cargo clippy -- -D warnings || exit 1
grep -r "@plan:PLAN-20260723-SELFHOST-RELIABILITY.P03" workflow/src/persistence/recovery_epoch.rs workflow/src/persistence/recovery_operations.rs workflow/src/persistence/attempts.rs workflow/src/persistence/effect_intents.rs
```

## Structural Verification Checklist

- [ ] `recovery_epoch` table is distinct from `recovery_attempts` (not derived
      from MAX(generation)). [C1]
- [ ] `cas_advance_epoch` has CAS guard (`WHERE epoch = ?`) and affected-row
      check. [C1]
- [ ] `recovery_operations` table has stable `operation_id` PRIMARY KEY,
      Pending/Completed/Refused/Conflict status, serialized_outcome. [C2]
- [ ] `recovery_attempts` table is append-only (AUTOINCREMENT PK, complete
      `StateSnapshot`, capsule binding, snapshot digest). [C3]
- [ ] `effect_intents` table has stable `effect_key`, operation/attempt/sequence
      binding, canonical payload/digest/version, expected target/predecessor,
      observed result, Prepared/Completed/Conflict. [C7]
- [ ] `EffectKind` covers Commit/Push/OpenPr/Merge.
- [ ] `ReconcileVerdict` has Completed/NeedsReissue/Conflict.
- [ ] `CasOutcome` has Advanced/Stale.
- [ ] `OperationStatus` has Pending/Completed/Refused/Conflict.
- [ ] No `// TODO` comments.

## Semantic Verification Checklist

- [ ] `read_epoch` reads a dedicated row, not `MAX(generation)` from attempts. [C1]
- [ ] `cas_advance_epoch` uses conditional UPDATE with affected-row check, not
      unconditional bump. [C1]
- [ ] `append_attempt` is an INSERT carrying complete `StateSnapshot` + digests
      (not an upsert). [C3]
- [ ] `compute_operation_key` binds run_id + step_id + capsule_envelope_digest +
      source_attempt_id. [C2]
- [ ] `compute_effect_key` binds operation_id + attempt_id + sequence + kind. [C7]
- [ ] No temporary in-memory persistence facade is introduced.

## Failure Recovery

Two-cycle cap.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P03A.md`
