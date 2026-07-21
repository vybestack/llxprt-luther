//! Durable workspace ownership marker for cleanup-failure-abandonment recovery.
//!
//! Extracted from the parent continuation module to keep the marker
//! provisioning and verification logic in a single cohesive unit. The marker
//! is a regular file at `.luther/workspace-owner` whose content is the owning
//! `run_id`.
//!
//! @plan:PLAN-20260623-LUTHER-CONTINUATION

use std::path::{Path, PathBuf};

pub(crate) const WORKSPACE_OWNER_MARKER: &str = ".luther/workspace-owner";

/// Marker file path recording the owning run id for a workspace.
fn workspace_owner_marker_path(workspace: &Path) -> PathBuf {
    workspace.join(WORKSPACE_OWNER_MARKER)
}

/// Reject symlinks in every existing workspace path component before marker
/// operations can follow a redirected ancestor. macOS's `/tmp` and `/var` are
/// fixed system aliases, so those two roots are the only accepted symlinks.
fn reject_symlinked_workspace_root(workspace: &Path) -> Option<String> {
    for component in workspace.ancestors() {
        match std::fs::symlink_metadata(component) {
            Ok(meta) if meta.file_type().is_symlink() => {
                #[cfg(target_os = "macos")]
                if component == Path::new("/tmp") || component == Path::new("/var") {
                    continue;
                }
                return Some(format!(
                    "workspace path contains a symlink and must use real directories: {}",
                    component.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Some(format!(
                    "workspace path component cannot be inspected: {}: {error}",
                    component.display()
                ));
            }
        }
    }
    None
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

/// Collision-safe temp path inside `.luther`, mirroring the established
/// crash-safe publication pattern in `scope_control::persistence`. The uuid
/// suffix guarantees uniqueness across concurrent writers and processes.
fn collision_safe_temp_path(luther_dir: &Path) -> PathBuf {
    let unique = uuid::Uuid::new_v4().simple().to_string();
    luther_dir.join(format!(".workspace-owner.tmp.{unique}"))
}

/// Fsync the `.luther` parent directory so the published hard-link is durable.
///
/// On Unix, the directory's own metadata sync (`sync_all`) is propagated to
/// the caller, because a directory fsync failure means the link entry may not
/// survive a crash and the marker could not be relied upon for durable
/// ownership. On non-Unix platforms where a reliable directory fsync is not
/// available through `sync_all`, this is a documented best-effort no-op that
/// returns `Ok(())` so callers never fail solely because the platform lacks
/// directory fsync support — mirroring the established scope-control
/// persistence helper.
fn fsync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        // Best-effort: attempt to open and sync, but treat any failure as a
        // no-op so callers never fail solely because the platform lacks
        // reliable directory fsync support.
        let _ = std::fs::File::open(dir).and_then(|file| file.sync_all());
        Ok(())
    }
}

/// Create the `.luther` directory beneath `workspace`, rejecting a symlinked
/// workspace root and a symlinked `.luther` both before and after creation to
/// close the TOCTOU window in which a concurrent attacker could swap a freshly
/// created directory for a symlink. The workspace root is checked before any
/// canonicalization so a symlinked root is rejected outright rather than
/// silently resolved.
fn ensure_luther_dir(workspace: &Path) -> std::io::Result<PathBuf> {
    if let Some(reason) = reject_symlinked_workspace_root(workspace) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            reason,
        ));
    }
    if let Some(reason) = reject_symlinked_luther_parent(workspace) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            reason,
        ));
    }
    let luther = workspace.join(".luther");
    std::fs::create_dir_all(&luther)?;
    if let Some(reason) = reject_symlinked_workspace_root(workspace) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            reason,
        ));
    }
    if let Some(reason) = reject_symlinked_luther_parent(workspace) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            reason,
        ));
    }
    Ok(luther)
}

