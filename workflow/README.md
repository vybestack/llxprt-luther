# luther-workflow

A locally runnable workflow runtime that executes multi-step software engineering workflows with branching, bounded loops, checkpointing, and resume.

## Quick Start

```bash
# Build
cargo build

# Run the hello-world proof workflow (dry run — shows steps without executing)
cargo run -- run --workflow-type hello-world-v1 --dry-run

# Run tests
cargo test
```

## What It Does

luther-workflow reads a **workflow type** (a graph of steps and transitions) and a **workflow config** (runtime parameters), binds them into a workflow instance, and executes each step through pluggable executors.

### Architecture

```
CLI (run / status / service)
  │
  ▼
EngineRunner
  │
  ├── WorkflowInstance (type + config + run_id)
  ├── ExecutorRegistry (step_type → executor dispatch)
  ├── StepContext (work_dir, run_id, inter-step variables)
  └── Persistence (SQLite checkpoints + events)
```

### Step Executors

| Executor | `step_type` | What It Does |
|----------|-------------|--------------|
| **ShellExecutor** | `"shell"` | Runs a command via `sh -c`, captures stdout/stderr, maps exit codes |
| **WriteFileExecutor** | `"write_file"` | Writes interpolated content to a file path relative to work_dir |
| **NoOpExecutor** | `"noop"` | Always returns Success (for testing) |

### Step Outcomes & Transitions

Each step returns an outcome that drives the next transition:

| Outcome | Meaning | Typical Next Action |
|---------|---------|---------------------|
| `success` | Step completed normally | Advance to next step |
| `fixable` | Non-zero exit / recoverable error | Loop back to a remediation step |
| `fatal` | Unrecoverable error | Terminate the run |
| `retryable` | Transient failure | Retry the same step |
| `abandon` | Guard limit reached | Terminate and log |

Transitions are declared in the workflow type definition, mapping `(from_step, outcome) → next_step`.

### Variable Interpolation

Step parameters support `{key}` interpolation from the execution context:

- `{work_dir}` — the working directory for this run
- `{run_id}` — the unique run identifier
- `{stdout}` — stdout from the most recent shell step
- `{stderr}` — stderr from the most recent shell step
- `{exit_code}` — exit code from the most recent shell step
- Any key set by a previous step via `StepContext::set()`

Undefined keys are left as-is (no error).

### Guardrails

Workflows are bounded by configurable limits:

- **max_retries** — per-step retry cap
- **max_iterations** — loop-back cycle cap (prevents infinite remediation loops)
- **timeout_seconds** — overall run timeout (declared, not yet enforced at runtime)

When a loop limit is reached the run terminates with `Abandoned`.

## Defining Workflows

### Workflow Type (TOML)

A workflow type defines the step graph. Place it in `config/workflows/` or `tests/fixtures/workflows/valid/`:

```toml
workflow_type_id = "my-workflow"

[[steps]]
step_id = "setup"
step_type = "shell"
description = "Set up the workspace"
[steps.parameters]
command = "mkdir -p {work_dir}/output"

[[steps]]
step_id = "generate"
step_type = "write_file"
description = "Generate a config file"
[steps.parameters]
path = "output/config.json"
content = '{"run_id": "{run_id}"}'

[[steps]]
step_id = "validate"
step_type = "shell"
description = "Validate the output"
[steps.parameters]
command = "cat {work_dir}/output/config.json"

[[transitions]]
from = "setup"
to = "generate"
condition = "success"

[[transitions]]
from = "generate"
to = "validate"
condition = "success"

[[transitions]]
from = "validate"
to = "generate"
condition = "fixable"

[guards]
max_retries = 3
timeout_seconds = 120
```

### Workflow Config (TOML)

A config provides runtime parameters for an instance. Place it in `config/workflow-configs/` or `tests/fixtures/workflow-configs/valid/`:

```toml
config_id = "my-config"
workflow_type_id = "my-workflow"

[runtime]
timeout_seconds = 120
max_retries = 3

[repository]
workspace_strategy = "temp_clone"
branch_template = "workflow-{run_id}"
base_branch = "main"

[guards]
max_iterations = 5
max_file_changes = 50
max_tokens = 10000
max_cost = 5.00
```

JSON equivalents are also supported — the loader tries both `.toml` and `.json`.

## CLI Commands

### `run`

Execute a workflow:

```bash
# Run with specific workflow type (resolves from fixture directory)
cargo run -- run --workflow-type hello-world-v1

# Dry run — prints steps without executing
cargo run -- run --workflow-type hello-world-v1 --dry-run

# Specify a config file path
cargo run -- run --config path/to/config.toml --workflow-type my-workflow
```

### `status`

Single-shot aggregate status combining daemon heartbeats and the run registry:

```bash
cargo run -- status
cargo run -- status --json
cargo run -- status --run-id <uuid>
cargo run -- status --config <config-id>
```

For a continuously refreshing view, use `monitor` (below).

### `service` (OS-supervised)

Manage a Luther service under the platform supervisor (launchd/systemd). This is
a subcommand tree, not an arg form:

```bash
cargo run -- service run --foreground   # run supervised process in foreground
cargo run -- service install            # install the OS service unit
cargo run -- service start              # start the installed service
cargo run -- service stop               # stop the supervisor process
cargo run -- service status             # query OS service state
cargo run -- service uninstall          # remove the OS service unit
```

> The old `service --foreground` arg form has been removed; use `service run
> --foreground`.

### `daemon` (foreground)

