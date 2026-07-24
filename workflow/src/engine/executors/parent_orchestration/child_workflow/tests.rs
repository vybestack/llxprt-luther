//! Unit tests for child workflow launch/resume: outcome mapping, recovery
//! protocol dispatch, and lease-ordering (prepare-before-mutate) invariants.

use super::*;
use crate::engine::recovery::{RecoveryOutcome, RefusalReason};
use crate::persistence::attempts::init_attempts_table;

/// Build a minimal `ChildWorkflowLaunchRequest` for testing.
fn test_request(run_id: &str) -> ChildWorkflowLaunchRequest {
    ChildWorkflowLaunchRequest {
        workflow_type_id: "wf".to_string(),
        config_id: "cfg".to_string(),
        run_id: run_id.to_string(),
        repo: "test/repo".to_string(),
        issue_number: 42,
        work_dir: None,
        artifact_dir: None,
        config_root: PathBuf::from("/config"),
    }
}

/// Create an in-memory SQLite connection with the attempts table.
fn attempts_conn() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    init_attempts_table(&conn).unwrap();
    conn
}

/// Insert a finalized attempt row with the given runner_result JSON.
fn insert_attempt(
    conn: &rusqlite::Connection,
    run_id: &str,
    runner_result: Option<serde_json::Value>,
) -> i64 {
    let now = chrono::Utc::now().to_rfc3339();
    let runner_json = runner_result.map(|v| v.to_string()).unwrap_or_default();
    let snapshot_json = serde_json::json!({
        "retry_count": 0,
        "loop_count": 0,
        "edge_loop_counts": {},
        "context": {},
        "status": "completed"
    })
    .to_string();
    conn.query_row(
        "INSERT INTO recovery_attempts
           (run_id, epoch, source_attempt_id, operation_id, step_id, step_status,
            capsule_schema_version, capsule_envelope_digest,
            state_snapshot_json, snapshot_digest, checkpoint_digest,
            runner_result_json, started_at, finalized_at)
         VALUES (?1, 0, NULL, 'op-1', 'step1', 'completed', 1, 'digest',
                 ?4, 'snap-digest', NULL, ?2, ?3, ?3)
         RETURNING attempt_id",
        rusqlite::params![run_id, runner_json, now, snapshot_json],
        |row| row.get(0),
    )
    .unwrap()
}

// -----------------------------------------------------------------------
// map_child_recovery_outcome: source-level outcome mapping
// -----------------------------------------------------------------------

#[test]
fn map_outcome_recovered_success_yields_completed_success() {
    let conn = attempts_conn();
    let request = test_request("child-rec-success");
    let attempt_id = insert_attempt(
        &conn,
        &request.run_id,
        Some(serde_json::json!({
            "outcome": "success",
            "step_id": "step1",
        })),
    );
    let config = resume_config();
    let outcome = RecoveryOutcome::Recovered {
        resumed_at_step: "step1".to_string(),
        attempt_id,
        operation_id: "op-1".to_string(),
    };
    let result =
        map_child_recovery_outcome(&conn, &request, &config, Path::new("/tmp/db"), outcome)
            .unwrap();
    assert_eq!(result, ChildWorkflowRunResult::CompletedSuccess);
}

#[test]
fn map_outcome_recovered_failure_yields_completed_failure() {
    let conn = attempts_conn();
    let request = test_request("child-rec-failure");
    let attempt_id = insert_attempt(
        &conn,
        &request.run_id,
        Some(serde_json::json!({
            "outcome": "failure",
            "step_id": "step1",
            "reason": "boom",
        })),
    );
    let config = resume_config();
    let outcome = RecoveryOutcome::Recovered {
        resumed_at_step: "step1".to_string(),
        attempt_id,
        operation_id: "op-1".to_string(),
    };
    let result =
        map_child_recovery_outcome(&conn, &request, &config, Path::new("/tmp/db"), outcome)
            .unwrap();
    assert_eq!(result, ChildWorkflowRunResult::CompletedFailure);
}

