# Luther Workflow — Step Execution and Hello-World Workflow

## 1) Overview

This document defines the specification for making the Luther workflow engine actually execute steps. The initial plan delivered the orchestration skeleton — config loading, transition routing, checkpoint persistence, monitor supervision, CLI — but left `execute_step()` as a no-op returning `Success` for every step regardless of type.

This plan fills that gap by building **step executors** and proving them with a concrete **hello-world workflow** that creates a project, writes a test, writes an implementation, and runs verification — all driven by the engine.

The hello-world workflow is deliberately simple: no CodeRabbit, no PR submission, no LLM integration. It exists solely to prove the engine can dispatch real work through typed step handlers, pass context between steps, and route transitions based on actual outcomes.

## 2) What exists today

From the initial plan (PLAN-20260404-INITIAL-RUNTIME, phases 01–12, all PASS):

- **Workflow schema** — `WorkflowType`, `WorkflowConfig`, `StepDef`, `TransitionDef` with TOML/JSON loading
- **Engine loop** — `EngineRunner.run()` walks steps, resolves transitions, enforces loop/retry guards
- **Persistence** — SQLite checkpoints, events, run metadata, file artifacts
- **Monitor** — heartbeats, singleton locking, IPC, restart policies
- **Service** — foreground mode, launchd/systemd generation
- **CLI** — `run`, `status`, `service` commands wired up
- **118 passing tests** across unit and integration suites

### What does NOT work

- `EngineRunner::execute_step()` returns `Ok(StepOutcome::Success)` for every step — no actual dispatch
- `StepDef.parameters` is `Option<serde_json::Value>` but nothing reads or uses it
- No mechanism for step output → next step input (context passing)
- No concrete step type implementations (`shell`, `write_file`, etc.)
- The `dagrs_runtime.rs` is a stub

## 3) What this plan delivers

1. **Step executor trait and dispatch** — A `StepExecutor` trait and a registry that maps `step_type` strings to executor implementations.
2. **Shell executor** — Runs shell commands, captures stdout/stderr/exit code, maps to `StepOutcome`.
3. **Write-file executor** — Writes content to a file path (for generating source/test files).
4. **Step context** — A key-value bag that accumulates across steps so earlier outputs feed later inputs.
5. **Engine integration** — `EngineRunner::execute_step()` dispatches through the executor registry instead of returning a hardcoded `Success`.
6. **Hello-world workflow definition** — A workflow type + config that:
   - Creates a temp project directory
   - Writes a Rust test file (`hello_test.rs`) that expects a `hello()` function
   - Writes a Rust implementation file (`hello.rs`) with the `hello()` function
   - Runs `cargo test` to verify
7. **End-to-end integration test** — Proves the CLI `run` command can execute the hello-world workflow from definition through completion.

## 4) Functional specification

### 4.1 Step executor contract

```rust
pub trait StepExecutor: Send + Sync {
    fn execute(&self, step: &StepDef, ctx: &mut StepContext) -> Result<StepOutcome, ExecutionError>;
}
```

- Receives the step definition (including `parameters`) and a mutable context bag.
- Returns a `StepOutcome` (Success, Retryable, Fatal, Fixable, Abandon) or an error.
- Executors are stateless — all state travels through `StepContext`.

### 4.2 Step context

```rust
pub struct StepContext {
    pub run_id: String,
    pub workflow_type_id: String,
    pub config_id: String,
    pub work_dir: PathBuf,
    pub values: HashMap<String, serde_json::Value>,
}
```

- `work_dir` is the working directory for the run (from config or temp).
- `values` is a key-value map that steps read from and write to.
- Steps declare what they produce and consume via their `parameters`.

### 4.3 Executor registry

A `HashMap<String, Box<dyn StepExecutor>>` mapping step_type → executor:

- `"shell"` → `ShellExecutor` — runs a command, captures output
- `"write_file"` → `WriteFileExecutor` — writes content to a path relative to `work_dir`
- Unknown types → `Fatal` outcome with clear error message

### 4.4 Shell executor behavior

Parameters (from `StepDef.parameters`):
- `command` (string, required) — the shell command to run
- `working_dir` (string, optional) — override work_dir for this step
- `timeout_seconds` (u64, optional) — per-step timeout

Behavior:
- Runs command via `std::process::Command` with `sh -c`
- Captures stdout and stderr into `StepContext.values` as `"{step_id}.stdout"` and `"{step_id}.stderr"`
- Exit code 0 → `Success`
- Non-zero exit → `Fixable` (allows remediation loop)
- Spawn/IO failure → `Fatal`

