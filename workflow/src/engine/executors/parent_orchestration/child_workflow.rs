use super::*;

use crate::workflow::config_loader::{resolve_workflow_config, resolve_workflow_type};

/// Verify (without writing) that a child workspace's durable ownership marker
/// already exists and belongs to the resume's `run_id`.
///
/// A resume re-enters a workspace that a prior launch provisioned and therefore
/// must never (re)write the marker. Instead it verifies the marker is present
/// and owned by the resuming run id, failing closed (returning an error) when
/// the marker is missing, empty, malformed, or owned by a different run. This
/// prevents a resume from silently claiming a workspace that was never
/// provisioned for it or that a concurrent run has since claimed.
pub(super) fn verify_existing_workspace_owner_marker(
    workspace: &Path,
    run_id: &str,
) -> Result<(), String> {
    crate::engine::workspace_ownership::verify_workspace_ownership(workspace, run_id)
        .map_or(Ok(()), Err)
}

pub fn child_launch_request(state: &OrchestrationState, child: u64) -> ChildWorkflowLaunchRequest {
    let stamp = Utc::now().timestamp_millis();
    child_request_with_run_id(
        state,
        child,
        format!("parent{}-child{}-{stamp}", state.parent_issue_number, child),
    )
}

pub fn child_resume_request(
    state: &OrchestrationState,
    child: u64,
    run_id: String,
) -> ChildWorkflowLaunchRequest {
    child_request_with_run_id(state, child, run_id)
}

pub fn child_request_with_run_id(
    state: &OrchestrationState,
    child: u64,
    run_id: String,
) -> ChildWorkflowLaunchRequest {
    let artifact_dir = state
        .artifact_dir
        .as_ref()
        .map(|base| child_artifact_dir(base, child, &run_id));
    // Derive an isolated workspace directory per child issue and run rather
    // than reusing the parent's `work_dir`. Each child workflow gets its own
    // persisted worktree so concurrent children and relaunches do not stomp
    // on a shared parent workspace, and the durable workspace-owner marker can
    // be bound to the child run id without cross-run conflicts.
    let work_dir = state
        .work_dir
        .as_ref()
        .map(|base| child_work_dir(base, child, &run_id));
    ChildWorkflowLaunchRequest {
        workflow_type_id: state.child_workflow_type_id.clone(),
        config_id: state.child_config_id.clone(),
        run_id,
        repo: state.repo.clone(),
        issue_number: child,
        work_dir,
        artifact_dir,
        config_root: state.config_root.clone(),
    }
}

pub fn child_artifact_dir(base: &Path, child: u64, run_id: &str) -> PathBuf {
    base.join(format!("issue-{child}")).join(run_id)
}

/// Derive an isolated persisted workspace directory for a child run.
///
/// Mirrors the per-child/per-run layout already used for artifact directories,
/// so each child issue and each relaunch of that child gets its own workspace
/// tree under the parent `work_dir` base rather than sharing it.
pub fn child_work_dir(base: &Path, child: u64, run_id: &str) -> PathBuf {
    base.join("children")
        .join(format!("issue-{child}"))
        .join(run_id)
}

pub fn resume_child_process(
    request: &ChildWorkflowLaunchRequest,
) -> Result<ChildWorkflowRunResult, String> {
    run_child_workflow(request, ChildRunMode::Resume)
}

pub fn launch_child_process(
    request: &ChildWorkflowLaunchRequest,
) -> Result<ChildWorkflowRunResult, String> {
    run_child_workflow(request, ChildRunMode::Launch)
}

pub enum ChildRunMode {
    Launch,
    Resume,
}

pub fn run_child_workflow(
    request: &ChildWorkflowLaunchRequest,
    mode: ChildRunMode,
) -> Result<ChildWorkflowRunResult, String> {
    validate_run_id_path_component(&request.run_id)?;
    let config_root = &request.config_root;
    let config_id = validated_child_id(&request.config_id, "config id")?;
    let workflow_type_id = validated_child_id(&request.workflow_type_id, "type id")?;
    let mut config = resolve_workflow_config(config_id, config_root)
        .map_err(|err| format!("resolve child config '{config_id}': {err}"))?;
    let workflow_type = resolve_workflow_type(workflow_type_id, config_root)
        .map_err(|err| format!("resolve child workflow type: {err}"))?;
    apply_child_overrides(&mut config, request)?;
    let db_path = crate::runtime_paths::get_data_dir().join("checkpoints.db");
    match mode {
        ChildRunMode::Launch => {
            launch_child_workflow(request, &workflow_type, &config, config_root, &db_path)
        }
        ChildRunMode::Resume => {
            resume_child_workflow(request, &workflow_type, &config, config_root, &db_path)
        }
    }
}

