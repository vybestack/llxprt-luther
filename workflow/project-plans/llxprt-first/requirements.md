# llxprt-first Requirements (EARS)

Requirements for the first real workflow: Luther working llxprt-code issues end-to-end.

Format legend:

- Ubiquitous: The `<system>` shall `<response>`.
- Event-driven: When `<trigger>`, the `<system>` shall `<response>`.
- State-driven: While `<state>`, the `<system>` shall `<response>`.
- Unwanted behavior: If `<fault/condition>`, then the `<system>` shall `<response>`.
- Optional feature: Where `<feature enabled>`, the `<system>` shall `<response>`.

---

## 1) Enhanced shell executor

### REQ-LF-SHELL-001 (Optional feature)
Where a step's parameters include `output_format: "json"` and a `context_map`, the ShellExecutor shall parse stdout as JSON and extract fields into named context variables using dot-path notation.

### REQ-LF-SHELL-002 (Unwanted behavior)
If `output_format: "json"` is specified and stdout is not valid JSON, then the ShellExecutor shall return a `Fatal` outcome with a diagnostic message.

### REQ-LF-SHELL-003 (Optional feature)
Where a step's parameters include a `stdin` field, the ShellExecutor shall pipe the interpolated value of that field to the command's standard input.

### REQ-LF-SHELL-004 (Optional feature)
Where a step's parameters include a `stdin_file` field, the ShellExecutor shall read the specified file (relative to work_dir) and pipe its contents to the command's standard input.

### REQ-LF-SHELL-008 (Unwanted behavior)
If a `stdin_file` is specified and the file does not exist or cannot be read, then the ShellExecutor shall return a `Fatal` outcome with a diagnostic identifying the missing file.

### REQ-LF-SHELL-009 (Unwanted behavior)
If `output_format: "json"` is specified with a `context_map` and a dot-path key does not exist in the parsed JSON, then the ShellExecutor shall return a `Fatal` outcome identifying the missing path and the available top-level keys.

### REQ-LF-SHELL-005 (Optional feature)
Where a step's parameters include `outcome_on_stdout`, the ShellExecutor shall scan stdout for the configured string keys and map the first match to the corresponding `StepOutcome` value.

### REQ-LF-SHELL-006 (Event-driven)
When `outcome_on_stdout` is configured and the command exits with code 0 but no configured string is found in stdout, the ShellExecutor shall return `Success` as the default outcome.

### REQ-LF-SHELL-007 (Unwanted behavior)
If `outcome_on_stdout` is configured and the command exits with a non-zero code, then the ShellExecutor shall return `Fixable` regardless of stdout content, preserving existing exit-code semantics.

---

## 2) Verify executor

### REQ-LF-VERIFY-001 (Ubiquitous)
The VerifyExecutor shall run a configurable sequence of verification checks specified in the step's `parameters.checks` array.

### REQ-LF-VERIFY-002 (Event-driven)
When all checks pass, the VerifyExecutor shall return `Success` and set context variable `verify_passed` to `"true"`.

### REQ-LF-VERIFY-003 (Event-driven)
When any check fails, the VerifyExecutor shall return `Fixable`, set `verify_passed` to `"false"`, and write a structured failure report to `.luther/verify-report.json` in the working directory.

### REQ-LF-VERIFY-004 (Ubiquitous)
The VerifyExecutor shall set a `verify_summary` context variable containing a human-readable one-line summary of all check results (e.g., "lint: pass, typecheck: 2 errors, test: 3 failed").

### REQ-LF-VERIFY-005 (Ubiquitous)
The structured failure report shall contain per-check results with parsed error details including at minimum: file path, line number, and error message.

### REQ-LF-VERIFY-006 (Ubiquitous)
For test check failures, the failure report shall include test name, file, line, assertion kind, and where available, expected and actual values.

### REQ-LF-VERIFY-007 (Ubiquitous)
The check suite shall be a parameter, not hardcoded. The VerifyExecutor shall support at minimum: `lint`, `typecheck`, `test`, `format`, and `build` check types for Node/TypeScript projects.

### REQ-LF-VERIFY-008 (Unwanted behavior)
If a check command cannot be spawned (binary not found, permission denied), then the VerifyExecutor shall return `Fatal` with a diagnostic identifying the failed check and command.

### REQ-LF-VERIFY-009 (Ubiquitous)
The VerifyExecutor shall set per-check-type context variables (`test_failures`, `build_errors`, `type_errors`, `lint_errors`) containing JSON arrays of structured error records.

---

## 3) Namespaced context

### REQ-LF-CTX-001 (Ubiquitous)
The StepContext shall support namespaced variable references in the form `{step_id.variable_name}`, resolving to the value set by the named step.

