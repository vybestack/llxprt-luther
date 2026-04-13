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

Luther picks up a real llxprt-code bug and works it. The workflow steps, in order:

### Phase 1: Issue selection and workspace setup (deterministic)

1. **select_issue** — Query GitHub for the lowest-numbered open issue in the lowest-numbered module that is not assigned. Assign it to the configured user. Add label "Luther working".
2. **fetch_issue** — Pull issue body and all comments into context variables.
3. **setup_workspace** — Check out the target repo into a working directory. Create branch `issue{number}`.

### Phase 2: Planning (LLM, bounded loop)

4. **create_plan** — Invoke llxprt-code with a "planning" model profile. Ask it to produce a plan to address the issue.
5. **evaluate_plan** — Invoke llxprt-code with an "evaluating" model profile. Ask it to review the plan, suggest improvements, and pass or fail it.
6. If the plan fails evaluation, loop back to `create_plan` (up to 5 iterations). If it passes, proceed.

### Phase 3: Implementation (LLM)

7. **implement** — Invoke llxprt-code with an "implementing" model profile. Give it the approved plan. It writes the code changes.

### Phase 4: Verification (deterministic + LLM remediation loop)

8. **evaluate_impl** — Invoke llxprt-code with the "evaluating" model profile. Review the changes against the original issue. Does the implementation address it?
9. **run_tests** — Deterministic step. Run the project's verification suite (lint, typecheck, test, format, build). Parse output into structured failure reports.
10. If tests fail, invoke llxprt-code with a "remediating" model profile, giving it the structured failure data. Loop back to `run_tests` (up to 5 iterations).

### Phase 5: PR submission (deterministic + LLM for description)

11. **push_changes** — `git add`, `git commit`, `git push`.
12. **generate_pr_description** — Invoke llxprt-code to write a PR description from the changes, issue, and plan. The description must include "Fixes #{issue_number}".
13. **create_pr** — `gh pr create` with the generated title and description.
14. **watch_ci** — `gh pr checks --watch`. Loop until all workflows finish.
15. **record_result** — Log outcome metrics. Remove "Luther working" label.

At any point, a fatal error or exceeded guard limit routes to **abandon_and_log** — comment on the issue explaining failure, clean up, record metrics.

## What must be built

Everything below is needed to run this workflow. The critical constraint: **none of this goes into the engine**. The engine is generic. New executors are pluggable. The workflow is a TOML file.

### 1. New executors

Four new executor types, registered in the `ExecutorRegistry` alongside the existing `shell`, `write_file`, and `noop`.

#### GhExecutor (`step_type: "gh"`)

Wraps the `gh` CLI with structured JSON output parsing and context variable extraction.

**Why not just ShellExecutor?** Because `gh` returns JSON that needs to be parsed into typed context variables (`{issue_number}`, `{pr_url}`, etc.), issue selection needs filtering/sorting logic, and review comment parsing needs structured extraction. ShellExecutor just captures raw stdout — it doesn't understand what it ran.

MVP subcommands:
- `issue_list` — list/filter issues, pick one, set `{issue_number}`
- `issue_view` — fetch details, set `{issue_title}`, `{issue_body}`, `{issue_comments}`
- `issue_edit` — assign user, add/remove labels
- `issue_comment` — post a comment
- `pr_create` — create PR, capture `{pr_number}`, `{pr_url}`
- `pr_checks` — watch CI status, parse pass/fail

Each subcommand is a `command` parameter in the step definition. The executor dispatches internally based on this parameter. The engine doesn't know or care what `"gh"` means — it just dispatches to the registered executor.

#### GitExecutor (`step_type: "git"`)

Wraps git operations with proper error handling and context extraction.

MVP subcommands:
- `setup_branch` — fetch, create branch from base, validate clean state
- `commit_push` — add all, commit with message, push to remote
- `cleanup` — delete local and remote branch

Same dispatch pattern as GhExecutor. The executor translates parameters + subcommand into git CLI calls.

#### LlxprtExecutor (`step_type: "llxprt"`)

Invokes llxprt-code as a subprocess. This is the creative engine — planning, implementing, evaluating, remediating are all llxprt-code invocations with different model profiles.

Key behaviors:
- Receives a `profile` parameter that maps to a model profile name (e.g., `"planning"` → `--profile-load luther-planner`)
- Profile name mapping is in the workflow config, not hardcoded
- Receives a `goal` parameter — the prompt for llxprt-code (interpolated with context variables)
- Runs llxprt-code in the workflow's working directory
- Captures structured output (llxprt-code writes results to files or stdout)
- Maps exit code to `StepOutcome`: 0 → success, non-zero → fixable

