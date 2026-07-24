# Phase 13A: Capsule Adapter Wiring TDD Verification (Red Phase)

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P13A`

## Prerequisites

- Required: P13 completed.

## Verification Commands

```bash
cargo test --test capsule_wiring_integration_tests 2>&1 | tail -20
grep -rn "should_panic" workflow/tests/capsule_wiring_integration_tests.rs && echo "FAIL"
```

## Structural Verification Checklist

- [ ] 7+ tests tagged `@plan:...P13`.
- [ ] No `#[should_panic]`.
- [ ] Tests exercise launch→persist→load→adapter→run end to end.

## Semantic Verification Checklist

1. Does the fresh-launch test assert a capsule row exists BEFORE any step event? [yes/no]
2. Does the resume test assert the object-safe adapter was used
   (`Box<dyn CapsuleAdapter>`, `.version()` callable), not the ad-hoc
   reconstruction? [yes/no] [C8]
3. Does the tampered-digest test assert resume refused with no step executed? [yes/no]
4. Would the unknown-version test FAIL if the adapter always returned V1? [yes/no]

## Failure Recovery

Two-cycle cap.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P13A.md`
