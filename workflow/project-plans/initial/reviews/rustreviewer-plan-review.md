# Plan Review: Luther Workflow Runtime Initial Implementation

**Plan ID**: PLAN-20260404-INITIAL-RUNTIME  
**Reviewer**: rustreviewer  
**Review Date**: 2026-04-04  
**Scope**: Full planning set — specification, execution tracker, analysis artifacts, pseudocode, and all 12+12a phase plans  

---

## 1. Alignment to Overview

**Reference**: `project-plans/initial/overview.md`

### Strong Alignment

| Overview Element | Plan Coverage | Evidence |
|---|---|---|
| Layered architecture (Monitor → Engine → Workflow → Adapters → Persistence) | Phases 03–12 build these layers sequentially | Phase 03 creates `src/workflow/`, Phase 06/08 create `src/engine/runner.rs`, `transition.rs`, `persistence/checkpoint.rs`, `artifacts.rs`; Phase 10 creates `src/monitor/`, `src/service/`, `src/adapters/git.rs` |
| Workflow type + instance config separation | Explicitly addressed in Phase 03 (schema + TOML files) and Phase 05 (binding implementation) | Phase 03 files-to-create: `config/workflows/issue-fix-v1.toml`, `config/workflow-configs/profile-0.toml` |
| `(workflow_type_id, config_id, run_id)` binding model | Specification `Data Schemas` section defines `WorkflowRunRef`; pseudocode `config-loading.md` step 10 builds it | `specification.md` Data Schemas; `analysis/pseudocode/config-loading.md` line 10 |
| TDD-first discipline | Every implementation phase (05, 08, 10, 12) is preceded by a TDD phase (04, 07, 09, 11) | Execution tracker shows strict alternation: `04 -> 04a -> 05 -> 05a -> ...` |
| No-placeholder rule | Implementation phases 05, 08, 10, 12 all include mandatory `grep -rn "todo!\|unimplemented!"` deferred-implementation detection | Phase 05, 08, 10, 12 verification commands sections |
| Release/distribution xtask pipeline | Phase 11/12 cover quality and release CI | Phase 11 files-to-modify: `.github/workflows/pr-quality.yml`, `.github/workflows/release.yml` |
| Config format: TOML primary, JSON optional, no YAML | Phase 03 req WF-002/WF-003; Phase 05 implements both | Phase 03 requirements, Phase 05 requirements |

### Partial Alignment

| Overview Element | Gap | Severity |
|---|---|---|
| **dagrs as workflow graph substrate** | Overview §4.1 lists `dagrs` as the workflow graph runtime. The specification mentions it nowhere. No plan phase includes `dagrs` integration, wrapping, or evaluation. The current `Cargo.toml` has no `dagrs` dependency. The plan phases create `engine/runner.rs` and `engine/transition.rs` but give no guidance on whether they wrap dagrs or build a custom graph engine. | **HIGH** |
| **Adapter layer breadth** | Overview §5.1 lists `adapters/shell.rs`, `git.rs`, `gh.rs`, `llm.rs`. Phase 10 creates only `src/adapters/git.rs`. No phase creates `shell.rs`, `gh.rs`, or `llm.rs`. | **MEDIUM** — MVP may not need all, but the plan should explicitly scope which adapters are in/out |
| **Config precedence layers** | Overview §6.2 defines 6-layer precedence (static defaults → monitor/engine config → workflow type → workflow config → env overrides → CLI flags). No phase explicitly addresses the env-override or CLI-flag precedence layers beyond "CLI integration" in Phase 12. | **LOW-MEDIUM** |
| **Runtime filesystem structure** | Overview §5.2 specifies `~/Library/Application Support/luther-workflow/` (macOS) and `~/.config/luther-workflow/` (Linux) with `directories` crate. The `directories` crate is not in `Cargo.toml` and no phase explicitly creates the path-resolution module. | **MEDIUM** |
| **MVP workflow path (scan_issues through terminal)** | Overview §2.2 defines the full step graph. No plan phase specifies authoring the actual step topology TOML for the complete MVP flow — Phase 03 creates `issue-fix-v1.toml` but provides only a minimal `[[steps]]` example in `specification.md`. | **MEDIUM** |
| **Structured logs/events via tracing** | Overview §4.1 lists `tracing` + `tracing-subscriber` (already in `Cargo.toml`). Plan phases reference "structured events" but no phase specifically creates the event schema or tracing span/event integration. | **LOW** |

