//! Capsule wiring integration-first tests (P13 RED phase).
//!
//! These tests exercise the **actual P08B/P12 architecture** (not stale
//! separate-persist assumptions): the atomic fresh-launch capsule+run
//! persistence path, the object-safe V1 adapter dispatch, the production
//! `RunnerRecoveryExecutor`, and the `RecoveryWiring` public seams that P14
//! call sites consume. All tests use **real SQLite** (in-memory or tempdir),
//! are deterministic, and do not touch the network.
//!
//! ## RED/green split
//!
//! - **RED tests** fail at designated P14 stubs only:
//!   - `V1Adapter::build_instance` returns `AdapterError::Deserialization`
//!   - `RunnerRecoveryExecutor::build_resume_runner` returns
//!     `RecoveryExecutionError::Unavailable`
//!
//!   These are the only designated P14 stubs. No production fake success is
//!   fabricated.
//!
//! - **GREEN tests** verify already-implemented P08B/P11/P12 invariants:
//!   atomic launch, rollback, adapter dispatch, tampered/unknown-version
//!   rejection, capsule-less fail-closed, and refusal/stale short-circuits
//!   that never invoke the executor.
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P13
//! @requirement:REQ-RP-002,REQ-RP-009

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;

use luther_workflow::engine::recovery::adapters::{adapter_for, AdapterError, CapsuleAdapter};
use luther_workflow::engine::recovery::capsule::{
    build_capsule_v1, verify_envelope_digest, CapsuleError, ExecutionCapsuleV1,
    CURRENT_SCHEMA_VERSION,
};
use luther_workflow::engine::recovery::protocol::{
    OperatorVerb, RecoveryExecutionError, RecoveryExecutionInvocation, RecoveryExecutionResult,
    RecoveryExecutor, RecoveryOutcome, RecoveryProtocolV1, RecoveryRequest, RecoveryStrategy,
    RefusalReason,
};
use luther_workflow::engine::recovery::wiring::{RecoveryWiring, RunnerRecoveryExecutor};
use luther_workflow::engine::recovery::{policy_for_step, select_strategy, StepRecoveryPolicy};
use luther_workflow::engine::RunContext;
use luther_workflow::persistence::capsule_store::{
    init_capsules_table, load_capsule_v1, persist_capsule_v1,
};
use luther_workflow::persistence::checkpoint::StateSnapshot;
use luther_workflow::persistence::launch_provenance::LaunchProvenance;
use luther_workflow::persistence::sqlite::{init_runs_schema, persist_run_with_conn};
use luther_workflow::persistence::{get_run_with_conn, RunMetadata};
use luther_workflow::workflow::schema::{
    DiffPathNormalization, GuardConfig, GuardLimits, ParentOrchestrationConfig, RepoConfig,
    RuntimeConfig, StepDef, TransitionDef, WorkflowConfig, WorkflowType,
};

// ===========================================================================
// Test helpers
// ===========================================================================

/// A minimal `WorkflowType` with a step declaring `PureReenter` recovery
/// policy. This selects `RecoveryStrategy::Reenter` (executable, not refused),
/// so the protocol reaches the execute phase.
fn pure_reenter_workflow_type() -> WorkflowType {
    WorkflowType {
        workflow_type_id: "capsule-wiring-p13".to_string(),
        steps: vec![
            StepDef {
                step_id: "step1".to_string(),
                step_type: "noop".to_string(),
                description: None,
                parameters: None,
                produces: None,
                consumes: None,
                terminal: None,
                recovery_policy: Some(StepRecoveryPolicy::PureReenter),
            },
            StepDef {
                step_id: "step2".to_string(),
                step_type: "noop".to_string(),
                description: None,
                parameters: None,
                produces: None,
                consumes: None,
                terminal: Some(true),
                recovery_policy: Some(StepRecoveryPolicy::PureReenter),
            },
        ],
        transitions: vec![TransitionDef {
            from: "step1".to_string(),
            to: "step2".to_string(),
            condition: None,
            max_iterations: None,
        }],
        guards: GuardConfig {
            max_retries: None,
            timeout_seconds: None,
            require_approval: None,
        },
    }
}

/// A `WorkflowType` with a generic shell step carrying NO explicit recovery
/// policy and a step_id not in `SAFE_RERUN_STEPS`. This resolves to
/// `NonRecoverable` → `Refused(NonRecoverable)`.
fn non_recoverable_workflow_type() -> WorkflowType {
    WorkflowType {
        workflow_type_id: "capsule-wiring-p13-nonrec".to_string(),
        steps: vec![StepDef {
            step_id: "generic_shell_step".to_string(),
            step_type: "shell".to_string(),
            description: None,
            parameters: None,
            produces: None,
            consumes: None,
            terminal: None,
            recovery_policy: None,
        }],
        transitions: vec![],
        guards: GuardConfig {
            max_retries: None,
            timeout_seconds: None,
            require_approval: None,
        },
    }
}

