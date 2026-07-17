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
/// workflow PID is stale or unrecorded, or when `--force` is specified. Force
/// overrides even a live Running claim, so operators must only use it after
/// confirming the recorded process is unrelated; all other terminal refusals
/// remain non-bypassable.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
fn check_resumable_status(metadata: &RunMetadata, request: &ContinuationRequest) -> SafetyCheck {
    let cleanup_recovery = metadata.is_cleanup_failure_abandonment()
        && metadata
            .failure_cleanup
            .as_ref()
            .is_some_and(crate::persistence::FailureCleanupState::recovery_is_available)
        && request.force
        && matches!(
            request.kind,
            ContinuationKind::Retry { .. } | ContinuationKind::Rewind { .. }
        );
    if metadata.status.is_resumable() {
        pass(
            "resumable_status",
            format!("run status {} is resumable", metadata.status),
        )
    } else if cleanup_recovery {
        pass(
            "resumable_status",
            "forced retry/rewind of an evidenced cleanup abandonment",
        )
    } else if running_claim_is_available(metadata) {
        pass(
            "resumable_status",
            format!(
                "run status {} is resumable because recorded workflow PID is stale or unrecorded",
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
        Some(path) if metadata.is_cleanup_failure_abandonment() => {
            check_cleanup_workspace_ownership(Path::new(path), metadata)
        }
        Some(path) if Path::new(path).is_dir() => {
            pass("workspace", format!("workspace_path={path}"))
        }
        Some(path) => pass("workspace", format!("workspace_path={path}")),
        None if metadata.is_cleanup_failure_abandonment() => fail(
            "workspace",
            "cleanup-abandonment recovery requires an explicit preserved workspace",
        ),
        None => pass("workspace", "workspace path reconstructable from run id"),
    }
}

/// Strengthened workspace ownership validation for cleanup-failure-abandonment
/// recovery. Rejects symlinks, requires the workspace to be a real directory,
/// and verifies durable workspace ownership via the `.luther/workspace-owner`
/// regular-file marker bound to the run_id, preventing wrong-owner
/// substitutions.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
fn check_cleanup_workspace_ownership(path: &Path, metadata: &RunMetadata) -> SafetyCheck {
    // Reject symlinks: a symlinked workspace could redirect cleanup recovery
    // into an attacker-controlled directory. Use symlink_metadata so the link
    // itself is inspected rather than its target.
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return fail(
                "workspace",
                format!(
                    "cleanup workspace must not be a symlink: {path_display}",
                    path_display = path.display()
                ),
            );
        }
        Ok(_) => {}
        Err(_) => {
            return fail(
                "workspace",
                format!(
                    "preserved workspace is missing or not a directory: {path_display}",
                    path_display = path.display()
                ),
            );
        }
    }
    if !path.is_dir() {
        return fail(
            "workspace",
            format!(
                "preserved workspace is not a directory: {path_display}",
                path_display = path.display()
            ),
        );
    }
    // Verify durable workspace ownership via the `.luther/workspace-owner`
    // regular-file marker, which records the run_id that created the
    // workspace. This prevents a wrong-owner substitution where a different
    // run's workspace is pointed at by the recorded workspace_path. The
    // marker is required: a missing, empty, non-regular (directory/symlink),
    // or mismatched marker all fail closed.
    if let Some(reason) = verify_workspace_ownership_marker(path, &metadata.run_id) {
        return fail("workspace", reason);
    }
    pass(
        "workspace",
        format!(
            "workspace_path={path_display} (ownership verified)",
            path_display = path.display()
        ),
    )
}

/// Marker file path recording the owning run id for a workspace.
fn workspace_owner_marker_path(workspace: &Path) -> PathBuf {
    workspace.join(".luther").join("workspace-owner")
}

