//! Cohesive workspace ownership abstraction: two-phase ownership evidence.
//!
//! This module is the single source of truth for workspace ownership evidence.
//! It unifies the bootstrap (`.luther/workspace-owner`) and durable
//! (`.git/luther/workspace-owner`) phases behind one provision / verify /
//! promote API, so call sites (daemon launch/resume, child orchestration,
//! failure cleanup, scope measurement, and the graph-level
//! `workspace_ownership` step) never duplicate the crash-safe publication or
//! fail-closed verification logic.
//!
//! ## Two-phase design (issue 158)
//!
//! 1. **Bootstrap evidence** — `.luther/workspace-owner`. Written *before* Git
//!    initialization so an interrupted `git init` never strands an unowned
//!    workspace. It is the provisioning anchor established by `provision_*`.
//! 2. **Durable evidence** — `.git/luther/workspace-owner`. After `.git`
//!    exists, the *exact* bootstrap content is promoted to the durable path
//!    using the same crash-safe exact-byte publication pattern
//!    (temp + atomic hard-link + fsync). The durable record is the long-lived
//!    anchor consulted by verify/cleanup paths because it survives repository
//!    resets and is naturally invisible to scope measurement (`.git` metadata).
//!
//! ## Verification contract
//!
//! `verify_workspace_ownership` accepts the workspace as trusted when **at
//! least one** exact owner record (bootstrap or durable) is present and valid,
//! but fails closed if **any** present bootstrap or durable record is
//! malformed, symlinked, non-regular, or foreign. Requiring `.git` and
//! `.git/luther` to be real directories for durable evidence prevents symlink
//! redirection of the durable record.
//!
//! Read-only validation, scope measurement, and failure cleanup call
//! `verify_workspace_ownership` and therefore never create first ownership.
//!
//! @plan:PLAN-20260623-LUTHER-CONTINUATION

use std::path::{Path, PathBuf};

use crate::engine::continuation::workspace_marker::{
    self, reject_symlinked_luther_parent, reject_symlinked_workspace_root,
};

mod durable_publication;

pub(crate) use durable_publication::{
    configure_fchdir_pre_exec, AnchoredMarkerVerdict, WorkspaceAnchor,
};

/// Bootstrap marker path: `.luther/workspace-owner`, written before Git init.
pub(crate) const BOOTSTRAP_OWNER_MARKER: &str = ".luther/workspace-owner";

/// Backward-compatible alias for the bootstrap marker, preserved so existing
/// call sites (scope-control exclusion) keep a stable constant name.
pub(crate) const WORKSPACE_OWNER_MARKER: &str = BOOTSTRAP_OWNER_MARKER;

/// Durable marker directory beneath `.git`: `.git/luther`.
pub(crate) const DURABLE_OWNER_DIR: &str = ".git/luther";

/// Durable marker path: `.git/luther/workspace-owner`, promoted after Git init.
pub(crate) const DURABLE_OWNER_MARKER: &str = ".git/luther/workspace-owner";

/// Immutable dev/inode authorization for a workspace, captured by the
/// `workspace_ownership_verify` step from a verified workspace descriptor and
/// stored outside mutable workflow variables in [`StepContext`](crate::engine::executor::StepContext).
///
/// The authorization is the exact identity of the workspace directory the
/// verify step anchored to. The shell step requires any workspace descriptor
/// it opens to match this identity exactly, so a TOCTOU swap of the workspace
/// path between the verify step and the shell step cannot redirect the shell
/// to a different directory.
///
/// The type is opaque: it cannot be constructed outside this module, so a
/// caller cannot forge an authorization. It is only produced by
/// [`capture_workspace_authorization`] after a successful anchored verification.
///
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceAuthorization {
    dev: u64,
    ino: u64,
}

impl WorkspaceAuthorization {
    /// The device number of the authorized workspace.
    #[must_use]
    pub const fn dev(self) -> u64 {
        self.dev
    }

    /// The inode number of the authorized workspace.
    #[must_use]
    pub const fn ino(self) -> u64 {
        self.ino
    }

