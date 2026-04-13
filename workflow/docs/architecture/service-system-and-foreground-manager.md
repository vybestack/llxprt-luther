# Service System and Foreground Manager Design

## Purpose

This document defines the service-management architecture for the Luther workflow system.

The primary goals are:

- run reliably as a long-lived local service
- work cleanly on macOS and Linux first
- preserve a straightforward path to Windows support later
- avoid classic self-daemonization traps
- keep service management separate from workflow logic
- support future evolution into a larger multi-process and self-improving system

---

## Core decision

## The daemon runs in the foreground

Luther should **not** daemonize itself with fork/detach/PID-file patterns as its primary runtime model.

Instead:

- the runtime process stays a normal foreground process
- OS-native service managers supervise it when installed as a service
- local development also runs the same foreground process directly

This means:

- **launchd** supervises it on macOS
- **systemd** supervises it on Linux
- a Windows service adapter can be added later without changing the core runtime model

This is the most compatible choice for:
- observability
- local debugging
- restart semantics
- future cross-platform support
- later multi-process evolution

---

## High-level architecture

There are three relevant process roles.

## 1. CLI control plane

This is the user-facing command surface.

Example commands:

- `luther run-once`
- `luther supervisor`
- `luther status`
- `luther service install`
- `luther service uninstall`
- `luther service start`
- `luther service stop`
- `luther service restart`
- `luther service status`
- `luther replay RUN_ID`
- `luther eval ...`

The CLI should not itself embed platform service logic everywhere. It should call a service management layer.

---

## 2. Foreground supervisor

This is the always-on runtime entrypoint for long-running operation.

Responsibilities:

- manage the runner lifecycle
- keep the system alive
- emit heartbeats
- track health and last progress
- own local singleton locking
- perform graceful shutdown coordination
- restart the runner on crash according to policy
- later manage multiple worker/runner processes if needed

This is the process that launchd/systemd should supervise.

The supervisor should **not** contain workflow-specific logic.

---

## 3. Workflow runner

This is the execution engine for the Luther workflow.

Responsibilities:

- load workflow definition
- execute workflow steps
- persist state
- write artifacts
- emit structured events/results
- stop cleanly on request

The runner should be restartable independently of the supervisor.

This separation is important because the long-term system may later include:

- primary issue-processing runners
- replay/eval runners
- workflow-variant benchmark runners
- mutation proposal workers

Keeping the runner concept separate now avoids coupling the whole future system into one big daemon process.

---

## Preferred process model

## Phase 1 preferred runtime

Externally, one binary may be sufficient, but internally the code should model two roles:

- supervisor role
- runner role

There are two reasonable implementation shapes:

### Option A: one binary, two modes

Example:
- `luther supervisor`
- `luther runner`

This is the recommended starting point.

Benefits:
- simplest packaging
- easiest Homebrew distribution
- no duplicated dependency graph
- still preserves architectural separation

### Option B: two binaries later

Example:
- `luther`
- `lutherd`

This can be adopted later if packaging or operational clarity requires it.

The internal architecture should make this split easy, but it does not need to exist immediately.

---

## Foreground supervisor behavior

## Startup

On startup the supervisor should:

1. resolve config, state, log, and IPC paths
2. acquire singleton lock for the current installation/scope
3. initialize heartbeat store
4. initialize IPC control plane
5. start the runner process or runner task
6. monitor runner lifecycle
7. report readiness

## Main loop

The supervisor loop should:

- maintain current health state
- observe runner exit/crash/success
- restart on crash when policy allows
- update heartbeat timestamps
- answer status requests
- react to stop/reload commands

## Shutdown

On shutdown it should:

1. stop accepting new control actions
2. tell the runner to shut down cleanly
3. wait for bounded graceful shutdown
4. force terminate only if required
5. flush state and heartbeat metadata
6. release singleton lock

---

## Runner supervision policy

For the MVP, use a simple restart policy.

### Suggested policy

- restart runner on unexpected crash
- do not restart immediately in a tight loop forever
- use bounded exponential backoff for repeated crash loops
- record crash counts and timestamps
- surface degraded status if restart threshold is exceeded

