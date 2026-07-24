# Phase 02A: Pseudocode Verification (remediation cycles 1 + 2)

## Phase ID

`PLAN-20260723-SELFHOST-RELIABILITY.P02A`

## Prerequisites

- Required: P02 completed.

## Purpose

Verify the pseudocode is internally consistent, covers every requirement, and is
referenced by the implementation phases that claim it. This verification covers
the 13 design corrections applied in remediation cycle 1 (`[C1]`–`[C13]`) and
the 13 refinements applied in remediation cycle 2 (`[B1]`–`[B13]`).

## Requirements Coverage

| Requirement | Pseudocode Component | Lines | Corrections |
|-------------|----------------------|-------|-------------|
| REQ-RP-001 (single typed abstraction) | `RecoveryProtocolV1::recover()` | protocol 72–124 | C5, C12, B1 |
| REQ-RP-002 (immutable capsule) | `ExecutionCapsuleV1` + envelope digest | capsule 01–113 | C8, B9, B10 |
| REQ-RP-003 (append-only IDs) | attempts `record_attempt_start` + `append_attempt_outcome` | attempts 23–93 | C3, B4 |
| REQ-RP-004 (epoch fence + idempotent) | epoch CAS + recovery_operations | epoch 17–41, operations 22–149, protocol 180–271 | C1, C2, B2, B3, B4 |
| REQ-RP-005 (step recovery policy) | `policy_for_step` + `select_strategy` | policy 23–56 | C6, B7 |
| REQ-RP-006 (ContinueWorkspace exact verify) | protocol execute + reserve phases | protocol 285–293, 189–213 | C4, C5, B6 |
| REQ-RP-007 (legacy salvage) | salvage classification + recover | salvage 07–42 | C9, B10 |
| REQ-RP-008 (persisted intents) | `effect_intents` state machine | intents 34–197 | C7, B5 |
| REQ-RP-009 (versioned adapter) | `CapsuleAdapter` object-safe trait | adapters 01–20 | C8, B9 |
| REQ-RP-010 (typed verified merge) | `TypedMergeArtifact` + proof + complete | typed_merge 02–236 | C10, C11, B11, B12 |

## Design Correction Verification — Cycle 1 (13 corrections `[C1]`–`[C13]`)

### [C1] Distinct durable recovery epoch
- [x] Recovery epoch is a distinct durable row, not `MAX(attempt generation)`
      (epoch lines 01–06).
- [x] CAS claim with affected-row check (epoch lines 17–36).
- [x] No synthetic attempts to bump generation.

### [C2] recovery_operations ledger
- [x] Stable `operation_id` idempotency key (operations lines 22–28).
- [x] Exact request/capsule/source-attempt binding (operations lines 61–79).
- [x] Pending/Completed/Refused/Conflict states (operations line 10, 101–130).
- [x] Serialized prior outcome on Completed (operations lines 101–112).
- [x] Exact completed duplicate returns prior outcome (protocol lines 84–90).
- [x] Pending duplicate executes/reconciles only after guarded lease adoption;
      a still-owned duplicate returns `InProgress` (protocol lines 100–102,
      244–262).
- [x] Conflicting logical-request binding refuses (protocol lines 91–94,
      239–243).

### [C3] Append-only attempts with complete StateSnapshot
- [x] Complete `StateSnapshot` stored as `state_snapshot_json` (attempts
      line 11).
- [x] Immutable `source_attempt_id` parent (attempts line 06).
- [x] Step ID/status (attempts lines 07–08A).
- [x] Capsule schema + envelope digest (attempts lines 09–10).
- [x] Snapshot/checkpoint digest (attempts lines 12–13).

### [C4] No trusted_internal bool; sealed RecoveryAuthority
- [x] `RecoveryRequest` has no `trusted_internal` field (protocol lines 03–08).
- [x] Sealed `RecoveryAuthority` derived from durable state +
      `WorkspaceAuthorization` (protocol lines 13–20, 166–167).
- [x] Descriptor-bound authorization revalidated in CAS transaction (protocol
      lines 208–213).

### [C5] Phased transaction model
- [x] Prepare outside writer tx (protocol lines 130–175).
- [x] Reserve in short IMMEDIATE tx (protocol lines 180–271).
- [x] Execute with no tx (protocol lines 276–313).
- [x] Finalize in short IMMEDIATE tx (protocol lines 319–357).