### REQ-LF-CTX-002 (Ubiquitous)
The interpolation resolver shall support unnamespaced references `{variable_name}`, resolving by searching all steps in most-recent-first order.

### REQ-LF-CTX-003 (Event-driven)
When an executor sets a context variable during step execution, the engine shall store it namespaced under the current step_id.

### REQ-LF-CTX-004 (Ubiquitous)
Built-in variables (`work_dir`, `run_id`) shall remain resolvable without a namespace prefix.

---

## 4) Per-edge loop limits

### REQ-LF-LOOP-001 (Ubiquitous)
The engine shall support an optional `max_iterations` field on `TransitionDef` and track and enforce loop counts independently for each transition edge that specifies one.

### REQ-LF-LOOP-002 (Ubiquitous)
The engine shall track loop counts per transition edge, keyed by `from:to` step pair, not as a single global counter.

### REQ-LF-LOOP-003 (Unwanted behavior)
If a per-edge loop count exceeds its configured `max_iterations`, then the engine shall return `Abandoned` with a message identifying the exceeded edge.

### REQ-LF-LOOP-004 (Ubiquitous)
The global `max_iterations` in `GuardLimits` shall serve as a fallback for transition edges that do not specify their own `max_iterations`.

### REQ-LF-LOOP-005 (Event-driven)
When a checkpoint is persisted, the engine shall include per-edge loop counts in the state snapshot so they survive resume.

---

## 5) Workflow data flow

### REQ-LF-DATA-001 (Ubiquitous)
Short metadata values (issue number, branch name, PR number, pass/fail signals) shall flow through StepContext variables.

### REQ-LF-DATA-002 (Ubiquitous)
Large content (issue body, comments, plans, failure reports, PR descriptions) shall be written to files in the working directory under a `.luther/` subdirectory.

### REQ-LF-DATA-003 (Ubiquitous)
LLM step prompts shall instruct llxprt to read input from and write output to specific files in the working directory, not through context variable interpolation of large content.

---

## 6) Profile configuration

### REQ-LF-PROF-001 (Ubiquitous)
The workflow type definition shall reference model profiles by logical role names (e.g., `{profile_planning}`) rather than concrete llxprt profile names.

### REQ-LF-PROF-002 (Ubiquitous)
The workflow instance config shall map logical role names to concrete llxprt profile names (e.g., `profile_planning = "opusthinking"`).

### REQ-LF-PROF-003 (Event-driven)
When a step references a profile variable, the engine shall resolve it through standard context variable interpolation — the workflow config values are loaded into context at run start.

### REQ-LF-PROF-004 (Ubiquitous)
Changing which model performs a role shall require only a workflow config edit, not a workflow type definition change.

---

## 7) Issue selection

### REQ-LF-ISSUE-001 (Event-driven)
When the select_issue step runs, it shall query GitHub for open issues in the lowest-versioned milestone first, then the next milestone if no eligible issues remain.

### REQ-LF-ISSUE-002 (Ubiquitous)
The select_issue step shall pick the lowest-numbered unassigned issue within the selected milestone.

### REQ-LF-ISSUE-003 (Event-driven)
When an issue is selected, the step shall assign it to the configured user and add the "Luther working" label.

### REQ-LF-ISSUE-004 (Unwanted behavior)
If no unassigned issues exist in any milestone, then the select_issue step shall return `Fatal` (nothing to do).

---

## 8) Fetch issue

### REQ-LF-FETCH-001 (Event-driven)
When the fetch_issue step runs, it shall retrieve the issue body and all comments from GitHub using `gh issue view --json`.

### REQ-LF-FETCH-002 (Event-driven)
When the issue data is retrieved, the step shall write the issue body to `.luther/issue.md` and all comments to `.luther/comments.md` in the working directory.

### REQ-LF-FETCH-003 (Event-driven)
When the issue data is retrieved, the step shall set context variables `issue_number`, `issue_title`, and `issue_url` for use by later steps.

### REQ-LF-FETCH-004 (Unwanted behavior)
If the issue cannot be retrieved (network error, invalid issue number, permission denied), then the step shall return `Fatal` with diagnostic output.

---

## 9) Workspace setup

### REQ-LF-WS-001 (Event-driven)
When the setup_workspace step runs, it shall check out the target repository into the configured working directory.

### REQ-LF-WS-002 (Event-driven)
When the workspace is ready, the step shall create a branch named `issue{number}` from the configured base branch.

### REQ-LF-WS-003 (Unwanted behavior)
If git checkout or branch creation fails, then the step shall return `Fatal` with diagnostic output.

### REQ-LF-WS-004 (Event-driven)
When the workspace is set up, the step shall create the `.luther/` subdirectory in the working directory for workflow artifact files.

---

## 10) Planning loop