/// Launch a fresh child workflow: provision workspace ownership, insert the
/// starting run row with provenance, construct the runner, and run.
fn launch_child_workflow(
    request: &ChildWorkflowLaunchRequest,
    workflow_type: &crate::workflow::schema::WorkflowType,
    config: &WorkflowConfig,
    config_root: &Path,
    db_path: &Path,
) -> Result<ChildWorkflowRunResult, String> {
    // Launch provisions the durable workspace ownership marker; it is the
    // only path that may write it.
    if let Some(work_dir) = request.work_dir.as_deref() {
        crate::engine::workspace_ownership::provision_workspace_ownership(
            work_dir,
            &request.run_id,
        )
        .map_err(|err| format!("provision child workspace ownership: {err}"))?;
    }
    let launch_provenance =
        crate::persistence::LaunchProvenance::from_resolved(workflow_type, config, config_root)
            .map_err(|err| format!("record child launch provenance: {err}"))?;
    // Build the immutable ExecutionCapsuleV1 from the exact resolved
    // post-override workflow/config/config-root/provenance/base-ref before
    // constructing the runner. The capsule must exist before any workflow
    // execution/effects.
    // @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08B
    let base_ref = config
        .repo
        .base_branch
        .clone()
        .unwrap_or_else(|| "main".to_string());
    let capsule = crate::engine::recovery::capsule::build_capsule_v1(
        request.run_id.clone(),
        workflow_type,
        config,
        config_root,
        &launch_provenance,
        base_ref,
    )
    .map_err(|err| format!("build child execution capsule: {err}"))?;
    let mut run_context = child_run_context(config, request)?;
    run_context.launch_provenance = Some(launch_provenance);
    let instance = WorkflowInstance::create_with_run_id(
        workflow_type.clone(),
        config.clone(),
        &request.run_id,
    );
    // A fresh launch must fail closed if the initial Starting RunMetadata with
    // Some provenance and the immutable capsule cannot be atomically inserted
    // (run_id collision, capsule collision, or DB error). Neither row is
    // durable on failure.
    let mut runner = EngineRunner::with_db_path_for_launch(
        instance,
        crate::engine::executor::ExecutorRegistry::with_defaults(),
        db_path,
        run_context,
        capsule,
    )
    .map_err(|err| err.to_string())?;
    let outcome = runner.run().map_err(|err| err.to_string())?;
    child_result_from_run_outcome(outcome, request, config, db_path)
}

