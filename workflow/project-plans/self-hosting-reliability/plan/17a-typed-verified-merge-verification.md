# Phase 17A: Typed Verified Merge Verification

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P17A`

## Prerequisites

- Required: P17 completed.

## Verification Commands

```bash
set -euo pipefail
cargo test || exit 1
cargo clippy --workspace --all-targets --all-features -- -D warnings || exit 1
grep -rn -E "(todo!|unimplemented!|TODO|FIXME|HACK|placeholder)" workflow/src/engine/recovery/typed_merge/mod.rs && { echo "FAIL: placeholder tokens found"; exit 1; } || true
# Expected: no matches in completion path
```

## Semantic Verification Checklist

1. **Does completion require BOTH artifact AND status?** `completion_satisfied`
   returns false when only one is present. [verified by test]
2. **Does `complete_typed_merge` refuse a non-merged PR?** Yes → `NotMerged`
   (before any transaction). [verified] [C11]
3. **Is the artifact immutable?** INSERT only; no overwrite. [verified]
4. **Is the status transition conditional with affected-row check?** Only
   non-terminal runs move to `Merged`; `affected != 1` → `AlreadyTerminal`.
   [verified] [C11]
5. **Is external verification separate from the transaction?** Reachability +
   observed-merge checks happen BEFORE the IMMEDIATE tx opens. The tx commits
   only DB rows. [verified] [C11]
6. **Is the merge proof strategy-specific?** `MergeReachabilityProof` is an enum
   with `MergeCommit` (two ancestry checks), `Squash` (ancestry + content
   evidence), `Rebase` (ancestry + patch evidence). `result_sha` is
   strategy-neutral. [verified] [C10]
7. **Does the normal merge-required flow avoid first writing Completed?** The
   flow writes artifact+status atomically; no intermediate Completed write.
   [verified] [C11]

### Safety Surfaces Preserved

- [x] `RunStatus::Merged` remains terminal and its SQL guard
      (`TERMINAL_SQL`) is unchanged.
- [x] PR-binding identity is unchanged.
- [x] Artifact is bound to repo/PR/head/capsule. [C11]

## Holistic Functionality Assessment

- What was implemented: typed merge artifact + strategy-specific proof + atomic artifact+status tx
- Does it satisfy REQ-RP-010? PASS
- Data flow: external verify (no tx) → short IMMEDIATE tx (artifact INSERT + conditional status UPDATE with affected-row check) → completion_satisfied
- Verdict: PASS

## Failure Recovery

Two-cycle cap.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P17A.md`
