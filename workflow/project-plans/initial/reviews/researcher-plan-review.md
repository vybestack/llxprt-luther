# Researcher Plan Review: Initial Multi-File Implementation Planning Set

## Executive Verdict

**Verdict: Conditionally usable as a planning scaffold, but not yet executable as a trustworthy implementation plan.**

The planning set is strong on intent preservation around architecture boundaries, declarative workflow data, persistence, and phase sequencing discipline. It aligns well with the high-level architecture in `project-plans/initial/overview.md`, especially the separation of monitor/engine/workflow/config and the bound runtime identity `(workflow_type_id, config_id, run_id)` (`project-plans/initial/overview.md:76-81`, `project-plans/initial/overview.md:262-267`, `project-plans/initial/specification.md:9-12`, `project-plans/initial/analysis/domain-model.md:18-29`).

However, the plan is **not yet operationally executable end-to-end** because several phases assume repository structures and tools that are not present today, and some requirement groups are only partially mapped into concrete implementation tasks. The largest issues are:

1. **The plan claims all EARS groups are implemented, but some are not concretely scheduled.** `specification.md` states all groups are covered (`project-plans/initial/specification.md:66-67`), yet there is no explicit implementation phase for monitor/service configuration files, runtime directory path resolution, or resumable checkpoint loading behavior despite those being central in the overview and requirements (`project-plans/initial/overview.md:388-397`, `project-plans/initial/overview.md:408-453`, `project-plans/initial/requirements-ears.md:76-87`, `project-plans/initial/requirements-ears.md:155-165`).
2. **The plan is only partially test-first.** Some component areas do follow stub -> TDD -> implementation sequencing (`project-plans/initial/plan/00-overview.md`, phase index), but phase 03 and phase 06 introduce production code before any failing behavioral tests, and phase 10 implements monitor/repo/service before any CLI TDD validates the actual entrypoints users will exercise.
3. **Several planned tasks conflict with the current repository baseline.** The plan assumes `tests/` and `tests/mod.rs` exist (`project-plans/initial/plan/04-config-binding-tdd.md:46-49`, `project-plans/initial/plan/07-engine-routing-persistence-tdd.md`, `project-plans/initial/plan/09-repo-monitor-service-tdd.md:176-181`, `project-plans/initial/plan/11-cli-e2e-quality-tdd.md:48-55`), but there is currently no `tests` directory at all. The plan also treats quality/release controls as if they still need enablement in later phases, but the repo already has substantial `xtask` and CI workflows in place (`xtask/src/main.rs:25-72`, `.cargo/config.toml:1-9`, `.github/workflows/pr-quality.yml:14-105`, `.github/workflows/release.yml:19-58`).
4. **Operational risk is concentrated too late.** Repository preparation, monitor/service control, and CLI wiring all land after core engine work, which means the plan delays proving the real execution topology until very late even though the overview makes that topology first-class (`project-plans/initial/overview.md:89-129`, `project-plans/initial/overview.md:172-184`).

Net: the planning set is a solid architectural skeleton, but it needs remediation before it can reliably govern implementation.

## Strengths

1. **Architectural intent is preserved clearly and repeatedly.**
   - Strict engine/workflow boundary is called out in the overview (`project-plans/initial/overview.md:76-81`) and reflected in the specification (`project-plans/initial/specification.md:9-12`, `project-plans/initial/specification.md:103-109`).
   - Domain analysis reinforces monitor independence and external workflow/config data (`project-plans/initial/analysis/domain-model.md:18-21`).

2. **The bound runtime identity is consistent across artifacts.**
   - Overview defines workflow instance as `(workflow_type_id, config_id, run_id)` (`project-plans/initial/overview.md:264-267`).
   - Requirements enforce the same (`project-plans/initial/requirements-ears.md:26-31`, `project-plans/initial/requirements-ears.md:190-194`).
   - Specification and domain model reuse the same structure (`project-plans/initial/specification.md:71-81`, `project-plans/initial/analysis/domain-model.md:23-29`).

