# Design Constraints and Implementation Details

Non-behavioral rules enforced via ESLint, `scripts/check-quality.ts`, code
review, or configuration — not via unit tests. These were extracted from the
test requirements document (`llxprt-luther/REQUIREMENTS.md`) to keep that
document purely behavioral.

---

## Enforced by Tooling (ESLint / Biome)

| ID | Constraint | Enforcement |
| --- | --- | --- |
| DC-001 | `src/lib/ui.ts` is the only module permitted to call `console.*` | ESLint `no-console: error` with file-level override for `src/lib/ui.ts` and `scripts/**/*.ts` |

## Enforced by Architecture / Code Review

| ID | Constraint | Enforcement |
| --- | --- | --- |
| DC-002 | `GhClient` uses the `gh` CLI exclusively (not the GitHub REST API directly) | Code review / architecture rule |
| DC-003 | `saveState` writes to a temporary file first, then renames to the target path (atomic write), preventing partial writes on crash | Code review / architecture rule |
| DC-006 | Guard symbol names in source code (e.g., `canRetryPlan`, `canRetryTests`, `isFirstPush`, `hasCRComments`, `hasPRRelatedFailures`, `hasActionableComments`, `loopLimitReached`, `testFixLimitReached`) are implementation details; the normative guard behaviors are defined in `REQUIREMENTS.md` §1.5 | Code review / naming convention |

## Enforced by Configuration

| ID | Constraint | Enforcement |
| --- | --- | --- |
| DC-004 | CI check polling interval is configurable and defaults to a value between 30 and 300 seconds (inclusive) | Configuration; verified via config validation |
| DC-005 | Log file location is platform-dependent (e.g., `~/Library/Logs/luther/` on macOS, `$XDG_STATE_HOME/luther/` on Linux) and determined by configuration or platform detection | Configuration / code review |
