# Phase 07A: Execution Capsule TDD Verification (Red Phase)

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P07A`

## Prerequisites

- Required: P07 completed.

## Verification Commands

```bash
cargo test --test execution_capsule_integration_tests 2>&1 | tail -20
grep -rn "should_panic" workflow/tests/execution_capsule_integration_tests.rs && echo "FAIL"
```

## Structural Verification Checklist

- [ ] 10+ tests exist, tagged `@plan:...P07`.
- [ ] No `#[should_panic]`.
- [ ] Tests assert real `ExecutionCapsuleV1` / envelope digest / adapter outcomes.

## Semantic Verification Checklist

1. Does the immutability test assert the OVERWRITE fails AND the load returns
   the original? [yes/no]
2. Does the envelope digest test recompute independently (not just call the
   capsule's own digest)? [yes/no] [C8]
3. Does the envelope digest test cover ALL replay authority fields (run_id,
   config_root_encoding, resolved bytes, launch_provenance_digest, base_ref)?
   [yes/no] [C8]
4. Would the version-dispatch test FAIL if `adapter_for` returned V1 for any
   version? [yes/no]
5. Does the object-safety test confirm `adapter_for` returns `Box<dyn
   CapsuleAdapter>` and `.version()` is callable on the trait object? [yes/no] [C8]

## Failure Recovery

Two-cycle cap on semantic review.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P07A.md`
