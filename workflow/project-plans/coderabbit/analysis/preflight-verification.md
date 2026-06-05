# Phase 0.5 Preflight Verification

Plan: `PLAN-20260429-CODERABBIT-PR-FOLLOWUP`

## Verdict

VERDICT: PASS

Preflight is analysis-only. Production Rust and workflow TOML were not modified. All required assumptions are documented; gaps are recorded as explicit follow-up requirements for later phases.

## Required files created/updated

- `project-plans/coderabbit/analysis/preflight-verification.md`
- `project-plans/coderabbit/analysis/final-dry-run-command.json`
- `project-plans/coderabbit/analysis/fixture-regeneration-command.json`
- `project-plans/coderabbit/analysis/expected-failing-tests.json`
- `project-plans/coderabbit/analysis/llxprt-remediation-seam.md`
- `project-plans/coderabbit/.completed/P0.5`
- `project-plans/coderabbit/execution-tracker.md`

## Concrete command evidence

### `pwd`, git status, required files, relevant paths, cargo metadata

Command:

```bash
pwd; echo '--- git status --short'; git status --short; echo '--- test implementation plan'; test -f project-plans/coderabbit/implementation-plan.md && echo present || echo missing; echo '--- ls relevant paths'; ls -l Cargo.toml config/workflows/llxprt-issue-fix-v1.toml tests/fixtures/workflows/valid/llxprt-issue-fix-v1.toml tests/fixtures/workflows/valid/llxprt-issue-fix-v1.json tests/e2e_workflow_integration.rs tests/smoke_test.rs tests/engine_integration_llxprt_first.rs 2>&1; echo '--- cargo metadata targets'; cargo metadata --no-deps --format-version 1 2>/dev/null | python3 -c 'import json,sys; d=json.load(sys.stdin); p=d["packages"][0]; print("package",p["name"]); print("bins", [t["name"] for t in p["targets"] if "bin" in t["kind"]]); print("tests", [t["name"] for t in p["targets"] if "test" in t["kind"]])'
```

Output:

```text
/Users/acoliver/projects/luther/workflow
--- git status --short
 M .llxprt/LLXPRT.md
 M config/workflow-configs/llxprt-code.toml
 M config/workflows/llxprt-issue-fix-v1.toml
 M src/adapters/git.rs
 M src/adapters/mod.rs
 M src/cli/mod.rs
 M src/engine/executor.rs
 M src/engine/executors/llxprt.rs
 M src/engine/executors/shell.rs
 M src/engine/executors/verify.rs
 M src/engine/executors/write_file.rs
 M src/engine/instance.rs
 M src/engine/mod.rs
 M src/engine/runner.rs
 M src/engine/transition.rs
 M src/lib.rs
 M src/main.rs
 M src/monitor/heartbeat.rs
 M src/monitor/ipc.rs
 M src/monitor/mod.rs
 M src/monitor/process.rs
 M src/persistence/artifacts.rs
 M src/persistence/checkpoint.rs
 M src/persistence/mod.rs
 M src/persistence/run_metadata.rs
 M src/persistence/sqlite.rs
 M src/repo/mod.rs
 M src/runtime_paths.rs
 M src/service/launchd.rs
 M src/service/mod.rs
 M src/service/spec.rs
 M src/service/systemd.rs
 M src/workflow/config_loader.rs
 M src/workflow/mod.rs
 M src/workflow/schema.rs
 M tests/cli_config_resolution_integration.rs
 M tests/cli_e2e_integration.rs
 M tests/config_binding_integration.rs
 M tests/config_binding_json_parity_integration.rs
 M tests/e2e_workflow_integration.rs
 M tests/engine_execution_integration.rs
 M tests/engine_integration_llxprt_first.rs
 M tests/engine_resume_integration.rs
 M tests/executor_unit_tests.rs
 M tests/fixtures/workflow-configs/valid/llxprt-code.json
 M tests/fixtures/workflow-configs/valid/llxprt-code.toml
 M tests/fixtures/workflows/valid/llxprt-issue-fix-v1.json
 M tests/fixtures/workflows/valid/llxprt-issue-fix-v1.toml
 M tests/hello_world_workflow_integration.rs
 M tests/live_workflow_integration.rs
 M tests/monitor_service_integration.rs
 M tests/monitor_singleton_and_restart_integration.rs
 M tests/namespaced_context_tests.rs
 M tests/per_edge_loop_tests.rs
 M tests/persistence_integration.rs
 M tests/quality_release_guardrails.rs
 M tests/repo_prep_integration.rs
 M tests/service_ipc_contract_integration.rs
 M tests/shell_enhanced_tests.rs
 M tests/smoke_test.rs
 M tests/verify_executor_tests.rs
?? project-plans/coderabbit/
--- test implementation plan
present
--- ls relevant paths
-rw-r--r--@ 1 acoliver  staff   1045 Apr 14 02:36 Cargo.toml
-rw-r--r--@ 1 acoliver  staff  19832 Apr 29 13:43 config/workflows/llxprt-issue-fix-v1.toml
-rw-r--r--@ 1 acoliver  staff  24177 Apr 29 12:28 tests/e2e_workflow_integration.rs
-rw-r--r--@ 1 acoliver  staff  36840 Apr 29 12:28 tests/engine_integration_llxprt_first.rs
-rw-r--r--@ 1 acoliver  staff  20620 Apr 29 13:43 tests/fixtures/workflows/valid/llxprt-issue-fix-v1.json
-rw-r--r--@ 1 acoliver  staff  19832 Apr 29 13:43 tests/fixtures/workflows/valid/llxprt-issue-fix-v1.toml
-rw-r--r--@ 1 acoliver  staff   9597 Apr 29 12:28 tests/smoke_test.rs
--- cargo metadata targets
package xtask
bins ['xtask']
tests []
```