    /// Build an authorization from a verified [`durable_publication::FileIdentity`].
    /// Private so the type can only be constructed via
    /// [`capture_workspace_authorization`].
    fn from_identity(identity: durable_publication::FileIdentity) -> Self {
        Self {
            dev: identity.dev(),
            ino: identity.ino(),
        }
    }
}

#[cfg(test)]
pub(crate) fn capture_workspace_authorization(
    workspace: &Path,
) -> std::io::Result<WorkspaceAuthorization> {
    let canonical = workspace.canonicalize()?;
    let anchor = durable_publication::WorkspaceAnchor::open(&canonical)?;
    Ok(WorkspaceAuthorization::from_identity(anchor.identity()))
}

/// Whether an open workspace descriptor matches `authorization` exactly,
/// descriptor-relative (no path re-resolution). The shell executor opens the
/// workspace descriptor, then calls this to confirm the descriptor it holds is
/// the exact inode the verify step authorized. A mismatch (e.g. a TOCTOU swap
/// of the workspace path between the verify step and the shell step) fails
/// closed.
///
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn descriptor_matches_authorization(
    fd: std::os::fd::BorrowedFd<'_>,
    authorization: &WorkspaceAuthorization,
) -> std::io::Result<bool> {
    let identity = durable_publication::FileIdentity::of_descriptor(fd)?;
    Ok(identity.dev() == authorization.dev() && identity.ino() == authorization.ino())
}

// ===========================================================================
// Consolidated ownership descriptor kernel (issue 158 slices 1-3)
// ===========================================================================
//
// The OwnershipVerdict is the single adjudication result that replaces the
// previous three-step dance (evidence_exists → verify → capture_authorization).
// It opens ONE workspace anchor and retains it through the verdict so callers
// that need the descriptor (authorization, promotion, child-spawn) never reopen
// the workspace path — closing the TOCTOU window end-to-end.

/// The consolidated workspace ownership adjudication result.
///
/// Produced by [`adjudicate_workspace_ownership`] from a single descriptor
/// kernel: one `WorkspaceAnchor` is opened and retained through the verdict,
/// so the authorization, promotion, and child-spawn paths all operate on the
/// exact same verified descriptor without reopening the workspace path.
///
/// - [`OwnershipVerdict::Owned`] means the workspace carries at least one
///   exact-owner marker (bootstrap or durable) verified descriptor-relative.
///   The retained [`VerifiedWorkspace`] yields an immutable authorization and
///   promotes durable evidence via the same anchor.
/// - [`OwnershipVerdict::NoEvidence`] means neither marker exists; the
///   workspace is unowned.
/// - [`OwnershipVerdict::Rejected`] means a present marker is malformed,
///   foreign, symlinked, or uninspectable. The caller must fail closed.
///
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
#[derive(Debug)]
pub enum OwnershipVerdict {
    /// The workspace is owned: at least one exact-owner marker was verified
    /// descriptor-relative. The retained anchor is available for
    /// authorization, promotion, and child-spawn.
    Owned(VerifiedWorkspace),
    /// No ownership evidence (bootstrap or durable) exists.
    NoEvidence,
    /// A present marker is rejected: malformed, foreign, symlinked,
    /// non-regular, or uninspectable. The reason is a bounded categorical
    /// message.
    Rejected(String),
}

/// A workspace whose ownership has been verified via the consolidated
/// descriptor kernel. Holds the single [`WorkspaceAnchor`] opened during
/// adjudication, so the descriptor is retained through authorization capture,
/// durable promotion, and child process spawning without reopening the
/// workspace path.
///
/// The type is opaque: it can only be constructed inside this module (via
/// [`adjudicate_workspace_ownership`]).
///
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
#[derive(Debug)]
pub struct VerifiedWorkspace {
    anchor: durable_publication::WorkspaceAnchor,
}