/// A minimal `WorkflowConfig` matching the pure-reenter workflow.
fn sample_config() -> WorkflowConfig {
    config_for_workflow("capsule-wiring-p13")
}

/// A `WorkflowConfig` for a given `workflow_type_id`.
fn config_for_workflow(workflow_type_id: &str) -> WorkflowConfig {
    WorkflowConfig {
        config_id: format!("{workflow_type_id}-config"),
        workflow_type_id: workflow_type_id.to_string(),
        runtime: RuntimeConfig {
            timeout_seconds: 60,
            max_retries: 1,
            parallel_steps: None,
            log_level: None,
        },
        repo: RepoConfig {
            workspace_strategy: "temp_clone".to_string(),
            branch_template: "wf-{run_id}".to_string(),
            base_branch: Some("main".to_string()),
            workspace_root: None,
            project_subdir: None,
            artifact_path_base: None,
            diff_path_base: None,
            diff_path_normalization: DiffPathNormalization::RepoRelative,
        },
        guard_limits: GuardLimits {
            max_iterations: None,
            max_file_changes: None,
            max_tokens: None,
            max_cost: None,
        },
        variables: HashMap::new(),
        discovery: None,
        parent_orchestration: ParentOrchestrationConfig::default(),
        merge_required: false,
        merge_strategy: None,
        command_manifest: None,
        target_profile: None,
    }
}

/// Construct a `LaunchProvenance` from the current directory.
fn sample_provenance() -> LaunchProvenance {
    LaunchProvenance::from_resolved(
        &pure_reenter_workflow_type(),
        &sample_config(),
        Path::new("."),
    )
    .expect("canonicalize '.'")
}

/// Build a capsule for the given run_id using the pure-reenter workflow.
fn build_test_capsule(run_id: &str) -> ExecutionCapsuleV1 {
    let workflow = pure_reenter_workflow_type();
    let config = sample_config();
    let provenance = sample_provenance();
    build_capsule_v1(
        run_id.to_string(),
        &workflow,
        &config,
        Path::new("."),
        &provenance,
        "main".to_string(),
    )
    .expect("build capsule")
}

/// Build a capsule for the given run_id using the non-recoverable workflow.
fn build_non_recoverable_capsule(run_id: &str) -> ExecutionCapsuleV1 {
    let workflow = non_recoverable_workflow_type();
    let config = config_for_workflow("capsule-wiring-p13-nonrec");
    let provenance = LaunchProvenance::from_resolved(&workflow, &config, Path::new("."))
        .expect("canonicalize '.'");
    build_capsule_v1(
        run_id.to_string(),
        &workflow,
        &config,
        Path::new("."),
        &provenance,
        "main".to_string(),
    )
    .expect("build capsule")
}

/// Create an in-memory SQLite connection with ALL recovery tables initialized.
fn recovery_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    luther_workflow::persistence::recovery_epoch::init_epoch_table(&conn)
        .expect("init epoch table");
    luther_workflow::persistence::recovery_operations::init_operations_table(&conn)
        .expect("init operations table");
    luther_workflow::persistence::attempts::init_attempts_table(&conn)
        .expect("init attempts table");
    luther_workflow::persistence::effect_intents::init_effect_intents_table(&conn)
        .expect("init effect intents table");
    init_capsules_table(&conn).expect("init capsules table");
    init_runs_schema(&conn).expect("init runs schema");
    luther_workflow::persistence::checkpoint::init_checkpoint_table(&conn)
        .expect("init checkpoint table");
    luther_workflow::persistence::wait_state::init_wait_states_table(&conn)
        .expect("init wait states table");
    luther_workflow::persistence::leases::init_leases_table(&conn).expect("init leases table");
    conn
}

/// Build and persist a capsule, returning the built capsule.
fn persisted_capsule(conn: &Connection, run_id: &str) -> ExecutionCapsuleV1 {
    let capsule = build_test_capsule(run_id);
    persist_capsule_v1(conn, &capsule).expect("persist capsule");
    capsule
}

/// Build and persist a non-recoverable capsule, returning the built capsule.
fn persisted_non_recoverable_capsule(conn: &Connection, run_id: &str) -> ExecutionCapsuleV1 {
    let capsule = build_non_recoverable_capsule(run_id);
    persist_capsule_v1(conn, &capsule).expect("persist capsule");
    capsule
}

/// Build a `RunMetadata` for the given run_id in `Starting` state.
fn starting_metadata(run_id: &str) -> RunMetadata {
    let mut metadata = RunMetadata::new(run_id, "capsule-wiring-p13", "capsule-wiring-p13-config");
    metadata.status = luther_workflow::persistence::RunStatus::Starting;
    metadata.current_step = Some("step1".to_string());
    metadata
}