Notes: workspace root contains a `[workspace]` plus root package. `cargo metadata --no-deps` package index selected `xtask`; concrete test binaries were listed via cargo below.

### Test listing and compile check

Command:

```bash
echo '--- cargo test e2e list'; cargo test --test e2e_workflow_integration -- --list; echo '--- cargo test smoke list'; cargo test --test smoke_test -- --list; echo '--- cargo test no-run'; cargo test --no-run
```

Output excerpt:

```text
--- cargo test e2e list
test_config_variables_injected_into_context: test
test_fatal_at_fetch_issue_routes_to_abandon_and_log: test
test_fatal_at_implement_routes_to_abandon_and_log: test
test_fatal_at_select_issue_routes_to_abandon_and_log: test
test_fatal_at_setup_workspace_routes_to_abandon_and_log: test
test_happy_path_all_steps_succeed: test
test_impl_evaluation_loop: test
test_plan_loop_exceeds_limit_abandons: test
test_plan_loop_fixable_then_approved: test
test_run_completion_records_metadata: test
test_test_remediation_loop_exceeds_limit_abandons: test
test_test_remediation_loop_fixable_then_passes: test
test_workflow_config_loads_from_toml: test
test_workflow_graph_completeness: test
test_workflow_type_loads_from_toml: test

15 tests, 0 benchmarks
--- cargo test smoke list
test_smoke_dry_run_prints_all_steps: test
test_smoke_select_and_fetch: test

2 tests, 0 benchmarks
--- cargo test no-run
warning: unused doc comment
   --> src/engine/runner.rs:125:9
[... warnings omitted in this excerpt; command completed successfully ...]
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.13s
  Executable unittests src/lib.rs (target/debug/deps/luther_workflow-cec7d813c7133e38)
  Executable unittests src/main.rs (target/debug/deps/luther_workflow-5b7ee5a8f0fb98b9)
  Executable tests/cli_config_resolution_integration.rs (target/debug/deps/cli_config_resolution_integration-0d508863ec2b3d20)
  Executable tests/cli_e2e_integration.rs (target/debug/deps/cli_e2e_integration-307f54dd73c6f965)
  Executable tests/config_binding_integration.rs (target/debug/deps/config_binding_integration-91eceaea69c49806)
  Executable tests/config_binding_json_parity_integration.rs (target/debug/deps/config_binding_json_parity_integration-5370f6f946d2bda8)
  Executable tests/e2e_workflow_integration.rs (target/debug/deps/e2e_workflow_integration-15d02569629249f1)
  Executable tests/engine_execution_integration.rs (target/debug/deps/engine_execution_integration-05e6131098a2a09c)
  Executable tests/engine_integration_llxprt_first.rs (target/debug/deps/engine_integration_llxprt_first-9968543dfceb2062)
  Executable tests/engine_resume_integration.rs (target/debug/deps/engine_resume_integration-1753adf5aa9f0634)
  Executable tests/executor_unit_tests.rs (target/debug/deps/executor_unit_tests-7822cdfcfb9c41bc)
  Executable tests/hello_world_workflow_integration.rs (target/debug/deps/hello_world_workflow_integration-a534af153e03815a)
  Executable tests/live_workflow_integration.rs (target/debug/deps/live_workflow_integration-6a5d50559aef280c)
  Executable tests/monitor_service_integration.rs (target/debug/deps/monitor_service_integration-76533047bc44baf7)
  Executable tests/monitor_singleton_and_restart_integration.rs (target/debug/deps/monitor_singleton_and_restart_integration-1fd824f40347c0d6)
  Executable tests/namespaced_context_tests.rs (target/debug/deps/namespaced_context_tests-fbf60a01f37a6f3e)
  Executable tests/per_edge_loop_tests.rs (target/debug/deps/per_edge_loop_tests-6c3a67a60ed06691)
  Executable tests/persistence_integration.rs (target/debug/deps/persistence_integration-9339e607e8d76614)
  Executable tests/quality_release_guardrails.rs (target/debug/deps/quality_release_guardrails-cfd39e160cec69fc)
  Executable tests/repo_prep_integration.rs (target/debug/deps/repo_prep_integration-bba37501c6104350)
  Executable tests/service_ipc_contract_integration.rs (target/debug/deps/service_ipc_contract_integration-36da8fc9406fa53c)
  Executable tests/shell_enhanced_tests.rs (target/debug/deps/shell_enhanced_tests-80b734aad984f887)
  Executable tests/smoke_test.rs (target/debug/deps/smoke_test-1ac2469ddbe1113f)
  Executable tests/verify_executor_tests.rs (target/debug/deps/verify_executor_tests-26ad07e18c7748f4)
```

