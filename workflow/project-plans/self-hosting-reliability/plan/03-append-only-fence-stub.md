# Phase 03: Epoch + Operations Ledger + Append-Only Attempts Stub (Milestone 1)

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P03`

## Prerequisites

- Required: P02A completed with PASS.
- Preflight (P00A) completed.

## Purpose

Create the durable persistence skeletons for Milestone 1:
- `src/persistence/recovery_epoch.rs` — distinct durable per-run epoch with CAS
  claim [C1] (epoch pseudocode lines 01–41). [C1]
- `src/persistence/recovery_operations.rs` — idempotent operation ledger [C2/B3]
  (operations pseudocode lines 01–149).
- `src/persistence/attempts.rs` — append-only attempt rows with complete
  `StateSnapshot` [C3/B4] (attempts pseudocode lines 01–111).
- `src/persistence/effect_intents.rs` — effect-intent state machine table [C7/B5]
  (intents pseudocode lines 01–17).

Stubs compile; behavior driven by P04. This phase lands the **durable** store
FIRST (Milestone 1), so later milestones build on a real durable foundation. No
in-memory persistence facade is introduced at any point.

> **Pseudocode references are to `02-pseudocode.md` component-local line
> numbers (B13).** Epoch = `recovery_epoch` lines; operations =
> `recovery_operations` lines; attempts = `recovery_attempts` lines; intents =
> `effect_intents` lines.

## Requirements Implemented (Expanded)

### REQ-RP-003: Append-only immutable attempt IDs with complete state [C3]
**Behavior**:
- GIVEN: an attempt store with attempts for run R
- WHEN: `append_attempt` is called
- THEN: a new row with a strictly greater attempt_id is inserted, carrying the
       complete `StateSnapshot`, capsule schema+envelope digest, and
       snapshot/checkpoint digest; no existing row is updated

### REQ-RP-004: Epoch-fenced idempotent recovery (durable half) [C1/C2]
**Behavior**:
- GIVEN: a run with epoch E
- WHEN: `cas_advance_epoch(tx, R, E)` is called inside an IMMEDIATE tx
- THEN: epoch advances to E+1 with affected-row check; concurrent advance returns `Stale`
- WHEN: `insert_pending(tx, operation_key, ...)` is called
- THEN: a pending operation row exists with the stable idempotency key

### REQ-RP-008: Effect-intent state machine table [C7]
**Behavior**:
- GIVEN: a commit/push/merge about to be issued
- WHEN: `prepare_effect` is called
- THEN: a durable intent row exists with stable key, payload digest, and
       Prepared status BEFORE the effect

## Implementation Tasks

### Files to Create

- `src/persistence/recovery_epoch.rs` [C1]
  - `pub const RECOVERY_EPOCH_TABLE: &str = "recovery_epoch";`
  - `pub fn init_epoch_table(conn) -> Result<()>`
  - `pub fn read_epoch(conn, run_id) -> Result<u64>` → stub returns 0
  - `pub fn cas_advance_epoch(tx, run_id, expected_epoch) -> Result<CasOutcome>` → `todo!()`
  - `pub enum CasOutcome { Advanced { from: u64, to: u64 }, Stale { persisted: u64, expected: u64 } }`
  - Table DDL (epoch pseudocode lines 02–06): single row per run, epoch INTEGER.
  - MUST include: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P03`, `/// @requirement:REQ-RP-004`

- `src/persistence/recovery_operations.rs` [C2/B3/B4]
  - `pub const RECOVERY_OPERATIONS_TABLE: &str = "recovery_operations";`
  - `pub fn init_operations_table(conn) -> Result<()>`
  - `pub fn compute_operation_id(run_id, step_id, capsule_envelope_digest, source_attempt_id, normalized_intent) -> String` → stub [B3]
  - `pub fn compute_logical_request_key(run_id, source_attempt_id, normalized_intent) -> String` → stub [B3]
  - `pub fn lookup_logical_operation(tx, logical_request_key) -> Result<Option<RecoveryOperation>>` → `todo!()`
  - `pub fn find_adoptable_pending(tx, logical_request_key, now) -> Result<Option<RecoveryOperation>>` → `todo!()` [B3]
  - `pub fn insert_pending(tx, operation_id, run_id, epoch, step_id, capsule_envelope_digest, source_attempt_id, logical_request_key, intent_digest, owner_pid, lease_expires_at, execution_attempt_id) -> Result<()>` → `todo!()` [B3/B4]
  - `pub fn try_adopt_pending(tx, operation_id, new_owner_pid, new_lease_expires_at, now) -> Result<AdoptOutcome>` → `todo!()` [B3]
  - `pub fn finalize_completed(tx, operation_id, serialized_outcome) -> Result<i64>` → `todo!()`
  - `pub fn finalize_refused(tx, operation_id, reason) -> Result<()>` → `todo!()`
  - `pub fn finalize_conflict(tx, operation_id, detail) -> Result<()>` → `todo!()`
  - `pub struct RecoveryOperation { operation_id, run_id, epoch, step_id, capsule_envelope_digest, source_attempt_id, logical_request_key, intent_digest, status, owner_pid, lease_expires_at, execution_attempt_id, serialized_outcome }`
  - `pub enum OperationStatus { Pending, Completed, Refused, Conflict }`
  - `pub enum AdoptOutcome { Adopted, StillOwned }` [B3]
  - Table DDL (operations pseudocode lines 02–17): includes `logical_request_key`, `owner_pid`, `lease_expires_at`, `execution_attempt_id` columns [B3/B4].
  - MUST include: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P03`, `/// @requirement:REQ-RP-004`

