# Phase 07a: Engine Integration TDD Verification

## Phase ID

`PLAN-20260408-STEP-EXEC.P07A`

## Prerequisites

- Required: Phase 07 completed
- Verification: `.completed/P07.md` exists

## Verification Checklist

- [ ] `tests/hello_world_workflow_integration.rs` exists with plan markers
- [ ] `tests/fixtures/workflows/valid/hello-world-v1.toml` exists and parses correctly
- [ ] `tests/fixtures/workflow-configs/valid/hello-world-config.toml` exists and parses correctly
- [ ] 5+ integration tests present
- [ ] No `#[should_panic]` (no reverse testing)
- [ ] Tests assert real behavior: `RunOutcome::Success`, actual file creation, actual cargo test pass
- [ ] REQ-EXEC-007 has at least one dedicated test
- [ ] Hello-world fixtures validate against existing `parse_workflow_type_toml()` and `parse_workflow_config_toml()`

## Fixture Validation

```bash
# Verify fixtures parse (can be checked via a quick Rust snippet or existing config_binding tests)
cargo test --test config_binding_integration -- hello 2>&1 || true
# Or simply verify TOML is valid:
cat tests/fixtures/workflows/valid/hello-world-v1.toml | head -5
```

## Verdict Rules

- PASS: Tests exist, fixtures parse, tests assert real behavior, red phase confirmed
- FAIL: Missing tests, unparseable fixtures, or tests that would pass with empty implementation

## Phase Completion Marker

Create: `project-plans/next/plan/.completed/P07A.md`
