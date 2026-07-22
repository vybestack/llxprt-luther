//! Audited, recoverable legacy ownership migration for provenance-less,
//! marker-less pre-marker continuation rows.
//!
//! ## Issue 158 durable state machine
//!
//! Some legacy rows (created before workspace ownership markers existed,
//! including preserved issue 118 data) have no `launch_provenance` **and** no
//! workspace ownership marker. A normal resume of such a row is correctly
//! refused because [`prepare_resume_authorization`] requires existing
//! ownership evidence — resume must never create a first claim.
//!
//! This module provides a **narrowly scoped, explicit** migration that an
//! operator invokes via `runs migrate-legacy-ownership`. Because the filesystem
//! (marker publication) and the database cannot be updated atomically together,
//! the migration is implemented as a **durable state machine**:
//!
//! 1. **Persist intent** — before any filesystem mutation, a `pending` row is
//!    written to `legacy_ownership_migrations`. If the process crashes here,
//!    a retry/reconciliation observes `pending` and continues.
//! 2. **Publish marker** — the bootstrap `.luther/workspace-owner` marker is
//!    published via the crash-safe, descriptor-anchored pattern using a
//!    retained `WorkspaceAnchor`.
//! 3. **Verify via retained anchor** — the published marker is re-read through
//!    the *same* anchor descriptor (not a path re-open) to close the TOCTOU
//!    window.
//! 4. **Record completion** — the `legacy_ownership_migrations` row is updated
//!    to `completed` with a timestamp, a completion audit event is recorded in
//!    the events table, and a synthetic provenance tag
//!    ([`MigrationSource::LegacyOwnershipMigration`]) is written to the run's
//!    `launch_provenance` column.
//!
//! A retry/reconciliation that observes `completed` produces **no additional**
//! completion audit (exactly-once completion). An ordinary resume trusts the
//! migrated marker **only** when a durable `completed` row exists; a `pending`
//! row blocks the resume.
//!
//! ### Safety contract
//!
//! - The caller must supply the exact persisted `run_id` and `workspace_path`.
//! - The workspace must be a real directory (not a symlink), inspectable, and
//!   currently **unowned** by a foreign run (no foreign marker).
//! - The persisted run row must exist and have `None` `launch_provenance`
//!   (confirming it is a genuine legacy row, not a new record that lost its
//!   provenance).
//! - A foreign marker (belonging to a different run id) is refused without
//!   overwrite.
//!
//! @plan:PLAN-20260722-ISSUE158-LEGACY-OWNERSHIP-MIGRATION

use std::path::Path;

use chrono::Utc;

use crate::engine::workspace_ownership;
use crate::persistence::legacy_migration_state::{
    guarded_complete_migration_in_transaction, init_legacy_migration_table, load_migration_state,
    persist_migration_intent, GuardedCompletionOutcome, MigrationStateRow, MigrationStatus,
};
use crate::persistence::{
    append_typed_event_with_conn, checkpoint::EventType, get_run_with_conn, LaunchProvenance,
    MigrationSource,
};

/// Audit step id recorded for legacy ownership migration events.
const MIGRATION_AUDIT_STEP: &str = "legacy_ownership_migration";

/// The outcome of a legacy ownership migration attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyMigrationOutcome {
    /// The marker was published, the audit event was recorded, the durable
    /// completion row was written, and the provenance was tagged.
    Migrated,
    /// The migration was already durably completed; the retry produced no new
    /// completion audit (exactly-once).
    AlreadyCompleted,
    /// The marker was already present and owned by this run before migration;
    /// the durable completion was recorded.
    IdempotentlyCompleted,
}

