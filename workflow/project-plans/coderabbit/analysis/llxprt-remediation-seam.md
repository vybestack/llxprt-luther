# P0.5 llxprt Remediation Seam

Plan: PLAN-20260429-CODERABBIT-PR-FOLLOWUP

## Decision

Use a dedicated `PrFollowupRemediationExecutor` step type for post-PR remediation. It may reuse process/argv/output-capture helper code from `src/engine/executors/llxprt.rs`, but it must own PR follow-through artifact writing, process evidence, changed-path evidence, result-file validation handoff, and routing.

## Evidence from current llxprt executor

- `src/engine/executors/llxprt.rs` builds argv as `llxprt --set reasoning.includeInResponse=false [--profile-load <profile>] --yolo -p <prompt>` and sets current directory to `StepContext::work_dir()`.
- It supports `success_file`, `stdout_file`, `stderr_file`, `success_on_diff`, `required_changed_paths`, `required_changed_path_patterns`, and `outcome_on_stdout` params.
- Relative artifact paths are resolved under `context.work_dir()`.
- `success_file` is treated as success when the resolved file exists and has non-zero length.
- Timeout currently returns `StepOutcome::Fatal` and stores `exit_code=124` plus a diagnostic in context.
- It captures stdout/stderr buffers and can write them to configured files, but it does not expose a structured process-result object containing all PR follow-through evidence fields.

## Contract required for P12

`PrFollowupRemediationExecutor` must persist before routing:

- argv
- exit status or signal
- timeout/spawn/error class
- bounded stdout/stderr
- full stdout/stderr/log paths when captured
- success-file/result-file presence and size
- changed-path evidence
- binding fields needed for `pr-remediation-llxprt-run.json`
- validator-readable `pr-remediation-result.json` or failure artifact when possible

## Follow-up

P12 must implement the owned runner/wrapper seam if existing helpers remain private or insufficient. Do not route directly from raw `llxprt` process status to product state.
