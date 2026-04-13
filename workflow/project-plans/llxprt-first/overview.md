# llxprt-first: First Real Workflow

## Goal

Get Luther to the point where it can pick up a real llxprt-code issue from GitHub, work it through planning → implementation → verification → PR submission, and close the loop — end to end, unattended.

This is not a demo or a hello-world. This is the first production workflow: Luther working on real bugs in a real codebase (vybestack/llxprt-code).

## What exists today

### Workflow engine (generic, workflow-agnostic)

The engine can load a workflow definition from TOML/JSON, execute steps sequentially, route transitions based on `StepOutcome` (success/fixable/fatal/retryable/abandon), persist checkpoints for resume, and dispatch steps to registered executors.

| Component | Status | Location |
|---|---|---|
| `WorkflowType` schema (steps, transitions, guards) | Done | `src/workflow/schema.rs` |
| `WorkflowConfig` schema (runtime, repo, guard limits) | Done | `src/workflow/schema.rs` |
| TOML/JSON config loading + validation | Done | `src/workflow/config_loader.rs` |
| `EngineRunner` (step loop, transition routing) | Done | `src/engine/runner.rs` |
| `StepExecutor` trait | Done | `src/engine/executor.rs` |
| `ExecutorRegistry` (step_type → executor dispatch) | Done | `src/engine/executor.rs` |
| `StepContext` (work_dir, run_id, variables, interpolation) | Done | `src/engine/executor.rs` |
| `ShellExecutor` (run command, capture output, map exit code) | Done | `src/engine/executors/shell.rs` |
| `WriteFileExecutor` (write files with interpolation) | Done | `src/engine/executors/write_file.rs` |
| `NoOpExecutor` (test convenience) | Done | `src/engine/executors/noop.rs` |
| Checkpoint persistence (SQLite) | Done | `src/persistence/checkpoint.rs` |
| Event logging | Done | `src/persistence/checkpoint.rs` |
| Run metadata storage | Done | `src/persistence/sqlite.rs` |
| Monitor + heartbeat + IPC | Done | `src/monitor/` |
| CLI (`run`, `status`, `service`) | Done | `src/cli/`, `src/main.rs` |
| 144 tests passing | Done | `src/` + `tests/` |

### What the engine does NOT know

The engine knows nothing about GitHub, issues, PRs, llxprt-code, Node.js, TypeScript, CodeRabbit, or any domain concept. It executes steps and routes on outcomes. This boundary is inviolable.

## The target workflow

Luther picks up a real llxprt-code bug and works it. The workflow ends at PR creation — CI watching and review remediation come later.

### Phase 1: Issue selection and workspace setup (deterministic)

1. **select_issue** — Query GitHub for the lowest-numbered open issue in the lowest-numbered release milestone (e.g., 0.10.0 before 0.11.0) that is not assigned. Assign it to the configured user. Add label "Luther working".
2. **fetch_issue** — Pull issue body and all comments. Write them to files in the working directory (`.luther/issue.md`, `.luther/comments.md`) so llxprt-code can read them naturally.
3. **setup_workspace** — Check out the target repo into a working directory. Create branch `issue{number}`.

### Phase 2: Planning (LLM, bounded loop)

4. **create_plan** — Invoke llxprt with a configurable planning profile. Ask it to produce a plan to address the issue.
5. **evaluate_plan** — Invoke llxprt with a configurable evaluating profile. Ask it to review the plan, suggest improvements, and respond with an exact pass/fail string (e.g., `PLAN_APPROVED` or `PLAN_NEEDS_REVISION`).
6. If the plan fails evaluation, loop back to `create_plan` (up to 5 iterations). If it passes, proceed.

### Phase 3: Implementation (LLM)

7. **implement** — Invoke llxprt with a configurable implementing profile. Give it the approved plan. It writes the code changes.

### Phase 4: Verification (deterministic + LLM remediation loop)

8. **evaluate_impl** — Invoke llxprt with the evaluating profile. Review the changes against the original issue. Respond with exact string (`IMPL_APPROVED` or `IMPL_NEEDS_WORK`).
9. **run_tests** — Deterministic step. Run the project's verification suite (lint, typecheck, test, format, build). Parse output into structured failure reports.
10. If tests fail, invoke llxprt with a configurable remediating profile, giving it the structured failure data. Loop back to `run_tests` (up to 5 iterations).

### Phase 5: PR submission (deterministic + LLM for description)

