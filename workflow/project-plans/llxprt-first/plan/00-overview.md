# Plan: llxprt-first — First Real Workflow

Plan ID: `PLAN-20260408-LLXPRT-FIRST`
Generated: 2026-04-08
Total Phases: 44 (P00a through P21a, including verification sub-phases)
Source Specification: `project-plans/llxprt-first/overview.md`
Requirements Document: `project-plans/llxprt-first/requirements.md`

## Requirements Coverage

This plan implements all requirements from the requirements document:

| Requirement Group | IDs | Phases |
|---|---|---|
| Enhanced ShellExecutor | REQ-LF-SHELL-001 through REQ-LF-SHELL-010 | P03–P05 |
| VerifyExecutor | REQ-LF-VERIFY-001 through REQ-LF-VERIFY-009 | P06–P08 |
| Namespaced Context | REQ-LF-CTX-001 through REQ-LF-CTX-004 | P09–P11 |
| Per-edge Loop Limits | REQ-LF-LOOP-001 through REQ-LF-LOOP-005 | P12–P14 |
| Engine Integration | REQ-LF-SEP-001, REQ-LF-PROF-003, REQ-LF-PROF-004 | P15–P16 |
| Profile Configuration | REQ-LF-PROF-001 through REQ-LF-PROF-004 | P15, P17 |
| Data Flow | REQ-LF-DATA-001 through REQ-LF-DATA-003 | P17 |
| Issue Selection | REQ-LF-ISSUE-001 through REQ-LF-ISSUE-004 | P17, P18 |
| Fetch Issue | REQ-LF-FETCH-001 through REQ-LF-FETCH-004 | P17, P18 |
| Workspace Setup | REQ-LF-WS-001 through REQ-LF-WS-004 | P17, P18 |
| Planning Loop | REQ-LF-PLAN-001 through REQ-LF-PLAN-005 | P17, P18 |
| Implementation | REQ-LF-IMPL-001 through REQ-LF-IMPL-003 | P17, P18 |
| Test/Remediation | REQ-LF-TEST-001 through REQ-LF-TEST-003 | P17, P18 |
| PR Submission | REQ-LF-PR-001 through REQ-LF-PR-004 | P17, P18 |
| Failure/Abandonment | REQ-LF-FAIL-001 through REQ-LF-FAIL-005 | P15–P16, P17, P18 |
| Engine/Workflow Separation | REQ-LF-SEP-001 through REQ-LF-SEP-003 | P15, P19 |
| Scope Boundary | REQ-LF-SCOPE-001, REQ-LF-SCOPE-002 | P17, P18 |

## Critical Reminders

Before implementing ANY phase, ensure you have:

1. Completed preflight verification (Phase 00a)
2. Read `dev-docs/goodtests.md` — tests must be behavioral
3. Verified all dependencies and types exist as assumed
4. Written integration tests BEFORE unit tests for multi-component features
5. Every function/struct/test MUST include `@plan:PLAN-20260408-LLXPRT-FIRST.PNN` and `@requirement:REQ-LF-XXX` markers

## Phase Index