3. **The plan has disciplined sequencing mechanics.**
   - Execution order is explicit and exhaustive (`project-plans/initial/plan/00-overview.md`).
   - The execution tracker mirrors the phase map and enforces no skipped phases (`project-plans/initial/execution-tracker.md:5-38`).
   - Verification phases are separated from implementation phases, which is useful for auditability.

4. **Pseudocode is concise and mostly aligned to requirements.**
   - Config-loading pseudocode covers resolution, validation, identity binding, and initial metadata persistence (`project-plans/initial/analysis/pseudocode/config-loading.md:3-12`).
   - Engine-runner pseudocode covers structured outcomes, checkpointing, routing, guardrails, shutdown, and terminal state writing (`project-plans/initial/analysis/pseudocode/engine-runner.md:3-12`).
   - Monitor-loop and repository-prep pseudocode both express the intended operational contracts clearly (`project-plans/initial/analysis/pseudocode/monitor-loop.md:3-11`, `project-plans/initial/analysis/pseudocode/repository-prep.md:3-12`).

5. **The plan is safety-conscious.**
   - It explicitly forbids placeholders in implementation phases (`project-plans/initial/specification.md:105-109`, `project-plans/initial/plan/00-overview.md:10-14`).
   - It has a preflight phase and binary PASS/FAIL gates (`project-plans/initial/plan/00a-preflight-verification.md`, `project-plans/initial/plan/00-overview.md:9-14`).

## Coverage vs EARS

### Group Matrix

| Group | Overall status | Evidence | Review note |
|---|---|---|---|
| ARCH | Strong | `overview.md:76-81`, `requirements-ears.md:17-31`, `specification.md:9-12`, `domain-model.md:18-20` | Clear boundary conformance. |
| WF | Moderate | `requirements-ears.md:36-56`, `specification.md:38-39`, `config-loading.md:3-12`, phases 03-05 | Format/config loading are planned, but schema-level details for transitions/guards are underspecified. |
| MON | Moderate | `requirements-ears.md:61-74`, `monitor-loop.md:3-11`, phases 09-10 | Lifecycle intent exists, but monitor config/state persistence details are not concretely scheduled. |
| ENG | Moderate | `requirements-ears.md:76-87`, `engine-runner.md:3-12`, phases 05-08 | Core loop is planned, but resume/loading behavior is only a pseudocode statement, not a clearly assigned file/task. |
| ROUTE | Strong | `requirements-ears.md:92-103`, `engine-runner.md:5-10`, phases 06-08 | Good alignment around structured outcomes and guardrails. |
| REPO | Moderate | `overview.md:545-572`, `requirements-ears.md:108-133`, `repository-prep.md:3-12`, phases 09-10 | Policy intent is strong, but implementation scope is collapsed into a single `src/adapters/git.rs`, which is likely too thin. |
| PERSIST | Moderate | `requirements-ears.md:139-149`, `config-loading.md:11`, `engine-runner.md:4-5,12`, phases 05-08 | Run metadata/checkpoints/artifacts are planned, but storage layout and DB/checkpoint interplay are unresolved. |
| SVC | Partial | `overview.md:314-329`, `requirements-ears.md:155-165`, phases 09-10 | Install/control are planned, but service config generation and IPC contracts lack enough detail to be executable. |
| QUAL | Weak-to-Moderate | `requirements-ears.md:171-181`, phases 11-12 | Requirements are planned late even though repo already contains active quality/release tooling. Scope is mispositioned. |
| SCALE | Moderate | `requirements-ears.md:187-194`, `domain-model.md:23-29`, phases 01, 04-05, 09-10 | Single-instance and future profile selection are acknowledged, but monitor selection mechanics are not concretely designed. |

### Notable Requirement-Level Checks

#### Architecture and boundaries

- **REQ-EARS-ARCH-001 / 002 / 003 / 004**: Well represented. The overview, specification, and domain model all reinforce separation of concerns and bound runtime identity (`project-plans/initial/overview.md:76-81`, `project-plans/initial/specification.md:9-12`, `project-plans/initial/analysis/domain-model.md:18-29`).
- **REQ-EARS-ARCH-005**: Only partially operationalized. The plan references single-instance enforcement in analysis and monitor phases, but there is no concrete task for lock scope semantics plus persisted metadata preservation together.