/// Errors that prevent a legacy ownership migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyMigrationError {
    /// The persisted run row was not found.
    RunNotFound(String),
    /// The persisted row has `Some` launch provenance, so it is not a legacy
    /// row and must not be migrated.
    NotLegacyRow(String),
    /// The persisted workspace path is missing from run metadata.
    MissingWorkspacePath(String),
    /// The supplied workspace does not match the persisted workspace path.
    WorkspaceMismatch {
        run_id: String,
        supplied: String,
        persisted: String,
    },
    /// The workspace is not safe for migration: it is a symlink, uninspectable,
    /// or already carries a foreign marker.
    UnsafeWorkspace(String),
    /// The workspace already has ownership evidence belonging to a different
    /// run; migration is refused without overwrite.
    ForeignMarker(String),
    /// The durable migration state exists for a different workspace than the
    /// one supplied. This indicates a conflicting re-invocation.
    ConflictingIntent {
        run_id: String,
        expected: String,
        found: String,
    },
    /// The migration is durably pending (intent recorded but not completed) and
    /// the marker is missing; reconciliation attempted to publish but failed.
    PendingMarkerMissing(String),
    /// An I/O or database error occurred during publication or audit.
    Io(String),
}

impl std::fmt::Display for LegacyMigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RunNotFound(run_id) => {
                write!(f, "legacy migration: run '{run_id}' not found in registry")
            }
            Self::NotLegacyRow(run_id) => write!(
                f,
                "legacy migration: run '{run_id}' has launch provenance and is not a legacy row"
            ),
            Self::MissingWorkspacePath(run_id) => {
                write!(f, "legacy migration: run '{run_id}' has no persisted workspace_path")
            }
            Self::WorkspaceMismatch {
                run_id,
                supplied,
                persisted,
            } => write!(
                f,
                "legacy migration: run '{run_id}' supplied workspace '{supplied}' does not match persisted '{persisted}'"
            ),
            Self::UnsafeWorkspace(reason) => {
                write!(f, "legacy migration: unsafe workspace: {reason}")
            }
            Self::ForeignMarker(reason) => {
                write!(f, "legacy migration: foreign marker: {reason}")
            }
            Self::ConflictingIntent {
                run_id,
                expected,
                found,
            } => write!(
                f,
                "legacy migration: run '{run_id}' durable intent workspace '{found}' conflicts with supplied '{expected}'"
            ),
            Self::PendingMarkerMissing(reason) => {
                write!(f, "legacy migration: pending marker missing: {reason}")
            }
            Self::Io(reason) => write!(f, "legacy migration: {reason}"),
        }
    }
}

impl std::error::Error for LegacyMigrationError {}

impl From<std::io::Error> for LegacyMigrationError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

impl From<rusqlite::Error> for LegacyMigrationError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Io(err.to_string())
    }
}

/// Migrate a legacy provenance-less, marker-less row through the durable state
/// machine.
///
/// This is an **explicit operator operation**: it is never invoked automatically
/// by any resume path. The caller must have determined that the row is a
/// genuine legacy row (created before ownership markers existed) and that the
/// workspace is safe to claim.
///
/// # Arguments
///
/// - `conn` — the run-registry SQLite connection.
/// - `run_id` — the exact persisted run id to migrate.
/// - `workspace` — the exact persisted workspace path (validated to match).
/// - `config_root` — the exact canonical config root to record in launch
///   provenance. The workspace path is **never** placed in provenance; only
///   this canonical config root is encoded via
///   [`crate::persistence::encode_config_root`].
///
/// ## State machine flow
///
/// 1. Initialize the durable migration table (idempotent).
/// 2. Validate: run exists, no provenance, workspace matches persisted.
/// 3. Check durable state: if `completed`, return `AlreadyCompleted`
///    (exactly-once). If `pending`, reconcile.
/// 4. Persist intent (`pending`) if not already present.
/// 5. Validate workspace safety (real dir, not symlink, no foreign marker).
/// 6. Publish marker via retained `WorkspaceAnchor`.
/// 7. Verify via the same anchor descriptor.
/// 8. Record completion (single SQLite transaction): guarded
///    `pending → completed` transition, completion audit event, and
///    provenance tag commit atomically.
///
/// ## Idempotency / exactly-once completion
///
/// A crash between step 4 (intent) and step 8 (completion) leaves the row
/// `pending`. A retry observes `pending`, re-publishes the marker if missing,
/// and records exactly one completion audit. A retry after `completed` returns
/// `AlreadyCompleted` without producing another audit. The guarded conditional
/// `UPDATE` in step 8 ensures that, under concurrent connections, exactly one
/// writer performs the completion audit and provenance tag.
///
/// @plan:PLAN-20260722-ISSUE158-LEGACY-OWNERSHIP-MIGRATION
pub fn migrate_legacy_ownership(
    conn: &rusqlite::Connection,
    run_id: &str,
    workspace: &Path,
    config_root: &Path,
) -> Result<LegacyMigrationOutcome, LegacyMigrationError> {
    init_legacy_migration_table(conn)?;
    validate_legacy_row(conn, run_id)?;
    validate_workspace_match(conn, run_id, workspace)?;
    if migration_already_completed(conn, run_id) {
        return Ok(LegacyMigrationOutcome::AlreadyCompleted);
    }
    persist_migration_intent_if_needed(conn, run_id, workspace)?;
    validate_workspace_safety(workspace, run_id)?;
    let already_owned = publish_or_reconcile_migration_marker(workspace, run_id)?;
    let completion = record_completion(conn, run_id, config_root)?;
    interpret_migration_completion(already_owned, completion)
}

