# Execution Tracker — Self-Hosting Reliability

Plan ID: `PLAN-20260723-SELFHOST-RELIABILITY`

## Status Summary

- Total Phases: 20 (00 through 19, sequential, no gaps)
- P0 Prerequisite Milestones: 3 (bounded as Phases 03–05, 06–08, 09–11)
- Completed: 41 (P00 through P19A, including P08B)
- In Progress: 0
- Current Phase: COMPLETE
- Plan Status: **QUALIFIED** — self-hosting viable under the bounded plan scope

## Phase Sequence (MANDATORY: execute in exact order, no skips)

```
P00 → P00A → P01 → P01A → P02 → P02A
  → [Milestone 1] P03 → P03A → P04 → P04A → P05 → P05A
  → [Milestone 2] P06 → P06A → P07 → P07A → P08 → P08A
  → [Milestone 3] P09 → P09A → P10 → P10A → P11 → P11A
  → P12 → P12A → P13 → P13A → P14 → P14A
  → P15 → P15A
  → P16 → P16A
  → P17 → P17A
  → P18 → P18A
  → P19 → P19A
```

## Execution Status

| Phase | ID | Status | Started | Completed | Verified | Semantic? | Notes |
|-------|-----|--------|---------|-----------|----------|-----------|-------|
| 00 | P00 | [x] | 2026-07-23 | 2026-07-23 | PASS | N/A | Overview / spec locked (decisions locked at P00A) |
| 00A | P00A | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | Source assumptions corrected; full locked tests, strict Clippy, formatting, and diff checks passed |
| 01 | P01 | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | Domain analysis + integration map |
| 01A | P01A | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | All requirements mapped; structural and semantic analysis verified |
| 02 | P02 | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | Locked numbered pseudocode after two capped semantic remediation cycles |
| 02A | P02A | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | All ten requirements and dependent-phase references verified |
| 03 | P03 | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | **P0-M1** Epoch + operations ledger + append-only attempts (complete StateSnapshot) + effect intents stub |
| 03A | P03A | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | Stub compiles |
| 04 | P04 | [x] | 2026-07-23 | 2026-07-23 | PASS (RED) | [x] | **P0-M1** 23 real-SQLite epoch/operations/attempt/effect tests |
| 04A | P04A | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | Both targets compile/lint; all 23 failures reach designated P05 stubs |
| 05 | P05 | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | **P0-M1** Durable epoch/operations/attempt/effect state machines implemented |
| 05A | P05A | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | 23 focused + full library/integration suites green; strict quality gates pass |
| 06 | P06 | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | **P0-M2** ExecutionCapsuleV1, store, object-safe adapter, and policy stubs |
| 06A | P06A | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | All targets compile; strict Clippy, markers, and object-safety verified |
| 07 | P07 | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | **P0-M2** 10 ExecutionCapsuleV1 integration-first tests |
| 07A | P07A | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | Valid red: 5 designated P08 stub failures, 5 passing invariants |
| 08 | P08 | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | **P0-M2** Capsule construction, verification, immutable store, V1 adapter impl |
| 08A | P08A | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | Implementation green; M2 blocker resolved by P08B |
| 08B | P08B | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | **P0-M2 closure** Atomic fresh-launch capsule persistence: `persist_launch_atomically` inserts Starting RunMetadata + immutable ExecutionCapsuleV1 in one SQLite IMMEDIATE tx; all fresh callers (CLI, daemon, child) wired; 7 real-SQLite tests; all gates green |
| 09 | P09 | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | **P0-M3** RecoveryProtocolV1 stub: sealed RecoveryAuthority/PreparedRecovery, RecoveryRequest (no auth bool), outcomes/errors/operator verbs; recover stubbed todo!, fail-closed policy/strategy defaults |
| 09A | P09A | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | Stub compiles; all structural/semantic checks pass; ConflictingOperation present |
| 10 | P10 | [x] | 2026-07-23 | 2026-07-23 | PASS (RED) | [x] | **P0-M3** 32 real-SQLite integration-first RecoveryProtocolV1 tests; 16 RED at designated P11 todo!()/fail-closed stubs, 16 GREEN durable/structural invariants |
| 10A | P10A | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | Valid red verified: all failures reach designated P11 recover todo!() or fail-closed policy stubs; compile + strict Clippy + fmt pass |
| 11 | P11 | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | **P0-M3 core** Four phases, exact authority revalidation, single reserve CAS, durable attempt, injected truthful executor, guarded finalize |
| 11A | P11A | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | 46 protocol + full library/integration suites green; strict lint/complexity/source-size gates pass; production reachability deferred to P12-P15 |
| 12 | P12 | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | Capsule-backed RecoveryExecutor wiring skeleton; actual resume surfaces marked; P08B atomic launch preserved |
| 12A | P12A | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | Object-safe adapter/executor seams compile; strict lint/format/diff gates pass; designated P14 stubs fail closed |
| 13 | P13 | [x] | 2026-07-23 | 2026-07-23 | PASS (RED) | [x] | 13 integration-first real-SQLite/tempdir capsule-wiring tests; 10 GREEN invariants, 3 RED at designated P14 stubs (build_instance, build_resume_runner, protocol execute propagation) |
| 13A | P13A | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | Valid RED verified; all failures reach only designated P14 stubs; compile + strict Clippy + fmt pass; no should_panic, no fake success, no network |
| 14 | P14 | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | Adapter wiring impl: `V1Adapter::build_instance` (canonical deserialization of capsule workflow/config bytes + exact run_id), `RunnerRecoveryExecutor::build_resume_runner` (adapter → instance → `with_db_path_and_context` → `execute_step` → truthful `StepOutcome` map). 3 P13 RED tests green; P08B atomic launch and fail-closed unknown/tampered/capsuleless behavior preserved |
| 14A | P14A | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | 13 capsule-wiring + 46 recovery protocol + full library (1353) + all integration targets green; strict all-target/all-feature Clippy, fmt, guard, changed-file lizard all clean; no test modifications, no suppressions |
| 15 | P15 | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | Migration & deprecation: salvage lineage (no capsule = salvage-only [C9/B10]); `trusted_internal` removed from `ContinuationRequest` [C4]; authority derived from durable wait-state; CLI/daemon/child entrypoints construct `RecoveryRequest`-compatible selectors; no parallel legacy execution path from fresh entrypoints; `salvage_lineage` table added (additive, non-destructive); 15 deterministic P15 integration tests |
| 15A | P15A | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | Migration verified: legacy checkpoint rows retained/readable; backfill refused; three CLI verb selectors map correctly; fresh valid capsule executes only protocol; no legacy executor path; ownership/lease/loop/ownership-denied guards unchanged |
| 16 | P16 | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | F1-F14 deterministic real-SQLite/tempdir failpoint matrix; outcome + durable invariant per case; fixed unfinalized-attempt self-source adoption bug |
| 16A | P16A | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | 14/14 failpoints plus full library/integration, strict Clippy, fmt, lizard, marker and diff gates passed |
| 17 | P17 | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | Typed verified merge, ReviewReady predecessor, production PrMerge wait, exact strategy/base/identity binding, atomic artifact+Merged CAS |
| 17A | P17A | [x] | 2026-07-23 | 2026-07-23 | PASS | [x] | 38 typed merge + 8 orchestration tests; full sequential suite and strict quality gates passed |
| 18 | P18 | [x] | 2026-07-24 | 2026-07-24 | PASS | [x] | **Canary harness** — 7 deterministic end-to-end tests over production RecoveryProtocolV1/persistence/capsule/typed-merge; three consecutive mixed canaries (MergeCommit/Squash/Rebase) traversing all nine viability-gate stages with zero invariant violations |
| 18A | P18A | [x] | 2026-07-24 | 2026-07-24 | PASS | [x] | Canary verified: 7 harness + 14 failpoint + 38 typed merge tests green; strict Clippy/fmt/lizard all clean; REQ-QUAL-001/002 satisfied |
| 19 | P19 | [x] | 2026-07-24 | 2026-07-24 | PASS | [x] | Qualification report: every metric measured, three canaries, zero prohibited escapes, 14/14 failpoints, strict gates green |
| 19A | P19A | [x] | 2026-07-24 | 2026-07-24 | PASS — QUALIFIED | [x] | Final bounded self-hosting viability gate verified; reconstruction safety regression restored before qualification |

