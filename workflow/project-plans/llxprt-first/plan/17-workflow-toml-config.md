# Phase 17: Workflow TOML + Config Files

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P17`

## Prerequisites

- Required: Phase 16a (Engine Integration Verification) completed
- Verification: `grep -r "@plan:PLAN-20260408-LLXPRT-FIRST.P16" tests/`
- Expected: All engine integration tests pass, all components wired together

## Purpose

Create the actual workflow definition and workflow instance config files that encode the complete llxprt-code issue-fix workflow. These are pure TOML data files — zero Rust code. They define the workflow graph (steps, transitions, loop limits) and the runtime configuration (profiles, guard limits, repo settings).

This phase also creates test fixture copies of these files so integration tests can validate parsing.

## Requirements Implemented (Expanded)

### REQ-LF-PROF-001: Logical role names in workflow type

**Full Text**: The workflow type definition shall reference model profiles by logical role names (e.g., `{profile_planning}`) rather than concrete llxprt profile names.
**Behavior**:
- GIVEN: The workflow type TOML file
- WHEN: A step's command references a profile
- THEN: It uses `{profile_planning}`, `{profile_evaluating}`, etc. — not `opusthinking` or `gpt54xhigh`

### REQ-LF-PROF-002: Config maps logical roles to concrete profiles

**Full Text**: The workflow instance config shall map logical role names to concrete llxprt profile names (e.g., `profile_planning = "opusthinking"`).
**Behavior**:
- GIVEN: The workflow config TOML file
- WHEN: Read the `[variables]` section
- THEN: Contains `profile_planning`, `profile_evaluating`, `profile_implementing`, `profile_remediating` mapped to concrete profile names

### REQ-LF-DATA-001: Short metadata via context variables

**Full Text**: Short metadata values (issue number, branch name, PR number, pass/fail signals) shall flow through StepContext variables.
**Behavior**:
- GIVEN: The workflow type steps
- WHEN: A step sets `issue_number` via `context_map` on JSON output
- THEN: Later steps reference it as `{issue_number}` in commands and parameters

### REQ-LF-DATA-002: Large content via working directory files

**Full Text**: Large content (issue body, comments, plans, failure reports, PR descriptions) shall be written to files in the working directory under a `.luther/` subdirectory.
**Behavior**:
- GIVEN: The `fetch_issue` step retrieves issue data
- WHEN: The step completes
- THEN: Issue body is at `.luther/issue.md`, comments at `.luther/comments.md` in the working directory

### REQ-LF-DATA-003: LLM prompts reference files, not context variables

**Full Text**: LLM step prompts shall instruct llxprt to read input from and write output to specific files in the working directory, not through context variable interpolation of large content.
**Behavior**:
- GIVEN: The `create_plan` step's prompt (via stdin)
- WHEN: Examined
- THEN: It says "Read the issue in .luther/issue.md" rather than interpolating `{issue_body}` as a large string

### REQ-LF-ISSUE-001 through REQ-LF-ISSUE-004: Issue selection steps

These requirements define the `select_issue` step behavior — the TOML specifies the `gh` commands and context_map extraction to implement them.

### REQ-LF-FETCH-001 through REQ-LF-FETCH-004: Fetch issue steps

These requirements define the `fetch_issue` step — the TOML specifies gh commands, file writes, and context_map.

### REQ-LF-WS-001 through REQ-LF-WS-004: Workspace setup steps

These requirements define the `setup_workspace` step — the TOML specifies git commands and branch creation.

### REQ-LF-PLAN-001 through REQ-LF-PLAN-005: Planning loop steps and transitions

These requirements define `create_plan`, `evaluate_plan`, and the loop-back transition with `max_iterations: 5`.

### REQ-LF-IMPL-001 through REQ-LF-IMPL-003: Implementation and evaluation steps

These requirements define `implement` and `evaluate_impl` steps.

### REQ-LF-TEST-001 through REQ-LF-TEST-003: Test and remediation loop

These requirements define `run_tests` (verify executor), `remediate`, and the loop-back transition with `max_iterations: 5`.

### REQ-LF-PR-001 through REQ-LF-PR-004: PR submission steps

These requirements define `push_changes`, `generate_pr_description`, and `create_pr` as separate steps.

### REQ-LF-FAIL-001 through REQ-LF-FAIL-005: Failure and abandonment

These requirements define the `abandon_and_log` and `log_completion` terminal steps, and the fatal/abandon transitions.

### REQ-LF-SCOPE-001: Stops at PR creation

**Full Text**: This plan shall stop at PR creation. CI watching, review parsing, and review remediation are out of scope.

### REQ-LF-SCOPE-002: Node/TypeScript only for MVP

**Full Text**: The VerifyExecutor shall implement Node/TypeScript check parsers for the MVP. Rust check parsers are out of scope for this plan.

## Implementation Tasks

### Files to Create

- `config/workflows/llxprt-issue-fix-v1.toml` — Workflow type definition
- `config/workflow-configs/llxprt-code.toml` — Workflow instance config
- `tests/fixtures/workflows/valid/llxprt-issue-fix-v1.toml` — Test fixture copy of workflow type
- `tests/fixtures/workflow-configs/valid/llxprt-code.toml` — Test fixture copy of workflow config

### Workflow Type: `config/workflows/llxprt-issue-fix-v1.toml`

The complete workflow definition with all steps, parameters, and transitions.

#### Steps (14 total)

| Step ID | Step Type | Purpose | Key Parameters |
|---|---|---|---|
| `select_issue` | shell | Query GitHub for open issue in lowest milestone, assign it | `output_format: "json"`, `context_map` for issue_number/title, `exit_code_map` maps exit 1→fatal |
| `setup_workspace` | shell | Clone or fetch+reset repo, create branch, create .luther/ | git clone/fetch/reset/checkout, mkdir |
| `fetch_issue` | shell | Pull issue body/comments, write directly to .luther/ files | `output_format: "json"`, `context_map` for title/url, shell writes `.luther/issue.md` and `.luther/comments.md` |
| `create_plan` | shell | Invoke llxprt with planning profile | `stdin` with prompt referencing .luther/issue.md, `{profile_planning}` |
| `evaluate_plan` | shell | Invoke llxprt to review plan | `stdin` with evaluation prompt, `outcome_on_stdout` |
| `implement` | shell | Invoke llxprt to write code | `stdin` referencing .luther/plan.md, `{profile_implementing}` |
| `evaluate_impl` | shell | Invoke llxprt to review implementation | `stdin` with eval prompt, `outcome_on_stdout` |
| `run_tests` | verify | Run lint, typecheck, test, format, build | `checks` array |
| `remediate` | shell | Invoke llxprt to fix test failures | `stdin` referencing .luther/verify-report.json, `{profile_remediating}` |
| `push_changes` | shell | git add, commit, push | pure git |
| `generate_pr_description` | shell | Invoke llxprt to write PR description | `stdin` with prompt, writes .luther/pr-description.md |
| `create_pr` | shell | gh pr create with --body-file | pure gh |
| `abandon_and_log` | shell | Comment on issue, remove label, unassign | gh commands |
| `log_completion` | shell | Record success outcome | echo/logging |

**Note**: The original `write_issue_files` step has been eliminated. The `fetch_issue` step writes large content (issue body, comments) directly to `.luther/` files via shell I/O redirection, and only extracts short metadata (title, url) through `context_map`. This avoids shuttling large content through context variables (REQ-LF-DATA-001, REQ-LF-DATA-002).

#### Transitions (~24)

| From | To | Condition | max_iterations |
|---|---|---|---|
| select_issue | setup_workspace | (success, default) | — |
| setup_workspace | fetch_issue | (success, default) | — |
| fetch_issue | create_plan | (success, default) | — |
| create_plan | evaluate_plan | (success, default) | — |
| evaluate_plan | implement | success (via `outcome_on_stdout` → Success) | — |
| evaluate_plan | create_plan | fixable (via `outcome_on_stdout` → Fixable) | **5** |
| implement | evaluate_impl | (success, default) | — |
| evaluate_impl | run_tests | success (via `outcome_on_stdout` → Success) | — |
| evaluate_impl | implement | fixable (via `outcome_on_stdout` → Fixable) | 5 |
| run_tests | push_changes | success | — |
| run_tests | remediate | fixable | — |
| remediate | run_tests | (success, default) | **5** |
| push_changes | generate_pr_description | (success, default) | — |
| generate_pr_description | create_pr | (success, default) | — |
| create_pr | log_completion | (success, default) | — |
| select_issue | abandon_and_log | fatal | — |
| fetch_issue | abandon_and_log | fatal | — |
| setup_workspace | abandon_and_log | fatal | — |
| create_plan | abandon_and_log | fatal | — |
| evaluate_plan | abandon_and_log | fatal | — |
| implement | abandon_and_log | fatal | — |
| evaluate_impl | abandon_and_log | fatal | — |
| run_tests | abandon_and_log | fatal | — |
| remediate | abandon_and_log | fatal | — |
| push_changes | abandon_and_log | fatal | — |
| create_pr | abandon_and_log | fatal | — |

#### TOML Structure

The full TOML structure follows the detailed step parameters above. See the "Detailed Step Parameters" section for the concrete commands, context_map definitions, exit_code_map/outcome_on_stdout mappings for every step. The TOML file will be a direct transcription of those specifications into the `[[steps]]` and `[[transitions]]` arrays.

### Workflow Config: `config/workflow-configs/llxprt-code.toml`

```toml
# llxprt-code Workflow Configuration
# @plan:PLAN-20260408-LLXPRT-FIRST.P17
# @requirement:REQ-LF-PROF-001,REQ-LF-PROF-002

