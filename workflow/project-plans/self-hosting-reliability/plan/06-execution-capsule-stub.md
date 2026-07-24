# Phase 06: ExecutionCapsuleV1 Stub (Milestone 2)

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P06`

## Prerequisites

- Required: P05A completed with PASS (Milestone 1 gate checked).

## Purpose

Create the minimal skeleton of `ExecutionCapsuleV1` (with ONE envelope digest
over a **framed canonical envelope** [C8/B9]), its builder, the immutable capsule
store, and the object-safe `CapsuleAdapter` trait + V1 adapter + version registry
[C8/B9]. **[B7]** This phase ALSO adds `recovery_policy: Option<StepRecoveryPolicy>`
to `StepDef` (schema + canonicalizer + validation) so the capsule carries it.

Stubs compile; behavior driven by P07.

> **Pseudocode references are to `02-pseudocode.md` component-local line
> numbers (B13).** Capsule = `ExecutionCapsuleV1` lines; adapter =
> `CapsuleAdapter` lines; policy = `StepRecoveryPolicy` lines.

## Requirements Implemented (Expanded)

### REQ-RP-002: Immutable canonical capsule with envelope digest [C8]
**Behavior**:
- GIVEN: a freshly resolved workflow type + config + provenance + base ref
- WHEN: `build_capsule_v1(...)` is called
- THEN: returns a `ExecutionCapsuleV1` with explicit canonicalization/schema/
       domain versions and ONE envelope digest over ALL replay authority fields
- GIVEN: a persisted capsule
- WHEN: an attempt is made to mutate it (re-write with different content)
- THEN: the store refuses (immutable)

### REQ-RP-009: Versioned capsule execution via object-safe adapters [C8]
**Behavior**:
- GIVEN: a V1 capsule
- WHEN: `adapter_for(capsule)` is called
- THEN: returns a `Box<dyn CapsuleAdapter>` (object-safe via `fn version(&self)`)

## Implementation Tasks

### Files to Create

- `src/engine/recovery/capsule.rs`
  - `pub struct ExecutionCapsuleV1` with fields per capsule pseudocode 02–22:
    schema_version, canonicalization_version, domain_version, provenance_version
    [B9], run_id, config_root_encoding, resolved_workflow_bytes,
    resolved_config_bytes, launch_provenance_digest, base_ref, envelope_digest
    (THE authority, computed over framed canonical envelope [B9]),
    workflow_digest (metadata), config_digest (metadata), created_at. [C8/B9]
  - `pub const SUPPORTED_SCHEMA_VERSIONS: &[u32] = &[1];` (and canonicalization/
    domain/provenance) per capsule pseudocode 25–28. [B9]
  - `pub fn build_envelope_frame(...) -> Vec<u8>` per capsule pseudocode 43–56
    (framed canonical envelope byte format: fixed-width version header +
    length-prefixed authority fields). [B9]
  - `pub fn build_capsule_v1(...) -> Result<ExecutionCapsuleV1, CapsuleError>` → `todo!()` [C8/B9]
  - `pub fn verify_envelope_digest(capsule) -> Result<(), CapsuleError>` → `todo!()` [C8/B9: fail-closed version dispatch]
  - `pub enum CapsuleError { EnvelopeDigestMismatch, UnsupportedSchema(u32), UnsupportedCanonicalization(u32), UnsupportedDomain(u32), UnsupportedProvenance(u32), Canonicalize { config_root, io_error }, InvalidEncoding { encoded, reason } }` [C8/B9]
  - MUST include: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P06`
  - MUST include: `/// @requirement:REQ-RP-002`

- `src/engine/recovery/adapters/mod.rs`
  - `pub trait CapsuleAdapter` with `fn version(&self) -> u32` (NOT `const VERSION`) [C8],
    `fn step_def_for`, `fn build_instance`, `fn envelope_digest` (per adapters
    pseudocode 02–07).
  - `pub fn adapter_for(capsule) -> Result<Box<dyn CapsuleAdapter>>` (lines 10–15) [C8]
  - `pub enum AdapterError { UnsupportedCapsuleVersion(u32), StepNotFound { step_id } }`
  - MUST include: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P06`
  - MUST include: `/// @requirement:REQ-RP-009`

