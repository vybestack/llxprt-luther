//! Atomic fresh-launch persistence tests (M2 closure).
//!
//! These tests exercise the **real durable store (SQLite)** directly for the
//! atomic launch persistence API
//! ([`persist_launch_atomically`](luther_workflow::persistence::persist_launch_atomically))
//! and the production fresh-launch callers that use it. They verify that the
//! initial `Starting` `RunMetadata` and the immutable `ExecutionCapsuleV1` are
//! persisted in **one** `IMMEDIATE` transaction, with full rollback on any
//! failure (run collision, capsule collision, injected/constraint capsule
//! failure).
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08B
//! @requirement:REQ-RP-002

use std::collections::HashMap;
use std::path::Path;

use rusqlite::Connection;

use luther_workflow::engine::recovery::capsule::{
    build_capsule_v1, verify_envelope_digest, CapsuleError, ExecutionCapsuleV1,
};
use luther_workflow::persistence::capsule_store::{
    init_capsules_table, load_capsule_v1, persist_capsule_v1, persist_launch_atomically,
    LaunchPersistenceError, LaunchPersistenceOutcome,
};
use luther_workflow::persistence::launch_provenance::LaunchProvenance;
use luther_workflow::persistence::sqlite::{init_runs_schema, persist_run_with_conn};
use luther_workflow::persistence::{
    get_run_with_conn, RunMetadata, RunStatus, EXECUTION_CAPSULES_TABLE,
};
use luther_workflow::workflow::schema::{
    DiffPathNormalization, GuardConfig, GuardLimits, ParentOrchestrationConfig, RepoConfig,
    RuntimeConfig, StepDef, TransitionDef, WorkflowConfig, WorkflowType,
};

// ===========================================================================
// Test helpers
// ===========================================================================