config_id = "llxprt-code"
workflow_type_id = "llxprt-issue-fix-v1"

[runtime]
timeout_seconds = 7200
max_retries = 3

[repository]
workspace_strategy = "temp_clone"
branch_template = "issue{issue_number}"
base_branch = "main"

[guards]
max_iterations = 10
max_file_changes = 200
max_tokens = 500000
max_cost = 50.0

[variables]
target_repo = "vybestack/llxprt-code"
assignee = "acoliver"
work_dir = "/tmp/luther-workspaces/llxprt-code"
base_branch = "main"
profile_planning = "opusthinking"
profile_evaluating = "deepthinker"
profile_implementing = "typescriptexpert"
profile_remediating = "typescriptexpert"
luther_label = "Luther working"
```

### Detailed Step Parameters

Each step's full TOML parameters are specified below with concrete commands and data flow.

#### select_issue (REQ-LF-ISSUE-001 through REQ-LF-ISSUE-004)

This step implements milestone-ordered issue selection. It is a multi-command shell step that:
1. Queries milestones via `gh api`, sorts by semver (`sort -V`), picks the lowest-versioned
2. For that milestone, lists open unassigned issues sorted by number, picks the lowest-numbered
3. If no issues in that milestone, tries the next milestone up
4. Filters out issues that already have an open PR or are assigned (skips in-progress work)
5. Assigns the selected issue and adds the "Luther working" label
6. Outputs JSON with `number` and `title` for `context_map` extraction

**Outcome detection design**: Uses exit codes for outcome routing and `output_format: "json"` + `context_map` for data extraction on the success path. This cleanly separates concerns:
- **Success (exit 0)**: Script outputs JSON to stdout. ShellExecutor parses it via `output_format = "json"` and extracts `issue_number`/`issue_title` via `context_map`. Outcome defaults to `Success`.
- **Fatal (exit 1)**: Script prints a diagnostic message to stderr and exits 1. ShellExecutor maps exit code 1 to `Fatal` via `exit_code_map`. No JSON parsing or `context_map` extraction is attempted because the step already has a non-Success outcome.

The `exit_code_map` is a new parameter for ShellExecutor (added in this phase's design) that maps specific non-zero exit codes to specific `StepOutcome` values. Without `exit_code_map`, the default behavior is: exit 0 → Success, non-zero → Fixable. With `exit_code_map`, specific exit codes can be routed to `Fatal`, `Fixable`, or any other outcome. This avoids the antipattern of mixing `outcome_on_stdout` with `output_format: "json"` (which would require scanning JSON text for sentinel strings).

**ShellExecutor execution order** (unchanged from Phase 05 defaults):
1. Run command, capture exit code, stdout, stderr
2. Check `exit_code_map`: if exit code has a mapping → return that outcome immediately
3. Check exit code: non-zero with no `exit_code_map` match → Fixable (existing default)
4. Check `outcome_on_stdout`: if match → return matched outcome
5. Check `output_format == "json"`: parse JSON, apply `context_map`
6. Default → Success

```toml
[[steps]]
step_id = "select_issue"
step_type = "shell"
description = "Select lowest-numbered unassigned issue in lowest-versioned milestone"

