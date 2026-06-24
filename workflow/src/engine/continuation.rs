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
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::persistence::{
    append_typed_event_with_conn, get_checkpoint_for_step, get_run_with_conn,
    is_resumable_checkpoint_status, list_checkpoints, load_checkpoint_before_step,
    persist_run_with_conn, set_resume_point, Checkpoint, EventType, PersistenceError, RunMetadata,
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
        issue: md.issue_number.map(|issue| issue.to_string()),
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
/// `RunStatus::is_resumable`. This refusal is NOT bypassable by `--force`, which
/// only relaxes the safe-step whitelist.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
fn check_resumable_status(metadata: &RunMetadata) -> SafetyCheck {
    if metadata.status.is_resumable() {
        pass(
            "resumable_status",
            format!("run status {} is resumable", metadata.status),
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
    checks.push(check_resumable_status(&metadata));
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
    set_resume_point(conn, &request.run_id, step_id)?;
    reopen_run(conn, &request.run_id, &request.kind, step_id)
}

fn reopen_run(
    conn: &Connection,
    run_id: &str,
    kind: &ContinuationKind,
    step_id: &str,
) -> Result<RunMetadata, ContinuationError> {
    let mut metadata = get_run_with_conn(conn, run_id)?
        .ok_or_else(|| ContinuationError::RunNotFound(run_id.to_string()))?;
    let prior_status = metadata.status.to_string();
    metadata.reopen();
    metadata.set_current_step(step_id);
    persist_run_with_conn(conn, &metadata)?;
    let detail = format!(
        "continuation={} from_status={prior_status} resume_step={step_id}",
        kind.verb()
    );
    let _ = append_typed_event_with_conn(
        conn,
        run_id,
        step_id,
        "reopened",
        EventType::StepStart,
        Some(&detail),
        Utc::now(),
    );
    Ok(metadata)
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
mod tests {
    use super::*;
    use crate::persistence::checkpoint::init_checkpoint_table;
    use crate::persistence::run_metadata::init_runs_table;
    use crate::persistence::{
        save_checkpoint_with_conn, RunStatus, StateSnapshot, CHECKPOINT_STATUS_WAITING,
    };
    use std::collections::HashMap;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        init_checkpoint_table(&conn).expect("checkpoint table");
        init_runs_table(&conn).expect("runs table");
        conn
    }

    fn seed_run(conn: &Connection, run_id: &str, status: RunStatus, current_step: &str) {
        let mut md = RunMetadata::new(run_id, "llxprt-issue-fix", "llxprt-issue-fix-v1");
        md.status = status;
        md.current_step = Some(current_step.to_string());
        md.repository = Some("vybestack/llxprt-code".to_string());
        md.issue_number = Some(2133);
        md.pr_number = Some(2138);
        md.workspace_path = Some("/tmp/ws".to_string());
        persist_run_with_conn(conn, &md).expect("persist run");
    }

    fn seed_checkpoint(conn: &Connection, run_id: &str, step: &str, status: &str) {
        let snapshot = StateSnapshot {
            retry_count: 0,
            loop_count: 0,
            edge_loop_counts: HashMap::new(),
            context: HashMap::new(),
            status: status.to_string(),
        };
        let cp = Checkpoint::with_snapshot(run_id, step, snapshot);
        save_checkpoint_with_conn(conn, &cp).expect("save checkpoint");
        // Ensure distinct, increasing timestamps for ordering assertions.
        std::thread::sleep(std::time::Duration::from_millis(2));
    }

    fn seed_terminal_failed_run(conn: &Connection, run_id: &str) {
        seed_run(conn, run_id, RunStatus::Failed, TERMINAL_STEP);
        seed_checkpoint(conn, run_id, "capture_pr_identity", "completed");
        seed_checkpoint(conn, run_id, "post_pr_iteration_guard", "completed");
        seed_checkpoint(conn, run_id, "watch_pr_checks", "completed");
        seed_checkpoint(conn, run_id, "collect_ci_failures", "completed");
        seed_checkpoint(conn, run_id, TERMINAL_STEP, "completed");
    }

    fn request(run_id: &str, kind: ContinuationKind, force: bool) -> ContinuationRequest {
        ContinuationRequest {
            run_id: run_id.to_string(),
            kind,
            force,
        }
    }

    #[test]
    fn validation_fails_when_run_missing() {
        let conn = test_conn();
        let req = request("absent", ContinuationKind::Resume, false);
        let validation = validate_continuation(&conn, &req).expect("validate");
        assert!(!validation.ok);
        assert!(validation
            .failure_reasons()
            .iter()
            .any(|r| r.contains("run_exists")));
    }

    #[test]
    fn validation_passes_for_terminal_failed_resume() {
        let conn = test_conn();
        seed_terminal_failed_run(&conn, "run-1");
        let req = request("run-1", ContinuationKind::Resume, false);
        let validation = validate_continuation(&conn, &req).expect("validate");
        assert!(validation.ok, "reasons: {:?}", validation.failure_reasons());
    }

    #[test]
    fn validation_rejects_unsafe_step_without_force() {
        let conn = test_conn();
        seed_run(&conn, "run-2", RunStatus::Failed, "implement");
        seed_checkpoint(&conn, "run-2", "implement", "completed");
        let req = request(
            "run-2",
            ContinuationKind::Rewind {
                target: RewindTarget::ToStep("implement".to_string()),
            },
            false,
        );
        let validation = validate_continuation(&conn, &req).expect("validate");
        assert!(!validation.ok);
        assert!(validation
            .failure_reasons()
            .iter()
            .any(|r| r.contains("safe_step")));
    }

    #[test]
    fn validation_allows_unsafe_step_with_force() {
        let conn = test_conn();
        seed_run(&conn, "run-3", RunStatus::Failed, "implement");
        seed_checkpoint(&conn, "run-3", "implement", "completed");
        let req = request(
            "run-3",
            ContinuationKind::Rewind {
                target: RewindTarget::ToStep("implement".to_string()),
            },
            true,
        );
        let validation = validate_continuation(&conn, &req).expect("validate");
        assert!(validation.ok, "reasons: {:?}", validation.failure_reasons());
    }

    #[test]
    fn validation_rejects_missing_rewind_step() {
        let conn = test_conn();
        seed_terminal_failed_run(&conn, "run-4");
        let req = request(
            "run-4",
            ContinuationKind::Rewind {
                target: RewindTarget::ToStep("does_not_exist".to_string()),
            },
            false,
        );
        let validation = validate_continuation(&conn, &req).expect("validate");
        assert!(!validation.ok);
        assert!(validation
            .failure_reasons()
            .iter()
            .any(|r| r.contains("checkpoint_exists")));
    }

    #[test]
    fn resume_selects_checkpoint_before_terminal_step() {
        let conn = test_conn();
        seed_terminal_failed_run(&conn, "run-5");
        let md = get_run_with_conn(&conn, "run-5").unwrap().unwrap();
        let req = request("run-5", ContinuationKind::Resume, false);
        let cp = select_checkpoint(&conn, &req, &md).expect("select");
        assert_eq!(cp.step_id, "collect_ci_failures");
    }

    #[test]
    fn resume_prefers_waiting_checkpoint_when_present() {
        let conn = test_conn();
        seed_run(
            &conn,
            "run-6",
            RunStatus::WaitingForChecks,
            "watch_pr_checks",
        );
        seed_checkpoint(&conn, "run-6", "capture_pr_identity", "completed");
        seed_checkpoint(&conn, "run-6", "watch_pr_checks", CHECKPOINT_STATUS_WAITING);
        let md = get_run_with_conn(&conn, "run-6").unwrap().unwrap();
        let req = request("run-6", ContinuationKind::Resume, false);
        let cp = select_checkpoint(&conn, &req, &md).expect("select");
        assert_eq!(cp.step_id, "watch_pr_checks");
        assert_eq!(cp.state_snapshot.status, CHECKPOINT_STATUS_WAITING);
    }

    #[test]
    fn retry_from_failed_step_selects_watch_pr_checks() {
        let conn = test_conn();
        seed_terminal_failed_run(&conn, "run-7");
        let md = get_run_with_conn(&conn, "run-7").unwrap().unwrap();
        let req = request(
            "run-7",
            ContinuationKind::Retry {
                from_failed_step: true,
            },
            false,
        );
        let cp = select_checkpoint(&conn, &req, &md).expect("select");
        assert_eq!(cp.step_id, "watch_pr_checks");
    }

    #[test]
    fn rewind_to_checkpoint_validates_timestamp() {
        let conn = test_conn();
        seed_terminal_failed_run(&conn, "run-8");
        let md = get_run_with_conn(&conn, "run-8").unwrap().unwrap();
        let guard = get_checkpoint_for_step(&conn, "run-8", "post_pr_iteration_guard")
            .unwrap()
            .unwrap();
        let identity = checkpoint_identity(&guard);
        let req = request(
            "run-8",
            ContinuationKind::Rewind {
                target: RewindTarget::ToCheckpoint(identity),
            },
            false,
        );
        let cp = select_checkpoint(&conn, &req, &md).expect("select");
        assert_eq!(cp.step_id, "post_pr_iteration_guard");
    }

    #[test]
    fn rewind_to_checkpoint_rejects_timestamp_mismatch() {
        let conn = test_conn();
        seed_terminal_failed_run(&conn, "run-9");
        let bogus = "watch_pr_checks@2000-01-01T00:00:00+00:00".to_string();
        let err = select_rewind_checkpoint(&conn, "run-9", &RewindTarget::ToCheckpoint(bogus))
            .expect_err("mismatch must error");
        assert!(matches!(err, ContinuationError::InvalidTarget(_)));
    }

    #[test]
    fn commit_continuation_reopens_run_and_rearms_checkpoint() {
        let conn = test_conn();
        seed_terminal_failed_run(&conn, "run-10");
        let req = request("run-10", ContinuationKind::Resume, false);
        let md = commit_continuation(&conn, &req, "collect_ci_failures").expect("commit");
        assert_eq!(md.status, RunStatus::Running);
        // The re-stamped checkpoint becomes the newest and is ready_to_resume.
        let newest = crate::persistence::load_checkpoint_with_conn(&conn, "run-10")
            .unwrap()
            .unwrap();
        assert_eq!(newest.step_id, "collect_ci_failures");
        assert_eq!(
            newest.state_snapshot.status,
            crate::persistence::CHECKPOINT_STATUS_READY_TO_RESUME
        );
    }

    #[test]
    fn prepare_continuation_writes_artifacts() {
        let conn = test_conn();
        let temp = tempfile::tempdir().expect("tempdir");
        seed_terminal_failed_run(&conn, "run-11");
        let mut md = get_run_with_conn(&conn, "run-11").unwrap().unwrap();
        md.artifact_root = Some(temp.path().to_string_lossy().to_string());
        let req = request("run-11", ContinuationKind::Resume, false);
        let plan = prepare_continuation(&conn, &req, &md).expect("prepare");
        assert!(plan.validation.ok);
        assert!(plan.artifact_dir.join("continuation-request.json").exists());
        assert!(plan
            .artifact_dir
            .join("continuation-validation.json")
            .exists());
        assert!(plan.artifact_dir.join("checkpoint-selection.json").exists());
    }

    #[test]
    fn prepare_continuation_writes_validation_on_failure() {
        let conn = test_conn();
        let temp = tempfile::tempdir().expect("tempdir");
        seed_run(&conn, "run-12", RunStatus::Failed, "implement");
        seed_checkpoint(&conn, "run-12", "implement", "completed");
        let mut md = get_run_with_conn(&conn, "run-12").unwrap().unwrap();
        md.artifact_root = Some(temp.path().to_string_lossy().to_string());
        let req = request(
            "run-12",
            ContinuationKind::Rewind {
                target: RewindTarget::ToStep("implement".to_string()),
            },
            false,
        );
        let plan = prepare_continuation(&conn, &req, &md).expect("prepare");
        assert!(!plan.validation.ok);
        assert!(plan.selected.is_none());
        assert!(plan
            .artifact_dir
            .join("continuation-validation.json")
            .exists());
    }

    #[test]
    fn result_artifact_name_differs_for_retry() {
        assert_eq!(
            result_artifact_name(&ContinuationKind::Resume),
            "resume-result.json"
        );
        assert_eq!(
            result_artifact_name(&ContinuationKind::Retry {
                from_failed_step: true
            }),
            "retry-result.json"
        );
    }

    /// Continuation kinds that should be rejected uniformly when a run is in a
    /// non-resumable terminal state, regardless of `--force`.
    fn resumable_kinds() -> Vec<ContinuationKind> {
        vec![
            ContinuationKind::Resume,
            ContinuationKind::Retry {
                from_failed_step: false,
            },
            ContinuationKind::Retry {
                from_failed_step: true,
            },
            ContinuationKind::Rewind {
                target: RewindTarget::ToStep("watch_pr_checks".to_string()),
            },
        ]
    }

    /// Seed a run in `status` with a whitelisted, resumable `watch_pr_checks`
    /// checkpoint, then assert every continuation kind is rejected with a
    /// `resumable_status` failure, even with `force = true`.
    fn assert_non_resumable_rejected(status: RunStatus) {
        let conn = test_conn();
        seed_run(&conn, "term", status.clone(), "watch_pr_checks");
        seed_checkpoint(&conn, "term", "watch_pr_checks", CHECKPOINT_STATUS_WAITING);
        for kind in resumable_kinds() {
            for force in [false, true] {
                let req = request("term", kind.clone(), force);
                let validation = validate_continuation(&conn, &req).expect("validate");
                assert!(
                    !validation.ok,
                    "status {status:?} kind {kind:?} force={force} must be rejected"
                );
                assert!(
                    validation
                        .failure_reasons()
                        .iter()
                        .any(|r| r.contains("resumable_status")),
                    "expected resumable_status failure for {status:?} (got {:?})",
                    validation.failure_reasons()
                );
            }
        }
    }

    #[test]
    fn validation_rejects_completed_run() {
        // @plan:PLAN-20260623-LUTHER-CONTINUATION
        assert_non_resumable_rejected(RunStatus::Completed);
    }

    #[test]
    fn validation_rejects_merged_run() {
        // @plan:PLAN-20260623-LUTHER-CONTINUATION
        assert_non_resumable_rejected(RunStatus::Merged);
    }

    #[test]
    fn validation_rejects_abandoned_run() {
        // @plan:PLAN-20260623-LUTHER-CONTINUATION
        assert_non_resumable_rejected(RunStatus::Abandoned);
    }

    #[test]
    fn validation_rejects_cancelled_run() {
        // @plan:PLAN-20260623-LUTHER-CONTINUATION
        assert_non_resumable_rejected(RunStatus::Cancelled);
    }

    #[test]
    fn validation_accepts_resumable_statuses() {
        // @plan:PLAN-20260623-LUTHER-CONTINUATION
        for status in [
            RunStatus::Failed,
            RunStatus::WaitingForChecks,
            RunStatus::Paused,
            RunStatus::Blocked,
        ] {
            let conn = test_conn();
            seed_run(&conn, "ok", status.clone(), "watch_pr_checks");
            seed_checkpoint(&conn, "ok", "watch_pr_checks", CHECKPOINT_STATUS_WAITING);
            let req = request("ok", ContinuationKind::Resume, false);
            let validation = validate_continuation(&conn, &req).expect("validate");
            assert!(
                validation.ok,
                "status {status:?} should be resumable; reasons: {:?}",
                validation.failure_reasons()
            );
        }
    }

    #[test]
    fn validation_rejects_repo_only_identity() {
        // @plan:PLAN-20260623-LUTHER-CONTINUATION
        let conn = test_conn();
        let mut md = RunMetadata::new("anchorless", "wf", "cfg");
        md.status = RunStatus::Failed;
        md.current_step = Some("watch_pr_checks".to_string());
        md.repository = Some("vybestack/llxprt-code".to_string());
        // Neither issue_number nor pr_number recorded.
        persist_run_with_conn(&conn, &md).expect("persist run");
        seed_checkpoint(
            &conn,
            "anchorless",
            "watch_pr_checks",
            CHECKPOINT_STATUS_WAITING,
        );
        let req = request("anchorless", ContinuationKind::Resume, false);
        let validation = validate_continuation(&conn, &req).expect("validate");
        assert!(!validation.ok);
        assert!(validation
            .failure_reasons()
            .iter()
            .any(|r| r.contains("identity_recoverable")));
    }

    #[test]
    fn continuation_overrides_maps_recorded_identity() {
        // @plan:PLAN-20260623-LUTHER-CONTINUATION
        let mut md = RunMetadata::new("r", "wf", "cfg");
        md.repository = Some("vybestack/llxprt-luther".to_string());
        md.issue_number = Some(65);
        md.workspace_path = Some("/tmp/luther-workspaces/llxprt-luther".to_string());
        md.artifact_root = Some("/tmp/luther-artifacts/llxprt-luther".to_string());

        let overrides = continuation_overrides(&md);

        assert_eq!(overrides.repo.as_deref(), Some("vybestack/llxprt-luther"));
        assert_eq!(overrides.issue.as_deref(), Some("65"));
        assert_eq!(
            overrides.work_dir,
            Some(PathBuf::from("/tmp/luther-workspaces/llxprt-luther"))
        );
        assert_eq!(
            overrides.artifact_dir,
            Some(PathBuf::from("/tmp/luther-artifacts/llxprt-luther"))
        );
    }

    #[test]
    fn continuation_overrides_omits_unrecorded_fields() {
        // @plan:PLAN-20260623-LUTHER-CONTINUATION
        let md = RunMetadata::new("r", "wf", "cfg");
        let overrides = continuation_overrides(&md);
        assert!(
            overrides.is_empty(),
            "a run with no recorded identity must not emit overrides"
        );
    }
}
