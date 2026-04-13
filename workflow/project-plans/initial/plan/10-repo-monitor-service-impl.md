# Phase 10: Repository Workspace/Branching + Monitor/Service Implementation

## Phase ID
`PLAN-20260404-INITIAL-RUNTIME.P10`

## Prerequisites

- Required: Phase 09 verification completed
- Verification marker required: `project-plans/initial/plan/.completed/P09A.md`
- Preflight verification marker required: `project-plans/initial/plan/.completed/P00A.md`

## Requirements Implemented (Expanded)

### REQ-EARS-REPO-001
**Full Text**: The workflow config shall define repository checkout source, workspace root, and branch policy.
**Behavior**:
- GIVEN: a workflow config TOML has `[repository]` with `source`, `workspace.root`, and `branch.base` fields
- WHEN: deserializing the config
- THEN: `config.repository.source` is a valid URL or path, `config.repository.workspace.root` is a directory path, and `config.repository.branch.base` is a branch name string
**Why This Matters**: Makes repository setup reproducible and safe before any workflow step executes.

### REQ-EARS-REPO-002
**Full Text**: When a run initializes repository context, the engine shall resolve or create the configured working directory according to workspace strategy.
**Behavior**:
- GIVEN: a workflow config has `workspace.strategy = "shared"` and `workspace.root = "/tmp/luther-work"`
- WHEN: the engine initializes repository context for a new run
- THEN: the resolved working directory is exactly `/tmp/luther-work` (no run-specific subdirectory appended)
**Why This Matters**: Makes repository setup reproducible and safe before any workflow step executes.

### REQ-EARS-REPO-003
**Full Text**: Where `workspace.strategy = shared`, the engine shall reuse a single configured checkout path for successive runs.
**Behavior**:
- GIVEN: `workspace.strategy = "shared"` and `workspace.root = "/tmp/luther-work"`
- WHEN: two successive runs initialize repository context
- THEN: both runs resolve to the identical working directory path `/tmp/luther-work`
**Why This Matters**: Makes repository setup reproducible and safe before any workflow step executes.

### REQ-EARS-REPO-004
**Full Text**: Where `workspace.strategy = per-run`, the engine shall create an isolated working path derived from configured path template and run metadata.
**Behavior**:
- GIVEN: `workspace.strategy = "per-run"` and `workspace.path_template = "/tmp/luther-work/{run_id}"`
- WHEN: a run with `run_id = "abc-123"` initializes repository context
- THEN: the working directory is created at `/tmp/luther-work/abc-123`, and it is distinct from any other run's directory
**Why This Matters**: Makes repository setup reproducible and safe before any workflow step executes.

### REQ-EARS-REPO-005
**Full Text**: When preparing a run branch, the engine shall checkout configured base branch and create/switch to a branch derived from `branch.name_template`.
**Behavior**:
- GIVEN: a git repo exists at the working directory with `branch.base = "main"` and `branch.name_template = "luther/{run_id}"`
- WHEN: the engine prepares the run branch for `run_id = "abc-123"`
- THEN: the repo checkout is on branch `luther/abc-123`, which was branched from `main`
**Why This Matters**: Makes repository setup reproducible and safe before any workflow step executes.

### REQ-EARS-REPO-006
**Full Text**: Where `branch.create_if_missing = true`, the engine shall create the branch when it does not exist.
**Behavior**:
- GIVEN: `branch.create_if_missing = true` and branch `luther/abc-123` does not exist yet
- WHEN: the engine prepares the run branch
- THEN: the branch `luther/abc-123` is created (verified via `git branch --list luther/abc-123` returning a match)
**Why This Matters**: Makes repository setup reproducible and safe before any workflow step executes.

### REQ-EARS-REPO-007
**Full Text**: Where `branch.force_reset = true`, the engine shall hard-reset run branch to configured base branch before workflow execution begins.
**Behavior**:
- GIVEN: `branch.force_reset = true` and branch `luther/abc-123` exists with commits ahead of `main`
- WHEN: the engine prepares the run branch
- THEN: the branch is hard-reset to `main` HEAD (the extra commits are discarded; `git log --oneline luther/abc-123..main` shows no difference)
**Why This Matters**: Makes repository setup reproducible and safe before any workflow step executes.

### REQ-EARS-REPO-008
**Full Text**: If repository checkout, fetch, or branch preparation fails, then the engine shall fail run initialization with structured diagnostics and no partial workflow execution.
**Behavior**:
- GIVEN: an invalid repository source path is configured (e.g., `/nonexistent/repo`)
- WHEN: the engine attempts to initialize repository context
- THEN: it returns `Err(RepoPrepError)` containing the failed operation ("checkout"/"fetch"/"branch"), the underlying OS/git error message, and no workflow steps have executed
**Why This Matters**: Makes repository setup reproducible and safe before any workflow step executes.

