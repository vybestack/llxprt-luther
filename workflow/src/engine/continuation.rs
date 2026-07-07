//! Operator continuation: resume/retry/rewind failed or waiting workflow runs.
//!
//! This module owns the reusable, side-effect-scoped logic behind the
//! `luther-workflow runs {checkpoints,resume,retry,rewind}` commands. It
//! validates that a continuation is safe (refusing rather than corrupting
//! state), selects the checkpoint to resume from, re-stamps it as the resume
//! point, reopens the run record without erasing history, and writes auditable
//! artifacts. The actual engine re-execution is performed by the caller
//! (main.rs) after `commit_continuation`, by reconstructing an `EngineRunner`
//! against the same `run_id` and `checkpoints.db` so the standard
//! newest-checkpoint resume loader naturally picks up the re-stamped point.
//!
//! @plan:PLAN-20260623-LUTHER-CONTINUATION

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::persistence::{
    append_typed_event_with_conn, get_checkpoint_for_step, get_run_with_conn,
    is_resumable_checkpoint_status, list_checkpoints, load_checkpoint_before_step,
    persist_run_with_conn, set_resume_point, Checkpoint, EventType, PersistenceError, RunMetadata,
    RunStatus,
};
use crate::workflow::target_profile::TargetProfileOverrides;

/// Steps that are safe to re-run because they are external-wait or otherwise
/// idempotent. Continuation onto any other step requires `--force`.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub const SAFE_RERUN_STEPS: &[&str] = &[
    "watch_pr_checks",
    "collect_ci_failures",
    "collect_coderabbit_feedback",
    "capture_pr_identity",
    "post_pr_iteration_guard",
];

/// Retryable external-wait steps selected by `retry --from-failed-step`.
const RETRYABLE_WAIT_STEPS: &[&str] = &["watch_pr_checks"];

/// Default terminal sink step for PR-followup workflows.
const TERMINAL_STEP: &str = "post_pr_failure_terminal";

/// Whether the given step id is in the safe-to-rerun whitelist.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn is_safe_rerun_step(step_id: &str) -> bool {
    SAFE_RERUN_STEPS.contains(&step_id)
}

/// Reconstruct the effective runtime overrides from a persisted run row so a
/// continuation resumes against the original run's target/workspace/artifacts
/// rather than the static config defaults.
///
/// Only fields the run actually recorded produce `Some(..)`, mirroring how the
/// initial run inserts only the overrides that were provided; untouched fields
/// keep the static config defaults.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
#[must_use]
pub fn continuation_overrides(md: &RunMetadata) -> TargetProfileOverrides {
    TargetProfileOverrides {
        repo: md.repository.clone(),
        // GitHub issues and PRs share a single number space, so a PR-only run
        // can safely reuse its pr_number as the issue anchor. Preserving it via
        // `or(pr_number)` keeps a PR-only continuation (which
        // `check_identity_recoverable` accepts) from silently falling back to
        // the static config/default issue during reconstruction.
        // @plan:PLAN-20260623-LUTHER-CONTINUATION
        issue: md
            .issue_number
            .or(md.pr_number)
            .map(|anchor| anchor.to_string()),
        work_dir: md.workspace_path.as_ref().map(PathBuf::from),
        artifact_dir: md.artifact_root.as_ref().map(PathBuf::from),
    }
}

/// Where to rewind a run's resume point.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewindTarget {
    /// Rewind to the (unique) checkpoint recorded for this step.
    ToStep(String),
    /// Rewind to a checkpoint by identity, formatted `step_id@rfc3339`.
    ToCheckpoint(String),
}

/// The kind of continuation requested by the operator.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContinuationKind {
    /// Resume from the latest resumable checkpoint (or the pre-terminal one).
    Resume,
    /// Retry, optionally targeting the failed external-wait step.
    Retry { from_failed_step: bool },
    /// Rewind the resume point to a selected earlier checkpoint.
    Rewind { target: RewindTarget },
}

