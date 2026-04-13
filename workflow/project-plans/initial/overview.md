# Luther Rust Workflow Setup and Runner Spec (Initial)

## 1) Overview

This document defines the initial functional and technical specification for the Luther Rust workflow system.

The objective is to build a **locally runnable, embeddable workflow runtime** that can execute software-engineering workflows with:

- explicit steps
- branching on tool/CLI outcomes
- bounded loops and remediation cycles
- persistent state and resume
- strict separation between generic engine and domain logic

The first implementation targets:

- local development and supervised service execution
- declarative workflow definitions (separate from implementation code)
- deterministic orchestration with selective agent/tool invocation

This spec is progressive:

1. Overview and goals
2. Functional behavior
3. Diagrams and workflow model
4. Technical architecture
5. Build-time/runtime directory structures
6. Configuration model and storage
7. Dependency plan with upstream last-commit metadata

---

## 2) Functional specification

## 2.1 Product behavior (initial)

The runtime executes a Luther workflow that handles issue-to-outcome automation in bounded loops.

Core capability set:

1. Load workflow type definition from file (TOML initially; JSON supported as an alternate format).
2. Load workflow instance config separately from workflow type.
3. Execute workflow steps through registered action handlers.
4. Route transitions based on structured step outcomes.
5. Support bounded loops (e.g., remediation/retest cycles).
6. Persist run state/checkpoints and resume after interruption.
7. Emit structured logs/events/artifacts.
8. Expose status and control via supervisor + CLI.

## 2.2 First MVP workflow behavior

The first MVP workflow path is:

- `scan_issues`
- `plan_fix`
- `review_plan`
- `implement_fix`
- `run_checks`
- `commit_push`
- `submit_pr`
- `watch_pr_checks`
- conditional loop on failure/review outcomes:
  - `diagnose_ci`
  - `triage_comments`
  - `respond_comments`
  - `remediate`
  - back to `run_checks`
- terminal outcomes:
  - `log_outcome_success`
  - `abandon_and_log`

The workflow is bounded by loop/retry guardrails from config.

## 2.3 Functional constraints

1. **Engine/workflow boundary is strict**: workflow semantics are not embedded in the generic runner core.
2. **Workflow type is data**: topology/transitions are loaded from declarative definitions.
3. **Workflow instance config is separate data**: runtime parameters are externalized and bound at instantiation.
4. **Tool outcomes drive branching**: step outputs are typed, routable events.
5. **Service execution is foreground-supervised**: no self-daemonization model.
6. **Artifacts are first-class**: all runs produce traceable persisted metadata.

---

## 3) Diagrams and workflow model

## 3.1 System architecture diagram

```text
+-----------------------------+      +----------------------------+
|           CLI               |      |     OS Service Manager     |
| luther service/status/run   |      | launchd / systemd          |
+-------------+---------------+      +-------------+--------------+
              |                                      |
              | control/status                        | supervises
              v                                      v
+-----------------------------+      +----------------------------+
|       Monitor/Supervisor    |<---->|     Heartbeat / IPC       |
| foreground long-lived proc  |      | socket + state metadata   |
+-------------+---------------+      +----------------------------+
              |
              | start/restart/stop engine-runner
              v
+-----------------------------+
|        Workflow Engine      |
| (runtime + dispatch core)   |
+-------------+---------------+
              |
              | instantiate workflow type + instance config
              v
+-----------------------------+
|    Workflow Instance        |
| type: issue-fix-v1          |
| config: profile-0/profile-10|
+-------------+---------------+
              |
              | executes actions
              v
+-----------------------------+
|   Step Action Adapters      |
| git | gh | shell | llm      |
+-------------+---------------+
              |
              v
+-----------------------------+
| Persistence + Artifacts     |
| sqlite/checkpoints/logs     |
+-----------------------------+
```

## 3.2 Workflow control-flow diagram (MVP)

```text
scan_issues
   |
   v
plan_fix --> review_plan --revise--> plan_fix
   |             |
   |             +--approved--------+
   |                                v
   +----------------------------> implement_fix
                                      |
                                      v
                                  run_checks
                                      |
                        +-------------+-------------+
                        |                           |
                       pass                        fail
                        |                           |
                        v                           v
                   commit_push                 diagnose_ci
                        |                           |
                        v                           v
                    submit_pr                 triage_comments
                        |                           |
                        v                           v
                 watch_pr_checks <--- respond_comments
                        |                           |
                        +-----------needs_remediation+
                                    |
                                    v
                                 remediate
                                    |
                                    v
                                 run_checks

Terminal edges:
- success -> log_outcome_success
- fatal/abandon -> abandon_and_log
```