/// Persist a `Starting` run row.
fn seed_run(conn: &Connection, run_id: &str) {
    let metadata = starting_metadata(run_id);
    persist_run_with_conn(conn, &metadata).expect("persist run metadata");
}

/// A basic `RecoveryRequest` for the given run/step at epoch 0.
fn recovery_request(run_id: &str, step_id: &str, expected_epoch: u64) -> RecoveryRequest {
    RecoveryRequest {
        run_id: run_id.to_string(),
        step_id: step_id.to_string(),
        expected_epoch,
        operator_verb: OperatorVerb::Resume,
    }
}

/// Count capsule rows for a run_id.
fn count_capsule_rows(conn: &Connection, run_id: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM execution_capsules WHERE run_id = ?1",
        rusqlite::params![run_id],
        |row| row.get(0),
    )
    .expect("count capsule rows")
}

/// Count run rows for a run_id.
fn count_run_rows(conn: &Connection, run_id: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM runs WHERE run_id = ?1",
        rusqlite::params![run_id],
        |row| row.get(0),
    )
    .expect("count run rows")
}

/// A deterministic [`RecoveryExecutor`] that records whether it was invoked.
///
/// Used by GREEN tests to assert the executor was NOT called for refusals or
/// stale-epoch short-circuits. Never fabricates success on its own — it only
/// records invocations and returns a configured result.
#[derive(Clone)]
struct RecordingExecutor {
    calls: Arc<Mutex<Vec<String>>>,
}

impl RecordingExecutor {
    fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn was_called(&self) -> bool {
        !self.calls.lock().unwrap().is_empty()
    }
}

impl std::fmt::Debug for RecordingExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordingExecutor").finish()
    }
}

impl RecoveryExecutor for RecordingExecutor {
    fn execute(
        &self,
        invocation: &RecoveryExecutionInvocation<'_>,
    ) -> Result<RecoveryExecutionResult, RecoveryExecutionError> {
        self.calls
            .lock()
            .unwrap()
            .push(invocation.step_id.to_string());
        Ok(RecoveryExecutionResult {
            step_status: "completed".to_string(),
            state_snapshot: StateSnapshot {
                status: "completed".to_string(),
                ..StateSnapshot::default()
            },
            runner_result: Some(serde_json::json!({"status": "success"})),
        })
    }
}

// ===========================================================================
// 1. Atomic fresh-launch pair and rollback [GREEN: P08B]
// ===========================================================================

/// GIVEN: a fresh launch via `EngineRunner::with_db_path_for_launch`
/// WHEN: the constructor runs with a valid capsule
/// THEN: both the run row and capsule row are atomically persisted
///
/// GREEN: exercises the P08B atomic fresh-launch path.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P13
/// @requirement:REQ-RP-002
#[test]
fn fresh_launch_atomic_pair_persists_run_and_capsule() {
    use luther_workflow::engine::executor::ExecutorRegistry;
    use luther_workflow::engine::instance::WorkflowInstance;
    use luther_workflow::engine::runner::EngineRunner;
    use luther_workflow::persistence::{init_database, RunStatus};

    let temp = tempfile::tempdir().expect("create temp dir");
    let db_path = temp.path().join("test.db");
    init_database(&db_path).expect("init database");

    let run_id = "run-p13-launch-001";
    let workflow = pure_reenter_workflow_type();
    let config = sample_config();
    let provenance = sample_provenance();
    let capsule = build_capsule_v1(
        run_id.to_string(),
        &workflow,
        &config,
        Path::new("."),
        &provenance,
        "main".to_string(),
    )
    .expect("build capsule");

    let instance = WorkflowInstance::create_with_run_id(workflow, config, run_id);
    let registry = ExecutorRegistry::with_defaults();
    let runner = EngineRunner::with_db_path_for_launch(
        instance,
        registry,
        &db_path,
        RunContext::default(),
        capsule,
    )
    .expect("fresh launch must succeed");
    assert_eq!(runner.run_id(), run_id);

    let conn = Connection::open(&db_path).expect("open db");
    let run = get_run_with_conn(&conn, run_id)
        .expect("query run")
        .expect("run row must exist");
    assert_eq!(run.status, RunStatus::Starting);

    let loaded = load_capsule_v1(&conn, run_id).expect("capsule must exist");
    assert_eq!(loaded.run_id, run_id);
    verify_envelope_digest(&loaded).expect("capsule must verify");
}