- `src/engine/recovery/adapters/v1.rs`
  - `pub struct V1Adapter;` implementing `CapsuleAdapter` with `fn version(&self) -> u32 { 1 }`; methods `todo!()` [C8]
  - MUST include: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P06`

- `src/persistence/capsule_store.rs`
  - `pub fn persist_capsule_v1(conn, capsule) -> Result<()>` → `todo!()` (immutable: refuse overwrite)
  - `pub fn load_capsule_v1(conn, run_id) -> Result<ExecutionCapsuleV1>` → `todo!()`
  - Table: `execution_capsules (run_id TEXT PRIMARY KEY, schema_version, canonicalization_version, domain_version, capsule_json, envelope_digest, workflow_digest, config_digest, created_at)` — stores the envelope digest as authority + component digests as metadata. [C8]
  - MUST include: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P06`
  - MUST include: `/// @requirement:REQ-RP-002`

### Files to Modify

- `src/persistence/mod.rs`
  - ADD: `pub mod capsule_store;`
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P06`

- `src/engine/recovery/mod.rs`
  - ADD: `pub mod capsule;` (if not already), ensure `pub mod adapters;`
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P06`

### [B7] StepDef Schema Changes

- `src/workflow/schema.rs`
  - ADD `recovery_policy: Option<StepRecoveryPolicy>` field to `StepDef` per
    policy pseudocode lines 11–18. [B7]
  - The field is `Option<StepRecoveryPolicy>` (serde-tagged enum).
  - Default is `None` (falls through to SAFE_RERUN_STEPS or NonRecoverable).
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P06`, `/// @requirement:REQ-RP-005`

- `src/persistence/launch_provenance.rs`
  - `canonicalize_workflow_type`: include the `recovery_policy` field in the
    canonical serialization so the capsule envelope digest covers it. [B7]
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P06`, `/// @requirement:REQ-RP-005`

- `src/engine/recovery/policy.rs` (if not already created in P06 for the enum)
  - `pub enum StepRecoveryPolicy { PureReenter, Idempotent, ReconcileThenReenter, ContinueWorkspace, CompensateThenRetry, NonRecoverable }` per policy pseudocode 02–09.
  - `policy_for_step` and `select_strategy` stubs come in P09.

## Verification Commands

```bash
cargo build --all-targets || exit 1
cargo clippy -- -D warnings || exit 1
grep -r "@plan:PLAN-20260723-SELFHOST-RELIABILITY.P06" workflow/src/engine/recovery/ workflow/src/persistence/capsule_store.rs
grep -r "@requirement:REQ-RP-002" workflow/src/engine/recovery/capsule.rs workflow/src/persistence/capsule_store.rs
grep -r "@requirement:REQ-RP-009" workflow/src/engine/recovery/adapters/mod.rs
# Verify object-safe adapter: fn version(&self), not const VERSION [C8]
grep -rn "const VERSION" workflow/src/engine/recovery/adapters/ && echo "FAIL: const VERSION is not object-safe"
grep -rn "fn version(&self)" workflow/src/engine/recovery/adapters/mod.rs
grep -rn "// TODO\|// FIXME" workflow/src/engine/recovery/ workflow/src/persistence/capsule_store.rs && echo "FAIL"
```

## Success Criteria

- `cargo build --all-targets` succeeds.
- `ExecutionCapsuleV1` has envelope digest (not capsule-scoped component digests
  as authority). [C8]
- `CapsuleAdapter` uses `fn version(&self)` (object-safe), not `const VERSION`. [C8]
- Stubs use `todo!()` or defaults; no `// TODO` comments.

## Failure Recovery

`git checkout -- workflow/src/engine/recovery/capsule.rs workflow/src/engine/recovery/adapters/ workflow/src/persistence/capsule_store.rs workflow/src/persistence/mod.rs`

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P06.md`