/// Resume an existing child workflow exclusively through
/// [`RecoveryProtocolV1`]: perform the complete read-only validation
/// (identity, provenance, workspace marker, current-step, checkpoint,
/// authorization) BEFORE any durable mutation, then promote workspace
/// ownership, read the durable recovery epoch, construct the authoritative
/// [`RunContext`] with descriptor-bound workspace authorization, build the
/// production [`RunnerRecoveryExecutor`], and dispatch through
/// [`RecoveryProtocolV1::recover_with_executor`]. The actual durable
/// [`RunOutcome`] is mapped back to [`ChildWorkflowRunResult`]; no synthetic
/// success is fabricated.
///
/// Lease ordering is preserved: the read-only preparation completes before any
/// durable mutation (ownership promotion, epoch CAS, checkpoint commit, runner
/// construction). No legacy `commit_continuation` + `EngineRunner` fallback
/// path remains.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14
fn resume_child_workflow(
    request: &ChildWorkflowLaunchRequest,
    workflow_type: &crate::workflow::schema::WorkflowType,
    config: &WorkflowConfig,
    config_root: &Path,
    db_path: &Path,
) -> Result<ChildWorkflowRunResult, String> {
    // Issue 158 gap 1: perform the COMPLETE read-only validation BEFORE any
    // durable mutation. This returns the ephemeral WorkspaceAuthorization and
    // the selected checkpoint identity. On any failure (foreign owner, missing
    // evidence, malformed marker, provenance mismatch, missing current_step)
    // the child run aborts without touching markers, lease, or checkpoint.
    let prepared = child_resume_preparation::prepare_child_resume_readonly(
        db_path,
        request,
        workflow_type,
        config,
        config_root,
    )?;
    // Promote verified existing evidence only AFTER the read-only
    // authorization succeeded. Resume never creates a first claim.
    let workspace_path = request.work_dir.as_deref().ok_or_else(|| {
        format!(
            "missing work_dir for child workflow resume {}",
            request.run_id
        )
    })?;
    crate::engine::workspace_ownership::ensure_durable_workspace_ownership(
        workspace_path,
        &request.run_id,
    )
    .map_err(|err| format!("verify child workspace ownership: {err}"))?;
    // Read the durable recovery epoch BEFORE constructing the RecoveryRequest.
    // This is the caller's view of the current epoch; the protocol's reserve
    // phase CAS-advances it.
    let conn = open_parent_orchestration_connection(db_path)?;
    let expected_epoch = crate::persistence::recovery_epoch::read_epoch(&conn, &request.run_id)
        .map_err(|err| format!("read child recovery epoch: {err}"))?;
    // Construct the authoritative RunContext with the descriptor-bound
    // workspace authorization reconstructed during read-only preparation.
    let mut run_context = child_run_context(config, request)?;
    if let Some(authorization) = prepared.authorization() {
        run_context.workspace_authorization = Some(authorization);
    }
    // Build the production capsule-backed recovery executor. The executor
    // reconstructs the WorkflowInstance from the immutable capsule and runs
    // the reserved step on its own connection, outside the protocol's
    // transaction.
    let executor =
        crate::engine::recovery::RecoveryWiring.runner_executor(db_path.to_path_buf(), run_context);
    let recovery_request = crate::engine::recovery::RecoveryRequest {
        run_id: request.run_id.clone(),
        step_id: prepared.resume_step_id().to_string(),
        expected_epoch,
        operator_verb: crate::engine::recovery::OperatorVerb::Resume,
    };
    // Dispatch through RecoveryProtocolV1. The protocol owns prepare → reserve
    // (epoch CAS, attempt allocation, checkpoint revalidation) → execute
    // (capsule-backed runner) → finalize (attempt outcome append). No legacy
    // fallback.
    let outcome = crate::engine::recovery::RecoveryProtocolV1
        .recover_with_executor(&conn, workspace_path, &recovery_request, &executor)
        .map_err(|err| format!("child recovery protocol failed: {err}"))?;
    map_child_recovery_outcome(&conn, request, config, db_path, outcome)
}