impl ContinuationKind {
    /// Short verb used in artifacts and audit events.
    pub fn verb(&self) -> &'static str {
        match self {
            ContinuationKind::Resume => "resume",
            ContinuationKind::Retry { .. } => "retry",
            ContinuationKind::Rewind { .. } => "rewind",
        }
    }
}

/// A fully-specified operator continuation request.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
#[derive(Debug, Clone)]
pub struct ContinuationRequest {
    pub run_id: String,
    pub kind: ContinuationKind,
    pub force: bool,
}

/// Errors that make a continuation impossible to plan or apply.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContinuationError {
    RunNotFound(String),
    NoResumableCheckpoint(String),
    CheckpointNotFound(String),
    InvalidTarget(String),
    Persistence(String),
}

impl std::fmt::Display for ContinuationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContinuationError::RunNotFound(id) => write!(f, "run {id} not found in registry"),
            ContinuationError::NoResumableCheckpoint(id) => {
                write!(f, "run {id} has no resumable checkpoint")
            }
            ContinuationError::CheckpointNotFound(what) => {
                write!(f, "checkpoint not found: {what}")
            }
            ContinuationError::InvalidTarget(detail) => write!(f, "invalid target: {detail}"),
            ContinuationError::Persistence(detail) => write!(f, "persistence error: {detail}"),
        }
    }
}

impl std::error::Error for ContinuationError {}

impl From<PersistenceError> for ContinuationError {
    fn from(err: PersistenceError) -> Self {
        ContinuationError::Persistence(err.to_string())
    }
}

impl From<rusqlite::Error> for ContinuationError {
    fn from(err: rusqlite::Error) -> Self {
        ContinuationError::Persistence(err.to_string())
    }
}

/// A single safety check result.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyCheck {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

/// Aggregate validation result for a continuation request.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinuationValidation {
    pub ok: bool,
    pub checks: Vec<SafetyCheck>,
}

impl ContinuationValidation {
    fn from_checks(checks: Vec<SafetyCheck>) -> Self {
        let ok = checks.iter().all(|c| c.passed);
        Self { ok, checks }
    }

    /// Human-readable reasons for the failed checks (empty when `ok`).
    pub fn failure_reasons(&self) -> Vec<String> {
        self.checks
            .iter()
            .filter(|c| !c.passed)
            .map(|c| format!("{}: {}", c.name, c.detail))
            .collect()
    }
}

fn pass(name: &str, detail: impl Into<String>) -> SafetyCheck {
    SafetyCheck {
        name: name.to_string(),
        passed: true,
        detail: detail.into(),
    }
}

fn fail(name: &str, detail: impl Into<String>) -> SafetyCheck {
    SafetyCheck {
        name: name.to_string(),
        passed: false,
        detail: detail.into(),
    }
}

fn check_run_exists(metadata: &Option<RunMetadata>, run_id: &str) -> SafetyCheck {
    match metadata {
        Some(_) => pass("run_exists", format!("run {run_id} found in registry")),
        None => fail("run_exists", format!("run {run_id} not found in registry")),
    }
}

fn check_workflow_resolvable(metadata: &RunMetadata) -> SafetyCheck {
    if metadata.workflow_type_id.is_empty() || metadata.config_id.is_empty() {
        fail(
            "workflow_resolvable",
            "run record is missing workflow_type_id or config_id",
        )
    } else {
        pass(
            "workflow_resolvable",
            format!(
                "workflow_type={} config={}",
                metadata.workflow_type_id, metadata.config_id
            ),
        )
    }
}

