# Luther Initial Requirements (EARS)

This document captures initial requirements in EARS format for the monitor, engine, workflow type/config model, runtime behavior, and repository working-copy management.

Format legend:

- Ubiquitous: The `<system>` shall `<response>`.
- Event-driven: When `<trigger>`, the `<system>` shall `<response>`.
- State-driven: While `<state>`, the `<system>` shall `<response>`.
- Unwanted behavior: If `<fault/condition>`, then the `<system>` shall `<response>`.
- Optional feature: Where `<feature enabled>`, the `<system>` shall `<response>`.

---

## 1) Architecture and boundaries

### REQ-EARS-ARCH-001 (Ubiquitous)
The runtime platform shall separate monitor responsibilities, engine responsibilities, workflow type definitions, and workflow instance configuration.

### REQ-EARS-ARCH-002 (Ubiquitous)
The engine shall not embed workflow-domain-specific policy logic that belongs in workflow type/config definitions.

### REQ-EARS-ARCH-003 (Ubiquitous)
The monitor shall supervise engine lifecycle without depending on workflow step semantics.

### REQ-EARS-ARCH-004 (Ubiquitous)
The engine shall instantiate workflow execution from `(workflow_type_id, config_id, run_id)`.

### REQ-EARS-ARCH-005 (Optional feature)
Where multi-instance execution is disabled, the monitor shall enforce a single active workflow instance while preserving type/config identifiers in persisted metadata.

---

## 2) Workflow definition and externalized config

### REQ-EARS-WF-001 (Ubiquitous)
The system shall treat workflow topology as external declarative data, separate from Rust implementation code.

### REQ-EARS-WF-002 (Ubiquitous)
The system shall support TOML as the primary format for workflow type and instance configuration.

### REQ-EARS-WF-003 (Optional feature)
Where JSON input support is enabled, the system shall accept semantically equivalent JSON representations of workflow type and instance configuration.

### REQ-EARS-WF-004 (Event-driven)
When a workflow run is requested, the engine shall resolve workflow type and workflow config files by configured identifiers.

### REQ-EARS-WF-005 (Unwanted behavior)
If workflow type or workflow config validation fails, then the engine shall reject run startup and emit structured validation errors.

### REQ-EARS-WF-006 (Ubiquitous)
The workflow type definition shall include step topology, transitions, and guard references.

### REQ-EARS-WF-007 (Ubiquitous)
The workflow instance config shall include runtime parameters, guard limits, adapter settings, and repository workspace/branch settings.

---

## 3) Monitor and engine lifecycle

### REQ-EARS-MON-001 (Event-driven)
When the monitor starts, it shall acquire singleton ownership for its configured scope before launching an engine instance.

### REQ-EARS-MON-002 (State-driven)
While the monitor is running, it shall maintain heartbeat/status metadata for CLI and service observability.

### REQ-EARS-MON-003 (Event-driven)
When the engine process exits unexpectedly, the monitor shall apply configured restart/backoff policy.

### REQ-EARS-MON-004 (Unwanted behavior)
If restart attempts exceed configured safety limits, then the monitor shall transition to degraded/unhealthy state and stop unbounded restart loops.

### REQ-EARS-MON-005 (Event-driven)
When a shutdown command is received, the monitor shall request graceful engine stop and persist final monitor state.

### REQ-EARS-ENG-001 (Event-driven)
When an engine run starts, the engine shall bind workflow type and workflow config into a concrete workflow instance.

### REQ-EARS-ENG-002 (State-driven)
While a workflow instance is executing, the engine shall persist checkpoints and structured events after each step transition.

### REQ-EARS-ENG-003 (Unwanted behavior)
If a step returns a fatal error condition, then the engine shall route to configured terminal failure handling and write terminal run artifacts.

### REQ-EARS-ENG-004 (Event-driven)
When an interrupt/shutdown signal is received, the engine shall persist a resumable checkpoint and exit cleanly.

---

## 4) Step routing, loops, and guardrails

### REQ-EARS-ROUTE-001 (Ubiquitous)
The engine shall route transitions using structured step outcomes rather than string-matching unstructured logs.

### REQ-EARS-ROUTE-002 (State-driven)
While in remediation-capable states, the engine shall permit configured loop-back transitions to prior execution states.