### Misalignment

| Overview Element | Issue |
|---|---|
| **Dependency availability** | Overview §7 dependency table lists `dagrs 0.8.0`, `tokio 1.51.0`, `clap 4.6.0`, `rusqlite 0.39.0`, `uuid 1.23.0`, `directories 6.0.0`, `toml 1.1.2`. Of these, **none** are in the current `Cargo.toml`. The preflight phase (00a) includes dependency verification commands but does not specify which deps must be added or when. This creates a "silent assumption" that dependencies will be added during implementation phases, but no phase's "files to modify" section lists `Cargo.toml` for dependency additions until Phase 12. |

---

## 2. Coverage vs EARS Requirement IDs

### Methodology

Cross-referenced every REQ-EARS-* ID from `requirements-ears.md` against every phase's "Requirements Implemented" section. Verified that each requirement has at least one TDD phase + one implementation phase claiming it, and that the phase's file outputs could plausibly satisfy the requirement.

### Grouped Matrix

#### ARCH — Architecture and Boundaries

| Req ID | Requirement Summary | Plan Phase(s) | Verdict | Evidence/Notes |
|---|---|---|---|---|
| ARCH-001 | Separate monitor/engine/type/config | P01, P03 | **PASS** | P01 creates domain-model.md with boundary rules; P03 creates separate workflow schema module |
| ARCH-002 | Engine shall not embed domain policy | P01 | **PARTIAL** | P01 claims it but only produces analysis artifacts. No implementation phase has a structural test that the engine module doesn't import workflow-domain types. Verification is aspirational, not enforced. |
| ARCH-003 | Monitor independent of step semantics | P01 | **PARTIAL** | Same issue — claimed in analysis, no compile-time or test-enforced boundary. |
| ARCH-004 | Engine instantiates from (type, config, run) tuple | P01, P03, P04, P05 | **PASS** | Full TDD→impl chain with `WorkflowRunRef` struct |
| ARCH-005 | Single-instance enforcement with preserved identifiers | P01 | **PARTIAL** | Claimed at analysis level only. Implementation appears in P10 (monitor) but ARCH-005 is not listed in P10's requirements. |

#### WF — Workflow Definition and Config

| Req ID | Requirement Summary | Plan Phase(s) | Verdict | Evidence/Notes |
|---|---|---|---|---|
| WF-001 | Topology as external declarative data | P03, P05 | **PASS** | P03 creates TOML schema types; P05 implements loader |
| WF-002 | TOML primary format | P03, P05 | **PASS** | Explicit in both phases |
| WF-003 | JSON optional equivalent | P03, P05 | **PASS** | Explicit in both phases |
| WF-004 | Resolve type/config by identifiers | P02, P04, P05 | **PASS** | Pseudocode + TDD + impl chain |
| WF-005 | Reject invalid configs with structured errors | P02, P04, P05 | **PASS** | Pseudocode step 4/7/8/9; TDD phase 04 |
| WF-006 | Type definition includes topology/transitions/guards | P03, P05 | **PASS** | Schema stub (P03) + implementation (P05) |
| WF-007 | Instance config includes params/guards/adapters/repo settings | P03, P05 | **PASS** | Schema stub (P03) + implementation (P05) |

#### MON — Monitor and Engine Lifecycle

| Req ID | Requirement Summary | Plan Phase(s) | Verdict | Evidence/Notes |
|---|---|---|---|---|
| MON-001 | Singleton lock on start | P09 (TDD), P10 (impl) | **PASS** | Both phases list MON-001 |
| MON-002 | Heartbeat/status metadata | P09, P10 | **PASS** | P10 creates `monitor/heartbeat.rs` |
| MON-003 | Restart/backoff on unexpected exit | P09, P10 | **PASS** | Pseudocode `monitor-loop.md` steps 7–10 |
| MON-004 | Stop restart loops when limit exceeded | P09, P10 | **PASS** | Pseudocode step 8 |
| MON-005 | Graceful shutdown + persist state | P09, P10 | **PASS** | Pseudocode step 6 |