## P0 Prerequisite Milestone Gates

- [x] **Milestone 1 (P03–P05)**: durable append-only attempt rows (with complete
  `StateSnapshot`, capsule binding, and snapshot digest [C3]) + generalized
  `effect_intents` state machine table [C7] + distinct durable recovery epoch
  with CAS claim [C1] + `recovery_operations` ledger [C2] exist and are durable
  from the start (no in-memory facade); a stale epoch is rejected via CAS
  affected-row check; a re-issued recovery is a verified no-op against the real
  SQLite tables.
- [x] **Milestone 2 (P06–P08)**: `ExecutionCapsuleV1` with ONE envelope digest
  over ALL replay authority fields [C8] is persisted immutably at fresh launch
  via `persist_launch_atomically` (one SQLite IMMEDIATE transaction inserting
  both the initial Starting RunMetadata and the immutable capsule); an attempt
  to mutate a persisted capsule is rejected. Duplicate run ID or capsule causes
  full rollback. Adapter is object-safe (`fn version(&self)`) [C8].
- [x] **Milestone 3 core (P09–P11)**: `RecoveryProtocolV1` typed abstraction
  exists, passes its integration tests, consumes the durable epoch, operation
  ledger, append-only store, and capsule store directly (no in-memory facade),
  uses prepare → reserve → execute → finalize [C5], executes through a truthful
  injected executor outside writer transactions, and cannot return `Recovered`
  before finalize [C12]. Production capsule-backed executor wiring and removal
  of legacy resume/retry/rewind entry paths are the explicit P12–P15 integration
  closure; the fail-closed default prevents unsupported production execution.