#### Workflow definition and config

- **REQ-EARS-WF-001 / 002**: Covered by phase 03 schema/config harness and by overview constraints (`project-plans/initial/plan/03-runtime-stub.md`, `project-plans/initial/overview.md:471-482`).
- **REQ-EARS-WF-003**: Under-specified. JSON support is promised in overview/specification (`project-plans/initial/overview.md:41`, `project-plans/initial/specification.md:38-39`) and mapped to phases 03/05, but no file tasks or tests explicitly mention JSON fixtures or parser parity. This is a plan gap.
- **REQ-EARS-WF-005**: Config-loading pseudocode includes structured startup validation failures (`project-plans/initial/analysis/pseudocode/config-loading.md:4-9`), but test tasks do not explicitly call for structured error assertions.
- **REQ-EARS-WF-006 / 007**: Present in phase mapping, but the example schemas in the specification are too thin to validate these requirements fully (`project-plans/initial/specification.md:83-101`).

#### Monitor and engine lifecycle

- **REQ-EARS-MON-001..005**: Intent is captured in pseudocode (`project-plans/initial/analysis/pseudocode/monitor-loop.md:3-11`) and in phases 09-10, but missing explicit tasks for monitor config file parsing or persisted degraded-state schema, even though overview defines monitor config directories and persisted heartbeat/state (`project-plans/initial/overview.md:388-392`, `project-plans/initial/overview.md:420-429`).
- **REQ-EARS-ENG-002..004**: Covered conceptually by pseudocode and phases 06-08. But resume behavior is only a line in pseudocode (`project-plans/initial/analysis/pseudocode/engine-runner.md:3`) with no dedicated implementation task such as `resume.rs`, checkpoint loader integration, or restart/resume acceptance tests.

#### Routing, loops, and guardrails

- **REQ-EARS-ROUTE-001..004**: This is one of the better-covered areas. Requirements, pseudocode, and phases 06-08 all line up (`project-plans/initial/requirements-ears.md:92-103`, `project-plans/initial/analysis/pseudocode/engine-runner.md:5-10`, `project-plans/initial/plan/06-engine-routing-persistence-stub.md`, `07-engine-routing-persistence-tdd.md`, `08-engine-routing-persistence-impl.md`).

#### Repository working copy, checkout, and branching

- **REQ-EARS-REPO-001..008**: Requirements are represented, and the repository prep pseudocode is good (`project-plans/initial/analysis/pseudocode/repository-prep.md:3-12`). But the implementation collapse into one adapter file plus no explicit workspace-path resolver, branch-template renderer, or structured diagnostics module increases execution risk (`project-plans/initial/plan/10-repo-monitor-service-impl.md:176-183`).
- **REQ-EARS-REPO-009**: The requirement says push belongs to workflow actions (`project-plans/initial/requirements-ears.md:132-133`), but the plan assigns repo work to `src/adapters/git.rs` and does not explicitly connect that behavior to workflow steps like `commit_push`/`submit_pr` defined in the overview (`project-plans/initial/overview.md:54-67`). This is a traceability gap.

#### Persistence, artifacts, traceability

- **REQ-EARS-PERSIST-001..004**: Broadly represented in config-loading and engine pseudocode (`project-plans/initial/analysis/pseudocode/config-loading.md:11`, `project-plans/initial/analysis/pseudocode/engine-runner.md:4-5,12`) and in phases 05-08. But the plan does not reconcile whether durable storage is SQLite-first, checkpoint-file-first, or mixed despite overview naming both `runtime.db` and JSON checkpoints (`project-plans/initial/overview.md:420-435`). That ambiguity matters for implementation feasibility.

#### Service mode and control plane

- **REQ-EARS-SVC-001..004**: Service mode is named correctly in requirements and overview (`project-plans/initial/requirements-ears.md:155-165`, `project-plans/initial/overview.md:80`, `project-plans/initial/overview.md:456-461`), but implementation is thinly specified. Only `spec.rs`, `launchd.rs`, and `systemd.rs` are planned (`project-plans/initial/plan/10-repo-monitor-service-impl.md:181-183`), with no explicit command/status contract tests beyond a generic integration test file. That is not enough detail for reliable service behavior.

