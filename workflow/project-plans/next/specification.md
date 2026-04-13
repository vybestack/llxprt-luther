# Feature Specification: Step Execution and Hello-World Workflow

## Purpose

Make the Luther workflow engine actually execute steps by building a step executor dispatch system, concrete executor implementations (shell, write_file), step-to-step context passing, and a hello-world proof workflow that creates a Rust project, writes a test, writes an implementation, and runs `cargo test` to verify — all driven by the engine.

## Architectural Decisions

- **Pattern**: Strategy pattern for step executors behind a trait, with a registry mapping `step_type` → executor
- **Technology Stack**: No new dependencies. `std::process::Command` for shell, `std::fs` for file writes, existing `serde_json` for parameter extraction
- **Data Flow**: `EngineRunner` → `ExecutorRegistry::dispatch(step_type)` → `StepExecutor::execute(step_def, context)` → `StepOutcome`
- **Integration Points**: Modifies `EngineRunner::execute_step()` to dispatch instead of returning hardcoded Success

## Project Structure

```text
src/engine/
  executor.rs          # StepExecutor trait, ExecutorRegistry, StepContext, ExecutionError
  executors/
    mod.rs             # Sub-module exports
    shell.rs           # ShellExecutor
    write_file.rs      # WriteFileExecutor
```

## Technical Environment

- **Type**: Extension to existing workflow engine
- **Runtime**: Synchronous step execution (std::process::Command), within existing tokio app
- **Configuration Format**: Existing TOML/JSON workflow definitions; step parameters via serde_json::Value
- **Dependencies**: No new crates

## Integration Points (MANDATORY)

### Existing Code That Will Use This Feature
- `src/engine/runner.rs` — `execute_step()` will dispatch through `ExecutorRegistry` instead of returning `Ok(StepOutcome::Success)`
- `src/main.rs` — No changes needed; `handle_run_command` already calls `runner.run()`
- `src/workflow/config_loader.rs` — No changes needed; already loads step definitions with `parameters` field

### Existing Code To Be Modified
- `src/engine/runner.rs` — Replace hardcoded Success in `execute_step()` with registry dispatch
- `src/engine/instance.rs` — Add `StepContext` field to `WorkflowInstance`
- `src/engine/mod.rs` — Add `pub mod executor; pub mod executors;` and re-exports

### Existing Tests To Be Updated
- All tests that construct `EngineRunner::new(instance)` must be updated to pass an `ExecutorRegistry`. For engine tests that use step_types like `"analysis"` and `"planning"`, register a simple `NoOpExecutor` that returns `Success` — making their intent explicit rather than relying on hidden fallback behavior.

### User Access Points
- `luther run --workflow-type hello-world-v1 --config hello-world-config`
- Existing `luther run` invocations continue to work

### Migration Requirements
- `EngineRunner::new()` signature changes to require an `ExecutorRegistry`. All callers (tests and main.rs) updated. No backward-compatible shim — the old signature is removed.

## Formal Requirements

### REQ-EXEC-001 (Ubiquitous)
The engine shall dispatch step execution to a registered executor based on the step's `step_type` field.

### REQ-EXEC-002 (Unwanted behavior)
If no executor is registered for a step's `step_type`, then the engine shall return a `Fatal` outcome with an error message identifying the unregistered type.

### REQ-EXEC-003 (Ubiquitous)
The `ShellExecutor` shall run a shell command specified in the step's `parameters.command` field, capture stdout and stderr, and map exit code 0 to `Success` and non-zero to `Fixable`.

### REQ-EXEC-004 (Ubiquitous)
The `WriteFileExecutor` shall write content specified in `parameters.content` to the path specified in `parameters.path`, relative to the step context's working directory.

### REQ-EXEC-005 (Ubiquitous)
The step context shall carry key-value pairs across step executions within a single run, allowing later steps to read outputs from earlier steps.

### REQ-EXEC-006 (Ubiquitous)
Step parameters shall support `{variable}` interpolation, resolving keys from the step context's values map and built-in variables (`work_dir`, `run_id`).