#### ENG — Engine Execution

| Req ID | Requirement Summary | Plan Phase(s) | Verdict | Evidence/Notes |
|---|---|---|---|---|
| ENG-001 | Bind type+config into instance | P02, P04, P05 | **PASS** | Full chain |
| ENG-002 | Persist checkpoints/events after each step | P06, P07, P08 | **PASS** | All three phases claim it; P08 creates checkpoint.rs |
| ENG-003 | Route fatal to terminal + write artifacts | P06, P07, P08 | **PASS** | Pseudocode engine-runner.md step 9 |
| ENG-004 | Persist checkpoint on interrupt/shutdown | P06, P07, P08 | **PASS** | Pseudocode engine-runner.md step 10 |

#### ROUTE — Step Routing, Loops, and Guardrails

| Req ID | Requirement Summary | Plan Phase(s) | Verdict | Evidence/Notes |
|---|---|---|---|---|
| ROUTE-001 | Structured outcomes, not string matching | P02, P06, P07, P08 | **PASS** | Pseudocode step 3/6 are explicit |
| ROUTE-002 | Loop-back transitions in remediation states | P02, P06, P07, P08 | **PASS** | Pseudocode step 7 |
| ROUTE-003 | Abandon when loop limits reached | P02, P06, P07, P08 | **PASS** | Pseudocode step 8 |
| ROUTE-004 | Enforce guardrails from config | P02, P06, P07, P08 | **PASS** | Pseudocode step 7 |

#### REPO — Repository Working Directory

| Req ID | Requirement Summary | Plan Phase(s) | Verdict | Evidence/Notes |
|---|---|---|---|---|
| REPO-001 | Config defines checkout source/workspace/branch policy | P09, P10 | **PASS** | Pseudocode repository-prep.md step 1 |
| REPO-002 | Resolve/create working directory by strategy | P09, P10 | **PASS** | Pseudocode steps 2–4 |
| REPO-003 | Shared strategy reuses path | P09, P10 | **PASS** | Pseudocode step 3 |
| REPO-004 | Per-run strategy creates isolated path | P09, P10 | **PASS** | Pseudocode step 4 |
| REPO-005 | Checkout base + create/switch branch | P09, P10 | **PASS** | Pseudocode steps 6–9 |
| REPO-006 | Create branch if missing | P09, P10 | **PASS** | Pseudocode step 9 |
| REPO-007 | Force-reset if configured | P09, P10 | **PASS** | Pseudocode step 10 |
| REPO-008 | Fail init with diagnostics on repo errors | P09, P10 | **PASS** | Pseudocode step 11 |
| REPO-009 | Push to remote when configured | P09, P10 | **PASS** | Pseudocode step 12 context; P09 claims it |

#### PERSIST — Persistence, Artifacts, Traceability

| Req ID | Requirement Summary | Plan Phase(s) | Verdict | Evidence/Notes |
|---|---|---|---|---|
| PERSIST-001 | Persist run metadata in local durable storage | P04, P05, P07, P08 | **PASS** | Multiple phases; `persistence/run_metadata.rs` created in P05 |
| PERSIST-002 | Event record + checkpoint after each step | P06, P07, P08 | **PASS** | Pseudocode engine-runner.md steps 4–5 |
| PERSIST-003 | Per-run artifacts in deterministic paths | P06, P07, P08 | **PASS** | `persistence/artifacts.rs` created in P06/P08 |
| PERSIST-004 | Structured error on persistence failure | P06, P07, P08 | **PASS** | Claimed in all three |

#### SVC — Service Mode and Control Plane

| Req ID | Requirement Summary | Plan Phase(s) | Verdict | Evidence/Notes |
|---|---|---|---|---|
| SVC-001 | Foreground process, no self-daemonization | P09, P10 | **PASS** | P10 creates `service/launchd.rs`, `service/systemd.rs` |
| SVC-002 | Generate and install platform service definitions | P09, P10 | **PASS** | P10 creates `service/spec.rs` |
| SVC-003 | IPC status/control while monitor active | P09, P10 | **PASS** | P10 creates `monitor/ipc.rs` |
| SVC-004 | Explicit platform diagnostics on failure | P09, P10 | **PASS** | Claimed in P09/P10 requirements |