impl VerifiedWorkspace {
    /// Produce the immutable [`WorkspaceAuthorization`] from the retained
    /// anchor's captured identity. No second descriptor open occurs: the
    /// identity was captured at anchor construction and is simply read.
    #[must_use]
    pub fn authorization(&self) -> WorkspaceAuthorization {
        WorkspaceAuthorization::from_identity(self.anchor.identity())
    }

    /// Promote verified bootstrap ownership evidence to the durable path via
    /// the SAME retained anchor. No reopen of the workspace path occurs,
    /// closing the TOCTOU window in which a concurrent attacker could swap
    /// the workspace path between verify and promotion.
    ///
    /// @plan:PLAN-20260623-LUTHER-CONTINUATION
    pub fn promote(&self, run_id: &str) -> std::io::Result<()> {
        durable_publication::promote_via_anchor(&self.anchor, run_id)
    }
}

/// Adjudicate workspace ownership from a single descriptor kernel.
///
/// Opens ONE [`WorkspaceAnchor`] from the canonical workspace path, verifies
/// both the bootstrap and durable markers descriptor-relative relative to that
/// exact anchor, and returns a [`OwnershipVerdict`] that retains the anchor
/// for downstream use. There is no second anchor open: the authorization,
/// promotion, and child-spawn paths all operate on the same verified
/// descriptor.
///
/// **Fail-closed contract:**
/// - `Owned` when at least one marker is trusted.
/// - `NoEvidence` when neither marker exists.
/// - `Rejected` when any present marker is malformed, foreign, symlinked,
///   non-regular, or uninspectable, OR when the workspace root is symlinked,
///   non-canonicalizable, or cannot be anchored.
///
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn adjudicate_workspace_ownership(workspace: &Path, run_id: &str) -> OwnershipVerdict {
    if let Some(reason) = reject_symlinked_workspace_root(workspace) {
        return OwnershipVerdict::Rejected(reason);
    }
    let canonical_workspace = match workspace.canonicalize() {
        Ok(path) => path,
        Err(err) => {
            return OwnershipVerdict::Rejected(format!("workspace cannot be canonicalized: {err}"));
        }
    };
    if let Some(reason) =
        workspace_marker::revalidate_workspace_root_identity(workspace, &canonical_workspace)
    {
        return OwnershipVerdict::Rejected(reason);
    }
    if let Some(reason) = reject_symlinked_luther_parent(&canonical_workspace) {
        return OwnershipVerdict::Rejected(reason);
    }
    let anchor = match durable_publication::WorkspaceAnchor::open(&canonical_workspace) {
        Ok(anchor) => anchor,
        Err(err) => {
            return OwnershipVerdict::Rejected(format!("workspace cannot be anchored: {err}"));
        }
    };
    if let Some(reason) = validate_durable_directory(&canonical_workspace) {
        return OwnershipVerdict::Rejected(reason);
    }
    let bootstrap_verdict = durable_publication::snapshot_bootstrap_marker(anchor.as_fd(), run_id);
    let durable_verdict = durable_publication::snapshot_durable_marker(anchor.as_fd(), run_id);
    if bootstrap_verdict.is_rejected() {
        return OwnershipVerdict::Rejected(rejection_reason(bootstrap_verdict).unwrap_or_default());
    }
    if durable_verdict.is_rejected() {
        return OwnershipVerdict::Rejected(rejection_reason(durable_verdict).unwrap_or_default());
    }
    if bootstrap_verdict == durable_publication::AnchoredMarkerVerdict::Trusted
        || durable_verdict == durable_publication::AnchoredMarkerVerdict::Trusted
    {
        return OwnershipVerdict::Owned(VerifiedWorkspace { anchor });
    }
    OwnershipVerdict::NoEvidence
}

/// Bootstrap marker path beneath a workspace root.
fn bootstrap_marker_path(workspace: &Path) -> PathBuf {
    workspace.join(BOOTSTRAP_OWNER_MARKER)
}

/// Durable marker path beneath a workspace root.
fn durable_marker_path(workspace: &Path) -> PathBuf {
    workspace.join(DURABLE_OWNER_MARKER)
}

/// Durable marker directory beneath a workspace root.
fn durable_dir(workspace: &Path) -> PathBuf {
    workspace.join(DURABLE_OWNER_DIR)
}

