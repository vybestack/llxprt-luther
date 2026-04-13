# Phase 11a: Behavioral TDD for CLI End-to-End and Quality/Release Controls Verification

## Phase ID
`PLAN-20260404-INITIAL-RUNTIME.P11A`

## Prerequisites

- Completion marker required: `project-plans/initial/plan/.completed/P11.md`
- Previous verification marker required: `project-plans/initial/plan/.completed/P10A.md` (except for phase 01, which requires `P00A.md`)

## Verification Scope

- Verify claimed files/changes from Phase 11
- Verify requirement markers and behavioral evidence
- Produce binary verdict: PASS or FAIL

## Verification Commands

```bash
grep -r "@plan:PLAN-20260404-INITIAL-RUNTIME.P11" src tests project-plans || true
cargo build --all-targets
cargo test
```

## Auditor Checklist

- [ ] Evidence file for phase 11 exists and includes command output
- [ ] Requirement text is actually satisfied by implementation/tests
- [ ] No skipped phases or missing prerequisite markers
- [ ] Verdict is binary PASS/FAIL (no partial pass)

## Completion Marker

Create: `project-plans/initial/plan/.completed/P11A.md`

```markdown
Phase: P11A
Verdict: PASS|FAIL
Findings:
- [itemized findings]
Commands:
- [exact commands + outputs]
```
