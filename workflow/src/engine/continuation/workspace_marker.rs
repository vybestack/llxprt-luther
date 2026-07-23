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
pub(crate) fn reject_symlinked_workspace_root(workspace: &Path) -> Option<String> {
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
///
/// **Reject parent metadata errors:** a `NotFound` `.luther` is acceptable
/// (the directory has not been created yet), but any other inspection error
/// (e.g. `PermissionDenied`) must fail closed rather than be silently ignored.
/// Treating an unreadable `.luther` as "not a symlink" would allow a
/// fail-open bypass.
pub(crate) fn reject_symlinked_luther_parent(workspace: &Path) -> Option<String> {
    let luther = workspace.join(".luther");
    match std::fs::symlink_metadata(&luther) {
        Ok(meta) if meta.file_type().is_symlink() => Some(format!(
            "workspace `.luther` parent is a symlink and must be a real directory: {luther_display}",
            luther_display = luther.display()
        )),
        Ok(_) => None,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => Some(format!(
            "workspace `.luther` parent cannot be inspected: {error}: {luther_display}",
            luther_display = luther.display()
        )),
    }
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

/// Maximum marker size (matches the small run-id strings published). Path-based
/// marker reads are bounded to this size so a hostile or oversized marker
/// cannot exhaust memory or be streamed indefinitely.
const MAX_MARKER_BYTES: u64 = 4096;

/// Read a marker file as a UTF-8 string with a bounded size. Requires the path
/// to be a regular file (verified via `symlink_metadata`) of bounded size
/// *before* the read so a special file that `metadata.is_file()` misclassifies
/// (or an oversized marker) cannot be streamed unbounded. Returns the trimmed
/// string content.
///
/// On Unix the file is opened with `O_NONBLOCK` so a special file that slipped
/// past the regular-file check cannot block the read.
fn read_marker_bounded(marker: &Path) -> std::io::Result<String> {
    let meta = std::fs::symlink_metadata(marker)?;
    if meta.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "workspace ownership marker is a symlink: {marker_display}",
                marker_display = marker.display()
            ),
        ));
    }
    if !meta.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "workspace ownership marker is not a regular file: {marker_display}",
                marker_display = marker.display()
            ),
        ));
    }
    if meta.len() > MAX_MARKER_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "workspace ownership marker exceeds maximum size",
        ));
    }
    #[cfg(unix)]
    {
        use std::io::Read;
        use std::os::unix::fs::OpenOptionsExt;
        // Open with NONBLOCK so a special file misclassified as regular by
        // metadata cannot block the read. `read` on a regular file is
        // unaffected by NONBLOCK.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(marker)?;
        let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
        let reader = std::io::BufReader::new(file);
        reader.take(MAX_MARKER_BYTES + 1).read_to_end(&mut bytes)?;
        if u64::try_from(bytes.len()).map_or(true, |len| len > MAX_MARKER_BYTES) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "workspace ownership marker exceeds maximum size",
            ));
        }
        String::from_utf8(bytes).map_err(|err| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("workspace ownership marker is not valid UTF-8: {err}"),
            )
        })
    }
    #[cfg(not(unix))]
    {
        // Best-effort bounded read on non-Unix: rely on the pre-read size
        // bound from metadata and a capped read_to_end.
        use std::io::Read;
        let mut file = std::fs::File::open(marker)?;
        let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
        file.take(MAX_MARKER_BYTES + 1).read_to_end(&mut bytes)?;
        if u64::try_from(bytes.len()).map_or(true, |len| len > MAX_MARKER_BYTES) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "workspace ownership marker exceeds maximum size",
            ));
        }
        String::from_utf8(bytes).map_err(|err| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("workspace ownership marker is not valid UTF-8: {err}"),
            )
        })
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
    // Issue 158 finding 3: no auto-adopt of a pre-existing empty workspace.
    // Only an atomically created-by-this-launch directory, or a directory
    // carrying exact interrupted-publication evidence (a `.luther` subdir
    // containing only `.workspace-owner.tmp.*` temp files for this run), may
    // be first-claimed. A pre-existing empty directory created by some other
    // actor must NOT be silently adopted, because it carries no provenance
    // tying it to this launch.
    let created_by_this_launch = bootstrap_workspace_creation(workspace)?;
    if let Some(reason) = reject_symlinked_workspace_root(workspace) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            reason,
        ));
    }
    let marker = workspace_owner_marker_path(workspace);
    match resolve_existing_marker(&marker, run_id)? {
        ExistingMarker::Valid => return Ok(()),
        ExistingMarker::Absent => {}
    }
    // A directory created by this launch may always be first-claimed. A
    // pre-existing directory may only be claimed when it carries exact
    // interrupted-publication evidence; otherwise it is rejected outright.
    let claimable = created_by_this_launch
        || workspace_has_interrupted_publication_evidence(workspace, run_id)?;
    if !claimable {
        return resolve_unclaimable_workspace(workspace, &marker, run_id);
    }
    let canonical = workspace.canonicalize()?;
    let anchor = crate::engine::workspace_ownership::WorkspaceAnchor::open(&canonical)?;
    crate::engine::workspace_ownership::publish_bootstrap_via_anchor(&anchor, run_id)?;
    // Issue 158 descriptor retention: adjudicate the published bootstrap
    // marker through the SAME retained anchor descriptor rather than
    // re-opening the workspace path via `adjudicate_workspace_ownership`.
    // The anchor was opened before publication and is the exact descriptor
    // the publication wrote through; using a path-based re-open here would
    // re-introduce a TOCTOU window in which a concurrent attacker could swap
    // the workspace path between publication and adjudication.
    match crate::engine::workspace_ownership::snapshot_bootstrap_marker_via_anchor(&anchor, run_id)
    {
        crate::engine::workspace_ownership::AnchoredMarkerVerdict::Trusted => Ok(()),
        crate::engine::workspace_ownership::AnchoredMarkerVerdict::Absent => {
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "bootstrap owner publication produced no ownership evidence",
            ))
        }
        crate::engine::workspace_ownership::AnchoredMarkerVerdict::Rejected(reason) => {
            Err(std::io::Error::new(std::io::ErrorKind::InvalidData, reason))
        }
    }
}