fn sample_workflow_type() -> WorkflowType {
    WorkflowType {
        workflow_type_id: "atomic-launch-test".to_string(),
        steps: vec![StepDef {
            step_id: "step1".to_string(),
            step_type: "noop".to_string(),
            description: None,
            parameters: None,
            produces: None,
            consumes: None,
            terminal: None,
            recovery_policy: None,
        }],
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

fn sample_config() -> WorkflowConfig {
    WorkflowConfig {
        config_id: "atomic-launch-test-config".to_string(),
        workflow_type_id: "atomic-launch-test".to_string(),
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

fn sample_provenance() -> LaunchProvenance {
    LaunchProvenance::from_resolved(&sample_workflow_type(), &sample_config(), Path::new("."))
        .expect("canonicalize '.'")
}

/// Create an in-memory SQLite connection with both the runs and capsules
/// tables initialized.
fn initialized_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    init_runs_schema(&conn).expect("init runs schema");
    init_capsules_table(&conn).expect("init capsules table");
    conn
}

/// Build a capsule for the given run_id, using the current directory as
/// config_root.
fn build_test_capsule(run_id: &str) -> ExecutionCapsuleV1 {
    let workflow = sample_workflow_type();
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

/// Build a `RunMetadata` for the given run_id in `Starting` state.
fn build_starting_metadata(run_id: &str) -> RunMetadata {
    let mut metadata = RunMetadata::new(run_id, "atomic-launch-test", "atomic-launch-test-config");
    metadata.status = RunStatus::Starting;
    metadata.current_step = Some("step1".to_string());
    metadata
}

/// Count capsule rows for a run_id (for verifying rollback).
fn count_capsule_rows(conn: &Connection, run_id: &str) -> i64 {
    conn.query_row(
        &format!("SELECT COUNT(*) FROM {EXECUTION_CAPSULES_TABLE} WHERE run_id = ?1"),
        rusqlite::params![run_id],
        |row| row.get(0),
    )
    .expect("count capsule rows")
}

/// Count run rows for a run_id (for verifying rollback).
fn count_run_rows(conn: &Connection, run_id: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM runs WHERE run_id = ?1",
        rusqlite::params![run_id],
        |row| row.get(0),
    )
    .expect("count run rows")
}

// ===========================================================================
// 1. Successful atomic pair
// ===========================================================================

/// GIVEN: an empty initialized database
/// WHEN: `persist_launch_atomically` is called with valid metadata + capsule
/// THEN: both the run row and capsule row are persisted and queryable.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08B
/// @requirement:REQ-RP-002
#[test]
fn successful_atomic_pair_inserts_run_and_capsule() {
    let conn = initialized_conn();
    let run_id = "run-atomic-001";
    let metadata = build_starting_metadata(run_id);
    let capsule = build_test_capsule(run_id);

    let outcome = persist_launch_atomically(&conn, &metadata, &capsule)
        .expect("atomic launch persistence must succeed");
    assert_eq!(outcome, LaunchPersistenceOutcome::Persisted);

    // The run row exists.
    let loaded_run = get_run_with_conn(&conn, run_id)
        .expect("query run")
        .expect("run row must exist");
    assert_eq!(loaded_run.run_id, run_id);
    assert_eq!(loaded_run.status, RunStatus::Starting);

    // The capsule row exists.
    let loaded_capsule = load_capsule_v1(&conn, run_id).expect("capsule must exist");
    assert_eq!(loaded_capsule.run_id, run_id);
    verify_envelope_digest(&loaded_capsule).expect("capsule envelope must verify");
}

// ===========================================================================
// 2. Run collision rollback
// ===========================================================================

/// GIVEN: a database with a pre-existing run row for the same run_id
/// WHEN: `persist_launch_atomically` is called
/// THEN: returns `RunCollision`, and NO capsule row is left behind.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08B
/// @requirement:REQ-RP-002
#[test]
fn run_collision_rolls_back_capsule() {
    let conn = initialized_conn();
    let run_id = "run-atomic-collision-001";

    // Pre-insert a run row so the launch insert collides.
    let existing = build_starting_metadata(run_id);
    persist_run_with_conn(&conn, &existing).expect("seed existing run");

    let metadata = build_starting_metadata(run_id);
    let capsule = build_test_capsule(run_id);

    let error =
        persist_launch_atomically(&conn, &metadata, &capsule).expect_err("collision must error");
    assert!(
        matches!(error, LaunchPersistenceError::RunCollision(ref id) if id == run_id),
        "expected RunCollision, got {error:?}"
    );

    // No capsule should be left behind.
    assert_eq!(
        count_capsule_rows(&conn, run_id),
        0,
        "capsule must not exist after run collision rollback"
    );
    // The original run row is preserved.
    assert_eq!(
        count_run_rows(&conn, run_id),
        1,
        "original run row must be preserved"
    );
}

// ===========================================================================
// 3. Capsule collision rollback
// ===========================================================================

/// GIVEN: a database with a pre-existing capsule for the same run_id but no
///        run row (simulating a prior partial-state or foreign capsule)
/// WHEN: `persist_launch_atomically` is called
/// THEN: returns `CapsuleCollision`, and NO run row is left behind.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08B
/// @requirement:REQ-RP-002
#[test]
fn capsule_collision_rolls_back_run() {
    let conn = initialized_conn();
    let run_id = "run-atomic-collision-002";

    // Pre-insert a capsule so the capsule insert collides.
    let existing_capsule = build_test_capsule(run_id);
    persist_capsule_v1(&conn, &existing_capsule).expect("seed existing capsule");

    let metadata = build_starting_metadata(run_id);
    let capsule = build_test_capsule(run_id);

    let error = persist_launch_atomically(&conn, &metadata, &capsule)
        .expect_err("capsule collision must error");
    assert!(
        matches!(error, LaunchPersistenceError::CapsuleCollision(ref id) if id == run_id),
        "expected CapsuleCollision, got {error:?}"
    );

    // No run row should be left behind.
    assert_eq!(
        count_run_rows(&conn, run_id),
        0,
        "run must not exist after capsule collision rollback"
    );
    // The original capsule is preserved.
    assert_eq!(
        count_capsule_rows(&conn, run_id),
        1,
        "original capsule must be preserved"
    );
}

// ===========================================================================
// 4. Injected capsule failure rollback (envelope-digest verification failure)
// ===========================================================================

/// GIVEN: a capsule with a tampered envelope digest (fails verification)
/// WHEN: `persist_launch_atomically` is called
/// THEN: the capsule insert fails, the transaction is rolled back, and NO run
///       row is left behind.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08B
/// @requirement:REQ-RP-002
#[test]
fn injected_capsule_failure_rolls_back_run() {
    let conn = initialized_conn();
    let run_id = "run-atomic-injected-001";

    // Build a valid capsule then tamper the envelope digest so verification
    // fails inside persist_capsule_v1.
    let mut capsule = build_test_capsule(run_id);
    capsule.envelope_digest =
        "0000000000000000000000000000000000000000000000000000000000000000".to_string();
    // Verify it would fail verification.
    assert_eq!(
        verify_envelope_digest(&capsule),
        Err(CapsuleError::EnvelopeDigestMismatch),
        "tampered capsule must fail verification"
    );

    let metadata = build_starting_metadata(run_id);

    let error = persist_launch_atomically(&conn, &metadata, &capsule)
        .expect_err("injected failure must error");
    assert!(
        matches!(error, LaunchPersistenceError::Database { .. }),
        "expected Database error for injected capsule failure, got {error:?}"
    );

    // No run row should be left behind.
    assert_eq!(
        count_run_rows(&conn, run_id),
        0,
        "run must not exist after injected capsule failure rollback"
    );
    // No capsule row should be left behind.
    assert_eq!(
        count_capsule_rows(&conn, run_id),
        0,
        "capsule must not exist after injected capsule failure rollback"
    );
}

// ===========================================================================
// 5. Constraint capsule failure rollback (PRIMARY KEY on capsule)
// ===========================================================================

/// GIVEN: a capsule that collides with an existing capsule AND a run row that
///        does NOT collide (so the run insert succeeds but the capsule insert
///        hits a PRIMARY KEY constraint)
/// WHEN: `persist_launch_atomically` is called
/// THEN: returns `CapsuleCollision`, and the run row that was inserted inside
///       the transaction is rolled back (no run row exists).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08B
/// @requirement:REQ-RP-002
#[test]
fn constraint_capsule_failure_rolls_back_run() {
    let conn = initialized_conn();
    let run_id = "run-atomic-constraint-001";

    // Pre-insert ONLY a capsule (no run row). This means the run insert
    // succeeds inside the transaction, but the capsule insert hits the PRIMARY
    // KEY constraint. The run insert must be rolled back.
    let existing_capsule = build_test_capsule(run_id);
    persist_capsule_v1(&conn, &existing_capsule).expect("seed existing capsule");
    assert_eq!(count_run_rows(&conn, run_id), 0, "no run row before launch");

    let metadata = build_starting_metadata(run_id);
    let capsule = build_test_capsule(run_id);

    let error = persist_launch_atomically(&conn, &metadata, &capsule)
        .expect_err("constraint failure must error");
    assert!(
        matches!(error, LaunchPersistenceError::CapsuleCollision(ref id) if id == run_id),
        "expected CapsuleCollision for constraint failure, got {error:?}"
    );

    // The run row inserted inside the transaction must have been rolled back.
    assert_eq!(
        count_run_rows(&conn, run_id),
        0,
        "run must not exist after constraint capsule failure rollback"
    );
    // The original capsule is preserved.
    assert_eq!(count_capsule_rows(&conn, run_id), 1);
}

// ===========================================================================
// 6. Fresh-launch caller tests (CLI, daemon, child)
// ===========================================================================

/// GIVEN: a fresh `EngineRunner::with_db_path_for_launch` call with a valid
///        capsule
/// WHEN: the runner is constructed
/// THEN: both the run row and the capsule row exist in the database.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08B
/// @requirement:REQ-RP-002
#[test]
fn fresh_launch_caller_persists_run_and_capsule_atomically() {
    use luther_workflow::engine::executor::ExecutorRegistry;
    use luther_workflow::engine::instance::WorkflowInstance;
    use luther_workflow::engine::runner::EngineRunner;
    use luther_workflow::engine::RunContext;
    use luther_workflow::persistence::{get_run_with_conn, RunStatus};

    let temp = tempfile::tempdir().expect("create temp dir");
    let db_path = temp.path().join("test.db");
    luther_workflow::persistence::init_database(&db_path).expect("init database");

    let run_id = "run-launch-caller-001";
    let workflow = sample_workflow_type();
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
    let run_context = RunContext::default();
    let runner =
        EngineRunner::with_db_path_for_launch(instance, registry, &db_path, run_context, capsule)
            .expect("fresh launch must succeed");
    assert_eq!(runner.run_id(), run_id);

    // Verify both rows exist.
    let conn = Connection::open(&db_path).expect("open db");
    let run = get_run_with_conn(&conn, run_id)
        .expect("query run")
        .expect("run row must exist");
    assert_eq!(run.status, RunStatus::Starting);

    let capsule = load_capsule_v1(&conn, run_id).expect("capsule must exist");
    assert_eq!(capsule.run_id, run_id);
}

/// GIVEN: a database with a pre-existing run row for the same run_id
/// WHEN: `EngineRunner::with_db_path_for_launch` is called with a capsule
/// THEN: the constructor returns an error (run collision), and NO capsule is
///       left behind.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08B
/// @requirement:REQ-RP-002
#[test]
fn fresh_launch_caller_collision_leaves_no_capsule() {
    use luther_workflow::engine::executor::ExecutorRegistry;
    use luther_workflow::engine::instance::WorkflowInstance;
    use luther_workflow::engine::runner::EngineRunner;
    use luther_workflow::engine::RunContext;
    use luther_workflow::persistence::persist_run_with_conn;

    let temp = tempfile::tempdir().expect("create temp dir");
    let db_path = temp.path().join("test.db");
    luther_workflow::persistence::init_database(&db_path).expect("init database");

    let run_id = "run-launch-caller-collision-001";
    // Seed an existing run row.
    let existing = build_starting_metadata(run_id);
    let conn = Connection::open(&db_path).expect("open db");
    persist_run_with_conn(&conn, &existing).expect("seed existing run");
    drop(conn);

    let workflow = sample_workflow_type();
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
    let run_context = RunContext::default();
    let result =
        EngineRunner::with_db_path_for_launch(instance, registry, &db_path, run_context, capsule);
    let error = match result {
        Ok(_) => panic!("collision must error"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("launch collision"),
        "expected collision error, got: {error}"
    );

    // No capsule should be left behind.
    let conn = Connection::open(&db_path).expect("open db");
    assert_eq!(
        count_capsule_rows(&conn, run_id),
        0,
        "no capsule after collision"
    );
}
