//! Read-only resume preparation for daemon lease dispatch.
//!
//! [`prepare_resume_lease`] performs **every** fallible read-only check
//! available from persisted metadata before the `ReadyToResume → Running` CAS.
//! On success it returns a [`PreparedResume`] carrying all validated data; on
//! any validation failure it returns a [`SkipReason`] **without mutation**.
//!
//! The checks, in order:
//! 1. `run_id` present on the lease.
//! 2. Claim receipt present.
//! 3. Run metadata present and non-empty workflow type.
//! 4. Ownership-denied terminal guard (non-resumable).
//! 5. Workspace ownership verification (persisted marker).
//! 6. Launch provenance verification (recompute digests from the persisted
//!    canonical config root).
//! 7. Continuation authorization (current_step / checkpoint / safe-step).
//!
//! Only after all seven checks pass does the CAS acquire the lease.
//!
//! @plan:PLAN-20260722-ISSUE158-RESUME-PREPARATION

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::daemon::discovery::SkipReason;
use crate::persistence::claim_metadata::{get_claim_metadata, ClaimMetadataReceipt};
use crate::persistence::launch_provenance::{
    verify_provenance, LegacyAllowed, ProvenanceVerification,
};
use crate::persistence::leases::{
    update_lease_status_conditional_outcome, ConditionalLeaseStatusOutcome, IssueLease, LeaseStatus,
};
use crate::persistence::{get_run_with_conn, RunMetadata};
use crate::workflow::config_loader::{resolve_workflow_config, resolve_workflow_type};

use super::{ClaimedLaunch, LaunchRequest};

/// The canonical config root used by daemon-managed workflows.
///
/// Daemon launches and resumes always resolve from this root; CLI `runs resume
/// --config-dir` covers temporary per-run config roots. Centralising it here
/// keeps the resume-preparation provenance verification in lock-step with the
/// launch path.
const DAEMON_CONFIG_ROOT: &str = "config";

/// Typed prepared resume data: all validated values needed to construct a
/// [`ClaimedLaunch`] after the lease CAS succeeds.
///
/// Every field is loaded and validated **before** the CAS so no fallible read
/// remains after the lease is acquired. The `resume_daemon_workflow` surface
/// consumes and revalidates these values.
///
/// @plan:PLAN-20260722-ISSUE158-RESUME-PREPARATION
#[derive(Debug, Clone)]
pub struct PreparedResume {
    pub run_id: String,
    pub workflow_type_id: String,
    pub config_id: String,
    pub config_root: PathBuf,
    pub claim_assignment_added: bool,
    pub claim_label_added: bool,
}

impl PreparedResume {
    /// Build the [`ClaimedLaunch`] request from the prepared data.
    ///
    /// Called only after the CAS has transitioned the lease to `Running`, so
    /// no fallible operation remains.
    pub fn into_claimed_launch(self, lease: &IssueLease) -> ClaimedLaunch {
        ClaimedLaunch {
            lease_id: lease.lease_id.clone(),
            request: LaunchRequest {
                config_id: lease.config_id.clone(),
                workflow_type_id: Some(self.workflow_type_id),
                run_id: self.run_id,
                repo: lease.issue_repo.clone(),
                issue_number: lease.issue_number,
                daemon_managed_claim: true,
                claim_assignment_added: self.claim_assignment_added,
                claim_label_added: self.claim_label_added,
                // Resumes reuse persisted RunMetadata paths; do not synthesize
                // new per-run paths for a resumed run. @plan:issue-117
                work_dir: None,
                artifact_dir: None,
                // Flow the persisted canonical config root (decoded from the
                // launch provenance) through to the resume so the workflow is
                // re-resolved from exactly the same root.
                // @plan:PLAN-20260722-ISSUE158-LAUNCH-PROVENANCE
                config_root: self.config_root,
            },
        }
    }
}

/// Prepare a ready-to-resume lease for dispatch by validating durable state
/// before acquiring ownership.
///
/// All fallible reads are performed and validated **before** the conditional
/// lease acquisition. Once the CAS transitions the lease to `Running`, no
/// fallible operation remains — the [`PreparedResume`] carries all validated
/// values. This eliminates the transaction-blocker window where a
/// post-acquisition read failure would strand the lease in `Running` without
/// compensation.
///
/// The CAS acquires only when the lease is exactly `ReadyToResume` **and**
/// owned by the expected `run_id`, so a concurrent writer that reassigned the
/// lease cannot be overwritten by a stale preparation.
///
/// **No durable mutation before all read-only resume checks.** A missing
/// `run_id` or any validation failure skips without mutating the lease (the
/// lease is left in `ReadyToResume`).
///
/// @plan:PLAN-20260722-ISSUE158-RESUME-PREPARATION
pub fn prepare_resume_lease(
    lease: &IssueLease,
    conn: &Connection,
) -> Result<Result<PreparedResume, SkipReason>, rusqlite::Error> {
    let Some(prepared) = validate_resume_read_only(lease, conn)? else {
        return Ok(Err(SkipReason::InvalidLeaseState));
    };
    if !acquire_resume_lease(conn, &lease.lease_id, &prepared.run_id)? {
        return Ok(Err(SkipReason::InvalidLeaseState));
    }
    Ok(Ok(prepared))
}

