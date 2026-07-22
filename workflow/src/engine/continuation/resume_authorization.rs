//! Typed resume authorization: reconstruct ephemeral
//! [`WorkspaceAuthorization`] from a freshly-verified workspace descriptor
//! before any lease CAS, marker promotion, or resumed step execution.
//!
//! ## Issue 158 slice 6 contract
//!
//! Every resume surface (daemon scheduler/launcher, child orchestration, CLI
//! runs resume/retry/rewind) must perform a **complete read-only** persisted
//! identity + ownership + current checkpoint authorization **before** any
//! durable mutation (lease CAS, marker promotion, run reopen). The
//! [`PreparedResume`] type is the result of that read-only authorization: it
//! holds the verified workspace identity and the ephemeral
//! [`WorkspaceAuthorization`] reconstructed from the **same** verified
//! descriptor, so the caller can inject it into [`RunContext`](crate::engine::runner::RunContext)
//! before constructing the resumed runner.
//!
//! The authorization is **ephemeral** (dev/inode): it is never persisted and
//! must be reconstructed on every resume from a freshly-verified workspace so
//! a TOCTOU swap of the workspace path between runs cannot replay a stale
//! authorization.
//!
//! @plan:PLAN-20260623-LUTHER-CONTINUATION

use std::path::{Path, PathBuf};

use crate::engine::workspace_ownership::{
    adjudicate_workspace_ownership, OwnershipVerdict, WorkspaceAuthorization,
};

/// A complete read-only resume authorization: persisted workspace identity +
/// ownership verified, and the ephemeral [`WorkspaceAuthorization`]
/// reconstructed from the same verified descriptor.
///
/// Constructed exclusively by [`prepare_resume_authorization`], which performs
/// **only read-only** operations (no lease CAS, no marker promotion, no run
/// mutation). The type is opaque outside this module so a caller cannot forge
/// an authorization: it is only produced after anchored ownership
/// adjudication succeeds.
///
/// Callers consume the authorization via [`Self::authorization`] and inject it
/// into [`RunContext`](crate::engine::runner::RunContext) (or
/// [`StepContext`](crate::engine::executor::StepContext)) before constructing
/// the resumed runner, so resumed shell steps retain descriptor-anchored
/// authorization without re-running the `workspace_ownership_verify` graph
/// step.
///
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
#[derive(Debug, Clone)]
pub struct PreparedResume {
    /// The canonical workspace path that was verified. Retained so the caller
    /// can confirm the resumed workspace matches the persisted identity
    /// without re-resolving the path (the authorization is already bound to
    /// this exact descriptor).
    workspace_path: PathBuf,
    /// The ephemeral dev/inode authorization reconstructed from the verified
    /// workspace descriptor. Never persisted.
    authorization: WorkspaceAuthorization,
}

impl PreparedResume {
    /// The ephemeral [`WorkspaceAuthorization`] reconstructed from the
    /// verified workspace descriptor. Inject this into
    /// [`RunContext`](crate::engine::runner::RunContext) before constructing
    /// the resumed runner.
    #[must_use]
    pub const fn authorization(&self) -> WorkspaceAuthorization {
        self.authorization
    }

    /// The canonical workspace path that was verified read-only.
    #[must_use]
    pub fn workspace_path(&self) -> &Path {
        &self.workspace_path
    }
}

/// Read-only error from [`prepare_resume_authorization`]. The error carries no
/// mutable state: all failures leave the lease, markers, and run record
/// unchanged.
///
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeAuthorizationError {
    /// The persisted workspace path is missing from run metadata.
    MissingWorkspacePath,
    /// The workspace path cannot be canonicalized (does not exist, permission
    /// denied, etc.).
    CannotCanonicalize { path: String, reason: String },
    /// Workspace ownership adjudication rejected the workspace (malformed,
    /// foreign, symlinked, or uninspectable marker).
    OwnershipRejected { reason: String },
    /// No ownership evidence exists for the workspace. A resume re-enters a
    /// workspace that a prior launch provisioned, so missing evidence is a
    /// corruption/divergence signal that fails closed.
    NoOwnershipEvidence,
}

impl std::fmt::Display for ResumeAuthorizationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingWorkspacePath => write!(
                f,
                "missing persisted workspace_path for resume authorization"
            ),
            Self::CannotCanonicalize { path, reason } => {
                write!(f, "cannot canonicalize workspace '{path}': {reason}")
            }
            Self::OwnershipRejected { reason } => {
                write!(f, "workspace ownership rejected: {reason}")
            }
            Self::NoOwnershipEvidence => {
                write!(f, "no workspace ownership evidence for resume")
            }
        }
    }
}