### GitHub CLI, dependencies, transition, and regeneration evidence

Command:

```bash
echo '--- gh --version'; if command -v gh >/dev/null 2>&1; then gh --version; else echo 'gh not found'; fi; echo '--- gh auth status'; if command -v gh >/dev/null 2>&1; then gh auth status 2>&1 || true; else echo 'gh auth status skipped: gh not found'; fi; echo '--- cargo tree deps'; cargo tree -p serde_json; cargo tree -p chrono; cargo tree -p tokio; cargo tree -p toml; echo '--- create_pr transitions'; grep -n 'from = "create_pr"' config/workflows/llxprt-issue-fix-v1.toml; echo '--- StepOutcome Abandon'; grep -R "StepOutcome::Abandon" src/engine/runner.rs src/engine/transition.rs; echo '--- regen script'; ls -l junk/regen_fixtures.py 2>&1; echo '--- run regen dry metadata'; python3 junk/regen_fixtures.py --help 2>&1 || true
```

Output:

```text
--- gh --version
gh version 2.83.2 (2025-12-10)
https://github.com/cli/cli/releases/tag/v2.83.2
--- gh auth status
github.com
  [OK] Logged in to github.com account acoliver (keyring)
  - Active account: true
  - Git operations protocol: ssh
  - Token: gho_************************************
  - Token scopes: 'admin:public_key', 'gist', 'project', 'read:org', 'repo'
--- cargo tree deps
serde_json v1.0.149
├── itoa v1.0.18
├── memchr v2.8.0
├── serde_core v1.0.228
└── zmij v1.0.21
chrono v0.4.44
├── iana-time-zone v0.1.65
│   └── core-foundation-sys v0.8.7
└── num-traits v0.2.19
    [build-dependencies]
    └── autocfg v1.5.0
tokio v1.51.0
[... dependency tree omitted for brevity ...]
toml v0.8.23
[... dependency tree omitted for brevity ...]
--- create_pr transitions
471:from = "create_pr"
534:from = "create_pr"
--- StepOutcome Abandon
src/engine/runner.rs:            if outcome == StepOutcome::Abandon {
src/engine/transition.rs:            StepOutcome::Abandon => write!(f, "abandon"),
src/engine/transition.rs:        StepOutcome::Abandon => "abandon",
src/engine/transition.rs:        StepOutcome::Abandon => "abandon",
src/engine/transition.rs:        let _abandon = StepOutcome::Abandon;
src/engine/transition.rs:        assert_eq!(StepOutcome::Abandon.to_string(), "abandon");
--- regen script
-rw-r--r--@ 1 acoliver  staff  523 Apr 17 01:12 junk/regen_fixtures.py
--- run regen dry metadata
regen tests/fixtures/workflows/valid/llxprt-issue-fix-v1.json
regen tests/fixtures/workflow-configs/valid/llxprt-code.json
```