### REQ-EARS-REPO-009
**Full Text**: Where `branch.push_remote = true`, the workflow actions shall push run branches to configured remote as part of push/submit stages.
**Behavior**:
- GIVEN: `branch.push_remote = true` and a valid remote is configured
- WHEN: a push/submit workflow step executes after commits are made
- THEN: the run branch is pushed to the configured remote (verifiable by `git ls-remote` showing the branch ref)
**Why This Matters**: Makes repository setup reproducible and safe before any workflow step executes.

### REQ-EARS-MON-001
**Full Text**: When the monitor starts, it shall acquire singleton ownership for its configured scope before launching an engine instance.
**Behavior**:
- GIVEN: no other monitor instance is running for this scope
- WHEN: the monitor starts and attempts to acquire the singleton lock
- THEN: it acquires the lock, and a concurrent second monitor start for the same scope returns an `AlreadyRunning` error that includes the existing PID
**Why This Matters**: Ensures operational stability and controlled runtime lifecycle management.

### REQ-EARS-MON-002
**Full Text**: While the monitor is running, it shall maintain heartbeat/status metadata for CLI and service observability.
**Behavior**:
- GIVEN: the monitor is running and has launched an engine
- WHEN: reading the heartbeat/status metadata file or querying the IPC status endpoint
- THEN: the response includes: monitor PID, engine state (`running`/`stopped`), last heartbeat timestamp (within 30s of current time), and current run identifiers
**Why This Matters**: Ensures operational stability and controlled runtime lifecycle management.

### REQ-EARS-MON-003
**Full Text**: When the engine process exits unexpectedly, the monitor shall apply configured restart/backoff policy.
**Behavior**:
- GIVEN: the monitor is running with restart policy `max_restarts = 3, backoff_seconds = 1`
- WHEN: the engine process exits with a non-zero exit code
- THEN: the monitor waits `backoff_seconds`, spawns a new engine instance, and the restart counter increments by 1
**Why This Matters**: Ensures operational stability and controlled runtime lifecycle management.

### REQ-EARS-MON-004
**Full Text**: If restart attempts exceed configured safety limits, then the monitor shall transition to degraded/unhealthy state and stop unbounded restart loops.
**Behavior**:
- GIVEN: the monitor has already restarted the engine `max_restarts` (3) times
- WHEN: the engine exits unexpectedly again (4th failure)
- THEN: the monitor transitions to `degraded` state, does NOT restart the engine, and logs a structured health record indicating the restart limit was exceeded
**Why This Matters**: Ensures operational stability and controlled runtime lifecycle management.

### REQ-EARS-MON-005
**Full Text**: When a shutdown command is received, the monitor shall request graceful engine stop and persist final monitor state.
**Behavior**:
- GIVEN: the monitor is running with an active engine instance
- WHEN: a shutdown signal is received (SIGTERM or IPC stop command)
- THEN: the monitor sends a stop signal to the engine, waits for engine exit (up to configured timeout), persists final monitor state to disk, and exits with code 0
**Why This Matters**: Ensures operational stability and controlled runtime lifecycle management.

### REQ-EARS-SVC-001
**Full Text**: The runtime service mode shall run as a foreground process supervised by launchd/systemd rather than self-daemonizing.
**Behavior**:
- GIVEN: the runtime is started in service mode
- WHEN: examining the process attributes
- THEN: the luther process runs in the foreground (not self-daemonized), suitable for supervision by launchd (macOS) or systemd (Linux)
**Why This Matters**: Makes service operation observable and diagnosable in real deployments.

### REQ-EARS-SVC-002
**Full Text**: When service install is requested, the service layer shall generate and install platform-specific service definitions from current configuration.
**Behavior**:
- GIVEN: a `service install` command is executed on macOS
- WHEN: the service layer generates the platform definition
- THEN: a valid launchd plist is written to the appropriate `LaunchAgents` directory with correct binary path, arguments, working directory, and `KeepAlive = true`
**Why This Matters**: Makes service operation observable and diagnosable in real deployments.

### REQ-EARS-SVC-003
**Full Text**: While monitor is active, the control plane shall expose local status and control operations through IPC.
**Behavior**:
- GIVEN: the monitor is running and an IPC socket/pipe endpoint exists
- WHEN: a CLI `status` command connects to the IPC endpoint
- THEN: it receives a structured response containing: monitor state, engine state, current run identifiers, and last heartbeat timestamp
**Why This Matters**: Makes service operation observable and diagnosable in real deployments.

### REQ-EARS-SVC-004
**Full Text**: If service operations fail (install/start/stop/status), then the service layer shall return explicit platform-specific diagnostic details.
**Behavior**:
- GIVEN: a service install is attempted but permissions or target directory are invalid
- WHEN: the install operation fails
- THEN: the error includes: platform name (macOS/Linux), failed operation (install/start/stop/status), the OS-level error message, and a suggested remediation action
**Why This Matters**: Makes service operation observable and diagnosable in real deployments.

### REQ-EARS-SCALE-001
**Full Text**: While MVP single-instance mode is enabled, the monitor shall run exactly one active workflow instance loop.
**Behavior**:
- GIVEN: MVP single-instance mode is active (the default)
- WHEN: the monitor starts and enters its run loop
- THEN: exactly one engine instance loop runs at a time; the monitor never spawns concurrent engine processes
**Why This Matters**: Keeps MVP single-instance while preserving future multi-profile extensibility.