The executor does not know what "planning" or "evaluating" means. It runs llxprt-code with a profile and a goal. The workflow TOML controls which profile each step uses.

#### VerifyExecutor (`step_type: "verify"`)

Runs a configurable sequence of project verification commands and parses output into structured failure reports. This replaces "ask the LLM if tests passed" with hard data.

For the MVP target (llxprt-code, Node/TypeScript):
- lint (eslint or project lint script)
- typecheck (`tsc --noEmit`)
- test (vitest/jest with JSON reporter)
- format (prettier --check)
- build (npm run build)

The check suite is a parameter, not hardcoded. The executor has parsers for each check type. Different projects use different suites — Rust support comes later when Luther works on itself.

Output goes into context as structured JSON: `{test_failures}`, `{build_errors}`, `{type_errors}`, `{lint_errors}`, `{verify_summary}`. The remediating model receives these instead of raw stderr.

### 2. Engine enhancements (generic, not workflow-specific)

These are changes to the workflow engine itself. They are needed by this workflow but are general-purpose capabilities that any workflow could use.

#### Namespaced context outputs

**Current state**: `StepContext` stores flat key-value pairs. When ShellExecutor runs, it sets `stdout` and `stderr`. Every step overwrites the previous step's values.

**Problem**: Step 14 (`create_pr`) needs to reference `{issue_number}` from step 1 and `{pr_description}` from step 12. If any step between them sets `stdout`, the value is lost.

**Solution**: Context variables are namespaced by step_id. `{fetch_issue.issue_body}` references the `issue_body` variable set during the `fetch_issue` step. Unnamespaced `{issue_body}` resolves by searching most-recent-first. Executors set variables as `step_id.key` internally; the interpolation resolver handles both forms.

This is a change to `StepContext` and `interpolate_string()` in `src/engine/executor.rs`, and to the runner's step dispatch in `src/engine/runner.rs` (to pass the current step_id to context). No workflow-specific logic.

#### Per-edge loop limits

**Current state**: The engine tracks a single global `loop_count` and compares against `max_iterations` from config. Any backward transition increments the same counter.

**Problem**: This workflow has two independent loops: plan↔evaluate (up to 5) and test↔remediate (up to 5). With a global counter, 3 plan loops + 3 test loops = 6, which would exceed a limit of 5 — even though neither individual loop exceeded its limit.

**Solution**: `TransitionDef` gains an optional `max_iterations` field. The engine tracks loop counts per transition edge (keyed by `from:to`), not globally. The global `max_iterations` in `GuardLimits` becomes a fallback for edges without explicit limits. When an edge's count exceeds its limit, the engine returns `Abandoned`.

This is a change to `TransitionDef` in `src/workflow/schema.rs`, the loop tracking in `EngineRunner` in `src/engine/runner.rs`, and `StateSnapshot` in `src/persistence/checkpoint.rs` (to persist per-edge counts). No workflow-specific logic.

#### Step-level outcome mapping (optional, evaluate later)

**Current state**: Executors decide the `StepOutcome` internally (e.g., ShellExecutor maps exit 0 → Success, non-zero → Fixable).

**Potential need**: Some steps may want configurable outcome mapping. For example, `evaluate_plan` returns pass/fail — the executor could return this as a context variable, and the transition table routes on it. Or the TOML could specify `on_output.pass = "success"`, `on_output.fail = "fixable"`.

This may not be needed if the LlxprtExecutor can natively map structured llxprt-code output to outcomes. Evaluate during implementation — don't over-engineer.

### 3. Workflow definition files

These are pure data — TOML files describing the workflow graph and its configuration. Zero code.

#### `config/workflows/llxprt-issue-fix-v1.toml`

The workflow type definition. Contains all steps, their step_types, parameters, and the complete transition map. Replaces the current placeholder `issue-fix-v1.toml`.

#### `config/workflow-configs/llxprt-code.toml`

The workflow instance config. Contains:
- `target_repo` — `"vybestack/llxprt-code"`
- `base_branch` — `"main"`
- `assignee` — `"acoliver"`
- `workspace_root` — where to check out the repo
- `branch_template` — `"issue{issue_number}"`
- Profile mappings: `planning = "luther-planner"`, `evaluating = "luther-evaluator"`, `implementing = "luther-implementer"`, `remediating = "luther-remediator"`
- Guard limits: `plan_loop_max = 5`, `remediate_loop_max = 5`, `max_iterations = 20` (global fallback)
- Verify checks: `["lint", "typecheck", "test", "format", "build"]`