#### QUAL — Quality, Safety, and Release

| Req ID | Requirement Summary | Plan Phase(s) | Verdict | Evidence/Notes |
|---|---|---|---|---|
| QUAL-001 | Quality gate: fmt/clippy/guards/complexity/tests/coverage | P11, P12 | **PASS** | Already partially enforced by existing `xtask/src/main.rs` and `pr-quality.yml`; P11/P12 extend it |
| QUAL-002 | Release via xtask commands | P11, P12 | **PASS** | Already implemented in xtask; P12 lists CI workflow modifications |
| QUAL-003 | Tag-triggered `cargo release-all` | P11, P12 | **PASS** | Already in `release.yml` |
| QUAL-004 | Fail on missing secrets | P11, P12 | **PASS** | Already in `release.yml` step "Validate required secrets" |

#### SCALE — Single-Instance and Forward Scalability

| Req ID | Requirement Summary | Plan Phase(s) | Verdict | Evidence/Notes |
|---|---|---|---|---|
| SCALE-001 | MVP runs exactly one instance | P09, P10 | **PASS** | P09 TDD, P10 impl; MON boundary |
| SCALE-002 | Persisted run model includes type+config IDs | P04, P05, P07, P08 | **PASS** | `WorkflowRunRef` in spec |
| SCALE-003 | Select instance by ID without code change | P09, P10 | **PASS** | Claimed in P09/P10 |

### Coverage Summary

| Group | Total Reqs | Pass | Partial | Fail |
|---|---|---|---|---|
| ARCH | 5 | 2 | 3 | 0 |
| WF | 7 | 7 | 0 | 0 |
| MON | 5 | 5 | 0 | 0 |
| ENG | 4 | 4 | 0 | 0 |
| ROUTE | 4 | 4 | 0 | 0 |
| REPO | 9 | 9 | 0 | 0 |
| PERSIST | 4 | 4 | 0 | 0 |
| SVC | 4 | 4 | 0 | 0 |
| QUAL | 4 | 4 | 0 | 0 |
| SCALE | 3 | 3 | 0 | 0 |
| **TOTAL** | **49** | **46** | **3** | **0** |

The 3 PARTIAL items (ARCH-002, ARCH-003, ARCH-005) are all about enforcing architectural boundaries at test/compile time rather than just asserting them in analysis documents. They can be addressed with targeted test additions in P04 or P09.

---

## 3. Technical Feasibility

### Feasible

1. **Single-crate + xtask layout**: Already proven working. `Cargo.toml`, `xtask/`, quality gates, and CI workflows all exist and function. The plan extends rather than replaces this.
2. **TOML/JSON config parsing with serde**: Straightforward Rust — `serde`, `toml`, and `serde_json` are standard ecosystem crates already in `Cargo.toml` (serde/serde_json; `toml` needs adding).
3. **SQLite persistence via rusqlite**: Well-established pattern. `rusqlite` is mature and the usage here (checkpoints, events, run metadata) is a standard embedded-DB use case.
4. **Monitor/engine process supervision**: The plan describes in-process supervision (spawn + watch engine task), not multi-process. This is feasible with tokio task management but needs careful signal handling.
5. **TDD-first with alternating phases**: The plan structure (TDD → impl → verify cycle) is unusually disciplined and should produce high-quality output if followed. The `todo!`/`unimplemented!` detection is a strong safeguard.
6. **Existing quality infrastructure**: xtask already covers fmt, clippy, structural guards, complexity (lizard), coverage (llvm-cov with 80% gate), and file-size limits. This is a stronger starting point than most Rust projects.

### Feasibility Risks