#### Quality, safety, release controls

- **REQ-EARS-QUAL-001..004**: The repository already has meaningful quality and release automation via `xtask`, cargo aliases, and GitHub workflows (`xtask/src/main.rs:25-106`, `.cargo/config.toml:1-9`, `.github/workflows/pr-quality.yml:14-105`, `.github/workflows/release.yml:19-58`). The plan treats these as late-stage implementation targets (`project-plans/initial/plan/11-cli-e2e-quality-tdd.md:46-55`, `project-plans/initial/plan/12-cli-e2e-quality-impl.md:46-57`), which is misaligned with current reality and makes the phase mapping inaccurate.

#### Scale and forward extensibility

- **REQ-EARS-SCALE-001 / 002**: Fair coverage through bound IDs and single-instance intent (`project-plans/initial/requirements-ears.md:187-191`, `project-plans/initial/analysis/domain-model.md:23-29`).
- **REQ-EARS-SCALE-003**: Weak concretization. The plan mentions profile selection, but there is no explicit CLI/config selection design task until user-facing CLI integration very late (`project-plans/initial/specification.md:52-55`, `project-plans/initial/plan/10-repo-monitor-service-impl.md:166-172`).

## Feasibility Review

### Phase sequencing

The sequencing is logical at a high level, but there are four feasibility concerns.

1. **The plan introduces production stubs before failing behavior tests.**
   - Phase 03 creates `src/workflow/mod.rs`, `src/workflow/schema.rs`, and config files before config-binding TDD arrives in phase 04 (`project-plans/initial/plan/03-runtime-stub.md:46-54`, `project-plans/initial/plan/04-config-binding-tdd.md:46-54`).
   - Phase 06 similarly introduces engine/persistence production modules before phase 07 TDD (`project-plans/initial/plan/06-engine-routing-persistence-stub.md:66-75`, `project-plans/initial/plan/07-engine-routing-persistence-tdd.md`).
   This is acceptable as seam creation, but it is not genuinely test-first in the strict sense claimed by the specification (`project-plans/initial/specification.md:107-109`).

2. **High-risk operational integration is deferred too far.**
   The real system architecture in the overview is monitor-centric and service-aware from the start (`project-plans/initial/overview.md:89-129`). Yet monitor/service/repo prep do not get exercised until phases 09-10. That means the plan could spend many phases building an engine that later does not fit the real supervision/control plane.

3. **Current repository mismatch increases friction.**
   - Current `src/` contains only `lib.rs` and `main.rs` (`src` directory listing).
   - `tests/` does not exist at all, but multiple phases assume `tests/mod.rs` is already there (`project-plans/initial/plan/04-config-binding-tdd.md:52`, `project-plans/initial/plan/09-repo-monitor-service-tdd.md:180-181`, `project-plans/initial/plan/11-cli-e2e-quality-tdd.md:52-55`).
   - Current dependencies do not include key planned crates such as `tokio`, `clap`, `toml`, `rusqlite`, `uuid`, `directories`, or `dagrs` (`Cargo.toml:12-21`) even though the overview assumes them (`project-plans/initial/overview.md:194-203`).
   This does not make the plan impossible, but it means preflight must do more than verify assumptions; it must cause plan edits before implementation. Right now the preflight phase is a checklist, not an adaptation mechanism (`project-plans/initial/plan/00a-preflight-verification.md`).

4. **Quality/release phases are over-scoped and misplaced.**
   Since `xtask`, cargo aliases, and CI workflows already exist, phases 11-12 should focus on integrating new runtime paths into existing quality gates rather than implementing quality/release controls from scratch (`xtask/src/main.rs:25-106`, `.cargo/config.toml:1-9`, `.github/workflows/pr-quality.yml:14-105`, `.github/workflows/release.yml:19-58`). As written, those phases risk churning already-functional infrastructure unnecessarily.