11. **push_changes** — `git add -A && git commit && git push` (pure git, deterministic).
12. **generate_pr_description** — Invoke llxprt to write a PR description from the changes, issue, and plan. The description must include "Fixes #{issue_number}".
13. **create_pr** — `gh pr create` with the generated title and description (pure gh, deterministic).

At any point, a fatal error or exceeded guard limit routes to **abandon_and_log** — comment on the issue explaining failure, clean up, record metrics.

**Scope boundary**: This plan stops at PR creation. CI watching, review parsing, and review remediation are future additions.

## Do we need dedicated executors?

Most of these steps are shell commands. The question is whether GhExecutor, GitExecutor, and LlxprtExecutor are genuinely different from ShellExecutor, or whether they're unnecessary wrappers.

### Honest assessment

| Would-be executor | What it actually does | Value over ShellExecutor |
|---|---|---|
| GhExecutor | Runs `gh` with `--json`, parses JSON into context variables | **Moderate** — the JSON parsing and variable extraction is real work, but it's a generic pattern ("run command, parse JSON output, set context vars") not gh-specific |
| GitExecutor | Runs `git checkout -b`, `git push`, etc. | **Low** — these are just shell commands with exit code checking |
| LlxprtExecutor | Runs `llxprt --profile-load {profile} -p "{goal}" --yolo` | **Low to moderate** — it's a shell command, but the prompt might be large (piped via stdin) and output needs to be scanned for specific pass/fail strings |

### Recommended approach: enhance ShellExecutor instead

Rather than building 3 thin wrappers, add two optional capabilities to ShellExecutor:

**1. JSON output parsing** (`output_format: "json"` + `context_map`)

```toml
[[steps]]
step_id = "fetch_issue"
step_type = "shell"

[steps.parameters]
command = "gh issue view {issue_number} --repo {target_repo} --json title,body,labels,comments"
output_format = "json"

[steps.parameters.context_map]
issue_title = ".title"
issue_body = ".body"
issue_labels = ".labels"
issue_comments = ".comments"
```

ShellExecutor runs the command, sees `output_format: "json"`, parses stdout as JSON, and extracts fields into context variables using the `context_map` (simple dot-path extraction, not jq). This covers all `gh --json` use cases without a dedicated GhExecutor.

**2. Stdin piping** (`stdin` parameter)

```toml
[[steps]]
step_id = "create_plan"
step_type = "shell"

[steps.parameters]
command = "llxprt --profile-load {profile_planning} -p - --yolo"
stdin = "Create a plan to fix issue #{issue_number}. Read .luther/issue.md for details."
```

Or for large prompts, pipe from a file:

```toml
stdin_file = ".luther/planning-prompt.txt"
```

This covers llxprt invocations without a dedicated LlxprtExecutor.

**3. Output string scanning** (`outcome_pattern`)

```toml
[[steps]]
step_id = "evaluate_plan"
step_type = "shell"

[steps.parameters]
command = "llxprt --profile-load {profile_evaluating} -p - --yolo"
stdin = "Evaluate this plan. If it is ready, respond with exactly PLAN_APPROVED on its own line. If not, respond with exactly PLAN_NEEDS_REVISION.\n\nPlan:\n(read .luther/plan.md)"
outcome_on_stdout = { "PLAN_APPROVED" = "success", "PLAN_NEEDS_REVISION" = "fixable" }
```

ShellExecutor scans stdout for the configured strings and maps to `StepOutcome`. If none match and exit code is 0, default to success. This lets the workflow definition control how llxprt's text output maps to routing decisions — no executor needs to understand what "PLAN_APPROVED" means.

### The one real executor: VerifyExecutor

VerifyExecutor is genuinely different. It runs *multiple* commands (lint, typecheck, test, format, build), each with a different output parser, and produces a structured JSON failure report. This isn't a single shell command with JSON output — it's an orchestration of checks with per-check-type parsing logic.

For the MVP target (llxprt-code, Node/TypeScript):
- lint (eslint or project lint script) → parse lint errors
- typecheck (`tsc --noEmit`) → parse TypeScript errors (file, line, code, message)
- test (vitest/jest `--reporter=json`) → parse test results (name, file, assertion, expected vs actual)
- format (`prettier --check`) → parse unformatted file list
- build (`npm run build`) → parse build errors

The check suite is a parameter. The executor has parsers per check type. Different projects use different suites — Rust support (cargo build/test/clippy) comes later when Luther works on itself.