/// Load the existing durable migration state, persisting the durable intent
/// (`pending`) before any filesystem mutation. A conflicting durable intent
/// workspace is rejected. The caller must have already checked
/// [`migration_already_completed`] and returned `AlreadyCompleted`.
fn persist_migration_intent_if_needed(
    conn: &rusqlite::Connection,
    run_id: &str,
    workspace: &Path,
) -> Result<(), LegacyMigrationError> {
    let existing = load_migration_state(conn, run_id)?;
    // Persist (or preserve) the durable intent before any filesystem mutation.
    let workspace_str = workspace.to_string_lossy();
    if let Some(MigrationStateRow { workspace_path, .. }) = &existing {
        if workspace_path != workspace_str.as_ref() {
            return Err(LegacyMigrationError::ConflictingIntent {
                run_id: run_id.to_string(),
                expected: workspace_str.into_owned(),
                found: workspace_path.clone(),
            });
        }
    }
    persist_migration_intent(conn, run_id, &workspace_str, Utc::now())?;
    Ok(())
}

/// Returns whether the migration was already durably completed, so the caller
/// can skip all filesystem and completion work (exactly-once).
fn migration_already_completed(conn: &rusqlite::Connection, run_id: &str) -> bool {
    matches!(
        load_migration_state(conn, run_id),
        Ok(Some(MigrationStateRow {
            status: MigrationStatus::Completed,
            ..
        }))
    )
}

/// Publish the bootstrap ownership marker via a retained anchor, or reconcile
/// when the marker already belongs to this run (from a prior partial run that
/// published but crashed before completion). Returns whether the marker was
/// already owned (idempotent reconciliation) before this call.
fn publish_or_reconcile_migration_marker(
    workspace: &Path,
    run_id: &str,
) -> Result<bool, LegacyMigrationError> {
    // Open the anchor ONCE and retain it through publication + verification so
    // the marker cannot be redirected by a concurrent path rename.
    let canonical = workspace.canonicalize().map_err(|err| {
        LegacyMigrationError::UnsafeWorkspace(format!("cannot canonicalize: {err}"))
    })?;
    let anchor = workspace_ownership::WorkspaceAnchor::open(&canonical)?;

    // Reconciliation: if the marker already belongs to this run (from a prior
    // partial run that published but crashed before completion), finish the
    // completion without re-publishing.
    let already_owned = matches!(
        workspace_ownership::snapshot_bootstrap_marker_via_anchor(&anchor, run_id),
        workspace_ownership::AnchoredMarkerVerdict::Trusted
    );

    if !already_owned {
        workspace_ownership::publish_bootstrap_via_anchor(&anchor, run_id)?;
        verify_published_marker(&anchor, run_id)?;
    }
    Ok(already_owned)
}