### REQ-EXEC-007 (Event-driven)
When the hello-world workflow is executed, the engine shall create a Rust project, write a test, write an implementation, run `cargo test`, and reach a `Success` outcome.

### REQ-EXEC-008 (Unwanted behavior)
If a shell command fails (non-zero exit), the engine shall capture stdout and stderr in the step context and return a `Fixable` outcome to allow remediation transitions.

### REQ-EXEC-009 (Unwanted behavior)
If a shell command cannot be spawned (binary not found, permission denied), the engine shall return a `Fatal` outcome.

### REQ-EXEC-010 (Ubiquitous)
All existing tests shall be updated to use the new `EngineRunner` constructor (with `ExecutorRegistry`) and shall continue to pass. Tests that use non-executable step types (e.g., `"analysis"`, `"planning"`) shall register a `NoOpExecutor` explicitly.

## Data Schemas

```rust
/// Trait for step executors.
pub trait StepExecutor: Send + Sync {
    fn execute(
        &self,
        step: &StepDef,
        ctx: &mut StepContext,
    ) -> Result<StepOutcome, ExecutionError>;
}

/// Mutable context passed between steps.
pub struct StepContext {
    pub run_id: String,
    pub workflow_type_id: String,
    pub config_id: String,
    pub work_dir: PathBuf,
    pub values: HashMap<String, serde_json::Value>,
}

/// Registry mapping step_type → executor.
pub struct ExecutorRegistry {
    executors: HashMap<String, Box<dyn StepExecutor>>,
}

/// Errors from step execution (distinct from engine routing errors).
pub enum ExecutionError {
    ParameterMissing { step_id: String, param: String },
    IoError { step_id: String, message: String },
    SpawnError { step_id: String, command: String, message: String },
}
```

## Example Data

### hello-world-v1.toml (workflow type)

```toml
workflow_type_id = "hello-world-v1"

[[steps]]
step_id = "init_project"
step_type = "shell"
description = "Initialize a new Rust project"
[steps.parameters]
command = "cargo init --name hello_project {work_dir}/hello_project"

[[steps]]
step_id = "write_test"
step_type = "write_file"
description = "Write a hello world test"
[steps.parameters]
path = "hello_project/tests/hello_test.rs"
content = """
use hello_project::hello;

#[test]
fn test_hello() {
    assert_eq!(hello(), "Hello, world!");
}
"""

[[steps]]
step_id = "write_impl"
step_type = "write_file"
description = "Write hello function implementation"
[steps.parameters]
path = "hello_project/src/lib.rs"
content = """
pub fn hello() -> &'static str {
    "Hello, world!"
}
"""

[[steps]]
step_id = "run_tests"
step_type = "shell"
description = "Run cargo test to verify"
[steps.parameters]
command = "cd {work_dir}/hello_project && cargo test"

[[steps]]
step_id = "complete"
step_type = "shell"
description = "Log completion"
[steps.parameters]
command = "echo done"

[[transitions]]
from = "init_project"
to = "write_test"

[[transitions]]
from = "write_test"
to = "write_impl"

[[transitions]]
from = "write_impl"
to = "run_tests"

[[transitions]]
from = "run_tests"
to = "complete"
condition = "success"

[[transitions]]
from = "run_tests"
to = "write_impl"
condition = "fixable"
```

### hello-world-config.toml (workflow config)

```toml
config_id = "hello-world-config"
workflow_type_id = "hello-world-v1"

[runtime]
timeout_seconds = 300
max_retries = 2

[repository]
workspace_strategy = "temp_clone"
branch_template = "hello-{run_id}"
base_branch = "main"

[guards]
max_iterations = 3
max_file_changes = 20
```

## Constraints

- No new crate dependencies
- Synchronous step execution only (no async executors in this plan)
- Simple `{key}` variable interpolation, not a template language
- Hello-world test uses temp directories only — no filesystem side effects
- Must not break any existing tests
