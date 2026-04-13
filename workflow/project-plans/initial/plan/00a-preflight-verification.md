# Phase 0.5: Preflight Verification

## Phase ID
`PLAN-20260404-INITIAL-RUNTIME.P00A`

## Purpose
Verify assumptions about dependencies, types, call paths, and test infrastructure before any implementation phase.

## How to Execute This Phase

This phase is an **executable gate**, not a passive document.
The executing subagent must run every `Evidence command` below and record the output.
If ANY check returns a blocking result, the subagent must STOP, document the blocker in the
"Blocking Issues Found" section, and set the phase verdict to FAIL.
Do NOT proceed to Phase 01 with unresolved blockers.

### Executable Verification Script

Run each evidence command in sequence. For each row, record:
- The command executed
- The raw output
- PASS or FAIL determination

Write results into the completion marker file `project-plans/initial/plan/.completed/P00A.md`.

## Dependency Verification
| Dependency | Evidence command | Status | Notes |
|---|---|---|---|
| dagrs | `cargo search dagrs --limit 1 && cargo tree -i dagrs || true` | TODO | Stage-1 graph substrate decision is blocked until verified |
| tokio | `cargo tree -i tokio` | TODO | |
| serde / toml / serde_json | `cargo tree -i serde` / `cargo tree -i toml` / `cargo tree -i serde_json` | TODO | |
| clap | `cargo tree -i clap` | TODO | |
| rusqlite | `cargo tree -i rusqlite` | TODO | |

## Dependency Addition Schedule Verification
| Phase | Required `Cargo.toml` additions verified | Status | Notes |
|---|---|---|---|
| P03 | `toml`, `dagrs` | TODO | required for schema + dagrs adapter seam planning |
| P05 | `uuid` | TODO | required for run identity binding |
| P06 | `rusqlite` | TODO | required for persistence stub/DDL seam |
| P10 | `tokio`, `directories` | TODO | required for monitor/service runtime + path resolution |
| P12 | `clap` | TODO | required for CLI command surface |

## Type/Interface Verification
| Type or Module | Expected | Actual Evidence | Match |
|---|---|---|---|
| monitor layer | `src/monitor/*` | `ls src` | TODO |
| engine layer | `src/engine/*` | `ls src` | TODO |
| workflow schema/config | `src/workflow/*` | `ls src` | TODO |

## Call Path Verification
| Path | Evidence command | Status |
|---|---|---|
| CLI -> monitor -> engine | `grep -R "fn main\|run\|monitor" src` | TODO |
| engine -> persistence | `grep -R "checkpoint\|event\|artifact" src` | TODO |

## Test Infrastructure Verification
| Component | Evidence command | Status |
|---|---|---|
| integration test harness | `ls tests` | TODO |
| fixture layout | `find tests/fixtures -maxdepth 3 -type f` | TODO |

## Baseline Integration Verification (STRICT)
| Baseline Component | Evidence command | Status | Notes |
|---|---|---|---|
| xtask QA gate exists | `cargo xtask qa --help || cargo xtask qa` | TODO | must remain primary quality gate |
| cargo aliases wired | `cat .cargo/config.toml` | TODO | verify alias compatibility after runtime wiring |
| PR quality workflow exists | `test -f .github/workflows/pr-quality.yml && echo OK` | TODO | runtime checks must be additive, not replacement |
| Release workflow exists | `test -f .github/workflows/release.yml && echo OK` | TODO | release flow remains xtask-driven |

## Blocking Issues Found
- [ ] None
- [ ] Issue 1: __________________
- [ ] Issue 2: __________________

## Verification Gate
- [ ] dagrs decision is explicit and feasible for stage 1
- [ ] All dependencies verified
- [ ] Dependency addition schedule is coherent with phase files
- [ ] All types/interfaces verified
- [ ] Call paths are feasible
- [ ] Test infrastructure is ready
- [ ] STRICT baseline integration with existing xtask/CI is preserved

**If any box is unchecked: STOP and update phase docs before implementation.**