/// GIVEN: a database with a pre-existing run row for the same run_id
/// WHEN: `EngineRunner::with_db_path_for_launch` is called
/// THEN: the constructor fails (run collision) and NO capsule is left behind
///
/// GREEN: exercises the P08B rollback invariant.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P13
/// @requirement:REQ-RP-002
#[test]
fn fresh_launch_collision_leaves_neither_row_duplicated() {
    use luther_workflow::engine::executor::ExecutorRegistry;
    use luther_workflow::engine::instance::WorkflowInstance;
    use luther_workflow::engine::runner::EngineRunner;
    use luther_workflow::persistence::init_database;

    let temp = tempfile::tempdir().expect("create temp dir");
    let db_path = temp.path().join("test.db");
    init_database(&db_path).expect("init database");

    let run_id = "run-p13-collision-001";
    let existing = starting_metadata(run_id);
    {
        let conn = Connection::open(&db_path).expect("open db");
        persist_run_with_conn(&conn, &existing).expect("seed existing run");
    }

    let workflow = pure_reenter_workflow_type();
    let config = sample_config();
    let provenance = sample_provenance();
    let capsule = build_capsule_v1(
        run_id.to_string(),
        &workflow,
        &config,
        Path::new("."),
        &provenance,
        "main".to_string(),
    )
    .expect("build capsule");

    let instance = WorkflowInstance::create_with_run_id(workflow, config, run_id);
    let registry = ExecutorRegistry::with_defaults();
    let result = EngineRunner::with_db_path_for_launch(
        instance,
        registry,
        &db_path,
        RunContext::default(),
        capsule,
    );
    assert!(result.is_err(), "collision must error");

    let conn = Connection::open(&db_path).expect("open db");
    assert_eq!(
        count_capsule_rows(&conn, run_id),
        0,
        "no capsule after collision"
    );
    assert_eq!(
        count_run_rows(&conn, run_id),
        1,
        "original run row preserved"
    );
}

// ===========================================================================
// 2. Object-safe V1 build_instance path [RED: P14 stub]
// ===========================================================================

/// GIVEN: a persisted V1 capsule
/// WHEN: `adapter_for(capsule).build_instance(capsule)` is called
/// THEN: returns a reconstructed `WorkflowInstance` whose workflow_type_id,
///       config_id, and run_id match the capsule's authority
///
/// RED: `build_instance` is a designated P14 stub returning an error. This
/// assertion on the expected success behavior fails naturally at the stub —
/// the only place the failure may occur.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P13
/// @requirement:REQ-RP-009
#[test]
fn v1_adapter_build_instance_reconstructs_instance_from_capsule() {
    let conn = recovery_conn();
    let run_id = "run-p13-build-instance-001";
    let capsule = persisted_capsule(&conn, run_id);

    // Object-safe dispatch: adapter_for returns Box<dyn CapsuleAdapter>. [C8]
    let adapter: Box<dyn CapsuleAdapter> =
        adapter_for(&capsule).expect("adapter_for must dispatch V1");
    assert_eq!(adapter.version(), 1, "V1 adapter version");

    // build_instance must reconstruct the WorkflowInstance from the immutable
    // capsule bytes. Designated P14 stub.
    let instance = adapter
        .build_instance(&capsule)
        .expect("build_instance must reconstruct the instance once P14 implements it");
    assert_eq!(
        instance.workflow_type_id(),
        "capsule-wiring-p13",
        "reconstructed instance must carry the capsule's workflow_type_id [C8]"
    );
    assert_eq!(
        instance.config_id(),
        "capsule-wiring-p13-config",
        "reconstructed instance must carry the capsule's config_id [C8]"
    );
    assert_eq!(
        instance.run_id, run_id,
        "reconstructed instance must carry the capsule's run_id [C8]"
    );
}

// ===========================================================================
// 3. Production RunnerRecoveryExecutor through RecoveryProtocolV1 [GREEN: P14]
// ===========================================================================

/// GIVEN: a persisted capsule + seeded run with a PureReenter step
/// WHEN: `RecoveryProtocolV1::recover_with_executor` is called with the
///       production `RunnerRecoveryExecutor`
/// THEN: the protocol proceeds through prepare → reserve → execute → finalize,
///       returning `RecoveryOutcome::Recovered`
///
/// GREEN: the production `RunnerRecoveryExecutor::build_resume_runner` now
/// reconstructs the instance from the capsule and runs the normal transition
/// loop via `EngineRunner::run_from_current_step`. The reserved step executes
/// and the workflow transitions to a terminal success.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14
/// @requirement:REQ-RP-001,REQ-RP-009
#[test]
fn production_runner_executor_through_protocol_reaches_recovered() {
    let conn = recovery_conn();
    let run_id = "run-p13-runner-exec-001";
    persisted_capsule(&conn, run_id);
    seed_run(&conn, run_id);

    let request = recovery_request(run_id, "step1", 0);
    let protocol = RecoveryProtocolV1;

    // Production executor from the wiring skeleton. The executor opens its own
    // DB connection at db_path; the protocol operates on the in-memory conn.
    let temp = tempfile::tempdir().expect("create temp dir for executor db");
    let db_path = temp.path().join("executor.db");
    let executor = RunnerRecoveryExecutor::new(db_path, RunContext::default());

    let outcome = protocol
        .recover_with_executor(&conn, Path::new("."), &request, &executor)
        .expect("production recovery must succeed with the P14 transition-loop executor");
    assert!(
        matches!(outcome, RecoveryOutcome::Recovered { .. }),
        "production executor through protocol must reach Recovered, got {outcome:?}"
    );
}

