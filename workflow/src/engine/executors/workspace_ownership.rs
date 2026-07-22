//! `workspace_ownership` step executor: promote bootstrap ownership evidence
//! to durable evidence.
//!
//! This is a first-class graph step placed immediately after `setup_workspace`
//! and before any agent (`llxprt`) step. `setup_workspace` provisions the
//! bootstrap `.luther/workspace-owner` marker before Git initialization and
//! then runs `git init`. After `.git` exists, this step promotes the *exact*
//! bootstrap content to the durable `.git/luther/workspace-owner` marker using
//! the cohesive workspace-ownership abstraction's crash-safe exact-byte
//! publication pattern.
//!
//! ## Ownership enforcement contract (issue 158)
//!
//! The step enforces both bootstrap and durable markers consistently:
//!
//! - **Daemon-managed claim (`daemon_managed_claim = true`)**: at least one
//!   exact owner marker (bootstrap or durable) must exist. Any present marker
//!   must be a real regular file matching this run id. Missing evidence is
//!   fatal. After promotion, durable-only evidence must work.
//! - **Non-daemon (e.g. CLI runs)**: no evidence is allowed — the step is a
//!   success no-op so a manually-managed workspace without Luther ownership
//!   metadata can still proceed. Any *present* evidence is still validated and
//!   malformed evidence remains fatal.
//! - **Malformed evidence** (symlink, directory, empty, foreign owner,
//!   non-regular file): always fatal, regardless of daemon status.
//!
//! The step is idempotent: re-running it for an already-promoted same-owner
//! workspace succeeds without rewriting the durable marker.
//!
//! @plan:PLAN-20260623-LUTHER-CONTINUATION

use crate::engine::executor::{StepContext, StepExecutor};
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;
use crate::engine::workspace_ownership::{
    adjudicate_workspace_ownership, ensure_durable_workspace_ownership, verify_workspace_ownership,
    workspace_ownership_evidence_exists, OwnershipVerdict,
};

/// Step id used in error messages emitted by this executor.
const STEP_ID: &str = "workspace_ownership";

/// Workspace ownership promotion executor.
///
/// Promotes verified bootstrap ownership evidence to the durable path after
/// Git initialization. Read-only verification delegates to the cohesive
/// workspace-ownership abstraction so the two-phase contract is enforced in one
/// place. See the module docs for the ownership enforcement contract.
#[derive(Debug, Clone, Copy, Default)]
pub struct WorkspaceOwnershipExecutor;

impl StepExecutor for WorkspaceOwnershipExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        _params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        let work_dir = context.work_dir();
        let run_id = context.run_id();
        let daemon_managed = context.daemon_managed_claim();
        let authorization = context.workspace_authorization().copied();
        let evidence_exists = workspace_ownership_evidence_exists(work_dir);
        if !evidence_exists {
            // No ownership evidence at all. A daemon-managed workspace must
            // carry at least one exact owner marker; missing evidence is fatal.
            // A non-daemon (e.g. CLI) workspace without Luther ownership
            // metadata is allowed to proceed as a no-op success.
            if daemon_managed {
                return Err(ownership_fatal(
                    "workspace ownership evidence is missing for daemon-managed run",
                ));
            }
            context.set("workspace_ownership_skipped", "true");
            return Ok(StepOutcome::Success);
        }
        // Evidence exists: verify it is valid (read-only, fail-closed). This
        // rejects malformed, symlinked, non-regular, or foreign markers.
        if let Some(reason) = verify_workspace_ownership(work_dir, run_id) {
            return Err(ownership_fatal(&reason));
        }
        if let Some(authorization) = authorization {
            let verified = match crate::engine::workspace_ownership::adjudicate_workspace_ownership(
                work_dir, run_id,
            ) {
                crate::engine::workspace_ownership::OwnershipVerdict::Owned(verified) => verified,
                crate::engine::workspace_ownership::OwnershipVerdict::NoEvidence => {
                    return Err(ownership_fatal("workspace ownership evidence disappeared"));
                }
                crate::engine::workspace_ownership::OwnershipVerdict::Rejected(reason) => {
                    return Err(ownership_fatal(&reason));
                }
            };
            if verified.authorization() != authorization {
                return Err(ownership_fatal(
                    "workspace identity changed after ownership verification",
                ));
            }
            verified.promote(run_id).map_err(|err| {
                ownership_fatal(&format!("failed to promote workspace ownership: {err}"))
            })?;
        } else {
            ensure_durable_workspace_ownership(work_dir, run_id).map_err(|err| {
                ownership_fatal(&format!("failed to promote workspace ownership: {err}"))
            })?;
        }
        // Re-verify after promotion so a partial/tampered promotion still fails
        // closed rather than silently trusting the durable record.
        if let Some(reason) = verify_workspace_ownership(work_dir, run_id) {
            return Err(ownership_fatal(&format!(
                "workspace ownership verification failed after promotion: {reason}"
            )));
        }
        context.set("workspace_ownership_promoted", "true");
        Ok(StepOutcome::Success)
    }
}