/// Refuse to reopen terminal non-failed runs (Completed/Merged/Abandoned/
/// Cancelled). `Failed` is the single intentional terminal exception, encoded in
/// `RunStatus::is_resumable`. A `Running` run is accepted when its recorded
/// workflow PID is stale or unrecorded, or when `--force` is specified for
/// operator recovery from PID recycling; all other terminal refusals remain
/// non-bypassable.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
fn check_resumable_status(metadata: &RunMetadata, force: bool) -> SafetyCheck {
    if metadata.status.is_resumable() {
        pass(
            "resumable_status",
            format!("run status {} is resumable", metadata.status),
        )
    } else if running_claim_is_available(metadata) {
        pass(
            "resumable_status",
            format!(
                "run status {} is resumable because recorded workflow PID is stale or unrecorded",
                metadata.status
            ),
        )
    } else if metadata.status == RunStatus::Running && force {
        pass(
            "resumable_status",
            format!(
                "run status {} is resumable because --force was specified",
                metadata.status
            ),
        )
    } else {
        fail(
            "resumable_status",
            format!(
                "run status {} is not resumable; terminal states other than failed cannot be continued",
                metadata.status
            ),
        )
    }
}

/// A continuation must always have a repository plus an issue or PR anchor before
/// executor dispatch; a repo-only or anchor-less row cannot safely target work.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
fn check_identity_recoverable(metadata: &RunMetadata) -> SafetyCheck {
    let has_repo = metadata.repository.is_some();
    let has_anchor = metadata.issue_number.is_some() || metadata.pr_number.is_some();
    if has_repo && has_anchor {
        pass(
            "identity_recoverable",
            format!(
                "repository={:?} issue={:?} pr={:?}",
                metadata.repository, metadata.issue_number, metadata.pr_number
            ),
        )
    } else {
        fail(
            "identity_recoverable",
            format!(
                "continuation requires a repository plus an issue or PR anchor; got repository={:?} issue={:?} pr={:?}",
                metadata.repository, metadata.issue_number, metadata.pr_number
            ),
        )
    }
}

fn check_workspace(metadata: &RunMetadata) -> SafetyCheck {
    match &metadata.workspace_path {
        Some(path) => pass("workspace", format!("workspace_path={path}")),
        None => pass("workspace", "workspace path reconstructable from run id"),
    }
}

fn check_checkpoint_exists(selection: &Result<Checkpoint, ContinuationError>) -> SafetyCheck {
    match selection {
        Ok(cp) => pass(
            "checkpoint_exists",
            format!("selected checkpoint at step {}", cp.step_id),
        ),
        Err(err) => fail("checkpoint_exists", err.to_string()),
    }
}

fn check_safe_step(step_id: &str, force: bool) -> SafetyCheck {
    if is_safe_rerun_step(step_id) {
        pass(
            "safe_step",
            format!("step {step_id} is in the safe-rerun whitelist"),
        )
    } else if force {
        pass(
            "safe_step",
            format!("step {step_id} is not whitelisted but --force was supplied"),
        )
    } else {
        fail(
            "safe_step",
            format!("step {step_id} is not safe to re-run without --force"),
        )
    }
}

/// Validate a continuation request against the issue's checkpoint-safety list.
/// Returns per-check diagnostics; the caller refuses when `!ok`.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn validate_continuation(
    conn: &Connection,
    request: &ContinuationRequest,
) -> Result<ContinuationValidation, PersistenceError> {
    let mut checks = Vec::new();
    let metadata = get_run_with_conn(conn, &request.run_id)?;
    checks.push(check_run_exists(&metadata, &request.run_id));
    let Some(metadata) = metadata else {
        return Ok(ContinuationValidation::from_checks(checks));
    };
    checks.push(check_workflow_resolvable(&metadata));
    checks.push(check_resumable_status(&metadata, request.force));
    checks.push(check_identity_recoverable(&metadata));
    checks.push(check_workspace(&metadata));
    let selection = select_checkpoint(conn, request, &metadata);
    checks.push(check_checkpoint_exists(&selection));
    if let Ok(cp) = &selection {
        checks.push(check_safe_step(&cp.step_id, request.force));
    }
    Ok(ContinuationValidation::from_checks(checks))
}