### Suggested initial thresholds

These exact numbers can remain configuration, but conceptually:

- restart backoff begins immediately after first crash
- restart ceiling after several rapid failures transitions to unhealthy state
- service manager still supervises the supervisor process itself

This yields two layers of resilience:

- internal supervisor handles runner-level instability
- launchd/systemd handles supervisor-level failure

---

## Singleton and scope model

The system should prevent accidental duplicate supervisors for the same logical installation.

## Singleton scope

Use a lock scoped to:

- user-level config/state root
- service name
- environment/profile if applicable

For example, one user should not accidentally run:
- a launchd-managed supervisor
- and a shell-invoked supervisor
simultaneously against the same state/artifact directories.

## Lock behavior

The lock should:
- be created by the supervisor
- be removed on clean exit
- tolerate stale lock recovery on crash
- not be the primary monitoring mechanism

A lock is for mutual exclusion, not service health.

---

## Health and heartbeat model

The supervisor should continuously publish status.

## Heartbeat contents

At minimum:

- supervisor PID
- start time
- current overall status: starting / healthy / degraded / stopping / unhealthy
- last heartbeat timestamp
- runner status: starting / idle / active / crashed / stopping
- current run ID if active
- current workflow state if known
- last successful transition timestamp
- restart count in current window
- workflow version ID

## Storage of heartbeat

For the MVP, heartbeat can live in SQLite or another structured local store.

That is preferable to plain PID files because:
- richer status
- queryable by CLI
- extensible for later metrics/evolution tooling

---

## IPC and local control plane

Do not build control operations around blind PID signaling.

Instead, the supervisor should expose a local control plane.

## Recommended transport

### macOS and Linux
- Unix domain socket

### Windows later
- named pipe

## Initial commands

The IPC protocol should support at least:

- `ping`
- `status`
- `shutdown`
- `restart-runner`
- `reload-config` later

This lets the CLI distinguish between:

- service-manager operations
- application runtime operations

### Important distinction

- `luther service stop` means stop the OS-managed service
- `luther status` may query the running supervisor over IPC

That distinction becomes valuable later when the system grows.

---

## Service system abstraction

The service layer should be explicitly separated from the daemon runtime.

## Shared service spec

Define a platform-neutral `ServiceSpec` concept that includes:

- service name
- display name
- description
- exec path
- args
- working directory
- environment variables
- autostart setting
- restart policy
- user vs system scope
- stdout/stderr/log strategy

This lets platform-specific adapters render the right OS-native definition.

## Service manager trait

The service system should expose operations conceptually like:

- install
- uninstall
- start
- stop
- restart
- status

The CLI should talk to this abstraction, not directly to launchctl or systemctl everywhere.

---

## macOS design

## First-class target: launchd user agents

For macOS, start with **user-level launch agents**, not system daemons.

Why:
- avoids root requirement
- matches developer/workstation use well
- easier Homebrew story
- closer to the expected UX for a local automation daemon

## File location

Generate:
- `~/Library/LaunchAgents/<label>.plist`

## Recommended launchd properties

Include:

- `Label`
- `ProgramArguments`
- `RunAtLoad`
- `KeepAlive`
- `WorkingDirectory` when useful
- `EnvironmentVariables`
- `StandardOutPath`
- `StandardErrorPath`

## launchd philosophy

launchd should:
- own service startup
- own service keepalive semantics
- relaunch supervisor if it exits unexpectedly

The supervisor itself should remain a foreground process.

---

## Linux design

## First-class target: systemd user units

For Linux, start with **systemd user services**, not system-wide root services.

Why:
- avoids root requirement
- aligns with local CLI tool behavior
- mirrors launchd user-agent model
- easier installation and removal

## File location

Generate:
- `~/.config/systemd/user/luther.service`

## Recommended systemd properties

Include:

### [Unit]
- `Description=`
- `After=network-online.target` if needed