/// Typed inspection result for a marker path. Only [`MarkerState::Absent`]
/// means the marker is missing; [`MarkerState::Uninspectable`] is a
/// fail-closed rejection so a `PermissionDenied` (or any other inspection
/// error) can never be silently treated as "absent".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkerState {
    /// The marker path has an entry (file, symlink, directory, etc.).
    Present,
    /// The marker path has no entry (`NotFound`).
    Absent,
    /// Inspecting the marker path failed for any reason other than `NotFound`
    /// (e.g. `PermissionDenied`). Callers must reject, never treat as absent.
    Uninspectable,
}

/// Typed inspection of whether a marker path has an entry. Uses
/// `symlink_metadata` so a symlink is observed rather than its target. Only
/// `NotFound` maps to [`MarkerState::Absent`]; every other error maps to
/// [`MarkerState::Uninspectable`] so an unreadable marker is never silently
/// treated as missing.
fn marker_state(marker: &Path) -> MarkerState {
    match std::fs::symlink_metadata(marker) {
        Ok(_) => MarkerState::Present,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => MarkerState::Absent,
        Err(_) => MarkerState::Uninspectable,
    }
}

/// Reject a marker record at `marker` that is a symlink, a directory, empty, or
/// belongs to a different run. Returns `None` when the marker is absent or is a
/// valid exact-owner regular file; `Some(reason)` explains any rejection.
///
/// This is the path-based pre-check used by the promotion path before the
/// descriptor-anchored promotion re-validates the exact bytes. Read-only
/// verification (`verify_workspace_ownership`) no longer uses this path-based
/// classify-then-reread step: it uses a single descriptor-anchored snapshot
/// per marker.
fn validate_present_marker(marker: &Path, run_id: &str, workspace_root: &Path) -> Option<String> {
    workspace_marker::verify_marker_file(marker, run_id, workspace_root)
}

/// Require `.git` and `.git/luther` to be real directories for durable
/// evidence. Returns `None` when both are real directories, or `Some(reason)`
/// explaining any rejection (missing, symlink, or non-directory).
///
/// A missing `.git` simply means durable evidence is not yet established; the
/// caller decides whether that is acceptable. A *present* `.git` that is a
/// symlink or non-directory, or a `.git/luther` that is a symlink or
/// non-directory, is a hard failure so the durable record cannot be redirected.
fn validate_durable_directory(workspace: &Path) -> Option<String> {
    let git = workspace.join(".git");
    match std::fs::symlink_metadata(&git) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Some(format!(
                "workspace Git metadata is a symlink and must be a real directory: {git_display}",
                git_display = git.display()
            ));
        }
        Ok(meta) if !meta.is_dir() => {
            return Some(format!(
                "workspace Git metadata is not a directory: {git_display}",
                git_display = git.display()
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            return Some(format!(
                "workspace Git metadata cannot be inspected: {error}: {git_display}",
                git_display = git.display()
            ));
        }
    }
    let durable_dir = durable_dir(workspace);
    match std::fs::symlink_metadata(&durable_dir) {
        Ok(meta) if meta.file_type().is_symlink() => Some(format!(
            "durable ownership directory is a symlink and must be a real directory: {display}",
            display = durable_dir.display()
        )),
        Ok(meta) if !meta.is_dir() => Some(format!(
            "durable ownership directory is not a directory: {display}",
            display = durable_dir.display()
        )),
        Ok(_) => None,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => Some(format!(
            "durable ownership directory cannot be inspected: {error}: {display}",
            display = durable_dir.display()
        )),
    }
}