### REQ-EARS-ROUTE-003 (Unwanted behavior)
If configured loop limits are reached, then the engine shall route to configured abandonment/terminal logging outcomes.

### REQ-EARS-ROUTE-004 (Ubiquitous)
The engine shall enforce retry and loop guardrails from workflow config.

---

## 5) Repository working directory, checkout, and branching

### REQ-EARS-REPO-001 (Ubiquitous)
The workflow config shall define repository checkout source, workspace root, and branch policy.

### REQ-EARS-REPO-002 (Event-driven)
When a run initializes repository context, the engine shall resolve or create the configured working directory according to workspace strategy.

### REQ-EARS-REPO-003 (Optional feature)
Where `workspace.strategy = shared`, the engine shall reuse a single configured checkout path for successive runs.

### REQ-EARS-REPO-004 (Optional feature)
Where `workspace.strategy = per-run`, the engine shall create an isolated working path derived from configured path template and run metadata.

### REQ-EARS-REPO-005 (Event-driven)
When preparing a run branch, the engine shall checkout configured base branch and create/switch to a branch derived from `branch.name_template`.

### REQ-EARS-REPO-006 (Optional feature)
Where `branch.create_if_missing = true`, the engine shall create the branch when it does not exist.

### REQ-EARS-REPO-007 (Optional feature)
Where `branch.force_reset = true`, the engine shall hard-reset run branch to configured base branch before workflow execution begins.

### REQ-EARS-REPO-008 (Unwanted behavior)
If repository checkout, fetch, or branch preparation fails, then the engine shall fail run initialization with structured diagnostics and no partial workflow execution.

### REQ-EARS-REPO-009 (Optional feature)
Where `branch.push_remote = true`, the workflow actions shall push run branches to configured remote as part of push/submit stages.

---

## 6) Persistence, artifacts, and traceability

### REQ-EARS-PERSIST-001 (Ubiquitous)
The system shall persist run metadata, workflow instance identifiers, and state transitions in local durable storage.

### REQ-EARS-PERSIST-002 (Event-driven)
When each step completes, the engine shall append an event record and persist checkpoint data before entering the next step.

### REQ-EARS-PERSIST-003 (Ubiquitous)
The artifact subsystem shall write per-run outputs under deterministic run-scoped directories.

### REQ-EARS-PERSIST-004 (Unwanted behavior)
If persistence writes fail, then the engine shall raise a structured persistence error and avoid silent continuation.

---

## 7) Service mode and control plane

### REQ-EARS-SVC-001 (Ubiquitous)
The runtime service mode shall run as a foreground process supervised by launchd/systemd rather than self-daemonizing.

### REQ-EARS-SVC-002 (Event-driven)
When service install is requested, the service layer shall generate and install platform-specific service definitions from current configuration.

### REQ-EARS-SVC-003 (State-driven)
While monitor is active, the control plane shall expose local status and control operations through IPC.

### REQ-EARS-SVC-004 (Unwanted behavior)
If service operations fail (install/start/stop/status), then the service layer shall return explicit platform-specific diagnostic details.

---

## 8) Quality, safety, and release controls

### REQ-EARS-QUAL-001 (Ubiquitous)
The project quality gate shall enforce formatting, clippy checks, structural guards, complexity checks, tests, and coverage through xtask/CI.

### REQ-EARS-QUAL-002 (Ubiquitous)
The release process shall run through xtask commands for packaging, signing, publishing, and Homebrew tap update.

### REQ-EARS-QUAL-003 (Event-driven)
When release is triggered by a valid tag, the release workflow shall invoke `cargo release-all <tag>`.

### REQ-EARS-QUAL-004 (Unwanted behavior)
If required release secrets are missing, then release workflow execution shall fail before packaging/publish operations begin.

---

## 9) Initial single-instance operation and forward scalability

### REQ-EARS-SCALE-001 (State-driven)
While MVP single-instance mode is enabled, the monitor shall run exactly one active workflow instance loop.

### REQ-EARS-SCALE-002 (Ubiquitous)
The persisted run model shall include workflow type and config identifiers so later multi-instance scheduling can be added without schema redesign.

### REQ-EARS-SCALE-003 (Optional feature)
Where multiple workflow instance profiles are configured, the monitor shall be able to select a configured instance by ID without changing workflow type code.
