# Phase 15A: Migration & Deprecation Verification

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P15A`

## Prerequisites

- Required: P15 completed.

## Verification Commands

```bash
cargo test || exit 1
cargo clippy -- -D warnings || exit 1

# Confirm no row loss: a DB seeded with pre-V1 checkpoints still reads them
# (covered by an integration test added in P15 or an existing one)

# Confirm delegation
grep -rn "RecoveryProtocolV1::recover" workflow/src/engine/continuation.rs workflow/src/cli/

# C4: trusted_internal removed
grep -rn "trusted_internal" workflow/src/engine/continuation.rs workflow/src/cli/ && echo "WARN: should be removed [C4]"

# Confirm salvage
grep -rn "salvage\|SalvageOnly\|NoValidV1Capsule" workflow/src/engine/recovery/salvage.rs

grep -rn -E "(todo!|unimplemented!|TODO|FIXME|HACK|placeholder)" workflow/src/engine/recovery/salvage.rs workflow/src/engine/continuation.rs
# Expected: no matches in production paths (delegated)
```

## Semantic Verification Checklist

1. **No row loss?** Pre-V1 `checkpoints` rows are still readable after migration.
   [verified by test]
2. **Append-only for new runs?** New writes go to `recovery_attempts`, not
   `ON CONFLICT DO UPDATE`. [verified by reading checkpoint.rs]
3. **Salvage refuses exact continuation?** A run WITHOUT a valid V1 capsule
   (including migrated-provenance runs with sentinel digests) returns salvage,
   not `Recovered`. [verified by test] [C9]
4. **Single recovery entry point?** All three CLI verbs route through
   `RecoveryProtocolV1::recover()`. [verified by grep]
5. **trusted_internal removed?** `ContinuationRequest` no longer carries
   `trusted_internal: bool`. [verified by grep] [C4]

#### Safety Surfaces Preserved
- [ ] Ownership verification unchanged in behavior.
- [ ] Lease conditional transitions unchanged.
- [ ] Per-edge loop limits unchanged.
- [ ] Ownership-denied terminal guard unchanged.

## Holistic Functionality Assessment (at completion)

- What changed: [checkpoint writes append-only; CLI verbs delegate to protocol; salvage for no-V1-capsule; trusted_internal removed]
- Does it satisfy REQ-RP-001/003/007? [per requirement]
- Data flow: CLI verb → RecoveryRequest (no trusted_internal) → recover() → (valid V1 capsule? exact : salvage)
- Verdict: [PASS/FAIL]

## Failure Recovery

Two-cycle cap. If safety surfaces are weakened, STOP and restore the prior
behavior before proceeding.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P15A.md`