impl std::error::Error for ResumeAuthorizationError {}

/// Perform the complete read-only resume authorization: verify persisted
/// workspace ownership and reconstruct the ephemeral
/// [`WorkspaceAuthorization`] from the **same** verified descriptor.
///
/// This function performs **only read-only** operations:
/// - Canonicalizes the persisted workspace path.
/// - Adjudicates workspace ownership via the consolidated
///   [`adjudicate_workspace_ownership`] kernel (single descriptor, anchored
///   exact validation of bootstrap and durable markers).
/// - Extracts the [`WorkspaceAuthorization`] from the retained
///   [`crate::engine::workspace_ownership::VerifiedWorkspace`].
///
/// It **never** performs a lease CAS, marker promotion, or run mutation. On
/// any failure it returns [`ResumeAuthorizationError`] and leaves all durable
/// state unchanged. The caller must call this **before** any durable mutation.
///
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn prepare_resume_authorization(
    workspace_path: Option<&str>,
    run_id: &str,
) -> Result<PreparedResume, ResumeAuthorizationError> {
    let path_str = workspace_path.ok_or(ResumeAuthorizationError::MissingWorkspacePath)?;
    let path = Path::new(path_str);
    match adjudicate_workspace_ownership(path, run_id) {
        OwnershipVerdict::Owned(verified) => Ok(PreparedResume {
            workspace_path: path.canonicalize().map_err(|err| {
                ResumeAuthorizationError::CannotCanonicalize {
                    path: path_str.to_string(),
                    reason: err.to_string(),
                }
            })?,
            authorization: verified.authorization(),
        }),
        OwnershipVerdict::NoEvidence => Err(ResumeAuthorizationError::NoOwnershipEvidence),
        OwnershipVerdict::Rejected(reason) => {
            Err(ResumeAuthorizationError::OwnershipRejected { reason })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::workspace_ownership::provision_workspace_owner_marker;

    fn temp_workspace() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("create temp parent");
        let ws = dir.path().join("ws");
        (dir, ws)
    }

    #[test]
    fn prepared_resume_reconstructs_authorization_from_verified_workspace() {
        let (_dir, workspace) = temp_workspace();
        provision_workspace_owner_marker(&workspace, "run-resume-auth")
            .expect("provision bootstrap ownership");
        let prepared = prepare_resume_authorization(
            Some(workspace.to_str().expect("utf-8 path")),
            "run-resume-auth",
        )
        .expect("resume authorization succeeds for owned workspace");
        // The authorization is a non-zero dev/inode pair.
        let auth = prepared.authorization();
        assert!(
            auth.dev() != 0 || auth.ino() != 0,
            "authorization must be a real dev/inode pair"
        );
        assert_eq!(prepared.workspace_path(), workspace.canonicalize().unwrap());
    }

    #[test]
    fn prepared_resume_fails_closed_for_foreign_workspace() {
        let (_dir, workspace) = temp_workspace();
        provision_workspace_owner_marker(&workspace, "run-owner")
            .expect("provision bootstrap ownership");
        let err = prepare_resume_authorization(
            Some(workspace.to_str().expect("utf-8 path")),
            "run-foreign",
        )
        .expect_err("foreign run must fail closed");
        // A marker belonging to a different run is rejected (not no-evidence):
        // the marker is present and readable, but its owner run id does not
        // match the resuming run id.
        assert!(
            matches!(err, ResumeAuthorizationError::OwnershipRejected { .. }),
            "foreign run should be ownership-rejected, got {err:?}"
        );
    }

    #[test]
    fn prepared_resume_fails_closed_for_missing_workspace_path() {
        let err = prepare_resume_authorization(None, "run-x")
            .expect_err("missing workspace path must fail");
        assert_eq!(err, ResumeAuthorizationError::MissingWorkspacePath);
    }

    #[test]
    fn prepared_resume_fails_closed_for_nonexistent_workspace() {
        let err = prepare_resume_authorization(
            Some("/nonexistent/workspace/path/that/does/not/exist"),
            "run-x",
        )
        .expect_err("nonexistent workspace must fail");
        assert!(
            matches!(err, ResumeAuthorizationError::OwnershipRejected { .. }),
            "nonexistent workspace should be rejected by raw-path adjudication, got {err:?}"
        );
    }

    #[test]
    fn prepared_resume_fails_closed_for_unowned_workspace() {
        let dir = tempfile::tempdir().expect("create temp parent");
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        let err = prepare_resume_authorization(
            Some(workspace.to_str().expect("utf-8 path")),
            "run-unowned",
        )
        .expect_err("unowned workspace must fail");
        assert_eq!(err, ResumeAuthorizationError::NoOwnershipEvidence);
    }
}
