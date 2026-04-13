# Plan: Luther Workflow Runtime Initial Implementation

Plan ID: PLAN-20260404-INITIAL-RUNTIME
Generated: 2026-04-04
Total Phases: 12 (plus preflight and verification phases)
Requirements: See `project-plans/initial/requirements-ears.md` (all groups mapped)

## Critical Reminders

1. Complete preflight verification before implementation.
2. Execute phases strictly in sequence with no skips.
3. TDD phases run before implementation phases.
4. Verification outcomes are binary PASS/FAIL only.
5. Implementation phases must not contain placeholders (`todo!`, `unimplemented!`, TODO/FIXME comments).
6. Stage-1 engine routing uses **dagrs** as the workflow graph substrate; do not substitute a custom scheduler unless preflight marks dagrs infeasible and this plan is explicitly amended.
7. Quality/release integration is **STRICT**: extend the existing `xtask` + CI baseline in-place; do not replace it with parallel scripts or loosen existing gates.

## Directory Layout (PLAN.md compliant)

```text
project-plans/initial/
  specification.md
  execution-tracker.md
  analysis/
    domain-model.md
    integration-touchpoints.md
    pseudocode/
      config-loading.md
      engine-runner.md
      monitor-loop.md
      repository-prep.md
  plan/
    00-overview.md
    00a-preflight-verification.md
    01..12 phase docs + 01a..12a verification docs
    .completed/
```

## Execution Order

`00a -> 01 -> 01a -> 02 -> 02a -> 03 -> 03a -> 04 -> 04a -> 05 -> 05a -> 06 -> 06a -> 07 -> 07a -> 08 -> 08a -> 09 -> 09a -> 10 -> 10a -> 11 -> 11a -> 12 -> 12a`

## Phase Index

| Phase | File | Purpose |
|---|---|---|
| 01 | `01-analysis.md` | Domain Analysis and Boundary Definition |
| 01a | `01a-analysis-verification.md` | Verification for phase 01 |
| 02 | `02-pseudocode.md` | Pseudocode and Integration Blueprint |
| 02a | `02a-pseudocode-verification.md` | Verification for phase 02 |
| 03 | `03-runtime-stub.md` | Config and Schema Harness Stub |
| 03a | `03a-runtime-stub-verification.md` | Verification for phase 03 |
| 04 | `04-config-binding-tdd.md` | Behavioral TDD for Config Resolution and Validation |
| 04a | `04a-config-binding-tdd-verification.md` | Verification for phase 04 |
| 05 | `05-config-binding-impl.md` | Config Resolution and Binding Implementation |
| 05a | `05a-config-binding-impl-verification.md` | Verification for phase 05 |
| 06 | `06-engine-routing-persistence-stub.md` | Engine Routing and Persistence Harness Stub |
| 06a | `06a-engine-routing-persistence-stub-verification.md` | Verification for phase 06 |
| 07 | `07-engine-routing-persistence-tdd.md` | Behavioral TDD for Engine Execution, Loops, and Persistence |
| 07a | `07a-engine-routing-persistence-tdd-verification.md` | Verification for phase 07 |
| 08 | `08-engine-routing-persistence-impl.md` | Engine Execution, Loop Guard, and Persistence Implementation |
| 08a | `08a-engine-routing-persistence-impl-verification.md` | Verification for phase 08 |
| 09 | `09-repo-monitor-service-tdd.md` | Behavioral TDD for Repository Prep, Monitor, and Service Control |
| 09a | `09a-repo-monitor-service-tdd-verification.md` | Verification for phase 09 |
| 10 | `10-repo-monitor-service-impl.md` | Repository Workspace/Branching + Monitor/Service Implementation |
| 10a | `10a-repo-monitor-service-impl-verification.md` | Verification for phase 10 |
| 11 | `11-cli-e2e-quality-tdd.md` | Behavioral TDD for CLI End-to-End and Quality/Release Controls |
| 11a | `11a-cli-e2e-quality-tdd-verification.md` | Verification for phase 11 |
| 12 | `12-cli-e2e-quality-impl.md` | CLI Integration, End-to-End Runtime Wiring, and Quality Gate Enablement |
| 12a | `12a-cli-e2e-quality-impl-verification.md` | Verification for phase 12 |

## Risk Register (Condensed)

| Risk | Mitigation | Trigger |
|---|---|---|
| dagrs API mismatch | preflight dagrs compile spike + adapter seam (`src/engine/dagrs_runtime.rs`) before phase 06/08 implementation | dagrs integration fails or API assumptions are incorrect |
| Dependency mismatch | Preflight dependency verification gate | missing crate/type during preflight |
| Hollow implementation | TDD-first + semantic verification checks | tests pass structurally but no behavioral artifacts |
| Unsafe repository prep | temp-repo behavioral tests + explicit diagnostics | checkout/fetch/branch preparation errors |
| Unbounded restart loops | monitor restart limits + degraded state | restart counter exceeds policy |
| Quality gate drift | strictly integrate runtime checks into existing xtask/CI commands/workflows | quality command duplication or weakened checks |

## Rollback Policy

- If a phase fails verification, do not proceed.
- Revert only files touched by the failed phase as needed.
- Preserve planning/evidence artifacts and re-run the failed phase.
- Resume only after PASS is recorded in `.completed/PXXA.md`.