/// Unified workspace ownership verification.
///
/// Accepts the workspace as trusted when **at least one** exact owner record
/// (bootstrap or durable) is present and valid, but fails closed if **any**
/// present bootstrap or durable record is malformed, symlinked, non-regular,
/// or foreign. Durable evidence additionally requires `.git` and `.git/luther`
/// to be real directories.
///
/// Returns `None` when the workspace is trusted, or `Some(reason)` explaining
/// the rejection. This function is read-only: it never creates or modifies any
/// ownership evidence.
///
/// ## Descriptor-anchored single verdict (issue 158)
///
/// Both the bootstrap and durable markers are verified by a **single
/// descriptor-anchored snapshot** produced relative to a workspace descriptor
/// opened with `O_NOFOLLOW`. Each snapshot opens the marker relative to its
/// parent directory descriptor, `fstat`s the *actual opened fd*, requires a
/// regular file of bounded size, and reads the exact bytes for comparison.
/// There is no separate classify step that a TOCTOU swap could invalidate
/// between classification and the content read, and no boolean-exists success
/// path: an existing marker is only trusted when its descriptor-anchored
/// exact bytes match the expected owner run id.
///
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn verify_workspace_ownership(workspace: &Path, run_id: &str) -> Option<String> {
    if let Some(reason) = reject_symlinked_workspace_root(workspace) {
        return Some(reason);
    }
    let canonical_workspace = match workspace.canonicalize() {
        Ok(path) => path,
        Err(err) => return Some(format!("workspace cannot be canonicalized: {err}")),
    };
    if let Some(reason) =
        workspace_marker::revalidate_workspace_root_identity(workspace, &canonical_workspace)
    {
        return Some(reason);
    }
    if let Some(reason) = reject_symlinked_luther_parent(&canonical_workspace) {
        return Some(reason);
    }
    // Anchor every marker read to the canonical workspace descriptor so a
    // TOCTOU swap of the workspace path cannot redirect marker resolution.
    let anchor = match durable_publication::WorkspaceAnchor::open(&canonical_workspace) {
        Ok(anchor) => anchor,
        Err(err) => return Some(format!("workspace cannot be anchored: {err}")),
    };
    // The durable directory integrity check rejects a present `.git` or
    // `.git/luther` that is a symlink or non-directory, closing the redirect
    // hole before the durable marker snapshot.
    if let Some(reason) = validate_durable_directory(&canonical_workspace) {
        return Some(reason);
    }
    let bootstrap_verdict = durable_publication::snapshot_bootstrap_marker(anchor.as_fd(), run_id);
    let durable_verdict = durable_publication::snapshot_durable_marker(anchor.as_fd(), run_id);
    // Any present rejected marker fails closed regardless of the other.
    if bootstrap_verdict.is_rejected() {
        return rejection_reason(bootstrap_verdict);
    }
    if durable_verdict.is_rejected() {
        return rejection_reason(durable_verdict);
    }
    // At least one trusted marker: trusted.
    if bootstrap_verdict == durable_publication::AnchoredMarkerVerdict::Trusted
        || durable_verdict == durable_publication::AnchoredMarkerVerdict::Trusted
    {
        return None;
    }
    // Both absent: not trusted (no evidence).
    Some(format!(
        "workspace ownership marker is missing: no bootstrap or durable owner record for run '{run_id}'"
    ))
}

/// Extract the bounded categorical rejection reason from a rejected verdict.
fn rejection_reason(verdict: durable_publication::AnchoredMarkerVerdict) -> Option<String> {
    match verdict {
        durable_publication::AnchoredMarkerVerdict::Rejected(reason) => Some(reason),
        _ => None,
    }
}

/// Publish the bootstrap owner record through an already-opened workspace
/// descriptor. The caller retains the descriptor from workspace creation/open
/// through publication, so the marker cannot be redirected by a root rename.
pub(crate) fn publish_bootstrap_via_anchor(
    anchor: &WorkspaceAnchor,
    run_id: &str,
) -> std::io::Result<()> {
    durable_publication::publish_bootstrap_marker(anchor.as_fd(), run_id.as_bytes())
}