| Risk | Detail | Severity |
|---|---|---|
| **dagrs integration uncertainty** | `dagrs` is the claimed workflow graph substrate but has zero presence in the plan phases. dagrs 0.8.0's last upstream commit was 2026-01-16 (~3 months ago). The crate's API maturity, async compatibility, and checkpointing primitives need evaluation. If dagrs doesn't fit, a custom graph engine is needed, which significantly changes scope for Phases 06–08. | **HIGH** |
| **Phase 10 scope overload** | Phase 10 combines repository workspace/branching implementation, monitor process management, heartbeat, IPC, and service generation (launchd + systemd) into a single implementation phase. That's 7 new files spanning 4 distinct subsystems. This is the largest and most heterogeneous phase in the plan. | **HIGH** |
| **IPC mechanism unspecified** | The plan creates `monitor/ipc.rs` and `service/spec.rs` but never specifies the IPC protocol (Unix domain socket? Named pipe? HTTP? gRPC?). The pseudocode says "IPC status/control endpoint" but the protocol choice affects dependencies, cross-platform behavior, and test complexity significantly. | **MEDIUM** |
| **Async runtime introduction** | `tokio` is listed in the overview dependency table but is not in `Cargo.toml`. Adding tokio changes `main()` to `#[tokio::main]`, affects all adapter I/O, and requires decisions about sync-vs-async boundaries. No phase explicitly addresses the tokio adoption. | **MEDIUM** |
| **rusqlite on CI (Ubuntu)** | `rusqlite` requires either system sqlite3 or bundled compilation. The CI workflows (`pr-quality.yml`) don't install libsqlite3-dev. If using the `bundled` feature, compile times increase. Neither is addressed. | **LOW-MEDIUM** |
| **80% coverage gate with new modules** | The existing 80% coverage gate (`xtask coverage`) is trivially met with the current ~15-line codebase. As modules grow, maintaining 80% line coverage requires writing tests at every phase — feasible with TDD discipline but the gate may cause blocking failures during stub phases (03, 06) where stubs compile but have no behavioral tests yet. | **LOW-MEDIUM** |

---

## 4. Critical Gaps

### GAP-1: dagrs Integration Strategy is Missing (CRITICAL)

**Evidence**: Overview §4.1 states: "`dagrs`: workflow graph runtime substrate (branches/loops/router/checkpointing primitives)". The word "dagrs" appears 0 times in `specification.md`, 0 times in any plan phase file, 0 times in any pseudocode file, and 0 times in `domain-model.md`.

**Impact**: The entire engine execution model (Phases 06–08) depends on whether dagrs is used as the step-graph runtime or whether a custom implementation is built. This is the single most consequential technical decision in the project and it's unaddressed in the execution plan.

**Recommendation**: Add a dagrs evaluation/spike task to Phase 00a (preflight) or Phase 03. Either confirm dagrs as the substrate and document its API mapping to the domain model, or explicitly scope it out and document the custom alternative.

### GAP-2: Dependency Addition Timing is Unspecified (HIGH)

**Evidence**: `Cargo.toml` currently contains only `anyhow`, `serde`, `serde_json`, `thiserror`, `tracing`, `tracing-subscriber`. The plan requires `tokio`, `clap`, `rusqlite`, `uuid`, `directories`, `toml`, and potentially `dagrs` — none of which are present. No phase's "Files to Modify" section includes `Cargo.toml` for dependency additions until Phase 12 (which lists `Cargo.toml` for CLI wiring, not dependency setup).

**Impact**: Phases 03, 05, 06, 08, 10 will all need new dependencies to compile. Without explicit `Cargo.toml` modifications in each phase, implementers will be forced to improvise, which defeats the phase-discipline model.

**Recommendation**: Phase 03 should explicitly list `Cargo.toml` modifications to add `toml`. Phase 05 or 06 should add `rusqlite`, `uuid`. Phase 10 should add `tokio`, `directories`. Phase 12 should add `clap`. Each addition should be preflight-verified in 00a.

### GAP-3: No Adapter Implementation Phases for shell/gh/llm (MEDIUM)

**Evidence**: Overview §5.1 directory structure shows `adapters/shell.rs`, `git.rs`, `gh.rs`, `llm.rs`. Phase 10 creates only `src/adapters/git.rs`. No phase creates shell, gh, or llm adapters. The `adapters/mod.rs` is never created in any phase.

**Impact**: Without adapters, the engine can route between steps but cannot actually execute them. The MVP workflow needs at minimum shell execution (for running checks), git operations, and gh CLI invocations.

