# Phase 15: Migration & Deprecation

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P15`

## Prerequisites

- Required: P14A completed with PASS.

## Purpose

Migrate the existing checkpoint storage to append-only, deprecate the separate
resume/retry/rewind execution paths in favor of `RecoveryProtocolV1`, and
establish legacy salvage lineage [C9]. **[B10]** Historical capsule backfill is
PROHIBITED: every run without a capsule written by fresh launch before execution
is salvage-only. This phase is non-destructive: existing rows are preserved;
new runs use the new tables.

> **Pseudocode references are to `02-pseudocode.md` component-local line
> numbers (B13).** This phase doc does NOT carry inline pseudocode; it
> references `02-pseudocode.md` (the SOLE source).

## Requirements Implemented (Expanded)

### REQ-RP-003: Append-only migration
**Behavior**:
- GIVEN: a `checkpoints` table with pre-V1 rows
- WHEN: the migration runs
- THEN: existing rows are preserved (salvage lineage); new attempts go to
  `recovery_attempts`; the loader selects latest by (run, attempt)
- GIVEN: a pre-V1 run without a capsule
- WHEN: recovery is attempted
- THEN: it is treated as salvage lineage, NOT exact continuation

### REQ-RP-001: Deprecate separate paths
**Behavior**:
- GIVEN: the CLI verbs `runs resume|retry|rewind`
- WHEN: invoked
- THEN: each builds a `RecoveryRequest` (NO `trusted_internal` bool [C4]) and
       calls `RecoveryProtocolV1::recover()`; no separate execution path remains

### REQ-RP-007: Legacy salvage, never exact continuation [C9/B10]
**Behavior**:
- GIVEN: a run row WITHOUT a valid pre-execution V1 capsule (including runs with
       migrated `LaunchProvenance` sentinel digests but no V1 capsule) [C9]
- WHEN: any recovery is attempted
- THEN: returns salvage lineage (audit-only), refuses exact continuation [C9]
- GIVEN: a run WITH a valid pre-execution V1 capsule
- WHEN: recovery is attempted
- THEN: exact recovery proceeds normally
- GIVEN: an attempt to backfill a capsule for a historical run without one [B10]
- WHEN: recovery is attempted
- THEN: STILL salvage-only (backfill is prohibited) [B10]

### Legacy Salvage Implementation Reference

Follow `02-pseudocode.md` salvage component lines 06–60 (`classify_run`,
`salvage_recover`, `append_salvage_record`). Do NOT reimplement the pseudocode
inline here.

## Implementation Tasks

### Files to Modify

- `src/persistence/checkpoint.rs`
  - `save_checkpoint_with_conn`: replace `INSERT ... ON CONFLICT DO UPDATE` with
    an append insert into `recovery_attempts` (delegating to
    `attempts::append_attempt` with complete `StateSnapshot` [C3]) for new runs;
    preserve the legacy table for salvage reads. Keep a read shim that selects
    latest by (run, attempt).
  - `load_checkpoint_with_conn`: select latest by (run, attempt) from
    `recovery_attempts`, falling back to legacy `checkpoints` for pre-V1 runs
    (salvage).
  - `set_resume_point`: epoch-fenced re-arm via `recovery_epoch::cas_advance_epoch` [C1].
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P15`, `/// @requirement:REQ-RP-003,REQ-RP-007`

- `src/engine/continuation.rs`
  - `ContinuationKind` / `ContinuationRequest` become thin CLI-facing selectors
    that build a `RecoveryRequest` (mapping Resume/Retry/Rewind → `OperatorVerb`).
    The `trusted_internal: bool` field is REMOVED [C4]; authorization is derived
    from the sealed `RecoveryAuthority` inside the protocol. The execution logic
    delegates to `RecoveryProtocolV1::recover()`.
  - `prepare_continuation` / `commit_continuation`: delegate to the protocol;
    the generation fence is now the epoch CAS inside the protocol's reserve
    phase.
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P15`, `/// @requirement:REQ-RP-001`

- `src/cli/` (resume/retry/rewind handlers)
  - Build `RecoveryRequest` (without `trusted_internal` [C4]) and call
    `RecoveryProtocolV1::recover()`.
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P15`, `/// @requirement:REQ-RP-001`

- `src/engine/recovery/salvage.rs`
  - Implement salvage lineage per salvage pseudocode lines 01–35: every run
    WITHOUT a valid pre-execution V1 capsule (including migrated-provenance
    runs with sentinel digests) is salvage-only [C9]. Immutable idempotent
    lineage; exact recovery refuses.
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P15`, `/// @requirement:REQ-RP-007`

### Migration Script (idempotent, non-destructive)

- `init_recovery_tables(conn)`: create `recovery_epoch`, `recovery_operations`,
  `recovery_attempts`, `effect_intents`, `execution_capsules`,
  `salvage_lineage` if absent. Do NOT drop or rewrite `checkpoints`.
- A one-time salvage tag marks pre-V1 rows so the loader treats them as salvage.

## Verification Commands

```bash
set -euo pipefail
cargo test || exit 1
cargo clippy --workspace --all-targets --all-features -- -D warnings || exit 1

# No ON CONFLICT DO UPDATE remains for new-run writes
grep -rn "ON CONFLICT(run_id, step_id) DO UPDATE" workflow/src/persistence/checkpoint.rs && echo "WARN: legacy path retained for salvage read-only is OK; new writes must append" || true

# Separate execution paths removed: resume/retry/rewind delegate to protocol
grep -rn "RecoveryProtocolV1::recover" workflow/src/engine/continuation.rs workflow/src/cli/

# C4: trusted_internal removed from continuation
grep -rn "trusted_internal" workflow/src/engine/continuation.rs workflow/src/cli/ && echo "WARN: trusted_internal should be removed [C4]" || true

# Salvage refuses exact continuation for pre-V1 runs
grep -rn "salvage\|SalvageOnly\|NoValidV1Capsule" workflow/src/engine/recovery/salvage.rs

grep -r "@plan:PLAN-20260723-SELFHOST-RELIABILITY.P15" workflow/src/persistence/checkpoint.rs workflow/src/engine/continuation.rs workflow/src/cli/ workflow/src/engine/recovery/salvage.rs
```

## Semantic Checks

- [x] Existing pre-V1 `checkpoints` rows are readable (salvage), never mutated.
- [x] New runs append to `recovery_attempts`; the loader selects latest by (run, attempt).
- [x] A pre-V1 run WITHOUT a valid V1 capsule (including migrated-provenance
      runs with sentinel digests) cannot be exactly continued (salvage only). [C9]
- [x] `runs resume|retry|rewind` all route through `RecoveryProtocolV1::recover()`.
- [x] `trusted_internal` bool is removed from `ContinuationRequest`. [C4]
- [x] No row loss: migration is additive.

## Failure Recovery

If migration breaks existing tests: the legacy read shim is incomplete; restore
salvage reads without rewriting the legacy table. Two-cycle cap.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P15.md`