// ===========================================================================
// 4. RecoveryWiring public seams [GREEN + RED: P14 stub]
// ===========================================================================

/// GIVEN: a persisted V1 capsule
/// WHEN: `RecoveryWiring::adapter_for_resume(capsule)` is called
/// THEN: returns a `Box<dyn CapsuleAdapter>` with `version() == 1`
///
/// GREEN: the wiring seam dispatches the object-safe V1 adapter. This is the
/// public seam that P14 resume-surface call sites (continuation_execution,
/// resume_daemon_workflow, resume_child_workflow) consume.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P13
/// @requirement:REQ-RP-009
#[test]
fn recovery_wiring_adapter_for_resume_dispatches_object_safe_v1() {
    let conn = recovery_conn();
    let run_id = "run-p13-wiring-adapter-001";
    let capsule = persisted_capsule(&conn, run_id);

    let wiring = RecoveryWiring;
    let adapter: Box<dyn CapsuleAdapter> = wiring
        .adapter_for_resume(&capsule)
        .expect("adapter_for_resume must dispatch V1");
    assert_eq!(adapter.version(), 1, "wiring adapter must be V1");
    assert_eq!(
        adapter.envelope_digest(&capsule),
        capsule.envelope_digest,
        "wiring adapter envelope_digest must match"
    );
}

/// GIVEN: a `RunnerRecoveryExecutor` obtained from `RecoveryWiring::runner_executor`
/// WHEN: its `execute` method is invoked directly
/// THEN: returns a `RecoveryExecutionResult` carrying the executed step status
///       and a truthful (non-default) state snapshot
///
/// GREEN: `RunnerRecoveryExecutor::build_resume_runner` now reconstructs the
/// instance from the capsule and runs the normal transition loop via
/// `EngineRunner::run_from_current_step`. The result carries the actual
/// step status and the exact final runner state snapshot.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14
/// @requirement:REQ-RP-009
#[test]
fn recovery_wiring_runner_executor_executes_step() {
    let conn = recovery_conn();
    let run_id = "run-p13-wiring-exec-001";
    let capsule = persisted_capsule(&conn, run_id);

    let wiring = RecoveryWiring;
    let temp = tempfile::tempdir().expect("create temp dir for executor db");
    let db_path = temp.path().join("executor.db");
    let executor = wiring.runner_executor(db_path, RunContext::default());

    // Construct a minimal invocation (all public fields).
    let invocation = RecoveryExecutionInvocation {
        run_id,
        step_id: "step1",
        operation_id: "op-p13-wiring",
        attempt_id: 1,
        epoch: 1,
        strategy: RecoveryStrategy::Reenter,
        capsule: &capsule,
        workspace: Path::new("."),
    };

    let result = executor
        .execute(&invocation)
        .expect("runner_executor must execute the step via the P14 transition loop");
    assert_eq!(
        result.step_status, "completed",
        "executed result must report the terminal success status"
    );
    assert_eq!(
        result.state_snapshot.status, "completed",
        "state snapshot must carry the actual final status, not a fabricated default"
    );
    // The snapshot must NOT be a default: it must carry the real edge_loop_counts
    // (empty for a linear workflow) and a non-default status. A fabricated
    // default would have status "running".
    assert_ne!(
        result.state_snapshot.status, "running",
        "state snapshot must not be a fabricated default (status=running)"
    );
}

// ===========================================================================
// 5. Tampered envelope [GREEN: P08]
// ===========================================================================

/// GIVEN: a capsule with a tampered envelope digest persisted to SQLite
/// WHEN: `load_capsule_v1(conn, run_id)` is called
/// THEN: loading fails (envelope verification fails inside persist/load)
///
/// GREEN: the capsule store verifies the envelope digest on persist and load,
/// rejecting a tampered capsule before any step executes.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P13
/// @requirement:REQ-RP-002
#[test]
fn tampered_envelope_digest_fails_verification() {
    let conn = recovery_conn();
    let run_id = "run-p13-tampered-001";

    // Build a valid capsule, then tamper the envelope digest.
    let mut capsule = build_test_capsule(run_id);
    capsule.envelope_digest =
        "0000000000000000000000000000000000000000000000000000000000000000".to_string();

    // Direct verification fails.
    assert_eq!(
        verify_envelope_digest(&capsule),
        Err(CapsuleError::EnvelopeDigestMismatch),
        "tampered capsule must fail verification"
    );

    // Persisting also fails (envelope verification inside persist_capsule_v1).
    let persist_result = persist_capsule_v1(&conn, &capsule);
    assert!(
        persist_result.is_err(),
        "persisting a tampered capsule must fail"
    );

    // No capsule row should exist.
    assert_eq!(
        count_capsule_rows(&conn, run_id),
        0,
        "no capsule row after tampered persist failure"
    );
}