**Recommendation**: Either expand Phase 10 (already overloaded) or add a Phase 10b specifically for adapter implementations. At minimum, `adapters/mod.rs`, `adapters/shell.rs`, and `adapters/gh.rs` should be scoped into the plan. The LLM adapter can be deferred to a later plan.

### GAP-4: Phase 06 and Phase 08 Both Create the Same Files (MEDIUM)

**Evidence**: Phase 06 (stub) files-to-create: `src/engine/runner.rs`, `src/engine/transition.rs`, `src/persistence/checkpoint.rs`, `src/persistence/artifacts.rs`. Phase 08 (impl) files-to-create: identical list — `src/engine/runner.rs`, `src/engine/transition.rs`, `src/persistence/checkpoint.rs`, `src/persistence/artifacts.rs`.

**Impact**: Phase 06 is a "stub" phase that creates compilable seams. Phase 08 is the implementation phase. But both list the same files as "files to create." If Phase 06 creates these files (as stubs), Phase 08 should list them as "files to modify." This is a plan consistency error that will confuse implementers and verification.

**Recommendation**: Change Phase 08's file list from "Files to Create" to "Files to Modify" for all four files. Same applies to Phase 10 vs Phase 09 if applicable.

### GAP-5: No SQLite Schema Migration Strategy (LOW-MEDIUM)

**Evidence**: `rusqlite` is planned for run metadata, checkpoints, heartbeat, and state transitions. No plan phase defines the SQLite schema, migration approach, or schema versioning. The pseudocode references "persist run metadata" but never defines the table structure.

**Impact**: Without an explicit schema definition phase, the database layer will be improvised during implementation, leading to inconsistencies and making checkpoint/resume testing unreliable.

**Recommendation**: Add schema definition to Phase 05 (where `persistence/run_metadata.rs` is created) or Phase 06. Include at least table DDL for runs, events, checkpoints, and a schema version table.

### GAP-6: Verification Phase Behavioral Assertions Are Generic (LOW-MEDIUM)

**Evidence**: Every verification phase (01a through 12a) uses identical checklist items: "Behavioral outcomes match the requirement text", "Tests would fail if implementation were removed", "Feature is reachable through planned call paths". The BDD-style GIVEN/WHEN/THEN blocks in every requirements section are also generic: "GIVEN: the runtime is started with relevant configuration and context / WHEN: the condition described by this requirement occurs / THEN: the system behavior matches the requirement exactly".

**Impact**: These are structurally present but semantically vacuous. A verifier cannot actually use "WHEN: the condition described by this requirement occurs" to determine if a test is adequate. The verification gates look rigorous but provide no concrete acceptance criteria.

**Recommendation**: Replace the generic GIVEN/WHEN/THEN templates with requirement-specific behavioral assertions. For example, REQ-EARS-ROUTE-003 should have: "GIVEN: a workflow instance with max_remediation_loops=3 / WHEN: the 4th remediation loop is entered / THEN: the engine transitions to abandon_and_log".

---

## 5. Recommended Fixes (Prioritized)

### Priority 1 — Must Fix Before Execution

| # | Fix | Affected Phase(s) | Rationale |
|---|---|---|---|
| 1 | **Resolve dagrs status**: Add dagrs evaluation to P00a preflight or P03. Document whether dagrs is used, deferred, or replaced. If used, add API mapping to domain model. If not, document the custom engine approach. | P00a, P03, P06–P08 | Blocks the entire engine layer design |
| 2 | **Specify dependency additions per phase**: Add `Cargo.toml` to "Files to Modify" for each phase that introduces new crate dependencies, with explicit dependency names. | P03, P05, P06, P10, P12 | Without this, phases cannot compile |
| 3 | **Split Phase 10**: Separate repository-prep implementation from monitor/service implementation. Create P10 for repo+adapters and P10b for monitor+service+IPC, each with their own TDD phases. | P09, P09a, P10, P10a | Phase 10 is overloaded; 7 files across 4 subsystems is too large for a single atomic phase |
| 4 | **Fix Phase 06/08 file list duplication**: Phase 08 should modify, not create, the files already created as stubs in Phase 06. | P06, P08 | Plan consistency; implementer confusion |

### Priority 2 — Should Fix Before Execution