## 3.3 Runtime sequence (type + instance)

```text
Monitor -> Engine -> Load workflow type (TOML/JSON)
                 -> Load instance config (TOML/JSON)
                 -> Bind type + config => workflow instance
                 -> Execute next step
                 -> Persist checkpoint + event
                 -> Route transition
                 -> Repeat until terminal state
Monitor <- health/events -------- Engine
CLI <- status via IPC ----------- Monitor
```

---

## 4) Technical specification

## 4.1 Core libraries and roles

Planned Rust libraries for MVP implementation:

- `dagrs`: workflow graph runtime substrate (branches/loops/router/checkpointing primitives)
- `tokio`: async runtime for engine/adapters
- `serde`, `serde_json`, `toml`: config/workflow parsing and serialization
- `clap`: CLI command surface
- `tracing`, `tracing-subscriber`: structured logs/events
- `thiserror`, `anyhow`: error taxonomy and operational error handling
- `rusqlite`: local persistence/checkpoints/heartbeat metadata
- `directories`: platform-correct config/state paths
- `uuid`: run IDs / entity IDs

## 4.2 Layered architecture

### A) Monitor layer

Responsibilities:

- long-lived supervision loop
- engine lifecycle management
- restart/backoff policy
- heartbeat + IPC status/control

### B) Engine layer

Responsibilities:

- parse workflow type definitions
- parse workflow instance configs
- instantiate/bind workflow type + config
- execute dispatch loop
- route transitions and apply guardrails
- persist checkpoints/events

Must not depend on:

- GitHub-specific policy decisions
- issue/PR semantic details

### C) Workflow type layer

Responsibilities:

- step topology
- transition map
- expected inputs/outputs
- allowed hooks and guard references

### D) Workflow instance config layer

Responsibilities:

- parameter values for one run profile (e.g., profile 0, profile 10)
- limits (retry, loop bounds)
- target selection filters
- adapter command/runtime settings
- repository checkout path and workspace layout
- branch naming rules and branch-creation policy
- base branch, remote, and pull strategy configuration

### E) Adapter layer

Responsibilities:

- shell/git/gh command execution
- planner/LLM integrations
- artifact production

## 4.3 Type/instance model (avoid cornering)

To avoid cornering into a single hardcoded run model, the contract is:

- **Workflow type** = reusable logic graph + transition semantics.
- **Workflow config** = runtime parameters for one instance of that type.
- **Workflow instance** = `(workflow_type_id, config_id, run_id)` bound at launch.

This supports:

- one loop initially (single active instance)
- later N configured instances of same type
- later multiple workflow types
- controlled rollout by profile without code changes

## 4.4 Runner behavior contract

Engine lifecycle:

1. initialize context/state for run
2. load workflow type
3. load instance config
4. bind and validate type+config
5. execute state loop
6. after each step:
   - append structured event
   - persist checkpoint
   - evaluate transition
7. on shutdown signal:
   - persist final checkpoint
   - exit cleanly

Failure semantics:

- recoverable step failures route through remediation edges
- fatal failures route to abandonment/logging terminal path
- monitor handles engine crash restart policy

## 4.5 Monitor behavior contract

Monitor responsibilities:

- singleton lock acquisition
- spawn + monitor engine
- exponential backoff for crash loops
- heartbeat updates
- IPC status/control endpoint

Monitor control commands:

- `status`
- `shutdown`
- `restart-engine`

## 4.6 Release/distribution behavior

Release automation remains Rust-native via xtask commands:

- `cargo release-package <tag>`
- `cargo release-publish <tag>`
- `cargo release-update-tap <tag>`
- `cargo release-all <tag>`

Release flow includes:

- arm64 macOS build
- ad-hoc codesign
- checksum generation
- GitHub release upload/create
- Homebrew tap formula update

---

## 5) Directory structure

## 5.1 Build-time/repository structure (target)

