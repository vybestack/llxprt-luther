use super::*;
use crate::persistence::checkpoint::Checkpoint;

/// Populate the `WaitStateRecord` fields shared by every child wait-state
/// persistence path (identity, config, poll cadence, resume checkpoint). The
/// wait-kind-specific fields (`wait_kind`, `wait_condition`,
/// `last_observed_state`, and any PR identity) are set by the caller.
fn populate_common_child_wait_fields(
    record: &mut WaitStateRecord,
    request: &ChildWorkflowLaunchRequest,
    config: &WorkflowConfig,
    checkpoint: &Checkpoint,
    lease_id: Option<String>,
) {
    record.lease_id = lease_id;
    record.workflow_type = config.workflow_type_id.clone();
    record.config_id = config.config_id.clone();
    record.repository = request.repo.clone();
    record.issue_number = request.issue_number;
    record.poll_interval_seconds = child_wait_poll_interval(config);
    record.max_wait_seconds = child_wait_max_wait_seconds(config, record.wait_kind);
    record.next_poll_at = crate::polling::next_poll_time(record.poll_interval_seconds);
    record.resume_step = checkpoint.step_id.clone();
    record.checkpoint_id = crate::engine::continuation::checkpoint_identity(checkpoint);
}

/// Upsert the wait-state record and write its artifact, mapping errors into the
/// shared parent-orchestration error shape. Shared by both child wait-state
/// persistence paths.
fn commit_child_wait_record(
    conn: &rusqlite::Connection,
    run_id: &str,
    record: &WaitStateRecord,
) -> Result<(), EngineError> {
    upsert_wait_state(conn, record)
        .map_err(|err| parent_error(format!("persist child wait-state: {err}")))?;
    write_wait_state_artifact(run_id, record)
        .map(|_| ())
        .map_err(|err| parent_error(format!("write child wait-state artifact: {err}")))
}

pub(super) fn persist_child_interrupted_state(
    request: &ChildWorkflowLaunchRequest,
    config: &WorkflowConfig,
    db_path: &Path,
    step_id: &str,
) -> Result<(), EngineError> {
    let conn = open_parent_orchestration_connection(db_path).map_err(parent_error)?;
    let checkpoint = load_checkpoint_with_conn(&conn, &request.run_id)
        .map_err(|err| parent_error(format!("load child interrupted checkpoint: {err}")))?
        .ok_or_else(|| {
            parent_error(format!(
                "missing child interrupted checkpoint for {}",
                request.run_id
            ))
        })?;
    let previous = crate::persistence::get_wait_state(&conn, &request.run_id).map_err(sql_error)?;
    let mut record =
        previous.unwrap_or_else(|| WaitStateRecord::new(&request.run_id, &config.config_id));
    let lease_id = child_lease_id(&conn, request)?;
    record.wait_kind = child_wait_kind_for_step(step_id);
    populate_common_child_wait_fields(&mut record, request, config, &checkpoint, lease_id);
    record.wait_condition = json!({
        "step_id": step_id,
        "reason": "child_workflow_interrupted",
        "repository": request.repo,
        "issue_number": request.issue_number,
    });
    record.last_observed_state = json!({
        "classification": "interrupted",
        "step_id": step_id,
        "reason": "child_workflow_interrupted"
    });
    commit_child_wait_record(&conn, &request.run_id, &record)
}

