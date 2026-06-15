# Daemon Mode, Run Status, and the Monitor — Operator Guide

This guide explains how to operate Luther in daemon mode: how to start one or
more config-scoped daemons, discover and queue issues, inspect runs, watch live
status with the monitor, and shut everything down safely. It documents the
**actual** command surface exposed by the `luther-workflow` binary, the on-disk
state layout, the JSON schemas you can script against, signal/shutdown
semantics, and the known limitations of the current implementation.

> See also: `docs/architecture/service-system-and-foreground-manager.md` for the
> internal architecture of the process roles described below.

---

## 1. Process roles

Luther separates three responsibilities. **No process self-daemonizes**; long
running processes run in the foreground (or under an OS supervisor such as
launchd/systemd that you install explicitly).

1. **CLI control plane** — short-lived `luther-workflow` invocations that read
   state and print results (`status`, `runs`, `daemon status`, `daemon
   discover`, `daemon queue`, `monitor`). These commands are read-only with
   respect to your repositories and never delete workspace state.
2. **Foreground supervisor / daemon** — a long-running process started by
   `daemon run` (foreground) or supervised by the OS after `service install`.
   It owns issue discovery, the work queue, and scheduling for a single config.
3. **Workflow runner** — the executor pipeline that actually drives an issue fix
   to completion (clone/workspace, agent steps, PR creation/remediation).

---

## 2. Command reference

The top-level command tree is:

```text
luther-workflow run        # run a single workflow once
luther-workflow status     # single-shot aggregate status (human / --json)
luther-workflow service    # OS-supervised service lifecycle (install/start/...)
luther-workflow daemon     # foreground daemon lifecycle + discovery/queue
luther-workflow runs       # inspect persistent run registry
luther-workflow monitor    # continuous (or bounded) live status view
```

### 2.1 `run`

Runs a single workflow to completion in the foreground. Use this for one-off
fixes or for testing a workflow config without starting a daemon.

### 2.2 `service <subcommand>` (OS-supervised)

Manages a Luther service under the platform supervisor (launchd on macOS,
systemd on Linux):

```text
service run        # run the supervised process in the foreground
service install    # install the OS service unit
service start      # start the installed service
service stop       # stop the installed service (stops the supervisor process)
service status     # query the OS service state
service uninstall  # remove the OS service unit
```

> **Obsolete form:** earlier versions accepted `service --foreground`. That arg
> form no longer exists. Use the `service run --foreground` subcommand instead.