### 4.5 Write-file executor behavior

Parameters (from `StepDef.parameters`):
- `path` (string, required) — file path relative to `work_dir`
- `content` (string, required) — file content to write
- `mkdir` (bool, optional, default true) — create parent directories

Behavior:
- Resolves `path` relative to context `work_dir`
- Creates parent dirs if `mkdir` is true
- Writes content to file
- Success → `Success`
- IO error → `Fatal`

### 4.6 Hello-world workflow

Workflow type: `hello-world-v1`

Steps:
1. `init_project` (shell) — `cargo init --name hello_project {work_dir}/hello_project`
2. `write_test` (write_file) — writes a test into `hello_project/tests/hello_test.rs`
3. `write_impl` (write_file) — writes `pub fn hello() -> &'static str { "Hello, world!" }` into `hello_project/src/lib.rs`
4. `run_tests` (shell) — `cd {work_dir}/hello_project && cargo test`
5. `complete` (shell) — `echo "Workflow complete"`

Transitions:
- `init_project` → `write_test` (success)
- `write_test` → `write_impl` (success)
- `write_impl` → `run_tests` (success)
- `run_tests` → `complete` (success)
- `run_tests` → `write_impl` (fixable — allows one remediation loop)

Config: `hello-world-config`
- `max_retries: 2`
- `max_iterations: 3`
- `workspace_strategy: "temp_clone"` (uses temp dir)

### 4.7 Variable interpolation in parameters

Step parameters support `{variable}` interpolation from `StepContext.values`:
- `{work_dir}` → resolves to the run's working directory
- `{step_id.stdout}` → stdout from a previous step
- `{run_id}` → the current run ID

This is simple string replacement, not a full template engine.

## 5) Integration points

### Existing code that will be MODIFIED

| File | Change |
|---|---|
| `src/engine/runner.rs` | `EngineRunner` always takes an `ExecutorRegistry`; `execute_step()` always dispatches through it; `new()` signature changes to require registry |
| `src/engine/mod.rs` | Re-export new executor types |
| `src/engine/instance.rs` | Add `StepContext` to `WorkflowInstance` |
| `src/main.rs` | `handle_run_command` passes `ExecutorRegistry::with_defaults()` when constructing runner |
| `src/lib.rs` | No changes needed (engine module already exported) |
| Existing tests | All tests that construct `EngineRunner` updated to pass an `ExecutorRegistry` (either with defaults or with test-specific executors) |

### New code

| File | Purpose |
|---|---|
| `src/engine/executor.rs` | `StepExecutor` trait, `ExecutorRegistry`, `StepContext`, `ExecutionError` |
| `src/engine/executors/mod.rs` | Executor sub-module |
| `src/engine/executors/shell.rs` | `ShellExecutor` |
| `src/engine/executors/write_file.rs` | `WriteFileExecutor` |
| `tests/fixtures/workflows/valid/hello-world-v1.toml` | Hello-world workflow type definition |
| `tests/fixtures/workflow-configs/valid/hello-world-config.toml` | Hello-world config |
| `tests/hello_world_workflow_integration.rs` | End-to-end integration test |

### User access points

- `luther run --workflow-type hello-world-v1 --config hello-world-config` executes the hello-world workflow
- Existing `luther run` for other workflow types continues to work (unknown step_types produce Fatal)

## 6) Constraints

- No new crate dependencies. `std::process::Command` for shell execution; existing `serde_json` for parameter extraction.
- No async executors in this plan — shell commands run synchronously via `Command`. Async can be layered later.
- Variable interpolation is simple `{key}` replacement, not a template language. No nested expressions, no conditionals.
- The hello-world workflow uses a temp directory cleaned up after the test. It does not touch the real filesystem outside of temp.
- No backward-compatibility shims. `EngineRunner` always requires an `ExecutorRegistry`. Existing tests are updated to supply one. There is no fallback "return Success for everything" path.

## 7) Acceptance criteria

1. `EngineRunner::execute_step()` dispatches to real executors based on `step_type`.
2. `ShellExecutor` runs shell commands and maps exit codes to `StepOutcome`.
3. `WriteFileExecutor` writes files and reports success/failure.
4. `StepContext` carries values between steps within a run.
5. The hello-world workflow type and config load, validate, and execute end-to-end.
6. `cargo test` for the hello-world integration test passes — proving a real workflow that creates files, runs a compiler, and verifies output.
7. All 118 existing tests continue to pass.
8. No `todo!()`, `unimplemented!()`, or placeholder implementations in delivered code.