All three protocol-core milestone gates are checked; P12 may begin. Production
reachability is not qualified until P12–P15 complete.

## Review-Cycle Cap

Per phase, no more than **two cycles** of each review/remediation type:
structural review, semantic review, and integration review. After two cycles,
stop expanding scope, record the residual gap, and get the scoped change over
the line (defer follow-ups). Track cycles here:

| Phase | Review Type | Cycle 1 | Cycle 2 | Capped |
|-------|-------------|---------|---------|--------|
| P02/P02A | structural | complete | complete | yes |
| P02/P02A | semantic | complete | complete | yes |
| P02/P02A | integration | deferred to P03–P17 | _ | no |

## Completion Markers

- [x] All phases have `@plan:PLAN-20260723-SELFHOST-RELIABILITY.P##` markers in code.
- [x] All requirements have `@requirement:REQ-*` markers in tests.
- [x] Verification script passes for every phase.
- [x] No phases skipped.
- [x] Three consecutive mixed canaries passed (P18).
- [x] Qualification gate met: zero prohibited escapes (P19).

## Remediation Log

[To be filled during execution. Each entry: phase, review type, cycle, issue,
action, result.]

| Phase | Review Type | Cycle | Issue | Action | Result |
|-------|-------------|-------|-------|--------|--------|
| P02/P02A | semantic | 1 | 13 blocking design corrections: epoch vs generation, recovery_operations ledger, complete StateSnapshot in attempts, no trusted_internal bool, phased tx model, canonical StepDef policy, effect-intent state machine, capsule envelope digest + object-safe adapter, legacy salvage, strategy-specific merge proof, atomic artifact+status tx, protocol owns phases, P16 expectations | Rewrote 02-pseudocode.md with numbered pseudocode for 8 components (recovery_epoch, recovery_operations, attempts, capsule, adapter, policy, protocol, effect_intents, salvage, typed_merge). Updated specification.md (REQ-RP-001 through REQ-RP-010, architectural decisions, project structure, integration points, atomicity boundary, constraints). Updated 02a-pseudocode-verification.md with per-correction checklists. Synchronizing P03–P17 tasks/tests/references. | IN PROGRESS — pseudocode and specification rewritten; dependent phases pending sync |
| P02/P02A | semantic | 2 | Exact authority, one reserve CAS, intent-bound/race-safe operation identity, pre-effect attempt identity, idempotent effect preparation, retained workspace anchor, canonical policy schema, actual launch surfaces, framed versioning, no legacy backfill, authoritative merge probes, merge DDL/status semantics, and exact traceability | Synchronized specification and P02–P17; corrected first-insert epoch CAS and duplicate execution ownership during final verification | PASS — semantic review cap reached; design locked |