### [C6] Policy from canonical StepDef + SAFE_RERUN_STEPS
- [x] Consumes `StepDef` (policy line 23).
- [x] Explicit declaration takes precedence (policy lines 25–27).
- [x] `SAFE_RERUN_STEPS` classification by step_id (policy lines 29–31).
- [x] Generic shell/write_file defaults `NonRecoverable` (policy lines 33–35).
- [x] No `trusted_internal` parameter in `select_strategy` (policy line 47).

### [C7] Complete effect-intent state machine
- [x] Stable unique effect key (intents lines 20–27).
- [x] Operation/attempt/sequence binding (intents lines 04–06).
- [x] Canonical payload + digest/version (intents lines 08–10).
- [x] Expected target/predecessor (intents lines 11–12).
- [x] Observed result (intents line 13).
- [x] Prepared/Completed/Conflict (intents line 14).
- [x] Committed before effect (intents lines 34–71).
- [x] Effect-specific external reconciliation (intents lines 87–90).
- [x] Guarded finalize (intents lines 142–151).
- [x] Call-site phases wired: commit/push/open_pr/merge (intents lines 176–197).

### [C8] Capsule envelope digest + object-safe adapter
- [x] Canonicalization/schema/domain version (capsule lines 04–06).
- [x] ONE envelope digest over ALL replay authority fields (capsule lines 16,
      77–78, 97–98).
- [x] Component digests are metadata (capsule lines 18–19).
- [x] `LaunchProvenance` canonical digest included in envelope (capsule line 13).
- [x] Adapter object-safe via `fn version(&self)` (adapters line 03).

### [C9] Legacy salvage
- [x] Every run without valid V1 capsule is salvage-only (salvage lines 16–21).
- [x] Applies regardless of provenance/migration source (salvage lines 18–20).
- [x] Immutable salvage lineage (salvage lines 36–37, 51–55).
- [x] Exact recovery refuses (salvage line 39).

### [C10] Strategy-specific merge proof enum
- [x] Merge commit has two ancestry checks (typed_merge lines 32–38).
- [x] Squash includes ancestry + content evidence (typed_merge lines 39–47).
- [x] Rebase includes ancestry + patch evidence (typed_merge lines 48–57).
- [x] `result_sha` strategy-neutral (typed_merge line 05).

### [C11] Typed merge atomic artifact+status transaction
- [x] External verification before tx (typed_merge lines 147).
- [x] Short IMMEDIATE atomic artifact+status transaction (typed_merge lines
      152–198).
- [x] Bound to repo/PR/head/capsule (typed_merge lines 06–09, 179–180).
- [x] Explicit allowed predecessor (typed_merge line 178).
- [x] Affected-row check (typed_merge lines 161, 181, 184).
- [x] Exact idempotent retry (typed_merge lines 160, 164–170, 187–191).
- [x] Normal merge-required flow must NOT first write Completed (typed_merge
      lines 145, 175–178).

### [C12] Protocol owns all phases; cannot return Recovered early
- [x] Protocol owns prepare/reserve/execute/finalize (protocol lines 72–124).
- [x] Cannot return `Recovered` before finalize commits (protocol lines 110–119,
      332–334).

## Design Correction Verification — Cycle 2 (13 refinements `[B1]`–`[B13]`)

### [B1] Exact authority preservation in PreparedRecovery
- [x] `PreparedRecovery` preserves exact run/status/current-step/live-PID/
      checkpoint/wait/lease authority (protocol lines 31–37).
- [x] Authority captured during prepare (protocol lines 159–165).
- [x] Reserve reselects/revalidates each before mutation (protocol lines
      189–198).

### [B2] Single caller expected_epoch, single reserve CAS
- [x] `RecoveryRequest` carries `expected_epoch` (protocol line 06).
- [x] Single epoch CAS at reserve only (protocol line 215).
- [x] No CAS at finalize (protocol lines 325–357 contain no CAS).

### [B3] Logical request key vs. exact operation ID; guarded owner/lease
- [x] `compute_logical_request_key` is separate from `compute_operation_id`,
      and both bind normalized operator intent (operations lines 22–41).
- [x] `logical_request_key` is unique; reserve loads by that key and compares
      exact intent/capsule/step/source bindings before duplicate handling.
- [x] `Pending` operation has `owner_pid` + `lease_expires_at` guarded claim
      (operations lines 11–12, 61–79).
- [x] `find_adoptable_pending` + `try_adopt_pending` provide lease-expiry
      adoption; still-owned duplicates do not execute (operations lines 50–55,
      83–97; protocol lines 100–102, 256–262).

### [B4] Durable execution_attempt_id at reserve; outcome append at finalize
- [x] `record_attempt_start` allocates `execution_attempt_id` at reserve
      (attempts lines 23–42).