[steps.parameters]
command = """
set -euo pipefail

# Query milestones sorted by semver (sort -V)
MILESTONES=$(gh api repos/{target_repo}/milestones --jq '.[].title' | sort -V)

SELECTED_ISSUE=""
for MILESTONE in $MILESTONES; do
  # List open issues, filter out already-assigned and those with existing PRs
  ISSUES=$(gh issue list --repo {target_repo} \
    --milestone "$MILESTONE" --state open --assignee "" \
    --json number,title --limit 20 \
    --jq 'sort_by(.number)')

  for ROW in $(echo "$ISSUES" | jq -c '.[]'); do
    NUM=$(echo "$ROW" | jq -r '.number')
    # Skip issues that already have an open PR (REQ-LF-ISSUE-004)
    PR_COUNT=$(gh pr list --repo {target_repo} --search "issue:$NUM" --state open --json number --jq 'length')
    if [ "$PR_COUNT" = "0" ]; then
      SELECTED_ISSUE="$ROW"
      break 2
    fi
  done
done

if [ -z "$SELECTED_ISSUE" ]; then
  echo "FATAL: No eligible unassigned issues found in any milestone" >&2
  exit 1
fi

ISSUE_NUMBER=$(echo "$SELECTED_ISSUE" | jq -r '.number')
ISSUE_TITLE=$(echo "$SELECTED_ISSUE" | jq -r '.title')

gh issue edit "$ISSUE_NUMBER" --repo {target_repo} \
  --add-assignee {assignee} --add-label "{luther_label}"

# Output JSON for context_map extraction (only on success path)
jq -n --argjson num "$ISSUE_NUMBER" --arg title "$ISSUE_TITLE" \
  '{number: $num, title: $title}'
"""
output_format = "json"

