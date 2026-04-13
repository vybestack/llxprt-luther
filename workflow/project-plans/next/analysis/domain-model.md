# Domain Model Analysis

**Plan**: PLAN-20260408-STEP-EXEC  
**Phase**: 01 - Domain Analysis  
**Date**: 2026-04-08

---

## 1. Engine Execution Boundary

### 1.1 `execute_step()` Signature and Location

**File**: `src/engine/runner.rs`  
**Line**: 236

```rust
/// Execute a single step and return its outcome.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
/// @requirement:REQ-EARS-ENG-002
pub fn execute_step(&mut self, step_id: &str) -> Result<StepOutcome, EngineError> {
```

**Current Implementation** (lines 236-252):
```rust
pub fn execute_step(&mut self, step_id: &str) -> Result<StepOutcome, EngineError> {
    // Verify step exists in the workflow
    let step_exists = self
        .instance
        .workflow_type
        .steps
        .iter()
        .any(|s| s.step_id == step_id);

    if !step_exists {
        return Err(EngineError::StepNotFound(step_id.to_string()));
    }

    // For now, return Success for all steps (to be implemented with actual step logic)
    // This is a placeholder that allows tests to pass
    Ok(StepOutcome::Success)
}
```

### 1.2 Call Site in `run()` Loop

**File**: `src/engine/runner.rs`  
**Line**: 174

```rust
// Execute the current step
let outcome = self.execute_step(&current_step_id)?;
```

**Full Context** (lines 162-188):
```rust
loop {
    // Check for interrupt
    if *self.interrupted.borrow() {
        let checkpoint = self.create_checkpoint(&current_step_id, "interrupted");
        let conn = self.conn.borrow();
        save_checkpoint_with_conn(&conn, &checkpoint)?;
        return Ok(RunOutcome::Interrupted {
            step_id: current_step_id,
        });
    }

    // Execute the current step
    let outcome = self.execute_step(&current_step_id)?;

    // Persist checkpoint and event
    let checkpoint = self.create_checkpoint(&current_step_id, "completed");
    let conn = self.conn.borrow();
    save_checkpoint_with_conn(&conn, &checkpoint)?;
    append_event_with_conn(
        &conn,
        &self.instance.run_id,
        &current_step_id,
        &outcome.to_string(),
        chrono::Utc::now(),
    )?;
    drop(conn);
```

### 1.3 Data Available at Call Site

At line 174, the following data is accessible via `self`:

| Data | Access Path | Type |
|------|-------------|------|
| `step_id` | `current_step_id: String` (parameter to `execute_step`) | `&str` |
| `instance` | `self.instance` | `WorkflowInstance` |
| `workflow_type` | `self.instance.workflow_type` | `WorkflowType` |
| `steps` | `self.instance.workflow_type.steps` | `Vec<StepDef>` |
| `transitions` | `self.instance.workflow_type.transitions` | `Vec<TransitionDef>` |
| `run_id` | `self.instance.run_id` | `String` |
| `config` | `self.instance.config` | `WorkflowConfig` |
| `retry_count` | `self.retry_count` | `u32` |
| `loop_count` | `self.loop_count` | `u32` |

### 1.4 StepOutcome Flow Back to Transition Resolution

**Lines 174-231**: After `execute_step()` returns `outcome`:

1. **Persistence** (lines 177-187): Checkpoint and event persisted
2. **Terminal Check** (lines 190-204): Fatal/Abandon outcomes return early
3. **Transition Resolution** (line 207): `resolve_next_step(&current_step_id, &outcome)`
4. **Loop Handling** (lines 209-230): Loop detection and counter increment

---

## 2. Step Definition Data Flow

### 2.1 TOML → Schema.rs Parse Chain

**StepDef Definition** (`src/workflow/schema.rs`, lines 60-66):

```rust
/// Definition of a workflow step.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
#[derive(Debug, Clone, serde::Deserialize)]
pub struct StepDef {
    pub step_id: String,
    pub step_type: String,
    pub description: Option<String>,
    pub parameters: Option<serde_json::Value>,
}
```