- [x] Attempt-start row has `finalized_at = NULL` (attempts lines 16, 40).
- [x] `append_attempt_outcome` appends immutable outcome at finalize (attempts
      lines 48–65).
- [x] `runner_result_json` field for crash recovery (attempts lines 14, 108).
- [x] `load_unfinalized_for_operation` recovers after crash (attempts
      lines 80–84).

### [B5] Insert-or-load exact-binding effect prepare
- [x] `prepare_effect` is insert-or-load (intents lines 47–70).
- [x] Exact-binding comparison: payload_digest + expected_target +
      expected_predecessor (intents lines 60–62).
- [x] Mismatch transitions to conflict state (intents lines 64–65).
- [x] Match returns existing intent (intents lines 67–68).
- [x] Merge intent completed/conflicted in atomic merge transaction (intents
      lines 133–137).

### [B6] Retained VerifiedWorkspace anchor
- [x] `PreparedRecovery` owns retained `VerifiedWorkspace` (protocol lines 39,
      144–150).
- [x] Anchor obtained via `adjudicate_workspace_ownership` kernel (protocol
      line 144).
- [x] `OwnershipVerdict::Owned(VerifiedWorkspace)` match (protocol lines
      145–150).
- [x] `verified_workspace.authorization()` for descriptor-bound auth (protocol
      line 151).
- [x] Descriptor-relative marker re-snapshot (protocol lines 201–202).
- [x] Exact descriptor identity comparison (protocol lines 208–210).

### [B7] StepDef.recovery_policy in capsule milestone
- [x] `recovery_policy: Option<StepRecoveryPolicy>` field declared (policy
      lines 11–18).
- [x] Added in capsule milestone (P06–P08) before protocol (policy line 11).
- [x] Persisted in canonical workflow bytes (policy line 17).
- [x] Canonicalizer includes the field (policy line 18).

### [B8] Actual launch/resume surface mapping
- [x] Specification integration points map `app/run.rs`,
      `app/daemon_run.rs`, `parent_orchestration/child_workflow.rs`/
      `child_run.rs`, `app/runs/continuation_execution.rs`.
- [x] Capsule-before-run-row atomic ordering stated.
- [x] P14 implementation phase cites these exact surfaces.

### [B9] Framed canonical envelope byte format
- [x] `build_envelope_frame` produces framed bytes (capsule lines 43–56).
- [x] Fixed-width version header: schema/canonicalization/domain/provenance
      (capsule lines 45–48).
- [x] Length-prefixed authority fields (capsule lines 49–54).
- [x] `SUPPORTED_*_VERSIONS` constants (capsule lines 25–28).
- [x] Fail-closed version dispatch in `verify_envelope_digest` (capsule lines
      85–96).
- [x] Fail-closed adapter dispatch (adapters line 13).

### [B10] No historical capsule backfill
- [x] Backfill prohibited (salvage lines 04–05, 20).
- [x] Every run without capsule is salvage-only forever (salvage line 21).

### [B11] Authoritative injected merge verifier
- [x] `MergeGitProbe` trait (typed_merge lines 63–70).
- [x] `MergeRemoteProbe` trait (typed_merge lines 74–77).
- [x] `MergeVerifier` with injected probes + bound identity (typed_merge lines
      87–95).
- [x] `build_reachability_proof` computes evidence via probes (typed_merge lines
      98–134).

### [B12] Merge artifact DDL + fixed predecessor
- [x] `merge_artifacts` DDL (typed_merge lines 14–26).
- [x] Exact artifact equality on conflict (typed_merge lines 164–170).
- [x] Capsule lookup/join via `capsule_envelope_digest` (typed_merge lines 22,
      211–217).
- [x] Fixed allowed predecessor `ReviewReady` (typed_merge lines 138–139B).
- [x] Runner completion semantics for merge-required (typed_merge lines
      221–225).

### [B13] Sole pseudocode source
- [x] `02-pseudocode.md` is the sole pseudocode source (stated at top).
- [x] P05/P08/P11/P14/P15/P17 reference exact component-local line numbers.
- [x] No phase doc carries alternate conflicting pseudocode.

## Verification Checklist

- [x] Every REQ-RP-* maps to a pseudocode block with line numbers.
- [x] The epoch CAS is inside the IMMEDIATE transaction with affected-row check.
- [x] The epoch CAS is the ONLY CAS in the protocol (reserve only). [B2]
- [x] The idempotency ledger returns `AlreadyApplied` (Completed duplicate) or
      reconciles (Pending duplicate, via guarded owner/lease claim [B3]) or
      refuses (Conflict duplicate).
