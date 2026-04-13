# Phase 04a: Behavioral TDD for Config Resolution and Validation Verification

## Phase ID
`PLAN-20260404-INITIAL-RUNTIME.P04A`

## Prerequisites

- Completion marker required: `project-plans/initial/plan/.completed/P04.md`
- Previous verification marker required: `project-plans/initial/plan/.completed/P03A.md` (except for phase 01, which requires `P00A.md`)

## Verification Scope

- Verify claimed files/changes from Phase 04
- Verify requirement markers and behavioral evidence
- Produce binary verdict: PASS or FAIL

## Verification Commands

```bash
grep -r "@plan:PLAN-20260404-INITIAL-RUNTIME.P04" src tests project-plans || true
cargo build --all-targets
cargo test
```

## Auditor Checklist

- [ ] Evidence file for phase 04 exists and includes command output
- [ ] Requirement text is actually satisfied by implementation/tests
- [ ] No skipped phases or missing prerequisite markers
- [ ] Verdict is binary PASS/FAIL (no partial pass)

## Completion Marker

Create: `project-plans/initial/plan/.completed/P04A.md`

```markdown
Phase: P04A
Verdict: PASS|FAIL
Findings:
- [itemized findings]
Commands:
- [exact commands + outputs]
```