/// Outcome of inspecting a marker path that may already exist.
enum ExistingMarker {
    /// The marker is present and records the exact same owner (idempotent).
    Valid,
    /// The marker path has no entry yet.
    Absent,
}

/// Atomically determine whether `workspace` was created by this launch.
///
/// `create_dir` (single-component) creates the final path component atomically;
/// `AlreadyExists` means the directory pre-existed and requires evidence.
/// `create_dir_all` would silently succeed on a pre-existing directory and lose
/// the creation signal, so the parent chain is created separately first when the
/// final component's parent does not yet exist.
fn bootstrap_workspace_creation(workspace: &Path) -> std::io::Result<bool> {
    match std::fs::create_dir(workspace) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // The parent chain does not exist yet; create it, then attempt the
            // atomic single-component create again so the final component's
            // creation is still observed.
            if let Some(parent) = workspace.parent() {
                std::fs::create_dir_all(parent)?;
            }
            match std::fs::create_dir(workspace) {
                Ok(()) => Ok(true),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
                Err(err) => Err(err),
            }
        }
        Err(error) => Err(error),
    }
}

/// Inspect a marker that may already exist. `Valid` means it records the exact
/// same owner (idempotent success); `Absent` means no entry exists yet. Any
/// other condition (foreign owner, symlink, malformed) is an error.
fn resolve_existing_marker(marker: &Path, run_id: &str) -> std::io::Result<ExistingMarker> {
    match std::fs::symlink_metadata(marker) {
        Ok(_) => {
            inspect_existing_marker(marker, run_id)?;
            Ok(ExistingMarker::Valid)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ExistingMarker::Absent),
        Err(error) => Err(error),
    }
}