/// Re-read the published marker through the retained anchor descriptor and
/// fail closed when it is absent or rejected (closing the TOCTOU window).
fn verify_published_marker(
    anchor: &workspace_ownership::WorkspaceAnchor,
    run_id: &str,
) -> Result<(), LegacyMigrationError> {
    match workspace_ownership::snapshot_bootstrap_marker_via_anchor(anchor, run_id) {
        workspace_ownership::AnchoredMarkerVerdict::Trusted => Ok(()),
        workspace_ownership::AnchoredMarkerVerdict::Absent => Err(LegacyMigrationError::Io(
            "ownership evidence missing after legacy migration publication".to_string(),
        )),
        workspace_ownership::AnchoredMarkerVerdict::Rejected(reason) => {
            Err(LegacyMigrationError::Io(reason))
        }
    }
}

/// Interpret the guarded completion outcome, mapping the durable transition
/// result to the caller-facing outcome. A `Missing` outcome is an integrity
/// failure because the intent was persisted earlier in this connection.
fn interpret_migration_completion(
    already_owned: bool,
    completion: GuardedCompletionOutcome,
) -> Result<LegacyMigrationOutcome, LegacyMigrationError> {
    match (already_owned, completion) {
        (_, GuardedCompletionOutcome::AlreadyCompleted) => {
            Ok(LegacyMigrationOutcome::AlreadyCompleted)
        }
        (true, GuardedCompletionOutcome::Transitioned) => {
            Ok(LegacyMigrationOutcome::IdempotentlyCompleted)
        }
        (false, GuardedCompletionOutcome::Transitioned) => Ok(LegacyMigrationOutcome::Migrated),
        // The intent was persisted above, so a Missing outcome here is an
        // integrity failure: the guarded UPDATE cannot miss a row we just
        // wrote in this connection.
        (_, GuardedCompletionOutcome::Missing) => Err(LegacyMigrationError::Io(
            "guarded completion reported missing row after intent was persisted".to_string(),
        )),
    }
}

/// Validate that the persisted row is a genuine legacy row: it exists and has
/// no launch provenance, OR has a synthetic migration-source provenance tag
/// (from a prior migration of the same row).
fn validate_legacy_row(
    conn: &rusqlite::Connection,
    run_id: &str,
) -> Result<(), LegacyMigrationError> {
    let metadata = get_run_with_conn(conn, run_id)?
        .ok_or_else(|| LegacyMigrationError::RunNotFound(run_id.to_string()))?;
    match &metadata.launch_provenance {
        None => Ok(()),
        Some(provenance) if provenance.is_migrated() => Ok(()),
        Some(_) => Err(LegacyMigrationError::NotLegacyRow(run_id.to_string())),
    }
}

/// Validate that the supplied workspace matches the persisted workspace path
/// exactly.
fn validate_workspace_match(
    conn: &rusqlite::Connection,
    run_id: &str,
    workspace: &Path,
) -> Result<(), LegacyMigrationError> {
    let metadata = get_run_with_conn(conn, run_id)?
        .ok_or_else(|| LegacyMigrationError::RunNotFound(run_id.to_string()))?;
    let persisted = metadata
        .workspace_path
        .as_deref()
        .ok_or_else(|| LegacyMigrationError::MissingWorkspacePath(run_id.to_string()))?;
    let supplied = workspace.to_string_lossy();
    if supplied != persisted {
        return Err(LegacyMigrationError::WorkspaceMismatch {
            run_id: run_id.to_string(),
            supplied: supplied.into_owned(),
            persisted: persisted.to_string(),
        });
    }
    Ok(())
}

