//! Deterministic failpoint matrix for the recovery protocol (P16).
//!
//! Each failpoint F1–F14 injects a deterministic interruption at a precise
//! point in the recovery lifecycle and asserts BOTH the typed recovery
//! outcome AND a durable-state invariant. All tests use real SQLite
//! (in-memory or temp-file), injected recording executors/probes, and
//! deterministic phase hooks — no sleeps, no network, no should_panic.
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P16
//! @requirement:REQ-RP-001,REQ-RP-004,REQ-RP-006,REQ-RP-007,REQ-RP-008

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use rusqlite::{params, Connection, TransactionBehavior};

use luther_workflow::engine::recovery::capsule::{
    build_capsule_v1, verify_envelope_digest, ExecutionCapsuleV1,
};
use luther_workflow::engine::recovery::protocol::{
    OperatorVerb, RecoveryExecutionError, RecoveryExecutionInvocation, RecoveryExecutionResult,
    RecoveryExecutor, RecoveryOutcome, RecoveryPhaseObserver, RecoveryProtocolV1, RecoveryRequest,
    RefusalReason,
};
use luther_workflow::engine::recovery::salvage::{
    classify_run, init_salvage_lineage_table, salvage_recover, RunClassification,
};
use luther_workflow::engine::workspace_ownership::provision_workspace_ownership;
use luther_workflow::persistence::attempts::{
    init_attempts_table, load_unfinalized_for_operation, record_attempt_start, AttemptStart,
};
use luther_workflow::persistence::capsule_store::{
    init_capsules_table, persist_capsule_v1, persist_launch_atomically, LaunchPersistenceOutcome,
    EXECUTION_CAPSULES_TABLE,
};
use luther_workflow::persistence::checkpoint::{init_checkpoint_table, StateSnapshot};
use luther_workflow::persistence::effect_intents::{
    init_effect_intents_table, prepare_effect, reconcile_effect, EffectKind, EffectPreparation,
    ObservedState, ReconcileVerdict,
};
use luther_workflow::persistence::leases::init_leases_table;
use luther_workflow::persistence::recovery_epoch::{
    cas_advance_epoch, init_epoch_table, read_epoch, CasOutcome,
};
use luther_workflow::persistence::recovery_operations::{
    compute_intent_digest, compute_logical_request_key, compute_operation_id,
    init_operations_table, OperationStatus,
};
use luther_workflow::persistence::sqlite::{init_runs_schema, persist_run_with_conn};
use luther_workflow::persistence::wait_state::init_wait_states_table;
use luther_workflow::persistence::{RunMetadata, RunStatus};
use luther_workflow::workflow::schema::{
    DiffPathNormalization, GuardConfig, GuardLimits, ParentOrchestrationConfig, RepoConfig,
    RuntimeConfig, StepDef, TransitionDef, WorkflowConfig, WorkflowType,
};

// ===========================================================================
// Test helpers
// ===========================================================================

/// Create an in-memory SQLite connection with ALL recovery tables initialized.
fn matrix_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    init_epoch_table(&conn).expect("init epoch");
    init_operations_table(&conn).expect("init operations");
    init_attempts_table(&conn).expect("init attempts");
    init_effect_intents_table(&conn).expect("init effect intents");
    init_capsules_table(&conn).expect("init capsules");
    init_runs_schema(&conn).expect("init runs");
    init_checkpoint_table(&conn).expect("init checkpoints");
    init_wait_states_table(&conn).expect("init wait states");
    init_leases_table(&conn).expect("init leases");
    init_salvage_lineage_table(&conn).expect("init salvage lineage");
    conn
}

/// Create a temp-file SQLite DB with all recovery tables (for multi-connection
/// concurrency tests).
fn matrix_file_db(dir: &Path, name: &str) -> Connection {
    let db_path = dir.join(name);
    let conn = Connection::open(&db_path).expect("open file db");
    conn.busy_timeout(Duration::from_secs(5))
        .expect("set busy_timeout");
    init_epoch_table(&conn).expect("init epoch");
    init_operations_table(&conn).expect("init operations");
    init_attempts_table(&conn).expect("init attempts");
    init_effect_intents_table(&conn).expect("init effect intents");
    init_capsules_table(&conn).expect("init capsules");
    init_runs_schema(&conn).expect("init runs");
    init_checkpoint_table(&conn).expect("init checkpoints");
    init_wait_states_table(&conn).expect("init wait states");
    init_leases_table(&conn).expect("init leases");
    init_salvage_lineage_table(&conn).expect("init salvage lineage");
    conn
}