/// Write the `.luther/workspace-owner` marker recording `run_id` as the owner
/// of `workspace`. Creates `.luther/` and the marker regular file, refusing to
/// overwrite an existing marker that belongs to a different run so two
/// concurrent runs cannot claim the same workspace. Returns `Ok(())` when the
/// marker already records the same `run_id`.
///
/// Crash-safe no-replace publication: a new marker is written to a unique temp
/// file inside `.luther`, fully synced (`write_all` + `flush` + `sync_all`),
/// then atomically hard-linked into the final marker path. `hard_link` never
/// replaces an existing file: it fails atomically with `AlreadyExists` if a
/// concurrent writer linked first, so exactly one writer wins and every later
/// writer observes the committed content. The temp is removed and the parent
/// directory fsynced so the link metadata is durable. An existing final marker
/// is never rewritten: it is validated for exact ownership, preserving the
/// symlink/empty/foreign rejection.
///
/// This is the durable ownership anchor consulted during
/// cleanup-failure-abandonment recovery. Provisioning call sites should write it once
/// when a run's workspace is created.
///
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
/// Provision ownership only for an empty workspace or an interrupted marker
/// publication containing no state except `.luther/.workspace-owner.tmp.*`.
pub fn provision_workspace_owner_marker(workspace: &Path, run_id: &str) -> std::io::Result<()> {
    for _ in 0..100 {
        match provision_workspace_owner_marker_once(workspace, run_id) {
            Ok(()) => return Ok(()),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound
                        | std::io::ErrorKind::AlreadyExists
                        | std::io::ErrorKind::InvalidData
                ) =>
            {
                let marker = workspace_owner_marker_path(workspace);
                match inspect_existing_marker(&marker, run_id) {
                    Ok(()) => return Ok(()),
                    Err(marker_error) if marker_exists(&marker) => return Err(marker_error),
                    Err(_) => {}
                }
                std::thread::yield_now();
                continue;
            }
            Err(error) => return Err(error),
        }
    }
    provision_workspace_owner_marker_once(workspace, run_id)
}

fn provision_workspace_owner_marker_once(workspace: &Path, run_id: &str) -> std::io::Result<()> {
    if let Some(reason) = reject_symlinked_workspace_root(workspace) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            reason,
        ));
    }
    std::fs::create_dir_all(workspace)?;
    if let Some(reason) = reject_symlinked_workspace_root(workspace) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            reason,
        ));
    }
    let marker = workspace_owner_marker_path(workspace);
    match std::fs::symlink_metadata(&marker) {
        Ok(_) => return inspect_existing_marker(&marker, run_id),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    if !workspace_is_claimable(workspace, run_id)? {
        if marker_exists(&marker) {
            return inspect_existing_marker(&marker, run_id);
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "refusing to claim pre-existing non-empty workspace without ownership marker: {}",
                workspace.display()
            ),
        ));
    }
    write_workspace_owner_marker(workspace, run_id)?;
    if !workspace_is_owned_after_claim(workspace, run_id)? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "workspace changed while ownership was being established: {}",
                workspace.display()
            ),
        ));
    }
    Ok(())
}

fn workspace_is_claimable(workspace: &Path, run_id: &str) -> std::io::Result<bool> {
    for entry in std::fs::read_dir(workspace)? {
        let entry = entry?;
        if entry.file_name() != ".luther" || !luther_dir_is_claimable(&entry.path(), run_id)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn luther_dir_is_claimable(path: &Path, run_id: &str) -> std::io::Result<bool> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Ok(false);
    }
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let name_is_temp = entry
            .file_name()
            .to_str()
            .is_some_and(|value| value.starts_with(".workspace-owner.tmp."));
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if !name_is_temp || metadata.file_type().is_symlink() || !metadata.is_file() {
            return Ok(false);
        }
        if std::fs::read_to_string(entry.path())? != run_id {
            return Ok(false);
        }
    }
    Ok(true)
}