`python3 junk/regen_fixtures.py --help` is not a help-only mode; the script ignores args and regenerated fixture JSON. This is a fixture write, not production/workflow TOML modification. Exact source-of-truth command is recorded in `fixture-regeneration-command.json`.

### TOML nested params, duplicate transition, CodeRabbit, dry-run, regeneration script

Command:

```bash
echo '--- schema nested params parse smoke'; python3 - <<'PY'
import tomllib, tempfile, pathlib
content='''workflow_type_id="nested-test"\n[[steps]]\nstep_id="s"\nstep_type="noop"\n[steps.parameters]\nartifact_root="{artifact_dir}"\n[steps.parameters.budgets]\nmax=3\n[steps.parameters.commands.post_pr_tests]\nargv=["cargo","test"]\n[[transitions]]\nfrom="s"\nto="s2"\ncondition="success"\n'''
print(tomllib.loads(content)['steps'][0]['parameters'])
PY

echo '--- duplicate transition behavior test references'; grep -R "duplicate\|same.*condition\|resolve_transition" -n tests src/workflow src/engine | head -80

echo '--- CodeRabbit references'; grep -R "coderabbit\|CodeRabbit\|code rabbit" -n . --exclude-dir=target --exclude-dir=.git | head -80

echo '--- dry-run cli references'; sed -n '1,150p' src/cli/mod.rs; sed -n '90,145p' src/main.rs

echo '--- fixture regen script'; cat junk/regen_fixtures.py
```

Output excerpt:

```text
--- schema nested params parse smoke
{'artifact_root': '{artifact_dir}', 'budgets': {'max': 3}, 'commands': {'post_pr_tests': {'argv': ['cargo', 'test']}}}
--- duplicate transition behavior test references
tests/engine_execution_integration.rs:145:        luther_workflow::engine::transition::resolve_transition("step_a", &outcome, &transitions);
tests/engine_execution_integration.rs:203:    let next_step = luther_workflow::engine::transition::resolve_transition(
tests/engine_execution_integration.rs:308:    let next_step = luther_workflow::engine::transition::resolve_transition(
tests/engine_execution_integration.rs:373:    let next_step = luther_workflow::engine::transition::resolve_transition(
src/engine/transition.rs:102:pub fn resolve_transition(
src/engine/transition.rs:139:pub fn resolve_transition_schema(
src/engine/runner.rs:12:use crate::engine::transition::{resolve_transition_schema, StepOutcome};
src/engine/runner.rs:517:        let next_step = resolve_transition_schema(step_id, outcome, transitions);
src/engine/mod.rs:12:pub use transition::{resolve_transition, resolve_transition_schema, StepOutcome};
--- CodeRabbit references
[only docs/research/project-plan references; no production CodeRabbit implementation found]
--- dry-run cli references
pub struct RunArgs {
    pub config: Option<PathBuf>,
    pub dry_run: bool,
    pub workflow_type: Option<String>,
    pub config_dir: Option<PathBuf>,
}
[...]
    if args.dry_run {
        println!("Dry run mode - workflow would execute the following steps:");
        for step in &workflow_type.steps {
            println!(
                "  - {} ({}): {:?}",
                step.step_id,
                step.step_type,
                step.description.as_deref().unwrap_or("No description")
            );
        }
        println!("\nDry run complete. No changes made.");
        process::exit(0);
    }
--- fixture regen script
import tomllib, json, pathlib
root = pathlib.Path("/Users/acoliver/projects/luther/workflow")
pairs = [
    ("tests/fixtures/workflows/valid/llxprt-issue-fix-v1.toml",
     "tests/fixtures/workflows/valid/llxprt-issue-fix-v1.json"),
    ("tests/fixtures/workflow-configs/valid/llxprt-code.toml",
     "tests/fixtures/workflow-configs/valid/llxprt-code.json"),
]
for src, dst in pairs:
    data = tomllib.loads((root / src).read_text())
    (root / dst).write_text(json.dumps(data, indent=2) + "\n")
    print("regen", dst)
```