/// Reject a symlinked `.luther` parent directory: a symlinked `.luther` could
/// redirect the workspace-owner marker to an attacker-controlled location. The
/// check uses `symlink_metadata` so the link itself is inspected rather than
/// its target, matching the symlink rejection already applied to the workspace
/// root and the marker file.
fn reject_symlinked_luther_parent(workspace: &Path) -> Option<String> {
    let luther = workspace.join(".luther");
    if let Ok(meta) = std::fs::symlink_metadata(&luther) {
        if meta.file_type().is_symlink() {
            return Some(format!(
                "workspace `.luther` parent is a symlink and must be a real directory: {luther_display}",
                luther_display = luther.display()
            ));
        }
    }
    None
}

/// Write the `.luther/workspace-owner` marker recording `run_id` as the owner
/// of `workspace`. Creates `.luther/` and the marker regular file, refusing to
/// overwrite an existing marker that belongs to a different run so two
/// concurrent runs cannot claim the same workspace. Returns `Ok(())` when the
/// marker already records the same `run_id`.
///
/// Atomicity: the marker file is created with an exclusive `O_CREAT | O_EXCL`
/// primitive (`OpenOptions::create_new`) so that a concurrent first-writer for
/// the same workspace wins and all later writers observe the committed content.
/// This closes the check-then-write TOCTOU window that a naive
/// metadata-check-then-overwrite would leave between the existence probe and
/// `write`.
///
/// This is the durable ownership anchor consulted by
/// [`check_cleanup_workspace_ownership`] during cleanup-failure-abandonment
/// recovery. Provisioning call sites (workspace creation) should write it once
/// when a run's workspace is created.
///
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn write_workspace_owner_marker(workspace: &Path, run_id: &str) -> std::io::Result<()> {
    use std::io::Write;
    let marker = workspace_owner_marker_path(workspace);
    // Reject a symlinked `.luther` parent before creating it: `create_dir_all`
    // would happily follow an existing symlink and place the marker outside the
    // real workspace.
    if let Some(reason) = reject_symlinked_luther_parent(workspace) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            reason,
        ));
    }
    std::fs::create_dir_all(marker.parent().unwrap_or(Path::new(".")))?;
    // Re-check the `.luther` parent after creation: a concurrent attacker could
    // replace the freshly created directory with a symlink between the first
    // check and now.
    if let Some(reason) = reject_symlinked_luther_parent(workspace) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            reason,
        ));
    }
    // Atomic create-new: wins exactly one concurrent writer. Existing files
    // fall through to the same-owner / different-owner inspection below.
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).read(true).create_new(true);
    match opts.open(&marker) {
        Ok(mut file) => {
            file.write_all(run_id.as_bytes())?;
            file.flush()?;
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            // Existing marker: validate it is a regular file with a matching
            // or empty owner. Every malformed condition is rejected.
            inspect_existing_marker(&marker, run_id)
        }
        Err(err) => Err(err),
    }
}

/// Validate an existing marker file: reject symlinks, directories, empty
/// content, and a different owner. Returns `Ok(())` only for exact same-owner
/// idempotency.
fn inspect_existing_marker(marker: &Path, run_id: &str) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(marker)?;
    if meta.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "workspace owner marker is a symlink and must be a regular file: {marker_display}",
                marker_display = marker.display()
            ),
        ));
    }
    if meta.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "workspace owner marker is a directory and must be a regular file: {marker_display}",
                marker_display = marker.display()
            ),
        ));
    }
    let existing = std::fs::read_to_string(marker)?;
    let trimmed = existing.trim();
    if trimmed.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "workspace owner marker is empty and cannot establish ownership: {}",
                marker.display()
            ),
        ));
    }
    if trimmed != run_id {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("workspace owner marker belongs to run '{trimmed}' not '{run_id}'",),
        ));
    }
    Ok(())
}