/// The stable identity string for a checkpoint: `step_id@rfc3339`.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn checkpoint_identity(cp: &Checkpoint) -> String {
    format!("{}@{}", cp.step_id, cp.timestamp.to_rfc3339())
}

fn parse_checkpoint_identity(id: &str) -> (String, Option<DateTime<Utc>>) {
    match id.split_once('@') {
        Some((step, ts)) => (
            step.to_string(),
            DateTime::parse_from_rfc3339(ts)
                .ok()
                .map(|d| d.with_timezone(&Utc)),
        ),
        None => (id.to_string(), None),
    }
}

fn newest_resumable(checkpoints: &[Checkpoint]) -> Option<Checkpoint> {
    checkpoints
        .iter()
        .rev()
        .find(|c| is_resumable_checkpoint_status(&c.state_snapshot.status))
        .cloned()
}

fn select_resume_checkpoint(
    conn: &Connection,
    run_id: &str,
    metadata: &RunMetadata,
) -> Result<Checkpoint, ContinuationError> {
    let checkpoints = list_checkpoints(conn, run_id)?;
    if checkpoints.is_empty() {
        return Err(ContinuationError::NoResumableCheckpoint(run_id.to_string()));
    }
    if let Some(cp) = newest_resumable(&checkpoints) {
        return Ok(cp);
    }
    // Terminal failed run: rewind to the checkpoint just before the terminal step.
    let terminal_step = metadata.current_step.as_deref().unwrap_or(TERMINAL_STEP);
    if let Some(cp) = load_checkpoint_before_step(conn, run_id, terminal_step)? {
        return Ok(cp);
    }
    checkpoints
        .last()
        .cloned()
        .ok_or_else(|| ContinuationError::NoResumableCheckpoint(run_id.to_string()))
}

fn select_retry_checkpoint(
    conn: &Connection,
    run_id: &str,
    metadata: &RunMetadata,
    from_failed_step: bool,
) -> Result<Checkpoint, ContinuationError> {
    if from_failed_step {
        let checkpoints = list_checkpoints(conn, run_id)?;
        if let Some(cp) = checkpoints
            .iter()
            .rev()
            .find(|c| RETRYABLE_WAIT_STEPS.contains(&c.step_id.as_str()))
            .cloned()
        {
            return Ok(cp);
        }
        return Err(ContinuationError::NoResumableCheckpoint(format!(
            "{run_id} has no retryable external-wait checkpoint"
        )));
    }
    select_resume_checkpoint(conn, run_id, metadata)
}

fn select_rewind_checkpoint(
    conn: &Connection,
    run_id: &str,
    target: &RewindTarget,
) -> Result<Checkpoint, ContinuationError> {
    let (step, expected_ts) = match target {
        RewindTarget::ToStep(step) => (step.clone(), None),
        RewindTarget::ToCheckpoint(id) => parse_checkpoint_identity(id),
    };
    let cp = get_checkpoint_for_step(conn, run_id, &step)?
        .ok_or_else(|| ContinuationError::CheckpointNotFound(format!("{run_id}:{step}")))?;
    if let Some(expected) = expected_ts {
        if cp.timestamp != expected {
            return Err(ContinuationError::InvalidTarget(format!(
                "checkpoint timestamp mismatch for {step}: stored {}, requested {}",
                cp.timestamp.to_rfc3339(),
                expected.to_rfc3339()
            )));
        }
    }
    Ok(cp)
}

/// Select the checkpoint a continuation request should resume from.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn select_checkpoint(
    conn: &Connection,
    request: &ContinuationRequest,
    metadata: &RunMetadata,
) -> Result<Checkpoint, ContinuationError> {
    match &request.kind {
        ContinuationKind::Resume => select_resume_checkpoint(conn, &request.run_id, metadata),
        ContinuationKind::Retry { from_failed_step } => {
            select_retry_checkpoint(conn, &request.run_id, metadata, *from_failed_step)
        }
        ContinuationKind::Rewind { target } => {
            select_rewind_checkpoint(conn, &request.run_id, target)
        }
    }
}

