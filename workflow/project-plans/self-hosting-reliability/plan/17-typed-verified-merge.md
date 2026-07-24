# Phase 17: Typed Verified Merge & Durable Merged State

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P17`

## Prerequisites

- Required: P16A completed with PASS.

## Purpose

Implement the typed verified merge: completion requires a `TypedMergeArtifact`
(observed merge + strategy-specific reachability proof [C10]) AND the durable
`RunStatus::Merged` state, committed in a single short `IMMEDIATE` atomic
artifact+status transaction [C11]. A status field alone never satisfies
completion. This closes the viability gate stage "typed merge and reachability
proof".

This phase also introduces the first **writer** of `RunStatus::Merged`. Today
the variant exists and is terminal (`is_terminal()` returns true, and it is in
`TERMINAL_SQL`), but no code path sets it. P17 adds the conditional transition
to `Merged` gated on the typed artifact.

> **The sole implementation pseudocode is `02-pseudocode.md`, typed_merge
> component lines 01–236 (B13). Any illustrative type fragments below are
> explanatory projections and must not override that contract.**

## Requirements Implemented (Expanded)

### REQ-RP-010: Typed verified merge with strategy-specific proof and atomic transaction [C10/C11]
**Behavior**:
- GIVEN: a run whose PR is observed merged with a valid strategy-specific proof
- WHEN: `complete_typed_merge(conn, artifact)` is called
- THEN: external verification happens FIRST (no tx), THEN a single short
       `IMMEDIATE` tx commits BOTH the immutable merge artifact AND the
       conditional `RunStatus::Merged` transition, bound to repo/PR/head/capsule,
       with an explicit allowed predecessor and affected-row check [C11]
- GIVEN: a run whose `RunStatus` is set to `Merged` WITHOUT a typed artifact
- WHEN: completion is checked
- THEN: it is NOT satisfied (artifact required)
- GIVEN: a run whose observed PR is NOT merged
- WHEN: `complete_typed_merge` is called
- THEN: it refuses (external verification fails before any tx) [C11]
- GIVEN: the normal merge-required flow
- WHEN: a merge is in progress
- THEN: the flow does NOT first write Completed (it writes the artifact+status atomically) [C11]

## Strategy-Specific Merge Proof (C10)

The merge proof is a strategy-specific enum. Different merge strategies produce
different ancestry relationships, so the proof records the exact evidence per
strategy. The `result_sha` is strategy-neutral.

```
01 // MergeProof — strategy-specific reachability evidence. [C10]
02 pub enum MergeProof {
03     MergeCommit {
04         merge_commit_sha: String,
05         head_ancestor_check: AncestryCheck,   // head is ancestor of merge_commit
06         base_ancestor_check: AncestryCheck,   // base is ancestor of merge_commit
07     },
08     Squash {
09         squash_commit_sha: String,
10         base_ancestor_check: AncestryCheck,   // base is ancestor of squash_commit
11         content_evidence: ContentEvidence,    // expected vs observed content digest
12     },
13     Rebase {
14         final_head_sha: String,
15         base_ancestor_check: AncestryCheck,   // base is ancestor of final_head
16         patch_evidence: PatchEvidence,        // expected vs observed patch-id
17     },
18 }
19
20 // AncestryCheck — records the exact tested ancestor/descendant pair.
21 pub struct AncestryCheck {
22     tested_ancestor: String,
23     tested_descendant: String,
24     exit_code: i32,   // 0 = confirmed ancestor
25 }
26
27 // ContentEvidence — computed expected/observed content digest (squash). [C10]
28 pub struct ContentEvidence {
29     expected_digest: String,
30     observed_digest: String,
31 }
32
33 // PatchEvidence — computed expected/observed patch-id equivalence (rebase). [C10]
34 pub struct PatchEvidence {
35     expected_patch_id: String,
36     observed_patch_id: String,
37 }
```

| Strategy | Proof variant | Ancestry checks | Additional evidence |
|----------|---------------|-----------------|---------------------|
| **merge commit** | `MergeCommit` | TWO: head→merge_commit AND base→merge_commit | None (ancestry suffices) |
| **squash** | `Squash` | ONE: base→squash_commit | Content digest match (expected vs observed) |
| **rebase** | `Rebase` | ONE: base→final_head | Patch-id equivalence (expected vs observed) |

The verifier runs `git merge-base --is-ancestor <ancestor> <descendant>` for
each recorded `AncestryCheck` and fails closed on a non-zero exit. For
squash/rebase, it additionally verifies the content/patch evidence.

> NOTE: `git merge-base` is already used in the codebase
> (`src/engine/executors/scope_control/task_charter.rs`), but `--is-ancestor` is
> not yet used; P17 introduces it.

## Atomic Artifact+Status Transaction (C11)

```
01 // complete_typed_merge — external verification THEN atomic DB tx. [C11]
02 pub fn complete_typed_merge(conn, artifact) -> Result<(), MergeError> {
03     // PHASE 1: External verification (NO transaction)
04     verify observed pr.merged == true                                // line 04
05     verify_reachability(work_dir, &artifact.merge_proof)?            // line 05
06     compute expected_predecessor = current_run_status                // line 06
07
08     // PHASE 2: Short IMMEDIATE atomic artifact+status transaction
09     let tx = Transaction::new(conn, Immediate)?                      // line 09
10     // Explicit allowed predecessor check
11     let current = run_metadata::read_status(tx, &artifact.run_id)?   // line 11
12     if current != expected_predecessor {                             // line 12
13         tx.rollback()?                                               // line 13
14         return Err(MergeError::AlreadyTerminal)                      // line 14
15     }
16     // Insert immutable merge artifact
17     INSERT INTO merge_artifacts (...) VALUES (...)                    // line 17
18     // Conditional status update with affected-row check
19     let affected = UPDATE run SET status='merged'                    // line 19
20         WHERE run_id = ? AND status = ?expected_predecessor          // line 20
21     if affected != 1 {                                               // line 21
22         tx.rollback()?                                               // line 22
23         return Err(MergeError::AlreadyTerminal)                      // line 23
24     }
25     tx.commit()?                                                     // line 25
26     Ok(())                                                           // line 26
27 }
```

**Idempotent retry**: if the transaction is retried (e.g., after a crash), the
explicit allowed-predecessor check and affected-row check ensure the retry is
safe: if the artifact already exists and status is already `Merged`, the
predecessor check fails cleanly (AlreadyTerminal), not as an error.

## Implementation Tasks

### Files to Create

- `src/engine/recovery/typed_merge/mod.rs`
  - `pub struct TypedMergeArtifact { run_id, pr_number, result_sha, base_sha, head_sha, merge_proof, capsule_envelope_digest, recorded_at }` (bound to capsule [C11])
  - `pub enum MergeProof { MergeCommit { ... }, Squash { ... }, Rebase { ... } }` — strategy-specific [C10]
  - `pub struct AncestryCheck { tested_ancestor, tested_descendant, exit_code }`
  - `pub struct ContentEvidence { expected_digest, observed_digest }` [C10]
  - `pub struct PatchEvidence { expected_patch_id, observed_patch_id }` [C10]
  - `pub fn verify_reachability(work_dir, proof) -> Result<(), MergeError>` — runs
    `git merge-base --is-ancestor` for each `AncestryCheck`; for squash/rebase,
    verifies content/patch evidence. Fails closed on non-zero exit or mismatch.
  - `pub fn complete_typed_merge(conn, artifact) -> Result<(), MergeError>` [C11]:
    external verification (no tx) → short IMMEDIATE atomic artifact+status tx
    with explicit allowed predecessor + affected-row check.
  - `pub fn completion_satisfied(conn, run_id) -> bool`: requires BOTH a typed
    artifact row AND `RunStatus::Merged`.
  - `pub enum MergeError { NotMerged, ReachabilityFailed(String), AlreadyTerminal, ContentMismatch, PatchMismatch }`
  - MUST include: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17`, `/// @requirement:REQ-RP-010`