## Findings by Phase 0.5 checklist area

### Config/workflow resolver paths and exact production/fixture workflow files

- Production workflow TOML: `config/workflows/llxprt-issue-fix-v1.toml`.
- Fixture workflow TOML: `tests/fixtures/workflows/valid/llxprt-issue-fix-v1.toml`.
- Fixture workflow JSON: `tests/fixtures/workflows/valid/llxprt-issue-fix-v1.json`.
- Production workflow config TOML: `config/workflow-configs/llxprt-code.toml`.
- Fixture workflow config TOML/JSON: `tests/fixtures/workflow-configs/valid/llxprt-code.toml`, `tests/fixtures/workflow-configs/valid/llxprt-code.json`.
- `tests/e2e_workflow_integration.rs` loads `llxprt-issue-fix-v1` and `llxprt-code` via config loader tests.
- `src/cli/mod.rs` accepts `--workflow-type`, `--config`, `--config-dir`, and `--dry-run`; `src/main.rs` dry-run prints steps and exits before execution.

### Fixture regeneration source-of-truth command

Recorded in `project-plans/coderabbit/analysis/fixture-regeneration-command.json` as:

```json
{"argv":["python3","junk/regen_fixtures.py"]}
```

The script serializes fixture TOML to fixture JSON. P17 must copy production TOML into the fixture TOML before running the command if workflow TOML changes.

### GitHub permissions/auth-scope assumptions

Reconnaissance only; no mutating GitHub command was run.

- Installed `gh`: `2.83.2`.
- Auth status: logged in to `github.com` as `acoliver` via keyring.
- Token prefix/type observed: `gho_...` OAuth token masked by `gh`.
- Token scopes reported: `admin:public_key`, `gist`, `project`, `read:org`, `repo`.
- Git operations protocol: ssh.

Required permissions/scopes to document in P02/P04 fixture-backed contract:

- GraphQL PR/review-thread reads: `repo` for private repos or public repo read access.
- GraphQL `resolveReviewThread`: repository write/maintain-level access and token accepted for mutation.
- REST/GraphQL comment creation: pull request/issues write permission, normally covered by `repo` for private repos.
- Paginated comments/reviews: repo read permissions.
- Pushing to PR branch: SSH key/repo write access to the branch/fork.
- Actions checks/jobs/log downloads: Actions read permission; `repo` is sufficient for private repos through classic token, but fine-grained tokens need Actions read.

Permission-denied artifact expectation for every GitHub surface: fatal/non-success artifact with `permission_denied`, `operation`, `required_scope_or_permission`, `account_login`, `token_type`, and safe command/query metadata. Fixture-backed auth failure cases are required before consuming live fields.

### CodeRabbit configuration/availability assumption for `llxprt-issue-fix-v1`

No production CodeRabbit implementation/config exists yet. Current repository references are docs/research/plan material only. Required P02/P04 follow-up:

- Define configured identity set (for example `coderabbitai[bot]`, `coderabbitai`, and any GitHub App/bot login observed in fixtures).
- Define readiness signals in fixture-backed API payloads: stable ready observations, no in-progress CodeRabbit run, and normalized feedback surfaces.
- If CodeRabbit is unavailable or unsupported for a PR, collector must write `coderabbit-feedback.json` with `readiness_state=fatal` or a documented unsupported semantic and route fatal/non-success through `post_pr_failure_terminal`; it must not silently pass.

### llxprt remediation invocation evidence contract and seam decision

Detailed seam file: `project-plans/coderabbit/analysis/llxprt-remediation-seam.md`.

Decision: use dedicated `PrFollowupRemediationExecutor`, not raw `llxprt` routing. Existing `llxprt.rs` supports prompt/profile, `success_file`, stdout/stderr artifacts, timeout, success-on-diff, changed path checks, and stdout outcome markers. It does not expose a structured process-result object containing all required PR follow-through fields. P12 must add/reuse helper seams to capture argv, exit/signal, timeout/spawn class, bounded/full logs, result-file presence, changed-path evidence, and validator-readable failure artifacts before routing.

### Test support for fake executors/command runners/temp artifact dirs/no-network defaults

