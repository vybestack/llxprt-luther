//! Integration-first tests for `RecoveryProtocolV1::recover()` and the
//! step recovery policy/strategy selection.
//!
//! These tests are the **RED phase** for P10 (Milestone 3). They exercise the
//! protocol's public API and its interaction with the **durable persistence
//! layer (real SQLite)** — the epoch CAS, operation ledger, append-only
//! attempt store, and immutable capsule store landed in P05/P08. No in-memory
//! facade, no private-field construction, no mocks that bypass authority.
//!
//! ## RED/green split
//!
//! - **Tests calling `RecoveryProtocolV1::recover()`** assert the *expected*
//!   P11 contract (e.g. `Recovered`, `AlreadyApplied`, `StaleEpoch`). Since
//!   `recover()` is `todo!()`, these tests **fail naturally** at the panic —
//!   valid red exclusively at the designated P11 behavior.
//! - **Policy/strategy tests** exercise the fail-closed stubs and assert those
//!   defaults hold (passing — demonstrating fail-closed).
//! - **Durable-store tests** exercise the real SQLite stores landed in P05/P08
//!   (passing — verifying the durable invariants the protocol consumes).
//!
//! Tests do not expect panics, use reverse assertions, or swallow designated
//! red failures.
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{Duration, Utc};
use rusqlite::{params, Connection, Transaction, TransactionBehavior};

use luther_workflow::engine::recovery::capsule::{
    build_capsule_v1, verify_envelope_digest, ExecutionCapsuleV1,
};
use luther_workflow::engine::recovery::policy::{
    policy_for_step, select_strategy, StepRecoveryPolicy,
};
use luther_workflow::engine::recovery::protocol::{
    normalize_operator_verb, OperatorVerb, RecoveryError, RecoveryExecutionError,
    RecoveryExecutionInvocation, RecoveryExecutionResult, RecoveryExecutor, RecoveryOutcome,
    RecoveryPhaseObserver, RecoveryProtocolV1, RecoveryRequest, RecoveryStrategy, RefusalReason,
};
use luther_workflow::engine::workspace_ownership::provision_workspace_ownership;
use luther_workflow::persistence::attempts::{
    init_attempts_table, load_unfinalized_for_operation, record_attempt_start, AttemptStart,
};
use luther_workflow::persistence::capsule_store::{init_capsules_table, persist_capsule_v1};
use luther_workflow::persistence::checkpoint::{
    save_checkpoint_with_conn, Checkpoint, StateSnapshot,
};
use luther_workflow::persistence::effect_intents::init_effect_intents_table;
use luther_workflow::persistence::recovery_epoch::{
    cas_advance_epoch, init_epoch_table, read_epoch,
};
use luther_workflow::persistence::recovery_operations::{
    compute_intent_digest, compute_logical_request_key, compute_operation_id,
    init_operations_table, insert_pending, lookup_logical_operation, try_adopt_pending,
    AdoptOutcome, OperationStatus, PendingOperationInsert,
};
use luther_workflow::persistence::sqlite::{init_runs_schema, persist_run_with_conn};
use luther_workflow::persistence::wait_state::{upsert_wait_state, WaitKind, WaitStateRecord};
use luther_workflow::persistence::{
    create_lease, get_lease_for_run, IssueLease, LeaseStatus, RunMetadata, RunStatus,
};
use luther_workflow::workflow::schema::{
    DiffPathNormalization, GuardConfig, GuardLimits, ParentOrchestrationConfig, RepoConfig,
    RuntimeConfig, StepDef, TransitionDef, WorkflowConfig, WorkflowType,
};

// ===========================================================================
// Test helpers
// ===========================================================================

/// Create an in-memory SQLite connection with ALL recovery tables initialized.
///
/// Mirrors `init_database` but uses an in-memory connection for test speed.
/// These are the real durable stores — no in-memory facade.
fn recovery_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    init_epoch_table(&conn).expect("init epoch table");
    init_operations_table(&conn).expect("init operations table");
    init_attempts_table(&conn).expect("init attempts table");
    init_effect_intents_table(&conn).expect("init effect intents table");
    init_capsules_table(&conn).expect("init capsules table");
    init_runs_schema(&conn).expect("init runs schema");
    // The P11 authority snapshot loads checkpoint/wait-state/lease rows, so
    // those real durable tables must exist for the protocol's prepare phase.
    luther_workflow::persistence::checkpoint::init_checkpoint_table(&conn)
        .expect("init checkpoint table");
    luther_workflow::persistence::wait_state::init_wait_states_table(&conn)
        .expect("init wait states table");
    luther_workflow::persistence::leases::init_leases_table(&conn).expect("init leases table");
    conn
}

/// Begin an IMMEDIATE transaction (mirrors the protocol's reserve/finalize tx).
fn begin_tx(conn: &Connection) -> Transaction<'_> {
    Transaction::new_unchecked(conn, TransactionBehavior::Immediate).expect("begin IMMEDIATE tx")
}