| # | Fix | Affected Phase(s) | Rationale |
|---|---|---|---|
| 5 | **Add concrete GIVEN/WHEN/THEN per requirement**: Replace generic behavioral templates with specific, testable assertions for each EARS requirement. | All phase docs | Verification phases are currently unfalsifiable |
| 6 | **Scope adapter breadth**: Explicitly state which adapters are MVP (shell, git, gh) and which are deferred (llm). Add `adapters/mod.rs` and at least `shell.rs` to the plan. | P09, P10 or new phase | Engine has no way to execute steps without adapters |
| 7 | **Define SQLite schema**: Add table DDL and schema versioning approach to Phase 05 or Phase 06. | P05 or P06 | Persistence layer needs structure before TDD tests can assert against it |
| 8 | **Specify IPC protocol**: Choose Unix domain socket (recommended for local-only MVP) and document in specification or Phase 10. | Specification, P10 | Unspecified protocol creates implementation ambiguity |

### Priority 3 — Should Fix, Can Be Deferred

| # | Fix | Affected Phase(s) | Rationale |
|---|---|---|---|
| 9 | **Add ARCH boundary enforcement tests**: Add compile-time or test-time checks that `engine` doesn't depend on workflow-domain types, and `monitor` doesn't depend on step semantics. | P04 or P07 | ARCH-002, ARCH-003 are partial |
| 10 | **Address coverage gate during stub phases**: Document that coverage may temporarily drop during stub phases (03, 06) and add an interim lower gate or exclude stub modules from coverage until implementation phases. | P03, P06 | 80% gate may block stub phase completion |
| 11 | **Add CI rusqlite dependency**: Update `pr-quality.yml` to install `libsqlite3-dev` or use `rusqlite` with `bundled` feature. | P06+ | CI will fail when rusqlite is added |
| 12 | **Document tokio adoption boundary**: Explicitly state when `main()` transitions to `#[tokio::main]` and which modules use async vs sync. | P10 or P12 | tokio is in dependency table but adoption is implicit |
| 13 | **Complete MVP workflow type TOML**: The example in specification.md is a 4-line fragment. Phase 03 should produce the complete `issue-fix-v1.toml` with all steps from overview §2.2. | P03 | The actual workflow graph must be authored somewhere |

---

## 6. Go/No-Go Assessment

### Verdict: **CONDITIONAL GO**

The plan is structurally sound, well-sequenced, and covers 94% (46/49) of EARS requirements at PASS level. The TDD-first discipline, no-placeholder enforcement, and existing quality infrastructure (xtask + CI) put this plan well ahead of typical project plans. The analysis artifacts, pseudocode, and domain model show genuine domain understanding.

**However**, execution cannot proceed safely until Priority 1 fixes are applied:

1. The dagrs question is the largest single risk — it's either the core of the engine or absent from it, and the plan doesn't say which. This must be resolved before Phase 03.
2. Dependency timing must be made explicit or phases will fail to compile.
3. Phase 10 must be split — its current scope is 2–3x larger than any other implementation phase and combines unrelated subsystems.
4. The Phase 06/08 file duplication must be corrected as a basic plan hygiene issue.

**With those 4 fixes applied**, the plan is viable for execution by a Rust team. The remaining Priority 2/3 items can be addressed during Phase 01 (analysis) or as amendments during execution without blocking the phase chain.

### Strengths Worth Preserving

- Strict phase gating with binary PASS/FAIL — resist any temptation to weaken this
- Deferred-implementation detection (`todo!`/`unimplemented!` grep) — this is unusually rigorous
- Existing xtask quality infrastructure is production-grade
- The `(workflow_type_id, config_id, run_id)` binding model is clean and extensible
- Pseudocode is numbered and referenced by line in implementation phases — good traceability
- The plan explicitly addresses forward-scalability (multi-instance, multi-type) without over-engineering MVP

### Sequencing Assessment

The 12-phase sequence is logically ordered: analysis → pseudocode → schema stubs → config TDD → config impl → engine stubs → engine TDD → engine impl → repo/monitor TDD → repo/monitor impl → CLI/quality TDD → CLI/quality impl. This bottom-up build order minimizes rework. The only sequencing concern is Phase 10's overload (see Priority 1 Fix #3).
