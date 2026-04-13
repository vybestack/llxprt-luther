# Phase 07a: Behavioral TDD for Engine Execution, Loops, and Persistence Verification

## Phase ID
`PLAN-20260404-INITIAL-RUNTIME.P07A`

## Prerequisites

- Completion marker required: `project-plans/initial/plan/.completed/P07.md`
- Previous verification marker required: `project-plans/initial/plan/.completed/P06A.md` (except for phase 01, which requires `P00A.md`)

## Verification Scope

- Verify claimed files/changes from Phase 07
- Verify requirement markers and behavioral evidence
- Produce binary verdict: PASS or FAIL

## Verification Commands

```bash
grep -r "@plan:PLAN-20260404-INITIAL-RUNTIME.P07" src tests project-plans || true
cargo build --all-targets
cargo test
```

## Auditor Checklist

- [ ] Evidence file for phase 07 exists and includes command output
- [ ] Requirement text is actually satisfied by implementation/tests
- [ ] No skipped phases or missing prerequisite markers
- [ ] Verdict is binary PASS/FAIL (no partial pass)

## Completion Marker

Create: `project-plans/initial/plan/.completed/P07A.md`

```markdown
Phase: P07A
Verdict: PASS|FAIL
Findings:
- [itemized findings]
Commands:
- [exact commands + outputs]
```