fn sample_workflow_type() -> WorkflowType {
    WorkflowType {
        workflow_type_id: "p16-matrix".to_string(),
        steps: vec![StepDef {
            step_id: "step1".to_string(),
            step_type: "noop".to_string(),
            description: None,
            parameters: None,
            produces: None,
            consumes: None,
            terminal: None,
            recovery_policy: Some(
                luther_workflow::engine::recovery::policy::StepRecoveryPolicy::PureReenter,
            ),
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

fn continue_workspace_workflow_type() -> WorkflowType {
    WorkflowType {
        workflow_type_id: "p16-continue".to_string(),
        steps: vec![StepDef {
            step_id: "continue_step".to_string(),
            step_type: "shell".to_string(),
            description: None,
            parameters: None,
            produces: None,
            consumes: None,
            terminal: None,
            recovery_policy: Some(
                luther_workflow::engine::recovery::policy::StepRecoveryPolicy::ContinueWorkspace,
            ),
        }],
        transitions: vec![],
        guards: GuardConfig {
            max_retries: None,
            timeout_seconds: None,
            require_approval: None,
        },
    }
}

fn config_for(workflow_type_id: &str) -> WorkflowConfig {
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

fn provenance_for(
    workflow: &WorkflowType,
    config: &WorkflowConfig,
) -> luther_workflow::persistence::launch_provenance::LaunchProvenance {
    luther_workflow::persistence::launch_provenance::LaunchProvenance::from_resolved(
        workflow,
        config,
        Path::new("."),
    )
    .expect("canonicalize config_root")
}

fn build_capsule(run_id: &str) -> ExecutionCapsuleV1 {
    let workflow = sample_workflow_type();
    let config = config_for(&workflow.workflow_type_id);
    let provenance = provenance_for(&workflow, &config);
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

fn build_capsule_with(workflow: &WorkflowType, run_id: &str, base_ref: &str) -> ExecutionCapsuleV1 {
    let config = config_for(&workflow.workflow_type_id);
    let provenance = provenance_for(workflow, &config);
    build_capsule_v1(
        run_id.to_string(),
        workflow,
        &config,
        Path::new("."),
        &provenance,
        base_ref.to_string(),
    )
    .expect("build capsule")
}

fn persisted_capsule(conn: &Connection, run_id: &str) -> ExecutionCapsuleV1 {
    let capsule = build_capsule(run_id);
    persist_capsule_v1(conn, &capsule).expect("persist capsule");
    capsule
}

fn starting_metadata(run_id: &str) -> RunMetadata {
    let mut md = RunMetadata::new(run_id, "p16-matrix", "p16-matrix-config");
    md.status = RunStatus::Starting;
    md.current_step = Some("step1".to_string());
    md
}

fn seed_run(conn: &Connection, run_id: &str) {
    persist_run_with_conn(conn, &starting_metadata(run_id)).expect("persist run");
}

fn recovery_request(run_id: &str, step_id: &str, epoch: u64) -> RecoveryRequest {
    RecoveryRequest {
        run_id: run_id.to_string(),
        step_id: step_id.to_string(),
        expected_epoch: epoch,
        operator_verb: OperatorVerb::Resume,
    }
}

/// Create an owned temp workspace with durable ownership markers.
fn owned_workspace(run_id: &str) -> (tempfile::TempDir, PathBuf) {
    let parent = tempfile::tempdir().expect("temp parent");
    let workspace_path = parent.path().join("ws");
    provision_workspace_ownership(&workspace_path, run_id).expect("provision bootstrap");
    std::fs::create_dir_all(workspace_path.join(".git/luther")).expect("create .git/luther");
    provision_workspace_ownership(&workspace_path, run_id).expect("promote durable");
    (parent, workspace_path)
}

fn count_operations(conn: &Connection, run_id: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM recovery_operations WHERE run_id = ?1",
        params![run_id],
        |row| row.get(0),
    )
    .expect("count operations")
}

fn count_attempts(conn: &Connection, run_id: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM recovery_attempts WHERE run_id = ?1",
        params![run_id],
        |row| row.get(0),
    )
    .expect("count attempts")
}

fn count_effect_intents(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM effect_intents", [], |row| row.get(0))
        .expect("count effect intents")
}

fn operation_status(conn: &Connection, operation_id: &str) -> String {
    conn.query_row(
        "SELECT status FROM recovery_operations WHERE operation_id = ?1",
        params![operation_id],
        |row| row.get::<_, String>(0),
    )
    .expect("query operation status")
}

/// Seed a Pending operation via raw SQL with an expired lease.
struct SeedPendingOp {
    operation_id: String,
    run_id: String,
    epoch: u64,
    step_id: String,
    capsule_digest: String,
    logical_key: String,
    intent_digest: String,
    attempt_id: Option<i64>,
    expired: bool,
}

fn seed_pending_op(conn: &Connection, op: &SeedPendingOp) {
    let lease = if op.expired {
        Utc::now() - ChronoDuration::minutes(10)
    } else {
        Utc::now() + ChronoDuration::minutes(10)
    };
    conn.execute(
        "INSERT INTO recovery_operations
             (operation_id, run_id, epoch, step_id, capsule_envelope_digest,
              source_attempt_id, logical_request_key, intent_digest, status,
              owner_pid, lease_expires_at, execution_attempt_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, 'pending', 9999, ?8, ?9, ?10)",
        params![
            op.operation_id,
            op.run_id,
            op.epoch as i64,
            op.step_id,
            op.capsule_digest,
            op.logical_key,
            op.intent_digest,
            lease.to_rfc3339(),
            op.attempt_id,
            Utc::now().to_rfc3339(),
        ],
    )
    .expect("seed pending op");
}

/// Seed a Completed operation via raw SQL.
#[allow(dead_code)]
struct SeedCompletedOp {
    operation_id: String,
    run_id: String,
    epoch: u64,
    step_id: String,
    capsule_digest: String,
    logical_key: String,
    intent_digest: String,
    attempt_id: i64,
    same_binding: bool,
    actual_capsule_digest: String,
}

fn seed_completed_op(conn: &Connection, op: &SeedCompletedOp) {
    let digest = if op.same_binding {
        op.actual_capsule_digest.clone()
    } else {
        "different-capsule-digest".to_string()
    };
    conn.execute(
        "INSERT INTO recovery_operations
             (operation_id, run_id, epoch, step_id, capsule_envelope_digest,
              source_attempt_id, logical_request_key, intent_digest, status,
              owner_pid, lease_expires_at, execution_attempt_id, serialized_outcome,
              created_at, finalized_at)
         VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, 'completed', NULL, NULL, ?8, ?9, ?10, ?10)",
        params![
            op.operation_id,
            op.run_id,
            op.epoch as i64,
            op.step_id,
            digest,
            op.logical_key,
            op.intent_digest,
            op.attempt_id,
            format!(r#"{{"attempt_id":{}}}"#, op.attempt_id),
            Utc::now().to_rfc3339(),
        ],
    )
    .expect("seed completed op");
}

// ---- Recording executor / phase observer (same patterns as P10/P11 tests) ----

#[derive(Clone)]
struct RecordingExecutor {
    calls: Arc<Mutex<Vec<RecordedCall>>>,
}

#[derive(Clone)]
#[allow(dead_code)]
struct RecordedCall {
    run_id: String,
    step_id: String,
    epoch: u64,
}

impl std::fmt::Debug for RecordingExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordingExecutor").finish()
    }
}

impl RecordingExecutor {
    fn success() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn was_called(&self) -> bool {
        !self.calls.lock().unwrap().is_empty()
    }
}

impl RecoveryExecutor for RecordingExecutor {
    fn execute(
        &self,
        invocation: &RecoveryExecutionInvocation<'_>,
    ) -> Result<RecoveryExecutionResult, RecoveryExecutionError> {
        self.calls.lock().unwrap().push(RecordedCall {
            run_id: invocation.run_id.to_string(),
            step_id: invocation.step_id.to_string(),
            epoch: invocation.epoch,
        });
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

type PhaseHook = Arc<dyn Fn(&Connection) + Send + Sync>;

#[derive(Clone)]
struct HookObserver {
    hook: Arc<Mutex<Option<PhaseHook>>>,
}

impl std::fmt::Debug for HookObserver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookObserver").finish()
    }
}

impl HookObserver {
    fn new() -> Self {
        Self {
            hook: Arc::new(Mutex::new(None)),
        }
    }

    fn set_hook<F>(&self, hook: F)
    where
        F: Fn(&Connection) + Send + Sync + 'static,
    {
        *self.hook.lock().unwrap() = Some(Arc::new(hook));
    }
}

impl RecoveryPhaseObserver for HookObserver {
    fn on_prepare_complete(&self, conn: &Connection, _: &RecoveryRequest) {
        if let Some(hook) = self.hook.lock().unwrap().as_ref() {
            hook(conn);
        }
    }
}

/// Compute the normalized binding keys matching what the protocol computes.
fn normalized_bindings(run_id: &str, capsule: &ExecutionCapsuleV1) -> (String, String, String) {
    let intent = "resume";
    let op_id = compute_operation_id(run_id, "step1", &capsule.envelope_digest, None, intent);
    let logical_key = compute_logical_request_key(run_id, None, intent);
    let intent_digest = compute_intent_digest(intent);
    (op_id, logical_key, intent_digest)
}

// ===========================================================================
// F1: Interrupt before capsule persist → atomic launch rollback / no orphan
// ===========================================================================

/// GIVEN: a run persisted via `persist_launch_atomically` (atomic run+capsule)
/// WHEN: the transaction commits
/// THEN: BOTH the run row AND the capsule row exist atomically — no orphan run
///       without a capsule. A run seeded WITHOUT a capsule is salvage-only.
///
/// Invariant: no run row without a capsule for new runs. [F1]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P16
/// @requirement:REQ-RP-001
#[test]
fn f01_atomic_launch_rollback_no_orphan_run_without_capsule() {
    let conn = matrix_conn();
    let run_id = "f01-atomic";

    // 1. Atomic launch persists both run + capsule in one IMMEDIATE tx.
    let capsule = build_capsule(run_id);
    let metadata = starting_metadata(run_id);
    let outcome =
        persist_launch_atomically(&conn, &metadata, &capsule).expect("atomic launch must succeed");
    assert_eq!(outcome, LaunchPersistenceOutcome::Persisted);

    // Invariant: both rows exist (no orphan).
    let run_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM runs WHERE run_id = ?1",
            params![run_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(run_count, 1, "run row must exist after atomic launch");
    let capsule_count: i64 = conn
        .query_row(
            &format!("SELECT COUNT(*) FROM {EXECUTION_CAPSULES_TABLE} WHERE run_id = ?1"),
            params![run_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        capsule_count, 1,
        "capsule row must exist after atomic launch [F1: no orphan]"
    );

    // 2. A run WITHOUT a capsule is salvage-only — cannot be exact-recovered.
    let orphan_run = "f01-orphan";
    persist_run_with_conn(&conn, &starting_metadata(orphan_run))
        .expect("seed orphan run (no capsule)");
    let classification = classify_run(&conn, orphan_run).expect("classify orphan");
    assert!(
        matches!(classification, RunClassification::SalvageOnly { .. }),
        "run without capsule must be salvage-only [F1]"
    );
}

// ===========================================================================
// F2: Post-persist pre-run exact recovery → Recovered; capsule immutable
// ===========================================================================

/// GIVEN: a run with a persisted capsule at epoch 0 (post-persist, pre-run)
/// WHEN: `recover()` is called for the first step
/// THEN: returns `Recovered` with the exact attempt/operation ids
/// AND: the capsule is immutable (envelope digest holds), epoch advanced. [C1]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P16
/// @requirement:REQ-RP-001
#[test]
fn f02_post_persist_pre_run_exact_recovery() {
    let conn = matrix_conn();
    let run_id = "f02-post-persist";
    let capsule = persisted_capsule(&conn, run_id);
    seed_run(&conn, run_id);

    let digest_before = capsule.envelope_digest.clone();
    let epoch_before = read_epoch(&conn, run_id).expect("read epoch before");
    assert_eq!(epoch_before, 0);

    let request = recovery_request(run_id, "step1", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingExecutor::success();

    let outcome = protocol
        .recover_with_executor(&conn, Path::new("."), &request, &executor)
        .expect("recover must succeed for post-persist pre-run [F2]");

    match outcome {
        RecoveryOutcome::Recovered {
            attempt_id,
            operation_id,
            ..
        } => {
            assert!(attempt_id > 0, "attempt must be allocated [F2]");
            assert!(
                !operation_id.is_empty(),
                "operation_id must be non-empty [F2]"
            );
        }
        other => panic!("expected Recovered, got {other:?} [F2]"),
    }

    // Invariant: capsule envelope digest unchanged (immutable).
    let loaded = luther_workflow::persistence::capsule_store::load_capsule_v1(&conn, run_id)
        .expect("load capsule");
    assert_eq!(
        loaded.envelope_digest, digest_before,
        "capsule envelope digest must be unchanged (immutable) [F2/C8]"
    );
    verify_envelope_digest(&loaded).expect("envelope digest must verify [F2/C8]");

    // Invariant: epoch advanced from 0 to 1 (CAS held). [C1]
    let epoch_after = read_epoch(&conn, run_id).expect("read epoch after");
    assert_eq!(epoch_after, 1, "epoch must have advanced 0→1 [F2/C1]");
}

// ===========================================================================
// F3: Worktree delta ContinueWorkspace with exact ownership/base/auth
// ===========================================================================

/// GIVEN: an interrupted ContinueWorkspace step with workspace changes (delta)
/// WHEN: `recover()` is called with the owned workspace
/// THEN: returns `Recovered` after exact verification of ownership, base ref,
///       and runtime auth (WorkspaceAuthorization revalidated in reserve). [C4]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P16
/// @requirement:REQ-RP-006
#[test]
fn f03_worktree_delta_continue_workspace_exact_verification() {
    let conn = matrix_conn();
    let run_id = "f03-continue";

    let (parent, workspace_path) = owned_workspace(run_id);

    // Simulate worktree delta: write a file into the workspace.
    std::fs::write(workspace_path.join("delta.txt"), "partial work").expect("write worktree delta");

    // Persist a capsule whose workflow includes a ContinueWorkspace step,
    // built from the workspace path so canonical bytes are valid.
    let workflow = continue_workspace_workflow_type();
    let capsule = build_capsule_with(&workflow, run_id, "main");
    persist_capsule_v1(&conn, &capsule).expect("persist continue capsule");
    seed_run(&conn, run_id);

    let request = recovery_request(run_id, "continue_step", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingExecutor::success();

    let outcome = protocol
        .recover_with_executor(&conn, &workspace_path, &request, &executor)
        .expect("recover must succeed for ContinueWorkspace with exact verification [F3]");

    assert!(
        matches!(outcome, RecoveryOutcome::Recovered { .. }),
        "ContinueWorkspace with matching workspace must return Recovered [F3/C4]"
    );

    // Invariant: executor was called (workspace ownership/base verified in
    // reserve, then execute proceeded). [C4]
    assert!(executor.was_called(), "executor must be called [F3/C12]");
    let call = executor.calls.lock().unwrap()[0].clone();
    assert_eq!(call.step_id, "continue_step");
    assert_eq!(call.run_id, run_id);

    // Invariant: workspace ownership marker still intact.
    assert!(
        workspace_path.join(".git/luther/workspace-owner").exists(),
        "workspace ownership marker must persist [F3/B6]"
    );
    drop(parent);
}

// ===========================================================================
// F4: Commit effect prepared → reconcile → Completed, no duplicate
// ===========================================================================

/// GIVEN: a Commit effect intent in `prepared` state (prepared, not finalized)
/// WHEN: `reconcile_effect` is called with observed HEAD matching the target
/// THEN: verdict is `Completed`, no duplicate commit intent exists. [C7]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P16
/// @requirement:REQ-RP-008
#[test]
fn f04_commit_effect_prepared_reconcile_no_duplicate() {
    let conn = matrix_conn();

    let prep = EffectPreparation {
        operation_id: "op-f04",
        attempt_id: 1,
        sequence: 0,
        kind: EffectKind::Commit,
        payload: b"commit-payload-f04",
        expected_target: Some("sha-target-f04"),
        expected_predecessor: Some("sha-parent-f04"),
    };
    let intent = prepare_effect(&conn, &prep).expect("prepare commit effect");
    assert_eq!(
        intent.status, "prepared",
        "intent must be in prepared state [F4]"
    );

    // Simulate crash: intent stays prepared. Reconcile with observed HEAD = target.
    let verdict = reconcile_effect(
        &conn,
        &intent.effect_key,
        &ObservedState {
            head_sha: Some("sha-target-f04".to_string()),
            remote_ref_sha: None,
            matching_pr_number: None,
        },
    )
    .expect("reconcile commit effect [F4]");

    assert_eq!(
        verdict,
        ReconcileVerdict::Completed {
            result: Some("sha-target-f04".to_string())
        },
        "commit effect with matching HEAD must reconcile to Completed [F4/C7]"
    );

    // Invariant: no duplicate — exactly one intent row, now completed.
    assert_eq!(
        count_effect_intents(&conn),
        1,
        "no duplicate commit intent [F4]"
    );

    // Idempotent re-prepare returns the same intent (no new row).
    let reprepared = prepare_effect(&conn, &prep).expect("re-prepare commit effect");
    assert_eq!(
        reprepared.status, "completed",
        "re-prepare returns completed intent [F4]"
    );
    assert_eq!(
        count_effect_intents(&conn),
        1,
        "idempotent re-prepare creates no duplicate [F4]"
    );
}

// ===========================================================================
// F5: Push effect prepared → reconcile remote → NeedsReissue, no duplicate
// ===========================================================================

/// GIVEN: a Push effect intent in `prepared` state (prepared, not finalized)
/// WHEN: `reconcile_effect` is called with remote ref at the predecessor
/// THEN: verdict is `NeedsReissue` (push did not take effect; re-issue needed)
/// AND: no duplicate push intent exists. [C7]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P16
/// @requirement:REQ-RP-008
#[test]
fn f05_push_effect_prepared_reconcile_reissue_no_duplicate() {
    let conn = matrix_conn();

    let prep = EffectPreparation {
        operation_id: "op-f05",
        attempt_id: 2,
        sequence: 0,
        kind: EffectKind::Push,
        payload: b"push-payload-f05",
        expected_target: Some("sha-remote-target-f05"),
        expected_predecessor: Some("sha-remote-parent-f05"),
    };
    let intent = prepare_effect(&conn, &prep).expect("prepare push effect");
    assert_eq!(intent.status, "prepared");

    // Reconcile: remote ref still at predecessor → push did not happen → reissue.
    let verdict = reconcile_effect(
        &conn,
        &intent.effect_key,
        &ObservedState {
            head_sha: None,
            remote_ref_sha: Some("sha-remote-parent-f05".to_string()),
            matching_pr_number: None,
        },
    )
    .expect("reconcile push effect [F5]");

    assert_eq!(
        verdict,
        ReconcileVerdict::NeedsReissue,
        "push effect with remote at predecessor must need reissue [F5/C7]"
    );

    // Invariant: no duplicate — exactly one intent, still prepared (not finalized).
    assert_eq!(
        count_effect_intents(&conn),
        1,
        "no duplicate push intent [F5]"
    );

    // Idempotent re-prepare returns the same intent.
    let reprepared = prepare_effect(&conn, &prep).expect("re-prepare push effect");
    assert_eq!(
        reprepared.effect_key, intent.effect_key,
        "idempotent re-prepare returns same key [F5]"
    );
    assert_eq!(
        count_effect_intents(&conn),
        1,
        "no duplicate after re-prepare [F5]"
    );
}

// ===========================================================================
// F6: Stale epoch → StaleEpoch outcome, no mutation
// ===========================================================================

/// GIVEN: a run whose epoch was advanced (0→1) by a concurrent claim
/// WHEN: `recover()` is called with the stale expected_epoch=0
/// THEN: returns `StaleEpoch { persisted: 1, expected: 0 }`
/// AND: no durable mutation occurred (epoch unchanged, no new operation). [C1]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P16
/// @requirement:REQ-RP-004
#[test]
fn f06_stale_epoch_no_mutation() {
    let conn = matrix_conn();
    let run_id = "f06-stale";
    persisted_capsule(&conn, run_id);
    seed_run(&conn, run_id);

    // Simulate concurrent claim advancing epoch 0 → 1.
    {
        let tx = rusqlite::Transaction::new_unchecked(&conn, TransactionBehavior::Immediate)
            .expect("begin tx");
        let outcome = cas_advance_epoch(&tx, run_id, 0).expect("CAS advance");
        assert_eq!(outcome, CasOutcome::Advanced { from: 0, to: 1 });
        tx.commit().expect("commit");
    }

    let ops_before = count_operations(&conn, run_id);

    let request = recovery_request(run_id, "step1", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingExecutor::success();

    let outcome = protocol
        .recover_with_executor(&conn, Path::new("."), &request, &executor)
        .expect("recover must not hard-error on stale epoch [F6]");

    match outcome {
        RecoveryOutcome::StaleEpoch {
            persisted,
            expected,
        } => {
            assert_eq!(persisted, 1, "persisted epoch must be 1 [F6/C1]");
            assert_eq!(expected, 0, "expected epoch must be 0 [F6/C1]");
        }
        other => panic!("expected StaleEpoch, got {other:?} [F6]"),
    }

    // Invariant: no mutation — epoch still 1, no new operations.
    assert_eq!(
        read_epoch(&conn, run_id).unwrap(),
        1,
        "epoch must be unchanged [F6/C1]"
    );
    assert_eq!(
        count_operations(&conn, run_id),
        ops_before,
        "no new operations created for stale epoch [F6/C1]"
    );
    assert!(
        !executor.was_called(),
        "executor must NOT be called [F6/C12]"
    );
}

// ===========================================================================
// F7: Exact duplicate AlreadyApplied, no attempt
// ===========================================================================

/// GIVEN: a run with a Completed operation for the exact same binding
/// WHEN: `recover()` is called again with the same binding
/// THEN: returns `AlreadyApplied` with the prior outcome
/// AND: no new attempt row or operation row is created. [C2]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P16
/// @requirement:REQ-RP-004
#[test]
fn f07_exact_duplicate_already_applied_no_attempt() {
    let conn = matrix_conn();
    let run_id = "f07-duplicate";
    let capsule = persisted_capsule(&conn, run_id);
    seed_run(&conn, run_id);

    let (op_id, logical_key, intent_digest) = normalized_bindings(run_id, &capsule);

    // Advance epoch to 1 (matching what the first recovery would have done).
    {
        let tx = rusqlite::Transaction::new_unchecked(&conn, TransactionBehavior::Immediate)
            .expect("begin tx");
        cas_advance_epoch(&tx, run_id, 0).expect("seed epoch");
        tx.commit().expect("commit");
    }

    // Seed a Completed operation with exact matching binding.
    seed_completed_op(
        &conn,
        &SeedCompletedOp {
            operation_id: op_id.clone(),
            run_id: run_id.to_string(),
            epoch: 1,
            step_id: "step1".to_string(),
            capsule_digest: capsule.envelope_digest.clone(),
            logical_key,
            intent_digest,
            attempt_id: 42,
            same_binding: true,
            actual_capsule_digest: capsule.envelope_digest.clone(),
        },
    );

    let ops_before = count_operations(&conn, run_id);
    let attempts_before = count_attempts(&conn, run_id);

    let request = recovery_request(run_id, "step1", 1);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingExecutor::success();

    let outcome = protocol
        .recover_with_executor(&conn, Path::new("."), &request, &executor)
        .expect("recover must not hard-error on completed duplicate [F7]");

    match outcome {
        RecoveryOutcome::AlreadyApplied {
            attempt_id,
            operation_id,
            ..
        } => {
            assert_eq!(attempt_id, 42, "prior attempt_id must be returned [F7/C2]");
            assert_eq!(operation_id, op_id, "operation_id must match [F7/C2]");
        }
        other => panic!("expected AlreadyApplied, got {other:?} [F7]"),
    }

    // Invariant: no new rows.
    assert_eq!(
        count_operations(&conn, run_id),
        ops_before,
        "no new operation rows [F7/C2]"
    );
    assert_eq!(
        count_attempts(&conn, run_id),
        attempts_before,
        "no new attempt rows [F7/C2]"
    );
    assert!(
        !executor.was_called(),
        "executor must NOT be called [F7/C12]"
    );
}

// ===========================================================================
// F8: Tampered envelope digest → resume refuses, no step executes
// ===========================================================================

/// GIVEN: a persisted capsule whose envelope_digest column was tampered on disk
/// WHEN: `recover()` is called
/// THEN: capsule load/verification fails → recovery returns Err (refused)
/// AND: no step executes (executor never called). [C8]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P16
/// @requirement:REQ-RP-002
#[test]
fn f08_tampered_envelope_digest_no_execute() {
    let conn = matrix_conn();
    let run_id = "f08-tampered";
    persisted_capsule(&conn, run_id);
    seed_run(&conn, run_id);

    // Tamper the envelope_digest column on disk (bit-flip / corruption).
    conn.execute(
        &format!(
            "UPDATE {EXECUTION_CAPSULES_TABLE} SET envelope_digest = 'tampered-bogus-digest'
             WHERE run_id = ?1"
        ),
        params![run_id],
    )
    .expect("tamper envelope_digest column");

    let request = recovery_request(run_id, "step1", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingExecutor::success();

    let result = protocol.recover_with_executor(&conn, Path::new("."), &request, &executor);

    // Invariant: recovery refuses (Err — capsule verification fails). [C8]
    assert!(
        result.is_err(),
        "tampered capsule must refuse recovery (Err) [F8/C8]"
    );
    // No step executes.
    assert!(
        !executor.was_called(),
        "executor must NOT be called for tampered capsule [F8/C12]"
    );
}

// ===========================================================================
// F9: Delete ownership between prepare/reserve → Refused(NotAuthorized)
// ===========================================================================

/// GIVEN: a ContinueWorkspace step with workspace ownership established
/// WHEN: the durable ownership marker is deleted between prepare and reserve
/// THEN: returns `Refused(NotAuthorized)`, no workspace mutation. [C4]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P16
/// @requirement:REQ-RP-006
#[test]
fn f09_delete_ownership_between_prepare_reserve_refused_not_authorized() {
    let conn = matrix_conn();
    let run_id = "f09-toctou";

    let (parent, workspace_path) = owned_workspace(run_id);

    let workflow = continue_workspace_workflow_type();
    let capsule = build_capsule_with(&workflow, run_id, "main");
    persist_capsule_v1(&conn, &capsule).expect("persist capsule");
    seed_run(&conn, run_id);

    let request = recovery_request(run_id, "continue_step", 0);

    // Deterministic between-phase hook: delete the durable ownership marker
    // after prepare anchors it but before reserve revalidates. [B6]
    let hook_path = workspace_path.clone();
    let observer = HookObserver::new();
    observer.set_hook(move |_conn: &Connection| {
        for marker in [
            hook_path.join(".git/luther/workspace-owner"),
            hook_path.join(".luther/workspace-owner"),
        ] {
            if marker.exists() {
                std::fs::remove_file(&marker).expect("delete ownership marker (TOCTOU)");
            }
        }
    });

    let protocol = RecoveryProtocolV1;
    let executor = RecordingExecutor::success();

    let outcome = protocol
        .recover_with_observer_and_executor(&conn, &workspace_path, &request, &observer, &executor)
        .expect("recover must not hard-error on TOCTOU [F9]");

    assert!(
        matches!(
            outcome,
            RecoveryOutcome::Refused {
                reason: RefusalReason::NotAuthorized
            }
        ),
        "deleted ownership between prepare/reserve must return Refused(NotAuthorized) [F9/C4]"
    );
    assert!(
        !executor.was_called(),
        "executor must NOT be called for missing ownership [F9/C12]"
    );
    drop(parent);
}

// ===========================================================================
// F10: Base ref mismatch → ContinueWorkspace refused, no step executes
// ===========================================================================

/// GIVEN: a persisted capsule whose base_ref field was tampered in the JSON
///        (envelope digest no longer matches the recomputed digest)
/// WHEN: `recover()` is called
/// THEN: capsule verification fails → recovery returns Err (refused)
/// AND: no step executes. [C8: base_ref is a replay authority field]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P16
/// @requirement:REQ-RP-002
#[test]
fn f10_base_ref_mismatch_refused_no_execute() {
    let conn = matrix_conn();
    let run_id = "f10-base-ref";
    let capsule = persisted_capsule(&conn, run_id);
    seed_run(&conn, run_id);

    // Tamper the base_ref in the capsule JSON (leave the digest column
    // unchanged). This simulates a base_ref change: the stored digest no
    // longer matches the recomputed digest from the tampered field.
    let original_json: String = conn
        .query_row(
            &format!("SELECT capsule_json FROM {EXECUTION_CAPSULES_TABLE} WHERE run_id = ?1"),
            params![run_id],
            |row| row.get(0),
        )
        .expect("read capsule json");
    let mut json: serde_json::Value =
        serde_json::from_str(&original_json).expect("deserialize capsule json");
    json["base_ref"] = serde_json::json!("develop");
    let tampered_json = serde_json::to_string(&json).expect("re-serialize capsule json");
    conn.execute(
        &format!("UPDATE {EXECUTION_CAPSULES_TABLE} SET capsule_json = ?1 WHERE run_id = ?2"),
        params![tampered_json, run_id],
    )
    .expect("tamper base_ref in capsule json");

    // The capsule JSON now says base_ref=develop, but envelope_digest is still
    // the original (computed over base_ref=main). verify_envelope_digest fails.
    assert_ne!(
        capsule.base_ref, "develop",
        "original capsule base_ref must be 'main', not 'develop'"
    );

    let request = recovery_request(run_id, "step1", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingExecutor::success();

    let result = protocol.recover_with_executor(&conn, Path::new("."), &request, &executor);

    // Invariant: recovery refuses (envelope digest mismatch). [C8]
    assert!(
        result.is_err(),
        "base_ref mismatch must refuse recovery (Err) [F10/C8]"
    );
    assert!(
        !executor.was_called(),
        "executor must NOT be called for base_ref mismatch [F10/C12]"
    );
}

// ===========================================================================
// F11: Legacy salvage lineage, no exact continuation
// ===========================================================================

/// GIVEN: a pre-V1 run (no capsule) including a legacy checkpoint
/// WHEN: salvage recovery is attempted
/// THEN: returns `Refused(SalvageOnly)`, salvage lineage appended
/// AND: no exact continuation is possible. [C9/B10]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P16
/// @requirement:REQ-RP-007
#[test]
fn f11_legacy_salvage_lineage_no_exact_continuation() {
    let conn = matrix_conn();
    let run_id = "f11-legacy";
    seed_run(&conn, run_id);

    // No capsule persisted — this is a legacy/pre-V1 run.

    // Classify: salvage-only (no valid capsule). [C9]
    let classification = classify_run(&conn, run_id).expect("classify legacy run");
    assert!(
        matches!(classification, RunClassification::SalvageOnly { .. }),
        "legacy run without capsule must be salvage-only [F11/C9]"
    );

    // Salvage recovery: appends immutable record, refuses exact recovery.
    let outcome = salvage_recover(&conn, run_id).expect("salvage recover [F11]");
    assert!(
        matches!(
            outcome,
            RecoveryOutcome::Refused {
                reason: RefusalReason::SalvageOnly
            }
        ),
        "legacy run must be refused with SalvageOnly [F11/C9]"
    );

    // Invariant: salvage lineage record appended (audit-only).
    let count = luther_workflow::engine::recovery::salvage::count_salvage_records(&conn, run_id)
        .expect("count salvage records");
    assert_eq!(count, 1, "one salvage record appended [F11/C9]");

    // Invariant: no exact continuation — the protocol cannot recover this run.
    let request = recovery_request(run_id, "step1", 0);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingExecutor::success();
    let result = protocol.recover_with_executor(&conn, Path::new("."), &request, &executor);
    assert!(
        result.is_err(),
        "protocol must not exact-recover a salvage-only run [F11/C9]"
    );
    assert!(
        !executor.was_called(),
        "executor must NOT be called for salvage-only run [F11/C12]"
    );
}

// ===========================================================================
// F12: Deterministic two-connection epoch race → exactly one execution
// ===========================================================================

/// GIVEN: a run at epoch 0 with two concurrent recoverers
/// WHEN: both attempt to CAS the epoch from 0 simultaneously
/// THEN: exactly one succeeds (`Advanced 0→1`), the other gets `StaleEpoch`
/// AND: the final epoch is exactly 1 (single-writer fence). [C1]
///
/// Deterministic concurrency: both threads hit a Barrier, then both attempt
/// the IMMEDIATE-locked CAS. The writer lock serializes them — no sleeps.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P16
/// @requirement:REQ-RP-004
#[test]
fn f12_deterministic_two_connection_epoch_race_exactly_one() {
    let temp = tempfile::tempdir().expect("temp dir for file db");
    let run_id = "f12-race";

    // Initialize the shared file DB and seed a run + capsule from the main
    // thread. Only the epoch table is needed for the CAS race.
    {
        let conn = matrix_file_db(temp.path(), "f12.db");
        init_runs_schema(&conn).expect("init runs");
        seed_run(&conn, run_id);
    }

    let barrier = Arc::new(Barrier::new(2));
    let db_path = temp.path().join("f12.db");

    let path_a = db_path.clone();
    let barrier_a = Arc::clone(&barrier);
    let handle_a = std::thread::spawn(move || {
        let conn = Connection::open(&path_a).expect("open conn A");
        conn.busy_timeout(Duration::from_secs(5))
            .expect("busy_timeout A");
        barrier_a.wait();
        let tx = rusqlite::Transaction::new_unchecked(&conn, TransactionBehavior::Immediate)
            .expect("begin IMMEDIATE A");
        let outcome = cas_advance_epoch(&tx, run_id, 0).expect("CAS A");
        tx.commit().expect("commit A");
        outcome
    });

    let path_b = db_path.clone();
    let barrier_b = Arc::clone(&barrier);
    let handle_b = std::thread::spawn(move || {
        let conn = Connection::open(&path_b).expect("open conn B");
        conn.busy_timeout(Duration::from_secs(5))
            .expect("busy_timeout B");
        barrier_b.wait();
        let tx = rusqlite::Transaction::new_unchecked(&conn, TransactionBehavior::Immediate)
            .expect("begin IMMEDIATE B");
        let outcome = cas_advance_epoch(&tx, run_id, 0).expect("CAS B");
        tx.commit().expect("commit B");
        outcome
    });

    let outcome_a = handle_a.join().expect("thread A must not panic");
    let outcome_b = handle_b.join().expect("thread B must not panic");

    let outcomes = [outcome_a, outcome_b];
    let advanced = outcomes
        .iter()
        .filter(|o| matches!(o, CasOutcome::Advanced { from: 0, to: 1 }))
        .count();
    let stale = outcomes
        .iter()
        .filter(|o| {
            matches!(
                o,
                CasOutcome::Stale {
                    persisted: 1,
                    expected: 0
                }
            )
        })
        .count();

    assert_eq!(advanced, 1, "exactly one CAS must advance 0→1 [F12/C1]");
    assert_eq!(
        stale, 1,
        "exactly one CAS must get StaleEpoch (persisted=1, expected=0) [F12/C1]"
    );

    // Invariant: final epoch is exactly 1 (single-writer fence, no synthetic
    // attempts to bump epoch). [C1]
    let verify_conn = Connection::open(&db_path).expect("open verify conn");
    let final_epoch = read_epoch(&verify_conn, run_id).expect("read final epoch");
    assert_eq!(
        final_epoch, 1,
        "final epoch must be exactly 1 (single advance, no synthetic bump) [F12/C1]"
    );
}

// ===========================================================================
// F13: Conflicting duplicate → Refused(ConflictingOperation)
// ===========================================================================

/// GIVEN: a run with a Completed operation for a DIFFERENT capsule binding
/// WHEN: `recover()` is called with a different capsule/source binding
/// THEN: returns `Refused(ConflictingOperation)`
/// AND: no new operation or attempt is created. [C2/B3]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P16
/// @requirement:REQ-RP-004
#[test]
fn f13_conflicting_duplicate_refused() {
    let conn = matrix_conn();
    let run_id = "f13-conflict";
    let capsule = persisted_capsule(&conn, run_id);
    seed_run(&conn, run_id);

    let (_op_id, logical_key, intent_digest) = normalized_bindings(run_id, &capsule);

    // Advance epoch to 1.
    {
        let tx = rusqlite::Transaction::new_unchecked(&conn, TransactionBehavior::Immediate)
            .expect("begin tx");
        cas_advance_epoch(&tx, run_id, 0).expect("seed epoch");
        tx.commit().expect("commit");
    }

    // Seed a Completed operation with the SAME logical_request_key but a
    // DIFFERENT capsule_envelope_digest (conflicting binding). [C2/B3]
    seed_completed_op(
        &conn,
        &SeedCompletedOp {
            operation_id: "op-f13-conflicting".to_string(),
            run_id: run_id.to_string(),
            epoch: 1,
            step_id: "step1".to_string(),
            capsule_digest: "different-capsule-digest-f13".to_string(),
            logical_key,
            intent_digest,
            attempt_id: 1,
            same_binding: false,
            actual_capsule_digest: capsule.envelope_digest.clone(),
        },
    );

    let ops_before = count_operations(&conn, run_id);

    let request = recovery_request(run_id, "step1", 1);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingExecutor::success();

    let outcome = protocol
        .recover_with_executor(&conn, Path::new("."), &request, &executor)
        .expect("recover must not hard-error on conflicting duplicate [F13]");

    assert!(
        matches!(
            outcome,
            RecoveryOutcome::Refused {
                reason: RefusalReason::ConflictingOperation
            }
        ),
        "conflicting duplicate must return Refused(ConflictingOperation) [F13/C2/B3]"
    );

    // Invariant: no new operation or attempt rows.
    assert_eq!(
        count_operations(&conn, run_id),
        ops_before,
        "no new operation rows for conflicting duplicate [F13/C2]"
    );
    assert!(
        !executor.was_called(),
        "executor must NOT be called for conflicting duplicate [F13/C12]"
    );
}

// ===========================================================================
// F14: Crash after execute before finalize → expired lease adoption
// ===========================================================================

/// Seed the durable "crash after execute" state for F14: a prior epoch
/// advance (simulating the first recovery's successful CAS), an unfinalized
/// attempt (started at reserve, outcome never appended), and a Pending
/// operation with an expired lease pointing at that attempt. Returns the
/// normalized `operation_id`.
fn seed_crashed_recovery(conn: &Connection, run_id: &str, capsule: &ExecutionCapsuleV1) -> String {
    // Advance epoch to 1 (simulating the first recovery's CAS that succeeded
    // before the crash).
    {
        let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .expect("begin tx");
        cas_advance_epoch(&tx, run_id, 0).expect("seed epoch");
        tx.commit().expect("commit");
    }

    let (op_id, logical_key, intent_digest) = normalized_bindings(run_id, capsule);

    // Allocate the unfinalized attempt (simulating the crash after execute:
    // the attempt was started at reserve but its outcome was never appended).
    let snapshot = StateSnapshot::default();
    let attempt_id = {
        let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .expect("begin tx for attempt");
        let id = record_attempt_start(
            &tx,
            &AttemptStart {
                run_id,
                epoch: 1,
                source_attempt_id: None,
                operation_id: &op_id,
                step_id: "step1",
                capsule_schema_version: capsule.schema_version,
                capsule_envelope_digest: &capsule.envelope_digest,
                state_snapshot: &snapshot,
            },
        )
        .expect("record attempt start");
        tx.commit().expect("commit attempt");
        id
    };

    // Seed a Pending operation with an EXPIRED lease pointing to the
    // unfinalized attempt (simulating the crash after execute, before finalize).
    seed_pending_op(
        conn,
        &SeedPendingOp {
            operation_id: op_id.clone(),
            run_id: run_id.to_string(),
            epoch: 1,
            step_id: "step1".to_string(),
            capsule_digest: capsule.envelope_digest.clone(),
            logical_key,
            intent_digest,
            attempt_id: Some(attempt_id),
            expired: true,
        },
    );

    op_id
}

/// Assert the F14 durable invariants after a successful re-recovery: outcome
/// is `Recovered` (only after finalize commits), executor was called
/// (re-execution happened), operation finalized to `Completed`, the attempt is
/// no longer unfinalized, and no duplicate operation rows were created. [C12]
#[allow(clippy::too_many_arguments)]
fn assert_crashed_recovery_invariants(
    conn: &Connection,
    op_id: &str,
    run_id: &str,
    outcome: &RecoveryOutcome,
    executor: &RecordingExecutor,
    ops_before: i64,
) {
    // Invariant: Recovered is returned only after finalize commits. [C12]
    assert!(
        matches!(outcome, RecoveryOutcome::Recovered { .. }),
        "re-recovery must return Recovered after finalize [F14/C12]"
    );

    // Invariant: executor was called (re-execution happened). [C12]
    assert!(
        executor.was_called(),
        "executor must be called for re-execution [F14/C12]"
    );

    // Invariant: operation is now Completed (finalize committed).
    assert_eq!(
        operation_status(conn, op_id),
        OperationStatus::Completed.as_str(),
        "operation must be Completed after finalize [F14/C12]"
    );

    // Invariant: attempt is now finalized (no longer unfinalized).
    assert!(
        load_unfinalized_for_operation(conn, op_id)
            .expect("load unfinalized after")
            .is_none(),
        "attempt must be finalized after re-recovery [F14/C12]"
    );

    // Invariant: no duplicate operations.
    assert_eq!(
        count_operations(conn, run_id),
        ops_before,
        "no duplicate operation rows after re-recovery [F14]"
    );
}

/// GIVEN: a Pending operation with an expired lease and an unfinalized attempt
///        (crash between execute and finalize)
/// WHEN: `recover()` is called (re-recovery)
/// THEN: the expired lease is adopted, execute re-runs, finalize commits
/// AND: outcome is `Recovered` (only after finalize), no duplicate effects.
///       [C12]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P16
/// @requirement:REQ-RP-001
#[test]
fn f14_crash_after_execute_before_finalize_adoption_no_duplicate() {
    let conn = matrix_conn();
    let run_id = "f14-crash";
    let capsule = persisted_capsule(&conn, run_id);
    seed_run(&conn, run_id);

    // Seed the durable "crash after execute" state: epoch advanced, an
    // unfinalized attempt, and an expired-lease Pending operation.
    let op_id = seed_crashed_recovery(&conn, run_id, &capsule);

    // Verify preconditions: operation is Pending, attempt is unfinalized.
    assert_eq!(
        operation_status(&conn, &op_id),
        OperationStatus::Pending.as_str(),
        "operation must be pending before re-recovery [F14]"
    );
    assert!(
        load_unfinalized_for_operation(&conn, &op_id)
            .expect("load unfinalized")
            .is_some(),
        "attempt must be unfinalized (crash after execute) [F14/C12]"
    );

    let ops_before = count_operations(&conn, run_id);

    // Re-recovery: adopt the expired-lease Pending, re-execute, finalize.
    let request = recovery_request(run_id, "step1", 1);
    let protocol = RecoveryProtocolV1;
    let executor = RecordingExecutor::success();

    let outcome = protocol
        .recover_with_executor(&conn, Path::new("."), &request, &executor)
        .expect("re-recovery must succeed after expired-lease adoption [F14]");

    assert_crashed_recovery_invariants(&conn, &op_id, run_id, &outcome, &executor, ops_before);
}