- `tests/e2e_workflow_integration.rs` defines `SharedMockExecutor` and uses fake registered executors for graph/runner behavior.
- `ExecutorRegistry::new()` and `register()` allow test fake executors by step type.
- `tempfile = "3"` is a dev-dependency; tests use temp dirs in multiple modules.
- No-network default for PR follow-through is required before live use; P04/P16/P18 must use fixtures and fake command runners/fake `gh` and must not depend on live GitHub.
- No generic command-runner trait exists in production yet; see command-runner section.

### Clock/sleeper abstraction availability or required plan follow-up

No injectable clock/sleeper abstraction is available for executor watch loops. Current code uses `thread::sleep` in `llxprt.rs`, `shell.rs`, `verify.rs`, and monitor code, plus `tokio::time` in service/main paths. P04/P06 must introduce a minimal injectable clock/sleeper abstraction for PR check watching and CodeRabbit readiness so one-hour watch tests do not sleep in real time.

### Command-runner injection seam availability or required plan follow-up

No general command-runner injection seam exists. Current executors directly call `std::process::Command::new(...)` for shell, llxprt, git, verify commands, service commands, and adapters. P03/P04 must add/design a command runner trait/seam for GitHub executors, marker actions, post-PR tests, and push operations so tests can use fake `gh`/shell runners with no network or real pushes.

### Artifact root/path convention and `artifact_root` creation/canonicalization rule

Current `VerifyExecutor` has `artifact_root` support, defaulting to `context.work_dir().join(".luther")` when omitted and resolving relative paths under `context.work_dir()`. It does not canonicalize before use. `LlxprtExecutor` resolves individual artifact files under `context.work_dir()` and does not use an artifact store.

P01/P05 required rule: PR follow-through executors must require TOML param `artifact_root` (no `artifact_dir` alias), expand variables, resolve relative values under the same workflow work-dir semantics, create the directory, canonicalize it, and initialize `PrFollowupArtifactStore` only from that root. Missing/empty/unexpandable/non-canonicalizable/conflicting roots are fatal/config errors. Canonical current path convention:

```text
<artifact_root>/pr-followup/current/<run_id>/<repository_owner>/<repository_name>/<pr_number>/
```

History snapshot convention:

```text
<artifact_root>/pr-followup/history/<run_id>/<repository_owner>/<repository_name>/<pr_number>/<artifact_family>/<artifact_sequence>-<write_sequence>-<producer_step_id>.json
```

### Feedback evaluator LLM adapter availability or required plan follow-up

No dedicated LLM adapter exists for one `FeedbackEvaluationRequest` per item. Existing `llxprt` executor is process/prompt oriented and routes on stdout/file conditions. P09 must implement a feedback evaluator adapter compatible with existing command/process patterns or a dedicated wrapper that captures raw JSON responses, validates schema, and records accepted/rejected attempts without workflow self-loops.

### TOML nested params support

TOML and schema can carry nested params because `StepDef.parameters` is `Option<serde_json::Value>`. Python `tomllib` smoke parse showed nested tables become nested maps:

```text
{'artifact_root': '{artifact_dir}', 'budgets': {'max': 3}, 'commands': {'post_pr_tests': {'argv': ['cargo', 'test']}}}
```

P04/P16 should add Rust fixture/schema tests proving nested PR follow-through params survive through the actual config loader.

### Duplicate transition ambiguity/current behavior

`resolve_transition` and `resolve_transition_schema` iterate transitions in declaration order and return the first matching transition. No global duplicate-transition validation was found. P04/P16 must add workflow-specific graph tests for `llxprt-issue-fix-v1` that fail on duplicate outcome branches in the post-PR tail; do not assume global validation exists.

### GitHub API field preliminary inventory

Preliminary fields that must be pinned in fixtures/contracts before implementation consumes them:

- PR identity: `number`, `url`, `headRefName`, `baseRefName`, `headRefOid`/head SHA, `baseRefOid`/base SHA, repository owner/name, PR node ID.
- Checks/status: check suite/run ID, name, status, conclusion, started/completed timestamps, details URL, workflow name, head SHA, app/creator identity, jobs/log URL or database ID.
- Actions jobs/logs: workflow run ID, job ID, conclusion, status, head SHA, logs endpoint, check run association.
- Reviews/comments: review thread node ID, review comment node ID/database ID, body, author login/type, path, line/original line, diff hunk, outdated/resolved state, URL, created/updated timestamps, commit/head SHA when present.
- Issue comments/check summary: comment ID/node ID, author login/type, body, URL, created/updated timestamps.
- CodeRabbit readiness: bot/app identity, in-progress vs completed signal, stable observation key/hash, observed head SHA, surfaces included.
- Mutations: `resolveReviewThread` node ID response, comment creation ID/URL, permission errors and rate-limit errors.
- Push: local/remote head SHA, remote ref, push stderr/stdout, retryable/fatal class.

