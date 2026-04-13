# Domain Model

## Core Entities

| Entity | Description | Key Fields |
|---|---|---|
| `WorkflowType` | Declarative topology and transitions | `workflow_type_id`, `steps`, `transitions`, `guards` |
| `WorkflowConfig` | Runtime profile for one instance | `config_id`, `workflow_type_id`, `runtime`, `repo`, `guard_limits` |
| `WorkflowRunRef` | Bound runtime identity | `workflow_type_id`, `config_id`, `run_id` |
| `MonitorState` | Supervisor lifecycle and health view | `lock_scope`, `heartbeat`, `engine_status`, `restart_counter` |
| `EngineState` | Current execution position and status | `step_id`, `attempts`, `loop_counts`, `checkpoint_ref` |
| `Checkpoint` | Durable resumable point | `run_id`, `state_snapshot`, `last_event_id`, `timestamp` |
| `ArtifactRecord` | Per-run output metadata | `run_id`, `artifact_path`, `artifact_kind`, `step_id` |
| `RepoWorkspacePolicy` | Working copy strategy and branch policy | `strategy`, `workspace_root`, `branch_template`, flags |

## Boundary Rules

1. Monitor lifecycle is independent from workflow semantics.
2. Engine executes generic step routing and guardrails; no issue/PR domain policy inside runner core.
3. Workflow type/config remain external declarative files.
4. Persistence is required for run metadata, events, checkpoints, and artifacts.

## Required Binding Contract

```text
(workflow_type_id, config_id, run_id)
```

This tuple is created at run start, persisted, and used across checkpoint/resume paths.
