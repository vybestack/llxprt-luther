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
//! The logic is organized into cohesive submodules for validation, checkpoint
//! selection, authorization, transactional commit, audit artifacts, and the
//! durable `.luther/workspace-owner` ownership marker.
//!
//! @plan:PLAN-20260623-LUTHER-CONTINUATION

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::persistence::{Checkpoint, PersistenceError, RunMetadata};
use crate::workflow::target_profile::TargetProfileOverrides;

mod artifacts;
mod authorization;
mod commit;
mod selection;
mod validation;
mod workspace_marker;

// Re-export the public API of the submodules so existing callers
// (`crate::engine::continuation::...` and the `engine` re-exports) are
// unaffected by the internal split.
pub use artifacts::{
    continuation_artifact_dir, request_artifact, result_artifact, result_artifact_name,
    write_json_artifact,
};
pub use authorization::ResumeAuthorization;
pub use commit::commit_continuation;
pub use selection::select_checkpoint;
pub use validation::validate_continuation;

// Test-facing helpers re-exported so the inline test module can reach
// submodule-internal items they historically called directly. These are
// cfg(test)-gated so the non-test public API is unchanged.
#[cfg(test)]
pub(crate) use selection::{select_rewind_checkpoint, TERMINAL_STEP};
pub(crate) use workspace_marker::verify_workspace_ownership_marker;

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
    /// Explicit internal-trust capability for engine-internal resume paths
    /// (daemon launcher, parent-orchestration child resume). This is never
    /// set by CLI handlers, so an operator `runs resume` cannot infer
    /// `TrustedInternalWait` authorization from durable wait state alone.
    /// The capability is revalidated against the durable `wait_states` row
    /// during authorization and inside the commit transaction, failing closed
    /// to [`ResumeAuthorization::Operator`] when the wait identity does not
    /// match.
    pub trusted_internal: bool,
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

impl From<std::io::Error> for ContinuationError {
    fn from(err: std::io::Error) -> Self {
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
    pub(super) fn from_checks(checks: Vec<SafetyCheck>) -> Self {
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

/// The stable identity string for a checkpoint: `step_id@rfc3339`.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn checkpoint_identity(cp: &Checkpoint) -> String {
    format!("{}@{}", cp.step_id, cp.timestamp.to_rfc3339())
}

/// Parse a `step_id@rfc3339` checkpoint identity into its step and timestamp.
/// Returns an error when the `@` separator is absent or the timestamp is not
/// valid RFC3339, so a malformed `ToCheckpoint` target cannot degrade into a
/// step-only match that defeats exact checkpoint binding.
pub(super) fn parse_checkpoint_identity_target(
    id: &str,
) -> Result<(String, DateTime<Utc>), ContinuationError> {
    let Some((step, ts)) = id.split_once('@') else {
        return Err(ContinuationError::InvalidTarget(format!(
            "checkpoint identity '{id}' is missing the '@' separator"
        )));
    };
    let parsed = DateTime::parse_from_rfc3339(ts).map_err(|err| {
        ContinuationError::InvalidTarget(format!(
            "checkpoint identity '{id}' has an invalid RFC3339 timestamp: {err}"
        ))
    })?;
    Ok((step.to_string(), parsed.with_timezone(&Utc)))
}

/// Outcome of planning (validating + selecting) a continuation, with the
/// request/validation/selection artifacts already written.
///
/// `checkpoint_identity` binds the prepared plan to the exact checkpoint
/// (`step_id@rfc3339`) selected at plan time. [`commit_continuation`] compares
/// this identity inside its `IMMEDIATE` transaction against the freshly
/// re-selected checkpoint before any lease or run mutation, so a concurrent
/// same-step checkpoint replacement between plan and commit cannot sneak
/// through as a stale or substituted resume point.
///
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
#[derive(Debug, Clone)]
pub struct ContinuationPlan {
    pub validation: ContinuationValidation,
    pub selected: Option<Checkpoint>,
    /// Exact `step_id@rfc3339` identity of the planned checkpoint. Bound at
    /// plan time and verified inside `commit_continuation`'s transaction.
    pub checkpoint_identity: String,
    pub artifact_dir: PathBuf,
}

/// Validate the request, write request/validation artifacts, and (when valid)
/// select the checkpoint and write the selection artifact. Does not mutate run
/// state; call `commit_continuation` after a successful plan.
///
/// The returned [`ContinuationPlan`] binds its `checkpoint_identity` to the
/// exact `step_id@rfc3339` of the selected checkpoint so `commit_continuation`
/// can verify it inside the transaction.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn prepare_continuation(
    conn: &rusqlite::Connection,
    request: &ContinuationRequest,
    metadata: &RunMetadata,
) -> Result<ContinuationPlan, ContinuationError> {
    let artifact_dir = continuation_artifact_dir(metadata, &request.run_id);
    write_json_artifact(
        &artifact_dir,
        "continuation-request.json",
        &request_artifact(request),
    )?;
    let validation = validate_continuation(conn, request)?;
    write_json_artifact(
        &artifact_dir,
        "continuation-validation.json",
        &artifacts::validation_artifact(&validation),
    )?;
    if !validation.ok {
        return Ok(ContinuationPlan {
            validation,
            selected: None,
            checkpoint_identity: String::new(),
            artifact_dir,
        });
    }
    let checkpoint = select_checkpoint(conn, request, metadata)?;
    let checkpoint_identity = checkpoint_identity(&checkpoint);
    write_json_artifact(
        &artifact_dir,
        "checkpoint-selection.json",
        &artifacts::selection_artifact(&checkpoint),
    )?;
    Ok(ContinuationPlan {
        validation,
        selected: Some(checkpoint),
        checkpoint_identity,
        artifact_dir,
    })
}

// Re-export the workspace owner marker provisioning API (used by workspace
// creation call sites) so external `crate::engine::continuation::...` paths keep
// resolving after the marker logic moved into a submodule.
pub use workspace_marker::write_workspace_owner_marker;

#[cfg(test)]
mod tests;