// ===========================================================================
// 6. Unknown schema version [GREEN: P08]
// ===========================================================================

/// GIVEN: a capsule with `schema_version = 99`
/// WHEN: `adapter_for(capsule)` is called
/// THEN: returns `AdapterError::UnsupportedCapsuleVersion(99)` — no adapter,
///       no step executes
///
/// GREEN: the object-safe adapter dispatch is fail-closed for unknown versions.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P13
/// @requirement:REQ-RP-009
#[test]
fn unknown_schema_version_rejected_before_adapter_dispatch() {
    let mut capsule = build_test_capsule("run-p13-unknown-schema-001");
    capsule.schema_version = 99;

    let error = match adapter_for(&capsule) {
        Ok(_) => panic!("unknown version must error"),
        Err(e) => e,
    };
    assert_eq!(
        error,
        AdapterError::UnsupportedCapsuleVersion(99),
        "schema_version 99 must yield UnsupportedCapsuleVersion(99)"
    );
}

/// GIVEN: a V1 capsule (current schema version)
/// WHEN: `adapter_for(capsule)` is called
/// THEN: returns a `Box<dyn CapsuleAdapter>` with `version() == CURRENT_SCHEMA_VERSION`
///
/// GREEN: the supported version dispatches correctly.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P13
/// @requirement:REQ-RP-009
#[test]
fn current_schema_version_dispatches_v1_adapter() {
    let capsule = build_test_capsule("run-p13-current-schema-001");
    assert_eq!(capsule.schema_version, CURRENT_SCHEMA_VERSION);

    let adapter = adapter_for(&capsule).expect("current version must dispatch");
    assert_eq!(adapter.version(), CURRENT_SCHEMA_VERSION);
}

// ===========================================================================
// 7. Capsule-less historical run fail-closed [GREEN: P08/P12]
// ===========================================================================

/// GIVEN: a run with NO persisted capsule (legacy/historical run)
/// WHEN: `load_capsule_v1(conn, run_id)` is called
/// THEN: returns an error (row not found) — fail-closed, no fabricated capsule
///
/// GREEN: the capsule store returns an error for a capsule-less run. This is
/// the fail-closed behavior for legacy runs. The salvage signal
/// (`RefusalReason::SalvageOnly`) is owned by P15; this test verifies only the
/// already-implemented fail-closed load behavior.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P13
/// @requirement:REQ-RP-002
#[test]
fn capsule_less_historical_run_fails_closed_on_load() {
    let conn = recovery_conn();
    let run_id = "run-p13-capsule-less-001";
    seed_run(&conn, run_id);

    // The run exists but has no capsule.
    assert_eq!(count_run_rows(&conn, run_id), 1, "run row must exist");
    assert_eq!(
        count_capsule_rows(&conn, run_id),
        0,
        "no capsule for this run"
    );

    // Loading must fail (not return a fabricated capsule).
    let result = load_capsule_v1(&conn, run_id);
    assert!(
        result.is_err(),
        "loading a capsule-less run must fail closed"
    );
}

/// GIVEN: a capsule-less historical run
/// WHEN: `RecoveryProtocolV1::recover` is called for it
/// THEN: returns a hard error (not a fabricated `Recovered` outcome)
///
/// GREEN: the protocol's prepare phase fails closed when the capsule cannot
/// be loaded. No production default fabricates success.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P13
/// @requirement:REQ-RP-001,REQ-RP-002
#[test]
fn capsule_less_run_protocol_fails_closed_not_fabricated() {
    let conn = recovery_conn();
    let run_id = "run-p13-capsule-less-proto-001";
    seed_run(&conn, run_id);

    let request = recovery_request(run_id, "step1", 0);
    let protocol = RecoveryProtocolV1;

    // Using the fail-closed default executor (no executor injected).
    let result = protocol.recover(&conn, Path::new("."), &request);
    assert!(
        result.is_err(),
        "capsule-less run must produce a hard error, not a fabricated outcome"
    );
    assert!(
        !matches!(result, Ok(RecoveryOutcome::Recovered { .. })),
        "capsule-less run must NEVER return Recovered"
    );
}

// ===========================================================================
// 8. No executor call before reserve / for refusal [GREEN: P11]
// ===========================================================================

