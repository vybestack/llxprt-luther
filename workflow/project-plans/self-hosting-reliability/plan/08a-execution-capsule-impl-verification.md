# Phase 08A: Execution Capsule Implementation Verification

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P08A`

## Prerequisites

- Required: P08 completed.

## Verification Commands

```bash
cargo test --test execution_capsule_integration_tests || exit 1
cargo test || exit 1
cargo clippy -- -D warnings || exit 1
grep -rn -E "(todo!|unimplemented!|TODO|FIXME|HACK|placeholder|not yet|will be)" workflow/src/engine/recovery/capsule.rs workflow/src/persistence/capsule_store.rs workflow/src/engine/recovery/adapters/
# Expected: no matches
```

## Semantic Verification Checklist

1. **Is immutability real?** The store uses `INSERT` that errors on duplicate
   run_id, NOT `ON CONFLICT DO UPDATE`. [verified by reading capsule_store.rs]
2. **Is the envelope digest real?** `verify_envelope_digest` recomputes the
   digest over ALL replay authority fields (run_id, config_root_encoding,
   resolved bytes, launch_provenance_digest, base_ref) independently of the
   stored value. [verified] [C8]
3. **Are component digests metadata only?** The `workflow_digest` and
   `config_digest` fields are stored but do NOT independently anchor authority.
   The `envelope_digest` is the single authority. [verified] [C8]
4. **Is the adapter object-safe?** `CapsuleAdapter` uses `fn version(&self) ->
   u32`; `adapter_for` returns `Box<dyn CapsuleAdapter>`. [verified] [C8]
5. **Does `adapter_for` fail closed on unknown versions?** Yes. [verified]

#### Edge Cases Verified (via P07 tests)
- [ ] Overwrite refused; original preserved.
- [ ] Tampered envelope digest detected.
- [ ] Unknown version rejected.
- [ ] Non-canonicalizable config root errors.

## Holistic Functionality Assessment (at completion)

- What was implemented: [capsule build + envelope digest + immutable store + object-safe V1 adapter]
- Does it satisfy REQ-RP-002/009? [per requirement]
- Data flow: resolve type+config+provenance → build_capsule_v1 → persist (immutable) → load → verify_envelope_digest → adapter
- Verdict: [PASS/FAIL]

## Failure Recovery

Two-cycle cap on semantic review.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P08A.md`