/// Verify the bootstrap marker through an already-opened workspace descriptor,
/// producing a single cohesive verdict bound to the exact inode held open. No
/// path re-resolution occurs, closing the TOCTOU window in which a concurrent
/// attacker could swap the workspace path between publication and
/// adjudication.
///
/// Callers that retain a [`WorkspaceAnchor`] through publication must use this
/// instead of [`adjudicate_workspace_ownership`], which opens its own anchor
/// from a path and therefore re-introduces a path-based trust decision after a
/// descriptor-anchored write.
pub(crate) fn snapshot_bootstrap_marker_via_anchor(
    anchor: &WorkspaceAnchor,
    run_id: &str,
) -> AnchoredMarkerVerdict {
    durable_publication::snapshot_bootstrap_marker(anchor.as_fd(), run_id)
}

/// Provision bootstrap ownership evidence for an empty or self-owned workspace.
///
/// Writes the `.luther/workspace-owner` bootstrap marker recording `run_id` as
/// the owner, using the established crash-safe publication pattern. This is the
/// only provisioning entry point: it establishes bootstrap evidence before Git
/// initialization so an interrupted `git init` never strands an unowned
/// workspace. After `.git` exists, call [`promote_workspace_owner_marker`] to
/// promote the exact bootstrap content to the durable path.
///
/// Delegates to the existing `continuation::workspace_marker` provisioning so
/// the claimable-workspace invariants (empty or only `.luther` with temp
/// files) and idempotent same-owner behavior are preserved.
///
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn provision_workspace_owner_marker(workspace: &Path, run_id: &str) -> std::io::Result<()> {
    workspace_marker::provision_workspace_owner_marker(workspace, run_id)
}

/// Ensure a daemon-controlled workspace has valid ownership evidence for its
/// current lifecycle phase.
///
/// Before Git initialization this establishes the bootstrap claim. Once a real
/// `.git` directory exists it never creates a first claim: it verifies existing
/// evidence and only promotes an exact bootstrap claim when durable evidence is
/// not already present.
pub fn provision_workspace_ownership(workspace: &Path, run_id: &str) -> std::io::Result<()> {
    match std::fs::symlink_metadata(workspace.join(".git")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            provision_workspace_owner_marker(workspace, run_id)
        }
        Err(error) => Err(error),
        Ok(_) => ensure_durable_workspace_ownership(workspace, run_id),
    }
}

/// Verify existing ownership and ensure the durable record exists when Git has
/// been initialized. This function may only promote already-verified bootstrap
/// evidence; it never creates a first claim for a populated workspace.
///
/// **No boolean-exists success:** every existing durable entry is validated by
/// [`verify_workspace_ownership`] (which does anchored exact validation of both
/// bootstrap and durable records) before any existence check. Only after
/// verification passes may an existing durable record be accepted or a new one
/// promoted.
///
/// **Issue 158 root anchor:** the same workspace anchor is retained through
/// verify, durable inspection, and promotion. The anchor is opened once from
/// the canonicalized workspace path, its dev/inode is captured at construction,
/// and promotion operates relative to that exact descriptor without reopening
/// the workspace path. This closes the TOCTOU window in which a concurrent
/// attacker could swap the workspace path between verify and promotion.
pub fn ensure_durable_workspace_ownership(workspace: &Path, run_id: &str) -> std::io::Result<()> {
    // Reject a symlinked workspace root before canonicalizing, matching the
    // guards in verify_workspace_ownership and promote_workspace_owner_marker.
    // canonicalize resolves symlinks, which would silently accept a symlinked
    // root as if it were a real directory.
    if let Some(reason) = reject_symlinked_workspace_root(workspace) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, reason));
    }
    let canonical_workspace = workspace.canonicalize()?;
    ensure_durable_with_anchor(&canonical_workspace, run_id)
}