```text
workflow/
  Cargo.toml
  Cargo.lock
  .cargo/
    config.toml
  .github/
    workflows/
      pr-quality.yml
      release.yml
  xtask/
    Cargo.toml
    src/main.rs

  src/
    main.rs
    lib.rs
    cli/
    monitor/
      mod.rs
      process.rs
      heartbeat.rs
      ipc.rs
    engine/
      mod.rs
      loader.rs
      runner.rs
      transition.rs
      instance.rs
    workflow/
      mod.rs
      schema.rs
      types/
      config/
    adapters/
      mod.rs
      shell.rs
      git.rs
      gh.rs
      llm.rs
    persistence/
      mod.rs
      sqlite.rs
      checkpoint.rs
      artifacts.rs
    service/
      mod.rs
      launchd.rs
      systemd.rs
      spec.rs

  config/
    monitor/
      default.toml
    engine/
      default.toml
    workflows/
      issue-fix-v1.toml        # workflow type
    workflow-configs/
      profile-0.toml           # workflow instance config
      profile-10.toml

  docs/
  project-plans/
    initial/
      overview.md
  research/
```

## 5.2 Runtime filesystem structure (per user)

macOS:

```text
~/Library/Application Support/luther-workflow/
  config/
    monitor.toml
    engine.toml
    workflows/
      issue-fix-v1.toml
    workflow-configs/
      profile-0.toml
      profile-10.toml
  state/
    runtime.db
    checkpoints/
      <run-id>.json
    heartbeats/
      monitor.json
    locks/
      monitor.lock
    ipc/
      monitor.sock
  artifacts/
    runs/
      <run-id>/
        events.jsonl
        step-output/
        diagnostics/
```

Linux:

```text
~/.config/luther-workflow/
  monitor.toml
  engine.toml
  workflows/issue-fix-v1.toml
  workflow-configs/profile-0.toml

~/.local/state/luther-workflow/
  runtime.db
  checkpoints/<run-id>.json
  heartbeats/monitor.json
  locks/monitor.lock
  ipc/monitor.sock
  artifacts/runs/<run-id>/...
```

Service definitions (generated):

```text
macOS: ~/Library/LaunchAgents/com.acoliver.luther-workflow.plist
Linux: ~/.config/systemd/user/luther-workflow.service
```

---

## 6) Configuration model and storage

## 6.1 Preferred config format choice

Given your preference, default to:

- **TOML as primary**
- **JSON as supported alternate**
- **No YAML in MVP**

TOML is used for:

- monitor config
- engine config
- workflow type definitions
- workflow instance configs

JSON support can be enabled for tool interoperability or generated configs.

## 6.2 Config layers and precedence

1. Static defaults (compiled)
2. Monitor/engine base config files
3. Workflow type file
4. Workflow instance config file
5. Environment overrides (`LUTHER_*`)
6. CLI flags (highest precedence)

## 6.3 Externalization model you requested

Conceptual hierarchy:

- Monitor
  - Engine
    - Workflow type
      - Workflow config (instance)

Runtime binding model:

- Monitor starts engine with `(workflow_type_id, config_id)`.
- Engine resolves files and binds them to create one workflow instance.
- For MVP we run exactly one active instance in a loop.
- Design remains multi-instance ready by making `(workflow_type_id, config_id)` explicit and persisted.

## 6.4 Where config is stored

Source-controlled defaults in repo:

- `config/workflows/*.toml` (type definitions)
- `config/workflow-configs/*.toml` (instance configs)
- `config/monitor/default.toml`
- `config/engine/default.toml`

User/runtime overrides:

- macOS: `~/Library/Application Support/luther-workflow/config/...`
- Linux: `~/.config/luther-workflow/...`

Persistence/state:

- macOS: `~/Library/Application Support/luther-workflow/state/...`
- Linux: `~/.local/state/luther-workflow/...`

## 6.5 Example binding contract

```text
workflow_type_id = "issue-fix-v1"
config_id        = "profile-0"
run_id           = "<uuid>"
```

Engine resolves:

- `config/workflows/issue-fix-v1.toml`
- `config/workflow-configs/profile-0.toml`

and writes run metadata with all three IDs.

This ensures later profile-10 runs or additional workflow types do not require architecture changes.

## 6.6 Working directory, checkout, and branch configuration

Workflow instance config must externalize repository working-copy behavior so the engine can instantiate runs without hardcoded paths.

Required config fields:

- `repo.url`: git remote URL to clone/fetch
- `repo.default_base_branch`: base branch for new work (e.g., `main`)
- `repo.remote_name`: default remote name (e.g., `origin`)
- `workspace.root`: local root directory for managed working copies
- `workspace.strategy`: one of:
  - `shared` (single reused checkout)
  - `per-run` (isolated checkout per run)
- `workspace.path_template`: optional template including `{run_id}` and `{config_id}`
- `branch.name_template`: naming template (e.g., `luther/{issue_number}-{slug}`)
- `branch.create_if_missing`: boolean
- `branch.force_reset`: boolean (whether to hard-reset branch to base on run start)
- `branch.push_remote`: boolean

MVP default recommendation:

- `workspace.strategy = "shared"`
- explicit `workspace.root` under state/config root
- deterministic `branch.name_template`
- `branch.create_if_missing = true`
- `branch.force_reset = false` (safety default)

This keeps single-loop execution simple now while preserving multi-instance and isolated-worktree modes later.

---

## 7) Dependency plan with upstream last commit dates

Below are planned dependencies, with observed latest crate version and upstream repository last commit metadata at planning time.

> Source method: crates.io API for crate metadata + GitHub commits API for repository head commit.

| Dependency | Planned Version | Repository | Upstream Last Commit (UTC) | Commit SHA |
|---|---:|---|---|---|
| dagrs | 0.8.0 | https://github.com/dagrs-dev/dagrs | 2026-01-16T07:35:06Z | 32e4021f6163 |
| tokio | 1.51.0 | https://github.com/tokio-rs/tokio | 2026-04-04T19:46:10Z | ad8c59add6a1 |
| serde | 1.0.228 | https://github.com/serde-rs/serde | 2026-03-06T05:45:51Z | fa7da4a93567 |
| serde_json | 1.0.149 | https://github.com/serde-rs/json | 2026-02-16T03:03:48Z | a42fa980f855 |
| thiserror | 2.0.18 | https://github.com/dtolnay/thiserror | 2026-03-24T02:34:27Z | d4a2507576d2 |
| anyhow | 1.0.102 | https://github.com/dtolnay/anyhow | 2026-03-24T02:44:29Z | 841522b2aa09 |
| tracing | 0.1.44 | https://github.com/tokio-rs/tracing | 2026-04-01T09:31:18Z | 72cf52a9e272 |
| tracing-subscriber | 0.3.23 | https://github.com/tokio-rs/tracing | 2026-04-01T09:31:18Z | 72cf52a9e272 |
| clap | 4.6.0 | https://github.com/clap-rs/clap | 2026-04-01T15:58:24Z | ddc008bbbc19 |
| toml | 1.1.2+spec-1.1.0 | https://github.com/toml-rs/toml | 2026-04-02T18:51:45Z | 36e558e13427 |
| directories | 6.0.0 | https://github.com/soc/directories-rs | 2025-01-12T19:35:36Z | 4d76f1a5a0a9 |
| rusqlite | 0.39.0 | https://github.com/rusqlite/rusqlite | 2026-04-02T17:50:53Z | 0b5c9d8b099b |
| uuid | 1.23.0 | https://github.com/uuid-rs/uuid | 2026-03-26T23:35:39Z | 00ab922d5351 |

---

## 8) Implementation milestones (initial)

1. Create module skeleton (`monitor/engine/workflow/adapters/persistence/service`).
2. Add TOML schemas/types for:
   - monitor config
   - engine config
   - workflow type
   - workflow instance config
3. Add JSON parser support for same schema model.
4. Integrate dagrs-backed engine wrapper and step dispatch contracts.
5. Add persistence/checkpoint/event logging pipeline.
6. Implement monitor + IPC + heartbeat.
7. Implement initial service adapters (launchd/systemd user mode).
8. Deliver MVP workflow type + profile-0 instance config and validation tests.
9. Wire CLI commands for run/monitor/service/status.

---

## 9) Acceptance criteria for this initial setup

- Workflow type and workflow instance config are external files, separate from code.
- Engine binds `(workflow_type_id, config_id)` into runtime workflow instances.
- MVP runs one configured instance in a loop without hardcoding single-instance assumptions.
- Runner can execute step graph with branches and bounded loops.
- Checkpoint/resume path works for interrupted runs.
- Monitor can restart engine on crash and report health over IPC.
- Service install/start/status works in user mode on macOS and Linux.
- Structured run artifacts are persisted in deterministic paths.
- Quality gates and release/tap flow remain enforced through xtask + CI.