/// Resolve a workspace that is not first-claimable: if a marker appeared between
/// the claimability check and now, validate it; otherwise reject the adoption.
fn resolve_unclaimable_workspace(
    workspace: &Path,
    marker: &Path,
    run_id: &str,
) -> std::io::Result<()> {
    if marker_exists(marker) {
        return inspect_existing_marker(marker, run_id);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!(
            "refusing to adopt pre-existing workspace without ownership marker or \
             interrupted-publication evidence: {}",
            workspace.display()
        ),
    ))
}

/// Whether a pre-existing workspace directory carries exact evidence that a
/// prior publication attempt by `run_id` was interrupted. The only accepted
/// evidence is a real `.luther` directory containing exclusively
/// `.workspace-owner.tmp.*` temp files whose content matches `run_id` (and no
/// final `workspace-owner` marker). An empty directory, a directory with any
/// other content, or a directory whose `.luther` has a foreign-owner temp file
/// is NOT claimable.
fn workspace_has_interrupted_publication_evidence(
    workspace: &Path,
    run_id: &str,
) -> std::io::Result<bool> {
    let mut found_evidence = false;
    for entry in std::fs::read_dir(workspace)? {
        let entry = entry?;
        if entry.file_name() == ".luther" {
            if !luther_dir_is_claimable(&entry.path(), run_id)? {
                return Ok(false);
            }
            // `.luther` exists and contains only same-run temp files: that is
            // the interrupted-publication signal.
            found_evidence = true;
        } else {
            // Any other entry means the workspace is not a bare interrupted
            // publication.
            return Ok(false);
        }
    }
    Ok(found_evidence)
}

fn luther_dir_is_claimable(path: &Path, run_id: &str) -> std::io::Result<bool> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Ok(false);
    }
    let mut found_temp = false;
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
        if read_marker_bounded(&entry.path())? != run_id {
            return Ok(false);
        }
        found_temp = true;
    }
    Ok(found_temp)
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
    let existing = read_marker_bounded(marker)?;
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
///
/// **Typed marker inspection:** only `NotFound` means the marker is absent.
/// `PermissionDenied` or any other inspection error is a rejection, never a
/// silent "absent", because treating an unreadable marker as missing would
/// allow a fail-open bypass of ownership verification (e.g. an attacker that
/// strips read permission on the marker could make verification report it as
/// missing).
pub(crate) fn verify_marker_file(
    marker: &Path,
    run_id: &str,
    workspace_root: &Path,
) -> Option<String> {
    let meta_before = match std::fs::symlink_metadata(marker) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Some(format!(
                "workspace ownership marker is missing: {marker_display}",
                marker_display = marker.display()
            ));
        }
        Err(error) => {
            // Any non-NotFound inspection error (PermissionDenied, Io, etc.)
            // must reject rather than be silently treated as "absent".
            return Some(format!(
                "workspace ownership marker cannot be inspected: {error}: {marker_display}",
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
    if !meta_before.is_file() {
        return Some(format!(
            "workspace ownership marker is not a regular file: {marker_display}",
            marker_display = marker.display()
        ));
    }
    // Containment: the canonicalized marker must stay beneath the canonical
    // workspace root, ruling out any redirection that escaped the earlier
    // component checks.
    if let Some(reason) = verify_marker_containment(marker, workspace_root) {
        return Some(reason);
    }
    let contents = match read_marker_bounded(marker) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Some(format!(
                "workspace ownership marker vanished during verification: {marker_display}",
                marker_display = marker.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
            // A size/UTF-8 failure is a categorical rejection (never raw
            // diagnostics) so a bounded-oversized or non-UTF-8 marker fails
            // closed.
            return Some(format!("workspace ownership marker is invalid: {error}"));
        }
        Err(error) => {
            // A read error other than NotFound (e.g. PermissionDenied) is a
            // rejection, never silently "absent".
            return Some(format!(
                "workspace ownership marker is not readable: {error}: {marker_display}",
                marker_display = marker.display()
            ));
        }
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
    if !meta_after.is_file() {
        return Some(
            "workspace ownership marker became a non-regular file during verification".to_string(),
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
pub(crate) fn revalidate_workspace_root_identity(
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