`service run` prints `Press Ctrl+C to stop` and stops gracefully on SIGINT; when
run under the OS supervisor, stop/start are driven by `service stop`/`service
start` (or the platform's own controls).

### 2.3 `daemon <subcommand>` (foreground)

```text
daemon start              # start the daemon for a config
daemon run [--once]       # run the daemon in the foreground (--once: one pass)
daemon stop [--config ID] # stop one daemon
daemon stop --all         # stop all daemons
daemon status [--config ID] [--json]   # per-config or aggregate health
daemon discover [--json]  # dry-run issue discovery (no work performed)
daemon queue              # show queue/lease state
```

The **config id is the file stem** of the workflow-config file. For example a
config loaded from `daemon-config-a.toml` has config id `daemon-config-a`.

`daemon run` installs interrupt handlers for both Ctrl-C (SIGINT) and SIGTERM
and shuts down gracefully using a shared shutdown flag and an early-waking sleep,
so it reacts to a signal promptly rather than after the next full interval.

### 2.4 `runs <subcommand>` (persistent run registry)

```text
runs list           # list known runs
runs show <run-id>  # show a single run's details (errors include the run id)
runs tail <run-id>  # tail a run's log
runs ps             # show active run processes
```

### 2.5 `status`

`status` is a **single-shot** aggregate view that combines daemon heartbeats and
the run registry:

```text
status                       # human-readable aggregate
status --json                # machine-readable aggregate
status --config <config-id>  # scope to one daemon config
status --run-id <run-id>     # filter to one run
```

For a continuously refreshing view, use `monitor` (see below).

### 2.6 `monitor`

`monitor` is a **continuous, read-only** live view. By default it refreshes
forever until you press Ctrl-C.

```text
monitor                       # continuous (refresh every 2s by default)
monitor --interval <secs>     # change refresh delay
monitor --times <N>           # render exactly N snapshots, then exit 0
monitor --once                # equivalent to --times 1
monitor --no-clear            # do not clear the screen between snapshots
monitor --tail                # include tailing output
monitor --config <config-id>  # scope to one config
monitor --run <run-id>        # scope to one run
monitor --issue <issue>       # scope to one issue
```

`monitor` never mutates state. Pressing Ctrl-C exits the poll loop cleanly and
prints `Monitor stopped`.

---

## 3. Multi-config example: two daemons, aggregate status

You can run independent daemons for different configs simultaneously. Each is
scoped by its config id (the file stem).

Start two foreground daemons (in separate terminals or under the OS supervisor):

```bash
# Terminal 1
luther-workflow daemon run --config daemon-config-a.toml

# Terminal 2
luther-workflow daemon run --config daemon-config-b.toml
```

View aggregate health across both daemons:

```bash
luther-workflow daemon status            # both configs' heartbeats
luther-workflow daemon status --json     # same, machine-readable
```

View the combined run picture across both configs:

```bash
luther-workflow status                   # aggregate runs + heartbeats
luther-workflow status --json            # machine-readable aggregate
luther-workflow status --config daemon-config-a   # scope to one config
luther-workflow monitor                  # live aggregate view
```

---

## 4. Discovery and config precedence

Configs are resolved in this order (first match wins):

1. **Production layout** under the config root:
   - `<root>/workflows/<id>.toml` then `<root>/workflows/<id>.json`
   - `<root>/workflow-configs/<id>.toml` then `<root>/workflow-configs/<id>.json`
2. **Fixture layout** (test/dev): the same paths under a `valid/` subdirectory.

TOML is always tried before JSON for a given id. Use `--config-dir` to override
the config root.

Discovery is observable without performing any work:

```bash
luther-workflow daemon discover --json   # dry-run: what would be picked up
luther-workflow daemon queue             # current queue + lease states
```

---

## 5. Run visibility

Inspect runs at several levels of detail:

- **`runs list`** — enumerate known runs from the persistent registry.
- **`runs show <run-id>`** — full detail for one run (logs path, events,
  artifacts, workspace path, PR info when available, status). Errors for a
  missing id include the queried run id so they are actionable.
- **`runs tail <run-id>`** — follow a run's log output.
- **`runs ps`** — list active run processes.
- **`status` / `status --json`** — aggregate snapshot with `--run-id` and
  `--config` filters.
- **`monitor`** — live aggregate (continuous by default; bounded with
  `--times N` / `--once`).

`monitor` is strictly read-only; it never starts, stops, or cancels runs.

---

## 6. JSON schemas

### 6.1 Heartbeat (`monitor/heartbeat.rs`)

| Field            | Type     | Description                                   |
|------------------|----------|-----------------------------------------------|
| `instance_id`    | string   | Daemon/config instance identifier             |
| `timestamp`      | RFC3339  | When the heartbeat was written                |
| `uptime_secs`    | integer  | Seconds the instance has been running         |
| `version`        | string   | Binary version                                |
| `state`          | enum     | `Starting`/`Running`/`Degraded`/`Stopping`/`Stopped`/`Error` |
| `active_workers` | integer  | Number of active workers                      |
| `run_id`         | string?  | Current run id, if any                        |
| `metadata`       | object   | Free-form key/value metadata                  |

### 6.2 Monitor snapshot (`monitor/snapshot.rs`)

| Field            | Type                  | Description                       |
|------------------|-----------------------|-----------------------------------|
| `generated_at`   | RFC3339               | Snapshot generation time          |
| `daemons`        | array<DaemonSummary>  | Per-daemon summaries              |
| `counts`         | RunCounts             | Aggregate run counts              |
| `runs`           | array                 | Runs included in the snapshot     |
| `selected`       | object?               | The selected run/config, if any   |
| `recent_events`  | array                 | Most recent events                |

### 6.3 `status --json` payload

The `status --json` payload includes:

- `timestamp` — when the status was generated.
- `heartbeats` — the per-config daemon heartbeats (see 6.1).
- `runs` — runs from the persistent registry (filtered by `--run-id`/`--config`
  when provided).
- `registry_error` — present when the run registry could not be read; the
  message includes the relevant id(s) so the failure is actionable.

---

## 7. Shutdown semantics and signals

- **`daemon run`** — installs handlers for Ctrl-C (SIGINT) and SIGTERM and
  shuts down gracefully via a shared shutdown flag, waking early from its sleep
  interval to respond promptly.
- **`service run`** — runs in the foreground (prints `Press Ctrl+C to stop`) and
  stops gracefully on SIGINT; under the OS supervisor, use `service stop`.
- **`monitor`** — Ctrl-C exits the poll loop cleanly and prints `Monitor
  stopped`. The monitor never mutates state, so interrupting it is always safe.

**Stopping a daemon vs cancelling a run.** `daemon stop` / `service stop` stop
the *supervisor process* (and, with `daemon stop --all`, every daemon). They are
not the same as cancelling an in-flight run: a run is tracked in the registry
and inspected/cancelled through the `runs` family. Stopping a daemon halts
scheduling of new work; it does not delete run history or workspace state.

**Exit codes.** `0` on clean completion, `130` when terminated by SIGINT
(Ctrl-C), and a non-zero error code on failure.

---

## 8. State directories and inspection

Paths are computed by `runtime_paths.rs`:

- **Data root** — `get_data_dir()`. Contains:
  - `daemons/<config_id>/` — per-config daemon state and heartbeats.
  - `artifacts/` — run artifacts (`get_artifacts_root`).
  - `checkpoints.db` — the persistent run registry database.
- **Logs** — `get_log_dir()`: macOS `~/Library/Logs/luther`, Linux
  `<data>/logs`.
- **Workspaces** — a run uses either a `temp_clone` (ephemeral clone) or a
  `shared` workspace; the per-run path is `repo::Workspace::path_for_run`.

To find PR information, inspect the run's artifacts via `runs show <run-id>`,
which surfaces the workspace path, artifacts, and PR references when available.

---

## 9. `.llxprt` and user workspace safety

`.llxprt` directories (and `LLXPRT.md` and generated-notice files) are created
and owned by the `llxprt` agent binary. **Luther never deletes them.**

- The remediation push path excludes `.llxprt` / `.llxprt/…` / `LLXPRT.md` /
  generated-notice files (`push_path_is_excluded`).
- A shared workspace-deletion guard (`is_protected_workspace_path` +
  `tree_contains_protected_workspace_path` + `guarded_remove_dir_all` in
  `repo/mod.rs`) refuses to delete any path with a `.llxprt` component **or any
  directory tree that contains a nested `.llxprt` descendant** (the tree walk
  does not follow symlinks). This is the **single sanctioned destructive
  helper**; any future workspace cleanup must route through it. Regression tests
  (`tests/workspace_protection_tests.rs`) prove a `.llxprt` directory is left
  intact and that deleting a parent directory containing a nested `.llxprt` is
  refused.

---

## 10. Known limitations

These behaviors are documented rather than implemented in the current release:

- **Workspace cleanup is not automatic.** `cleanup_on_success` /
  `cleanup_on_failure` are declared in `RepositoryConfig` but not yet
  implemented. The deletion guard exists so that when cleanup is added it cannot
  remove protected paths.
- **`timeout_seconds` is not enforced** by the runner yet.
- **`status` is single-shot.** Use `monitor` for a continuously refreshing view.
- **Supervisor differences.** launchd (macOS) and systemd (Linux) have
  different service semantics; `service install`/`start`/`stop` abstract most of
  this but platform-specific behavior may differ.
- **`.llxprt` is never deleted** by any Luther command (by design).