- `src/persistence/attempts.rs` [C3/B4]
  - `pub const RECOVERY_ATTEMPTS_TABLE: &str = "recovery_attempts";`
  - `pub fn init_attempts_table(conn) -> Result<()>`
  - `pub fn record_attempt_start(tx, run_id, epoch, source_attempt_id, operation_id, step_id, capsule_schema_version, capsule_envelope_digest, state_snapshot) -> Result<i64>` → `todo!()` [B4]
  - `pub fn append_attempt_outcome(tx, attempt_id, step_status, state_snapshot, runner_result, checkpoint_digest) -> Result<()>` → `todo!()` [B4]
  - `pub fn latest_for_step(conn, run_id, step_id) -> Result<Option<AttemptRow>>` → `todo!()`
  - `pub fn load_attempt(conn, attempt_id) -> Result<AttemptRow>` → `todo!()`
  - `pub fn load_unfinalized_for_operation(conn, operation_id) -> Result<Option<AttemptRow>>` → `todo!()` [B4]
  - `pub fn verify_snapshot_digest(row) -> Result<()>` → `todo!()`
  - `pub struct AttemptRow { attempt_id, run_id, epoch, source_attempt_id, operation_id, step_id, step_status, capsule_schema_version, capsule_envelope_digest, state_snapshot, snapshot_digest, checkpoint_digest, runner_result_json, started_at, finalized_at }` [B4]
  - Table DDL (attempts pseudocode lines 02–17): append-only, AUTOINCREMENT PK, complete StateSnapshot, capsule binding, digests, `operation_id`, `runner_result_json`, `started_at`, `finalized_at` columns [C3/B4].
  - MUST include: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P03`, `/// @requirement:REQ-RP-003`

- `src/persistence/effect_intents.rs` [C7/B5]
  - `pub fn init_effect_intents_table(conn) -> Result<()>`
  - `pub fn compute_effect_key(operation_id, attempt_id, sequence, effect_kind) -> String` → stub
  - `pub fn prepare_effect(conn, operation_id, attempt_id, sequence, kind, payload, expected_target, expected_predecessor) -> Result<EffectIntent>` → `todo!()` [B5: insert-or-load exact-binding comparison]
  - `pub fn load_effect(conn, key) -> Result<EffectIntent>` → `todo!()`
  - `pub fn reconcile_effect(conn, key, observed) -> Result<ReconcileVerdict>` → `todo!()`
  - `pub fn finalize_effect(conn, key, status, observed_result) -> Result<()>` → `todo!()`
  - `pub struct EffectIntent { effect_key, operation_id, attempt_id, sequence, effect_kind, canonical_payload, payload_digest, payload_version, expected_target, expected_predecessor, observed_result, status }`
  - `pub enum EffectKind { Commit, Push, OpenPr, Merge }`
  - `pub enum ReconcileVerdict { Completed { result: Option<String> }, NeedsReissue, Conflict { detail: String } }`
  - Table DDL (intents pseudocode lines 02–17): stable key, binding, canonical payload/digest/version, expected target/predecessor, observed result, Prepared/Completed/Conflict. [B5: prepare is insert-or-load with exact-binding comparison.]
  - MUST include: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P03`, `/// @requirement:REQ-RP-008`

### Files to Modify

- `src/persistence/mod.rs`
  - ADD: `pub mod recovery_epoch;`, `pub mod recovery_operations;`, `pub mod attempts;`, `pub mod effect_intents;`
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P03`

## Verification Commands

```bash
set -euo pipefail
cargo build --all-targets || exit 1
cargo clippy --workspace --all-targets --all-features -- -D warnings || exit 1
grep -r "@plan:PLAN-20260723-SELFHOST-RELIABILITY.P03" workflow/src/persistence/recovery_epoch.rs workflow/src/persistence/recovery_operations.rs workflow/src/persistence/attempts.rs workflow/src/persistence/effect_intents.rs workflow/src/persistence/mod.rs
grep -r "@requirement:REQ-RP-003" workflow/src/persistence/attempts.rs
grep -r "@requirement:REQ-RP-004" workflow/src/persistence/recovery_epoch.rs workflow/src/persistence/recovery_operations.rs
grep -r "@requirement:REQ-RP-008" workflow/src/persistence/effect_intents.rs
grep -rn "// TODO\|// FIXME" workflow/src/persistence/recovery_epoch.rs workflow/src/persistence/recovery_operations.rs workflow/src/persistence/attempts.rs workflow/src/persistence/effect_intents.rs && { echo "FAIL"; exit 1; } || true
```

## Success Criteria

- Compiles; stubs use `todo!()`; no `// TODO` comments.
- Tables are append-only by design (AUTOINCREMENT PK, no UPDATE of existing rows
  except the epoch CAS which is a guarded conditional UPDATE).
- Epoch table is distinct from attempts (not derived from MAX(generation)).
- Effect intents table has stable key + binding + canonical payload/digest.

## Failure Recovery

`git checkout -- workflow/src/persistence/recovery_epoch.rs workflow/src/persistence/recovery_operations.rs workflow/src/persistence/attempts.rs workflow/src/persistence/effect_intents.rs workflow/src/persistence/mod.rs`

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P03.md`