/// Re-stamp the selected checkpoint as the resume point and reopen the run
/// record, appending an audit event. History (events, prior checkpoint rows)
/// is preserved.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn commit_continuation(
    conn: &Connection,
    request: &ContinuationRequest,
    step_id: &str,
) -> Result<RunMetadata, ContinuationError> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let metadata = commit_continuation_in_transaction(&tx, request, step_id)?;
    tx.commit()?;
    Ok(metadata)
}

fn commit_continuation_in_transaction(
    conn: &Connection,
    request: &ContinuationRequest,
    step_id: &str,
) -> Result<RunMetadata, ContinuationError> {
    let metadata = get_run_with_conn(conn, &request.run_id)?
        .ok_or_else(|| ContinuationError::RunNotFound(request.run_id.clone()))?;
    ensure_reopen_claim_is_available(&metadata, request)?;
    set_resume_point(conn, &request.run_id, step_id)?;
    reopen_run(conn, request, step_id, metadata)
}

fn reopen_run(
    conn: &Connection,
    request: &ContinuationRequest,
    step_id: &str,
    mut metadata: RunMetadata,
) -> Result<RunMetadata, ContinuationError> {
    let prior_status = metadata.status.to_string();
    metadata.reopen();
    metadata.set_current_step(step_id);
    persist_run_with_conn(conn, &metadata)?;
    let detail = format!(
        "continuation={} from_status={prior_status} resume_step={step_id}",
        request.kind.verb()
    );
    append_typed_event_with_conn(
        conn,
        &request.run_id,
        step_id,
        "reopened",
        EventType::StepStart,
        Some(&detail),
        Utc::now(),
    )?;
    Ok(metadata)
}

fn ensure_reopen_claim_is_available(
    metadata: &RunMetadata,
    request: &ContinuationRequest,
) -> Result<(), ContinuationError> {
    if !reopen_status_is_allowed(metadata) {
        return Err(ContinuationError::InvalidTarget(format!(
            "run {} status {} is not resumable; terminal states other than failed cannot be continued",
            request.run_id, metadata.status
        )));
    }
    if !request.force {
        if let Some(pid) = live_running_claim_pid(metadata) {
            return Err(ContinuationError::InvalidTarget(format!(
                "run {} is already running with live workflow PID {pid}; retry with --force only if that process is unrelated",
                request.run_id
            )));
        }
    }
    Ok(())
}

fn reopen_status_is_allowed(metadata: &RunMetadata) -> bool {
    metadata.status.is_resumable() || metadata.status == RunStatus::Running
}

fn running_claim_is_available(metadata: &RunMetadata) -> bool {
    metadata.status == RunStatus::Running
        && (metadata.is_process_stale() || metadata.process_pid.is_none())
}

fn live_running_claim_pid(metadata: &RunMetadata) -> Option<u32> {
    if metadata.status == RunStatus::Running
        && metadata.process_pid.is_some()
        && !metadata.is_process_stale()
    {
        metadata.process_pid
    } else {
        None
    }
}

/// Directory under which continuation artifacts for a run are written.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn continuation_artifact_dir(metadata: &RunMetadata, run_id: &str) -> PathBuf {
    let root = metadata.artifact_root.clone().unwrap_or_else(|| {
        crate::runtime_paths::get_artifacts_root()
            .to_string_lossy()
            .to_string()
    });
    Path::new(&root).join("continuation").join(run_id)
}

/// Write a JSON artifact, creating parent directories as needed.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn write_json_artifact(dir: &Path, name: &str, value: &Value) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(name);
    let bytes = serde_json::to_vec_pretty(value).unwrap_or_default();
    std::fs::write(&path, bytes)?;
    Ok(path)
}

