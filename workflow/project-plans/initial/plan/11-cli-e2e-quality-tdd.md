# Phase 11: Behavioral TDD for CLI End-to-End and Quality/Release Controls

## Phase ID
`PLAN-20260404-INITIAL-RUNTIME.P11`

## Prerequisites

- Required: Phase 10 verification completed
- Verification marker required: `project-plans/initial/plan/.completed/P10A.md`
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
- `tests/cli_e2e_integration.rs`
- `tests/quality_release_guardrails.rs`

### Files to Modify
- (none required)

### Required Code Markers

Every function/struct/test created in this phase must include markers:

```rust
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P11
/// @requirement:REQ-EARS-...
```

## Verification Commands

### Automated Checks

```bash
# Plan markers
grep -r "@plan:PLAN-20260404-INITIAL-RUNTIME.P11" src tests project-plans || true

# Test files exist
test -f tests/cli_e2e_integration.rs && echo "OK" || echo "FAIL: CLI e2e test missing"
test -f tests/quality_release_guardrails.rs && echo "OK" || echo "FAIL: quality test missing"

# Test counts
echo "cli_e2e tests: $(grep -c '#\[test\]\|#\[rstest\]\|#\[tokio::test\]' tests/cli_e2e_integration.rs)"
echo "quality tests: $(grep -c '#\[test\]\|#\[rstest\]' tests/quality_release_guardrails.rs)"

# Verify existing xtask/CI baseline is not broken
cargo xtask qa --help >/dev/null 2>&1 || cargo xtask qa 2>&1 | head -5
test -f .github/workflows/pr-quality.yml && echo "OK: pr-quality.yml exists" || echo "FAIL"
test -f .github/workflows/release.yml && echo "OK: release.yml exists" || echo "FAIL"

# Build compiles, tests expected to FAIL (TDD red phase)
cargo build --all-targets
cargo test 2>&1 | tail -20
echo "NOTE: test failures are EXPECTED in this TDD phase"
```

### Structural Verification Checklist

- [ ] Prerequisite marker exists (`P10A`)
- [ ] All listed files created/modified
- [ ] Plan + requirement markers present
- [ ] `cargo build` passes
- [ ] `cargo test` status matches phase expectation

### Semantic Verification Checklist

- [ ] Behavioral outcomes match the requirement text
- [ ] Tests would fail if implementation were removed
- [ ] Feature is reachable through planned call paths
- [ ] Behavioral tests are added first and fail naturally before implementation.
- [ ] STRICT baseline integration assertions are present (tests validate existing `cargo xtask qa` + release workflow contracts rather than redefining them).

## Success Criteria

- Required artifacts and code changes are complete.
- Verification checklist is fully satisfied.
- Evidence is written to completion marker file.

## Failure Recovery

1. Revert files changed only by this phase as needed.
2. Fix issues discovered by verification.
3. Re-run this phase; do not proceed until PASS.

## Phase Completion Marker

Create: `project-plans/initial/plan/.completed/P11.md`

```markdown
Phase: P11
Verdict: PASS|FAIL
Evidence:
- commands run
- test/build outputs
- file list changed
```