/// Validate workspace safety: real directory, not symlink, inspectable, and
/// currently unowned or already owned by the same run id (idempotent).
fn validate_workspace_safety(workspace: &Path, run_id: &str) -> Result<(), LegacyMigrationError> {
    if let Some(reason) =
        crate::engine::continuation::workspace_marker::reject_symlinked_workspace_root(workspace)
    {
        return Err(LegacyMigrationError::UnsafeWorkspace(reason));
    }
    let canonical = workspace.canonicalize().map_err(|err| {
        LegacyMigrationError::UnsafeWorkspace(format!("cannot canonicalize: {err}"))
    })?;
    let meta = std::fs::symlink_metadata(&canonical)
        .map_err(|err| LegacyMigrationError::UnsafeWorkspace(format!("cannot inspect: {err}")))?;
    if !meta.is_dir() {
        return Err(LegacyMigrationError::UnsafeWorkspace(
            "workspace path is not a directory".to_string(),
        ));
    }
    match workspace_ownership::adjudicate_workspace_ownership(&canonical, run_id) {
        workspace_ownership::OwnershipVerdict::Owned(_) => Ok(()),
        workspace_ownership::OwnershipVerdict::NoEvidence => Ok(()),
        workspace_ownership::OwnershipVerdict::Rejected(reason) => {
            Err(LegacyMigrationError::ForeignMarker(reason))
        }
    }
}

/// Record the durable completion as a single atomic SQLite transaction: the
/// guarded `pending → completed` transition, the completion audit event, and
/// the provenance tag all commit together. The guarded conditional `UPDATE`
/// ensures exactly-once completion across concurrent connections.
///
/// The caller supplies the canonical `config_root` to record in provenance.
/// The workspace path is never placed in provenance.
fn record_completion(
    conn: &rusqlite::Connection,
    run_id: &str,
    config_root: &Path,
) -> Result<GuardedCompletionOutcome, LegacyMigrationError> {
    let now = Utc::now();
    let tx = conn.unchecked_transaction()?;
    let outcome = guarded_complete_migration_in_transaction(&tx, run_id, now)?;
    // Only record the completion audit and provenance tag when this writer
    // performed the transition. A concurrent writer that already completed
    // has already recorded exactly one audit; this writer records none.
    if matches!(outcome, GuardedCompletionOutcome::Transitioned) {
        let metadata = serde_json::json!({
            "operation": "legacy_ownership_migration",
            "detail": "completed",
            "run_id": run_id,
            "completed_at": now.to_rfc3339(),
        });
        append_typed_event_with_conn(
            &tx,
            run_id,
            MIGRATION_AUDIT_STEP,
            "completed",
            EventType::TerminalState,
            Some(&metadata.to_string()),
            now,
        )
        .map_err(|e| LegacyMigrationError::Io(e.to_string()))?;
        tag_migrated_provenance(&tx, run_id, config_root)?;
    }
    tx.commit()?;
    Ok(outcome)
}

/// Write a synthetic [`MigrationSource::LegacyOwnershipMigration`] provenance
/// tag to the run's `launch_provenance` column, using the supplied canonical
/// `config_root`. The workspace path is never encoded in provenance.
///
/// After this, the row is explicitly tagged as schema-migrated. Post-upgrade,
/// a NULL provenance is denied because it indicates a row that was never
/// migrated (genuine legacy or a bug), not a migrated row.
fn tag_migrated_provenance(
    conn: &rusqlite::Connection,
    run_id: &str,
    config_root: &Path,
) -> Result<(), LegacyMigrationError> {
    let metadata = get_run_with_conn(conn, run_id)?
        .ok_or_else(|| LegacyMigrationError::RunNotFound(run_id.to_string()))?;
    if metadata.launch_provenance.is_some() {
        return Ok(());
    }
    let canonical_root = crate::persistence::encode_config_root(config_root);
    let provenance =
        LaunchProvenance::migrated(MigrationSource::LegacyOwnershipMigration, canonical_root);
    let json = serde_json::to_string(&provenance)
        .map_err(|e| LegacyMigrationError::Io(format!("serialize provenance: {e}")))?;
    conn.execute(
        "UPDATE runs SET launch_provenance = ?1 WHERE run_id = ?2 AND launch_provenance IS NULL",
        rusqlite::params![json, run_id],
    )?;
    Ok(())
}