/// Build a fatal step-execution error for the `workspace_ownership` step.
fn ownership_fatal(detail: &str) -> EngineError {
    EngineError::StepExecutionError {
        step_id: STEP_ID.to_string(),
        message: detail.to_string(),
    }
}

/// Step id used in error messages emitted by the pre-setup verifier.
const VERIFY_STEP_ID: &str = "workspace_ownership_verify";

/// Read-only pre-setup workspace ownership verification executor.
///
/// This is a first-class graph step placed **before** `setup_workspace` so that
/// the shell never has to infer ownership absence and cannot mutate the
/// workspace before typed Rust validation runs. It performs **only** read-only
/// verification via the cohesive `verify_workspace_ownership` abstraction; it
/// never provisions, promotes, or writes any marker.
///
/// ## Verification contract (issue 158)
///
/// The pre-setup verifier enforces the same fail-closed contract as the
/// post-setup promotion step, but without any write path:
///
/// - **Daemon-managed claim (`daemon_managed_claim = true`)**: requires exact
///   ownership evidence. A daemon-managed workspace must carry at least one
///   exact owner marker; missing evidence is fatal regardless of whether this
///   is a fresh launch or a re-entry. The fail-closed posture is mandatory for
///   daemon-managed claims: the daemon never silently proceeds on a workspace
///   it does not own, because a fresh daemon launch provisions bootstrap
///   evidence before this step runs (via `provision_workspace_ownership` in
///   the daemon launch path), so reaching this step with no evidence on a
///   daemon-managed run is itself a corruption/divergence signal that must
///   fail closed.
/// - **Non-daemon (e.g. CLI runs)**: no evidence is allowed → success no-op.
///   A non-daemon workspace without Luther ownership metadata can proceed.
/// - **Malformed evidence** (symlink, directory, empty, foreign owner,
///   non-regular file): always fatal, regardless of daemon status or evidence
///   presence. This is the key invariant: the shell can never mutate before
///   typed Rust validation rejects malformed evidence.
/// - **Uninspectable evidence** (e.g. `PermissionDenied`): always fatal.
///
/// The post-setup `workspace_ownership` step (promotion) remains in the graph
/// and runs after `setup_workspace` provisions bootstrap evidence.
#[derive(Debug, Clone, Copy, Default)]
pub struct WorkspaceOwnershipVerifyExecutor;

impl StepExecutor for WorkspaceOwnershipVerifyExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        _params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        let work_dir = context.work_dir();
        let run_id = context.run_id();
        let daemon_managed = context.daemon_managed_claim();
        match adjudicate_workspace_ownership(work_dir, run_id) {
            OwnershipVerdict::NoEvidence => handle_no_evidence(context, daemon_managed),
            OwnershipVerdict::Rejected(reason) => Err(verify_fatal(&reason)),
            OwnershipVerdict::Owned(verified) => {
                context.set_workspace_authorization(verified.authorization());
                context.set("workspace_ownership_verified", "true");
                Ok(StepOutcome::Success)
            }
        }
    }
}