### [Service]
- `Type=simple`
- `ExecStart=`
- `WorkingDirectory=`
- `Environment=`
- `Restart=on-failure`
- `RestartSec=`

### [Install]
- `WantedBy=default.target`

Again, systemd supervises the foreground supervisor process.

---

## Windows path later

The architecture should leave room for a Windows adapter later.

The easiest future route is:
- keep supervisor foreground-oriented
- add a Windows Service adapter for install/start/stop/status
- keep IPC abstraction separate from service management

Because the runtime remains a normal foreground service process, later Windows support becomes an additive adapter rather than a redesign.

---

## Logging design

## Primary logging rule

The supervisor and runner should log to stdout/stderr in foreground mode.

This allows:
- local shell execution without special setup
- launchd/systemd capture when service-managed
- easier debugging

## Secondary logging rule

Optional rolling file logs can be added, but should not replace stdout/stderr as the primary service-mode logging path.

## Platform-specific notes

- macOS launchd can route stdout/stderr to configured files
- Linux systemd naturally captures stdout/stderr into journald

This is one more reason not to self-daemonize.

---

## Config, state, and artifact paths

Use platform-appropriate user directories.

## Config

- macOS: `~/Library/Application Support/<app>/`
- Linux: `${XDG_CONFIG_HOME:-~/.config}/<app>/`

## State

- macOS: `~/Library/Application Support/<app>/`
- Linux: `${XDG_STATE_HOME:-~/.local/state}/<app>/`

## Runtime IPC and locks

Use the state/runtime area for:
- lock file
- IPC socket
- heartbeat records
- supervisor metadata

## Artifacts

Artifacts for workflow runs should live under a structured state/data root, not mixed into temporary directories with service files.

---

## Service installation strategy

Service files should be generated from actual installation metadata, not treated as static hardcoded files.

Why:
- binary path may vary across Homebrew/manual/dev installs
- config path may vary by platform and scope
- user/system scope affects location and commands

A generation-based installation flow is the safest default.

---

## What the service system should not do

The service system should **not**:

- know workflow semantics
- know issue/PR logic
- read workflow files directly
- perform runner state transitions
- own run persistence
- expose application internals beyond status/control commands

It is infrastructure, not workflow logic.

---

## Relationship to the self-evolving future

This design supports future growth because it cleanly separates:

- deployment/service management
- long-lived supervision
- workflow execution
- later replay/eval/mutation workers

As the system evolves, this allows adding:

- dedicated eval workers
- background replay jobs
- mutation-testing workers
- variant runners
- benchmark orchestration

without overloading a single daemon implementation with every concern.

The core architectural rule remains:

> service management, supervision, workflow execution, and evaluation should remain distinct layers.

---

## Recommended initial command model

## Foreground runtime
- `luther supervisor`
- `luther runner`
- `luther run-once`

## Runtime control
- `luther status`
- `luther shutdown`
- `luther restart-runner`

## Service control
- `luther service install --user`
- `luther service uninstall --user`
- `luther service start --user`
- `luther service stop --user`
- `luther service restart --user`
- `luther service status --user`

The `--user` concept should be the default for macOS and Linux initially.

---

## Recommended project modules

Suggested internal layout:

- `src/daemon/`
  - supervisor runtime
  - signal handling
  - heartbeat management
  - runner lifecycle management
  - IPC server

- `src/service/`
  - service abstraction
  - launchd adapter
  - systemd adapter
  - later windows adapter
  - service spec rendering

- `src/runner/`
  - workflow execution entrypoint
  - state persistence integration
  - artifact handling hooks

- `src/cli/`
  - user-facing command handlers

This keeps service concerns out of runner code.

---

## MVP service-system summary

For the MVP:

- build a **foreground supervisor**
- let **launchd/systemd** supervise it
- implement **service install/start/stop/status** via a dedicated service layer
- use **user-level launchd/systemd units** first
- use **IPC** for runtime status/control
- avoid self-daemonization entirely

That gives the project:
- a reliable service model now
- a clean local dev story
- a clean release story
- a clean path to future Windows support
- a clean path to future multi-runner and self-evolving infrastructure