fn rewind_target_json(kind: &ContinuationKind) -> Value {
    match kind {
        ContinuationKind::Rewind { target } => match target {
            RewindTarget::ToStep(step) => json!({ "to_step": step }),
            RewindTarget::ToCheckpoint(id) => json!({ "to_checkpoint": id }),
        },
        _ => Value::Null,
    }
}

/// JSON body of `continuation-request.json`.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn request_artifact(request: &ContinuationRequest) -> Value {
    json!({
        "run_id": request.run_id,
        "kind": request.kind.verb(),
        "from_failed_step": matches!(
            request.kind,
            ContinuationKind::Retry { from_failed_step: true }
        ),
        "rewind_target": rewind_target_json(&request.kind),
        "force": request.force,
        "requested_at": Utc::now().to_rfc3339(),
        "why": "operator-initiated continuation of a failed or waiting run",
    })
}

fn validation_artifact(validation: &ContinuationValidation) -> Value {
    json!({
        "ok": validation.ok,
        "checks": validation
            .checks
            .iter()
            .map(|c| json!({ "name": c.name, "passed": c.passed, "detail": c.detail }))
            .collect::<Vec<_>>(),
        "validated_at": Utc::now().to_rfc3339(),
    })
}

fn selection_artifact(cp: &Checkpoint) -> Value {
    json!({
        "step_id": cp.step_id,
        "checkpoint_id": checkpoint_identity(cp),
        "status": cp.state_snapshot.status,
        "timestamp": cp.timestamp.to_rfc3339(),
        "loop_count": cp.state_snapshot.loop_count,
        "retry_count": cp.state_snapshot.retry_count,
    })
}

/// The result artifact file name for a continuation kind.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn result_artifact_name(kind: &ContinuationKind) -> &'static str {
    match kind {
        ContinuationKind::Retry { .. } => "retry-result.json",
        _ => "resume-result.json",
    }
}

/// JSON body of the `resume-result.json` / `retry-result.json` artifact.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn result_artifact(
    kind: &ContinuationKind,
    status_label: &str,
    resumed_step: &str,
    external_state: Option<&str>,
) -> Value {
    json!({
        "kind": kind.verb(),
        "resumed_step": resumed_step,
        "status": status_label,
        "external_state_observed": external_state,
        "completed_at": Utc::now().to_rfc3339(),
    })
}

/// Outcome of planning (validating + selecting) a continuation, with the
/// request/validation/selection artifacts already written.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
#[derive(Debug, Clone)]
pub struct ContinuationPlan {
    pub validation: ContinuationValidation,
    pub selected: Option<Checkpoint>,
    pub artifact_dir: PathBuf,
}

/// Validate the request, write request/validation artifacts, and (when valid)
/// select the checkpoint and write the selection artifact. Does not mutate run
/// state; call `commit_continuation` after a successful plan.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn prepare_continuation(
    conn: &Connection,
    request: &ContinuationRequest,
    metadata: &RunMetadata,
) -> Result<ContinuationPlan, ContinuationError> {
    let artifact_dir = continuation_artifact_dir(metadata, &request.run_id);
    let _ = write_json_artifact(
        &artifact_dir,
        "continuation-request.json",
        &request_artifact(request),
    );
    let validation = validate_continuation(conn, request)?;
    let _ = write_json_artifact(
        &artifact_dir,
        "continuation-validation.json",
        &validation_artifact(&validation),
    );
    if !validation.ok {
        return Ok(ContinuationPlan {
            validation,
            selected: None,
            artifact_dir,
        });
    }
    let checkpoint = select_checkpoint(conn, request, metadata)?;
    let _ = write_json_artifact(
        &artifact_dir,
        "checkpoint-selection.json",
        &selection_artifact(&checkpoint),
    );
    Ok(ContinuationPlan {
        validation,
        selected: Some(checkpoint),
        artifact_dir,
    })
}

#[cfg(test)]
mod tests;