[steps.parameters.exit_code_map]
1 = "fatal"

[steps.parameters.context_map]
issue_number = ".number"
issue_title = ".title"
```

Key design decisions:
- `sort -V` for semver milestone ordering (REQ-LF-ISSUE-001)
- `--assignee ""` filters to unassigned issues only (REQ-LF-ISSUE-002)
- Loops through milestones in ascending version order (REQ-LF-ISSUE-001)
- Skips issues with existing open PRs (REQ-LF-ISSUE-004: skip already-in-progress issues)
- **Exit code for outcome routing**: exit 1 → `Fatal` via `exit_code_map`, exit 0 → `Success` with JSON output parsed by `context_map`. No mixing of `outcome_on_stdout` with `output_format: "json"` — each mechanism has a single clear responsibility.
- `exit_code_map` is a new ShellExecutor parameter: `{ exit_code_int = "outcome_string" }`. It is checked before the default non-zero→Fixable fallback, allowing specific exit codes to map to `Fatal` or other outcomes. Unmapped non-zero codes still default to `Fixable`.
- On fatal path: diagnostic goes to stderr (not stdout), so stdout is empty/irrelevant — no JSON parsing attempted because `exit_code_map` already resolved the outcome.
- On success path: `jq -n` produces clean JSON output, avoiding `printf` string-escaping pitfalls with issue titles containing special characters.

#### setup_workspace (REQ-LF-WS-001 through REQ-LF-WS-004)

This step ensures a clean repo checkout exists in `{work_dir}`, then creates the issue branch. Two cases:
- **work_dir does not exist or has no `.git/`**: Clone the repo into `{work_dir}`.
- **work_dir exists with `.git/`**: Fetch origin and hard-reset to `{base_branch}`.

Then: create the issue branch and the `.luther/` artifact directory.

**This step MUST run before `fetch_issue`** because `fetch_issue` writes to `.luther/` files inside `{work_dir}`, which requires the workspace and `.luther/` directory to exist first.

```toml
[[steps]]
step_id = "setup_workspace"
step_type = "shell"
description = "Ensure repo is checked out, create issue branch, create .luther/ dir"