### Technical execution risk

#### Low-to-moderate risk
- Config loading and identity binding: conceptually straightforward if schema is tightened.
- Structured routing and loop guardrails: feasible if implemented with plain Rust types first.

#### Moderate-to-high risk
- Resume semantics: no concrete storage/read-path design.
- Monitor/service IPC and degraded-state persistence: underdesigned.
- Repository working copy safety: easy to get wrong without explicit fixture repo strategy and destructive-action constraints.
- `dagrs` adoption: named in the overview (`project-plans/initial/overview.md:194`) but not integrated into any phase task. This suggests either the dependency choice is aspirational or the plan intentionally defers the decision. Either way, that ambiguity should be resolved early.

## Whether the Plan Is Genuinely Test-First and Operationally Executable

### Test-first assessment

**Answer: partially, but not genuinely across the whole plan.**

What is good:
- There are explicit TDD phases before corresponding implementation phases for config binding, engine routing/persistence, repo/monitor/service, and CLI/quality (`project-plans/initial/plan/00-overview.md`, phase index).
- Semantic checklists repeatedly say tests must fail naturally before implementation (`project-plans/initial/plan/04-config-binding-tdd.md:87-89`, `project-plans/initial/plan/07-engine-routing-persistence-tdd.md`, `project-plans/initial/plan/09-repo-monitor-service-tdd.md:212-215`, `project-plans/initial/plan/11-cli-e2e-quality-tdd.md:84-89`).

What undermines the claim:
- Stub phases 03 and 06 put production code in place before TDD.
- No TDD phase exists for architecture skeleton, module wiring, or runtime directory/path resolution even though these are behaviorally significant.
- CLI user paths are declared in specification early (`project-plans/initial/specification.md:52-55`) but only tested in phase 11, after monitor/service implementation in phase 10.
- Some TDD phases are not operationally ready because they depend on non-existent baseline directories/files like `tests/mod.rs`.

### Operational executability assessment

**Answer: not yet.**

The plan is not fully executable without prior cleanup because:
- it assumes missing repo structures (`tests/`);
- it duplicates or reopens already-existing quality/release infrastructure instead of integrating with it;
- it lacks concrete tasks for some runtime-critical assets present in the overview, including default monitor/engine config files and runtime path resolution (`project-plans/initial/overview.md:388-397`, `project-plans/initial/overview.md:408-453`);
- it does not explicitly assign implementation of resume loading, monitor config parsing, or service install path generation despite requiring them.

## Major Omissions, Contradictions, and Over/Under-Scoping Issues

### Major omissions

1. **No explicit plan for monitor config and engine config files/parsers.**
   The overview makes them first-class (`project-plans/initial/overview.md:388-392`, `project-plans/initial/overview.md:477-480`), but plan phases only create workflow type/config files in phase 03 (`project-plans/initial/plan/03-runtime-stub.md:46-54`).

2. **No explicit runtime path/directory resolution component.**
   The overview heavily specifies macOS/Linux runtime paths (`project-plans/initial/overview.md:408-461`), but no phase names or tasks a path resolver using `directories` or equivalent.

3. **Resume/checkpoint restoration is underplanned.**
   Pseudocode mentions loading resumable checkpoints (`project-plans/initial/analysis/pseudocode/engine-runner.md:3`), but there is no dedicated test or implementation task for restoring engine state from storage.

4. **No explicit adapter strategy for non-git actions.**
   The overview calls for shell/git/gh/llm adapters (`project-plans/initial/overview.md:252-258`, `project-plans/initial/overview.md:371-376`), but the plan only explicitly names `src/adapters/git.rs` in implementation tasks (`project-plans/initial/plan/10-repo-monitor-service-impl.md:177`). That is below the architectural intent.

5. **No clear plan for workflow step action handler registration/dispatch contracts.**
   The overview requires execution through registered action handlers (`project-plans/initial/overview.md:43-45`), but no specific file/task is assigned for adapter registry or handler dispatch.

### Contradictions

1. **“All groups mapped” vs partial concrete mapping.**
   `specification.md` says all groups are implemented (`project-plans/initial/specification.md:66-67`), but several groups are only represented narratively, not with sufficient file/task coverage.

