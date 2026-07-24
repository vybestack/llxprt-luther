# Phase 14A: Capsule Adapter Wiring Implementation Verification

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P14A`

## Prerequisites

- Required: P14 completed.

## Verification Commands

```bash
cargo test || exit 1
cargo clippy -- -D warnings || exit 1
grep -rn -E "(todo!|unimplemented!|TODO|FIXME|HACK|placeholder|not yet|will be)" workflow/src/engine/runner.rs workflow/src/main.rs workflow/src/engine/recovery/adapters/
# Expected: no matches in wiring code
```

## Semantic Verification Checklist

1. **Is the capsule persisted before any step?** Launch persists before
   `EngineRunner::run()`. [verified by reading main.rs]
2. **Is resume adapter-driven?** `resume_from_checkpoint` (private) loads the
   capsule and reconstructs via `Box<dyn CapsuleAdapter>`, not ad-hoc. External
   launch/resume surfaces reconstruct the capsule-backed context then invoke
   `EngineRunner::run`. [verified] [C8]
3. **Is the envelope digest verified on resume?** `verify_envelope_digest` is
   called before instance reconstruction. [verified] [C8]
4. **Are existing run paths unbroken?** Full `cargo test` passes.

#### Integration Points Verified
- [ ] Launch surface → `build_capsule_v1` → `persist_capsule_v1` → `EngineRunner`.
- [ ] Resume surface → `load_capsule_v1` → `verify_envelope_digest` → `adapter_for` → instance. [C8]

#### Edge Cases Verified (via P13 tests)
- [ ] Tampered envelope digest refuses resume. [C8]
- [ ] Unknown version refuses resume.
- [ ] Base ref honored.

## Holistic Functionality Assessment (at completion)

- What was implemented: [launch persists capsule; resume loads+verifies+dispatches object-safe adapter]
- Does it satisfy REQ-RP-002/009? [per requirement]
- Data flow: launch resolve → build capsule → persist → run; resume → load → verify_envelope_digest → adapter → instance → run
- Verdict: [PASS/FAIL]

## Failure Recovery

Two-cycle cap.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P14A.md`
