# Phase 11: RecoveryProtocolV1 Implementation (Milestone 3 Complete)

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P11`

## Prerequisites

- Required: P10A completed with PASS (red phase confirmed).
- Durable epoch + operations ledger + append-only store (P03–P05) and capsule
  store (P06–P08) are in place.

## Purpose

Implement `RecoveryProtocolV1::recover()` to make all P10 tests pass, following
the pseudocode (02-pseudocode.md, protocol lines 72–357) line-by-line. The
protocol owns the four-phase execution model: prepare → reserve → execute →
finalize [C5/C12]. Because the durable epoch, operation ledger, append-only
attempt store, and capsule store already exist, the protocol consumes them
directly. **No in-memory persistence facade is introduced at any point.**

> **Pseudocode references are to `02-pseudocode.md` component-local line
> numbers (B13).** Protocol = `RecoveryProtocolV1` lines; policy =
> `StepRecoveryPolicy` lines.

## The Phased Execution Model [C5/B1/B2/B4/B6]

1. **Prepare** (no transaction): load capsule + verify envelope digest; load
   StepDef; resolve policy; derive `RecoveryAuthority` from exact durable state
   + descriptor-bound `WorkspaceAuthorization` via
   `adjudicate_workspace_ownership` [B6]; capture exact authority snapshot
   (run/status/current-step/live-PID/checkpoint/wait/lease) [B1]; construct
   `PreparedRecovery` with retained `VerifiedWorkspace` anchor [B6].
2. **Reserve** (short IMMEDIATE tx): reselect/revalidate exact authority snapshot
   [B1]; descriptor-relative marker re-snapshot + exact identity comparison
   (TOCTOU guard) [B6]; SINGLE epoch CAS (the ONLY CAS in the protocol) [B2];
   reconcile operation ledger (Completed → prior outcome, Pending → reconcile
   via guarded owner/lease claim [B3], Conflict → refuse); allocate durable
   `execution_attempt_id` via `record_attempt_start` [B4]; insert Pending if new.
3. **Execute** (no transaction): external work (runner) or ContinueWorkspace
   exact verification. The protocol CANNOT return `Recovered` here. [C12]
4. **Finalize** (short IMMEDIATE tx): append outcome via
   `append_attempt_outcome` [B4]; finalize operation (guarded). NO epoch CAS
   here [B2]. Only now may the protocol return `Recovered`. [C12]

## Requirements Implemented (Expanded)

### REQ-RP-001: Single typed recovery abstraction
**Implementation**: protocol pseudocode 72–124 — one entry point dispatches all
recovery via the four-phase model.

### REQ-RP-004: Epoch-fenced idempotent recovery with operation ledger
**Implementation**: protocol reserve phase (pseudocode 180–271) — SINGLE epoch
CAS [B2], operation ledger lookup, Pending/Completed/Conflict reconciliation,
guarded owner/lease claim [B3]. [C1/C2]

### REQ-RP-005: Step recovery policy from canonical StepDef
**Implementation**: prepare phase (pseudocode 130–139) — `policy_for_step(step_def)`
consuming canonical StepDef + SAFE_RERUN_STEPS. [C6/B7]

### REQ-RP-006: ContinueWorkspace exact verification with sealed authority
**Implementation**: execute phase (pseudocode 285–293) — verify worktree, ownership
(revalidated in reserve via `adjudicate_workspace_ownership` + descriptor-relative
marker re-snapshot [B6]), base ref, diagnostic.

## Implementation Tasks

### Files to Modify

- `src/engine/recovery/protocol/mod.rs` (and split phase modules
  `protocol/prepare.rs`, `protocol/reserve.rs`, `protocol/execute.rs`,
  `protocol/finalize.rs`, `protocol/executor.rs`)
  - Implement `RecoveryProtocolV1::recover(conn, request)` per pseudocode
    lines 72–357 using the four-phase model. [C5]
  - **Prepare phase** (no tx, pseudocode 130–175): load capsule via
    `capsule_store::load_capsule_v1`; `verify_envelope_digest`; load StepDef via
    adapter; `policy_for_step`; adjudicate ownership via
    `adjudicate_workspace_ownership(workspace, run_id)` [B6]; capture exact
    authority snapshot (run_status, current_step, live_pid, checkpoint_identity,
    wait_state, lease) [B1]; derive sealed `RecoveryAuthority`; construct
    `PreparedRecovery` with retained `VerifiedWorkspace` anchor [B6].
  - **Reserve phase** (short IMMEDIATE tx, pseudocode 180–271): reselect/revalidate
    exact authority snapshot inside tx (rollback on change) [B1];
    descriptor-relative marker re-snapshot + exact identity comparison [B6];
    SINGLE `cas_advance_epoch` (the ONLY CAS) [B2]; `lookup_operation`;
    reconcile (Completed → prior outcome, Pending → guarded owner/lease claim
    adoption [B3], conflict → refuse); `record_attempt_start` to allocate durable
    `execution_attempt_id` [B4]; `insert_pending` if new. [C1/C2/C4]
  - **Execute phase** (no tx, pseudocode 276–313): ContinueWorkspace exact
    verification OR runner external work. Protocol CANNOT return Recovered here.
    [C12]
  - **Finalize phase** (short IMMEDIATE tx, pseudocode 319–357): `append_attempt_outcome`
    (guarded) [B4]; `finalize_completed` (guarded). NO epoch CAS here. [B2/C12]
    Return `Recovered` only after finalize commits. [C12]
  - Reference pseudocode line numbers in comments.
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11`
  - ADD: `/// @requirement:REQ-RP-001,REQ-RP-004,REQ-RP-005,REQ-RP-006`