/// Decide the outcome when no ownership evidence exists. A daemon-managed
/// workspace must carry at least one exact owner marker; missing evidence is
/// fatal. A non-daemon (e.g. CLI) workspace without Luther ownership metadata
/// is allowed to proceed as a no-op success.
fn handle_no_evidence(
    context: &mut StepContext,
    daemon_managed: bool,
) -> Result<StepOutcome, EngineError> {
    if daemon_managed {
        return Err(verify_fatal(
            "workspace ownership evidence is missing for daemon-managed run",
        ));
    }
    context.set("workspace_ownership_verify_no_evidence", "true");
    Ok(StepOutcome::Success)
}

/// Build a fatal step-execution error for the pre-setup verification step.
fn verify_fatal(detail: &str) -> EngineError {
    EngineError::StepExecutionError {
        step_id: VERIFY_STEP_ID.to_string(),
        message: detail.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::executor::StepContext;
    use crate::engine::runner::RunContext;
    use std::path::{Path, PathBuf};

    /// Non-daemon context (default, `daemon_managed = false`).
    fn ctx(workspace: &Path) -> StepContext {
        ctx_with_daemon(workspace, "run-A", false)
    }

    /// Daemon-managed context (`daemon_managed = true`).
    fn ctx_daemon(workspace: &Path, run_id: &str) -> StepContext {
        ctx_with_daemon(workspace, run_id, true)
    }

    fn ctx_with_daemon(workspace: &Path, run_id: &str, daemon: bool) -> StepContext {
        let run_context = RunContext {
            daemon_managed: daemon,
            ..RunContext::default()
        };
        let mut context = StepContext::from_run_context(
            PathBuf::from(workspace),
            run_id.to_string(),
            &run_context,
        );
        context.set_current_step_id("workspace_ownership");
        context
    }

    fn init_git(workspace: &Path) {
        std::fs::create_dir_all(workspace.join(".git")).unwrap();
    }

    fn write_bootstrap(workspace: &Path, run_id: &str) {
        let luther = workspace.join(".luther");
        std::fs::create_dir_all(&luther).unwrap();
        std::fs::write(luther.join("workspace-owner"), run_id).unwrap();
    }

    fn write_durable(workspace: &Path, run_id: &str) {
        let durable = workspace.join(".git/luther");
        std::fs::create_dir_all(&durable).unwrap();
        std::fs::write(durable.join("workspace-owner"), run_id).unwrap();
    }

    // -----------------------------------------------------------------------
    // Promotion: daemon-managed workspaces with valid evidence
    // -----------------------------------------------------------------------

    #[test]
    fn promotes_bootstrap_to_durable() {
        let dir = tempfile::tempdir().unwrap();
        write_bootstrap(dir.path(), "run-A");
        init_git(dir.path());
        let mut context = ctx_daemon(dir.path(), "run-A");
        let outcome = WorkspaceOwnershipExecutor.execute(&mut context, &serde_json::Value::Null);
        assert!(matches!(outcome, Ok(StepOutcome::Success)));
        let durable = dir.path().join(".git/luther/workspace-owner");
        assert_eq!(std::fs::read_to_string(&durable).unwrap(), "run-A");
        assert_eq!(
            context
                .get("workspace_ownership_promoted")
                .map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn idempotent_when_durable_already_valid() {
        let dir = tempfile::tempdir().unwrap();
        write_bootstrap(dir.path(), "run-A");
        init_git(dir.path());
        write_durable(dir.path(), "run-A");
        let mut context = ctx_daemon(dir.path(), "run-A");
        let outcome = WorkspaceOwnershipExecutor.execute(&mut context, &serde_json::Value::Null);
        assert!(matches!(outcome, Ok(StepOutcome::Success)));
    }

    #[test]
    fn durable_only_works_without_bootstrap() {
        let dir = tempfile::tempdir().unwrap();
        init_git(dir.path());
        write_durable(dir.path(), "run-A");
        // No bootstrap marker; durable-only evidence is trusted.
        let mut context = ctx_daemon(dir.path(), "run-A");
        let outcome = WorkspaceOwnershipExecutor.execute(&mut context, &serde_json::Value::Null);
        assert!(matches!(outcome, Ok(StepOutcome::Success)));
    }

    // -----------------------------------------------------------------------
    // Daemon-managed + no evidence → fatal
    // -----------------------------------------------------------------------

    #[test]
    fn daemon_no_evidence_is_fatal() {
        let dir = tempfile::tempdir().unwrap();
        init_git(dir.path());
        let mut context = ctx_daemon(dir.path(), "run-A");
        let outcome = WorkspaceOwnershipExecutor.execute(&mut context, &serde_json::Value::Null);
        let err = outcome.unwrap_err();
        assert!(err.to_string().contains("missing"));
    }

    #[test]
    fn daemon_no_git_no_evidence_is_fatal() {
        let dir = tempfile::tempdir().unwrap();
        // No .git at all; daemon-managed with no evidence is still fatal.
        let mut context = ctx_daemon(dir.path(), "run-A");
        let outcome = WorkspaceOwnershipExecutor.execute(&mut context, &serde_json::Value::Null);
        assert!(outcome.is_err());
    }

    // -----------------------------------------------------------------------
    // Non-daemon + no evidence → success no-op
    // -----------------------------------------------------------------------

    #[test]
    fn non_daemon_no_evidence_is_success_noop() {
        let dir = tempfile::tempdir().unwrap();
        init_git(dir.path());
        let mut context = ctx(dir.path());
        let outcome = WorkspaceOwnershipExecutor.execute(&mut context, &serde_json::Value::Null);
        assert!(matches!(outcome, Ok(StepOutcome::Success)));
        assert_eq!(
            context
                .get("workspace_ownership_skipped")
                .map(String::as_str),
            Some("true")
        );
        // No durable evidence was created.
        assert!(!dir.path().join(".git/luther/workspace-owner").exists());
    }

    #[test]
    fn non_daemon_no_git_no_evidence_is_success_noop() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = ctx(dir.path());
        let outcome = WorkspaceOwnershipExecutor.execute(&mut context, &serde_json::Value::Null);
        assert!(matches!(outcome, Ok(StepOutcome::Success)));
    }

    // -----------------------------------------------------------------------
    // Malformed evidence → always fatal (regardless of daemon)
    // -----------------------------------------------------------------------

    #[test]
    fn fatal_when_bootstrap_foreign() {
        let dir = tempfile::tempdir().unwrap();
        write_bootstrap(dir.path(), "run-foreign");
        init_git(dir.path());
        let mut context = ctx_daemon(dir.path(), "run-A");
        let outcome = WorkspaceOwnershipExecutor.execute(&mut context, &serde_json::Value::Null);
        let err = outcome.unwrap_err();
        assert!(err.to_string().contains("run-foreign"));
    }

    #[test]
    fn fatal_when_durable_foreign() {
        let dir = tempfile::tempdir().unwrap();
        init_git(dir.path());
        write_durable(dir.path(), "run-foreign");
        let mut context = ctx_daemon(dir.path(), "run-A");
        let outcome = WorkspaceOwnershipExecutor.execute(&mut context, &serde_json::Value::Null);
        let err = outcome.unwrap_err();
        assert!(err.to_string().contains("run-foreign"));
    }

    #[test]
    fn fatal_when_bootstrap_empty() {
        let dir = tempfile::tempdir().unwrap();
        write_bootstrap(dir.path(), "");
        init_git(dir.path());
        let mut context = ctx_daemon(dir.path(), "run-A");
        let outcome = WorkspaceOwnershipExecutor.execute(&mut context, &serde_json::Value::Null);
        assert!(outcome.is_err());
    }

    #[test]
    fn fatal_when_bootstrap_symlink() {
        let dir = tempfile::tempdir().unwrap();
        init_git(dir.path());
        let luther = dir.path().join(".luther");
        std::fs::create_dir_all(&luther).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", luther.join("workspace-owner")).unwrap();
        let mut context = ctx_daemon(dir.path(), "run-A");
        let outcome = WorkspaceOwnershipExecutor.execute(&mut context, &serde_json::Value::Null);
        assert!(outcome.is_err());
    }

    #[test]
    fn non_daemon_malformed_evidence_is_fatal() {
        // Even for non-daemon, present malformed evidence must fail closed.
        let dir = tempfile::tempdir().unwrap();
        write_bootstrap(dir.path(), "run-foreign");
        init_git(dir.path());
        let mut context = ctx(dir.path());
        let outcome = WorkspaceOwnershipExecutor.execute(&mut context, &serde_json::Value::Null);
        assert!(outcome.is_err());
    }

    // -----------------------------------------------------------------------
    // Verifier (WorkspaceOwnershipVerifyExecutor): daemon-managed + no
    // evidence is fatal; only non-daemon absent evidence is a no-op.
    // -----------------------------------------------------------------------

    /// Non-daemon context for the verify step.
    fn verify_ctx(workspace: &Path) -> StepContext {
        verify_ctx_with_daemon(workspace, "run-A", false)
    }

    /// Daemon-managed context for the verify step.
    fn verify_ctx_daemon(workspace: &Path, run_id: &str) -> StepContext {
        verify_ctx_with_daemon(workspace, run_id, true)
    }

    fn verify_ctx_with_daemon(workspace: &Path, run_id: &str, daemon: bool) -> StepContext {
        let run_context = RunContext {
            daemon_managed: daemon,
            ..RunContext::default()
        };
        let mut context = StepContext::from_run_context(
            PathBuf::from(workspace),
            run_id.to_string(),
            &run_context,
        );
        context.set_current_step_id("workspace_ownership_verify");
        context
    }

    #[test]
    fn verify_daemon_no_evidence_is_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = verify_ctx_daemon(dir.path(), "run-A");
        let outcome =
            WorkspaceOwnershipVerifyExecutor.execute(&mut context, &serde_json::Value::Null);
        let err = outcome.unwrap_err();
        assert!(
            err.to_string().contains("missing"),
            "daemon-managed missing evidence must be fatal, got: {err}"
        );
    }

    #[test]
    fn verify_daemon_no_git_no_evidence_is_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = verify_ctx_daemon(dir.path(), "run-A");
        let outcome =
            WorkspaceOwnershipVerifyExecutor.execute(&mut context, &serde_json::Value::Null);
        assert!(
            outcome.is_err(),
            "daemon-managed missing evidence with no .git must be fatal"
        );
    }

    #[test]
    fn verify_daemon_valid_evidence_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        write_bootstrap(dir.path(), "run-A");
        init_git(dir.path());
        write_durable(dir.path(), "run-A");
        let mut context = verify_ctx_daemon(dir.path(), "run-A");
        let outcome =
            WorkspaceOwnershipVerifyExecutor.execute(&mut context, &serde_json::Value::Null);
        assert!(matches!(outcome, Ok(StepOutcome::Success)));
        assert_eq!(
            context
                .get("workspace_ownership_verified")
                .map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn verify_daemon_bootstrap_only_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        write_bootstrap(dir.path(), "run-A");
        let mut context = verify_ctx_daemon(dir.path(), "run-A");
        let outcome =
            WorkspaceOwnershipVerifyExecutor.execute(&mut context, &serde_json::Value::Null);
        assert!(matches!(outcome, Ok(StepOutcome::Success)));
    }

    #[test]
    fn verify_daemon_durable_only_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        init_git(dir.path());
        write_durable(dir.path(), "run-A");
        let mut context = verify_ctx_daemon(dir.path(), "run-A");
        let outcome =
            WorkspaceOwnershipVerifyExecutor.execute(&mut context, &serde_json::Value::Null);
        assert!(matches!(outcome, Ok(StepOutcome::Success)));
    }

    #[test]
    fn verify_daemon_foreign_evidence_is_fatal() {
        let dir = tempfile::tempdir().unwrap();
        write_bootstrap(dir.path(), "run-foreign");
        init_git(dir.path());
        let mut context = verify_ctx_daemon(dir.path(), "run-A");
        let outcome =
            WorkspaceOwnershipVerifyExecutor.execute(&mut context, &serde_json::Value::Null);
        let err = outcome.unwrap_err();
        assert!(err.to_string().contains("run-foreign"));
    }

    #[test]
    fn verify_daemon_malformed_evidence_is_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let luther = dir.path().join(".luther");
        std::fs::create_dir_all(&luther).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", luther.join("workspace-owner")).unwrap();
        let mut context = verify_ctx_daemon(dir.path(), "run-A");
        let outcome =
            WorkspaceOwnershipVerifyExecutor.execute(&mut context, &serde_json::Value::Null);
        assert!(outcome.is_err());
    }

    #[test]
    fn verify_non_daemon_no_evidence_is_success_noop() {
        let dir = tempfile::tempdir().unwrap();
        init_git(dir.path());
        let mut context = verify_ctx(dir.path());
        let outcome =
            WorkspaceOwnershipVerifyExecutor.execute(&mut context, &serde_json::Value::Null);
        assert!(matches!(outcome, Ok(StepOutcome::Success)));
        assert_eq!(
            context
                .get("workspace_ownership_verify_no_evidence")
                .map(String::as_str),
            Some("true")
        );
        assert!(!dir.path().join(".git/luther/workspace-owner").exists());
    }

    #[test]
    fn verify_non_daemon_no_git_no_evidence_is_success_noop() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = verify_ctx(dir.path());
        let outcome =
            WorkspaceOwnershipVerifyExecutor.execute(&mut context, &serde_json::Value::Null);
        assert!(matches!(outcome, Ok(StepOutcome::Success)));
    }

    #[test]
    fn verify_non_daemon_malformed_evidence_is_fatal() {
        let dir = tempfile::tempdir().unwrap();
        write_bootstrap(dir.path(), "run-foreign");
        init_git(dir.path());
        let mut context = verify_ctx(dir.path());
        let outcome =
            WorkspaceOwnershipVerifyExecutor.execute(&mut context, &serde_json::Value::Null);
        assert!(
            outcome.is_err(),
            "present malformed evidence must fail closed even for non-daemon"
        );
    }

    #[test]
    fn verify_non_daemon_valid_evidence_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        write_bootstrap(dir.path(), "run-A");
        init_git(dir.path());
        let mut context = verify_ctx(dir.path());
        let outcome =
            WorkspaceOwnershipVerifyExecutor.execute(&mut context, &serde_json::Value::Null);
        assert!(matches!(outcome, Ok(StepOutcome::Success)));
    }

    // -----------------------------------------------------------------------
    // Issue 158 inode authorization: the verify step must produce an
    // immutable dev/inode authorization stored outside mutable workflow
    // variables. The authorization is required by the shell step so a TOCTOU
    // swap of the workspace path between the verify step and the shell step
    // cannot redirect the shell.
    // -----------------------------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn verify_captures_immutable_workspace_authorization() {
        // After a successful verify, the context must carry an immutable
        // workspace authorization (dev/inode) captured from a fresh anchored
        // descriptor. The authorization is stored outside mutable workflow
        // variables: it is not reachable via context.get(...) and cannot be
        // overwritten by context.set(...).
        let dir = tempfile::tempdir().unwrap();
        write_bootstrap(dir.path(), "run-A");
        init_git(dir.path());
        write_durable(dir.path(), "run-A");
        let mut context = verify_ctx_daemon(dir.path(), "run-A");
        let outcome =
            WorkspaceOwnershipVerifyExecutor.execute(&mut context, &serde_json::Value::Null);
        assert!(matches!(outcome, Ok(StepOutcome::Success)));
        let authorization = context
            .workspace_authorization()
            .expect("verify must populate the immutable workspace authorization");
        // The authorization must match the workspace's actual dev/inode.
        use std::os::unix::fs::MetadataExt;
        let canonical = dir.path().canonicalize().unwrap();
        let meta = std::fs::symlink_metadata(&canonical).unwrap();
        assert_eq!(authorization.dev(), meta.dev());
        assert_eq!(authorization.ino(), meta.ino());
        // The authorization is NOT a mutable workflow variable: it cannot be
        // read via context.get(...) and cannot be overwritten by set(...).
        assert!(
            context.get("workspace_authorization").is_none(),
            "the authorization must not leak into mutable workflow variables"
        );
    }

    #[cfg(unix)]
    #[test]
    fn verify_authorization_is_immutable_once_set() {
        // set_workspace_authorization is a no-op once the authorization is
        // already set: a later step cannot silently replace a verified
        // identity. The first authorization wins.
        let dir = tempfile::tempdir().unwrap();
        write_bootstrap(dir.path(), "run-A");
        init_git(dir.path());
        let mut context = verify_ctx_daemon(dir.path(), "run-A");
        WorkspaceOwnershipVerifyExecutor
            .execute(&mut context, &serde_json::Value::Null)
            .unwrap();
        let first = *context.workspace_authorization().unwrap();
        // Attempt to overwrite with a forged authorization for a different
        // directory: the setter must be a no-op.
        let other = tempfile::tempdir().unwrap();
        let forged =
            crate::engine::workspace_ownership::capture_workspace_authorization(other.path())
                .unwrap();
        context.set_workspace_authorization(forged);
        assert_eq!(
            *context.workspace_authorization().unwrap(),
            first,
            "the authorization must be immutable once set by the verify step"
        );
    }

    #[cfg(unix)]
    #[test]
    fn verify_no_evidence_does_not_set_authorization() {
        // When no evidence exists (non-daemon no-op success), the verify step
        // must NOT capture an authorization. The shell step will then operate
        // without an authorization requirement (legacy/test compatibility).
        let dir = tempfile::tempdir().unwrap();
        let mut context = verify_ctx(dir.path());
        let outcome =
            WorkspaceOwnershipVerifyExecutor.execute(&mut context, &serde_json::Value::Null);
        assert!(matches!(outcome, Ok(StepOutcome::Success)));
        assert!(
            context.workspace_authorization().is_none(),
            "no-evidence no-op must not capture an authorization"
        );
    }

    // -----------------------------------------------------------------------
    // Deterministic swap-between-verifier-and-shell test (issue 158). The
    // verify step captures an authorization from workspace A. A swap replaces
    // the workspace path with workspace B before the shell step runs. The
    // shell step must fail closed because workspace B's inode does not match
    // the authorization captured from workspace A.
    //
    // This test exercises the `descriptor_matches_authorization` seam directly
    // (the same seam the shell executor calls) so it is deterministic and does
    // not depend on process spawning timing.
    // -----------------------------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn swap_between_verifier_and_shell_is_detected() {
        use crate::engine::workspace_ownership::{
            capture_workspace_authorization, descriptor_matches_authorization, WorkspaceAnchor,
        };

        let original = tempfile::tempdir().unwrap();
        let replacement = tempfile::tempdir().unwrap();
        let canonical_original = original.path().canonicalize().unwrap();
        // Step 1 (verify): capture the authorization from workspace A.
        let authorization =
            capture_workspace_authorization(&canonical_original).expect("capture authorization");
        // Step 2 (swap): replace the workspace path with workspace B. In a real
        // run this would be a rename swap; here we simulate it by opening the
        // replacement descriptor directly.
        let canonical_replacement = replacement.path().canonicalize().unwrap();
        let swapped_anchor =
            WorkspaceAnchor::open(&canonical_replacement).expect("open swapped ws");
        // The shell step opens workspace B's descriptor and checks it against
        // the authorization captured from workspace A. The match must fail.
        let matches = descriptor_matches_authorization(swapped_anchor.as_fd(), &authorization)
            .expect("descriptor identity read");
        assert!(
            !matches,
            "a workspace swap between verifier and shell must be detected"
        );
    }

    // -----------------------------------------------------------------------
    // Deterministic post-open rename test (issue 158). The verify step
    // captures an authorization from workspace A. The shell step opens
    // workspace A's descriptor (matching the authorization), then a rename
    // swaps a different directory into workspace A's path. Because the shell
    // step's descriptor is pinned to workspace A's inode via fchdir, the child
    // lands in workspace A, not the replacement. This mirrors the
    // anchor kernel's post-open-rename invariant test but through the
    // verify→shell authorization contract.
    // -----------------------------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn post_open_rename_does_not_redirect_authorized_shell() {
        use std::process::Command;

        use crate::engine::workspace_ownership::capture_workspace_authorization;
        use crate::engine::workspace_ownership::{
            configure_fchdir_pre_exec, descriptor_matches_authorization, WorkspaceAnchor,
        };

        let original = tempfile::tempdir().unwrap();
        let replacement = tempfile::tempdir().unwrap();
        let canonical_original = original.path().canonicalize().unwrap();
        // Step 1 (verify): capture the authorization.
        let authorization =
            capture_workspace_authorization(&canonical_original).expect("capture authorization");
        // Step 2 (shell): open workspace A's descriptor. It matches the
        // authorization, so the shell proceeds.
        let anchor = WorkspaceAnchor::open(&canonical_original).expect("open anchor");
        assert!(
            descriptor_matches_authorization(anchor.as_fd(), &authorization).unwrap(),
            "the opened descriptor must match the authorization before the rename"
        );
        let child_fd = anchor.prepare_child_fd().expect("prepare child fd");
        // Step 3 (post-open rename): swap a replacement directory into
        // workspace A's path AFTER the descriptor is opened and authorized.
        let parent = canonical_original.parent().unwrap().to_path_buf();
        let parked = parent.join(format!(
            "verify-shell-rename-parked-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::rename(&canonical_original, &parked).expect("park original");
        std::fs::rename(replacement.path(), &canonical_original).expect("swap replacement in");
        // Write a sentinel into the parked (original) directory so we can
        // verify the child landed there, not in the replacement.
        std::fs::write(parked.join("verify-shell-sentinel"), b"original").unwrap();
        // The child's cwd is pinned to the original inode via fchdir, so it
        // must land in the original despite the path swap.
        let mut command = Command::new("sh");
        configure_fchdir_pre_exec(&mut command, &child_fd).unwrap();
        command
            .arg("-c")
            .arg("test -f verify-shell-sentinel && echo ORIGINAL || echo REDIRECTED");
        let output = command.output().expect("spawn anchored child");
        drop(child_fd);
        let result = String::from_utf8_lossy(&output.stdout).trim().to_string();
        assert_eq!(
            result, "ORIGINAL",
            "post-open rename must not redirect the authorized shell"
        );
    }

    // -----------------------------------------------------------------------
    // The shell executor fails closed when the workspace descriptor does not
    // match the authorization captured by the verify step. This is the
    // end-to-end verify→shell contract through the real ShellExecutor.
    // -----------------------------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn shell_fails_closed_when_workspace_swapped_after_verify() {
        use crate::engine::executors::ShellExecutor;

        let original = tempfile::tempdir().unwrap();
        let replacement = tempfile::tempdir().unwrap();
        let canonical_original = original.path().canonicalize().unwrap();
        // Step 1 (verify): capture the authorization from workspace A and set
        // it on the context (as the verify step does).
        let authorization = crate::engine::workspace_ownership::capture_workspace_authorization(
            &canonical_original,
        )
        .unwrap();
        // Set up a context pointing at workspace A and carry the authorization.
        let mut context = StepContext::new(canonical_original.clone(), "run-swap-e2e".to_string());
        context.set_workspace_authorization(authorization);
        // Step 2 (swap): replace the workspace path with workspace B.
        let parent = canonical_original.parent().unwrap().to_path_buf();
        let parked = parent.join(format!(
            "shell-swap-parked-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::rename(&canonical_original, &parked).expect("park original");
        std::fs::rename(replacement.path(), &canonical_original).expect("swap replacement in");
        // Step 3 (shell): the shell executor opens the descriptor at workspace
        // A's path (now the replacement) and checks it against the
        // authorization (captured from the original). The mismatch must fail
        // closed.
        let params = serde_json::json!({"command": "true"});
        let outcome = ShellExecutor.execute(&mut context, &params);
        let err = outcome.expect_err("swap after verify must fail closed in shell");
        assert!(
            err.to_string().contains("does not match the authorization"),
            "expected authorization mismatch error, got: {err}"
        );
    }
}