**TOML Example** (`tests/fixtures/hello-world-v1.toml`):
```toml
[[steps]]
step_id = "write-hello"
step_type = "write_file"
description = "Write hello.txt"
parameters = { path = "hello.txt", content = "Hello, World!" }
```

### 2.2 Data Flow Diagram

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              DATA FLOW                                      │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  hello-world-v1.toml                                                        │
│  ┌─────────────────────────────────────┐                                    │
│  │ [[steps]]                           │                                    │
│  │ step_id = "write-hello"             │────┐                               │
│  │ step_type = "write_file"            │    │                               │
│  │ parameters = { ... }                │    │  (serde deserialization)      │
│  └─────────────────────────────────────┘    │                               │
│                                               ▼                               │
│  src/workflow/schema.rs                       ┌─────────────────────────────┐│
│  ┌─────────────────────────────────────┐      │ StepDef {                   ││
│  │ #[derive(Deserialize)]              │◄─────│   step_id: "write-hello", ││
│  │ pub struct StepDef {               │      │   step_type: "write_file",││
│  │   pub step_id: String,              │      │   parameters: Some({...})   ││
│  │   pub step_type: String,            │      │ }                           ││
│  │   pub parameters: Option<Value>,    │      └─────────────────────────────┘│
│  │ }                                   │                  │                  │
│  └─────────────────────────────────────┘                  │                  │
│                                                           │                  │
│  src/engine/instance.rs                                   │                  │
│  ┌─────────────────────────────────────┐                    │                  │
│  │ pub struct WorkflowInstance {       │◄─────────────────┘                  │
│  │   pub workflow_type: WorkflowType,  │                                   │
│  │   pub config: WorkflowConfig,       │                                   │
│  │   pub run_id: String,               │                                   │
│  │   pub current_state: String,          │                                   │
│  │ }                                   │                                   │
│  └─────────────────────────────────────┘                                   │
│                                                           │                  │
│  src/engine/runner.rs                                     │                  │
│  ┌─────────────────────────────────────┐                    │                  │
│  │ pub struct EngineRunner {           │◄─────────────────┘                  │
│  │   instance: WorkflowInstance,         │                                   │
│  │   ...                               │                                   │
│  │ }                                   │                                   │
│  └─────────────────────────────────────┘                                   │
│                                                           │                  │
│                                                           ▼                  │
│  execute_step("write-hello")              ┌─────────────────────────────┐    │
│  ┌─────────────────────────────────────┐  │ step_def = self             │    │
│  │ let step_exists = self              │  │   .instance                 │    │
│  │   .instance                         │  │   .workflow_type            │    │
│  │   .workflow_type                     │  │   .steps                     │    │
│  │   .steps                             │  │   .iter()                    │    │
│  │   .iter()                            │  │   .find(|s| s.step_id == ...│    │
│  │   .any(|s| s.step_id == step_id);   │  │                             │    │
│  └─────────────────────────────────────┘  └─────────────────────────────┘    │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 2.3 Looking Up StepDef by step_id

**Pattern** (from `execute_step()` line 275-281):

```rust
let step_exists = self
    .instance
    .workflow_type
    .steps
    .iter()
    .any(|s| s.step_id == step_id);
```

**To get the full StepDef** (for executor dispatch):

```rust
let step_def = self
    .instance
    .workflow_type
    .steps
    .iter()
    .find(|s| s.step_id == step_id)
    .ok_or_else(|| EngineError::StepNotFound(step_id.to_string()))?;
```

**Fields Available**:
- `step_def.step_id: String` - Unique identifier
- `step_def.step_type: String` - Dispatch key for executor registry
- `step_def.description: Option<String>` - Human-readable
- `step_def.parameters: Option<serde_json::Value>` - Step-specific config

---

## 3. Existing Test Compatibility

### 3.1 EngineRunner Call Sites (ALL must be updated)

**Summary**: 10 call sites across 4 files. All will be updated to pass `ExecutorRegistry` with `NoOpExecutor`. No backward-compatibility shim.

| File | Line(s) | Context | Step Types Used |
|------|---------|---------|-----------------|
| `tests/engine_execution_integration.rs` | 105, 149, 235, 282 | `EngineRunner::new(instance)` | "test", "analysis", "execution" |
| `tests/engine_resume_integration.rs` | 115, 167, 243 | `EngineRunner::new(instance)` | "test", "analysis", "execution" |
| `tests/persistence_integration.rs` | 173 | `EngineRunner::new(instance)` | "test" |
| `src/main.rs` | 124 | `EngineRunner::new(instance)` | Real workflow step_types |

### 3.2 Detailed Call Site Analysis

#### tests/engine_execution_integration.rs

**Line 105** (in `test_step_transition_uses_structured_outcome`):
```rust
let mut runner = EngineRunner::new(instance);
```
- Workflow uses step_type: `"test"`
- Will register: `NoOpExecutor` for `"test"`

**Line 149** (in `test_fatal_error_routes_to_terminal`):
```rust
let mut runner = EngineRunner::new(instance);
```
- Step types: `"test"`, `"cleanup"`
- Will register: `NoOpExecutor` for both

**Line 235** (in `test_loop_back_transition_increments_counter`):
```rust
let mut runner = EngineRunner::new(instance);
```
- Step types: `"test"`, `"analysis"`, `"execution"`
- Will register: `NoOpExecutor` for all

**Line 282** (in `test_loop_limit_exceeded_abandons`):
```rust
let mut runner = EngineRunner::new(instance);
```
- Step types: `"test"`
- Will register: `NoOpExecutor`

#### tests/engine_resume_integration.rs

**Line 115** (in `test_resume_from_checkpoint_continues_at_step`):
```rust
let mut resumed_runner = EngineRunner::new(resumed_instance);
```

**Line 167** (in `test_interrupt_persists_resumable_checkpoint`):
```rust
let mut runner = EngineRunner::new(instance);
```

**Line 243** (in `test_resume_preserves_loop_and_retry_counters`):
```rust
let mut runner = EngineRunner::new(resumed_instance);
```

All use `test_workflow_type()` with step_type: `"test"`

#### tests/persistence_integration.rs

**Line 173** (in `test_persistence_error_halts_execution`):
```rust
let mut runner = EngineRunner::new(instance);
```
- Step types: `"test"`
- Will register: `NoOpExecutor`

#### src/main.rs

**Line 124** (in `handle_run_command`):
```rust
let mut runner = EngineRunner::new(instance);
```
- Real workflow step_types from TOML
- Will use: `ExecutorRegistry::with_defaults()`

### 3.3 Test Update Strategy

**No Shim Philosophy**: Per plan specification, there is NO fallback "return Success for everything" path. All tests must explicitly provide an ExecutorRegistry.

**Pattern for Tests**:
```rust
// Before:
let mut runner = EngineRunner::new(instance);

// After:
let mut registry = ExecutorRegistry::new();
registry.register("test", Box::new(NoOpExecutor));
let mut runner = EngineRunner::new(instance, registry);
```

**Pattern for main.rs**:
```rust
// Before:
let mut runner = EngineRunner::new(instance);

// After:
let registry = ExecutorRegistry::with_defaults();
let mut runner = EngineRunner::new(instance, registry);
```

---

## 4. Context Passing Design

### 4.1 Decision: StepContext Lives on EngineRunner

**Rationale**: EngineRunner already has `&mut self` in `execute_step()` and manages the full run lifecycle.

| Aspect | EngineRunner | WorkflowInstance |
|--------|--------------|------------------|
| **Mutable Access** | [OK] Already has `&mut self` in execution loop | [ERROR] Instance is owned, but context mutations during run needed |
| **Lifecycle Owner** | [OK] Owns the run from start to finish | [ERROR] Just data container |
| **Already Thread-safe** | [OK] Has `RefCell` for interrupt handling | N/A |
| **Checkpoint/Resume** | [OK] Handles persistence | [ERROR] No persistence logic |
| **Injectability** | [OK] Natural place for registry field | [ERROR] Would need to add |

### 4.2 EngineRunner Field Addition

**Current Struct** (`src/engine/runner.rs`, lines 69-85):

```rust
pub struct EngineRunner {
    /// The workflow instance being executed.
    instance: WorkflowInstance,
    /// Current retry count for the current step.
    retry_count: u32,
    /// Remediation loop counter for tracking cycles.
    loop_count: u32,
    /// Maximum retries allowed from config.
    max_retries: u32,
    /// Maximum remediation loops allowed.
    max_loops: u32,
    /// SQLite connection for persistence.
    conn: RefCell<Connection>,
    /// Flag indicating if an interrupt was received.
    interrupted: RefCell<bool>,
}
```

**Proposed Addition**:

```rust
pub struct EngineRunner {
    /// The workflow instance being executed.
    instance: WorkflowInstance,
    /// Current retry count for the current step.
    retry_count: u32,
    /// Remediation loop counter for tracking cycles.
    loop_count: u32,
    /// Maximum retries allowed from config.
    max_retries: u32,
    /// Maximum remediation loops allowed.
    max_loops: u32,
    /// SQLite connection for persistence.
    conn: RefCell<Connection>,
    /// Flag indicating if an interrupt was received.
    interrupted: RefCell<bool>,
    /// Executor registry for dispatching step execution.
    executor_registry: ExecutorRegistry,
    /// Step context accumulator for values across steps.
    step_context: StepContext,
}
```

### 4.3 Constructor Signature Change

**Current** (line 88):
```rust
pub fn new(instance: WorkflowInstance) -> Self
```

**New**:
```rust
pub fn new(instance: WorkflowInstance, registry: ExecutorRegistry) -> Self
```

**Implementation**:
```rust
pub fn new(instance: WorkflowInstance, registry: ExecutorRegistry) -> Self {
    let max_retries = instance.config.runtime.max_retries;
    let max_loops = instance.config.guard_limits.max_iterations.unwrap_or(10);

    let conn = Connection::open_in_memory()
        .expect("Failed to create in-memory database for runner");

    Self {
        instance,
        retry_count: 0,
        loop_count: 0,
        max_retries,
        max_loops,
        conn: RefCell::new(conn),
        interrupted: RefCell::new(false),
        executor_registry: registry,
        step_context: StepContext::new(),
    }
}
```

---

## 5. Integration Touch Points

### 5.1 File-by-File Modification Plan

| File | Line(s) | Action Required |
|------|---------|-----------------|
| **src/engine/runner.rs** | 69-85 | Add `executor_registry` and `step_context` fields to struct |
| **src/engine/runner.rs** | 88-108 | Update `new()` signature and implementation |
| **src/engine/runner.rs** | 110-140 | Update `with_db_path()` signature and implementation |
| **src/engine/runner.rs** | 274-286 | Rewrite `execute_step()` to dispatch through registry |
| **src/engine/runner.rs** | 421 | Update test `engine_runner_can_be_created` |
| **src/main.rs** | 122-124 | Construct `ExecutorRegistry::with_defaults()`, pass to `new()` |
| **tests/engine_execution_integration.rs** | 105, 149, 235, 282 | Create registry with `NoOpExecutor`, pass to `new()` |
| **tests/engine_resume_integration.rs** | 115, 167, 243 | Create registry with `NoOpExecutor`, pass to `new()` |
| **tests/persistence_integration.rs** | 173 | Create registry with `NoOpExecutor`, pass to `new()` |

### 5.2 Detailed Line References

#### src/engine/runner.rs

```
Line  69-84   : MODIFY - Add executor_registry and step_context fields
Line  90-110  : MODIFY - EngineRunner::new() signature: add registry parameter
Line 112-143  : MODIFY - EngineRunner::with_db_path() signature: add registry parameter
Line 236-252  : REWRITE - execute_step() to use registry.dispatch() and pass StepContext
Line 369      : MODIFY - Test: engine_runner_can_be_created uses EngineRunner::new(instance)
```

#### src/main.rs

```
Line 122-124  : MODIFY - Add registry construction, pass to EngineRunner::new()
Current:
    // 4. Create EngineRunner
    let instance = WorkflowInstance::create(workflow_type, config);
    let mut runner = EngineRunner::new(instance);

New:
    // 4. Create EngineRunner with executor registry
    let instance = WorkflowInstance::create(workflow_type, config);
    let registry = ExecutorRegistry::with_defaults();
    let mut runner = EngineRunner::new(instance, registry);
```

#### tests/engine_execution_integration.rs

```
Line 105      : MODIFY - Add registry, NoOpExecutor for "test"
Line 149      : MODIFY - Add registry, NoOpExecutor for "test", "cleanup"
Line 235      : MODIFY - Add registry, NoOpExecutor for "test", "analysis", "execution"
Line 282      : MODIFY - Add registry, NoOpExecutor for "test"
```

#### tests/engine_resume_integration.rs

```
Line 115      : MODIFY - Add registry, NoOpExecutor for "test"
Line 167      : MODIFY - Add registry, NoOpExecutor for "test"
Line 243      : MODIFY - Add registry, NoOpExecutor for "test"
```

#### tests/persistence_integration.rs

```
Line 173      : MODIFY - Add registry, NoOpExecutor for "test"
```

### 5.3 Execution Flow After Integration

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                         EXECUTION FLOW WITH REGISTRY                            │
├─────────────────────────────────────────────────────────────────────────────────┤
│                                                                                 │
│  EngineRunner::run()                                                            │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │ loop {                                                                   │   │
│  │   let outcome = self.execute_step(&current_step_id)?;                    │   │
│  │   // Line 172                                                            │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                              │                                                  │
│                              ▼                                                  │
│  EngineRunner::execute_step() (MODIFIED)                                        │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │ 1. Find StepDef by step_id                                               │   │
│  │    let step_def = self.instance.workflow_type.steps.iter()...            │   │
│  │                                                                          │   │
│  │ 2. Get step_type for dispatch                                            │   │
│  │    let step_type = &step_def.step_type;                                  │   │
│  │                                                                          │   │
│  │ 3. Dispatch through registry                                             │   │
│  │    let outcome = self.executor_registry.dispatch(                          │   │
│  │        step_type,                                                        │   │
│  │        step_def,                                                         │   │
│  │        &mut self.step_context                                            │   │
│  │    )?;                                                                   │   │
│  │                                                                          │   │
│  │ 4. Return outcome                                                        │   │
│  │    Ok(outcome)                                                           │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                              │                                                  │
│                              ▼                                                  │
│  ExecutorRegistry::dispatch()                                                   │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │ 1. Look up executor by step_type                                           │
│  │    let executor = self.executors.get(step_type)?                         │   │
│  │                                                                          │   │
│  │ 2. Delegate execution                                                    │   │
│  │    executor.execute(step_def, context)                                   │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                              │                                                  │
│                              ▼                                                  │
│  Concrete Executor (e.g., NoOpExecutor)                                           │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │ fn execute(&self, step_def: &StepDef, ctx: &mut StepContext) -> ...        │   │
│  │     // Executor-specific logic                                             │   │
│  │     // Access: step_def.parameters                                          │   │
│  │     // Mutate: ctx.set_output(key, value)                                  │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```

---

## Summary

### Key Findings

1. **EngineRunner is the natural home for StepContext** — it already has `&mut self` in `execute_step()` and manages run lifecycle

2. **10 call sites need EngineRunner::new() updates** — all must pass an ExecutorRegistry

3. **StepDef lookup is simple iteration** — `instance.workflow_type.steps.iter().find(|s| s.step_id == step_id)`

4. **step_type is the dispatch key** — available at `step_def.step_type`

5. **parameters are serde_json::Value** — parsed automatically via serde, accessed via `step_def.parameters`

### Next Steps

Proceed to Phase 02 (Executor Registry Design) with confidence that the integration points are fully understood.