| Phase | ID | Title | Type | Description |
|---|---|---|---|---|
| 00a | P00a | Preflight Verification | Verification | Verify all assumptions before any code |
| 01 | P01 | Domain Analysis | Analysis | Analyze existing code, identify touch points, map data flow |
| 01a | P01a | Analysis Verification | Verification | Verify analysis completeness |
| 02 | P02 | Pseudocode | Design | Numbered pseudocode for all new components |
| 02a | P02a | Pseudocode Verification | Verification | Verify pseudocode completeness and traceability |
| 03 | P03 | Enhanced ShellExecutor — Stub | Stub | Add JSON parsing, stdin, outcome_on_stdout fields to ShellExecutor |
| 03a | P03a | Enhanced ShellExecutor — Stub Verification | Verification | Verify stub compiles |
| 04 | P04 | Enhanced ShellExecutor — TDD | TDD Tests | Behavioral tests for JSON parsing, stdin piping, outcome patterns |
| 04a | P04a | Enhanced ShellExecutor — TDD Verification | Verification | Verify tests fail correctly |
| 05 | P05 | Enhanced ShellExecutor — Implementation | Implementation | Implement all ShellExecutor enhancements |
| 05a | P05a | Enhanced ShellExecutor — Impl Verification | Verification | All ShellExecutor tests pass |
| 06 | P06 | VerifyExecutor — Stub | Stub | Create VerifyExecutor skeleton with check runner |
| 06a | P06a | VerifyExecutor — Stub Verification | Verification | Verify stub compiles |
| 07 | P07 | VerifyExecutor — TDD | TDD Tests | Behavioral tests for multi-check runner and parsers |
| 07a | P07a | VerifyExecutor — TDD Verification | Verification | Verify tests fail correctly |
| 08 | P08 | VerifyExecutor — Implementation | Implementation | Implement VerifyExecutor with parsers |
| 08a | P08a | VerifyExecutor — Impl Verification | Verification | All VerifyExecutor tests pass |
| 09 | P09 | Namespaced Context — Stub | Stub | Modify StepContext for namespaced storage |
| 09a | P09a | Namespaced Context — Stub Verification | Verification | Verify stub compiles, existing tests still pass |
| 10 | P10 | Namespaced Context — TDD | TDD Tests | Behavioral tests for namespaced variable resolution |
| 10a | P10a | Namespaced Context — TDD Verification | Verification | Verify tests fail correctly |
| 11 | P11 | Namespaced Context — Implementation | Implementation | Implement namespaced context and interpolation |
| 11a | P11a | Namespaced Context — Impl Verification | Verification | All context tests pass, existing tests still pass |
| 12 | P12 | Per-edge Loop Limits — Stub | Stub | Add max_iterations to TransitionDef, edge tracking to runner |
| 12a | P12a | Per-edge Loop Limits — Stub Verification | Verification | Verify stub compiles, existing tests still pass |
| 13 | P13 | Per-edge Loop Limits — TDD | TDD Tests | Behavioral tests for per-edge loop limits |
| 13a | P13a | Per-edge Loop Limits — TDD Verification | Verification | Verify tests fail correctly |
| 14 | P14 | Per-edge Loop Limits — Implementation | Implementation | Implement per-edge loop tracking and enforcement |
| 14a | P14a | Per-edge Loop Limits — Impl Verification | Verification | All loop limit tests pass |
| 15 | P15 | Engine Integration — Stub | Stub | Wire VerifyExecutor into registry, add config variables to schema, seed context |
| 15a | P15a | Engine Integration — Stub Verification | Verification | Verify compilation, existing tests pass |
| 16 | P16 | Engine Integration — TDD + Implementation | TDD+Impl | Integration tests: components work together through engine |
| 16a | P16a | Engine Integration — Verification | Verification | Full integration verification |
| 17 | P17 | Workflow TOML + Config Files | Data | Create llxprt-issue-fix workflow definition and config |
| 17a | P17a | Workflow TOML Verification | Verification | TOML loads and validates, all steps/transitions present |
| 18 | P18 | End-to-End Workflow Integration Test | E2E Test | Graph routing tests (mock) + live integration tests (real gh/git) |
| 18a | P18a | End-to-End Verification | Verification | All routing tests pass, live tests pass with --ignored |
| 20 | P20 | CLI Production Wiring | Implementation | Resolve from config/ not tests/fixtures, add --config-dir flag |
| 20a | P20a | CLI Production Wiring Verification | Verification | Dry run works, resolution tests pass |
| 21 | P21 | Smoke Test | E2E Test | Real e2e: select_issue → setup_workspace → fetch_issue against real GitHub |
| 21a | P21a | Smoke Test Verification | Verification | Smoke test passes, files created, cleanup works |
| 19 | P19 | Engine/Workflow Separation Verification | Verification | Engine compiles without workflow files, no domain leakage (final gate) |

## Risk Register

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| JSON dot-path extraction edge cases (nested arrays, missing keys) | Medium | Medium | Keep dot-path simple (no array indexing), fail fast with clear errors |
| VerifyExecutor output parsers break on unexpected tool versions | Medium | High | Parsers handle unparseable output gracefully (return raw stderr), not Fatal |
| Namespaced context breaks existing interpolation tests | Medium | High | Backward-compatible: unnamespaced `{key}` still resolves as before |
| Per-edge loop limits break existing global loop detection | Medium | High | Global `max_iterations` becomes fallback, existing behavior preserved |
| TransitionDef deserialization breaks existing TOML fixtures | Low | High | New `max_iterations` field is `Option<u32>` with `#[serde(default)]` |
| stdin piping + large prompts cause memory issues | Low | Medium | `stdin_file` reads from disk, `stdin` field for small values only |
| WorkflowConfig variables field breaks existing config loading | Low | High | `#[serde(default)]` makes it optional, empty HashMap default |
| E2E tests become brittle if TOML structure changes | Medium | Medium | Tests verify graph properties (connectivity, loop bounds), not exact TOML content |

## Rollback Policy

Each phase modifies a small, well-defined set of files. If a phase fails:

1. `git checkout -- <modified-files>` to revert the phase
2. Delete any newly created files
3. Run `cargo test` to verify clean state
4. Cannot proceed to next phase until the failed phase passes