2. **Quality/release treated as future implementation although already present.**
   The repository already has active xtask aliases and CI workflows (`xtask/src/main.rs:25-106`, `.cargo/config.toml:1-9`, `.github/workflows/pr-quality.yml:14-105`, `.github/workflows/release.yml:19-58`), contradicting the implication that quality/release control enablement is mostly phase-11/12 work.

3. **Strict no-placeholder rule vs explicit stub phases.**
   The spec says implementation phases cannot ship placeholders (`project-plans/initial/specification.md:105-109`), while overview and phase docs allow stubs in phases 03 and 06. This is survivable, but the wording should distinguish scaffolding from deferred behavior more carefully.

### Over-scoping

1. **Phase 10 is too broad.**
   Repository prep, monitor lifecycle, IPC, service generation, and main/lib integration all land together (`project-plans/initial/plan/10-repo-monitor-service-impl.md:176-187`). That is too much operational risk in one implementation phase.

2. **Phase 12 bundles CLI integration with quality gate enablement.**
   These are distinct concerns, and one of them already exists in the repo. This phase should be narrower.

### Under-scoping

1. **Schema/test fixture detail is too thin.**
   Example workflow/config TOML is minimal (`project-plans/initial/specification.md:83-101`) and does not exercise guard references, branching, terminal edges, repo policy, or JSON equivalence.

2. **Service behavior tests are underspecified.**
   A single `tests/monitor_service_integration.rs` is too vague for launchd/systemd generation, IPC status/control, and diagnostics.

3. **Preflight is too passive.**
   It verifies assumptions but does not require updating plan docs when assumptions fail, despite the current repo already diverging from several assumptions.

## Priority Risks

1. **Late discovery of architecture mismatch between CLI/monitor/engine layers.**
   Because end-to-end user paths are not tested until late phases.

2. **Resume semantics fail late because storage format and restoration path are not concretely designed.**

3. **Repository-prep safety bugs due to compressed implementation scope and insufficient fixture strategy.**

4. **Wasted work or churn around quality/release controls because the plan underestimates existing repo infrastructure.**

5. **False confidence from verification markers.**
   Many verification commands are generic `cargo build` / `cargo test` plus `grep`, which can pass even if the intended requirement behavior is only weakly implemented.

## Actionable Remediation Plan

### Ordered patch plan

1. **Patch `project-plans/initial/specification.md`**
   - Add an explicit “Current Repository Baseline” section summarizing the actual repo state: only `src/main.rs` and `src/lib.rs`, no `tests/`, existing `xtask`, existing CI workflows, and current dependency set.
   - Revise the “Formal Requirements” claim from “This plan implements all requirement groups” to “This plan targets all requirement groups; concrete coverage gaps are tracked in execution plan artifacts.”
   - Add missing implementation concerns: monitor config, engine config, runtime path resolution, checkpoint resume loading, action handler registry, and JSON parity tests.
   - Rationale: corrects overstatement and anchors the plan in current reality.

2. **Patch `project-plans/initial/execution-tracker.md`**
   - Add a column or note for “baseline mismatch / plan adjustment required.”
   - Add explicit gates that preflight can force edits to later phase docs before phase 01 starts.
   - Rationale: turns preflight from passive checklist into a real planning control.

3. **Patch `project-plans/initial/analysis/domain-model.md`**
   - Expand entities to include `MonitorConfig`, `EngineConfig`, `WorkflowDefinition`, `StepActionBinding`, `HeartbeatRecord`, and `RunEvent`.
   - Add explicit relationships for checkpoint resume and action dispatch.
   - Rationale: the current model is too coarse for implementation and misses central runtime objects.

4. **Patch `project-plans/initial/analysis/integration-touchpoints.md`**
   - Add current-repo touchpoints for existing `xtask`, `.cargo/config.toml`, `.github/workflows/pr-quality.yml`, and `.github/workflows/release.yml`.
   - Add integration paths for runtime path resolution and adapter registry/dispatch.
   - Rationale: current touchpoints are incomplete and understate integration work.