fn workspace_is_owned_after_claim(workspace: &Path, run_id: &str) -> std::io::Result<bool> {
    inspect_existing_marker(&workspace_owner_marker_path(workspace), run_id)?;
    for entry in std::fs::read_dir(workspace)? {
        let entry = entry?;
        if entry.file_name() != ".luther" {
            return Ok(false);
        }
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Ok(false);
        }
        for luther_entry in std::fs::read_dir(entry.path())? {
            let luther_entry = luther_entry?;
            if luther_entry.file_name() == "workspace-owner" {
                if inspect_existing_marker(&luther_entry.path(), run_id).is_err() {
                    return Ok(false);
                }
                continue;
            }
            let is_temp = luther_entry
                .file_name()
                .to_str()
                .is_some_and(|value| value.starts_with(".workspace-owner.tmp."));
            let metadata = std::fs::symlink_metadata(luther_entry.path())?;
            if !is_temp
                || metadata.file_type().is_symlink()
                || !metadata.is_file()
                || std::fs::read_to_string(luther_entry.path())? != run_id
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

pub fn write_workspace_owner_marker(workspace: &Path, run_id: &str) -> std::io::Result<()> {
    let luther = ensure_luther_dir(workspace)?;
    let marker = workspace_owner_marker_path(workspace);
    // Existing final marker: validate exact ownership (idempotent re-run or
    // reject a foreign/empty/malformed marker) without ever rewriting it.
    if marker_exists(&marker) {
        return inspect_existing_marker(&marker, run_id);
    }
    // Publish a brand-new marker via the crash-safe temp + atomic hard-link
    // (no replace) path.
    publish_new_marker(&luther, &marker, run_id)
}

/// Whether any entry (including a symlink or directory) exists at the marker
/// path. `symlink_metadata` is used so a symlink is observed rather than its
/// target; a dangling symlink still counts as "exists" and is rejected by the
/// subsequent inspection.
fn marker_exists(marker: &Path) -> bool {
    std::fs::symlink_metadata(marker).is_ok()
}

/// Crash-safe no-replace publication of a brand-new marker. Writes a unique
/// temp file, fsyncs it, atomically hard-links it to the final path, removes
/// the temp, and fsyncs the parent directory. A concurrent winner is detected
/// via `hard_link`'s atomic `AlreadyExists` and delegated to exact-owner
/// validation.
fn publish_new_marker(luther: &Path, marker: &Path, run_id: &str) -> std::io::Result<()> {
    let temp = collision_safe_temp_path(luther);
    // Durability: fully write and fsync the temp before linking so a crash
    // never leaves a partial final marker.
    if let Err(err) = write_and_sync_temp(&temp, run_id.as_bytes()) {
        let _ = std::fs::remove_file(&temp);
        return Err(err);
    }
    link_temp_to_final(&temp, marker, luther, run_id)
}

/// Create a brand-new temp file, write `data`, flush, and fsync its contents to
/// stable storage. `create_new` guarantees the temp path is unique to this
/// writer.
fn write_and_sync_temp(temp: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp)?;
    file.write_all(data)?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

/// Atomically link the synced temp into the final marker path (no replace), then
/// remove the temp and fsync the parent directory. If a concurrent writer
/// already linked the final marker, validate the winner's content for exact
/// ownership instead of overwriting it.
fn link_temp_to_final(
    temp: &Path,
    marker: &Path,
    luther: &Path,
    run_id: &str,
) -> std::io::Result<()> {
    match std::fs::hard_link(temp, marker) {
        Ok(()) => {
            // The final link is committed; the temp is now an extra name for
            // the same inode and can be removed without affecting the marker.
            let _ = std::fs::remove_file(temp);
            // Persist the new directory entry for the marker.
            fsync_dir(luther)?;
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            // A concurrent writer won the race and linked the final marker.
            // Clean up our temp and validate the winner rather than overwrite.
            let _ = std::fs::remove_file(temp);
            inspect_existing_marker(marker, run_id)
        }
        Err(err) => {
            let _ = std::fs::remove_file(temp);
            Err(err)
        }
    }
}

/// Validate an existing marker file: reject symlinks, directories, empty
/// content, and a different owner. Returns `Ok(())` only for exact same-owner
/// idempotency. The marker is never rewritten by this path.
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
    if !meta.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "workspace owner marker is not a regular file: {marker_display}",
                marker_display = marker.display()
            ),
        ));
    }
    let existing = std::fs::read_to_string(marker)?;
    if existing.trim().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "workspace owner marker is empty and cannot establish ownership: {}",
                marker.display()
            ),
        ));
    }
    if existing != run_id {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("workspace owner marker belongs to run '{existing}' not '{run_id}'",),
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
///
/// The workspace is canonicalized first and the canonicalized `.luther`/marker
/// paths are required to remain beneath the canonical workspace root, ruling
/// out redirection through any path component. Marker metadata is rechecked
/// around the content read to detect a path swap (e.g. replaced with a symlink)
/// that occurs between the type check and the read.
///
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
pub fn verify_workspace_ownership_marker(workspace: &Path, run_id: &str) -> Option<String> {
    // Reject a symlinked workspace root *before* canonicalization. Canonicalize
    // would silently resolve the symlink and accept the redirected root, so the
    // link itself must be inspected first.
    if let Some(reason) = reject_symlinked_workspace_root(workspace) {
        return Some(reason);
    }
    // Canonicalize the workspace so subsequent containment checks are proof
    // against symlink redirection at any path component.
    let canonical_workspace = match workspace.canonicalize() {
        Ok(path) => path,
        Err(err) => {
            return Some(format!("workspace cannot be canonicalized: {err}"));
        }
    };
    // Revalidate the workspace root identity after canonicalization to detect a
    // TOCTOU swap (e.g. the root was replaced with a symlink between the initial
    // check and now). The canonicalized path is a real directory at this point.
    if let Some(reason) = revalidate_workspace_root_identity(workspace, &canonical_workspace) {
        return Some(reason);
    }
    if let Some(reason) = reject_symlinked_luther_parent(&canonical_workspace) {
        return Some(reason);
    }
    let marker = canonical_workspace.join(".luther").join("workspace-owner");
    verify_marker_file(&marker, run_id, &canonical_workspace)
}

/// Verify the marker file at `marker`: existence, regular-file type, containment
/// beneath `workspace_root`, content, and tamper-resistant metadata around the
/// read. Returns `None` when trusted or `Some(reason)` explaining a rejection.
fn verify_marker_file(marker: &Path, run_id: &str, workspace_root: &Path) -> Option<String> {
    let meta_before = match std::fs::symlink_metadata(marker) {
        Ok(meta) => meta,
        Err(_) => {
            return Some(format!(
                "workspace ownership marker is missing: {marker_display}",
                marker_display = marker.display()
            ));
        }
    };
    if meta_before.file_type().is_symlink() {
        return Some(format!(
            "workspace ownership marker is a symlink and must be a regular file: {marker_display}",
            marker_display = marker.display()
        ));
    }
    if meta_before.is_dir() {
        return Some(format!(
            "workspace ownership marker is a directory and must be a regular file: {marker_display}",
            marker_display = marker.display()
        ));
    }
    // Containment: the canonicalized marker must stay beneath the canonical
    // workspace root, ruling out any redirection that escaped the earlier
    // component checks.
    if let Some(reason) = verify_marker_containment(marker, workspace_root) {
        return Some(reason);
    }
    let contents = match std::fs::read_to_string(marker) {
        Ok(contents) => contents,
        Err(err) => return Some(format!("workspace ownership marker is not readable: {err}")),
    };
    // Recheck metadata around the read to detect a path swap (e.g. replaced
    // with a symlink) that occurred between the type check and the content
    // read. Fail closed on any change.
    if let Some(reason) = recheck_marker_metadata(marker, &meta_before) {
        return Some(reason);
    }
    evaluate_marker_contents(&contents, run_id, marker)
}

/// Ensure the canonicalized marker remains beneath the canonical workspace
/// root. Both operands are canonical (absolute, symlink-resolved, no `..`), so
/// `starts_with` is a valid containment check.
fn verify_marker_containment(marker: &Path, workspace_root: &Path) -> Option<String> {
    match marker.canonicalize() {
        Ok(canonical_marker) if !canonical_marker.starts_with(workspace_root) => Some(format!(
            "workspace ownership marker escapes the workspace root: {marker_display}",
            marker_display = marker.display()
        )),
        Ok(_) => None,
        Err(err) => Some(format!(
            "workspace ownership marker cannot be canonicalized: {err}"
        )),
    }
}

/// Re-fetch marker metadata after the content read and reject any change that
/// indicates the path was swapped between the pre-read type check and now.
fn recheck_marker_metadata(marker: &Path, meta_before: &std::fs::Metadata) -> Option<String> {
    let meta_after = match std::fs::symlink_metadata(marker) {
        Ok(meta) => meta,
        Err(err) => {
            return Some(format!(
                "workspace ownership marker vanished during verification: {err}"
            ));
        }
    };
    if meta_after.file_type().is_symlink() {
        return Some("workspace ownership marker became a symlink during verification".to_string());
    }
    if meta_after.is_dir() {
        return Some(
            "workspace ownership marker became a directory during verification".to_string(),
        );
    }
    if marker_identity_changed(meta_before, &meta_after) {
        return Some("workspace ownership marker identity changed during verification".to_string());
    }
    None
}

/// Compare two metadata snapshots for the same path to detect an inode swap.
/// On Unix the device+inode pair uniquely identifies the file; elsewhere we
/// fall back to length and mtime as a best-effort identity.
fn marker_identity_changed(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        before.dev() != after.dev() || before.ino() != after.ino()
    }
    #[cfg(not(unix))]
    {
        before.len() != after.len() || before.modified().ok() != after.modified().ok()
    }
}

