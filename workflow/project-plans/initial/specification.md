# Feature Specification: Luther Workflow Runtime (Initial)

## Purpose

Build a locally runnable, embeddable Rust workflow runtime that can execute bounded software-engineering workflows with explicit steps, branches, loops, and durable state.

## Architectural Decisions

- **Pattern**: Monitor -> Engine -> Workflow Type -> Workflow Config
- **Technology Stack**: Rust (single crate + xtask), tokio runtime, serde/toml/json parsing, local persistence
- **Data Flow**: CLI/service trigger -> monitor supervision -> engine bind `(workflow_type_id, config_id, run_id)` -> step execution -> persistence/artifacts
- **Integration Points**: CLI command surface, monitor lifecycle, engine routing/persistence, repo preparation, service generation

## Project Structure

```text
src/
  main.rs
  lib.rs
  cli/
  monitor/
  engine/
  workflow/
  adapters/
  persistence/
  service/
config/
  workflows/
  workflow-configs/
tests/
  integration-oriented behavioral tests
```

## Technical Environment

- **Type**: Local runtime + supervised foreground service process
- **Runtime**: Rust with tokio for async boundaries
- **Configuration Format**: TOML primary, JSON optional equivalent; no YAML in MVP
- **Dependencies**: constrained by `Cargo.toml` and preflight verification before additions

## Integration Points (MANDATORY)

### Existing Code That Will Use This Feature
- `src/main.rs`: replace bootstrap print path with CLI/monitor run entrypoint
- `src/lib.rs`: evolve from placeholder function to module exports for runtime layers
- `xtask/`: quality/release flows remain the gate for fmt/clippy/tests/coverage/release

### Existing Code To Be Replaced
- bootstrap-only behavior in `src/main.rs`
- placeholder-only crate shape in `src/lib.rs`

### User Access Points
- `luther run --workflow-type <id> --config <id>`
- `luther status`
- `luther service install|start|stop|status`

### Migration Requirements
- move from bootstrap-only binary to monitor/engine runtime startup path
- preserve existing quality/release automation while introducing runtime modules

## Formal Requirements

Canonical requirement source:
- `project-plans/initial/requirements-ears.md`

This plan implements all requirement groups:
- ARCH, WF, MON, ENG, ROUTE, REPO, PERSIST, SVC, QUAL, SCALE

## Data Schemas

```rust
pub struct WorkflowTypeId(pub String);
pub struct ConfigId(pub String);
pub struct RunId(pub String);

pub struct WorkflowRunRef {
    pub workflow_type_id: WorkflowTypeId,
    pub config_id: ConfigId,
    pub run_id: RunId,
}
```

## Example Data

```toml
# config/workflows/issue-fix-v1.toml
id = "issue-fix-v1"

[[steps]]
id = "scan_issues"
next = "plan_fix"
```

```toml
# config/workflow-configs/profile-0.toml
id = "profile-0"
workflow_type_id = "issue-fix-v1"

[guards]
max_remediation_loops = 3
```

## Constraints

- workflow topology must remain external data
- monitor and engine boundaries must stay strict
- implementation phases cannot ship placeholders (`todo!`, `unimplemented!`, TODO/FIXME comments)
- TDD phases precede implementation phases for each major component

## Performance/Operational Requirements

- monitor heartbeat/state must remain queryable while active
- engine checkpoint/event persistence occurs after each transition
- repository preparation failures halt run initialization before partial execution