5. **Patch `project-plans/initial/analysis/pseudocode/config-loading.md`**
   - Extend steps to include monitor/engine config resolution, precedence handling from overview (`project-plans/initial/overview.md:484-492`), JSON parity behavior, and runtime path resolution.
   - Rationale: current pseudocode only covers workflow type/config loading and misses system-level config responsibilities.

6. **Patch `project-plans/initial/analysis/pseudocode/engine-runner.md`**
   - Add explicit step-action lookup/dispatch, resume restoration from checkpoint, terminal artifact emission ordering, and persistence failure handling.
   - Rationale: current pseudocode is directionally right but too thin for reliable phase references.

7. **Patch `project-plans/initial/analysis/pseudocode/monitor-loop.md`**
   - Add monitor config load, heartbeat persistence cadence, IPC command handling contract, and degraded-state persistence.
   - Rationale: needed for MON and SVC coverage.

8. **Patch `project-plans/initial/analysis/pseudocode/repository-prep.md`**
   - Add explicit safety checks for dirty workspace handling, clone vs fetch policy, branch-template rendering, and structured diagnostics payloads.
   - Rationale: repository prep is high-risk and needs more operational detail.

9. **Patch `project-plans/initial/plan/00a-preflight-verification.md`**
   - Replace placeholder TODO tables with concrete required outputs and a mandatory “plan updates required?” section.
   - Add checks for existing CI/xtask assets, missing `tests/` directory, and dependency deltas against overview.
   - Rationale: this is the most important corrective gate.

10. **Patch `project-plans/initial/plan/03-runtime-stub.md` and `04-config-binding-tdd.md`**
   - In phase 03, create the `tests/` directory and fixture layout explicitly if absent.
   - In phase 04, remove reliance on pre-existing `tests/mod.rs` unless the repo convention truly needs it.
   - Add JSON fixture parity tests to phase 04.
   - Rationale: fixes immediate executability issues and improves WF-003 coverage.

11. **Patch `project-plans/initial/plan/06-engine-routing-persistence-stub.md`, `07-engine-routing-persistence-tdd.md`, and `08-engine-routing-persistence-impl.md`**
   - Add explicit tasks/tests for resume-from-checkpoint and action-dispatch registration.
   - Add a dedicated file target for run events if persistence is meant to separate metadata/checkpoints/events.
   - Rationale: closes ENG/PERSIST omissions.

12. **Patch `project-plans/initial/plan/09-repo-monitor-service-tdd.md` and `10-repo-monitor-service-impl.md`**
   - Split phase 10 into two phases or, at minimum, split implementation tasks within the docs: repository prep first, monitor+IPC second, service generation third.
   - Add explicit file targets for monitor config/types and runtime path resolver.
   - Add tests for degraded state, heartbeat persistence, and platform-specific service generation diagnostics.
   - Rationale: reduces risk concentration and aligns better with EARS MON/SVC details.

13. **Patch `project-plans/initial/plan/11-cli-e2e-quality-tdd.md` and `12-cli-e2e-quality-impl.md`**
   - Re-scope these phases around integration with existing quality/release infrastructure rather than initial creation.
   - Add CLI tests for `run --workflow-type --config`, `status`, and service commands, matching `specification.md` user access points (`project-plans/initial/specification.md:52-55`).
   - Rationale: aligns the plan to the actual repository and avoids unnecessary churn.

14. **Patch `project-plans/initial/plan/00-overview.md`**
   - Update the phase index and risk register after the above changes.
   - Add top risks for resume semantics, runtime path resolution, and existing-infra integration mismatch.
   - Rationale: the overview should reflect the corrected implementation plan.

## Bottom Line

The planning set is **architecturally thoughtful but operationally premature**. It is close enough to salvage without replacing the whole structure. The right move is not a rewrite; it is a targeted tightening pass that:
- grounds the plan in the current repository,
- makes preflight authoritative,
- closes the missing monitor/config/resume/path-resolution gaps,
- and re-scopes late quality/release phases around existing infrastructure.

With those corrections, the plan can become both credible and executable.