### REQ-EARS-SCALE-003
**Full Text**: Where multiple workflow instance profiles are configured, the monitor shall be able to select a configured instance by ID without changing workflow type code.
**Behavior**:
- GIVEN: multiple workflow config profiles exist on disk (e.g., `profile-0.toml`, `profile-1.toml`)
- WHEN: the monitor is started with `--config-id profile-1`
- THEN: it loads and executes using `profile-1` configuration, without requiring changes to the workflow type definition or Rust source code
**Why This Matters**: Keeps MVP single-instance while preserving future multi-profile extensibility.

## Implementation Tasks

> **This phase covers 4 subsystems. Implement in this order to minimize coupling risk.**

### Step 10A: Runtime Paths and Repository Preparation
Files to create: `src/runtime_paths.rs`, `src/adapters/git.rs`
- Implement `runtime_paths.rs` first (pure path resolution, no I/O dependencies).
- Then implement `src/adapters/git.rs` (workspace strategy, branch prep, push).
- **Gate**: `cargo test --test repo_prep_integration` compiles (tests may still fail for monitor-dependent cases).

### Step 10B: Monitor Process Management
Files to create: `src/monitor/process.rs`, `src/monitor/heartbeat.rs`, `src/monitor/ipc.rs`
- Implement singleton lock, heartbeat file, and IPC socket/pipe.
- **Gate**: `cargo test --test monitor_singleton_and_restart_integration` passes; `cargo test --test monitor_service_integration` passes.

### Step 10C: Service Layer
Files to create: `src/service/spec.rs`, `src/service/launchd.rs`, `src/service/systemd.rs`
- Generate platform-specific service definitions.
- **Gate**: `cargo test --test service_ipc_contract_integration` passes.

### Step 10D: Wiring
Files to modify: `src/main.rs`, `src/lib.rs`, `Cargo.toml`
- Wire monitor → engine → adapters call chain.
- Add `tokio` and `directories` to `Cargo.toml`.
- **Gate**: `cargo test` (all tests pass, including all P09 TDD tests).

## Required dependency additions in this phase

- `tokio`
- `directories`

### Required Code Markers

Every function/struct/test created in this phase must include markers:

```rust
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P10
/// @requirement:REQ-EARS-...
```

- Pseudocode reference: `analysis/pseudocode/monitor-loop.md` lines 1-11 and `analysis/pseudocode/repository-prep.md` lines 1-12
## Verification Commands

### Automated Checks

```bash
# Plan markers
grep -r "@plan:PLAN-20260404-INITIAL-RUNTIME.P10" src tests project-plans || true

# Dependencies added
grep -q 'tokio' Cargo.toml || echo "FAIL: tokio not in Cargo.toml"
grep -q 'directories' Cargo.toml || echo "FAIL: directories not in Cargo.toml"

# Implementation files exist
for f in src/adapters/git.rs src/runtime_paths.rs src/monitor/process.rs src/monitor/heartbeat.rs src/monitor/ipc.rs src/service/spec.rs src/service/launchd.rs src/service/systemd.rs; do
  test -f $f && echo "OK: $f" || echo "FAIL: $f missing"
done

# ALL P09 TDD tests must now pass
cargo test --test repo_prep_integration
cargo test --test monitor_service_integration
cargo test --test monitor_singleton_and_restart_integration
cargo test --test service_ipc_contract_integration

# No stubs in implementation
grep -rn "todo!\|unimplemented!" src/adapters/ src/monitor/ src/service/ src/runtime_paths.rs && echo "FAIL: stubs remain" || echo "OK: no stubs"

cargo build --all-targets
cargo test
```


### Deferred Implementation Detection (MANDATORY)

```bash
grep -rn "todo!\|unimplemented!" src tests
# Expected: no matches in implementation targets

grep -rn "// TODO\|// FIXME\|// HACK" src tests
# Expected: no matches in implementation targets
```
### Structural Verification Checklist

- [ ] Prerequisite marker exists (`P09A`)
- [ ] All listed files created/modified
- [ ] Plan + requirement markers present
- [ ] `cargo build` passes
- [ ] `cargo test` status matches phase expectation

### Semantic Verification Checklist

- [ ] Behavioral outcomes match the requirement text
- [ ] Tests would fail if implementation were removed
- [ ] Feature is reachable through planned call paths
- [ ] All preceding TDD tests pass without weakening tests or adding placeholders.

## Success Criteria

- Required artifacts and code changes are complete.
- Verification checklist is fully satisfied.
- Evidence is written to completion marker file.

## Failure Recovery

1. Revert files changed only by this phase as needed.
2. Fix issues discovered by verification.
3. Re-run this phase; do not proceed until PASS.

## Phase Completion Marker

Create: `project-plans/initial/plan/.completed/P10.md`

```markdown
Phase: P10
Verdict: PASS|FAIL
Evidence:
- commands run
- test/build outputs
- file list changed
```