/// Run every read-only validation check and return the prepared data, or
/// `None` to skip the resume without mutation.
///
/// This is the pre-CAS validation phase. No write occurs here; a `None` return
/// leaves the lease untouched.
fn validate_resume_read_only(
    lease: &IssueLease,
    conn: &Connection,
) -> Result<Option<PreparedResume>, rusqlite::Error> {
    let Some(run_id) = resolve_resume_run_id(lease) else {
        return Ok(None);
    };
    let Some(receipt) = load_resume_claim_receipt(conn, &lease.lease_id)? else {
        return Ok(None);
    };
    let Some(metadata) = load_resume_metadata(conn, &run_id)? else {
        return Ok(None);
    };
    let Some(workflow_type_id) = validate_resume_workflow_type(&metadata) else {
        return Ok(None);
    };
    if lease.config_id != metadata.config_id
        || metadata.repository.as_deref() != Some(lease.issue_repo.as_str())
        || metadata.issue_lease_number() != Some(lease.issue_number)
    {
        return Ok(None);
    }
    if !verify_not_ownership_denied_terminal(&metadata) {
        return Ok(None);
    }
    if !verify_no_pending_legacy_migration(conn, &run_id)? {
        return Ok(None);
    }
    if !verify_resume_workspace_ownership(&metadata, &run_id) {
        return Ok(None);
    }
    let config_root = match verify_resume_provenance(&metadata, &receipt) {
        Some(root) => root,
        None => return Ok(None),
    };
    if !verify_resume_continuation_authorization(conn, &metadata, &run_id)? {
        return Ok(None);
    }
    Ok(Some(PreparedResume {
        run_id,
        workflow_type_id,
        config_id: metadata.config_id.clone(),
        config_root,
        claim_assignment_added: receipt.assignment_added,
        claim_label_added: receipt.label_added,
    }))
}

/// Resolve the `run_id` from the lease, returning `None` to skip without
/// mutation when it is absent.
fn resolve_resume_run_id(lease: &IssueLease) -> Option<String> {
    lease.run_id.clone()
}

/// Load and validate the claim receipt before any state mutation.
fn load_resume_claim_receipt(
    conn: &Connection,
    lease_id: &str,
) -> Result<Option<ClaimMetadataReceipt>, rusqlite::Error> {
    get_claim_metadata(conn, lease_id)
}

/// Load the run metadata and validate it before the CAS.
fn load_resume_metadata(
    conn: &Connection,
    run_id: &str,
) -> Result<Option<RunMetadata>, rusqlite::Error> {
    get_run_with_conn(conn, run_id)
}

/// Validate that the workflow type id is present and return it, or `None` to
/// skip.
fn validate_resume_workflow_type(metadata: &RunMetadata) -> Option<String> {
    if metadata.workflow_type_id.is_empty() {
        None
    } else {
        Some(metadata.workflow_type_id.clone())
    }
}

/// Continuation identity guard: an ownership-denied terminal is non-resumable
/// and must never be acquired for resume. Returns `false` to skip without
/// mutation.
fn verify_not_ownership_denied_terminal(metadata: &RunMetadata) -> bool {
    !metadata.is_ownership_denied_terminal()
}

/// Reject a resume while a legacy ownership migration is durably pending. A
/// pending migration row signals an incomplete migration that may have
/// published the marker without recording the completion audit; the resume
/// trust contract requires a durable `completed` migration row before the
/// migrated marker is trusted. Returns `false` to skip without mutation.
///
/// @plan:PLAN-20260722-ISSUE158-LEGACY-OWNERSHIP-MIGRATION
fn verify_no_pending_legacy_migration(
    conn: &Connection,
    run_id: &str,
) -> Result<bool, rusqlite::Error> {
    Ok(!crate::persistence::migration_is_pending(conn, run_id))
}