[steps.parameters]
command = """
set -euo pipefail

if [ ! -d "{work_dir}/.git" ]; then
  git clone https://github.com/{target_repo}.git {work_dir}
fi

cd {work_dir}
git fetch origin
git checkout {base_branch}
git reset --hard origin/{base_branch}
git checkout -b issue{issue_number}
mkdir -p .luther
"""
```

Key design decisions:
- Tests for `{work_dir}/.git` (not `.git` relative to cwd) to determine if clone is needed (REQ-LF-WS-001)
- `git clone` creates the directory if it doesn't exist; `git fetch + reset --hard` refreshes it if it does (REQ-LF-WS-001)
- Branch `issue{issue_number}` created from the freshly-reset base branch (REQ-LF-WS-002)
- `.luther/` directory created for workflow artifacts (REQ-LF-WS-004)
- Non-zero exit from any command → `Fixable` via ShellExecutor default behavior; no `fixable` transition is defined from `setup_workspace`, so `resolve_next_step()` returns `None` and the engine returns `RunOutcome::Failure`. The `condition = "fatal"` transition from `setup_workspace` to `abandon_and_log` is only followed when the outcome is `Fatal`, not `Fixable`. If setup failures should route to `abandon_and_log`, either add `exit_code_map` to map specific exit codes to `fatal`, or add a `fixable` transition (REQ-LF-WS-003)

#### fetch_issue (REQ-LF-FETCH-001 through REQ-LF-FETCH-004)

This step fetches issue data via `gh issue view --json` and splits the result into two outputs:
- **Large content → files**: Issue body → `.luther/issue.md`, comments → `.luther/comments.md` (with thread structure preserved per REQ-LF-FETCH-003). These are written directly by the shell command via `jq` + I/O redirection — never stored in context variables.
- **Short metadata → context**: Title and URL are emitted as the command's JSON stdout, then extracted by `context_map` into `issue_title` and `issue_url`.

The `issue_number` variable is already in context from `select_issue` — it is not re-extracted here. This satisfies REQ-LF-FETCH-003 (`issue_number`, `issue_title`, `issue_url` available for later steps) because `issue_number` persists in context from the prior step, and `issue_title`/`issue_url` are set by this step's `context_map`.

**Runs after `setup_workspace`** so that `{work_dir}/.luther/` already exists.

```toml
[[steps]]
step_id = "fetch_issue"
step_type = "shell"
description = "Fetch issue body and comments, write to .luther/ files, extract metadata to context"

[steps.parameters]
command = """
set -euo pipefail

# Fetch full issue data as JSON
gh issue view {issue_number} --repo {target_repo} \
  --json title,body,comments,url > .luther/issue-raw.json

# Write issue body to file
jq -r '.body // ""' .luther/issue-raw.json > .luther/issue.md

# Write comments with thread structure preserved (REQ-LF-FETCH-003)
jq -r '.comments[] | "## Comment by \\(.author.login) at \\(.createdAt)\\n\\n\\(.body)\\n\\n---\\n"' \
  .luther/issue-raw.json > .luther/comments.md

# Emit short metadata as JSON for context_map
jq '{title: .title, url: .url}' .luther/issue-raw.json
"""
output_format = "json"

