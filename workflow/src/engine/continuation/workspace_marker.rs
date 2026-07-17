//! Durable workspace ownership marker for cleanup-failure-abandonment recovery.
//!
//! Extracted from the parent continuation module to keep the marker
//! provisioning and verification logic in a single cohesive unit. The marker
//! is a regular file at `.luther/workspace-owner` whose content is the owning
//! `run_id`.
//!
//! @plan:PLAN-20260623-LUTHER-CONTINUATION

use std::path::{Path, PathBuf};

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
/// This is the durable ownership anchor consulted during
/// cleanup-failure-abandonment recovery. Provisioning call sites should write it once
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
pub(crate) fn verify_workspace_ownership_marker(workspace: &Path, run_id: &str) -> Option<String> {
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