/// Internal: retain a single [`WorkspaceAnchor`] through verify, durable
/// inspection, and promotion. The anchor is opened from the canonical
/// workspace path; its dev/inode is captured at construction. Promotion
/// operates relative to that exact descriptor, so no reopen can be redirected
/// by a TOCTOU swap of the workspace path.
fn ensure_durable_with_anchor(canonical: &Path, run_id: &str) -> std::io::Result<()> {
    // Anchored exact validation: any malformed, foreign, symlinked, or
    // non-regular bootstrap or durable entry fails closed here before any
    // existence-based shortcut. The anchor is opened from the canonical path
    // and retained for the durable inspection and promotion below.
    let anchor = durable_publication::WorkspaceAnchor::open(canonical)?;
    if let Some(reason) = verify_anchored(&anchor, canonical, run_id) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, reason));
    }
    // Before `.git` exists (pre-Git state), only bootstrap evidence can be
    // present and it is already trusted. The durable record is promoted later
    // by the graph-level `workspace_ownership` step once `.git` exists, so a
    // verified bootstrap-only pre-Git state is a success here.
    //
    // Only `NotFound` means `.git` has not been initialized yet; every other
    // inspection error (e.g. `PermissionDenied`) must propagate rather than
    // be silently treated as "pre-Git" success, because an uninspectable
    // `.git` could mask an attacker-controlled durable record.
    match std::fs::symlink_metadata(canonical.join(".git")) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    // `.git` exists. If durable evidence is already present, it was validated
    // by verify_anchored above, so no promotion is needed. This handles
    // durable-only workspaces (e.g. bootstrap removed after promotion)
    // correctly. Otherwise, promote from verified bootstrap via the SAME
    // anchor (no reopen), so a TOCTOU swap of the workspace path between
    // verify and promotion cannot redirect the durable write.
    if durable_publication::snapshot_durable_marker(anchor.as_fd(), run_id)
        == durable_publication::AnchoredMarkerVerdict::Absent
    {
        durable_publication::promote_via_anchor(&anchor, run_id)?;
    }
    Ok(())
}

/// Anchored verify: run the read-only fail-closed verification relative to an
/// already-open anchor, capturing its dev/inode at construction. Returns
/// `None` when trusted, or `Some(reason)` explaining the rejection.
fn verify_anchored(
    anchor: &durable_publication::WorkspaceAnchor,
    canonical: &Path,
    run_id: &str,
) -> Option<String> {
    // The durable directory integrity check rejects a present `.git` or
    // `.git/luther` that is a symlink or non-directory, closing the redirect
    // hole before the durable marker snapshot.
    if let Some(reason) = validate_durable_directory(canonical) {
        return Some(reason);
    }
    let bootstrap_verdict = durable_publication::snapshot_bootstrap_marker(anchor.as_fd(), run_id);
    let durable_verdict = durable_publication::snapshot_durable_marker(anchor.as_fd(), run_id);
    // Any present rejected marker fails closed regardless of the other.
    if bootstrap_verdict.is_rejected() {
        return rejection_reason(bootstrap_verdict);
    }
    if durable_verdict.is_rejected() {
        return rejection_reason(durable_verdict);
    }
    // At least one trusted marker: trusted.
    if bootstrap_verdict == durable_publication::AnchoredMarkerVerdict::Trusted
        || durable_verdict == durable_publication::AnchoredMarkerVerdict::Trusted
    {
        return None;
    }
    // Both absent: not trusted (no evidence).
    Some(format!(
        "workspace ownership marker is missing: no bootstrap or durable owner record for run '{run_id}'"
    ))
}

/// Return whether either ownership record path has an entry, including a
/// malformed file or symlink. Callers use this only to decide whether strict
/// verification is required; the evidence itself is always validated by
/// [`verify_workspace_ownership`] via a descriptor-anchored snapshot.
///
/// **Fail closed on inspection errors:** an uninspectable marker path (e.g.
/// `PermissionDenied`) is treated as "evidence exists" so the caller proceeds
/// to strict verification (which then rejects), rather than silently treating
/// an unreadable marker as "no evidence" and skipping verification.
#[must_use]
pub fn workspace_ownership_evidence_exists(workspace: &Path) -> bool {
    // Anchor the existence check to the workspace descriptor so the decision
    // of whether strict anchored verification is required cannot be subverted
    // by a TOCTOU swap of the marker paths. If the workspace cannot be
    // anchored (e.g. it is a symlink root or does not exist), fall back to the
    // path-based inspection which fails closed on uninspectable markers.
    match workspace.canonicalize() {
        Ok(canonical) => match durable_publication::WorkspaceAnchor::open(&canonical) {
            Ok(anchor) => durable_publication::anchored_evidence_exists(anchor.as_fd()),
            Err(_) => path_based_evidence_exists(workspace),
        },
        Err(_) => path_based_evidence_exists(workspace),
    }
}