- `src/engine/recovery/policy.rs`
  - Implement `policy_for_step(step_def)` per pseudocode lines 23–36
    (canonical StepDef + explicit declaration + SAFE_RERUN_STEPS classification;
    generic shell/write_file default to NonRecoverable unless explicitly
    declared ContinueWorkspace). [C6/B7]
  - Implement `select_strategy(policy)` per lines 47–56 (no `authorized_internal`
    parameter). [C4]
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11`
  - ADD: `/// @requirement:REQ-RP-005`

- `src/engine/recovery/mod.rs`
  - Re-export any new public items.
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11`

## Verification Commands

```bash
set -euo pipefail
# All P10 tests now pass (green)
cargo test --test recovery_protocol_integration_tests || exit 1

# No test modifications during implementation
git diff workflow/tests/recovery_protocol_integration_tests.rs | grep -E "^[+-]" | grep -v "^[+-]{3}" && { echo "FAIL: tests modified"; exit 1; } || true

# No debug code
grep -rn "println!\|dbg!\|todo!\|unimplemented!" workflow/src/engine/recovery/protocol/ workflow/src/engine/recovery/policy.rs && { echo "FAIL"; exit 1; } || true

# No placeholder comments
grep -rn -E "(placeholder|not yet|will be)" workflow/src/engine/recovery/ && { echo "FAIL"; exit 1; } || true

# Plan + requirement markers
grep -r "@plan:PLAN-20260723-SELFHOST-RELIABILITY.P11" workflow/src/engine/recovery/
grep -r "@requirement:REQ-RP-001" workflow/src/engine/recovery/protocol/mod.rs

# C4: no trusted_internal in protocol
grep -rn "trusted_internal" workflow/src/engine/recovery/protocol/ && { echo "FAIL"; exit 1; } || true

# Full suite still passes
cargo test || exit 1
```

## Success Criteria

- All P10 integration tests pass.
- No test files modified.
- No `todo!()` / `println!` / `dbg!` / placeholder comments in implementation.
- `cargo test` (full suite) passes.
- Milestone 3 gate: `RecoveryProtocolV1` is the single typed recovery entry
  point, consuming the durable epoch/operations/attempts/capsule stores directly
  (no in-memory facade), using the phased model [C5].

## Failure Recovery

If this phase fails:

1. `git checkout -- workflow/src/engine/recovery/protocol/ workflow/src/engine/recovery/policy.rs`
2. Re-run P11. Review-cycle cap: two cycles.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P11.md`

## Milestone 3 Gate

After P11A passes, check the M3 box in `../execution-tracker.md`:
- [ ] `RecoveryProtocolV1` is the single typed recovery entry point.
- [ ] No separate resume/retry/rewind execution path is reachable from a fresh
      entry point (CLI verbs now build `RecoveryRequest`).
- [ ] The protocol consumes the durable `recovery_epoch`,
      `recovery_operations`, `recovery_attempts`, and `execution_capsules`
      tables directly (no in-memory facade).
- [ ] The protocol uses the phased model (prepare → reserve → execute →
      finalize). [C5]
- [ ] The protocol cannot return `Recovered` before finalize commits. [C12]
- [ ] `RecoveryRequest` has no `trusted_internal` bool; carries `expected_epoch`
      [B2]; `RecoveryAuthority` is sealed. [C4]
- [ ] `PreparedRecovery` captures exact authority snapshot and revalidates in
      reserve; owns retained `VerifiedWorkspace` anchor via
      `adjudicate_workspace_ownership`. [B1/B6]
- [ ] SINGLE epoch CAS at reserve; no finalize CAS. [B2]
- [ ] Durable `execution_attempt_id` allocated at reserve. [B4]

All three P0 milestone gates (M1, M2, M3) MUST be checked before P12 begins.