Run config-scoped daemons and inspect discovery/queue state:

```bash
cargo run -- daemon run --config <config>.toml   # foreground (--once for one pass)
cargo run -- daemon start --config <config>.toml
cargo run -- daemon stop --config <config-id>    # or --all
cargo run -- daemon status                       # aggregate (--config / --json)
cargo run -- daemon discover --json              # dry-run discovery
cargo run -- daemon queue                        # queue + lease state
```

The config id is the file stem of the config file (e.g. `daemon-config-a.toml`
=> `daemon-config-a`).

### `runs`

Inspect the persistent run registry:

```bash
cargo run -- runs list
cargo run -- runs show <run-id>
cargo run -- runs tail <run-id>
cargo run -- runs ps
```

### `monitor`

Continuous, read-only live status. Continuous by default; `--times N` bounds the
number of snapshots (`--once` == `--times 1`); Ctrl-C exits cleanly:

```bash
cargo run -- monitor                  # refresh forever (every 2s)
cargo run -- monitor --interval 5     # change refresh delay
cargo run -- monitor --times 3        # exactly 3 snapshots, then exit
cargo run -- monitor --once           # single snapshot
cargo run -- monitor --config <id> --run <run-id> --issue <n>
```

See [`docs/guides/daemon-mode.md`](docs/guides/daemon-mode.md) for the full
operator guide: multi-config usage, discovery/precedence, run visibility, JSON
schemas, shutdown semantics, state directories, and known limitations.

## Supervision & Monitoring

The runtime includes a monitor layer for long-lived operation:

- **Heartbeat files** — periodic state written to disk (`MonitorState`: Starting, Running, Degraded, Stopping, Stopped, Error)
- **Singleton locking** — prevents duplicate monitor instances
- **IPC** — Unix domain socket for status queries
- **Restart policy** — exponential or fixed backoff on crash loops
- **Degraded state** — enters degraded mode after max restart attempts

Service definitions can be generated for:
- **macOS**: launchd plist (`~/Library/LaunchAgents/`)
- **Linux**: systemd user unit (`~/.config/systemd/user/`)

## Persistence

Every step execution persists:
- **Checkpoints** — current step, retry/loop counters, context snapshot (SQLite)
- **Events** — step completion records with timestamps (SQLite)
- **Run metadata** — run_id, workflow_type_id, config_id, status, timestamps
- **Artifacts** — file-based storage keyed by run_id

Interrupted runs can be resumed from their last checkpoint.

## Project Layout

```
src/
  main.rs              CLI entry point
  lib.rs               Module exports
  cli/                 Clap-derived CLI
  engine/
    executor.rs        StepExecutor trait, ExecutorRegistry, StepContext, interpolation
    executors/         Concrete executors (shell, write_file, noop)
    runner.rs          EngineRunner — the execution loop
    instance.rs        WorkflowInstance (type + config binding)
    transition.rs      StepOutcome enum, transition resolution
  workflow/
    schema.rs          WorkflowType, WorkflowConfig, StepDef, TransitionDef
    config_loader.rs   TOML/JSON loading, validation, resolution
  persistence/         SQLite checkpoints, events, run metadata, artifacts
  monitor/             Heartbeat, IPC, singleton lock, process management
  service/             Service lifecycle, launchd/systemd generation
  repo/                Workspace and branch management
  adapters/            External system integrations (git)
  runtime_paths.rs     Cross-platform directory resolution

config/                Default workflow types and configs
tests/
  fixtures/            Test workflow definitions and configs
  *_integration.rs     Integration test suites
```

## Current Limitations

These are known gaps — not bugs, just work not yet done:

1. **No async step execution.** All executors are synchronous. Long-running shell commands block the engine thread.

2. **No parallel steps.** Steps execute sequentially. The `parallel_steps` config field is declared but not implemented.

3. **No timeout enforcement.** `timeout_seconds` is declared in config but not enforced at runtime. A hung shell command will block forever.

4. **Custom state-machine runtime by design.** `EngineRunner` is the single supported engine — a durable, resumable, outcome-routed state machine. A generic DAG library (such as `dagrs`) is intentionally not used because its static parallel task-graph model does not fit Luther's dynamic, resumable, transition-driven execution.

5. **Limited step types.** Only `shell`, `write_file`, and `noop` exist. There are no executors for HTTP, LLM, git operations, or GitHub API calls.

6. **No conditional logic in parameters.** Interpolation is simple `{key}` replacement — no expressions, conditionals, or loops in templates.

7. **stdout/stderr are overwritten each step.** Context variables `{stdout}` and `{stderr}` reflect only the most recent shell step. There's no `{step_id.stdout}` namespacing.

8. **Workspace cleanup is not automatic and `status` is single-shot.** Daemon mode, issue discovery/queueing, the persistent run registry, run inspection (`runs`), and the continuous `monitor` are implemented (see [`docs/guides/daemon-mode.md`](docs/guides/daemon-mode.md)). Remaining gaps: `cleanup_on_success`/`cleanup_on_failure` are declared but not yet implemented (a deletion guard exists so `.llxprt` and user state can never be removed), `timeout_seconds` is not enforced, and `status` is single-shot (use `monitor` for a continuous view).

9. **No real git/repo operations.** The repo module has workspace and branch management types but doesn't execute real git commands. The git adapter is minimal.

10. **CLI run command still hardcodes fixture paths.** The `run` command resolves workflow types from `tests/fixtures/` rather than from a configurable search path or the runtime config directory.

## License

MIT
