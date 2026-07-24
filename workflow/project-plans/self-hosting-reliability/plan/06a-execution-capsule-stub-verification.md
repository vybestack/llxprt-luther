# Phase 06A: Execution Capsule Stub Verification

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P06A`

## Prerequisites

- Required: P06 completed.

## Verification Commands

```bash
cargo build --all-targets || exit 1
cargo clippy -- -D warnings || exit 1
grep -r "@plan:PLAN-20260723-SELFHOST-RELIABILITY.P06" workflow/src/engine/recovery/ workflow/src/persistence/capsule_store.rs | wc -l
```

## Structural Verification Checklist

- [ ] `ExecutionCapsuleV1` has explicit canonicalization/schema/domain versions
      and ONE `envelope_digest` over ALL replay authority fields (run_id,
      config_root_encoding, resolved bytes, launch_provenance_digest, base_ref).
      Component digests (workflow/config) are metadata, NOT authority. [C8]
- [ ] `CapsuleAdapter` trait uses `fn version(&self) -> u32` (object-safe), NOT
      `const VERSION`. [C8]
- [ ] `V1Adapter` implements `CapsuleAdapter` with `version() == 1`.
- [ ] `adapter_for` dispatches on `schema_version` and errors on unknown.
- [ ] Capsule store table schema exists (envelope_digest as authority,
      component digests as metadata). [C8]
- [ ] No `// TODO` comments.

## Semantic Verification Checklist

- [ ] `CapsuleError` has `EnvelopeDigestMismatch` (not `DigestMismatch`). [C8]
- [ ] Store is designed to be immutable (PRIMARY KEY run_id; refuse overwrite,
      not `ON CONFLICT DO UPDATE`).
- [ ] `verify_envelope_digest` (not `verify_capsule_digest`) is the verification
      entry point. [C8]

## Failure Recovery

Two-cycle cap on structural review.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P06A.md`