[steps.parameters.context_map]
issue_title = ".title"
issue_url = ".url"
```

Key design decisions:
- `gh issue view --json title,body,comments,url` fetches exactly the fields needed (REQ-LF-FETCH-001)
- Large content (body, comments) is written directly to `.luther/` files by shell I/O redirection — never stored in context variables (REQ-LF-DATA-002)
- Comments written with author, timestamp, and body preserving thread structure (REQ-LF-FETCH-003)
- Only short metadata (title, url) flows through `context_map` into context variables (REQ-LF-DATA-001)
- The original `write_issue_files` step is **eliminated** — `fetch_issue` writes files directly, avoiding the data flow problem of shuttling large content through context variables to a separate write_file step

#### create_plan (REQ-LF-PLAN-001)

```toml
[[steps]]
step_id = "create_plan"
step_type = "shell"
description = "Invoke llxprt to create a plan for the issue"

[steps.parameters]
command = "llxprt --profile-load {profile_planning} -p - --yolo"
stdin = "Read the issue in .luther/issue.md and comments in .luther/comments.md. Create a detailed implementation plan to fix this issue. Write your plan to .luther/plan.md."
```

Key design decisions:
- Prompt references files, not interpolated content (REQ-LF-DATA-003)
- Uses `stdin` for the prompt (REQ-LF-SHELL-003)
- Profile is a logical role variable (REQ-LF-PROF-001)

#### evaluate_plan (REQ-LF-PLAN-002 through REQ-LF-PLAN-004)

```toml
[[steps]]
step_id = "evaluate_plan"
step_type = "shell"
description = "Invoke llxprt to evaluate the plan"

[steps.parameters]
command = "llxprt --profile-load {profile_evaluating} -p - --yolo"
stdin = "Read the issue in .luther/issue.md and the plan in .luther/plan.md. Evaluate whether the plan adequately addresses the issue. If the plan is ready, respond with exactly PLAN_APPROVED on its own line. If it needs revision, respond with exactly PLAN_NEEDS_REVISION on its own line, followed by your feedback. Write any detailed feedback to .luther/plan-evaluation.md."

[steps.parameters.outcome_on_stdout]
PLAN_APPROVED = "success"
PLAN_NEEDS_REVISION = "fixable"
```

#### implement (REQ-LF-IMPL-001)

```toml
[[steps]]
step_id = "implement"
step_type = "shell"
description = "Invoke llxprt to implement the plan"

[steps.parameters]
command = "llxprt --profile-load {profile_implementing} -p - --yolo"
stdin = "Read the approved plan in .luther/plan.md. Implement the changes described in the plan. Make all code changes directly in the working directory."
```

#### evaluate_impl (REQ-LF-IMPL-002, REQ-LF-IMPL-003)

```toml
[[steps]]
step_id = "evaluate_impl"
step_type = "shell"
description = "Invoke llxprt to evaluate the implementation"

[steps.parameters]
command = "llxprt --profile-load {profile_evaluating} -p - --yolo"
stdin = "Read the issue in .luther/issue.md and the plan in .luther/plan.md. Review the code changes (use git diff). If the implementation correctly addresses the issue and plan, respond with exactly IMPL_APPROVED on its own line. If it needs work, respond with exactly IMPL_NEEDS_WORK on its own line, followed by your feedback."

[steps.parameters.outcome_on_stdout]
IMPL_APPROVED = "success"
IMPL_NEEDS_WORK = "fixable"
```

#### run_tests (REQ-LF-TEST-001)

```toml
[[steps]]
step_id = "run_tests"
step_type = "verify"
description = "Run lint, typecheck, test, format, build checks"

[steps.parameters]
checks = ["lint", "typecheck", "test", "format", "build"]
```

#### remediate (REQ-LF-TEST-002)

```toml
[[steps]]
step_id = "remediate"
step_type = "shell"
description = "Invoke llxprt to fix test/lint/build failures"

[steps.parameters]
command = "llxprt --profile-load {profile_remediating} -p - --yolo"
stdin = "Read the verification failure report in .luther/verify-report.json. Fix all failing checks. The report contains structured error details including file paths, line numbers, and error messages."
```

#### push_changes (REQ-LF-PR-001)

```toml
[[steps]]
step_id = "push_changes"
step_type = "shell"
description = "Stage, commit, and push changes"

[steps.parameters]
command = "git add -A && git commit -m "Fix #{issue_number}: {issue_title}" && git push -u origin issue{issue_number}"
```

#### generate_pr_description (REQ-LF-PR-002)

```toml
[[steps]]
step_id = "generate_pr_description"
step_type = "shell"
description = "Invoke llxprt to write a PR description"