/// Verify persisted workspace ownership before the CAS. Returns `false` to
/// skip without mutation when the workspace is unowned, foreign, or carries a
/// malformed marker.
fn verify_resume_workspace_ownership(metadata: &RunMetadata, run_id: &str) -> bool {
    let Some(workspace) = metadata.workspace_path.as_deref() else {
        return false;
    };
    crate::engine::workspace_ownership::verify_workspace_ownership(Path::new(workspace), run_id)
        .is_none()
}

/// Reconstruct the launch-equivalent effective config and verify provenance
/// against the persisted launch provenance, refusing on mismatch before any
/// mutation.
///
/// Resolves the workflow type and config from the persisted canonical config
/// root, applies the persisted continuation overrides so the recomputed digest
/// matches the launch-equivalent effective config, and verifies the digests.
/// A `None` persisted provenance is admitted under the explicit
/// [`LegacyAllowed::Allowed`] policy with a warning (printed to stderr); a
/// `Mismatch` refuses the resume without mutation.
///
/// @plan:PLAN-20260722-ISSUE158-LAUNCH-PROVENANCE
fn verify_resume_provenance(
    metadata: &RunMetadata,
    receipt: &ClaimMetadataReceipt,
) -> Option<PathBuf> {
    if metadata.launch_provenance.is_none() {
        eprintln!(
            "Warning: launch provenance absent (legacy row); admitting resume under explicit LegacyAllowed policy"
        );
        return Some(PathBuf::from(DAEMON_CONFIG_ROOT));
    }
    let config_root = match metadata.launch_provenance.as_ref() {
        Some(provenance) => {
            match crate::persistence::decode_config_root(&provenance.canonical_config_root) {
                Ok(root) => root,
                Err(error) => {
                    eprintln!("Warning: persisted config root is corrupt: {error}");
                    return None;
                }
            }
        }
        None => PathBuf::from(DAEMON_CONFIG_ROOT),
    };
    let Ok(workflow_type) = resolve_workflow_type(&metadata.workflow_type_id, &config_root) else {
        return None;
    };
    let Ok(mut config) = resolve_workflow_config(&metadata.config_id, &config_root) else {
        return None;
    };
    let overrides = crate::engine::continuation::continuation_overrides(metadata);
    if crate::workflow::target_profile::apply_target_profile_overrides(&mut config, &overrides)
        .is_err()
    {
        return None;
    }
    for (key, value) in [
        ("daemon_managed_claim", true),
        ("claim_assignment_added", receipt.assignment_added),
        ("claim_label_added", receipt.label_added),
    ] {
        config.variables.insert(key.to_owned(), value.to_string());
    }
    match verify_provenance(
        &metadata.launch_provenance,
        &workflow_type,
        &config,
        &config_root,
        LegacyAllowed::Allowed,
    ) {
        ProvenanceVerification::Match => Some(config_root),
        ProvenanceVerification::Legacy(warning) => {
            eprintln!("Warning: {warning}");
            Some(config_root)
        }
        ProvenanceVerification::Mismatch(_) => None,
    }
}

/// Validate current_step / checkpoint / continuation authorization before the
/// CAS.
///
/// This delegates to the full continuation validation suite (run exists,
/// resumable status, identity recoverable, workspace, checkpoint exists,
/// safe-step) so a resume that would fail continuation authorization is
/// rejected before any lease mutation. Returns `false` to skip without
/// mutation on failure.
fn verify_resume_continuation_authorization(
    conn: &Connection,
    metadata: &RunMetadata,
    run_id: &str,
) -> Result<bool, rusqlite::Error> {
    if metadata
        .current_step
        .as_deref()
        .unwrap_or_default()
        .is_empty()
    {
        return Ok(false);
    }
    let request = crate::engine::ContinuationRequest {
        run_id: run_id.to_string(),
        kind: crate::engine::ContinuationKind::Resume,
        force: true,
        trusted_internal: true,
    };
    let validation = crate::engine::continuation::validate_continuation(conn, &request)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
    Ok(validation.ok)
}

/// Acquire exact `ReadyToResume` ownership via conditional update.
///
/// Returns `true` when the CAS applied, `false` when a concurrent writer
/// already advanced the lease (stale CAS). The expected_run_id guard rejects
/// a stale writer whose run_id was superseded by a concurrent reclaim,
/// preserving the durable ReadyToResume state.
fn acquire_resume_lease(
    conn: &Connection,
    lease_id: &str,
    run_id: &str,
) -> Result<bool, rusqlite::Error> {
    let acquired = update_lease_status_conditional_outcome(
        conn,
        lease_id,
        LeaseStatus::Running,
        &[LeaseStatus::ReadyToResume],
        Some(run_id),
        Some(run_id),
    )?;
    Ok(matches!(acquired, ConditionalLeaseStatusOutcome::Applied))
}
