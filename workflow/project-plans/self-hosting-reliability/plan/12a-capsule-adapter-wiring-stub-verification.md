# Phase 12A: Capsule Adapter Wiring Stub Verification

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P12A`

## Prerequisites

- Required: P12 completed.

## Verification Commands

```bash
set -euo pipefail
cargo build --all-targets || exit 1
cargo clippy --workspace --all-targets --all-features -- -D warnings || exit 1
grep -r "@plan:PLAN-20260723-SELFHOST-RELIABILITY.P12" workflow/src/engine/runner.rs workflow/src/main.rs
```

## Structural Verification Checklist

- [x] `resume_from_checkpoint()` has a marked call site for capsule load +
      `verify_envelope_digest` + adapter. [C8]
- [x] Fresh-launch path has a marked call site for capsule build + persist.
- [x] No `// TODO` comments (todo!() macro OK in stub).
- [x] Existing tests still compile (no signature break yet — P14 may change signatures).

## Failure Recovery

Two-cycle cap.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P12A.md`