/// A minimal `WorkflowType` for capsule/policy construction.
fn sample_workflow_type() -> WorkflowType {
    WorkflowType {
        workflow_type_id: "recovery-integration-test".to_string(),
        steps: vec![StepDef {
            step_id: "step1".to_string(),
            step_type: "noop".to_string(),
            description: None,
            parameters: None,
            produces: None,
            consumes: None,
            terminal: None,
            recovery_policy: Some(StepRecoveryPolicy::PureReenter),
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

/// A `WorkflowType` with a step declaring `ContinueWorkspace`.
fn workflow_with_continue_workspace_step() -> WorkflowType {
    WorkflowType {
        workflow_type_id: "recovery-continue-test".to_string(),
        steps: vec![StepDef {
            step_id: "continue_step".to_string(),
            step_type: "shell".to_string(),
            description: None,
            parameters: None,
            produces: None,
            consumes: None,
            terminal: None,
            recovery_policy: Some(StepRecoveryPolicy::ContinueWorkspace),
        }],
        transitions: vec![],
        guards: GuardConfig {
            max_retries: None,
            timeout_seconds: None,
            require_approval: None,
        },
    }
}

/// A minimal `WorkflowConfig` for capsule construction.
fn sample_config() -> WorkflowConfig {
    WorkflowConfig {
        config_id: "recovery-integration-test-config".to_string(),
        workflow_type_id: "recovery-integration-test".to_string(),
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

/// Construct a `LaunchProvenance` from the given workflow/config using the
/// given config_root.
fn provenance_for(
    workflow: &WorkflowType,
    config: &WorkflowConfig,
    config_root: &Path,
) -> luther_workflow::persistence::launch_provenance::LaunchProvenance {
    luther_workflow::persistence::launch_provenance::LaunchProvenance::from_resolved(
        workflow,
        config,
        config_root,
    )
    .expect("canonicalize config_root")
}

/// Construct a `LaunchProvenance` from the current directory.
fn sample_provenance() -> luther_workflow::persistence::launch_provenance::LaunchProvenance {
    provenance_for(&sample_workflow_type(), &sample_config(), Path::new("."))
}

/// Build a capsule for the given run_id using the current directory as
/// config_root and the sample (step1) workflow.
fn build_test_capsule(run_id: &str) -> ExecutionCapsuleV1 {
    let workflow = sample_workflow_type();
    let config = sample_config();
    let provenance = provenance_for(&workflow, &config, Path::new("."));
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

/// Build a capsule for the given run_id using the given workflow type and
/// config_root. The config is derived from the given workflow's
/// `workflow_type_id`.
fn build_capsule_with_workflow(
    run_id: &str,
    workflow: &WorkflowType,
    config_root: &Path,
) -> ExecutionCapsuleV1 {
    let config = config_for_workflow(&workflow.workflow_type_id);
    let provenance = provenance_for(workflow, &config, config_root);
    build_capsule_v1(
        run_id.to_string(),
        workflow,
        &config,
        config_root,
        &provenance,
        "main".to_string(),
    )
    .expect("build capsule")
}

/// Build a capsule whose canonical workflow includes `continue_step`
/// (declaring `ContinueWorkspace` policy), using the given config_root.
fn build_continue_capsule(run_id: &str, config_root: &Path) -> ExecutionCapsuleV1 {
    let workflow = workflow_with_continue_workspace_step();
    build_capsule_with_workflow(run_id, &workflow, config_root)
}

/// Build and persist a capsule (step1 workflow), returning the built capsule.
fn persisted_capsule(conn: &Connection, run_id: &str) -> ExecutionCapsuleV1 {
    let capsule = build_test_capsule(run_id);
    persist_capsule_v1(conn, &capsule).expect("persist capsule");
    capsule
}

/// Build and persist a capsule whose canonical workflow includes
/// `continue_step`, returning the built capsule.
fn persisted_continue_capsule(
    conn: &Connection,
    run_id: &str,
    config_root: &Path,
) -> ExecutionCapsuleV1 {
    let capsule = build_continue_capsule(run_id, config_root);
    persist_capsule_v1(conn, &capsule).expect("persist capsule");
    capsule
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

/// Create an owned temp workspace directory provisioned with durable
/// workspace-ownership evidence for the given run_id. [B6]
///
/// The workspace directory is atomically created by the provisioning function
/// itself (inside a temp parent), so `bootstrap_workspace_creation` observes
/// the atomic `create_dir` and permits first-claim. Bootstrap evidence
/// (`.luther/workspace-owner`) is established, then `.git` is created and the
/// bootstrap claim is promoted to the durable `.git/luther/workspace-owner`
/// path. Returned as a `(TempDir, PathBuf)` pair where the `TempDir` is the
/// parent and the `PathBuf` is the workspace path beneath it.
fn owned_workspace(run_id: &str) -> (tempfile::TempDir, PathBuf) {
    let parent = tempfile::tempdir().expect("create temp parent");

    // The workspace subdirectory does NOT exist yet: provision will atomically
    // create it via `bootstrap_workspace_creation`, observing first-creation.
    let workspace_path = parent.path().join("ws");

    // Provision bootstrap evidence (creates the workspace dir + marker).
    provision_workspace_ownership(&workspace_path, run_id)
        .expect("provision bootstrap workspace ownership");

    // Create .git so the next provision promotes to the durable path.
    std::fs::create_dir_all(workspace_path.join(".git/luther")).expect("create .git/luther dir");

    // Promote bootstrap evidence to the durable marker path.
    provision_workspace_ownership(&workspace_path, run_id)
        .expect("promote to durable workspace ownership");

    // The durable marker must exist after provisioning.
    assert!(
        workspace_path.join(".git/luther/workspace-owner").exists(),
        "durable workspace-owner marker must exist after provisioning"
    );
    (parent, workspace_path)
}

/// Type alias for the between-phase hook closure. [B1/B6]
type PhaseHook = Arc<dyn Fn(&Connection) + Send + Sync>;

/// A deterministic [`RecoveryPhaseObserver`] that invokes a closure between
/// prepare and reserve, enabling tests to simulate TOCTOU or authority changes
/// between phases. [B1/B6]
///
/// The closure receives a mutable reference to the [`Connection`] so it can
/// mutate durable state between protocol phases. This is an architecturally
/// clean test seam — production code uses [`NoOpRecoveryPhaseObserver`].
#[derive(Clone)]
struct PhaseHookObserver {
    hook: Arc<Mutex<Option<PhaseHook>>>,
}

impl PhaseHookObserver {
    fn new() -> Self {
        Self {
            hook: Arc::new(Mutex::new(None)),
        }
    }

    /// Set the between-phase hook closure. The closure is invoked with the
    /// connection after prepare and before reserve.
    fn set_hook<F>(&self, hook: F)
    where
        F: Fn(&Connection) + Send + Sync + 'static,
    {
        *self.hook.lock().unwrap() = Some(Arc::new(hook));
    }
}

impl std::fmt::Debug for PhaseHookObserver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhaseHookObserver").finish()
    }
}

impl RecoveryPhaseObserver for PhaseHookObserver {
    fn on_prepare_complete(&self, conn: &Connection, _request: &RecoveryRequest) {
        if let Some(hook) = self.hook.lock().unwrap().as_ref() {
            hook(conn);
        }
    }
}

/// A deterministic [`RecoveryExecutor`] that records invocations and returns
/// a configured success result. [C5/C12]
///
/// Used by expected-success tests to assert the executor was called with the
/// reserved durable ids (epoch/attempt) **after** reserve committed. The
/// refusal/stale/duplicate tests assert it was **not** called. [C12]
#[derive(Clone)]
struct RecordingRecoveryExecutor {
    calls: Arc<Mutex<Vec<RecordedExecutionCall>>>,
    result: Arc<Mutex<RecoveryExecutionResult>>,
}

/// A single recorded executor invocation for assertions.
#[derive(Clone)]
struct RecordedExecutionCall {
    run_id: String,
    step_id: String,
    operation_id: String,
    attempt_id: i64,
    epoch: u64,
}

impl std::fmt::Debug for RecordingRecoveryExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordingRecoveryExecutor").finish()
    }
}

impl RecordingRecoveryExecutor {
    /// Create a recording executor that returns a success result by default.
    fn success() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            result: Arc::new(Mutex::new(default_success_result())),
        }
    }

    /// Whether the executor was invoked at least once.
    fn was_called(&self) -> bool {
        !self.calls.lock().unwrap().is_empty()
    }

    /// The first recorded invocation, if any.
    fn first_call(&self) -> Option<RecordedExecutionCall> {
        self.calls.lock().unwrap().first().cloned()
    }
}

/// Build a truthful success result with a completed step status. [C12]
fn default_success_result() -> RecoveryExecutionResult {
    RecoveryExecutionResult {
        step_status: "completed".to_string(),
        state_snapshot: StateSnapshot {
            status: "completed".to_string(),
            ..StateSnapshot::default()
        },
        runner_result: Some(serde_json::json!({"status": "success"})),
    }
}

impl RecoveryExecutor for RecordingRecoveryExecutor {
    fn execute(
        &self,
        invocation: &RecoveryExecutionInvocation<'_>,
    ) -> Result<RecoveryExecutionResult, RecoveryExecutionError> {
        self.calls.lock().unwrap().push(RecordedExecutionCall {
            run_id: invocation.run_id.to_string(),
            step_id: invocation.step_id.to_string(),
            operation_id: invocation.operation_id.to_string(),
            attempt_id: invocation.attempt_id,
            epoch: invocation.epoch,
        });
        Ok(self.result.lock().unwrap().clone())
    }
}

/// Build a `RunMetadata` for the given run_id in `Starting` state.
fn starting_metadata(run_id: &str) -> RunMetadata {
    let mut metadata = RunMetadata::new(
        run_id,
        "recovery-integration-test",
        "recovery-integration-test-config",
    );
    metadata.status = RunStatus::Starting;
    metadata.current_step = Some("step1".to_string());
    metadata
}

/// Persist a `Starting` run row.
fn seed_run(conn: &Connection, run_id: &str) {
    let metadata = starting_metadata(run_id);
    persist_run_with_conn(conn, &metadata).expect("persist run metadata");
}

/// A minimal `StateSnapshot` with scalar fields set and empty maps.
fn test_snapshot(status: &str) -> StateSnapshot {
    StateSnapshot {
        retry_count: 0,
        loop_count: 0,
        edge_loop_counts: HashMap::new(),
        context: HashMap::new(),
        status: status.to_string(),
    }
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

/// Count rows in `recovery_operations` for a given run.
fn count_operations(conn: &Connection, run_id: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM recovery_operations WHERE run_id = ?1",
        params![run_id],
        |row| row.get(0),
    )
    .expect("count operations")
}

/// Seed a recovery operation via raw SQL.
struct SeedOperation {
    operation_id: String,
    run_id: String,
    epoch: u64,
    step_id: String,
    capsule_envelope_digest: String,
    source_attempt_id: Option<i64>,
    logical_request_key: String,
    intent_digest: String,
    status: String,
    owner_pid: Option<u32>,
    lease_expires_at: Option<String>,
    execution_attempt_id: Option<i64>,
    serialized_outcome: Option<String>,
}

fn seed_operation(conn: &Connection, op: &SeedOperation) {
    conn.execute(
        "INSERT INTO recovery_operations
             (operation_id, run_id, epoch, step_id, capsule_envelope_digest,
              source_attempt_id, logical_request_key, intent_digest, status,
              owner_pid, lease_expires_at, execution_attempt_id, serialized_outcome,
              created_at, finalized_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            op.operation_id,
            op.run_id,
            op.epoch as i64,
            op.step_id,
            op.capsule_envelope_digest,
            op.source_attempt_id,
            op.logical_request_key,
            op.intent_digest,
            op.status,
            op.owner_pid.map(|p| p as i64),
            op.lease_expires_at,
            op.execution_attempt_id,
            op.serialized_outcome,
            Utc::now().to_rfc3339(),
            if op.status == "pending" {
                None
            } else {
                Some(Utc::now().to_rfc3339())
            },
        ],
    )
    .expect("seed operation");
}

// ===========================================================================
// REQ-RP-001 / REQ-RP-004: Fresh recovery [C5/B2]
// ===========================================================================

/// GIVEN: a persisted Starting run with epoch 0 and a valid capsule
/// WHEN: `RecoveryProtocolV1::recover(request{step})` is called
/// THEN: returns `RecoveryOutcome::Recovered { resumed_at_step, attempt_id,
///       operation_id }` [C5/B2]
///
/// RED: `recover()` is `todo!()` (P11 owns behavior). The assertion on the
/// expected `Recovered` outcome fails naturally at the panic.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-001
#[test]
fn fresh_recovery_with_valid_epoch_returns_recovered() {
    let conn = recovery_conn();
    let run_id = "run-fresh-001";
    seed_run(&conn, run_id);
    persisted_capsule(&conn, run_id);

    let request = recovery_request(run_id, "step1", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingRecoveryExecutor::success();

    let outcome = protocol
        .recover_with_executor(&conn, Path::new("."), &request, &executor)
        .expect("recover must succeed for a valid fresh request [C5/B2]");
    assert!(
        matches!(outcome, RecoveryOutcome::Recovered { .. }),
        "fresh recovery with valid epoch must return Recovered [C5/B2]"
    );
    assert!(
        executor.was_called(),
        "executor must be called for an executable strategy [C12]"
    );
    let call = executor
        .first_call()
        .expect("executor must record the invocation [C12]");
    assert_eq!(call.run_id, run_id);
    assert_eq!(call.step_id, "step1");
    assert_eq!(
        call.epoch, 1,
        "executor must see the reserved (advanced) epoch [B2]"
    );
    assert!(
        call.attempt_id > 0,
        "executor must see a durable attempt id allocated at reserve [B4]"
    );
    assert!(
        !call.operation_id.is_empty(),
        "executor must see the reserved operation id [B3]"
    );
}

// ===========================================================================
// REQ-RP-004: Stale epoch [C1/B2]
// ===========================================================================

/// GIVEN: a run with persisted epoch E
/// WHEN: `recover(request)` is called after epoch has advanced (stale)
/// THEN: returns `RecoveryOutcome::StaleEpoch { persisted, expected }` [C1/B2]
///
/// RED: `recover()` is `todo!()` (P11 owns behavior). The assertion on the
/// expected `StaleEpoch` outcome fails naturally at the panic.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-004
#[test]
fn stale_epoch_returns_stale_epoch_outcome() {
    let conn = recovery_conn();
    let run_id = "run-stale-epoch";
    seed_run(&conn, run_id);
    persisted_capsule(&conn, run_id);

    // Simulate a concurrent claim advancing epoch 0 → 1.
    {
        let tx = begin_tx(&conn);
        let outcome = cas_advance_epoch(&tx, run_id, 0).expect("CAS advance");
        assert_eq!(
            outcome,
            luther_workflow::persistence::recovery_epoch::CasOutcome::Advanced { from: 0, to: 1 }
        );
        tx.commit().expect("commit");
    }

    let persisted = read_epoch(&conn, run_id).expect("read_epoch");
    assert_eq!(
        persisted, 1,
        "epoch must be advanced to 1 by concurrent claim"
    );

    let request = recovery_request(run_id, "step1", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingRecoveryExecutor::success();

    let outcome = protocol
        .recover_with_executor(&conn, Path::new("."), &request, &executor)
        .expect("recover must not hard-error on stale epoch [C1]");
    match outcome {
        RecoveryOutcome::StaleEpoch {
            persisted: p,
            expected: e,
        } => {
            assert_eq!(p, 1, "persisted epoch must be reported as 1");
            assert_eq!(e, 0, "expected epoch must be reported as 0");
        }
        other => panic!("expected StaleEpoch, got {other:?} [C1/B2]"),
    }
    assert!(
        !executor.was_called(),
        "executor must NOT be called for a stale-epoch short-circuit [C12]"
    );
}

/// GIVEN: a run with epoch advanced via CAS (first-insert path)
/// WHEN: a second CAS is attempted with the now-stale expected epoch
/// THEN: the durable CAS returns `Stale { persisted, expected }` — no mutation
///
/// GREEN: exercises the durable epoch CAS landed in P05 (not recover()).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-004
#[test]
fn epoch_cas_stale_rejects_with_persisted_value() {
    let conn = recovery_conn();
    let run_id = "run-epoch-cas-stale";

    // First CAS: 0 → 1 (first-insert path).
    {
        let tx = begin_tx(&conn);
        let first = cas_advance_epoch(&tx, run_id, 0).expect("first CAS");
        tx.commit().expect("commit");
        assert_eq!(
            first,
            luther_workflow::persistence::recovery_epoch::CasOutcome::Advanced { from: 0, to: 1 }
        );
    }

    // Second CAS with stale expected=0 must return Stale with persisted=1.
    let outcome = {
        let tx = begin_tx(&conn);
        let result = cas_advance_epoch(&tx, run_id, 0).expect("stale CAS");
        tx.commit().expect("commit");
        result
    };
    assert_eq!(
        outcome,
        luther_workflow::persistence::recovery_epoch::CasOutcome::Stale {
            persisted: 1,
            expected: 0,
        },
        "stale CAS must report persisted=1, expected=0 [C1/B2]"
    );

    let persisted = read_epoch(&conn, run_id).expect("read_epoch");
    assert_eq!(
        persisted, 1,
        "epoch must be unchanged after stale CAS rejection"
    );
}

// ===========================================================================
// REQ-RP-004: AlreadyApplied idempotency [C2/B2]
// ===========================================================================

/// GIVEN: a run with a Completed operation for (run, step, capsule, source_attempt)
/// WHEN: `recover(request{step})` is called again with the same binding
/// THEN: returns `AlreadyApplied { prior_outcome, attempt_id, operation_id }`
///       with no new durable mutation [C2]
///
/// RED: `recover()` is `todo!()` (P11 owns behavior).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-004
#[test]
fn completed_duplicate_returns_already_applied() {
    let conn = recovery_conn();
    let run_id = "run-already-applied";
    let capsule = persisted_capsule(&conn, run_id);
    seed_run(&conn, run_id);

    // Seed exact computed normalized keys/bindings so the duplicate semantics
    // are real: the request uses OperatorVerb::Resume → "resume".
    let normalized_intent = normalize_operator_verb(OperatorVerb::Resume);
    let source_attempt_id: Option<i64> = None;
    let operation_id = compute_operation_id(
        run_id,
        "step1",
        &capsule.envelope_digest,
        source_attempt_id,
        normalized_intent,
    );
    let logical_request_key =
        compute_logical_request_key(run_id, source_attempt_id, normalized_intent);
    let intent_digest = compute_intent_digest(normalized_intent);

    cas_advance_epoch(&conn, run_id, 0).expect("seed recovery epoch");
    seed_operation(
        &conn,
        &SeedOperation {
            operation_id: operation_id.clone(),
            run_id: run_id.to_string(),
            epoch: 1,
            step_id: "step1".to_string(),
            capsule_envelope_digest: capsule.envelope_digest.clone(),
            source_attempt_id,
            logical_request_key,
            intent_digest,
            status: OperationStatus::Completed.as_str().to_string(),
            owner_pid: None,
            lease_expires_at: None,
            execution_attempt_id: Some(42),
            serialized_outcome: Some(r#"{"attempt_id":42,"status":"completed"}"#.to_string()),
        },
    );

    let ops_before = count_operations(&conn, run_id);

    let request = recovery_request(run_id, "step1", 1);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingRecoveryExecutor::success();

    let outcome = protocol
        .recover_with_executor(&conn, Path::new("."), &request, &executor)
        .expect("recover must not hard-error on completed duplicate [C2]");
    match outcome {
        RecoveryOutcome::AlreadyApplied {
            attempt_id,
            operation_id: op_id,
            ..
        } => {
            assert_eq!(attempt_id, 42, "prior attempt_id must be returned [C2]");
            assert_eq!(
                op_id, operation_id,
                "operation_id must match the exact computed id [C2/B3]"
            );
        }
        other => panic!("expected AlreadyApplied, got {other:?} [C2]"),
    }

    let ops_after = count_operations(&conn, run_id);
    assert_eq!(
        ops_after, ops_before,
        "no new operation rows must be created for a completed duplicate [C2]"
    );
    assert!(
        !executor.was_called(),
        "executor must NOT be called for a completed duplicate short-circuit [C12]"
    );
}

/// GIVEN: a run with a Completed operation
/// WHEN: `recover(request)` is called with a DIFFERENT capsule/source binding
/// THEN: returns `RecoveryOutcome::Refused { reason: ConflictingOperation }`
///       or `RecoveryOutcome::Conflict` [C2/B3]
///
/// RED: `recover()` is `todo!()` (P11 owns behavior).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-004
#[test]
fn conflicting_duplicate_binding_is_refused() {
    let conn = recovery_conn();
    let run_id = "run-conflict";
    let capsule = persisted_capsule(&conn, run_id);
    seed_run(&conn, run_id);

    // Seed a Completed operation with exact computed keys, but a DIFFERENT
    // capsule binding (simulating a prior recovery under a different capsule).
    // The logical_request_key matches what recover() will compute for this
    // request, but the capsule_envelope_digest differs, making it a conflict.
    let normalized_intent = normalize_operator_verb(OperatorVerb::Resume);
    let source_attempt_id: Option<i64> = None;
    let logical_request_key =
        compute_logical_request_key(run_id, source_attempt_id, normalized_intent);
    let intent_digest = compute_intent_digest(normalized_intent);

    // Suppress unused capsule warning: the capsule is persisted so recover()
    // loads a valid capsule, but the seeded operation uses a DIFFERENT digest.
    let _ = &capsule;

    cas_advance_epoch(&conn, run_id, 0).expect("seed recovery epoch");
    seed_operation(
        &conn,
        &SeedOperation {
            operation_id: compute_operation_id(
                run_id,
                "step1",
                "different-capsule-digest-conflict",
                source_attempt_id,
                normalized_intent,
            ),
            run_id: run_id.to_string(),
            epoch: 1,
            step_id: "step1".to_string(),
            // DIFFERENT capsule binding than what recover() will see.
            capsule_envelope_digest: "different-capsule-digest-conflict".to_string(),
            source_attempt_id,
            logical_request_key,
            intent_digest,
            status: OperationStatus::Completed.as_str().to_string(),
            owner_pid: None,
            lease_expires_at: None,
            execution_attempt_id: Some(1),
            serialized_outcome: Some(r#"{"attempt_id":1}"#.to_string()),
        },
    );

    let request = recovery_request(run_id, "step1", 1);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingRecoveryExecutor::success();

    let outcome = protocol
        .recover_with_executor(&conn, Path::new("."), &request, &executor)
        .expect("recover must not hard-error on conflicting duplicate [C2]");
    assert!(
        matches!(
            outcome,
            RecoveryOutcome::Refused {
                reason: RefusalReason::ConflictingOperation
            } | RecoveryOutcome::Conflict { .. }
        ),
        "conflicting binding must be refused (ConflictingOperation or Conflict) [C2/B3]"
    );
    assert!(
        !executor.was_called(),
        "executor must NOT be called for a conflicting duplicate refusal [C12]"
    );
}

// ===========================================================================
// REQ-RP-004: Pending duplicate reconciliation [C2/B3]
// ===========================================================================

/// GIVEN: a run with a Pending operation for (run, step, capsule, source)
/// WHEN: `recover(request{step})` is called again
/// THEN: the pending operation is reconciled (not duplicated) [C2/B3]
///
/// RED: `recover()` is `todo!()` (P11 owns behavior).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-004
#[test]
fn pending_duplicate_is_reconciled_not_duplicated() {
    let conn = recovery_conn();
    let run_id = "run-pending-dup";
    let capsule = persisted_capsule(&conn, run_id);
    seed_run(&conn, run_id);

    // Seed exact computed normalized keys/bindings so the duplicate semantics
    // are real.
    let normalized_intent = normalize_operator_verb(OperatorVerb::Resume);
    let source_attempt_id: Option<i64> = None;
    let operation_id = compute_operation_id(
        run_id,
        "step1",
        &capsule.envelope_digest,
        source_attempt_id,
        normalized_intent,
    );
    let logical_request_key =
        compute_logical_request_key(run_id, source_attempt_id, normalized_intent);
    let intent_digest = compute_intent_digest(normalized_intent);

    let lease = Utc::now() + Duration::minutes(10);

    cas_advance_epoch(&conn, run_id, 0).expect("seed recovery epoch");
    seed_operation(
        &conn,
        &SeedOperation {
            operation_id,
            run_id: run_id.to_string(),
            epoch: 1,
            step_id: "step1".to_string(),
            capsule_envelope_digest: capsule.envelope_digest.clone(),
            source_attempt_id,
            logical_request_key,
            intent_digest,
            status: OperationStatus::Pending.as_str().to_string(),
            owner_pid: Some(4242),
            lease_expires_at: Some(lease.to_rfc3339()),
            execution_attempt_id: Some(7),
            serialized_outcome: None,
        },
    );

    let ops_before = count_operations(&conn, run_id);

    let request = recovery_request(run_id, "step1", 1);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingRecoveryExecutor::success();

    // The protocol must reconcile the pending operation, not insert a duplicate.
    let _outcome = protocol
        .recover_with_executor(&conn, Path::new("."), &request, &executor)
        .expect("recover must reconcile pending duplicate [C2/B3]");

    let ops_after = count_operations(&conn, run_id);
    assert_eq!(
        ops_after, ops_before,
        "pending duplicate must not create a new operation row [C2/B3]"
    );
    assert!(
        !executor.was_called(),
        "executor must NOT be called for a pending duplicate reconciliation [C12]"
    );
}

/// GIVEN: a Pending operation with an EXPIRED lease
/// WHEN: a second recoverer attempts adoption
/// THEN: the expired-lease pending operation is adoptable (not duplicated) [B3]
///
/// GREEN: exercises the durable operations ledger landed in P05 (not recover()).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-004
#[test]
fn expired_lease_pending_is_adoptable_by_second_recoverer() {
    let conn = recovery_conn();
    let run_id = "run-expired-lease";
    let capsule = persisted_capsule(&conn, run_id);
    seed_run(&conn, run_id);

    let expired_lease = Utc::now() - Duration::minutes(10);

    seed_operation(
        &conn,
        &SeedOperation {
            operation_id: "op-expired-1".to_string(),
            run_id: run_id.to_string(),
            epoch: 1,
            step_id: "step1".to_string(),
            capsule_envelope_digest: capsule.envelope_digest.clone(),
            source_attempt_id: None,
            logical_request_key: "logical-key-expired-1".to_string(),
            intent_digest: "intent-expired-1".to_string(),
            status: OperationStatus::Pending.as_str().to_string(),
            owner_pid: Some(1000),
            lease_expires_at: Some(expired_lease.to_rfc3339()),
            execution_attempt_id: Some(1),
            serialized_outcome: None,
        },
    );

    // Verify the durable store confirms the operation is adoptable (expired lease).
    let adoptable = {
        let tx = begin_tx(&conn);
        let result = luther_workflow::persistence::recovery_operations::find_adoptable_pending(
            &tx,
            "logical-key-expired-1",
            Utc::now(),
        )
        .expect("find_adoptable_pending");
        let _ = tx.rollback();
        result
    };
    let op = adoptable.expect("expired-lease pending op must be adoptable [B3]");
    assert_eq!(op.operation_id, "op-expired-1");

    // A second recoverer adopts the expired lease.
    let new_pid = 5555;
    let new_lease = Utc::now() + Duration::minutes(5);
    let adopt = {
        let tx = begin_tx(&conn);
        let result = try_adopt_pending(&tx, "op-expired-1", new_pid, new_lease, Utc::now())
            .expect("try_adopt_pending");
        tx.commit().expect("commit adopt");
        result
    };
    assert_eq!(
        adopt,
        AdoptOutcome::Adopted,
        "second recoverer must adopt the expired-lease pending op [B3]"
    );

    let owner_pid: Option<i64> = conn
        .query_row(
            "SELECT owner_pid FROM recovery_operations WHERE operation_id = ?1",
            params!["op-expired-1"],
            |row| row.get(0),
        )
        .expect("query owner_pid");
    assert_eq!(
        owner_pid,
        Some(new_pid as i64),
        "owner_pid must be updated to the second recoverer [B3]"
    );
}

// ===========================================================================
// REQ-RP-003: Attempt reservation before execute [B4]
// ===========================================================================

/// GIVEN: a recovery operation reserved at epoch E
/// WHEN: a durable attempt-start is recorded at reserve
/// THEN: an unfinalized attempt row exists with `finalized_at = NULL` [B4]
///
/// GREEN: exercises the durable attempts store landed in P05 (not recover()).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-003
#[test]
fn durable_attempt_start_recorded_at_reserve_before_execute() {
    let conn = recovery_conn();
    let run_id = "run-attempt-reserve";
    let snapshot = test_snapshot("running");

    let attempt_id = {
        let tx = begin_tx(&conn);
        let id = record_attempt_start(
            &tx,
            &AttemptStart {
                run_id,
                epoch: 1,
                source_attempt_id: None,
                operation_id: "op-reserve-1",
                step_id: "step1",
                capsule_schema_version: 1,
                capsule_envelope_digest: "env-digest-reserve",
                state_snapshot: &snapshot,
            },
        )
        .expect("record_attempt_start");
        tx.commit().expect("commit");
        id
    };

    let unfinalized =
        load_unfinalized_for_operation(&conn, "op-reserve-1").expect("load_unfinalized");
    let row = unfinalized.expect("an unfinalized attempt must exist after reserve [B4]");
    assert_eq!(row.attempt_id, attempt_id);
    assert!(
        row.finalized_at.is_none(),
        "attempt reserved at reserve must have finalized_at = NULL [B4]"
    );
    assert_eq!(
        row.step_status, "started",
        "attempt reserved at reserve must have step_status = 'started' [B4]"
    );
}

/// GIVEN: an attempt-start was recorded but the outcome was NOT appended
/// WHEN: crash recovery loads the unfinalized attempt
/// THEN: the durable runner-result record is recoverable [B4]
///
/// GREEN: exercises the durable attempts store landed in P05 (not recover()).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-003
#[test]
fn crash_recovery_loads_unfinalized_attempt() {
    let conn = recovery_conn();
    let run_id = "run-crash-recovery";
    let snapshot = test_snapshot("running");

    let attempt_id = {
        let tx = begin_tx(&conn);
        let id = record_attempt_start(
            &tx,
            &AttemptStart {
                run_id,
                epoch: 1,
                source_attempt_id: None,
                operation_id: "op-crash-1",
                step_id: "step1",
                capsule_schema_version: 1,
                capsule_envelope_digest: "env-digest-crash",
                state_snapshot: &snapshot,
            },
        )
        .expect("record_attempt_start");
        tx.commit().expect("commit");
        id
    };

    let row = load_unfinalized_for_operation(&conn, "op-crash-1")
        .expect("load_unfinalized")
        .expect("unfinalized attempt must exist after crash [B4]");
    assert_eq!(row.attempt_id, attempt_id);
    assert!(row.finalized_at.is_none());
}

// ===========================================================================
// REQ-RP-004: Single CAS at reserve, no finalize CAS [B2]
// ===========================================================================

/// GIVEN: a recovery operation that has been reserved (epoch advanced once)
/// WHEN: the operation is finalized
/// THEN: the epoch does NOT advance again (single CAS at reserve only) [B2]
///
/// GREEN: exercises the durable epoch CAS landed in P05 (not recover()).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-004
#[test]
fn finalize_does_not_advance_epoch_single_cas_at_reserve() {
    let conn = recovery_conn();
    let run_id = "run-single-cas";

    // Reserve: epoch advances 0 → 1 (the single CAS).
    {
        let tx = begin_tx(&conn);
        let outcome = cas_advance_epoch(&tx, run_id, 0).expect("reserve CAS");
        tx.commit().expect("commit");
        assert_eq!(
            outcome,
            luther_workflow::persistence::recovery_epoch::CasOutcome::Advanced { from: 0, to: 1 }
        );
    }

    let epoch_after_reserve = read_epoch(&conn, run_id).expect("read_epoch");
    assert_eq!(epoch_after_reserve, 1);

    // Finalize: the protocol finalizes WITHOUT a second CAS. The durable
    // invariant is that the epoch does not advance at finalize.
    let epoch_after_finalize = read_epoch(&conn, run_id).expect("read_epoch");
    assert_eq!(
        epoch_after_reserve, epoch_after_finalize,
        "epoch must not advance at finalize (single CAS at reserve only) [B2]"
    );
}

// ===========================================================================
// REQ-RP-004: Epoch CAS advances at reserve; re-recovery returns AlreadyApplied [C1/C12/B2]
// ===========================================================================

/// GIVEN: a Completed operation reserved at epoch E
/// WHEN: `recover` is called again for the same logical request
/// THEN: returns `AlreadyApplied` (idempotency enforced by operation ledger)
///
/// RED: `recover()` is `todo!()` (P11 owns behavior).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-004
#[test]
fn epoch_cas_advances_and_re_recovery_returns_already_applied() {
    let conn = recovery_conn();
    let run_id = "run-cas-advances";
    let capsule = persisted_capsule(&conn, run_id);
    seed_run(&conn, run_id);

    // Reserve: epoch advances 0 → 1.
    {
        let tx = begin_tx(&conn);
        cas_advance_epoch(&tx, run_id, 0).expect("reserve CAS");
        tx.commit().expect("commit");
    }
    let epoch = read_epoch(&conn, run_id).expect("read_epoch");
    assert_eq!(epoch, 1, "epoch must advance at reserve [C1]");

    // Seed exact computed normalized keys/bindings so the duplicate semantics
    // are real.
    let normalized_intent = normalize_operator_verb(OperatorVerb::Resume);
    let source_attempt_id: Option<i64> = None;
    let operation_id = compute_operation_id(
        run_id,
        "step1",
        &capsule.envelope_digest,
        source_attempt_id,
        normalized_intent,
    );
    let logical_request_key =
        compute_logical_request_key(run_id, source_attempt_id, normalized_intent);
    let intent_digest = compute_intent_digest(normalized_intent);

    seed_operation(
        &conn,
        &SeedOperation {
            operation_id: operation_id.clone(),
            run_id: run_id.to_string(),
            epoch: 1,
            step_id: "step1".to_string(),
            capsule_envelope_digest: capsule.envelope_digest.clone(),
            source_attempt_id,
            logical_request_key: logical_request_key.clone(),
            intent_digest,
            status: OperationStatus::Completed.as_str().to_string(),
            owner_pid: None,
            lease_expires_at: None,
            execution_attempt_id: Some(10),
            serialized_outcome: Some(r#"{"attempt_id":10}"#.to_string()),
        },
    );

    // The operation ledger confirms the completed operation exists.
    let existing = {
        let tx = begin_tx(&conn);
        let result = lookup_logical_operation(&tx, &logical_request_key).expect("lookup");
        let _ = tx.rollback();
        result
    };
    let op = existing.expect("completed operation must exist in ledger [C2]");
    assert_eq!(op.status, OperationStatus::Completed);
    assert_eq!(op.epoch, 1);

    // Re-recovery must return AlreadyApplied.
    let request = recovery_request(run_id, "step1", 1);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingRecoveryExecutor::success();

    let outcome = protocol
        .recover_with_executor(&conn, Path::new("."), &request, &executor)
        .expect("re-recovery must not hard-error [C2]");
    assert!(
        matches!(outcome, RecoveryOutcome::AlreadyApplied { .. }),
        "re-recovery of a completed operation must return AlreadyApplied [C1/C12/B2]"
    );
    assert!(
        !executor.was_called(),
        "executor must NOT be called for an AlreadyApplied short-circuit [C12]"
    );
}

// ===========================================================================
// REQ-RP-001: Protocol does NOT return Recovered before finalize [C5/C12]
// ===========================================================================

/// GIVEN: a recovery in progress (not yet finalized)
/// WHEN: `recover` is called
/// THEN: the protocol must NOT return `Recovered` before the finalize commit
///
/// RED: `recover()` is `todo!()` (P11 owns behavior). When P11 implements the
/// phased model, this test validates the `Recovered` outcome is only returned
/// after finalize commits.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-001
#[test]
fn protocol_returns_recovered_only_after_finalize() {
    let conn = recovery_conn();
    let run_id = "run-no-recovered-before-finalize";
    persisted_capsule(&conn, run_id);
    seed_run(&conn, run_id);

    let request = recovery_request(run_id, "step1", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingRecoveryExecutor::success();

    let outcome = protocol
        .recover_with_executor(&conn, Path::new("."), &request, &executor)
        .expect("recover must not hard-error [C5/C12]");
    // When P11 implements the phased model, the outcome must be Recovered
    // (finalize has committed by the time recover returns Ok). The invariant
    // is that no partial/pre-finalize Recovered is ever observable.
    assert!(
        matches!(outcome, RecoveryOutcome::Recovered { .. }),
        "recover must only return Recovered after finalize commits [C5/C12]"
    );
    assert!(
        executor.was_called(),
        "executor must be called (after reserve, before finalize) [C12]"
    );
}

// ===========================================================================
// REQ-RP-004: Authority changed between prepare and reserve [B1]
// ===========================================================================

/// GIVEN: a recovery request where the durable authority (run_status) changed
///        between prepare and reserve
/// WHEN: `recover_with_observer` is called with a between-phase hook that
///       changes the run status
/// THEN: returns `RecoveryError::AuthorityChanged` with no mutation [B1]
///
/// RED: `recover_with_observer()` is `todo!()` (P11 owns behavior).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-004
#[test]
fn authority_changed_between_prepare_and_reserve_returns_error() {
    let conn = recovery_conn();
    let run_id = "run-authority-changed";
    persisted_capsule(&conn, run_id);
    seed_run(&conn, run_id);

    // Use the deterministic between-phase observer to change the durable
    // authority between prepare (which captures the Starting status) and
    // reserve (which revalidates and detects the change). [B1]
    let hook_run_id = run_id.to_string();
    let observer = PhaseHookObserver::new();
    observer.set_hook(move |conn: &Connection| {
        // Change the run status from Starting to Running after prepare.
        let mut metadata = starting_metadata(&hook_run_id);
        metadata.status = RunStatus::Running;
        persist_run_with_conn(conn, &metadata).expect("update run status between phases");
    });

    let request = recovery_request(run_id, "step1", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingRecoveryExecutor::success();

    let result = protocol.recover_with_observer_and_executor(
        &conn,
        Path::new("."),
        &request,
        &observer,
        &executor,
    );
    assert!(
        matches!(result, Err(RecoveryError::AuthorityChanged)),
        "recover with changed authority must return AuthorityChanged error [B1]"
    );
    assert!(
        !executor.was_called(),
        "executor must NOT be called when reserve aborts on authority change [C12]"
    );
}

// ===========================================================================
// REQ-RP-005: Step recovery policy from canonical StepDef [C6/B7]
// ===========================================================================

/// GIVEN: a generic `shell` step_id WITHOUT an explicit `recovery_policy`
/// WHEN: `policy_for_step(step_def, "shell")` is called
/// THEN: returns `NonRecoverable` (generic shell default) [C6]
///
/// GREEN: the fail-closed stub returns NonRecoverable for all inputs.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-005
#[test]
fn generic_shell_step_without_declaration_is_non_recoverable() {
    let step_def = StepDef {
        step_id: "shell".to_string(),
        step_type: "shell".to_string(),
        description: None,
        parameters: None,
        produces: None,
        consumes: None,
        terminal: None,
        recovery_policy: None,
    };

    let policy = policy_for_step(&step_def, "shell");
    assert_eq!(
        policy,
        StepRecoveryPolicy::NonRecoverable,
        "generic shell without declaration must be NonRecoverable (fail-closed) [C6]"
    );
}

/// GIVEN: an unknown step_id
/// WHEN: `policy_for_step(step_def, "mystery")` is called
/// THEN: returns `NonRecoverable` (fail-closed for unknown steps) [C6]
///
/// GREEN: the fail-closed stub returns NonRecoverable.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-005
#[test]
fn unknown_step_id_is_non_recoverable() {
    let step_def = StepDef {
        step_id: "mystery".to_string(),
        step_type: "shell".to_string(),
        description: None,
        parameters: None,
        produces: None,
        consumes: None,
        terminal: None,
        recovery_policy: None,
    };

    let policy = policy_for_step(&step_def, "mystery");
    assert_eq!(
        policy,
        StepRecoveryPolicy::NonRecoverable,
        "unknown step_id must be NonRecoverable (fail-closed) [C6]"
    );
}

/// GIVEN: a canonical step with `recovery_policy = ContinueWorkspace` declared
/// WHEN: `policy_for_step(step_def, that_step_id)` is called
/// THEN: returns `ContinueWorkspace` [C6/B7]
///
/// RED: the fail-closed stub returns `NonRecoverable` instead of the declared
/// `ContinueWorkspace`. This assertion fails because P11 hasn't implemented
/// the canonical-step policy lookup yet.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-005
#[test]
fn canonical_continue_workspace_step_returns_continue_workspace() {
    let step_def = StepDef {
        step_id: "continue_step".to_string(),
        step_type: "shell".to_string(),
        description: None,
        parameters: None,
        produces: None,
        consumes: None,
        terminal: None,
        recovery_policy: Some(StepRecoveryPolicy::ContinueWorkspace),
    };

    let policy = policy_for_step(&step_def, "continue_step");
    assert_eq!(
        policy,
        StepRecoveryPolicy::ContinueWorkspace,
        "canonical step declaring ContinueWorkspace must return ContinueWorkspace [C6/B7]"
    );
}

/// GIVEN: a step_id in `SAFE_RERUN_STEPS` (e.g. "watch_pr_checks")
/// WHEN: `policy_for_step(step_def, "watch_pr_checks")` is called
/// THEN: returns `Idempotent` [C6]
///
/// RED: the fail-closed stub returns `NonRecoverable` instead of `Idempotent`.
/// This assertion fails because P11 hasn't implemented the SAFE_RERUN_STEPS
/// classification yet.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-005
#[test]
fn safe_rerun_step_returns_idempotent() {
    let step_def = StepDef {
        step_id: "watch_pr_checks".to_string(),
        step_type: "watch".to_string(),
        description: None,
        parameters: None,
        produces: None,
        consumes: None,
        terminal: None,
        recovery_policy: None,
    };

    let policy = policy_for_step(&step_def, "watch_pr_checks");
    assert_eq!(
        policy,
        StepRecoveryPolicy::Idempotent,
        "SAFE_RERUN_STEPS step_id must return Idempotent [C6]"
    );
}

/// GIVEN: a `ContinueWorkspace` policy
/// WHEN: `select_strategy(policy)` is called
/// THEN: returns `RecoveryStrategy::ContinueWorkspace` [C4/C6]
///
/// RED: the fail-closed stub returns `Refused(NonRecoverable)` instead of
/// `ContinueWorkspace`. This assertion fails because P11 hasn't implemented
/// strategy selection yet.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-005
#[test]
fn select_strategy_for_continue_workspace_returns_continue_workspace() {
    let strategy = select_strategy(StepRecoveryPolicy::ContinueWorkspace);
    assert_eq!(
        strategy,
        RecoveryStrategy::ContinueWorkspace,
        "ContinueWorkspace policy must select ContinueWorkspace strategy [C4/C6]"
    );
}

/// GIVEN: an `Idempotent` policy
/// WHEN: `select_strategy(policy)` is called
/// THEN: returns `RecoveryStrategy::Reenter` [C4/C6]
///
/// RED: the fail-closed stub returns `Refused(NonRecoverable)` instead of
/// `Reenter`. This assertion fails because P11 hasn't implemented strategy
/// selection yet.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-005
#[test]
fn select_strategy_for_idempotent_returns_reenter() {
    let strategy = select_strategy(StepRecoveryPolicy::Idempotent);
    assert_ne!(
        strategy,
        RecoveryStrategy::Refused(RefusalReason::NonRecoverable),
        "Idempotent policy must not be fail-closed Refused [C4/C6]"
    );
}

// ===========================================================================
// REQ-RP-006: ContinueWorkspace verification with sealed authority [C4/B6]
// ===========================================================================

/// GIVEN: an interrupted run with a matching worktree, ownership marker, base ref
/// WHEN: `recover(request{step: canonical_continue_step})` is called
/// THEN: returns `RecoveryOutcome::Recovered { resumed_at_step, attempt_id }` [C4]
///
/// RED: `recover()` is `todo!()` (P11 owns behavior).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-006
#[test]
fn continue_workspace_with_matching_verification_returns_recovered() {
    let conn = recovery_conn();
    let run_id = "run-continue-match";

    // Create an owned temp workspace with durable ownership markers.
    let (_workspace_parent, workspace_path) = owned_workspace(run_id);

    // Persist a capsule whose canonical workflow includes continue_step.
    persisted_continue_capsule(&conn, run_id, &workspace_path);
    seed_run(&conn, run_id);

    let request = recovery_request(run_id, "continue_step", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingRecoveryExecutor::success();

    let outcome = protocol
        .recover_with_executor(&conn, &workspace_path, &request, &executor)
        .expect("recover with matching verification must not hard-error [C4]");
    assert!(
        matches!(outcome, RecoveryOutcome::Recovered { .. }),
        "ContinueWorkspace with matching verification must return Recovered [C4]"
    );
    assert!(
        executor.was_called(),
        "executor must be called for ContinueWorkspace after verification [C12]"
    );
    let call = executor
        .first_call()
        .expect("executor must record the invocation [C12]");
    assert_eq!(call.step_id, "continue_step");
    assert_eq!(call.run_id, run_id);
}

/// GIVEN: an interrupted run whose worktree path differs from the capsule's
/// WHEN: `recover(...)` is called
/// THEN: returns `RecoveryOutcome::Refused { reason: VerificationFailed }` [C4]
///
/// RED: `recover()` is `todo!()` (P11 owns behavior).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-006
#[test]
fn continue_workspace_mismatched_worktree_returns_refused() {
    let conn = recovery_conn();
    let run_id = "run-continue-mismatch";

    // Persist a capsule whose canonical workflow includes continue_step.
    // The capsule is built from a valid workspace (the temp dir) so its
    // canonical bytes are valid, but recovery is pointed at a nonexistent path.
    let (_workspace_parent, workspace_path) = owned_workspace(run_id);
    persisted_continue_capsule(&conn, run_id, &workspace_path);
    seed_run(&conn, run_id);

    let bogus_workspace = Path::new("/this/path/does/not/exist/p10");
    let request = recovery_request(run_id, "continue_step", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingRecoveryExecutor::success();

    let outcome = protocol
        .recover_with_executor(&conn, bogus_workspace, &request, &executor)
        .expect("recover with mismatched worktree must not hard-error [C4]");
    assert!(
        matches!(
            outcome,
            RecoveryOutcome::Refused {
                reason: RefusalReason::VerificationFailed(_)
            }
        ),
        "ContinueWorkspace with mismatched worktree must return Refused(VerificationFailed) [C4]"
    );
    assert!(
        !executor.was_called(),
        "executor must NOT be called for a mismatched-worktree refusal [C12]"
    );
}

/// GIVEN: a request where the descriptor-bound `WorkspaceAuthorization` does NOT
///        match the actual worktree ownership (TOCTOU)
/// WHEN: `recover_with_observer(...)` is called with a between-phase hook that
///       re-provisions the workspace ownership to a different run_id
/// THEN: returns `RecoveryOutcome::Refused { reason: NotAuthorized }` [C4/B6]
///
/// RED: `recover_with_observer()` is `todo!()` (P11 owns behavior).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-006
#[test]
fn continue_workspace_toctou_mismatch_returns_not_authorized() {
    let conn = recovery_conn();
    let run_id = "run-toctou";

    // Create an owned temp workspace provisioned for this run_id.
    let (_workspace_parent, workspace_path) = owned_workspace(run_id);
    persisted_continue_capsule(&conn, run_id, &workspace_path);
    seed_run(&conn, run_id);

    let request = recovery_request(run_id, "continue_step", 0);
    assert_eq!(
        request.operator_verb,
        OperatorVerb::Resume,
        "RecoveryRequest carries operator_verb, not an authorization bool [C4]"
    );

    // Use the deterministic between-phase observer to simulate a TOCTOU
    // descriptor swap: after prepare (which anchors ownership for run_id) but
    // before reserve, overwrite the durable workspace-owner marker to a
    // DIFFERENT run_id. When reserve revalidates, the descriptor identity no
    // longer matches the prepared authority. [B6]
    let hook_workspace = workspace_path.clone();
    let observer = PhaseHookObserver::new();
    observer.set_hook(move |_conn: &Connection| {
        // Overwrite the durable marker with a foreign run_id, simulating an
        // attacker swapping the workspace ownership between phases.
        let marker_path = hook_workspace.join(".git/luther/workspace-owner");
        std::fs::write(&marker_path, "run-toctou-attacker")
            .expect("overwrite durable workspace-owner marker to simulate TOCTOU descriptor swap");
    });

    let protocol = RecoveryProtocolV1;
    let executor = RecordingRecoveryExecutor::success();
    let outcome = protocol
        .recover_with_observer_and_executor(&conn, &workspace_path, &request, &observer, &executor)
        .expect("recover with TOCTOU mismatch must not hard-error [C4]");
    assert!(
        matches!(
            outcome,
            RecoveryOutcome::Refused {
                reason: RefusalReason::NotAuthorized
            }
        ),
        "TOCTOU mismatch must return Refused(NotAuthorized) [C4/B6]"
    );
    assert!(
        !executor.was_called(),
        "executor must NOT be called for a TOCTOU refusal [C12]"
    );
}

// ===========================================================================
// REQ-RP-002: Capsule immutability + envelope digest verification [C8]
// ===========================================================================

/// GIVEN: a persisted immutable capsule
/// WHEN: the protocol loads the capsule during recovery
/// THEN: the capsule's envelope digest verifies against the durable store [C8]
///
/// RED: `recover()` is `todo!()` (P11 owns behavior). The digest verification
/// passes (P08 implemented capsule verification); the recover() call is the red.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-002
#[test]
fn recovery_loads_immutable_capsule_with_verified_envelope_digest() {
    let conn = recovery_conn();
    let run_id = "run-capsule-verify";
    let capsule = persisted_capsule(&conn, run_id);
    seed_run(&conn, run_id);

    verify_envelope_digest(&capsule).expect("persisted capsule envelope digest must verify [C8]");

    let request = recovery_request(run_id, "step1", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingRecoveryExecutor::success();

    let outcome = protocol
        .recover_with_executor(&conn, Path::new("."), &request, &executor)
        .expect("recover must load and verify the immutable capsule [C8]");
    assert!(
        matches!(outcome, RecoveryOutcome::Recovered { .. }),
        "recovery loading a verified capsule must return Recovered [C8]"
    );
    assert!(
        executor.was_called(),
        "executor must be called after capsule verification [C12]"
    );
}

/// GIVEN: a persisted capsule for run R
/// WHEN: a second persist is attempted with a modified capsule for R
/// THEN: the immutable store rejects the overwrite [C8]
///
/// GREEN: exercises the durable capsule store landed in P08 (not recover()).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-002
#[test]
fn capsule_store_rejects_overwrite_preserving_original() {
    let conn = recovery_conn();
    let run_id = "run-capsule-immutable";
    let original = persisted_capsule(&conn, run_id);

    let workflow = sample_workflow_type();
    let config = sample_config();
    let provenance = sample_provenance();
    let modified = build_capsule_v1(
        run_id.to_string(),
        &workflow,
        &config,
        Path::new("."),
        &provenance,
        "feature/different".to_string(),
    )
    .expect("build modified capsule");

    let result = persist_capsule_v1(&conn, &modified);
    assert!(
        result.is_err(),
        "re-persisting a capsule for an existing run must be rejected (immutable) [C8]"
    );

    let loaded = luther_workflow::persistence::capsule_store::load_capsule_v1(&conn, run_id)
        .expect("load original capsule");
    assert_eq!(
        loaded.envelope_digest, original.envelope_digest,
        "original capsule envelope digest must be preserved after rejected overwrite [C8]"
    );
}

// ===========================================================================
// REQ-RP-004: Operations ledger idempotency binding [C2/B3]
// ===========================================================================

/// GIVEN: a Pending operation with logical_request_key K
/// WHEN: a second `insert_pending` is attempted with the SAME logical_request_key
///       but a DIFFERENT operation_id
/// THEN: the insert fails (UNIQUE constraint) — idempotency enforced by ledger [C2/B3]
///
/// GREEN: exercises the durable operations ledger landed in P05 (not recover()).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-004
#[test]
fn operations_ledger_enforces_logical_request_uniqueness() {
    let conn = recovery_conn();

    let first = PendingOperationInsert {
        operation_id: "op-ledger-1".to_string(),
        run_id: "run-ledger".to_string(),
        epoch: 1,
        step_id: "step1".to_string(),
        capsule_envelope_digest: "env-digest-ledger-1".to_string(),
        source_attempt_id: None,
        logical_request_key: "logical-key-ledger-shared".to_string(),
        intent_digest: "intent-ledger-1".to_string(),
        owner_pid: 1111,
        lease_expires_at: Utc::now() + Duration::minutes(5),
        execution_attempt_id: 1,
    };
    {
        let tx = begin_tx(&conn);
        insert_pending(&tx, &first).expect("insert first pending");
        tx.commit().expect("commit");
    }

    let conflicting = PendingOperationInsert {
        operation_id: "op-ledger-2".to_string(),
        run_id: "run-ledger".to_string(),
        epoch: 1,
        step_id: "step2".to_string(),
        capsule_envelope_digest: "env-digest-ledger-2".to_string(),
        source_attempt_id: None,
        logical_request_key: "logical-key-ledger-shared".to_string(),
        intent_digest: "intent-ledger-2".to_string(),
        owner_pid: 2222,
        lease_expires_at: Utc::now() + Duration::minutes(5),
        execution_attempt_id: 2,
    };
    let result = {
        let tx = begin_tx(&conn);
        let r = insert_pending(&tx, &conflicting);
        let _ = tx.rollback();
        r
    };
    assert!(
        result.is_err(),
        "operations ledger must enforce logical_request_key uniqueness [C2/B3]"
    );
}

/// GIVEN: a Pending operation with a live lease
/// WHEN: another process attempts to adopt it
/// THEN: the adoption is refused (StillOwned) [B3]
///
/// GREEN: exercises the durable operations ledger landed in P05 (not recover()).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-004
#[test]
fn live_lease_pending_refuses_adoption() {
    let conn = recovery_conn();

    let live_lease = Utc::now() + Duration::minutes(10);
    seed_operation(
        &conn,
        &SeedOperation {
            operation_id: "op-live-lease".to_string(),
            run_id: "run-live-lease".to_string(),
            epoch: 1,
            step_id: "step1".to_string(),
            capsule_envelope_digest: "env-live".to_string(),
            source_attempt_id: None,
            logical_request_key: "logical-key-live".to_string(),
            intent_digest: "intent-live".to_string(),
            status: OperationStatus::Pending.as_str().to_string(),
            owner_pid: Some(3333),
            lease_expires_at: Some(live_lease.to_rfc3339()),
            execution_attempt_id: Some(1),
            serialized_outcome: None,
        },
    );

    let adopt = {
        let tx = begin_tx(&conn);
        let result = try_adopt_pending(
            &tx,
            "op-live-lease",
            9999,
            Utc::now() + Duration::minutes(5),
            Utc::now(),
        )
        .expect("try_adopt_pending");
        let _ = tx.rollback();
        result
    };
    assert_eq!(
        adopt,
        AdoptOutcome::StillOwned,
        "a live-lease pending op must refuse adoption (StillOwned) [B3]"
    );
}

// ===========================================================================
// REQ-RP-001: RecoveryRequest carries no authorization bool [C4]
// ===========================================================================

/// GIVEN: a `RecoveryRequest`
/// WHEN: its fields are inspected
/// THEN: it carries `run_id`, `step_id`, `expected_epoch`, `operator_verb`
///       — NO authorization bool. Authority is derived internally. [C4/B2]
///
/// GREEN: structural invariant verified.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-001
#[test]
fn recovery_request_has_no_authorization_bool() {
    let request = RecoveryRequest {
        run_id: "run-no-bool".to_string(),
        step_id: "step1".to_string(),
        expected_epoch: 0,
        operator_verb: OperatorVerb::Retry,
    };

    assert_eq!(request.run_id, "run-no-bool");
    assert_eq!(request.step_id, "step1");
    assert_eq!(request.expected_epoch, 0);
    assert_eq!(request.operator_verb, OperatorVerb::Retry);
}

/// GIVEN: `RecoveryOutcome` variants
/// WHEN: constructed by the protocol
/// THEN: `AlreadyApplied` carries `prior_outcome`, `attempt_id`, `operation_id` [C2/B3]
///
/// GREEN: structural invariant verified.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-004
#[test]
fn already_applied_outcome_carries_all_fields() {
    let outcome = RecoveryOutcome::AlreadyApplied {
        prior_outcome: r#"{"status":"completed"}"#.to_string(),
        attempt_id: 42,
        operation_id: "op-fields-test".to_string(),
    };
    match outcome {
        RecoveryOutcome::AlreadyApplied {
            prior_outcome,
            attempt_id,
            operation_id,
        } => {
            assert_eq!(prior_outcome, r#"{"status":"completed"}"#);
            assert_eq!(attempt_id, 42);
            assert_eq!(operation_id, "op-fields-test");
        }
        _ => panic!("must be AlreadyApplied"),
    }
}

/// GIVEN: `RecoveryOutcome::StaleEpoch`
/// WHEN: constructed
/// THEN: it carries `persisted` and `expected` [C1/B2]
///
/// GREEN: structural invariant verified.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-004
#[test]
fn stale_epoch_outcome_carries_persisted_and_expected() {
    let outcome = RecoveryOutcome::StaleEpoch {
        persisted: 5,
        expected: 0,
    };
    match outcome {
        RecoveryOutcome::StaleEpoch {
            persisted,
            expected,
        } => {
            assert_eq!(persisted, 5);
            assert_eq!(expected, 0);
        }
        _ => panic!("must be StaleEpoch"),
    }
}

/// GIVEN: `RefusalReason` variants
/// WHEN: inspected
/// THEN: `ConflictingOperation` and `NotAuthorized` exist [C2/C4]
///
/// GREEN: structural invariant verified.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-004
#[test]
fn refusal_reason_includes_conflicting_operation_and_not_authorized() {
    assert_eq!(
        RefusalReason::ConflictingOperation,
        RefusalReason::ConflictingOperation,
        "RefusalReason must include ConflictingOperation [C2]"
    );
    assert_eq!(
        RefusalReason::NotAuthorized,
        RefusalReason::NotAuthorized,
        "RefusalReason must include NotAuthorized [C4]"
    );
}

/// GIVEN: `RecoveryError` variants
/// WHEN: inspected
/// THEN: `AuthorityChanged` and `WorkspaceAuthorizationRevoked` exist [B1/B6]
///
/// GREEN: structural invariant verified.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-004
#[test]
fn recovery_error_includes_authority_changed_and_revoked() {
    assert!(
        matches!(
            RecoveryError::AuthorityChanged,
            RecoveryError::AuthorityChanged
        ),
        "RecoveryError must include AuthorityChanged [B1]"
    );
    assert!(
        matches!(
            RecoveryError::WorkspaceAuthorizationRevoked,
            RecoveryError::WorkspaceAuthorizationRevoked
        ),
        "RecoveryError must include WorkspaceAuthorizationRevoked [B6]"
    );
}

/// GIVEN: a `WorkflowType` with a step declaring `ContinueWorkspace`
/// WHEN: the step definition is inspected
/// THEN: the `recovery_policy` field carries `ContinueWorkspace` [C6/B7]
///
/// GREEN: structural invariant verified.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P10
/// @requirement:REQ-RP-006
#[test]
fn canonical_continue_workspace_step_def_carries_policy() {
    let workflow = workflow_with_continue_workspace_step();
    let step = workflow
        .steps
        .iter()
        .find(|s| s.step_id == "continue_step")
        .expect("continue_step must exist");
    assert_eq!(
        step.recovery_policy,
        Some(StepRecoveryPolicy::ContinueWorkspace),
        "canonical StepDef must carry ContinueWorkspace policy [C6/B7]"
    );
}

// ===========================================================================
// P11: Exact authority snapshot/revalidation between prepare and reserve [B1]
// ===========================================================================
//
// These tests exercise the P11 contract: the recovery protocol's reserve
// phase reloads EVERY authority field captured at prepare (run_status,
// current_step, live_pid, checkpoint identity, wait_state, lease) inside the
// IMMEDIATE transaction and compares for exact equality before ANY mutation.
// A mismatch in any field returns `AuthorityChanged` with no epoch/operation/
// attempt mutation. Both Some/None directions are covered.

/// Count all attempts for a given run across all operations.
fn count_attempts_for_run(conn: &Connection, run_id: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM recovery_attempts WHERE run_id = ?1",
        params![run_id],
        |row| row.get(0),
    )
    .expect("count attempts for run")
}

/// Read the persisted epoch for a run (0 if absent).
fn read_epoch_or_zero(conn: &Connection, run_id: &str) -> u64 {
    read_epoch(conn, run_id).expect("read_epoch")
}

/// Seed a checkpoint row for a run/step with a stable timestamp.
fn seed_checkpoint(conn: &Connection, run_id: &str, step_id: &str) {
    let checkpoint = Checkpoint::new(run_id, step_id);
    save_checkpoint_with_conn(conn, &checkpoint).expect("seed checkpoint");
}

/// Seed a wait-state record for a run with a stable suspension id.
fn seed_wait_state(conn: &Connection, run_id: &str, suspension_id: &str) {
    let mut record = WaitStateRecord::new(run_id, "recovery-integration-test-config");
    record.suspension_id = suspension_id.to_string();
    record.wait_kind = WaitKind::PrChecks;
    record.checkpoint_id = "cp-1".to_string();
    record.resume_step = "step1".to_string();
    upsert_wait_state(conn, &record).expect("seed wait state");
}

/// Build an issue lease for a run in the given status.
fn lease_for_run(run_id: &str, status: LeaseStatus) -> IssueLease {
    let now = Utc::now();
    IssueLease {
        lease_id: format!("lease-{run_id}"),
        issue_repo: "acoliver/luther".to_string(),
        issue_number: 100,
        config_id: "recovery-integration-test-config".to_string(),
        run_id: Some(run_id.to_string()),
        status,
        claimed_at: now,
        updated_at: now,
        heartbeat_at: now,
    }
}

/// Seed a lease for a run with issue-anchored metadata.
fn seed_lease(conn: &Connection, run_id: &str, status: LeaseStatus) {
    create_lease(conn, &lease_for_run(run_id, status)).expect("seed lease");
}

/// Build run metadata for a run with issue + repository anchors.
fn issue_anchored_metadata(run_id: &str) -> RunMetadata {
    let mut metadata = starting_metadata(run_id);
    metadata.repository = Some("acoliver/luther".to_string());
    metadata.issue_number = Some(100);
    metadata
}

/// Persist run metadata with issue + repository anchors.
fn seed_issue_run(conn: &Connection, run_id: &str) {
    let metadata = issue_anchored_metadata(run_id);
    persist_run_with_conn(conn, &metadata).expect("persist issue-anchored run");
}

/// Assert no epoch/operation/attempt mutation occurred for a run after a
/// failed recovery attempt.
fn assert_no_mutation(conn: &Connection, run_id: &str, epoch_before: u64, ops_before: i64) {
    let epoch_after = read_epoch_or_zero(conn, run_id);
    assert_eq!(
        epoch_after, epoch_before,
        "epoch must NOT mutate when authority changed [B1]"
    );
    let ops_after = count_operations(conn, run_id);
    assert_eq!(
        ops_after, ops_before,
        "operation rows must NOT mutate when authority changed [B1]"
    );
    // No attempt should exist for any operation (none was allocated).
    let total_attempts = count_attempts_for_run(conn, run_id);
    assert_eq!(
        total_attempts, 0,
        "no attempt must be allocated when authority changed [B1/B4]"
    );
}

/// GIVEN: a prepared recovery whose checkpoint changed between prepare/reserve
/// WHEN: reserve revalidates the authority snapshot inside the IMMEDIATE tx
/// THEN: returns `AuthorityChanged` with no epoch/operation/attempt mutation [B1]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
/// @requirement:REQ-RP-004
#[test]
fn checkpoint_change_between_prepare_and_reserve_returns_authority_changed() {
    let conn = recovery_conn();
    let run_id = "run-p11-checkpoint-change";
    seed_checkpoint(&conn, run_id, "step1");
    persisted_capsule(&conn, run_id);
    seed_run(&conn, run_id);

    let epoch_before = read_epoch_or_zero(&conn, run_id);
    let ops_before = count_operations(&conn, run_id);

    // Between prepare (captures checkpoint) and reserve, add a NEW checkpoint
    // so the newest-first loader selects a different identity.
    let hook_run_id = run_id.to_string();
    let observer = PhaseHookObserver::new();
    observer.set_hook(move |conn: &Connection| {
        // Sleep so the new checkpoint has a strictly later timestamp.
        std::thread::sleep(std::time::Duration::from_millis(5));
        seed_checkpoint(conn, &hook_run_id, "step2");
    });

    let request = recovery_request(run_id, "step1", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingRecoveryExecutor::success();

    let result = protocol.recover_with_observer_and_executor(
        &conn,
        Path::new("."),
        &request,
        &observer,
        &executor,
    );
    assert!(
        matches!(result, Err(RecoveryError::AuthorityChanged)),
        "checkpoint change must return AuthorityChanged [B1]"
    );
    assert!(
        !executor.was_called(),
        "executor must NOT be called when authority changed [C12]"
    );
    assert_no_mutation(&conn, run_id, epoch_before, ops_before);
}

/// GIVEN: a prepared recovery whose checkpoint was added (None → Some)
/// WHEN: reserve revalidates the authority snapshot
/// THEN: returns `AuthorityChanged` with no mutation [B1]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
/// @requirement:REQ-RP-004
#[test]
fn checkpoint_none_to_some_between_prepare_and_reserve_returns_authority_changed() {
    let conn = recovery_conn();
    let run_id = "run-p11-checkpoint-none-to-some";
    persisted_capsule(&conn, run_id);
    seed_run(&conn, run_id);

    let epoch_before = read_epoch_or_zero(&conn, run_id);
    let ops_before = count_operations(&conn, run_id);

    // Prepare captures None (no checkpoint); the hook adds one so reserve sees Some.
    let hook_run_id = run_id.to_string();
    let observer = PhaseHookObserver::new();
    observer.set_hook(move |conn: &Connection| {
        seed_checkpoint(conn, &hook_run_id, "step1");
    });

    let request = recovery_request(run_id, "step1", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingRecoveryExecutor::success();

    let result = protocol.recover_with_observer_and_executor(
        &conn,
        Path::new("."),
        &request,
        &observer,
        &executor,
    );
    assert!(
        matches!(result, Err(RecoveryError::AuthorityChanged)),
        "checkpoint None→Some must return AuthorityChanged [B1]"
    );
    assert_no_mutation(&conn, run_id, epoch_before, ops_before);
}

/// GIVEN: a prepared recovery whose checkpoint was removed (Some → None)
/// WHEN: reserve revalidates the authority snapshot
/// THEN: returns `AuthorityChanged` with no mutation [B1]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
/// @requirement:REQ-RP-004
#[test]
fn checkpoint_some_to_none_between_prepare_and_reserve_returns_authority_changed() {
    let conn = recovery_conn();
    let run_id = "run-p11-checkpoint-some-to-none";
    seed_checkpoint(&conn, run_id, "step1");
    persisted_capsule(&conn, run_id);
    seed_run(&conn, run_id);

    let epoch_before = read_epoch_or_zero(&conn, run_id);
    let ops_before = count_operations(&conn, run_id);

    // Prepare captures Some (checkpoint exists); the hook deletes it so reserve sees None.
    let hook_run_id = run_id.to_string();
    let observer = PhaseHookObserver::new();
    observer.set_hook(move |conn: &Connection| {
        conn.execute(
            "DELETE FROM checkpoints WHERE run_id = ?1",
            params![hook_run_id],
        )
        .expect("delete checkpoint between phases");
    });

    let request = recovery_request(run_id, "step1", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingRecoveryExecutor::success();

    let result = protocol.recover_with_observer_and_executor(
        &conn,
        Path::new("."),
        &request,
        &observer,
        &executor,
    );
    assert!(
        matches!(result, Err(RecoveryError::AuthorityChanged)),
        "checkpoint Some→None must return AuthorityChanged [B1]"
    );
    assert_no_mutation(&conn, run_id, epoch_before, ops_before);
}

/// GIVEN: a prepared recovery whose wait_state changed between prepare/reserve
/// WHEN: reserve revalidates the authority snapshot
/// THEN: returns `AuthorityChanged` with no mutation [B1]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
/// @requirement:REQ-RP-004
#[test]
fn wait_state_change_between_prepare_and_reserve_returns_authority_changed() {
    let conn = recovery_conn();
    let run_id = "run-p11-wait-change";
    seed_wait_state(&conn, run_id, "susp-prepare");
    persisted_capsule(&conn, run_id);
    seed_run(&conn, run_id);

    let epoch_before = read_epoch_or_zero(&conn, run_id);
    let ops_before = count_operations(&conn, run_id);

    // Between prepare and reserve, change the suspension id so the wait state
    // identity differs.
    let hook_run_id = run_id.to_string();
    let observer = PhaseHookObserver::new();
    observer.set_hook(move |conn: &Connection| {
        seed_wait_state(conn, &hook_run_id, "susp-reserve-different");
    });

    let request = recovery_request(run_id, "step1", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingRecoveryExecutor::success();

    let result = protocol.recover_with_observer_and_executor(
        &conn,
        Path::new("."),
        &request,
        &observer,
        &executor,
    );
    assert!(
        matches!(result, Err(RecoveryError::AuthorityChanged)),
        "wait_state change must return AuthorityChanged [B1]"
    );
    assert_no_mutation(&conn, run_id, epoch_before, ops_before);
}

/// GIVEN: a prepared recovery whose wait_state was added (None → Some)
/// WHEN: reserve revalidates the authority snapshot
/// THEN: returns `AuthorityChanged` with no mutation [B1]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
/// @requirement:REQ-RP-004
#[test]
fn wait_state_none_to_some_between_prepare_and_reserve_returns_authority_changed() {
    let conn = recovery_conn();
    let run_id = "run-p11-wait-none-to-some";
    persisted_capsule(&conn, run_id);
    seed_run(&conn, run_id);

    let epoch_before = read_epoch_or_zero(&conn, run_id);
    let ops_before = count_operations(&conn, run_id);

    let hook_run_id = run_id.to_string();
    let observer = PhaseHookObserver::new();
    observer.set_hook(move |conn: &Connection| {
        seed_wait_state(conn, &hook_run_id, "susp-new");
    });

    let request = recovery_request(run_id, "step1", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingRecoveryExecutor::success();

    let result = protocol.recover_with_observer_and_executor(
        &conn,
        Path::new("."),
        &request,
        &observer,
        &executor,
    );
    assert!(
        matches!(result, Err(RecoveryError::AuthorityChanged)),
        "wait_state None→Some must return AuthorityChanged [B1]"
    );
    assert_no_mutation(&conn, run_id, epoch_before, ops_before);
}

/// GIVEN: a prepared recovery whose process_pid (live_pid) changed
/// WHEN: reserve revalidates the authority snapshot
/// THEN: returns `AuthorityChanged` with no mutation [B1]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
/// @requirement:REQ-RP-004
#[test]
fn live_pid_change_between_prepare_and_reserve_returns_authority_changed() {
    let conn = recovery_conn();
    let run_id = "run-p11-pid-change";
    persisted_capsule(&conn, run_id);
    let mut metadata = starting_metadata(run_id);
    metadata.process_pid = Some(1111);
    persist_run_with_conn(&conn, &metadata).expect("seed run with pid");

    let epoch_before = read_epoch_or_zero(&conn, run_id);
    let ops_before = count_operations(&conn, run_id);

    let hook_run_id = run_id.to_string();
    let observer = PhaseHookObserver::new();
    observer.set_hook(move |conn: &Connection| {
        let mut m = starting_metadata(&hook_run_id);
        m.process_pid = Some(9999);
        persist_run_with_conn(conn, &m).expect("change pid between phases");
    });

    let request = recovery_request(run_id, "step1", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingRecoveryExecutor::success();

    let result = protocol.recover_with_observer_and_executor(
        &conn,
        Path::new("."),
        &request,
        &observer,
        &executor,
    );
    assert!(
        matches!(result, Err(RecoveryError::AuthorityChanged)),
        "live_pid change must return AuthorityChanged [B1]"
    );
    assert_no_mutation(&conn, run_id, epoch_before, ops_before);
}

/// GIVEN: a prepared recovery whose live_pid went from Some → None
/// WHEN: reserve revalidates the authority snapshot
/// THEN: returns `AuthorityChanged` with no mutation [B1]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
/// @requirement:REQ-RP-004
#[test]
fn live_pid_some_to_none_between_prepare_and_reserve_returns_authority_changed() {
    let conn = recovery_conn();
    let run_id = "run-p11-pid-some-to-none";
    persisted_capsule(&conn, run_id);
    let mut metadata = starting_metadata(run_id);
    metadata.process_pid = Some(2222);
    persist_run_with_conn(&conn, &metadata).expect("seed run with pid");

    let epoch_before = read_epoch_or_zero(&conn, run_id);
    let ops_before = count_operations(&conn, run_id);

    let hook_run_id = run_id.to_string();
    let observer = PhaseHookObserver::new();
    observer.set_hook(move |conn: &Connection| {
        let mut m = starting_metadata(&hook_run_id);
        m.process_pid = None;
        persist_run_with_conn(conn, &m).expect("clear pid between phases");
    });

    let request = recovery_request(run_id, "step1", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingRecoveryExecutor::success();

    let result = protocol.recover_with_observer_and_executor(
        &conn,
        Path::new("."),
        &request,
        &observer,
        &executor,
    );
    assert!(
        matches!(result, Err(RecoveryError::AuthorityChanged)),
        "live_pid Some→None must return AuthorityChanged [B1]"
    );
    assert_no_mutation(&conn, run_id, epoch_before, ops_before);
}

/// GIVEN: a prepared recovery whose current_step changed
/// WHEN: reserve revalidates the authority snapshot
/// THEN: returns `AuthorityChanged` with no mutation [B1]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
/// @requirement:REQ-RP-004
#[test]
fn current_step_change_between_prepare_and_reserve_returns_authority_changed() {
    let conn = recovery_conn();
    let run_id = "run-p11-step-change";
    persisted_capsule(&conn, run_id);
    seed_run(&conn, run_id);

    let epoch_before = read_epoch_or_zero(&conn, run_id);
    let ops_before = count_operations(&conn, run_id);

    let hook_run_id = run_id.to_string();
    let observer = PhaseHookObserver::new();
    observer.set_hook(move |conn: &Connection| {
        let mut m = starting_metadata(&hook_run_id);
        m.current_step = Some("step2-advanced".to_string());
        persist_run_with_conn(conn, &m).expect("change current_step between phases");
    });

    let request = recovery_request(run_id, "step1", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingRecoveryExecutor::success();

    let result = protocol.recover_with_observer_and_executor(
        &conn,
        Path::new("."),
        &request,
        &observer,
        &executor,
    );
    assert!(
        matches!(result, Err(RecoveryError::AuthorityChanged)),
        "current_step change must return AuthorityChanged [B1]"
    );
    assert_no_mutation(&conn, run_id, epoch_before, ops_before);
}

/// GIVEN: a prepared recovery whose current_step went from Some → None
/// WHEN: reserve revalidates the authority snapshot
/// THEN: returns `AuthorityChanged` with no mutation [B1]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
/// @requirement:REQ-RP-004
#[test]
fn current_step_some_to_none_between_prepare_and_reserve_returns_authority_changed() {
    let conn = recovery_conn();
    let run_id = "run-p11-step-some-to-none";
    persisted_capsule(&conn, run_id);
    seed_run(&conn, run_id);

    let epoch_before = read_epoch_or_zero(&conn, run_id);
    let ops_before = count_operations(&conn, run_id);

    let hook_run_id = run_id.to_string();
    let observer = PhaseHookObserver::new();
    observer.set_hook(move |conn: &Connection| {
        let mut m = starting_metadata(&hook_run_id);
        m.current_step = None;
        persist_run_with_conn(conn, &m).expect("clear current_step between phases");
    });

    let request = recovery_request(run_id, "step1", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingRecoveryExecutor::success();

    let result = protocol.recover_with_observer_and_executor(
        &conn,
        Path::new("."),
        &request,
        &observer,
        &executor,
    );
    assert!(
        matches!(result, Err(RecoveryError::AuthorityChanged)),
        "current_step Some→None must return AuthorityChanged [B1]"
    );
    assert_no_mutation(&conn, run_id, epoch_before, ops_before);
}

/// GIVEN: a prepared recovery whose issue lease status changed
/// WHEN: reserve revalidates the authority snapshot
/// THEN: returns `AuthorityChanged` with no mutation [B1]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
/// @requirement:REQ-RP-004
#[test]
fn lease_change_between_prepare_and_reserve_returns_authority_changed() {
    let conn = recovery_conn();
    let run_id = "run-p11-lease-change";
    seed_issue_run(&conn, run_id);
    seed_lease(&conn, run_id, LeaseStatus::Running);
    persisted_capsule(&conn, run_id);

    let epoch_before = read_epoch_or_zero(&conn, run_id);
    let ops_before = count_operations(&conn, run_id);

    let hook_run_id = run_id.to_string();
    let observer = PhaseHookObserver::new();
    observer.set_hook(move |conn: &Connection| {
        // Change the lease status from Running to WaitingExternal.
        luther_workflow::persistence::update_lease_status(
            conn,
            &format!("lease-{hook_run_id}"),
            LeaseStatus::WaitingExternal,
            Some(&hook_run_id),
        )
        .expect("change lease status between phases");
    });

    let request = recovery_request(run_id, "step1", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingRecoveryExecutor::success();

    let result = protocol.recover_with_observer_and_executor(
        &conn,
        Path::new("."),
        &request,
        &observer,
        &executor,
    );
    assert!(
        matches!(result, Err(RecoveryError::AuthorityChanged)),
        "lease change must return AuthorityChanged [B1]"
    );
    assert_no_mutation(&conn, run_id, epoch_before, ops_before);
}

/// GIVEN: a prepared recovery whose issue lease was removed (Some → None)
/// WHEN: reserve revalidates the authority snapshot
/// THEN: returns `AuthorityChanged` with no mutation [B1]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
/// @requirement:REQ-RP-004
#[test]
fn lease_some_to_none_between_prepare_and_reserve_returns_authority_changed() {
    let conn = recovery_conn();
    let run_id = "run-p11-lease-some-to-none";
    seed_issue_run(&conn, run_id);
    seed_lease(&conn, run_id, LeaseStatus::Running);
    persisted_capsule(&conn, run_id);

    let epoch_before = read_epoch_or_zero(&conn, run_id);
    let ops_before = count_operations(&conn, run_id);

    let hook_run_id = run_id.to_string();
    let observer = PhaseHookObserver::new();
    observer.set_hook(move |conn: &Connection| {
        conn.execute(
            "DELETE FROM issue_leases WHERE run_id = ?1",
            params![hook_run_id],
        )
        .expect("delete lease between phases");
    });

    let request = recovery_request(run_id, "step1", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingRecoveryExecutor::success();

    let result = protocol.recover_with_observer_and_executor(
        &conn,
        Path::new("."),
        &request,
        &observer,
        &executor,
    );
    assert!(
        matches!(result, Err(RecoveryError::AuthorityChanged)),
        "lease Some→None must return AuthorityChanged [B1]"
    );
    assert_no_mutation(&conn, run_id, epoch_before, ops_before);
}

/// GIVEN: a prepared recovery with NO authority changes between prepare/reserve
/// WHEN: reserve revalidates the authority snapshot
/// THEN: recovery proceeds normally (Recovered) — the snapshot matched exactly [B1]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
/// @requirement:REQ-RP-004
#[test]
fn unchanged_authority_proceeds_to_recovered() {
    let conn = recovery_conn();
    let run_id = "run-p11-unchanged";
    seed_checkpoint(&conn, run_id, "step1");
    seed_wait_state(&conn, run_id, "susp-stable");
    seed_issue_run(&conn, run_id);
    seed_lease(&conn, run_id, LeaseStatus::Running);
    persisted_capsule(&conn, run_id);

    let epoch_before = read_epoch_or_zero(&conn, run_id);
    let ops_before = count_operations(&conn, run_id);

    // No-op observer: no authority change between phases.
    let observer = PhaseHookObserver::new();
    let request = recovery_request(run_id, "step1", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingRecoveryExecutor::success();

    let outcome = protocol
        .recover_with_observer_and_executor(&conn, Path::new("."), &request, &observer, &executor)
        .expect("unchanged authority must proceed [B1]");
    assert!(
        matches!(outcome, RecoveryOutcome::Recovered { .. }),
        "unchanged authority must reach Recovered [B1]"
    );
    assert!(
        executor.was_called(),
        "executor must be called when authority matched [C12]"
    );
    // Epoch advanced and operation/attempt were allocated.
    let epoch_after = read_epoch_or_zero(&conn, run_id);
    assert_eq!(
        epoch_after,
        epoch_before + 1,
        "epoch must advance on a successful recovery [B2]"
    );
    let ops_after = count_operations(&conn, run_id);
    assert_eq!(
        ops_after,
        ops_before + 1,
        "exactly one operation row must be created [B3]"
    );
}

/// GIVEN: a successful recovery where the reserved attempt/operation use the
///        request step_id (not the run_id)
/// WHEN: the durable records are inspected
/// THEN: the attempt and operation `step_id` columns carry the request step_id [C5/C12]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
/// @requirement:REQ-RP-001
#[test]
fn reserved_attempt_and_operation_use_request_step_id() {
    let conn = recovery_conn();
    let run_id = "run-p11-step-id";
    persisted_capsule(&conn, run_id);
    seed_run(&conn, run_id);

    let request = recovery_request(run_id, "step1", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingRecoveryExecutor::success();

    let outcome = protocol
        .recover_with_executor(&conn, Path::new("."), &request, &executor)
        .expect("recover must succeed");
    let (operation_id, attempt_id) = match outcome {
        RecoveryOutcome::Recovered {
            resumed_at_step,
            attempt_id,
            operation_id,
        } => {
            assert_eq!(
                resumed_at_step, "step1",
                "Recovered.resumed_at_step must be the request step_id, not run_id [C5/C12]"
            );
            (operation_id, attempt_id)
        }
        other => panic!("expected Recovered, got {other:?}"),
    };

    // The durable operation row must carry the request step_id.
    let op_step_id: String = conn
        .query_row(
            "SELECT step_id FROM recovery_operations WHERE operation_id = ?1",
            params![operation_id],
            |row| row.get(0),
        )
        .expect("query operation step_id");
    assert_eq!(
        op_step_id, "step1",
        "operation step_id must be the request step_id, not run_id [C5/C12]"
    );

    // The durable attempt row must carry the request step_id.
    let attempt_step_id: String = conn
        .query_row(
            "SELECT step_id FROM recovery_attempts WHERE attempt_id = ?1",
            params![attempt_id],
            |row| row.get(0),
        )
        .expect("query attempt step_id");
    assert_eq!(
        attempt_step_id, "step1",
        "attempt step_id must be the request step_id, not run_id [C5/C12]"
    );

    // The executor invocation must also carry the request step_id.
    let call = executor
        .first_call()
        .expect("executor must record the invocation [C12]");
    assert_eq!(
        call.step_id, "step1",
        "executor invocation step_id must be the request step_id, not run_id [C5/C12]"
    );
}

/// GIVEN: a run with issue + repository anchors and a durable lease
/// WHEN: `get_lease_for_run` is queried
/// THEN: the lease is found by run_id [B1]
///
/// GREEN: exercises the durable lease-by-run_id lookup added for P11.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
/// @requirement:REQ-RP-004
#[test]
fn get_lease_for_run_finds_issue_anchored_lease() {
    let conn = recovery_conn();
    let run_id = "run-p11-lease-lookup";
    seed_issue_run(&conn, run_id);
    seed_lease(&conn, run_id, LeaseStatus::Running);

    let lease = get_lease_for_run(&conn, run_id)
        .expect("query")
        .expect("lease must be found by run_id");
    assert_eq!(lease.run_id.as_deref(), Some(run_id));
    assert_eq!(lease.status, LeaseStatus::Running);
}