pub(super) fn persist_child_external_wait_state(
    request: &ChildWorkflowLaunchRequest,
    config: &WorkflowConfig,
    db_path: &Path,
    step_id: &str,
    reason: &str,
) -> Result<(), EngineError> {
    let conn = open_parent_orchestration_connection(db_path).map_err(parent_error)?;
    let checkpoint = load_checkpoint_with_conn(&conn, &request.run_id)
        .map_err(|err| parent_error(format!("load child waiting checkpoint: {err}")))?
        .ok_or_else(|| {
            parent_error(format!(
                "missing child waiting checkpoint for {}",
                request.run_id
            ))
        })?;
    let metadata = get_run_with_conn(&conn, &request.run_id).map_err(sql_error)?;
    let wait_kind = child_wait_kind_for_step(step_id);
    let identity = child_wait_poll_identity(metadata.as_ref(), wait_kind).map_err(parent_error)?;
    let previous = crate::persistence::get_wait_state(&conn, &request.run_id).map_err(sql_error)?;
    let mut record =
        previous.unwrap_or_else(|| WaitStateRecord::new(&request.run_id, &config.config_id));
    let lease_id = child_lease_id(&conn, request)?;
    record.wait_kind = wait_kind;
    populate_common_child_wait_fields(&mut record, request, config, &checkpoint, lease_id);
    record.pr_number = identity.pr_number;
    record.head_sha = identity.head_sha;
    record.wait_condition = json!({
        "step_id": step_id,
        "reason": reason,
        "repository": request.repo,
        "issue_number": request.issue_number,
    });
    record.last_observed_state = json!({
        "classification": "suspended",
        "step_id": step_id,
        "reason": reason
    });
    commit_child_wait_record(&conn, &request.run_id, &record)
}

pub(super) fn child_wait_kind_for_step(step_id: &str) -> WaitKind {
    match step_id {
        "watch_pr_checks" => WaitKind::PrChecks,
        "collect_coderabbit_feedback" => WaitKind::CoderabbitReview,
        "merge_pr" | "wait_for_merge" => WaitKind::PrMerge,
        "launch_or_resume_child_workflow" | "dependency_child_workflow" => {
            WaitKind::DependencyChildWorkflow
        }
        "wait_for_child_merge" | "dependency_child_merge" => WaitKind::DependencyChildMerge,
        "rate_limit_backoff" | "github_rate_limit_backoff" => WaitKind::RateLimitBackoff,
        _ => WaitKind::HumanReview,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildWaitIdentity {
    pub pr_number: Option<u64>,
    pub head_sha: Option<String>,
}

pub(super) fn child_wait_poll_identity(
    metadata: Option<&RunMetadata>,
    wait_kind: WaitKind,
) -> Result<ChildWaitIdentity, String> {
    let identity = ChildWaitIdentity {
        pr_number: metadata
            .and_then(|md| md.pr_number)
            .and_then(|number| u64::try_from(number).ok()),
        head_sha: metadata.and_then(|md| md.head_sha.clone()),
    };
    match wait_kind {
        WaitKind::PrChecks if identity.pr_number.is_none() || identity.head_sha.is_none() => {
            Err("missing child PR number or head SHA for PR checks wait state".to_string())
        }
        WaitKind::DependencyChildWorkflow if identity.head_sha.is_none() => {
            Err("missing head SHA for dependency child workflow wait state".to_string())
        }
        WaitKind::CoderabbitReview
        | WaitKind::HumanReview
        | WaitKind::PrMerge
        | WaitKind::DependencyChildMerge
            if identity.pr_number.is_none() =>
        {
            Err(format!(
                "missing child PR number for {wait_kind} wait state"
            ))
        }
        _ => Ok(identity),
    }
}

pub(super) fn child_wait_poll_interval(config: &WorkflowConfig) -> u64 {
    config
        .discovery
        .as_ref()
        .and_then(|discovery| discovery.poll_interval_secs)
        .unwrap_or(300)
}

pub(super) fn child_wait_max_wait_seconds(
    config: &WorkflowConfig,
    wait_kind: WaitKind,
) -> Option<u64> {
    match wait_kind {
        WaitKind::DependencyChildWorkflow | WaitKind::DependencyChildMerge => Some(
            config
                .parent_orchestration
                .max_child_merge_wait_seconds
                .unwrap_or(crate::workflow::schema::DEFAULT_MAX_CHILD_MERGE_WAIT_SECONDS),
        ),
        _ => None,
    }
}

pub(super) fn child_lease_id(
    conn: &rusqlite::Connection,
    request: &ChildWorkflowLaunchRequest,
) -> Result<Option<String>, EngineError> {
    get_lease_for_issue(conn, &request.repo, request.issue_number)
        .map(|lease| lease.map(|lease| lease.lease_id))
        .map_err(sql_error)
}

pub(super) fn sql_error(err: rusqlite::Error) -> EngineError {
    parent_error(format!("database error: {err}"))
}