#[test]
fn map_outcome_recovered_abandoned_yields_completed_failure() {
    let conn = attempts_conn();
    let request = test_request("child-rec-abandoned");
    let attempt_id = insert_attempt(
        &conn,
        &request.run_id,
        Some(serde_json::json!({
            "outcome": "abandoned",
            "step_id": "step1",
            "reason": "gave up",
        })),
    );
    let config = resume_config();
    let outcome = RecoveryOutcome::Recovered {
        resumed_at_step: "step1".to_string(),
        attempt_id,
        operation_id: "op-1".to_string(),
    };
    let result =
        map_child_recovery_outcome(&conn, &request, &config, Path::new("/tmp/db"), outcome)
            .unwrap();
    assert_eq!(result, ChildWorkflowRunResult::CompletedFailure);
}

#[test]
fn map_outcome_refused_returns_error() {
    let conn = attempts_conn();
    let request = test_request("child-rec-refused");
    let config = resume_config();
    let outcome = RecoveryOutcome::Refused {
        reason: RefusalReason::NonRecoverable,
    };
    let result =
        map_child_recovery_outcome(&conn, &request, &config, Path::new("/tmp/db"), outcome);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("refused"), "error must mention refusal: {err}");
}

#[test]
fn map_outcome_stale_epoch_returns_error() {
    let conn = attempts_conn();
    let request = test_request("child-rec-stale");
    let config = resume_config();
    let outcome = RecoveryOutcome::StaleEpoch {
        persisted: 2,
        expected: 1,
    };
    let result =
        map_child_recovery_outcome(&conn, &request, &config, Path::new("/tmp/db"), outcome);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("stale epoch"),
        "error must mention stale epoch: {err}"
    );
}

#[test]
fn map_outcome_conflict_returns_error() {
    let conn = attempts_conn();
    let request = test_request("child-rec-conflict");
    let config = resume_config();
    let outcome = RecoveryOutcome::Conflict {
        detail: "duplicate operation".to_string(),
    };
    let result =
        map_child_recovery_outcome(&conn, &request, &config, Path::new("/tmp/db"), outcome);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("conflict"),
        "error must mention conflict: {err}"
    );
}

#[test]
fn map_outcome_already_applied_decodes_prior_outcome() {
    let conn = attempts_conn();
    let request = test_request("child-rec-already");
    let attempt_id = insert_attempt(
        &conn,
        &request.run_id,
        Some(serde_json::json!({
            "outcome": "success",
            "step_id": "step1",
        })),
    );
    let config = resume_config();
    let outcome = RecoveryOutcome::AlreadyApplied {
        prior_outcome: "success".to_string(),
        attempt_id,
        operation_id: "op-1".to_string(),
    };
    let result =
        map_child_recovery_outcome(&conn, &request, &config, Path::new("/tmp/db"), outcome)
            .unwrap();
    assert_eq!(result, ChildWorkflowRunResult::CompletedSuccess);
}

#[test]
fn map_outcome_missing_runner_result_returns_error() {
    let conn = attempts_conn();
    let request = test_request("child-rec-no-result");
    let attempt_id = insert_attempt(&conn, &request.run_id, None);
    let config = resume_config();
    let outcome = RecoveryOutcome::Recovered {
        resumed_at_step: "step1".to_string(),
        attempt_id,
        operation_id: "op-1".to_string(),
    };
    let result =
        map_child_recovery_outcome(&conn, &request, &config, Path::new("/tmp/db"), outcome);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("no runner result"),
        "error must mention missing runner result: {err}"
    );
}

#[test]
fn map_outcome_unknown_outcome_label_returns_error() {
    let conn = attempts_conn();
    let request = test_request("child-rec-unknown");
    let attempt_id = insert_attempt(
        &conn,
        &request.run_id,
        Some(serde_json::json!({
            "outcome": "glitched",
            "step_id": "step1",
            "reason": "",
        })),
    );
    let config = resume_config();
    let outcome = RecoveryOutcome::Recovered {
        resumed_at_step: "step1".to_string(),
        attempt_id,
        operation_id: "op-1".to_string(),
    };
    let result =
        map_child_recovery_outcome(&conn, &request, &config, Path::new("/tmp/db"), outcome);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("unknown"),
        "error must mention unknown outcome: {err}"
    );
}