[steps.parameters]
command = "llxprt --profile-load {profile_planning} -p - --yolo"
stdin = "Read the issue in .luther/issue.md and the plan in .luther/plan.md. Review the code changes (use git diff origin/{base_branch}). Write a PR description to .luther/pr-description.md. The description MUST include 'Fixes #{issue_number}' to auto-close the issue."
```

#### create_pr (REQ-LF-PR-003)

```toml
[[steps]]
step_id = "create_pr"
step_type = "shell"
description = "Create a GitHub pull request"

[steps.parameters]
command = "gh pr create --repo {target_repo} --title "Fix #{issue_number}: {issue_title}" --body-file .luther/pr-description.md --base {base_branch} --head issue{issue_number}"
```

#### abandon_and_log (REQ-LF-FAIL-002 through REQ-LF-FAIL-004)

```toml
[[steps]]
step_id = "abandon_and_log"
step_type = "shell"
description = "Comment on issue, remove label, unassign on failure"

[steps.parameters]
command = """
set -euo pipefail
gh issue comment {issue_number} --repo {target_repo} --body "Luther abandoning this issue: workflow failed at step {last_step_id}."
gh issue edit {issue_number} --repo {target_repo} --remove-label "{luther_label}"
gh issue edit {issue_number} --repo {target_repo} --remove-assignee {assignee}
"""
```

#### log_completion

```toml
[[steps]]
step_id = "log_completion"
step_type = "shell"
description = "Log successful workflow completion"

[steps.parameters]
command = "echo "Workflow completed successfully for issue #{issue_number}""
```

### Constraints

- Zero Rust code in this phase — only TOML data files
- The workflow TOML must be parseable by existing `resolve_workflow_type()` (needs `max_iterations` support on TransitionDef from Phase 12)
- The config TOML must be parseable by existing `resolve_workflow_config()` (needs `variables` support from Phase 15)
- Test fixtures are exact copies of the config files (not simplified versions)
- All step parameters must use `{variable_name}` interpolation for values that come from config or prior steps
- No hardcoded profile names in the workflow type file

### Required TOML Comments

```toml
# @plan:PLAN-20260408-LLXPRT-FIRST.P17
# @requirement:REQ-LF-XXX
```

## Verification Commands

### Automated Checks

```bash
# Files exist
test -f config/workflows/llxprt-issue-fix-v1.toml && echo "OK" || echo "MISSING"
test -f config/workflow-configs/llxprt-code.toml && echo "OK" || echo "MISSING"
test -f tests/fixtures/workflows/valid/llxprt-issue-fix-v1.toml && echo "OK" || echo "MISSING"
test -f tests/fixtures/workflow-configs/valid/llxprt-code.toml && echo "OK" || echo "MISSING"

# Plan markers in TOML files
grep -c "@plan:PLAN-20260408-LLXPRT-FIRST.P17" config/workflows/llxprt-issue-fix-v1.toml
# Expected: 1+
grep -c "@plan:PLAN-20260408-LLXPRT-FIRST.P17" config/workflow-configs/llxprt-code.toml
# Expected: 1+

# No hardcoded profile names in workflow type
grep -i "opusthinking\|deepthinker\|typescriptexpert" config/workflows/llxprt-issue-fix-v1.toml
# Expected: no output (profiles are in config, not workflow type)

# Profile variables are in workflow type
grep "profile_planning\|profile_evaluating\|profile_implementing\|profile_remediating" config/workflows/llxprt-issue-fix-v1.toml
# Expected: found (as {profile_planning} etc. in command params)

# Profile mappings are in config
grep "profile_planning\|profile_evaluating\|profile_implementing\|profile_remediating" config/workflow-configs/llxprt-code.toml
# Expected: found (as concrete profile name assignments)

# TOML is valid (parseable)
cargo test --test config_binding_integration 2>&1 | grep "test result"
# Expected: passes (existing tests still work)

# Compile
cargo build --all-targets