Cross-phase rollback is NOT supported. Phases must succeed sequentially.

## Integration Map

### Existing Files Modified

| File | Phase | Change |
|---|---|---|
| `src/engine/executors/shell.rs` | P03–P05 | JSON parsing, stdin piping, outcome patterns |
| `src/engine/executor.rs` | P09–P11, P15 | StepContext namespaced storage, interpolate_string namespace resolution, VerifyExecutor registration |
| `src/engine/runner.rs` | P12–P14, P15–P16 | Per-edge loop tracking, step_id on context, config variable loading, fatal-transition routing |
| `src/workflow/schema.rs` | P12–P14, P15 | `max_iterations` on TransitionDef, `variables` on WorkflowConfig |
| `src/persistence/checkpoint.rs` | P12–P14 | Per-edge loop counts in StateSnapshot |
| `src/engine/executors/mod.rs` | P06, P15 | Register VerifyExecutor |
| `src/engine/transition.rs` | P12 | `max_iterations` on local TransitionDef |
| `src/cli/mod.rs` | P20 | `--config-dir` flag on RunArgs |
| `src/main.rs` | P20 | Config resolution from `config/` instead of `tests/fixtures` |
| `src/workflow/config_loader.rs` | P20 | Resolve from `{root}/workflows/` with `valid/` fallback |

### New Files Created

| File | Phase | Purpose |
|---|---|---|
| `src/engine/executors/verify.rs` | P06–P08 | VerifyExecutor implementation |
| `tests/shell_enhanced_tests.rs` | P04 | Enhanced ShellExecutor tests |
| `tests/verify_executor_tests.rs` | P07 | VerifyExecutor tests |
| `tests/namespaced_context_tests.rs` | P10 | Namespaced context tests |
| `tests/per_edge_loop_tests.rs` | P13 | Per-edge loop limit tests |
| `tests/engine_integration_llxprt_first.rs` | P16 | Engine integration tests (programmatic) |
| `tests/e2e_workflow_integration.rs` | P18 | Graph routing tests loading TOML fixtures |
| `tests/live_workflow_integration.rs` | P18 | Live `gh`/`git` integration tests (#[ignore]) |
| `tests/cli_config_resolution_integration.rs` | P20 | CLI config resolution tests |
| `tests/smoke_test.rs` | P21 | End-to-end smoke test (#[ignore]) |
| `config/workflows/llxprt-issue-fix-v1.toml` | P17 | Workflow type definition |
| `config/workflow-configs/llxprt-code.toml` | P17 | Workflow instance config |
| `tests/fixtures/workflows/valid/llxprt-issue-fix-v1.toml` | P17 | Test fixture copy |
| `tests/fixtures/workflow-configs/valid/llxprt-code.toml` | P17 | Test fixture copy |

### User Access Points

- CLI: `luther-workflow run --workflow-type llxprt-issue-fix-v1 --config llxprt-code` (wired in P20)
- CLI: `luther-workflow run --workflow-type llxprt-issue-fix-v1 --config llxprt-code --dry-run` (print steps without executing)
- CLI: `luther-workflow run --workflow-type X --config Y --config-dir /custom/path` (custom config root)
- Engine: `EngineRunner::new(instance, ExecutorRegistry::with_defaults())` — VerifyExecutor auto-registered
- Config resolution: defaults to `config/` relative to cwd (P20), falls back to `{root}/workflows/valid/` for test fixtures

## Phase Dependency Graph

```
P00a (Preflight)
  └─► P01 (Analysis) → P01a
        └─► P02 (Pseudocode) → P02a
              ├─► P03 → P03a → P04 → P04a → P05 → P05a  (ShellExecutor: stub → TDD → impl)
              │     └─► P06 → P06a → P07 → P07a → P08 → P08a  (VerifyExecutor: stub → TDD → impl)
              │           └─► P09 → P09a → P10 → P10a → P11 → P11a  (Namespaced Context: stub → TDD → impl)
              │                 └─► P12 → P12a → P13 → P13a → P14 → P14a  (Per-edge Loops: stub → TDD → impl)
              │                       └─► P15 → P15a  (Engine Integration: stub + wiring + work_dir)
              │                             └─► P16 → P16a  (Engine Integration: TDD + impl)
              │                                   └─► P17 → P17a  (Workflow TOML + Config)
              │                                         └─► P18 → P18a  (E2E: graph routing + live gh tests)
              │                                               └─► P20 → P20a  (CLI production wiring)
              │                                                     └─► P21 → P21a  (Smoke test: real e2e)
              │                                                           └─► P19  (Separation Verification — final gate)
```

All phases are sequential. Each phase requires the previous verification phase to pass.
