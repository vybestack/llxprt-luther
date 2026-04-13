# Step Execution Requirements (EARS)

Format legend:
- Ubiquitous: The `<system>` shall `<response>`.
- Event-driven: When `<trigger>`, the `<system>` shall `<response>`.
- Unwanted behavior: If `<fault/condition>`, then the `<system>` shall `<response>`.

---

## 1) Step executor dispatch

### REQ-EXEC-001 (Ubiquitous)
The engine shall dispatch step execution to a registered executor based on the step's `step_type` field.

### REQ-EXEC-002 (Unwanted behavior)
If no executor is registered for a step's `step_type`, then the engine shall return a `Fatal` outcome with an error message identifying the unregistered type.

---

## 2) Shell executor

### REQ-EXEC-003 (Ubiquitous)
The `ShellExecutor` shall run a shell command specified in the step's `parameters.command` field, capture stdout and stderr, and map exit code 0 to `Success` and non-zero to `Fixable`.

### REQ-EXEC-008 (Unwanted behavior)
If a shell command fails (non-zero exit), the engine shall capture stdout and stderr in the step context and return a `Fixable` outcome to allow remediation transitions.

### REQ-EXEC-009 (Unwanted behavior)
If a shell command cannot be spawned (binary not found, permission denied), the engine shall return a `Fatal` outcome.

---

## 3) Write-file executor

### REQ-EXEC-004 (Ubiquitous)
The `WriteFileExecutor` shall write content specified in `parameters.content` to the path specified in `parameters.path`, relative to the step context's working directory.

---

## 4) Step context

### REQ-EXEC-005 (Ubiquitous)
The step context shall carry key-value pairs across step executions within a single run, allowing later steps to read outputs from earlier steps.

### REQ-EXEC-006 (Ubiquitous)
Step parameters shall support `{variable}` interpolation, resolving keys from the step context's values map and built-in variables (`work_dir`, `run_id`).

---

## 5) Hello-world proof workflow

### REQ-EXEC-007 (Event-driven)
When the hello-world workflow is executed, the engine shall create a Rust project, write a test, write an implementation, run `cargo test`, and reach a `Success` outcome.

---

## 6) Backward compatibility

### REQ-EXEC-010 (Ubiquitous)
All existing tests shall be updated to use the new `EngineRunner` constructor (with `ExecutorRegistry`) and shall continue to pass. Tests using non-executable step types shall register a `NoOpExecutor` explicitly.