/// Revalidate the workspace root identity after canonicalization: re-check the
/// original path is not a symlink or non-directory (closing the TOCTOU window
/// between the initial `reject_symlinked_workspace_root` and the canonicalize
/// call) and confirm the observed path resolves to the same inode as the
/// canonical root. This detects a swap of the workspace root for a symlink or a
/// different directory that occurred between the initial check and now. Fails
/// closed on any change.
///
/// The observed path is inspected with `symlink_metadata` so a TOCTOU swap to a
/// symlink is observed on this exact snapshot rather than being silently
/// followed to its target by `metadata`. Critically, a symlink that resolves
/// back to the canonical root would be followed by `metadata` and compare equal
/// to the canonical root's inode — silently passing. Using `symlink_metadata`
/// observes the link itself, so the symlink is rejected before the identity
/// comparison.
pub(super) fn revalidate_workspace_root_identity(
    observed_workspace: &Path,
    canonical_workspace: &Path,
) -> Option<String> {
    // Snapshot the observed path with symlink_metadata so a TOCTOU swap to a
    // symlink (including a symlink that resolves back to the canonical root)
    // is observed as a symlink on this snapshot rather than followed to its
    // target by `metadata`. A symlink followed to the canonical root would
    // otherwise compare equal to the canonical root and silently pass.
    let observed_meta = match std::fs::symlink_metadata(observed_workspace) {
        Ok(meta) => meta,
        Err(err) => {
            return Some(format!(
                "workspace root became inaccessible during verification: {err}"
            ));
        }
    };
    if observed_meta.file_type().is_symlink() {
        return Some(format!(
            "workspace root became a symlink during verification: {observed_display}",
            observed_display = observed_workspace.display()
        ));
    }
    if !observed_meta.is_dir() {
        return Some(format!(
            "workspace root is not a directory during verification: {observed_display}",
            observed_display = observed_workspace.display()
        ));
    }
    let canonical_meta = match std::fs::symlink_metadata(canonical_workspace) {
        Ok(meta) => meta,
        Err(err) => {
            return Some(format!(
                "canonical workspace root became inaccessible during verification: {err}"
            ));
        }
    };
    if marker_identity_changed(&observed_meta, &canonical_meta) {
        return Some("workspace root identity changed during verification".to_string());
    }
    None
}

/// Evaluate marker contents: reject empty content and a foreign owner; trust an
/// exact-owner match.
fn evaluate_marker_contents(contents: &str, run_id: &str, marker: &Path) -> Option<String> {
    if contents.trim().is_empty() {
        return Some(format!(
            "workspace ownership marker is empty: {marker_display}",
            marker_display = marker.display()
        ));
    }
    if contents == run_id {
        None
    } else {
        Some(format!(
            "workspace ownership marker belongs to run '{contents}' not '{run_id}'"
        ))
    }
}