# All tests pass
cargo test

# Step count
grep -c 'step_id = ' config/workflows/llxprt-issue-fix-v1.toml
# Expected: 14 (all steps present — write_issue_files eliminated, fetch_issue writes directly)

# Transition count
grep -c '^\[\[transitions\]\]' config/workflows/llxprt-issue-fix-v1.toml
# Expected: 24+ (including all fatal routes)

# Per-edge limits present
grep "max_iterations" config/workflows/llxprt-issue-fix-v1.toml
# Expected: 3 occurrences (evaluate_plan→create_plan, evaluate_impl→implement, remediate→run_tests)

# Verify executor step present
grep 'step_type = "verify"' config/workflows/llxprt-issue-fix-v1.toml
# Expected: 1 match (run_tests step)

# Variables section in config
grep "\[variables\]" config/workflow-configs/llxprt-code.toml
# Expected: found
```

### Structural Verification Checklist

- [ ] Workflow type file: 14 steps covering full issue-fix flow (write_issue_files eliminated — fetch_issue writes directly)
- [ ] Workflow type file: ~24 transitions including all fatal routes to abandon_and_log
- [ ] Workflow type file: 3 per-edge `max_iterations` on loop-back transitions
- [ ] Workflow type file: No hardcoded profile names — only `{profile_*}` variables
- [ ] Workflow type file: No hardcoded repo or assignee — only `{target_repo}`, `{assignee}` variables
- [ ] Config file: `[variables]` section with all profile mappings
- [ ] Config file: `[variables]` section with `target_repo`, `assignee`, `work_dir`, `base_branch`, `luther_label`
- [ ] Config file: `[guards]` with appropriate limits
- [ ] Config file: `workflow_type_id` matches workflow type file
- [ ] Test fixtures are exact copies of config files
- [ ] TOML files contain zero Rust code (REQ-LF-SEP-003)
- [ ] `push_changes` and `create_pr` are separate steps (REQ-LF-PR-004)
- [ ] `run_tests` uses `step_type = "verify"` (REQ-LF-TEST-001)

### Semantic Verification

- [ ] Data flow: `select_issue` sets `issue_number` and `issue_title` via context_map → used by all subsequent steps
- [ ] Data flow: `fetch_issue` retrieves data and writes large content directly to `.luther/issue.md` and `.luther/comments.md` via shell I/O — only short metadata (title, url) flows through context_map
- [ ] Data flow: `create_plan` reads `.luther/issue.md` → writes `.luther/plan.md`
- [ ] Data flow: `run_tests` writes `.luther/verify-report.json` → `remediate` reads it
- [ ] Data flow: `generate_pr_description` writes `.luther/pr-description.md` → `create_pr` uses `--body-file`
- [ ] Loop: `evaluate_plan` → `create_plan` (fixable, max 5) — bounded plan refinement
- [ ] Loop: `evaluate_impl` → `implement` (fixable, max 5) — bounded impl refinement
- [ ] Loop: `remediate` → `run_tests` (success, max 5) — bounded test fix loop
- [ ] Fatal: every non-terminal step has a `fatal` → `abandon_and_log` transition
- [ ] Terminal: `abandon_and_log` and `log_completion` have no outgoing transitions
- [ ] Profiles: changing `profile_planning` in config changes which model does planning (REQ-LF-PROF-004)

## Success Criteria

- 4 TOML files created (2 config, 2 test fixtures)
- TOML is syntactically valid and parseable by the existing config loader
- Complete workflow graph covers all 14 steps in the overview
- All requirements from sections 5-14 of requirements.md are traceable to step parameters or transitions
- No Rust code created in this phase

## Failure Recovery

If this phase fails:

1. Rollback: `rm config/workflows/llxprt-issue-fix-v1.toml config/workflow-configs/llxprt-code.toml`
2. Rollback: `rm tests/fixtures/workflows/valid/llxprt-issue-fix-v1.toml tests/fixtures/workflow-configs/valid/llxprt-code.toml`
3. Verify: `cargo test` still passes

## Phase Completion Marker

Create: `project-plans/llxprt-first/.completed/P17.md`
