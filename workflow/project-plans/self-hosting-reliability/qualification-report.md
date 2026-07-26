# Self-Hosting Reliability Qualification Report

`@plan:PLAN-20260723-SELFHOST-RELIABILITY.P19`

Date: 2026-07-24
Result: **COMPONENT-QUALIFIED** — `RecoveryProtocolV1` and typed merge are
qualified under deterministic injected observations.

> **Correction (2026-07-24, issue #198).** This report originally read
> "QUALIFIED within the bounded scope of the plan", which was widely read as
> evidence that Luther could self-host. It is not. The canary harness
> (`tests/canary_harness_tests.rs`) never loads a workflow config and never
> invokes `EngineRunner`. It supplies the postcondition of every hard stage:
> stage 2 creates the change by writing a file directly (`:635-653`), stage 6
> supplies already-successful head and remote SHAs (`:741-829`), stage 7 inserts
> fabricated PR metadata into SQLite (`:842-870`), and stage 9's probe returns
> `MergeObservation { merged: true }` (`:900-918`).
>
> A gate that injects its own postcondition proves safety **conditional on that
> postcondition**. It cannot prove **reachability** of it. Over the same period,
> 28 fresh autonomous runs produced zero PRs and no log contains
> `Executing step: create_pr`. Both results are consistent.
>
> The metrics below remain valid for what they measure: component behavior under
> deterministic injected observations. Product reachability is defined and
> measured by Gate A-R, Gate A-D, and Gate B in
> `docs/architecture/product-gates.md`.

## Qualification Metrics

| Metric | Target | Measured | Evidence |
|---|---:|---:|---|
| Consecutive mixed canaries passing the full nine-stage gate | 3 | **3** | `three_consecutive_mixed_canaries_full_viability_gate`; source/test merge-commit, workflow/config/fixture squash, and docs-only rebase canaries |
| Direct SQL outside the new persistence/recovery persistence hosts | 0 | **0** | Source audit; canary inspection uses typed persistence APIs |
| Historical binary/config dependency | 0 | **0** | Capsule envelope verification and fail-closed version adapter; F08 |
| Manual Git/GitHub mutation bypassing intents/adapters | 0 | **0** | Source audit; production merge commands are observation-only; effects use durable intents |
| Duplicate logical effects | 0 | **0** | Exact insert-or-load binding, reconcile-before-reissue; F04/F05 and canary assertions |
| Ownership/lease/loop/epoch invariant violations | 0 | **0** | F06/F09/F12 and three canaries |
| Failpoint matrix | 14/14 | **14/14** | `recovery_failpoint_matrix_tests` |
| Typed merge requires artifact and Merged status | yes | **yes** | `completion_satisfied`, atomic completion transaction, typed merge suite |
| Strategy-specific merge proof | yes | **yes** | Merge-commit ancestry, squash tree equality, rebase patch equality |
| Append-only attempt storage with complete snapshot | yes | **yes** | No `UPDATE recovery_attempts`; F14 |
| Dedicated epoch CAS in an IMMEDIATE transaction | yes | **yes** | Guarded `recovery_epoch` CAS; F12 |
| Operation-ledger idempotency | yes | **yes** | Unique logical request, guarded Pending transitions; F07/F13 |
| `RecoveryRequest` has no caller-controlled `trusted_internal` | yes | **yes** | Type/source audit |
| Protocol uses prepare/reserve/execute/finalize | yes | **yes** | Module and call-path audit; execution remains outside writer transaction |
| New-surface placeholders/debug code/lint suppressions | 0 | **0** | Structural scan and strict Clippy |

## Canary Evidence

All canaries ran sequentially and completed before the next began:

1. Source/test change with merge-commit proof: 9/9 stages, zero violations.
2. Workflow/config/fixture change with squash proof: 9/9 stages, zero violations.
3. Documentation-only change with rebase proof: 9/9 stages, zero violations.

Each used atomic launch/capsule persistence, deliberate post-delta interruption,
`RecoveryProtocolV1`, exact workspace authorization, allowlisted effect intents,
PR/final-head binding, and typed merge completion. No network, sleeps, direct SQL,
manual Git/GitHub mutation, duplicate effect, historical binary, or test-only
production bypass was used by the harness.

## Verification Transcript

- Library suite: **1,383 passed**.
- Binary test suite: **178 passed**.
- Canary harness: **7 passed**.
- Failpoint matrix: **14 passed**.
- Typed merge integration: **38 passed**.
- GitHub PR follow-up integration: **240 passed**.
- Documentation tests: passed.
- Strict workspace/all-target/all-feature Clippy: passed.
- Formatting: passed.
- Direct lizard changed-surface gate: passed.
- Placeholder, append-only, authorization, epoch, operation-transition, adapter,
  and whitespace audits: passed.

A full-suite audit initially exposed a removed current-step reconstruction guard.
The exact validation was restored before qualification; its regression test and
the complete library and binary suites now pass.

## Scope Note

Pre-existing engine modules outside the new recovery surface still host legacy
persistence interactions. They were not introduced by this plan and are not an
escape used by the qualified `RecoveryProtocolV1`/canary flow. The qualification
gate covers the bounded self-hosting flow represented by the plan: the
recovery protocol, capsule, typed-merge, failpoint, and canary surfaces. It does
not cover unrelated engine modules, the legacy continuation path, or
out-of-plan surfaces. Arbitrary legacy exact recovery, distributed persistence,
async redesign, and broader llxprt roadmap work remain explicitly deferred.

## Verdict

All P19 metrics meet their targets. `RecoveryProtocolV1` and typed merge are
**COMPONENT-QUALIFIED** under deterministic injected observations.

**What this verdict does not establish.** It does not establish that Luther can
take an approved issue and produce a pull request, nor that any workflow reaches
`create_pr`, nor that the recovery and merge machinery is reachable in
production. Those are the questions Gate A-R, Gate A-D, and Gate B answer, and
all three were unmeasured when this report was written.

The canary harness that produced this evidence does not execute Luther. It calls
nine helpers in sequence and asserts that it called them, having supplied the
result of each hard stage itself. That makes it a valid **component** test and an
invalid **product** test.