- [x] `policy_for_step` consumes `StepDef` and `SAFE_RERUN_STEPS`; generic
      shell/write_file defaults to `NonRecoverable`.
- [x] `RecoveryRequest` has no `trusted_internal` bool; `RecoveryAuthority` is
      sealed and descriptor-bound.
- [x] `PreparedRecovery` preserves exact authority and revalidates in reserve.
      [B1]
- [x] `PreparedRecovery` owns a retained `VerifiedWorkspace` anchor. [B6]
- [x] Effect intent is recorded BEFORE issuance with stable key, insert-or-load
      exact-binding comparison, and guarded finalize. [B5]
- [x] The capsule has ONE envelope digest over a framed canonical envelope;
      component digests are metadata. [B9]
- [x] The adapter is object-safe via `fn version(&self)` with fail-closed
      dispatch. [B9]
- [x] Durable `execution_attempt_id` is allocated at reserve; outcome appended
      at finalize. [B4]
- [x] Typed merge requires injected-probe evidence, strategy-specific
      reachability proof, and atomic artifact+status transaction with fixed
      predecessor `ReviewReady`. [B11/B12]
- [x] Every RecoveryStrategy execution is completed (execute phase dispatches
      all variants).
- [x] Legacy salvage refuses exact recovery for any run without a valid V1
      capsule; backfill is prohibited. [B10]
- [x] No placeholder branches in locked pseudocode (every match arm is
      concrete).
- [x] No claims of DB/remote atomicity (external verification is separate from
      the IMMEDIATE tx; the tx covers only DB rows).

## Semantic Verification Questions

1. If the epoch is stale, does the protocol reject WITHOUT mutating durable
   state? [protocol lines 233–235 — yes, rollback then return `StaleEpoch`]
2. If the same operation_id is submitted twice and the first is Completed, does
   the second return the prior outcome? [protocol lines 84–90 — yes,
   `AlreadyApplied`]
3. If the same operation is submitted twice while Pending, can both callers
   execute? [No — protocol lines 244–263 require guarded expired-lease adoption;
   a still-owned operation returns `InProgress` without executing [B3]]
4. Is the capsule ever mutated after first write? [No — envelope digest
   verified on load; component digests are metadata]
5. Does `ContinueWorkspace` revalidate the workspace authorization inside the
   CAS transaction (TOCTOU guard)? [protocol lines 201–213 — yes, via retained
   `VerifiedWorkspace` anchor [B6]]
6. Can `recover()` return `Recovered` before `finalize_recovery` commits?
   [No — protocol line 110 calls finalize, and `Recovered` is only returned
   after `FinalizeResult::Finalized` (lines 113–119)]
7. Is the epoch CAS the ONLY CAS in the protocol? [Yes — single CAS at reserve
   (line 215); no CAS at finalize (lines 325–357) [B2]]
8. Does the merge reachability proof record exact ancestry per strategy and
   avoid assuming the merged SHA is an ancestor of the final head?
   [typed_merge lines 30–57 — yes, per-strategy variants with exact pairs]
9. Does the typed merge transaction verify the affected-row count and handle
   idempotent retry? [typed_merge lines 161, 181, 184–196 — yes]
10. Is the effect-intent state machine insert-or-load with exact-binding
    comparison? [intents lines 47–70 — yes [B5]]
11. Is a run with a migrated `LaunchProvenance` (sentinel digests) but no V1
    capsule salvage-only? [salvage lines 18–21 — yes, classification depends
    on V1 capsule existence, not provenance; backfill prohibited [B10]]
12. Is a durable `execution_attempt_id` allocated at reserve before any effect?
    [attempts lines 23–42, protocol lines 219–223 — yes [B4]]
13. Does the merge verifier compute evidence via injected probes, not ambient
    shell? [typed_merge lines 98–134 — yes [B11]]
14. Does the merge transaction use fixed predecessor `ReviewReady`? [typed_merge
    lines 138–139B, 178 — yes [B12]]

## Self-Verification Status

**PASS.** The specification and pseudocode are internally consistent, every
requirement has numbered pseudocode, and P05/P08/P11/P14/P15/P17 cite the
applicable component-local ranges in this sole pseudocode source. Both allowed
semantic review/remediation cycles are complete; implementation must follow the
locked contracts without further architecture expansion.

## Failure Recovery

If pseudocode is inconsistent: update `02-pseudocode.md`, re-verify.

## Phase Completion Marker

Create: `project-plans/self-hosting-reliability/plan/.completed/P02A.md`