/// Map the actual durable [`crate::engine::recovery::RecoveryOutcome`] back to
/// [`ChildWorkflowRunResult`]. No synthetic success is fabricated: the durable
/// attempt row is loaded to recover the exact `RunOutcome` recorded by the
/// finalize phase.
///
/// - [`RecoveryOutcome::Recovered`] and [`RecoveryOutcome::AlreadyApplied`]
///   load the durable attempt and decode the persisted `runner_result_json`,
///   mapping `"success"` → [`CompletedSuccess`], `"waiting_external"` →
///   [`WaitingExternal`] (after persisting the wait state), and anything else
///   → [`CompletedFailure`].
/// - [`RecoveryOutcome::Refused`], [`RecoveryOutcome::StaleEpoch`], and
///   [`RecoveryOutcome::Conflict`] are hard failures.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14
fn map_child_recovery_outcome(
    conn: &rusqlite::Connection,
    request: &ChildWorkflowLaunchRequest,
    config: &WorkflowConfig,
    db_path: &Path,
    outcome: crate::engine::recovery::RecoveryOutcome,
) -> Result<ChildWorkflowRunResult, String> {
    use crate::engine::recovery::RecoveryOutcome;
    let attempt_id = match &outcome {
        RecoveryOutcome::Recovered { attempt_id, .. }
        | RecoveryOutcome::AlreadyApplied { attempt_id, .. } => *attempt_id,
        RecoveryOutcome::Refused { reason } => {
            return Err(format!("child recovery refused: {reason:?}"));
        }
        RecoveryOutcome::StaleEpoch {
            persisted,
            expected,
        } => {
            return Err(format!(
                "child recovery stale epoch: persisted {persisted}, expected {expected}"
            ));
        }
        RecoveryOutcome::Conflict { detail } => {
            return Err(format!("child recovery conflict: {detail}"));
        }
    };
    let attempt = crate::persistence::attempts::load_attempt(conn, attempt_id)
        .map_err(|err| format!("load child recovery attempt: {err}"))?;
    let runner_result: DurableChildRunnerResult = serde_json::from_value(
        attempt
            .runner_result_json
            .ok_or_else(|| "child recovery attempt has no runner result".to_string())?,
    )
    .map_err(|err| format!("decode child recovery runner result: {err}"))?;
    match runner_result.outcome.as_str() {
        "success" => Ok(ChildWorkflowRunResult::CompletedSuccess),
        "waiting_external" => {
            super::child_wait::persist_child_external_wait_state(
                request,
                config,
                db_path,
                &runner_result.step_id,
                &runner_result.reason,
            )
            .map_err(|err| err.to_string())?;
            Ok(ChildWorkflowRunResult::WaitingExternal)
        }
        "interrupted" => {
            super::child_wait::persist_child_interrupted_state(
                request,
                config,
                db_path,
                &runner_result.step_id,
            )
            .map_err(|err| err.to_string())?;
            Ok(ChildWorkflowRunResult::WaitingExternal)
        }
        "failure" | "abandoned" => {
            tracing::warn!(
                run_id = %request.run_id,
                step_id = %runner_result.step_id,
                reason = %runner_result.reason,
                "child workflow recovery ended in failure"
            );
            Ok(ChildWorkflowRunResult::CompletedFailure)
        }
        other => Err(format!("unknown child recovery runner outcome: {other}")),
    }
}

/// Deserialized durable runner result recorded by the recovery executor's
/// finalize phase. Mirrors the daemon path's `DurableDaemonRunnerResult`.
#[derive(Debug, serde::Deserialize)]
struct DurableChildRunnerResult {
    outcome: String,
    #[serde(default)]
    step_id: String,
    #[serde(default)]
    reason: String,
}

/// Verify the persisted launch provenance for a child resume against the
/// recomputed digests, refusing before any mutation on mismatch.
///
/// @plan:PLAN-20260722-ISSUE158-LAUNCH-PROVENANCE
pub(super) fn verify_child_resume_provenance(
    db_path: &Path,
    request: &ChildWorkflowLaunchRequest,
    workflow_type: &crate::workflow::schema::WorkflowType,
    config: &WorkflowConfig,
    config_root: &Path,
) -> Result<(), String> {
    let conn = rusqlite::Connection::open(db_path).map_err(|e| e.to_string())?;
    let metadata = crate::persistence::get_run_with_conn(&conn, &request.run_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("missing run metadata for child {}", request.run_id))?;
    let verification = crate::persistence::verify_provenance(
        &metadata.launch_provenance,
        workflow_type,
        config,
        config_root,
        crate::persistence::LegacyAllowed::Allowed,
    );
    match verification {
        crate::persistence::ProvenanceVerification::Match => Ok(()),
        crate::persistence::ProvenanceVerification::Legacy(warning) => {
            tracing::warn!("child run '{}': {warning}", request.run_id);
            Ok(())
        }
        crate::persistence::ProvenanceVerification::Mismatch(reason) => Err(format!(
            "child launch provenance mismatch for run '{}': {reason}",
            request.run_id
        )),
    }
}

#[cfg(test)]
mod tests {
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
            DiffPathNormalization, GuardLimits, ParentOrchestrationConfig, RepoConfig,
            RuntimeConfig,
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
        crate::engine::workspace_ownership::provision_workspace_ownership(workspace, run_id)
            .unwrap();
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
        let provenance = crate::persistence::LaunchProvenance::from_resolved(
            &workflow_type,
            &config,
            config_root,
        )
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

        let result =
            resume_child_workflow(&request, &workflow_type, &config, config_root, &db_path);

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

        let result =
            resume_child_workflow(&request, &workflow_type, &config, config_root, &db_path);

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

        let result =
            resume_child_workflow(&request, &workflow_type, &config, config_root, &db_path);

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

        let result =
            resume_child_workflow(&request, &workflow_type, &config, config_root, &db_path);

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
}