/// Path-based fallback for [`workspace_ownership_evidence_exists`] when the
/// workspace cannot be anchored. Fails closed on uninspectable markers.
fn path_based_evidence_exists(workspace: &Path) -> bool {
    !matches!(
        marker_state(&bootstrap_marker_path(workspace)),
        MarkerState::Absent
    ) || !matches!(
        marker_state(&durable_marker_path(workspace)),
        MarkerState::Absent
    )
}

/// Promote exact bootstrap ownership evidence to the durable path.
///
/// After `.git` exists, promotes the *exact* bootstrap marker content to
/// `.git/luther/workspace-owner` using the descriptor-anchored, no-follow
/// publication pattern (temp + atomic `linkat` + fsync, all relative to an
/// `O_NOFOLLOW` `.git/luther` directory descriptor).
///
/// **Anchored exact validation contract:** the expected `run_id` is passed
/// into the descriptor promotion, which reads the bootstrap bytes from a
/// descriptor opened with `O_NOFOLLOW` relative to the workspace descriptor
/// and compares them **exactly** to `run_id.as_bytes()`. It then opens `.git`
/// relative to the already-open workspace fd, publishes the exact bytes via the
/// descriptor-anchored path, and re-reads the published durable marker from
/// its descriptor for a final exact comparison. There is no "the entry exists
/// so it must be fine" success path: every existing durable entry is validated
/// by anchored exact comparison, and any mismatch (foreign, empty, symlink,
/// directory, FIFO, etc.) fails closed. This closes the TOCTOU window in which
/// a concurrent attacker could swap the bootstrap path for a symlink or a
/// different file between verification and publication.
///
/// Idempotency: a concurrent winner (`linkat` returns `AlreadyExists`) is
/// re-read from the durable descriptor and compared exactly to the expected
/// bytes; promotion succeeds only when the existing durable record is an exact
/// same-owner match.
///
/// Requires `.git` and `.git/luther` to be real directories (created here if
/// absent, rejecting symlinks).
///
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn promote_workspace_owner_marker(workspace: &Path, run_id: &str) -> std::io::Result<()> {
    if let Some(reason) = reject_symlinked_workspace_root(workspace) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            reason,
        ));
    }
    let canonical_workspace = workspace.canonicalize()?;
    if let Some(reason) =
        workspace_marker::revalidate_workspace_root_identity(workspace, &canonical_workspace)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            reason,
        ));
    }
    let bootstrap = bootstrap_marker_path(&canonical_workspace);
    if let Some(reason) = validate_present_marker(&bootstrap, run_id, &canonical_workspace) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "cannot promote workspace ownership without verified bootstrap evidence: {reason}"
            ),
        ));
    }
    // Open the workspace anchor ONCE from the canonical path and retain it
    // through promotion. The anchor's dev/inode is captured at construction,
    // and promotion operates entirely relative to that descriptor (no reopen),
    // closing the TOCTOU window in which a concurrent attacker could swap the
    // workspace path between the path-based pre-checks above and the durable
    // write. There is no boolean-exists success path: every existing durable
    // entry is validated exactly on the same descriptor chain.
    let anchor = durable_publication::WorkspaceAnchor::open(&canonical_workspace)?;
    durable_publication::promote_via_anchor(&anchor, run_id)
}

/// Whether the workspace currently has trusted ownership evidence for `run_id`.
///
/// Convenience wrapper around [`verify_workspace_ownership`] for call sites
/// that only need a boolean (e.g. scope-control exclusion decisions).
#[must_use]
pub fn has_trusted_workspace_ownership(workspace: &Path, run_id: &str) -> bool {
    verify_workspace_ownership(workspace, run_id).is_none()
}

#[cfg(test)]
mod tests;