P01/P02 must turn this into `github-api-contract.md` with exact JSON paths and fixture files before schema field tables are finalized.

### Final dry-run CLI syntax

Recorded in `project-plans/coderabbit/analysis/final-dry-run-command.json` as argv-safe JSON:

```json
["cargo","run","--","run","--workflow-type","llxprt-issue-fix-v1","--config","llxprt-code","--dry-run"]
```

### Pending marker carry-forward artifact location

Use canonical `pending-feedback-marker-actions.json` under the PR follow-through current/history artifact store paths. Required current path:

```text
<artifact_root>/pr-followup/current/<run_id>/<repository_owner>/<repository_name>/<pr_number>/pending-feedback-marker-actions.json
```

Required history family:

```text
<artifact_root>/pr-followup/history/<run_id>/<repository_owner>/<repository_name>/<pr_number>/pending-feedback-marker-actions/<artifact_sequence>-<write_sequence>-<producer_step_id>.json
```

P02 may document an equivalent section mapping only if it preserves exact fields/history semantics, but default assumption is the standalone artifact above.

### Expected failing test manifest initialized

Initialized `project-plans/coderabbit/analysis/expected-failing-tests.json` with schema version, exact allowed groups, and empty `entries`. P04/P16 TDD phases add future-phase failures with exact test names and assertion substrings.

### StepOutcome routing/current Abandon behavior

- Valid current condition strings: `success`, `retryable`, `fatal`, `fixable`, `abandon` from `StepOutcome` display/parse logic.
- `StepOutcome::Abandon` is terminal in `EngineRunner::run()` before transition resolution and produces `RunOutcome::Abandoned`.
- Per-edge loop caps in runner produce `RunOutcome::Abandoned` directly rather than a routable fatal artifact.
- Current `llxprt-issue-fix-v1.toml` has `create_pr -> log_completion` on success and `create_pr -> abandon_and_log` on fatal. P16/P17 must replace the post-PR success tail and prevent post-PR `abandon`/`abandon_and_log` routes.

### Current workflow schema and routing contract representation

- `WorkflowType.transitions` is a list of `TransitionDef { from, to, condition, max_iterations }`.
- Missing condition means success transition.
- `resolve_transition_schema` returns first transition matching `from` and condition/default success.
- `EngineRunner::find_transition` also returns first matching transition for per-edge max iteration lookup.
- Therefore ambiguous duplicate branches are possible unless workflow-specific tests reject them.

### Dependencies verified

`Cargo.toml` includes `serde`, `serde_json`, `chrono`, `tokio`, `toml`, `thiserror`, `anyhow`, `clap`, `uuid`, and `rusqlite`. `cargo tree` verified `serde_json v1.0.149`, `chrono v0.4.44`, `tokio v1.51.0`, and `toml v0.8.23`.

## Follow-up decisions required before implementation phases consume assumptions

1. P01/P02: create fixture-backed `github-api-contract.md` with exact JSON paths and permission-denied fixtures.
2. P03/P04: add command-runner injection seam for `gh`, shell, git, post-PR tests, and marker actions.
3. P04/P06: add injectable clock/sleeper for watch/readiness loops.
4. P04/P16: add workflow-specific duplicate outcome branch detection for the post-PR tail.
5. P05: implement `PrFollowupArtifactStore` owning canonical/current, history snapshots, sequence allocation, atomic writes, and canonicalized `artifact_root` initialization.
6. P09: implement/dedicate feedback evaluator adapter; existing raw `llxprt` executor is insufficient as-is.
7. P12: implement dedicated `PrFollowupRemediationExecutor` wrapper seam and process evidence contract.
8. P16/P17: ensure every post-PR TOML step has exactly one `artifact_root`, no `artifact_dir`, and no post-PR `abandon`/`abandon_and_log` route.

## Completion gate

All required P0.5 deliverables were created and assumptions/gaps were documented. No production Rust or workflow TOML was modified by this phase.