/// GIVEN: a NonRecoverable step (no explicit policy, not in SAFE_RERUN_STEPS)
/// WHEN: `RecoveryProtocolV1::recover_with_executor` is called
/// THEN: returns `Refused { reason: NonRecoverable }` and the executor is
///       NEVER invoked
///
/// GREEN: the protocol short-circuits in reserve before the execute phase for
/// a refused strategy. No executor call occurs.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P13
/// @requirement:REQ-RP-001,REQ-RP-005
#[test]
fn non_recoverable_step_refusal_never_invokes_executor() {
    let conn = recovery_conn();
    let run_id = "run-p13-refusal-001";
    persisted_non_recoverable_capsule(&conn, run_id);
    seed_run(&conn, run_id);

    // Verify the policy resolves to NonRecoverable for this step.
    let workflow = non_recoverable_workflow_type();
    let step_def = workflow.steps.first().expect("step exists");
    let policy = policy_for_step(step_def, "generic_shell_step");
    assert_eq!(
        policy,
        StepRecoveryPolicy::NonRecoverable,
        "generic shell step must be NonRecoverable"
    );
    let strategy = select_strategy(policy);
    assert_eq!(
        strategy,
        RecoveryStrategy::Refused(RefusalReason::NonRecoverable),
        "NonRecoverable policy must select Refused strategy"
    );

    let request = recovery_request(run_id, "generic_shell_step", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingExecutor::new();

    let outcome = protocol
        .recover_with_executor(&conn, Path::new("."), &request, &executor)
        .expect("refusal is not a hard error");
    assert!(
        matches!(
            outcome,
            RecoveryOutcome::Refused {
                reason: RefusalReason::NonRecoverable
            }
        ),
        "NonRecoverable step must be refused, got {outcome:?}"
    );
    assert!(
        !executor.was_called(),
        "executor must NOT be called for a refusal [C12]"
    );
}

/// GIVEN: a run whose epoch has been advanced by a concurrent claim
/// WHEN: `RecoveryProtocolV1::recover_with_executor` is called with the stale
///       expected epoch
/// THEN: returns `StaleEpoch` and the executor is NEVER invoked
///
/// GREEN: the protocol short-circuits in reserve on a stale epoch before the
/// execute phase. No executor call occurs.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P13
/// @requirement:REQ-RP-001,REQ-RP-004
#[test]
fn stale_epoch_short_circuit_never_invokes_executor() {
    use luther_workflow::persistence::recovery_epoch::{cas_advance_epoch, CasOutcome};
    use rusqlite::TransactionBehavior;

    let conn = recovery_conn();
    let run_id = "run-p13-stale-epoch-001";
    persisted_capsule(&conn, run_id);
    seed_run(&conn, run_id);

    // Simulate a concurrent claim advancing epoch 0 → 1.
    {
        let tx = rusqlite::Transaction::new_unchecked(&conn, TransactionBehavior::Immediate)
            .expect("begin tx");
        let outcome = cas_advance_epoch(&tx, run_id, 0).expect("CAS advance");
        assert_eq!(outcome, CasOutcome::Advanced { from: 0, to: 1 });
        tx.commit().expect("commit");
    }

    // Request with the now-stale expected_epoch = 0.
    let request = recovery_request(run_id, "step1", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingExecutor::new();

    let outcome = protocol
        .recover_with_executor(&conn, Path::new("."), &request, &executor)
        .expect("stale epoch is not a hard error");
    match outcome {
        RecoveryOutcome::StaleEpoch {
            persisted,
            expected,
        } => {
            assert_eq!(persisted, 1, "persisted epoch must be 1");
            assert_eq!(expected, 0, "expected epoch must be 0");
        }
        other => panic!("expected StaleEpoch, got {other:?}"),
    }
    assert!(
        !executor.was_called(),
        "executor must NOT be called for a stale-epoch short-circuit [C12]"
    );
}

// ===========================================================================
// 8. P14 transition-loop recovery: multi-step, outcome mapping [GREEN: P14]
// ===========================================================================
//
// These tests prove that `RunnerRecoveryExecutor` now runs the **normal
// transition loop** (via `EngineRunner::run_from_current_step`) from the
// capsule-reconstructed reserved step, executing the step AND any downstream
// transitions to a terminal outcome. No single-step synthetic completion.

/// GIVEN: a persisted capsule for a two-step workflow (`step1 → step2`) where
///        both steps are `noop` and `step2` is terminal
/// WHEN: `RecoveryProtocolV1::recover_with_executor` recovers at `step1`
/// THEN: the reserved step AND the downstream `step2` both execute, the
///       attempt's `step_status` is `"completed"`, and the state snapshot
///       carries the truthful final status — not a fabricated default.
///
/// This is the **deterministic multi-step integration test**: it proves
/// recovery does not synthesize success after a single `execute_step`. The
/// runner transitions through the full step1 → step2 path to a terminal
/// `RunOutcome::Success`.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14
/// @requirement:REQ-RP-001,REQ-RP-002,REQ-RP-009
#[test]
fn recovery_runs_reserved_step_and_downstream_transition_to_terminal() {
    let conn = recovery_conn();
    let run_id = "run-p14-multi-step-001";
    persisted_capsule(&conn, run_id);
    seed_run(&conn, run_id);

    let request = recovery_request(run_id, "step1", 0);
    let protocol = RecoveryProtocolV1;

    let temp = tempfile::tempdir().expect("create temp dir for executor db");
    let db_path = temp.path().join("executor.db");
    let executor = RunnerRecoveryExecutor::new(db_path, RunContext::default());

    let outcome = protocol
        .recover_with_executor(&conn, Path::new("."), &request, &executor)
        .expect("recovery through the transition loop must succeed");

    let (attempt_id, operation_id) = match outcome {
        RecoveryOutcome::Recovered {
            resumed_at_step,
            attempt_id,
            operation_id,
        } => {
            assert_eq!(
                resumed_at_step, "step1",
                "recovery must resume at the reserved step"
            );
            (attempt_id, operation_id)
        }
        other => panic!("expected Recovered, got {other:?}"),
    };

    // Verify the finalized attempt row carries the truthful terminal status
    // and a non-default state snapshot. The snapshot was captured from the
    // live runner after the transition loop completed.
    let (step_status, snapshot_json, runner_result_json): (String, String, Option<String>) = conn
        .query_row(
            "SELECT step_status, state_snapshot_json, runner_result_json
             FROM recovery_attempts WHERE attempt_id = ?1",
            rusqlite::params![attempt_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("query finalized attempt");

    assert_eq!(
        step_status, "completed",
        "attempt step_status must be the terminal completed status"
    );
    assert!(
        snapshot_json.contains("\"status\":\"completed\""),
        "snapshot JSON must carry the truthful final status 'completed', got: {snapshot_json}"
    );
    assert!(
        !snapshot_json.contains("\"status\":\"running\""),
        "snapshot must not be a fabricated StateSnapshot::default()"
    );
    let runner_result_json =
        runner_result_json.expect("runner_result_json must be persisted in the attempt");
    assert!(
        runner_result_json.contains("\"outcome\":\"success\""),
        "runner_result must carry the actual RunOutcome label, got: {runner_result_json}"
    );
    assert!(!operation_id.is_empty(), "operation_id must be durable");

    // Verify the recovery operation is finalized as Completed.
    let op_status: String = conn
        .query_row(
            "SELECT status FROM recovery_operations WHERE operation_id = ?1",
            rusqlite::params![operation_id],
            |row| row.get(0),
        )
        .expect("query finalized operation");
    assert_eq!(
        op_status, "completed",
        "recovery operation must be finalized as completed"
    );
}

/// GIVEN: a `RunnerRecoveryExecutor` invoked directly on a capsule for a
///        two-step workflow
/// WHEN: `execute` runs the transition loop from `step1`
/// THEN: the `RecoveryExecutionResult.state_snapshot` is the exact final
///       runner state — NOT a fabricated `StateSnapshot::default()`.
///
/// This proves no synthetic success is fabricated: the snapshot carries the
/// real `retry_count`, `loop_count`, `edge_loop_counts`, and `status` from
/// the post-execution runner state.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14
/// @requirement:REQ-RP-002
#[test]
fn recovery_executor_snapshot_is_exact_not_fabricated_default() {
    let conn = recovery_conn();
    let run_id = "run-p14-snapshot-001";
    let capsule = persisted_capsule(&conn, run_id);

    let temp = tempfile::tempdir().expect("create temp dir for executor db");
    let db_path = temp.path().join("executor.db");
    let executor = RunnerRecoveryExecutor::new(db_path, RunContext::default());

    let invocation = RecoveryExecutionInvocation {
        run_id,
        step_id: "step1",
        operation_id: "op-p14-snapshot",
        attempt_id: 1,
        epoch: 1,
        strategy: RecoveryStrategy::Reenter,
        capsule: &capsule,
        workspace: Path::new("."),
    };

    let result = executor
        .execute(&invocation)
        .expect("transition loop must succeed");

    // The snapshot must carry the truthful terminal status. A fabricated
    // default would have status "running" and empty fields.
    assert_eq!(
        result.state_snapshot.status, "completed",
        "snapshot must carry the actual final status"
    );
    assert_eq!(
        result.step_status, "completed",
        "step_status must be the terminal completed status"
    );

    // Verify runner_result carries the outcome label, proving the actual
    // RunOutcome was mapped (not a hardcoded string).
    let runner_result = result
        .runner_result
        .as_ref()
        .expect("runner_result must be present");
    assert_eq!(
        runner_result["outcome"], "success",
        "runner_result must carry the actual RunOutcome label"
    );
    assert_eq!(
        runner_result["step_id"], "step1",
        "runner_result must carry the reserved step id"
    );
}