### Files to Modify

- `src/engine/recovery/mod.rs`
  - `pub mod typed_merge;` + re-exports.
  - ADD: `/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17`

## Verification Commands

```bash
set -euo pipefail
cargo test || exit 1
cargo clippy --workspace --all-targets --all-features -- -D warnings || exit 1
grep -r "@plan:PLAN-20260723-SELFHOST-RELIABILITY.P17" workflow/src/engine/recovery/typed_merge/mod.rs
grep -r "@requirement:REQ-RP-010" workflow/src/engine/recovery/typed_merge/mod.rs

# Status-alone must not satisfy completion
grep -rn "completion_satisfied" workflow/src/engine/recovery/typed_merge/mod.rs

# Reachability uses --is-ancestor with recorded pair
grep -rn "is-ancestor" workflow/src/engine/recovery/typed_merge/mod.rs

# C10: strategy-specific proof enum
grep -rn "MergeProof\|MergeCommit\|Squash\|Rebase" workflow/src/engine/recovery/typed_merge/mod.rs

# C11: atomic artifact+status transaction
grep -rn "Transaction::new.*Immediate\|UPDATE run SET status" workflow/src/engine/recovery/typed_merge/mod.rs
```

## Success Criteria

- `complete_typed_merge` does external verification FIRST, then commits artifact
  + status in a single short IMMEDIATE tx with affected-row check. [C11]
- `completion_satisfied` returns false for a `Merged` status without an artifact.
- `verify_reachability` tests the correct evidence per strategy (merge: two
  ancestry checks; squash: ancestry + content; rebase: ancestry + patch). [C10]
- `result_sha` is strategy-neutral. [C10]
- The normal merge-required flow does NOT first write Completed. [C11]
- Full suite passes.

## Failure Recovery

Two-cycle cap. If reachability verification is hard to implement fully, ship the
observed-merge + artifact + durable-status binding (the safety-critical part)
and record reachability-depth as a follow-up — but never weaken the
artifact-required invariant or the atomic transaction.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P17.md`