### REQ-LF-PLAN-001 (Event-driven)
When the create_plan step runs, it shall invoke `llxprt --profile-load {resolved_profile} -p "{goal}" --yolo` in the working directory.

### REQ-LF-PLAN-002 (Event-driven)
When the evaluate_plan step runs, it shall invoke llxprt with the evaluating profile and a prompt that specifies exact pass/fail response strings (`PLAN_APPROVED` / `PLAN_NEEDS_REVISION`).

### REQ-LF-PLAN-003 (Event-driven)
When the evaluate_plan step output contains `PLAN_APPROVED`, the ShellExecutor shall map this to `Success` via `outcome_on_stdout`.

### REQ-LF-PLAN-004 (Event-driven)
When the evaluate_plan step output contains `PLAN_NEEDS_REVISION`, the ShellExecutor shall map this to `Fixable` via `outcome_on_stdout`, triggering a loop back to create_plan.

### REQ-LF-PLAN-005 (Unwanted behavior)
If the plan loop exceeds 5 iterations, then the engine shall abandon the run via per-edge loop limit on the evaluate_plan→create_plan transition.

---

## 11) Implementation and evaluation

### REQ-LF-IMPL-001 (Event-driven)
When the implement step runs, it shall invoke llxprt with the implementing profile, providing the approved plan as input (via file reference in the prompt).

### REQ-LF-IMPL-002 (Event-driven)
When the evaluate_impl step runs, it shall invoke llxprt with the evaluating profile and a prompt specifying exact response strings (`IMPL_APPROVED` / `IMPL_NEEDS_WORK`).

### REQ-LF-IMPL-003 (Event-driven)
When evaluate_impl returns `IMPL_NEEDS_WORK`, the engine shall route to remediation or back to implementation per the transition table.

---

## 12) Test and remediation loop

### REQ-LF-TEST-001 (Event-driven)
When the run_tests step runs, the VerifyExecutor shall execute the configured check suite against the target project in the working directory.

### REQ-LF-TEST-002 (Event-driven)
When tests fail, the remediate step shall invoke llxprt with the remediating profile, providing the structured failure report (`.luther/verify-report.json`) as input.

### REQ-LF-TEST-003 (Unwanted behavior)
If the test/remediate loop exceeds 5 iterations, then the engine shall abandon the run via per-edge loop limit on the run_tests→remediate transition.

---

## 13) PR submission

### REQ-LF-PR-001 (Event-driven)
When the push_changes step runs, it shall execute `git add -A`, `git commit`, and `git push` as a shell command.

### REQ-LF-PR-002 (Event-driven)
When the generate_pr_description step runs, it shall invoke llxprt to produce a PR description that includes "Fixes #{issue_number}", writing the output to `.luther/pr-description.md` in the working directory.

### REQ-LF-PR-003 (Event-driven)
When the create_pr step runs, it shall execute `gh pr create` with the generated title and body.

### REQ-LF-PR-004 (Ubiquitous)
The push_changes step (git) and create_pr step (gh) shall be separate workflow steps, not combined into a single step.

---

## 14) Failure and abandonment

### REQ-LF-FAIL-001 (Unwanted behavior)
If any step returns `Fatal`, then the engine shall route to the abandon_and_log terminal step.

### REQ-LF-FAIL-002 (Event-driven)
When the abandon_and_log step runs, it shall comment on the GitHub issue explaining the failure reason.

### REQ-LF-FAIL-003 (Event-driven)
When the abandon_and_log step runs, it shall remove the "Luther working" label from the issue.

### REQ-LF-FAIL-004 (Event-driven)
When the abandon_and_log step runs, it shall unassign the configured user from the issue.

### REQ-LF-FAIL-005 (Event-driven)
When a run completes (success or abandonment), the engine shall record the outcome, run_id, issue number, and step reached in the run metadata store.

---

## 15) Engine/workflow separation

### REQ-LF-SEP-001 (Ubiquitous)
The workflow engine shall contain no GitHub-specific, llxprt-specific, or Node/TypeScript-specific code. All domain operations shall be performed by executors dispatched via the registry.

### REQ-LF-SEP-002 (Ubiquitous)
The engine shall compile and all engine-level tests shall pass with no workflow definition files present in the config directory.

### REQ-LF-SEP-003 (Ubiquitous)
The workflow type definition and workflow instance config shall be pure TOML data files containing zero Rust code or compiled logic.

---

## 16) Scope boundary

### REQ-LF-SCOPE-001 (Ubiquitous)
This plan shall stop at PR creation. CI watching, review parsing, and review remediation are out of scope.

### REQ-LF-SCOPE-002 (Ubiquitous)
The VerifyExecutor shall implement Node/TypeScript check parsers for the MVP. Rust check parsers are out of scope for this plan.