/// Result of verifying a workspace ownership marker: `None` means the
/// workspace is trusted, `Some(reason)` explains the rejection.
///
/// Fails closed for every malformed condition: a missing marker, an empty
/// marker, a directory marker, a symlink marker, an unreadable marker, or a
/// marker whose recorded owner differs from `run_id` are all rejected. There
/// is no backward-compatibility exemption: the marker is mandatory for
/// cleanup-failure-abandonment recovery.
fn verify_workspace_ownership_marker(workspace: &Path, run_id: &str) -> Option<String> {
    let marker = workspace_owner_marker_path(workspace);
    // Reject a symlinked `.luther` parent: a symlink could redirect the marker
    // to an attacker-controlled location.
    if let Some(reason) = reject_symlinked_luther_parent(workspace) {
        return Some(reason);
    }
    let meta = match std::fs::symlink_metadata(&marker) {
        Ok(meta) => meta,
        Err(_) => {
            return Some(format!(
                "workspace ownership marker is missing: {marker_display}",
                marker_display = marker.display()
            ));
        }
    };
    if meta.file_type().is_symlink() {
        return Some(format!(
            "workspace ownership marker is a symlink and must be a regular file: {marker_display}",
            marker_display = marker.display()
        ));
    }
    if meta.is_dir() {
        return Some(format!(
            "workspace ownership marker is a directory and must be a regular file: {marker_display}",
            marker_display = marker.display()
        ));
    }
    match std::fs::read_to_string(&marker) {
        Ok(contents) => {
            let trimmed = contents.trim();
            if trimmed.is_empty() {
                Some(format!(
                    "workspace ownership marker is empty: {marker_display}",
                    marker_display = marker.display()
                ))
            } else if trimmed == run_id {
                None
            } else {
                Some(format!(
                    "workspace ownership marker belongs to run '{marker_owner}' not '{run_id}'",
                    marker_owner = trimmed
                ))
            }
        }
        Err(err) => Some(format!("workspace ownership marker is not readable: {err}")),
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

/// Architecturally typed authorization distinguishing operator-initiated
/// continuations from trusted-internal engine resumptions.
///
/// This type exists so a non-`SAFE_RERUN_STEPS` step can be resumed by the
/// engine only when the resumption is provably bound to an exact persisted
/// durable wait for the same run. It is never constructable from a CLI
/// `--force` flag, so operator safety is preserved: `Operator` authorization
/// remains subject to every rerun-safety rule and cannot bypass `safe_step`.
///
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeAuthorization {
    /// Operator-initiated continuation (CLI `runs resume/retry/rewind`).
    /// Subject to all rerun-safety rules; `--force` does not bypass
    /// `safe_step`.
    Operator,
    /// Engine-internal authorization bound to an exact persisted waiting
    /// checkpoint identity and run. Permits resuming a valid durable wait
    /// whose step is not in `SAFE_RERUN_STEPS` without exposing a generic
    /// operator bypass. The binding is verified against the durable
    /// `wait_states` row at validation and again inside the commit
    /// transaction so a stale or substituted checkpoint cannot be elevated.
    TrustedInternalWait {
        checkpoint_identity: String,
        run_id: String,
    },
}

impl ResumeAuthorization {
    /// Resolve the strongest authorization applicable to resuming `checkpoint`
    /// for `run_id` from the persisted durable wait state.
    ///
    /// Returns [`ResumeAuthorization::TrustedInternalWait`] only when a
    /// complete `wait_states` row exists for the exact `run_id`, its
    /// `checkpoint_id` matches the selected checkpoint identity, and its
    /// `resume_step` matches the checkpoint's step. Otherwise the caller is
    /// treated as a plain [`ResumeAuthorization::Operator`].
    ///
    /// @plan:PLAN-20260623-LUTHER-CONTINUATION
    fn for_resume(
        conn: &Connection,
        request: &ContinuationRequest,
        checkpoint: &Checkpoint,
    ) -> ResumeAuthorization {
        if !matches!(request.kind, ContinuationKind::Resume) {
            return ResumeAuthorization::Operator;
        }
        let identity = checkpoint_identity(checkpoint);
        let trusted = crate::persistence::get_wait_state(conn, &request.run_id)
            .ok()
            .flatten()
            .is_some_and(|wait| {
                wait.run_id == request.run_id
                    && wait.checkpoint_id == identity
                    && wait.resume_step == checkpoint.step_id
            });
        if trusted {
            ResumeAuthorization::TrustedInternalWait {
                checkpoint_identity: identity,
                run_id: request.run_id.clone(),
            }
        } else {
            ResumeAuthorization::Operator
        }
    }

    /// Whether this authorization permits resuming `checkpoint` despite its
    /// step not being in `SAFE_RERUN_STEPS`.
    ///
    /// Only a [`ResumeAuthorization::TrustedInternalWait`] bound to the exact
    /// checkpoint identity and run authorizes the bypass;
    /// [`ResumeAuthorization::Operator`] never does.
    fn permits_non_safe_rerun(&self, checkpoint: &Checkpoint) -> bool {
        match self {
            ResumeAuthorization::TrustedInternalWait {
                checkpoint_identity: bound_identity,
                run_id: bound_run_id,
            } => {
                *bound_identity == checkpoint_identity(checkpoint)
                    && bound_run_id == &checkpoint.run_id
            }
            ResumeAuthorization::Operator => false,
        }
    }
}

fn authorizes_cleanup_resume(
    metadata: &RunMetadata,
    request: &ContinuationRequest,
    checkpoint: &Checkpoint,
) -> bool {
    metadata.failure_cleanup.as_ref().is_some_and(|failure| {
        if !matches!(request.kind, ContinuationKind::Resume) {
            return false;
        }
        let exact_failed_checkpoint =
            checkpoint_identity(checkpoint) == failure.failed_checkpoint_id;
        (!failure.cleanup_succeeded
            && (exact_failed_checkpoint || checkpoint.step_id == failure.cleanup_step))
            || (metadata.status == RunStatus::Running
                && failure.is_complete()
                && failure.recovery_consumed_at.is_some()
                && exact_failed_checkpoint)
    })
}

/// Compute whether `checkpoint` is authorized to re-run outside
/// `SAFE_RERUN_STEPS` via either cleanup-recovery provenance or a
/// trusted-internal durable-wait authorization.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
fn checkpoint_is_authorized(
    conn: &Connection,
    metadata: &RunMetadata,
    request: &ContinuationRequest,
    checkpoint: &Checkpoint,
) -> bool {
    let authorized_failed_checkpoint = metadata
        .failure_cleanup
        .as_ref()
        .filter(|failure| {
            metadata.is_cleanup_failure_abandonment() && failure.recovery_is_available()
        })
        .is_some_and(|failure| checkpoint_identity(checkpoint) == failure.failed_checkpoint_id)
        || authorizes_cleanup_resume(metadata, request, checkpoint);
    authorized_failed_checkpoint
        || ResumeAuthorization::for_resume(conn, request, checkpoint)
            .permits_non_safe_rerun(checkpoint)
}

fn check_safe_step(step_id: &str, force: bool, authorized: bool) -> SafetyCheck {
    if is_safe_rerun_step(step_id) || authorized {
        pass(
            "safe_step",
            format!("step {step_id} is explicitly authorized for this rerun"),
        )
    } else {
        let force_note = if force {
            "; --force cannot bypass rerun safety"
        } else {
            ""
        };
        fail(
            "safe_step",
            format!("step {step_id} is not safe to re-run{force_note}"),
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
    checks.push(check_resumable_status(&metadata, request));
    checks.push(check_identity_recoverable(&metadata));
    checks.push(check_workspace(&metadata));
    let selection = select_checkpoint(conn, request, &metadata);
    checks.push(check_checkpoint_exists(&selection));
    if let Ok(cp) = &selection {
        let authorized = checkpoint_is_authorized(conn, &metadata, request, cp);
        checks.push(check_safe_step(&cp.step_id, request.force, authorized));
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
    // Finding #2: When failure cleanup provenance records a
    // `failed_checkpoint_id`, select and verify it before falling back to
    // generic resume selection. This ensures incomplete cleanup failures
    // target the actual failed step rather than whatever happens to be the
    // newest resumable checkpoint.
    if let Some(cp) = select_failed_cleanup_checkpoint(conn, run_id, metadata)? {
        return Ok(cp);
    }
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

/// Select and verify the `failed_checkpoint_id` from failure-cleanup
/// provenance before generic resume/retry selection.
///
/// Returns `Ok(Some)` when a valid failure-cleanup record exists with a
/// `failed_checkpoint_id` that resolves to an actual persisted checkpoint.
/// Returns `Ok(None)` when there is no failure-cleanup provenance or the
/// `failed_checkpoint_id` does not resolve, allowing the caller to fall
/// back to its standard selection path.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
fn select_failed_cleanup_checkpoint(
    conn: &Connection,
    run_id: &str,
    metadata: &RunMetadata,
) -> Result<Option<Checkpoint>, ContinuationError> {
    let Some(failure) = metadata.failure_cleanup.as_ref() else {
        return Ok(None);
    };
    if failure.failed_checkpoint_id.is_empty() {
        return Ok(None);
    }
    // Verify the persisted checkpoint actually exists and matches the recorded
    // identity before returning it, so a stale or tampered failed_checkpoint_id
    // cannot select an arbitrary resume point.
    let cp = select_rewind_checkpoint(
        conn,
        run_id,
        &RewindTarget::ToCheckpoint(failure.failed_checkpoint_id.clone()),
    )?;
    if checkpoint_identity(&cp) == failure.failed_checkpoint_id {
        Ok(Some(cp))
    } else {
        Ok(None)
    }
}

fn select_retry_checkpoint(
    conn: &Connection,
    run_id: &str,
    metadata: &RunMetadata,
    from_failed_step: bool,
) -> Result<Checkpoint, ContinuationError> {
    // Finding #2: Prefer the verified failed_checkpoint_id from incomplete
    // cleanup provenance before generic retry selection. This applies to runs
    // where cleanup was attempted but did not fully succeed, ensuring the
    // retry targets the actual failure point.
    if let Some(cp) = select_failed_cleanup_checkpoint(conn, run_id, metadata)? {
        return Ok(cp);
    }
    if let Some(failure) = metadata.failure_cleanup.as_ref().filter(|failure| {
        metadata.is_cleanup_failure_abandonment() && failure.recovery_is_available()
    }) {
        return select_rewind_checkpoint(
            conn,
            run_id,
            &RewindTarget::ToCheckpoint(failure.failed_checkpoint_id.clone()),
        );
    }
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
///
/// `checkpoint_identity` must be the exact `step_id@rfc3339` identity bound at
/// plan time (see [`ContinuationPlan::checkpoint_identity`]). It is compared
/// against the freshly re-selected checkpoint inside the `IMMEDIATE`
/// transaction **before any lease or run mutation**, so a concurrent same-step
/// checkpoint replacement between plan and commit is rejected with no durable
/// state change.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn commit_continuation(
    conn: &Connection,
    request: &ContinuationRequest,
    checkpoint_identity: &str,
) -> Result<RunMetadata, ContinuationError> {
    crate::persistence::leases::init_leases_table(conn)?;
    // `conn` is intentionally not reused until `tx` commits or rolls back.
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    match commit_continuation_in_transaction(&tx, request, checkpoint_identity) {
        Ok(metadata) => {
            tx.commit()?;
            Ok(metadata)
        }
        Err(err) => match tx.rollback() {
            Ok(()) => Err(err),
            Err(rollback_err) => {
                tracing::warn!(
                    error = %err,
                    rollback_error = %rollback_err,
                    "rollback failed after continuation commit error"
                );
                Err(ContinuationError::Persistence(format!(
                    "rollback failed after continuation commit error: original={err}; rollback={rollback_err}"
                )))
            }
        },
    }
}

fn commit_continuation_in_transaction(
    tx: &Transaction<'_>,
    request: &ContinuationRequest,
    checkpoint_identity: &str,
) -> Result<RunMetadata, ContinuationError> {
    let mut metadata = get_run_with_conn(tx, &request.run_id)?
        .ok_or_else(|| ContinuationError::RunNotFound(request.run_id.clone()))?;
    ensure_reopen_claim_is_available(tx, &metadata, request)?;
    let selected = select_checkpoint(tx, request, &metadata)?;
    // TOCTOU defense: compare the exact checkpoint identity (step@timestamp)
    // bound at plan time against the freshly re-selected checkpoint inside the
    // IMMEDIATE transaction, BEFORE any lease or run mutation. A concurrent
    // same-step checkpoint replacement produces a different timestamp and must
    // be rejected here so no durable state is mutated.
    let current_identity = crate::engine::continuation::checkpoint_identity(&selected);
    if current_identity != checkpoint_identity {
        return Err(ContinuationError::InvalidTarget(format!(
            "continuation checkpoint identity changed before commit: expected {checkpoint_identity}, selected {current_identity}"
        )));
    }
    let authorized = checkpoint_is_authorized(tx, &metadata, request, &selected);
    let safety = check_safe_step(&selected.step_id, request.force, authorized);
    if !safety.passed {
        return Err(ContinuationError::InvalidTarget(safety.detail));
    }
    let resume_timestamp = set_resume_point(tx, &request.run_id, &selected.step_id)?;
    if let Some(failure) = metadata.failure_cleanup.as_mut() {
        if failure.failed_checkpoint_id == current_identity {
            failure.failed_checkpoint_id =
                format!("{}@{}", selected.step_id, resume_timestamp.to_rfc3339());
        }
    }
    reopen_run(tx, request, &selected.step_id, metadata)
}

fn reopen_run(
    conn: &Connection,
    request: &ContinuationRequest,
    step_id: &str,
    mut metadata: RunMetadata,
) -> Result<RunMetadata, ContinuationError> {
    let prior_status = metadata.status.clone();
    if metadata.is_cleanup_failure_abandonment() {
        if let Some(failure) = metadata.failure_cleanup.as_mut() {
            failure.recovery_consumed_at = Some(Utc::now());
        }
    }
    metadata.reopen();
    // Finding #4: When reopening from ReadyToResume (the daemon handoff
    // path), clear child PIDs from the prior lifecycle to avoid a live PID
    // handoff race. The prior run's child/agent processes are no longer
    // owned by this continuation; leaving them would let a concurrent daemon
    // launcher observe a live child PID and conclude the run is still being
    // actively processed, creating ambiguous ownership semantics.
    if prior_status == RunStatus::ReadyToResume {
        metadata.clear_child_pids();
    }
    metadata.set_current_step(step_id);
    persist_run_with_conn(conn, &metadata)?;
    let failure_identity = metadata
        .failure_cleanup
        .as_ref()
        .map_or("none", |failure| failure.failed_checkpoint_id.as_str());
    let detail = format!(
        "continuation={} force={} from_status={prior_status} resume_step={step_id} failure_checkpoint={failure_identity}",
        request.kind.verb(), request.force
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
    conn: &Connection,
    metadata: &RunMetadata,
    request: &ContinuationRequest,
) -> Result<(), ContinuationError> {
    // This runs inside the IMMEDIATE transaction after re-reading metadata, so
    // a second concurrent continuation attempt observes the first claim's PID
    // before deciding whether the Running record is still available.
    if !reopen_status_is_allowed(metadata, request) {
        return Err(ContinuationError::InvalidTarget(format!(
            "run {} status {} is not resumable; terminal states other than failed cannot be continued",
            request.run_id, metadata.status
        )));
    }
    acquire_continuation_lease(conn, metadata, request)?;
    if let Some(pid) = metadata
        .process_pid
        .filter(|_| metadata.status == RunStatus::Running)
        .filter(|pid| !crate::persistence::is_pid_stale(*pid))
    {
        return Err(ContinuationError::InvalidTarget(format!(
            "run {} is already running with live workflow PID {pid}",
            request.run_id
        )));
    }
    if let Some(pid) = metadata
        .child_pids
        .iter()
        .copied()
        .find(|pid| !crate::persistence::is_pid_stale(*pid))
    {
        return Err(ContinuationError::InvalidTarget(format!(
            "run {} still has live child PID {pid}",
            request.run_id
        )));
    }
    if metadata.is_cleanup_failure_abandonment() {
        if let Some(path_str) = metadata.workspace_path.as_ref() {
            let workspace_check = check_cleanup_workspace_ownership(Path::new(path_str), metadata);
            if !workspace_check.passed {
                return Err(ContinuationError::InvalidTarget(format!(
                    "run {}: {}",
                    request.run_id, workspace_check.detail
                )));
            }
        } else {
            return Err(ContinuationError::InvalidTarget(format!(
                "run {} preserved workspace is missing or invalid",
                request.run_id
            )));
        }
    }
    Ok(())
}

fn reopen_status_is_allowed(metadata: &RunMetadata, request: &ContinuationRequest) -> bool {
    metadata.status.is_resumable()
        || metadata.status == RunStatus::Running
        || (metadata.is_cleanup_failure_abandonment()
            && metadata
                .failure_cleanup
                .as_ref()
                .is_some_and(crate::persistence::FailureCleanupState::recovery_is_available)
            && request.force
            && matches!(
                request.kind,
                ContinuationKind::Retry { .. } | ContinuationKind::Rewind { .. }
            ))
}

fn running_claim_is_available(metadata: &RunMetadata) -> bool {
    metadata.status == RunStatus::Running
        && (metadata.is_process_stale() || metadata.process_pid.is_none())
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
fn acquire_continuation_lease(
    conn: &Connection,
    metadata: &RunMetadata,
    request: &ContinuationRequest,
) -> Result<(), ContinuationError> {
    let (Some(repository), Some(issue_number)) = (
        metadata.repository.as_deref(),
        metadata
            .issue_number
            .or(metadata.pr_number)
            .and_then(|number| u64::try_from(number).ok()),
    ) else {
        return Ok(());
    };
    let Some(lease) =
        crate::persistence::leases::get_lease_for_issue(conn, repository, issue_number)?
    else {
        return Ok(());
    };
    let expected_owner = lease.run_id.as_deref().ok_or_else(|| {
        ContinuationError::InvalidTarget(format!(
            "issue lease {} is active without a run owner",
            lease.lease_id
        ))
    })?;
    if expected_owner != request.run_id {
        return Err(ContinuationError::InvalidTarget(format!(
            "issue lease {} belongs to run {} rather than {}",
            lease.lease_id, expected_owner, request.run_id
        )));
    }
    let expected_statuses = [
        crate::persistence::LeaseStatus::Claimed,
        crate::persistence::LeaseStatus::Running,
        crate::persistence::LeaseStatus::WaitingExternal,
        crate::persistence::LeaseStatus::ReadyToResume,
        crate::persistence::LeaseStatus::Failed,
        crate::persistence::LeaseStatus::Abandoned,
        crate::persistence::LeaseStatus::CleanupAbandoned,
        crate::persistence::LeaseStatus::Stale,
    ];
    let target_status = if matches!(request.kind, ContinuationKind::Rewind { .. }) {
        crate::persistence::LeaseStatus::ReadyToResume
    } else {
        crate::persistence::LeaseStatus::Running
    };
    let acquired = crate::persistence::leases::update_lease_status_conditional(
        conn,
        &lease.lease_id,
        target_status,
        &expected_statuses,
        Some(&request.run_id),
        Some(&request.run_id),
    )?;
    if !acquired {
        return Err(ContinuationError::InvalidTarget(format!(
            "issue lease {} could not be acquired for continuation",
            lease.lease_id
        )));
    }
    Ok(())
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
    conn: &Connection,
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
        &validation_artifact(&validation),
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
        &selection_artifact(&checkpoint),
    )?;
    Ok(ContinuationPlan {
        validation,
        selected: Some(checkpoint),
        checkpoint_identity,
        artifact_dir,
    })
}

#[cfg(test)]
mod tests;
