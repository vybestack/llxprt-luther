# Phase 09: RecoveryProtocolV1 Stub (Milestone 3)

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P09`

## Prerequisites

- Required: P08A completed with PASS (Milestone 2 gate checked).
- Durable epoch + operations ledger + append-only store (P03–P05) and capsule
  store (P06–P08) are in place.

## Purpose

Create the minimal skeleton of `RecoveryProtocolV1`, `RecoveryRequest` (NO
`trusted_internal` bool [C4]), sealed `RecoveryAuthority` / `PreparedRecovery`
[C4], `RecoveryOutcome`, `RecoveryStrategy`, `RefusalReason`, and
`StepRecoveryPolicy` that compiles. This is Milestone 3's first phase. Stubs
return defaults / `todo!()`; no real logic yet. Tests in P10 will drive the real
behavior.

The protocol owns the phased model: prepare → reserve → execute → finalize [C5].
It consumes the **durable** epoch, operation ledger, append-only attempt store,
and capsule store directly. No in-memory persistence facade is introduced.

## Requirements Implemented (Expanded)

### REQ-RP-001: Single typed recovery abstraction

**Behavior**:
- GIVEN: a recovery request for an interrupted run
- WHEN: `RecoveryProtocolV1::recover(request)` is called
- THEN: exactly one code path dispatches, returning a `RecoveryOutcome`

### REQ-RP-005: Step recovery policy from canonical StepDef [C6]

**Behavior**:
- GIVEN: a step with `step_id = "shell"` (generic, no explicit declaration)
- WHEN: the protocol resolves the policy
- THEN: the policy is `NonRecoverable` (generic shell/write_file default to
       NonRecoverable unless a specific canonical step opts into
       ContinueWorkspace) [C6]

## Implementation Tasks

### Files to Create

- `src/engine/recovery/mod.rs`
  - `pub mod protocol;`, `pub mod policy;` (`pub mod capsule;`, `pub mod adapters;`, `pub mod intents;`, `pub mod salvage;` already exist from P06)
  - Re-export the public types.
  - MUST include: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P09`

- `src/engine/recovery/protocol/mod.rs`
  - `pub struct RecoveryRequest` WITHOUT `trusted_internal: bool` [C4]:
    fields: `run_id, step_id, expected_epoch: u64, operator_verb` (CLI-facing
    only). NO authorization bool; authority is derived internally. [C4/B2:
    expected_epoch is the caller's view; it is the ONLY epoch input.]
  - `pub struct RecoveryAuthority` (sealed, NOT public-constructible) [C4]:
    carries `workspace_authorization: WorkspaceAuthorization, capsule,
    source_attempt, policy, strategy`.
  - `pub struct PreparedRecovery` (sealed) [C4/B1/B6]: derived from exact durable
    state. Fields include: `authority, expected_epoch, operation_id,
    logical_request_key` [B3], AND the exact authority snapshot captured during
    prepare: `run_status, current_step, live_pid, checkpoint_identity,
    wait_state, lease` [B1], AND a retained `verified_workspace:
    VerifiedWorkspace` anchor (OwnedFd-compatible, obtained via
    `adjudicate_workspace_ownership`) [B6].
  - `pub enum RecoveryOutcome { Recovered { resumed_at_step, attempt_id, operation_id }, AlreadyApplied { prior_outcome, attempt_id, operation_id }, Refused { reason }, StaleEpoch { persisted, expected }, Conflict { detail } }` [C1/C2/B2/B3]
  - `pub enum RecoveryStrategy { ContinueWorkspace, Reenter, ReconcileThenReenter, CompensateThenRetry, Refused(RefusalReason) }`
  - `pub enum RefusalReason { NonRecoverable, VerificationFailed(String), NotAuthorized, SalvageOnly, ConflictingOperation }` [C2/C4/B6]
  - `pub enum OperatorVerb { Resume, Retry, Rewind }`
  - `pub struct RecoveryProtocolV1;` with `pub fn recover(conn, request) -> Result<RecoveryOutcome, RecoveryError>` returning `todo!()` for now [C5/C12]
  - `pub enum RecoveryError { Persistence(String), Capsule(String), Verification(String), OperationConflict(String), WorkspaceNotOwned, AuthorityChanged, WorkspaceAuthorizationRevoked }` [B1/B6]
  - MUST include: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P09`
  - MUST include: `/// @requirement:REQ-RP-001`

- `src/engine/recovery/policy.rs`
  - `pub enum StepRecoveryPolicy { PureReenter, Idempotent, ReconcileThenReenter, ContinueWorkspace, CompensateThenRetry, NonRecoverable }`
  - `pub fn policy_for_step(step_def: &StepDef, step_id: &str) -> StepRecoveryPolicy` —
    stub returns `NonRecoverable` (fail closed) until P11. Consumes canonical
    StepDef + explicit declaration. Generic shell/write_file default to
    `NonRecoverable` unless a specific canonical step opts into
    ContinueWorkspace. [C6]
  - `pub fn select_strategy(policy) -> RecoveryStrategy` — stub returns
    `Refused(NonRecoverable)` until P11. No `authorized_internal` parameter
    (authorization is sealed). [C4/C6]
  - MUST include: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P09`
  - MUST include: `/// @requirement:REQ-RP-005`

### Files to Modify

- `src/engine/mod.rs`
  - ADD: `pub mod recovery;` (if not already present from P06 wiring)
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P09`

- `src/lib.rs` (if needed for module visibility)
  - Ensure `engine::recovery` is reachable.

## Verification Commands

```bash
set -euo pipefail
cargo build --all-targets

grep -r "@plan:PLAN-20260723-SELFHOST-RELIABILITY.P09" workflow/src/engine/recovery/
grep -r "@requirement:REQ-RP-001" workflow/src/engine/recovery/protocol/mod.rs
grep -r "@requirement:REQ-RP-005" workflow/src/engine/recovery/policy.rs

# C4: RecoveryRequest MUST NOT have trusted_internal field
grep -rn "trusted_internal" workflow/src/engine/recovery/protocol/ && { echo "FAIL: trusted_internal in protocol"; exit 1; } || true

# C4: RecoveryAuthority is sealed (not public-constructible)
grep -rn "pub struct RecoveryAuthority" workflow/src/engine/recovery/protocol/mod.rs

grep -rn "// TODO\|// FIXME" workflow/src/engine/recovery/ && { echo "FAIL"; exit 1; } || true
find workflow/src/engine/recovery -name "*_v2*" -o -name "*_new*" | grep . && { echo "FAIL"; exit 1; } || true
```

## Success Criteria

- `cargo build --all-targets` succeeds.
- `RecoveryProtocolV1`, `RecoveryRequest` (no `trusted_internal`), `RecoveryAuthority`
  (sealed), `RecoveryOutcome`, `StepRecoveryPolicy` exist and compile. [C4]
- Stubs use `todo!()` or return defaults; no `// TODO` comments.

## Failure Recovery

If this phase fails:

1. `git checkout -- workflow/src/engine/recovery/ workflow/src/engine/mod.rs`
2. Re-run Phase 09.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P09.md`