Output goes into context as structured JSON and is also written to `.luther/verify-report.json` in the working directory so the remediating llxprt invocation can read it as a file.

### Summary: what to build

| Component | Type | Notes |
|---|---|---|
| **Enhanced ShellExecutor** | Modify existing | Add JSON output parsing (`context_map`), stdin piping (`stdin`/`stdin_file`), outcome pattern matching (`outcome_on_stdout`) |
| **VerifyExecutor** | New executor | Multi-command check runner with per-check-type output parsers, structured failure reports |
| **Namespaced context** | Engine enhancement | `{step_id.variable}` references in StepContext and interpolation |
| **Per-edge loop limits** | Engine enhancement | Per-transition max_iterations instead of single global counter |
| **Workflow TOML** | Data file | `config/workflows/llxprt-issue-fix-v1.toml` |
| **Workflow config** | Data file | `config/workflow-configs/llxprt-code.toml` with profile mappings |

## Engine enhancements (generic, not workflow-specific)

These are changes to the workflow engine itself. They are needed by this workflow but are general-purpose capabilities that any workflow could use.

### Namespaced context outputs

**Current state**: `StepContext` stores flat key-value pairs. When ShellExecutor runs, it sets `stdout` and `stderr`. Every step overwrites the previous step's values.

**Problem**: Step 13 (`create_pr`) needs to reference `{issue_number}` from step 1 and `{pr_description}` from step 12. If any step between them sets `stdout`, the value is lost.

**Solution**: Context variables are namespaced by step_id. `{fetch_issue.issue_body}` references the `issue_body` variable set during the `fetch_issue` step. Unnamespaced `{issue_body}` resolves by searching most-recent-first. Executors set variables as `step_id.key` internally; the interpolation resolver handles both forms.

This is a change to `StepContext` and `interpolate_string()` in `src/engine/executor.rs`, and to the runner's step dispatch in `src/engine/runner.rs` (to pass the current step_id to context). No workflow-specific logic.

### Per-edge loop limits

**Current state**: The engine tracks a single global `loop_count` and compares against `max_iterations` from config. Any backward transition increments the same counter.

**Problem**: This workflow has two independent loops: plan↔evaluate (up to 5) and test↔remediate (up to 5). With a global counter, 3 plan loops + 3 test loops = 6, which would exceed a limit of 5 — even though neither individual loop exceeded its limit.

**Solution**: `TransitionDef` gains an optional `max_iterations` field. The engine tracks loop counts per transition edge (keyed by `from:to`), not globally. The global `max_iterations` in `GuardLimits` becomes a fallback for edges without explicit limits. When an edge's count exceeds its limit, the engine returns `Abandoned`.

This is a change to `TransitionDef` in `src/workflow/schema.rs`, the loop tracking in `EngineRunner` in `src/engine/runner.rs`, and `StateSnapshot` in `src/persistence/checkpoint.rs` (to persist per-edge counts). No workflow-specific logic.

## Data flow: context variables vs files

Context variables (`{issue_number}`, `{branch_name}`) are for short values that get interpolated into commands and prompts. They flow through `StepContext`.

Large content — issue bodies, comment threads, plans, failure reports — should be **files in the working directory**, not context variables. llxprt-code reads files naturally; stuffing a 5000-word issue body into a `{variable}` for interpolation is fragile and wasteful.

The pattern:
- `fetch_issue` writes `.luther/issue.md` and `.luther/comments.md` to the work dir
- `create_plan` writes `.luther/plan.md`
- `evaluate_plan` reads `.luther/plan.md`, writes `.luther/plan-evaluation.md`
- `run_tests` writes `.luther/verify-report.json`
- `remediate` reads `.luther/verify-report.json`

Short metadata goes into context variables: `{issue_number}`, `{branch_name}`, `{pr_number}`. Large documents go to files. The prompt tells llxprt where to find them: "Read the issue in .luther/issue.md".

For steps that need to write files as output, the existing `WriteFileExecutor` or the shell step's natural file I/O handles this. No special mechanism needed.

## Profile configuration

Profiles are **two-level indirection**: workflow TOML → workflow config → llxprt profile.

The **workflow type** (TOML) references logical role names:

```toml
[[steps]]
step_id = "create_plan"
step_type = "shell"

[steps.parameters]
command = "llxprt --profile-load {profile_planning} -p - --yolo"
```

The **workflow config** maps role names to actual llxprt profile names:

```toml
[profiles]
profile_planning = "opusthinking"
profile_evaluating = "gpt54xhigh"
profile_implementing = "opusthinking"
profile_remediating = "sonnetthinking"
```

These map to files in `~/.llxprt/profiles/` (e.g., `opusthinking.json`). Luther doesn't create or manage these profiles — they're llxprt-code's config format. Luther just passes the resolved name to `--profile-load`.

This means:
- Changing which model does planning = edit the workflow config TOML
- The workflow definition never mentions specific models
- Different configs (dev vs prod, cheap vs quality) are just different workflow config files pointing to different profiles

## Workflow definition files

These are pure data — TOML files describing the workflow graph and its configuration. Zero code.

### `config/workflows/llxprt-issue-fix-v1.toml`

The workflow type definition. Contains all steps, their step_types, parameters, and the complete transition map. Replaces the current placeholder `issue-fix-v1.toml`.

### `config/workflow-configs/llxprt-code.toml`

The workflow instance config. Contains:
- `target_repo` — `"vybestack/llxprt-code"`
- `base_branch` — `"main"`
- `assignee` — `"acoliver"`
- `workspace_root` — where to check out the repo
- `branch_template` — `"issue{issue_number}"`
- Profile mappings (see above)
- Guard limits per loop edge: `plan_loop_max = 5`, `remediate_loop_max = 5`
- Verify checks: `["lint", "typecheck", "test", "format", "build"]`

## What must NOT be built

To maintain the engine/workflow separation:

- **No GitHub-aware code in the engine.** The engine does not import `gh`, parse issue JSON, or know what a PR is. Shell steps run `gh` commands; the engine just dispatches.
- **No llxprt-aware code in the engine.** The engine does not know what a "planning model" is. Shell steps invoke `llxprt` with profile names resolved from config.
- **No Node/TypeScript-aware code in the engine.** The engine does not know what `tsc` or `vitest` is. The VerifyExecutor is parameterized by check suite — it doesn't hardcode any project type.
- **No workflow-specific routing logic in the engine.** The plan↔evaluate loop and test↔remediate loop are expressed as transitions in the TOML with per-edge `max_iterations`. The engine just follows the table.
- **No hardcoded profiles.** Profile names are in the workflow config. If you want a different model for planning, change the config TOML.

The test: if you deleted every file in `config/workflows/` and `config/workflow-configs/`, the engine should still compile and all engine-level tests should still pass.

## Rough sizing

| Component | Estimated scope |
|---|---|
| Enhanced ShellExecutor (JSON parsing, stdin, outcome patterns) | ~200-350 LoC |
| VerifyExecutor (multi-check runner + parsers) | ~400-600 LoC |
| Namespaced context | ~100-150 LoC |
| Per-edge loop limits | ~100-200 LoC |
| Workflow TOML + config | ~150-250 lines of TOML |
| Tests | ~400-600 LoC |
| **Total new code** | **~1350-2150 LoC** |

## Dependencies

No new crate dependencies anticipated. All steps shell out to existing CLI tools. Parsing is done with `serde_json` (already a dependency). The engine enhancements modify existing code.

External tool requirements (must be on PATH at runtime):
- `gh` (GitHub CLI, authenticated)
- `git`
- `llxprt` (the llxprt-code CLI binary)
- `node`/`npm` (for the verify checks on the target project)

## Resolved questions

1. **Issue selection: "lowest module"** — Modules are GitHub milestones (e.g., `0.10.0` with 88 open issues, `0.11.0` with 55). The `select_issue` step queries `gh issue list --milestone "0.10.0" --state open --json number,title,assignees`, picks the lowest-numbered unassigned issue. If no unassigned issues remain in the lowest milestone, move to the next.
2. **llxprt output capture** — LLM steps instruct llxprt to write output to files in the working directory (e.g., "write your plan to .luther/plan.md"). File writing is reliable across all models. No stdout capture or `--output-format` needed for content. Stdout is only scanned for pass/fail signal strings.
3. **Pass/fail string format** — Evaluation prompts specify exact strings: `PLAN_APPROVED` / `PLAN_NEEDS_REVISION` and `IMPL_APPROVED` / `IMPL_NEEDS_WORK`. ShellExecutor's `outcome_on_stdout` maps these to success/fixable.
4. **Push and PR are separate steps** — `push_changes` is a pure git operation. `create_pr` is a pure gh operation. Two steps, two executors (both via ShellExecutor).