// -----------------------------------------------------------------------
// resume_child_workflow: behavior tests for the RecoveryProtocolV1 path
// -----------------------------------------------------------------------

/// Set up a full in-memory DB for the resume path, matching the
/// production `init_database` schema.
fn full_resume_db() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("checkpoints.db");
    crate::persistence::init_database(&db_path).unwrap();
    (temp, db_path)
}

/// Set up a two-step pure-reenter workflow type for resume testing.
fn resume_workflow_type() -> crate::workflow::schema::WorkflowType {
    use crate::engine::recovery::StepRecoveryPolicy;
    use crate::workflow::schema::{GuardConfig, StepDef, TransitionDef, WorkflowType};
    WorkflowType {
        workflow_type_id: "child-resume-test".to_string(),
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

/// Build a resume config for the test workflow type.
fn resume_config() -> WorkflowConfig {
    use crate::workflow::schema::{
        DiffPathNormalization, GuardLimits, ParentOrchestrationConfig, RepoConfig, RuntimeConfig,
    };
    WorkflowConfig {
        config_id: "child-resume-test-config".to_string(),
        workflow_type_id: "child-resume-test".to_string(),
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
        variables: std::collections::HashMap::new(),
        discovery: None,
        parent_orchestration: ParentOrchestrationConfig::default(),
        merge_required: false,
        merge_strategy: None,
        command_manifest: None,
        target_profile: None,
    }
}

/// Seed the resume fixtures WITHOUT a workspace ownership marker:
/// the durable capsule, a `Running` run row at `step1`, and a
/// checkpoint at `step1`. Callers provision the workspace marker
/// (for this run, a foreign run, or not at all) to exercise the
/// read-only preparation checks.
fn seed_resume_fixture_without_marker(
    db_path: &Path,
    workspace: &Path,
    run_id: &str,
    workflow_type: &crate::workflow::schema::WorkflowType,
    config: &WorkflowConfig,
    config_root: &Path,
) {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    let provenance =
        crate::persistence::LaunchProvenance::from_resolved(workflow_type, config, config_root)
            .unwrap();
    let base_ref = config
        .repo
        .base_branch
        .clone()
        .unwrap_or_else(|| "main".to_string());
    let capsule = crate::engine::recovery::capsule::build_capsule_v1(
        run_id.to_string(),
        workflow_type,
        config,
        config_root,
        &provenance,
        base_ref,
    )
    .unwrap();
    crate::persistence::capsule_store::persist_capsule_v1(&conn, &capsule).unwrap();

    let mut metadata = crate::persistence::RunMetadata::new(
        run_id,
        &workflow_type.workflow_type_id,
        &config.config_id,
    );
    metadata.status = crate::persistence::RunStatus::Running;
    metadata.current_step = Some("step1".to_string());
    metadata.workspace_path = Some(workspace.to_string_lossy().to_string());
    metadata.repository = Some("test/repo".to_string());
    metadata.issue_number = Some(42);
    metadata.launch_provenance = Some(provenance);
    crate::persistence::persist_run_with_conn(&conn, &metadata).unwrap();

    let checkpoint = crate::persistence::checkpoint::Checkpoint {
        run_id: run_id.to_string(),
        step_id: "step1".to_string(),
        state_snapshot: crate::persistence::checkpoint::StateSnapshot::default(),
        timestamp: chrono::Utc::now(),
    };
    crate::persistence::checkpoint::save_checkpoint_with_conn(&conn, &checkpoint).unwrap();
}

/// Seed a resumable run (capsule, `Running` run row at `step1`, checkpoint
/// at `step1`) and provision the workspace marker for `run_id`.
fn seed_resumable_child(
    db_path: &Path,
    workspace: &Path,
    run_id: &str,
    workflow_type: &crate::workflow::schema::WorkflowType,
    config: &WorkflowConfig,
    config_root: &Path,
) {
    seed_resume_fixture_without_marker(
        db_path,
        workspace,
        run_id,
        workflow_type,
        config,
        config_root,
    );
    crate::engine::workspace_ownership::provision_workspace_ownership(workspace, run_id).unwrap();
}

#[test]
fn resume_child_workflow_fails_closed_without_workspace_marker() {
    // The read-only preparation must reject a resume when the workspace
    // ownership marker is missing (fail-closed). This preserves the
    // exact identity/provenance/workspace checks from
    // prepare_child_resume_readonly.
    let (_temp, db_path) = full_resume_db();
    let workspace = _temp.path().join("work-no-marker");
    std::fs::create_dir_all(&workspace).unwrap();
    let run_id = "child-resume-no-marker";
    let workflow_type = resume_workflow_type();
    let config = resume_config();
    let config_root = _temp.path();

    // Seed WITHOUT provisioning the workspace marker.
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let provenance =
        crate::persistence::LaunchProvenance::from_resolved(&workflow_type, &config, config_root)
            .unwrap();
    let capsule = crate::engine::recovery::capsule::build_capsule_v1(
        run_id.to_string(),
        &workflow_type,
        &config,
        config_root,
        &provenance,
        "main".to_string(),
    )
    .unwrap();
    crate::persistence::capsule_store::persist_capsule_v1(&conn, &capsule).unwrap();
    let mut metadata = crate::persistence::RunMetadata::new(
        run_id,
        &workflow_type.workflow_type_id,
        &config.config_id,
    );
    metadata.status = crate::persistence::RunStatus::Running;
    metadata.current_step = Some("step1".to_string());
    metadata.workspace_path = Some(workspace.to_string_lossy().to_string());
    metadata.repository = Some("test/repo".to_string());
    metadata.issue_number = Some(42);
    metadata.launch_provenance = Some(provenance);
    crate::persistence::persist_run_with_conn(&conn, &metadata).unwrap();
    let checkpoint = crate::persistence::checkpoint::Checkpoint {
        run_id: run_id.to_string(),
        step_id: "step1".to_string(),
        state_snapshot: crate::persistence::checkpoint::StateSnapshot::default(),
        timestamp: chrono::Utc::now(),
    };
    crate::persistence::checkpoint::save_checkpoint_with_conn(&conn, &checkpoint).unwrap();

    let request = ChildWorkflowLaunchRequest {
        workflow_type_id: workflow_type.workflow_type_id.clone(),
        config_id: config.config_id.clone(),
        run_id: run_id.to_string(),
        repo: "test/repo".to_string(),
        issue_number: 42,
        work_dir: Some(workspace.clone()),
        artifact_dir: None,
        config_root: config_root.to_path_buf(),
    };

    let result = resume_child_workflow(&request, &workflow_type, &config, config_root, &db_path);

    assert!(
        result.is_err(),
        "resume must fail closed when workspace marker is missing"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("missing") || err.contains("owner"),
        "error must indicate missing workspace ownership: {err}"
    );
}

#[test]
fn resume_child_workflow_dispatches_through_recovery_protocol() {
    // The production resume path must dispatch through
    // RecoveryProtocolV1::recover_with_executor, NOT through the legacy
    // commit_continuation + EngineRunner path. A properly seeded run
    // (capsule, checkpoint, workspace marker) must complete via the
    // recovery protocol and map to CompletedSuccess.
    let (temp, db_path) = full_resume_db();
    let workspace = temp.path().join("work");
    let run_id = "child-resume-v1";
    let workflow_type = resume_workflow_type();
    let config = resume_config();
    let config_root = temp.path();

    seed_resumable_child(
        &db_path,
        &workspace,
        run_id,
        &workflow_type,
        &config,
        config_root,
    );

    let request = ChildWorkflowLaunchRequest {
        workflow_type_id: workflow_type.workflow_type_id.clone(),
        config_id: config.config_id.clone(),
        run_id: run_id.to_string(),
        repo: "test/repo".to_string(),
        issue_number: 42,
        work_dir: Some(workspace.clone()),
        artifact_dir: None,
        config_root: config_root.to_path_buf(),
    };

    let result = resume_child_workflow(&request, &workflow_type, &config, config_root, &db_path);

    assert!(
        result.is_ok(),
        "resume through RecoveryProtocolV1 must succeed for a properly seeded run, got: {:?}",
        result
    );
    assert_eq!(
        result.unwrap(),
        ChildWorkflowRunResult::CompletedSuccess,
        "a two-step noop workflow resuming at step1 must complete successfully"
    );
}

#[test]
fn resume_child_workflow_preserves_lease_ordering_prepare_before_mutate() {
    // Lease ordering: the read-only preparation (identity, provenance,
    // workspace marker verification) must complete BEFORE any durable
    // mutation. A run with a foreign workspace marker must be rejected
    // BEFORE ownership promotion or epoch CAS occurs.
    let (temp, db_path) = full_resume_db();
    let workspace = temp.path().join("work-foreign");
    let run_id = "child-resume-foreign";
    let foreign_run = "foreign-owner-run";
    let workflow_type = resume_workflow_type();
    let config = resume_config();
    let config_root = temp.path();

    // Seed the durable fixtures, then provision the marker for a FOREIGN
    // run (not the resuming run).
    seed_resume_fixture_without_marker(
        &db_path,
        &workspace,
        run_id,
        &workflow_type,
        &config,
        config_root,
    );
    crate::engine::workspace_ownership::provision_workspace_ownership(&workspace, foreign_run)
        .unwrap();

    let request = ChildWorkflowLaunchRequest {
        workflow_type_id: workflow_type.workflow_type_id.clone(),
        config_id: config.config_id.clone(),
        run_id: run_id.to_string(),
        repo: "test/repo".to_string(),
        issue_number: 42,
        work_dir: Some(workspace.clone()),
        artifact_dir: None,
        config_root: config_root.to_path_buf(),
    };

    let result = resume_child_workflow(&request, &workflow_type, &config, config_root, &db_path);

    assert!(
        result.is_err(),
        "resume must fail closed when workspace marker is owned by a foreign run"
    );

    // The epoch must NOT have advanced: no mutation occurred before the
    // preparation failure (read_epoch returns 0 for a new run).
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let epoch = crate::persistence::recovery_epoch::read_epoch(&conn, run_id).unwrap_or(0);
    assert_eq!(
        epoch, 0,
        "epoch must not be advanced when preparation fails (lease ordering preserved)"
    );
}

#[test]
fn resume_child_workflow_missing_work_dir_fails_closed() {
    // The migrated resume path requires a work_dir for the
    // workspace_path parameter. A request without work_dir must fail
    // closed before any protocol dispatch.
    let (temp, db_path) = full_resume_db();
    let workspace = temp.path().join("work-missing");
    let run_id = "child-resume-no-workdir";
    let workflow_type = resume_workflow_type();
    let config = resume_config();
    let config_root = temp.path();

    // Seed a resumable run but the request will have no work_dir.
    seed_resumable_child(
        &db_path,
        &workspace,
        run_id,
        &workflow_type,
        &config,
        config_root,
    );

    let request = ChildWorkflowLaunchRequest {
        workflow_type_id: workflow_type.workflow_type_id.clone(),
        config_id: config.config_id.clone(),
        run_id: run_id.to_string(),
        repo: "test/repo".to_string(),
        issue_number: 42,
        work_dir: None,
        artifact_dir: None,
        config_root: config_root.to_path_buf(),
    };

    let result = resume_child_workflow(&request, &workflow_type, &config, config_root, &db_path);

    assert!(
        result.is_err(),
        "resume must fail closed when work_dir is missing"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("work_dir") || err.contains("workspace"),
        "error must mention missing work_dir/workspace: {err}"
    );
}