### 4. Prompt templates

Each LLM step needs a prompt — the `goal` parameter in the step definition. These are interpolated strings that reference context variables.

Examples:
- `create_plan.goal`: "Create a plan to fix issue #{issue_number}: {issue_title}\n\n{issue_body}\n\nComments:\n{issue_comments}"
- `evaluate_plan.goal`: "Evaluate this plan. Suggest improvements. Reply PASS or FAIL.\n\n{create_plan.plan}"
- `implement.goal`: "Implement the following plan:\n\n{create_plan.plan}"
- `remediate.goal`: "The following tests failed:\n\n{run_tests.test_failures}\n\nFix the issues."

These live in the workflow TOML as step parameters. They are data, not code.

### 5. Model profile files

llxprt-code model profiles that configure which model and behavior each role uses. These are llxprt-code's concern, not Luther's — Luther just passes `--profile-load {name}`.

Needed profiles:
- `luther-planner` — planning model (likely a reasoning model, higher cost)
- `luther-evaluator` — evaluation model (could be same model, different system prompt)
- `luther-implementer` — implementation model (needs tool use, code writing)
- `luther-remediator` — remediation model (needs tool use, focused on fixing specific failures)

These are created in the llxprt-code repo, not in Luther.

## What must NOT be built

To maintain the engine/workflow separation:

- **No GitHub-aware code in the engine.** The engine does not import `gh`, parse issue JSON, or know what a PR is. That's the GhExecutor's job.
- **No llxprt-code-aware code in the engine.** The engine does not know what a "planning model" is. That's the LlxprtExecutor's job, configured by the workflow TOML.
- **No Node/TypeScript-aware code in the engine.** The engine does not know what `tsc` or `vitest` is. That's the VerifyExecutor's job, configured by the check suite parameter.
- **No workflow-specific routing logic in the engine.** The plan↔evaluate loop and test↔remediate loop are expressed as transitions in the TOML with per-edge `max_iterations`. The engine just follows the table.
- **No hardcoded profiles.** Profile names are in the workflow config. The executor reads them. If you want a different model for planning, change the TOML.

The test: if you deleted every file in `config/workflows/` and `config/workflow-configs/`, the engine should still compile and all engine-level tests should still pass. The engine does not depend on any specific workflow.

## Rough sizing

| Component | Estimated scope |
|---|---|
| GhExecutor | ~300-500 LoC (subcommand dispatch, JSON parsing, context setting) |
| GitExecutor | ~200-300 LoC (3 subcommands, simpler than gh) |
| LlxprtExecutor | ~200-400 LoC (subprocess management, output capture, profile resolution) |
| VerifyExecutor | ~400-600 LoC (5 check types with parsers, structured output) |
| Namespaced context | ~100-150 LoC (changes to StepContext + interpolate_string) |
| Per-edge loop limits | ~100-200 LoC (schema change, per-edge counter map, checkpoint update) |
| Workflow TOML + config | ~150-250 lines of TOML (pure data) |
| Prompt templates | Embedded in workflow TOML step parameters |
| Tests | ~500-800 LoC (unit + integration per executor, e2e workflow test) |
| **Total new code** | **~2000-3200 LoC** |

## Dependencies

No new crate dependencies anticipated. All executors shell out to existing CLI tools (`gh`, `git`, `llxprt-code`, `npm`/`node`). Parsing is done with `serde_json` (already a dependency). The engine enhancements modify existing code.

External tool requirements (must be on PATH at runtime):
- `gh` (GitHub CLI, authenticated)
- `git`
- `llxprt-code` (or however it's invoked — binary name TBD)
- `node`/`npm` (for the verify checks on the target project)

## Open questions

1. **How does llxprt-code get invoked?** What's the exact CLI? `llxprt --goal "..." --profile-load luther-planner`? Does it accept stdin? Does it write structured output to a file or stdout?
2. **How does llxprt-code signal pass/fail for evaluation?** Exit code? A specific output format? A file it writes?
3. **Issue selection: "lowest module"** — What defines module ordering? Is it a label like `module:core`, `module:tools`? A path prefix? How is priority determined?
4. **Should `create_pr` and `push_changes` be one step or two?** The description separates them, but `gh pr create` can push as part of PR creation.
5. **CI remediation** — Noted as "add later". Is the plan to loop `watch_ci → remediate → push → watch_ci`? Or just report CI failures for now?
