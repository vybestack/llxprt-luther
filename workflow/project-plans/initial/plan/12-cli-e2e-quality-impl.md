# Phase 12: CLI Integration, End-to-End Runtime Wiring, and Quality Gate Enablement

## Phase ID
`PLAN-20260404-INITIAL-RUNTIME.P12`

## Prerequisites

- Required: Phase 11 verification completed
- Verification marker required: `project-plans/initial/plan/.completed/P11A.md`
- Preflight verification marker required: `project-plans/initial/plan/.completed/P00A.md`

## Requirements Implemented (Expanded)

### REQ-EARS-QUAL-001
**Full Text**: The project quality gate shall enforce formatting, clippy checks, structural guards, complexity checks, tests, and coverage through xtask/CI.
**Behavior**:
- GIVEN: the existing `cargo xtask qa` command and `.github/workflows/pr-quality.yml` are in place
- WHEN: runtime source code is added by this plan's phases
- THEN: `cargo xtask qa` still passes (fmt + clippy + test + coverage >= 80%), and all new runtime code is included in the checks without any quality gate weakening
**Why This Matters**: Protects maintainability/release safety with executable quality controls.

### REQ-EARS-QUAL-002
**Full Text**: The release process shall run through xtask commands for packaging, signing, publishing, and Homebrew tap update.
**Behavior**:
- GIVEN: the existing `cargo xtask release-all` command pipeline is functional
- WHEN: a release is prepared after runtime implementation is complete
- THEN: `cargo xtask release-all <tag>` packages the luther binary (now including runtime functionality) for distribution
**Why This Matters**: Protects maintainability/release safety with executable quality controls.

### REQ-EARS-QUAL-003
**Full Text**: When release is triggered by a valid tag, the release workflow shall invoke `cargo release-all <tag>`.
**Behavior**:
- GIVEN: `.github/workflows/release.yml` triggers on tag push matching `v*`
- WHEN: a tag `v0.1.0` is pushed to the repository
- THEN: the workflow invokes `cargo release-all v0.1.0` (verifiable by reading the workflow YAML `run:` step)
**Why This Matters**: Protects maintainability/release safety with executable quality controls.

### REQ-EARS-QUAL-004
**Full Text**: If required release secrets are missing, then release workflow execution shall fail before packaging/publish operations begin.
**Behavior**:
- GIVEN: the release workflow requires `APPLE_DEVELOPER_ID_APPLICATION` and `HOMEBREW_TAP_TOKEN` secrets
- WHEN: a release is triggered in an environment missing these secrets
- THEN: the workflow fails at the secrets-validation step BEFORE any packaging, signing, or publish operations execute
**Why This Matters**: Protects maintainability/release safety with executable quality controls.

## Implementation Tasks

### Files to Create
- `src/cli/mod.rs`

### Files to Modify
- `Cargo.toml`
- `src/main.rs`
- `xtask/src/main.rs`
- `.cargo/config.toml`
- `.github/workflows/pr-quality.yml`
- `.github/workflows/release.yml`

## Required dependency additions in this phase

- `clap`

### Required Code Markers

Every function/struct/test created in this phase must include markers:

```rust
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
/// @requirement:REQ-EARS-...
```

- Pseudocode reference: integrate `config-loading`, `engine-runner`, `monitor-loop`, and `repository-prep` sequences
## Verification Commands

### Automated Checks

```bash
# Plan markers
grep -r "@plan:PLAN-20260404-INITIAL-RUNTIME.P12" src tests project-plans || true

# CLI module exists
test -f src/cli/mod.rs && echo "OK" || echo "FAIL: cli/mod.rs missing"

# clap dependency
grep -q 'clap' Cargo.toml || echo "FAIL: clap not in Cargo.toml"

# ALL P11 TDD tests must pass
cargo test --test cli_e2e_integration
cargo test --test quality_release_guardrails

# Existing quality gate still works
cargo xtask qa

# No stubs or placeholders anywhere
grep -rn "todo!\|unimplemented!" src/ && echo "FAIL: stubs remain" || echo "OK: no stubs in src/"
grep -rn "// TODO\|// FIXME\|// HACK" src/ && echo "FAIL: deferred work" || echo "OK: no deferred markers"

cargo build --all-targets
cargo test
```


### Deferred Implementation Detection (MANDATORY)

```bash
grep -rn "todo!\|unimplemented!" src tests
# Expected: no matches in implementation targets

grep -rn "// TODO\|// FIXME\|// HACK" src tests
# Expected: no matches in implementation targets
```
### Structural Verification Checklist

- [ ] Prerequisite marker exists (`P11A`)
- [ ] All listed files created/modified
- [ ] Plan + requirement markers present
- [ ] `cargo build` passes
- [ ] `cargo test` status matches phase expectation

### Semantic Verification Checklist

- [ ] Behavioral outcomes match the requirement text
- [ ] Tests would fail if implementation were removed
- [ ] Feature is reachable through planned call paths
- [ ] All preceding TDD tests pass without weakening tests or adding placeholders.
- [ ] STRICT baseline integration preserved: runtime additions extend existing `xtask` + CI gates without replacing established commands or weakening thresholds.

## Success Criteria

- Required artifacts and code changes are complete.
- Verification checklist is fully satisfied.
- Evidence is written to completion marker file.

## Failure Recovery

1. Revert files changed only by this phase as needed.
2. Fix issues discovered by verification.
3. Re-run this phase; do not proceed until PASS.

## Phase Completion Marker

Create: `project-plans/initial/plan/.completed/P12.md`

```markdown
Phase: P12
Verdict: PASS|FAIL
Evidence:
- commands run
- test/build outputs
- file list changed
```
