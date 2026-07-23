//! Descriptor-anchored, no-follow publication of the durable ownership marker.
//!
//! The durable marker lives at `.git/luther/workspace-owner`. To close the
//! TOCTOU window around its publication, every filesystem operation is anchored
//! to a directory descriptor opened with `O_NOFOLLOW`:
//!
//! 1. Open `.git` as a real directory (`O_DIRECTORY | O_NOFOLLOW`).
//! 2. Open or create `.git/luther` relative to that descriptor.
//! 3. Open the verified bootstrap marker relative to the workspace root and
//!    read its bytes from the opened descriptor (the bytes are tied to the
//!    opened file, not a later path re-resolution).
//! 4. Publish the durable marker via temp + atomic `linkat` (no replace) +
//!    fsync, all relative to the `.git/luther` descriptor.
//! 5. Re-validate the published marker by opening it again relative to the
//!    `.git/luther` descriptor and reading from that descriptor.
//!
//! This mirrors the established
//! `pr_followup_artifacts::path_safety` descriptor-anchored publication
//! pattern. Only Unix is supported; non-Unix targets fall back to the
//! best-effort `std::fs` path because they lack the required `openat` /
//! `O_NOFOLLOW` semantics.

use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::Path;

use rustix::fs::{fstat, fsync, linkat, mkdirat, open, openat, statat, AtFlags, Mode, OFlags};
use rustix::io::Errno;

// Cohesive submodules. The anchor holds the `O_NOFOLLOW` workspace descriptor
// and its captured dev/inode identity; tests are split out to keep this file
// under the file-size gate. Re-exports keep the public `pub(super)` surface
// accessible at `durable_publication::` for the parent module.
mod anchor;
#[cfg(test)]
mod tests;

pub(crate) use anchor::{configure_fchdir_pre_exec, FileIdentity, WorkspaceAnchor};

/// Open `.git` beneath `workspace` as a real directory descriptor, rejecting a
/// symlink or non-directory. The descriptor anchors every subsequent
/// `.git/luther` operation so a TOCTOU swap of `.git` cannot redirect the
/// durable marker.
///
/// Retained for direct path-based test scaffolding. Production promotion uses
/// [`open_git_directory_relative`] to stay anchored to the workspace fd.
#[cfg(test)]
pub(super) fn open_git_directory(workspace: &Path) -> std::io::Result<OwnedFd> {
    let git = workspace.join(".git");
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    open(&git, flags, Mode::empty()).map_err(rustix_error("open .git directory", &git))
}

/// Open `.git` relative to the already-open workspace descriptor, rejecting a
/// symlink or non-directory. Anchoring to the workspace fd means a TOCTOU swap
/// of the workspace path cannot redirect the `.git` resolution: the descriptor
/// was opened and verified by `open_workspace_directory` and the `.git` entry
/// is resolved relative to that exact directory inode.
pub(super) fn open_git_directory_relative(
    workspace_fd: BorrowedFd<'_>,
) -> std::io::Result<OwnedFd> {
    let name = CString::new(".git").expect("static C string");
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    openat(workspace_fd, &name, flags, Mode::empty()).map_err(rustix_error(
        "open .git directory relative to workspace",
        Path::new(".git"),
    ))
}

/// Open or create `.git/luther` relative to the `.git` descriptor, returning a
/// real-directory descriptor that anchors the durable marker publication.
/// Rejects a symlink or non-directory both before and after creation.
pub(super) fn open_or_create_luther_directory(git_fd: BorrowedFd<'_>) -> std::io::Result<OwnedFd> {
    const LUTHER: &str = "luther";
    let name = CString::new(LUTHER).expect("static C string");
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    match openat(git_fd, &name, flags, Mode::empty()) {
        Ok(fd) => Ok(fd),
        Err(Errno::NOENT) => {
            // Create then re-open with the same no-follow directory flags so a
            // concurrent swap to a symlink between create and open is rejected.
            match mkdirat(git_fd, &name, Mode::from_raw_mode(0o755)) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(error) => {
                    return Err(io_error(
                        "create .git/luther directory",
                        &Path::new(".git").join(LUTHER),
                        error,
                    ));
                }
            }
            fsync(git_fd).map_err(rustix_error("sync .git directory", Path::new(".git")))?;
            openat(git_fd, &name, flags, Mode::empty()).map_err(rustix_error(
                "open .git/luther directory after creation",
                &Path::new(".git").join(LUTHER),
            ))
        }
        Err(error) => Err(rustix_error(
            "open .git/luther directory",
            &Path::new(".git").join(LUTHER),
        )(error)),
    }
}

/// Read the exact bytes of the bootstrap marker by opening it relative to the
/// workspace descriptor. The returned bytes are tied to the opened file, not a
/// later path re-resolution. Rejects a symlink, directory, or non-regular file
/// before the read, and requires a regular file of bounded size (via `fstat`
/// on the actual opened descriptor) before reading.
pub(super) fn read_bootstrap_bytes(workspace_fd: BorrowedFd<'_>) -> std::io::Result<Vec<u8>> {
    // Open via two anchored hops: first open .luther relative to workspace,
    // then workspace-owner relative to .luther. This keeps every hop anchored
    // to a no-follow descriptor.
    let luther_name = CString::new(".luther").expect("static C string");
    let owner_name = CString::new("workspace-owner").expect("static C string");
    let luther_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let luther_fd = openat(workspace_fd, &luther_name, luther_flags, Mode::empty())
        .map_err(rustix_error("open .luther directory", Path::new(".luther")))?;
    require_regular_at(
        luther_fd.as_fd(),
        &owner_name,
        Path::new(".luther/workspace-owner"),
    )?;
    // Open the bootstrap marker with NONBLOCK so a misclassified non-regular
    // file (e.g. a FIFO or a device the type check missed) cannot block the
    // read. NOFOLLOW rejects a symlink swap at this hop. The descriptor is then
    // fstat'd to confirm the *actual opened fd* is a regular file of bounded
    // size before any read.
    let read_flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let fd =
        openat(luther_fd.as_fd(), &owner_name, read_flags, Mode::empty()).map_err(rustix_error(
            "open bootstrap marker",
            Path::new(".luther/workspace-owner"),
        ))?;
    read_owned_descriptor(fd, Path::new(".luther/workspace-owner"))
}

/// Open or create the `.luther` directory relative to the workspace
/// descriptor, returning a real-directory descriptor that anchors the
/// bootstrap marker publication. Rejects a symlink or non-directory both
/// before and after creation, mirroring [`open_or_create_luther_directory`]
/// for the `.git/luther` case.
pub(super) fn open_or_create_bootstrap_directory(
    workspace_fd: BorrowedFd<'_>,
) -> std::io::Result<OwnedFd> {
    const LUTHER: &str = ".luther";
    let name = CString::new(LUTHER).expect("static C string");
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    match openat(workspace_fd, &name, flags, Mode::empty()) {
        Ok(fd) => Ok(fd),
        Err(Errno::NOENT) => {
            match mkdirat(workspace_fd, &name, Mode::from_raw_mode(0o755)) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(error) => {
                    return Err(io_error(
                        "create .luther directory",
                        Path::new(LUTHER),
                        error,
                    ));
                }
            }
            fsync(workspace_fd)
                .map_err(rustix_error("sync workspace directory", Path::new(".")))?;
            openat(workspace_fd, &name, flags, Mode::empty()).map_err(rustix_error(
                "open .luther directory after creation",
                Path::new(LUTHER),
            ))
        }
        Err(error) => Err(rustix_error("open .luther directory", Path::new(LUTHER))(
            error,
        )),
    }
}

/// Publish `bytes` as the bootstrap marker (`.luther/workspace-owner`)
/// relative to the workspace descriptor using the same crash-safe
/// descriptor-anchored publication pattern as the durable marker:
/// `open_or_create_bootstrap_directory` + `openat` temp (CREATE|EXCL|NOFOLLOW)
/// + `write_all` + `sync_all` + `linkat` (no replace) + `fsync`.
///
/// A concurrent winner is detected via `linkat`'s atomic `AlreadyExists` and reported.
///
/// This makes the bootstrap publication descriptor-relative end-to-end: the
/// exact created workspace fd is retained and all operations are relative to
/// it, so no path-based trust decision exists.
pub(crate) fn publish_bootstrap_marker(
    workspace_fd: BorrowedFd<'_>,
    bytes: &[u8],
) -> std::io::Result<()> {
    let luther_fd = open_or_create_bootstrap_directory(workspace_fd)?;
    publish_marker_relative(
        luther_fd.as_fd(),
        bytes,
        Path::new(".luther/workspace-owner"),
    )
}

/// Publish `bytes` as the durable marker relative to the `.git/luther`
/// descriptor via temp + atomic `linkat` (no replace) + fsync. The marker name
/// is `workspace-owner`. A concurrent winner is detected via `linkat`'s atomic
/// `AlreadyExists` and reported so the caller can validate the existing
/// content.
pub(super) fn publish_durable_marker(
    luther_fd: BorrowedFd<'_>,
    bytes: &[u8],
) -> std::io::Result<DurablePublication> {
    publish_marker_relative(luther_fd, bytes, Path::new(".git/luther/workspace-owner"))?;
    Ok(DurablePublication)
}

/// Shared crash-safe publication of a marker file relative to a directory
/// descriptor. Uses `openat` (CREATE|EXCL|NOFOLLOW) + `write_all` +
/// `sync_all` + `linkat` (no replace) + `fsync` so the marker is published
/// atomically and durably without any path-based trust decision.
///
/// A concurrent winner (`linkat` returns `AlreadyExists`) is surfaced as
/// `ErrorKind::AlreadyExists` so the caller can validate the existing content.
fn publish_marker_relative(
    dir_fd: BorrowedFd<'_>,
    bytes: &[u8],
    display: &Path,
) -> std::io::Result<()> {
    let temp_name = format!(".workspace-owner.tmp.{}", uuid::Uuid::new_v4().simple());
    let temp_c = CString::new(temp_name.clone()).map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("temp name: {err}"),
        )
    })?;
    let final_name = CString::new("workspace-owner").expect("static C string");
    let create_flags =
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd = match openat(dir_fd, &temp_c, create_flags, Mode::from_raw_mode(0o600)) {
        Ok(fd) => fd,
        Err(error) => {
            return Err(io_error(
                "create marker temp file",
                &display.with_file_name(&temp_name),
                error,
            ));
        }
    };
    let result: std::io::Result<()> = (|| {
        let mut file = File::from(fd);
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        let owned = file
            .try_clone()
            .map_err(|err| std::io::Error::other(format!("clone marker temp fd: {err}")))?;
        validate_descriptor_regular(&owned, bytes.len())?;
        match linkat(dir_fd, &temp_c, dir_fd, &final_name, AtFlags::empty()) {
            Ok(()) => {}
            Err(Errno::EXIST) => {
                let _ = rustix::fs::unlinkat(dir_fd, &temp_c, AtFlags::empty());
                return Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists));
            }
            Err(error) => {
                return Err(io_error("link marker without replacement", display, error));
            }
        }
        fsync(dir_fd).map_err(rustix_error("sync marker directory", display))?;
        let _ = rustix::fs::unlinkat(dir_fd, &temp_c, AtFlags::empty());
        Ok(())
    })();
    if let Err(err) = result {
        let _ = rustix::fs::unlinkat(dir_fd, &temp_c, AtFlags::empty());
        return Err(err);
    }
    Ok(())
}

/// Outcome token indicating the durable marker was published from the provided
/// bytes. Exists so callers cannot confuse "bytes published" with "existing
/// content validated".
#[derive(Debug)]
pub(super) struct DurablePublication;

/// Open the durable marker relative to the `.git/luther` descriptor and read
/// its bytes from the descriptor. Rejects a symlink, directory, or non-regular
/// file before and after the read, and requires a regular file of bounded size
/// via `fstat` on the actual opened descriptor before reading. Used to
/// re-validate the published bytes.
pub(super) fn read_durable_marker_bytes(luther_fd: BorrowedFd<'_>) -> std::io::Result<Vec<u8>> {
    let name = CString::new("workspace-owner").expect("static C string");
    require_regular_at(luther_fd, &name, Path::new(".git/luther/workspace-owner"))?;
    let read_flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let fd = openat(luther_fd, &name, read_flags, Mode::empty()).map_err(rustix_error(
        "open durable marker for re-validation",
        Path::new(".git/luther/workspace-owner"),
    ))?;
    read_owned_descriptor(fd, Path::new(".git/luther/workspace-owner"))
}

/// The expected bootstrap bytes for the owner run id are the exact run-id bytes
/// published by [`provision_workspace_owner_marker`](super::super::provision_workspace_owner_marker).
/// The durable marker is the exact same content, so the expected bytes are the
/// run-id bytes themselves (no trailing newline, no extra framing).
fn expected_bootstrap_bytes(run_id: &str) -> Vec<u8> {
    run_id.as_bytes().to_vec()
}

/// Promote the verified bootstrap marker bytes to the durable path using
/// descriptor-anchored, no-follow operations. The caller must have already
/// verified (via `verify_workspace_ownership`) that the bootstrap marker is a
/// valid exact-owner regular file.
///
/// This is the path-based entry point that opens its own [`WorkspaceAnchor`].
/// Production callers retain a single anchor through verify and promotion via
/// [`promote_via_anchor`]; this entry point is retained for direct path-based
/// test scaffolding.
#[cfg(test)]
pub(super) fn promote_via_descriptor(workspace: &Path, run_id: &str) -> std::io::Result<()> {
    let anchor = WorkspaceAnchor::open(workspace)?;
    promote_via_anchor(&anchor, run_id)
}

/// Promote the verified bootstrap marker bytes to the durable path using an
/// **already-open** [`WorkspaceAnchor`], so the same descriptor retained
/// through verify and durable inspection also anchors promotion. No reopen of
/// the workspace path occurs, closing the TOCTOU window in which a concurrent
/// attacker could swap the workspace path between verify and promotion.
///
/// The caller must have already verified (via `verify_workspace_ownership`)
/// that the bootstrap marker is a valid exact-owner regular file, and must
/// have constructed the anchor from the canonicalized workspace path. The
/// anchor's identity was captured at construction; promotion operates entirely
/// relative to that descriptor.
pub(super) fn promote_via_anchor(anchor: &WorkspaceAnchor, run_id: &str) -> std::io::Result<()> {
    let expected = expected_bootstrap_bytes(run_id);
    let bootstrap_bytes = read_bootstrap_bytes(anchor.as_fd())?;
    // Compare the descriptor-read bootstrap bytes exactly to the expected
    // run-id bytes. This closes the gap in which a foreign or tampered
    // bootstrap file could be promoted solely because some entry existed.
    if bootstrap_bytes != expected {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "bootstrap marker bytes do not match expected run id",
        ));
    }
    // Open `.git` relative to the already-open workspace descriptor so the
    // promotion chain stays anchored to the verified workspace fd and cannot
    // be redirected by a TOCTOU swap of the workspace path.
    let git_fd = open_git_directory_relative(anchor.as_fd())?;
    let luther_fd = open_or_create_luther_directory(git_fd.as_fd())?;
    match publish_durable_marker(luther_fd.as_fd(), &bootstrap_bytes) {
        Ok(DurablePublication) => {}
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            // Concurrent winner: validate the existing content matches the
            // expected bootstrap bytes so idempotent promotion succeeds only
            // for an exact same-owner durable record.
            let existing = read_durable_marker_bytes(luther_fd.as_fd())?;
            if existing != expected {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "concurrent durable marker does not match bootstrap evidence",
                ));
            }
        }
        Err(err) => return Err(err),
    }
    // Re-read the published durable marker and confirm byte-for-byte identity
    // with the expected bootstrap bytes that were promoted.
    let durable_bytes = read_durable_marker_bytes(luther_fd.as_fd())?;
    if durable_bytes != expected {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "promoted durable marker bytes do not match bootstrap evidence",
        ));
    }
    Ok(())
}

/// Open the workspace root as a directory descriptor with `O_NOFOLLOW`,
/// rejecting a symlink root.
fn open_workspace_directory(workspace: &Path) -> std::io::Result<OwnedFd> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    open(workspace, flags, Mode::empty())
        .map_err(rustix_error("open workspace directory", workspace))
}

/// Maximum marker size (matches the small run-id strings published).
const MAX_MARKER_BYTES: u64 = 4096;

fn read_fd_to_end(fd: OwnedFd, max_bytes: u64) -> std::io::Result<Vec<u8>> {
    let mut file = File::from(fd);
    let mut bytes = Vec::new();
    std::io::Read::take(&mut file, max_bytes + 1).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_or(true, |len| len > max_bytes) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "workspace ownership marker exceeds maximum size",
        ));
    }
    Ok(bytes)
}

/// Read bytes from an already-opened descriptor, requiring via `fstat` on the
/// *actual opened fd* that it is a regular file of bounded size before any
/// read. This closes the gap in which a path-based `statat` could be satisfied
/// by one inode while the descriptor the read consumes belongs to a different
/// (e.g. non-regular or oversized) inode. The descriptor must already be open
/// with `NOFOLLOW` and (where the caller opened it for a blocking-sensitive
/// path) `NONBLOCK`.
fn read_owned_descriptor(fd: OwnedFd, display: &Path) -> std::io::Result<Vec<u8>> {
    // fstat the actual opened descriptor (not the path) so the read is bound
    // to the exact inode we hold open.
    let metadata = fstat(fd.as_fd()).map_err(rustix_error("stat owned descriptor", display))?;
    let mode = metadata.st_mode;
    if (mode & libc::S_IFMT) != libc::S_IFREG {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "workspace ownership marker is not a regular file: {} (mode {mode:o})",
                display.display()
            ),
        ));
    }
    let size = u64::try_from(metadata.st_size).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "workspace ownership marker size exceeds supported range",
        )
    })?;
    if size > MAX_MARKER_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "workspace ownership marker exceeds maximum size",
        ));
    }
    read_fd_to_end(fd, MAX_MARKER_BYTES)
}

/// Require the entry named `name` beneath `dir` is a regular file (not a
/// symlink, directory, FIFO, socket, device, etc.) using `statat` with
/// `SYMLINK_NOFOLLOW`. This closes the non-regular-file hole (FIFOs, sockets,
/// devices) that a plain `is_file` check would miss.
fn require_regular_at(dir: BorrowedFd<'_>, name: &CString, display: &Path) -> std::io::Result<()> {
    let metadata = statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(rustix_error("stat marker", display))?;
    let mode = metadata.st_mode;
    let file_type = mode & libc::S_IFMT;
    if file_type != libc::S_IFREG {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "workspace ownership marker is not a regular file: {} (mode {mode:o})",
                display.display()
            ),
        ));
    }
    Ok(())
}

/// Validate that an owned descriptor refers to a regular file of the exact
/// expected size, defending against a swap to a non-regular file between the
/// temp write and the link.
fn validate_descriptor_regular(file: &File, expected_len: usize) -> std::io::Result<()> {
    let metadata = fstat(file).map_err(rustix_error(
        "stat durable temp descriptor",
        Path::new(".git/luther/.workspace-owner.tmp"),
    ))?;
    let mode = metadata.st_mode;
    if (mode & libc::S_IFMT) != libc::S_IFREG {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "durable marker temp file is not a regular file",
        ));
    }
    if metadata.st_size
        != i64::try_from(expected_len).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "durable marker temp file size exceeds supported range",
            )
        })?
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "durable marker temp file size changed after write",
        ));
    }
    Ok(())
}

fn rustix_error<'a>(action: &'a str, display: &'a Path) -> impl Fn(Errno) -> std::io::Error + 'a {
    move |error: Errno| io_error(action, display, error)
}

fn io_error(action: &str, display: &Path, error: Errno) -> std::io::Error {
    let raw = std::io::Error::from(error);
    std::io::Error::new(
        raw.kind(),
        format!("{action} {}: {error}", display.display()),
    )
}

// ===========================================================================
// Cohesive typed anchored verification (single snapshot/verdict)
// ===========================================================================
//
// The functions below replace the path-based classify-then-reread verification
// with a single descriptor-anchored snapshot. The read-only verification path
// (`verify_workspace_ownership`) used by cleanup/continuation/scope/precheck/
// resume consults exactly one typed verdict per marker, produced by opening
// the marker relative to a no-follow directory descriptor, `fstat`-ing the
// *actual opened fd*, requiring a regular file of bounded size, and reading
// the exact bytes — all from one opened descriptor. There is no separate
// classify step that a TOCTOU swap could invalidate between classification and
// the content read, and no boolean-exists success: an existing marker is only
// trusted when its descriptor-anchored exact bytes match the expected owner.

/// Cohesive typed verdict for a single descriptor-anchored marker snapshot.
///
/// Produced by [`snapshot_marker`] from one opened descriptor. Only
/// [`AnchoredMarkerVerdict::Trusted`] means the workspace is owned; every other
/// variant is a fail-closed rejection so a `PermissionDenied` or an
/// uninspectable marker is never silently treated as absent or trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AnchoredMarkerVerdict {
    /// The marker path has no entry (`NotFound`).
    Absent,
    /// The marker is a regular file of bounded size whose exact bytes match
    /// the expected owner run id.
    Trusted,
    /// The marker is present but must be rejected: symlink, directory,
    /// non-regular file (FIFO/socket/device), oversized, foreign owner, empty,
    /// or uninspectable (`PermissionDenied`/other error). The reason string is
    /// a bounded categorical message (never raw diagnostics).
    Rejected(String),
}

impl AnchoredMarkerVerdict {
    /// Whether this verdict represents a fatal rejection.
    #[must_use]
    pub(super) fn is_rejected(&self) -> bool {
        matches!(self, Self::Rejected(_))
    }
}

#[derive(Debug, Clone, Copy)]
enum Rejection {
    Symlink,
    Directory,
    NonRegular,
    Oversized,
    Empty,
    Uninspectable,
}

impl Rejection {
    fn message(self, display: &Path) -> String {
        match self {
            Self::Symlink => format!(
                "workspace ownership marker is a symlink and must be a regular file: {}",
                display.display()
            ),
            Self::Directory => format!(
                "workspace ownership marker is a directory and must be a regular file: {}",
                display.display()
            ),
            Self::NonRegular => format!(
                "workspace ownership marker is not a regular file: {}",
                display.display()
            ),
            Self::Oversized => "workspace ownership marker exceeds maximum size".to_string(),
            Self::Empty => format!("workspace ownership marker is empty: {}", display.display()),
            Self::Uninspectable => format!(
                "workspace ownership marker cannot be inspected: {}",
                display.display()
            ),
        }
    }
}

/// Produce a single descriptor-anchored snapshot verdict for the bootstrap
/// marker (`.luther/workspace-owner`) relative to the workspace descriptor.
///
/// Opens `.luther` relative to `workspace_fd` with `O_DIRECTORY | O_NOFOLLOW`,
/// then opens `workspace-owner` relative to that descriptor with
/// `O_NOFOLLOW | O_NONBLOCK`, `fstat`s the *actual opened fd*, requires a
/// regular file of bounded size, reads the exact bytes, and compares them
/// exactly to `expected_run_id`. Exactly one [`AnchoredMarkerVerdict`] is
/// produced: there is no separate classify step that a TOCTOU swap could
/// invalidate between classification and the content read.
///
/// `NotFound` at either hop maps to [`AnchoredMarkerVerdict::Absent`]; every
/// other inspection error maps to [`AnchoredMarkerVerdict::Rejected`].
pub(super) fn snapshot_bootstrap_marker(
    workspace_fd: BorrowedFd<'_>,
    expected_run_id: &str,
) -> AnchoredMarkerVerdict {
    snapshot_marker_two_hops(
        workspace_fd,
        ".luther",
        "workspace-owner",
        Path::new(".luther/workspace-owner"),
        expected_run_id,
    )
}

/// Produce a single descriptor-anchored snapshot verdict for the durable
/// marker (`.git/luther/workspace-owner`) relative to the workspace
/// descriptor.
///
/// Opens `.git` and `.git/luther` as no-follow directory descriptors relative
/// to the workspace descriptor, then opens `workspace-owner` relative to the
/// `.git/luther` descriptor with `O_NOFOLLOW | O_NONBLOCK`, `fstat`s the
/// *actual opened fd*, requires a regular file of bounded size, reads the
/// exact bytes, and compares them exactly to `expected_run_id`.
///
/// `NotFound` at any hop maps to [`AnchoredMarkerVerdict::Absent`]; every
/// other inspection error maps to [`AnchoredMarkerVerdict::Rejected`]. A
/// symlink or non-directory `.git` or `.git/luther` is a hard rejection.
pub(super) fn snapshot_durable_marker(
    workspace_fd: BorrowedFd<'_>,
    expected_run_id: &str,
) -> AnchoredMarkerVerdict {
    let git_fd = match open_directory_relative(workspace_fd, ".git", Path::new(".git")) {
        DirectoryOpen::Opened(fd) => fd,
        DirectoryOpen::NotFound => return AnchoredMarkerVerdict::Absent,
        DirectoryOpen::Rejected(reason) => return AnchoredMarkerVerdict::Rejected(reason),
    };
    let luther_fd =
        match open_directory_relative(git_fd.as_fd(), "luther", Path::new(".git/luther")) {
            DirectoryOpen::Opened(fd) => fd,
            DirectoryOpen::NotFound => return AnchoredMarkerVerdict::Absent,
            DirectoryOpen::Rejected(reason) => return AnchoredMarkerVerdict::Rejected(reason),
        };
    snapshot_marker_one_hop(
        luther_fd.as_fd(),
        "workspace-owner",
        Path::new(".git/luther/workspace-owner"),
        expected_run_id,
    )
}

/// Outcome of opening a directory component relative to a parent descriptor.
enum DirectoryOpen {
    Opened(OwnedFd),
    NotFound,
    Rejected(String),
}

/// Open `name` relative to `parent_fd` as a real directory descriptor with
/// `O_DIRECTORY | O_NOFOLLOW`. `NotFound` maps to [`DirectoryOpen::NotFound`];
/// a symlink or non-directory maps to [`DirectoryOpen::Rejected`]; every other
/// error maps to [`DirectoryOpen::Rejected`] with an uninspectable message.
fn open_directory_relative(parent_fd: BorrowedFd<'_>, name: &str, display: &Path) -> DirectoryOpen {
    let c_name = match CString::new(name) {
        Ok(c) => c,
        Err(_) => return DirectoryOpen::Rejected(Rejection::Uninspectable.message(display)),
    };
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    match openat(parent_fd, &c_name, flags, Mode::empty()) {
        Ok(fd) => DirectoryOpen::Opened(fd),
        Err(Errno::NOENT) => DirectoryOpen::NotFound,
        Err(error) => {
            let raw = std::io::Error::from(error);
            if raw.kind() == std::io::ErrorKind::NotFound {
                DirectoryOpen::NotFound
            } else {
                DirectoryOpen::Rejected(format!(
                    "workspace directory cannot be inspected: {}: {error}: {}",
                    display.display(),
                    raw
                ))
            }
        }
    }
}

/// Snapshot a marker reached via two anchored hops (`parent_name` then
/// `leaf_name`) relative to `workspace_fd`.
fn snapshot_marker_two_hops(
    workspace_fd: BorrowedFd<'_>,
    parent_name: &str,
    leaf_name: &str,
    display: &Path,
    expected_run_id: &str,
) -> AnchoredMarkerVerdict {
    let parent_display = display
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| display.to_path_buf());
    let parent_fd = match open_directory_relative(workspace_fd, parent_name, &parent_display) {
        DirectoryOpen::Opened(fd) => fd,
        DirectoryOpen::NotFound => return AnchoredMarkerVerdict::Absent,
        DirectoryOpen::Rejected(reason) => return AnchoredMarkerVerdict::Rejected(reason),
    };
    snapshot_marker_one_hop(parent_fd.as_fd(), leaf_name, display, expected_run_id)
}

/// Snapshot a marker (`leaf_name`) relative to its parent directory
/// descriptor, producing one cohesive verdict.
fn snapshot_marker_one_hop(
    parent_fd: BorrowedFd<'_>,
    leaf_name: &str,
    display: &Path,
    expected_run_id: &str,
) -> AnchoredMarkerVerdict {
    let Some(fd) = open_marker_descriptor(parent_fd, leaf_name, display) else {
        return AnchoredMarkerVerdict::Absent;
    };
    match fd {
        OpenedMarker::Fd(fd) => classify_opened_marker(fd, display, expected_run_id),
        OpenedMarker::Rejected(reason) => AnchoredMarkerVerdict::Rejected(reason),
    }
}

/// Outcome of opening a marker relative to its parent descriptor. `None` means
/// the marker is absent (`NotFound`); `Some(Fd)` is an opened descriptor to
/// classify; `Some(Rejected)` is an open-phase rejection (symlink or open error).
enum OpenedMarker {
    Fd(OwnedFd),
    Rejected(String),
}

/// Open `leaf_name` relative to `parent_fd` with `O_NOFOLLOW | O_NONBLOCK`.
/// Returns `None` for `NotFound`, `Some(Fd)` for a successful open, or
/// `Some(Rejected)` for a symlink (`ELOOP`) or any other open error.
fn open_marker_descriptor(
    parent_fd: BorrowedFd<'_>,
    leaf_name: &str,
    display: &Path,
) -> Option<OpenedMarker> {
    let c_name = CString::new(leaf_name)
        .map_err(|_| Rejection::Uninspectable.message(display))
        .ok()?;
    let read_flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    match openat(parent_fd, &c_name, read_flags, Mode::empty()) {
        Ok(fd) => Some(OpenedMarker::Fd(fd)),
        Err(Errno::NOENT) => None,
        Err(Errno::LOOP) => {
            // O_NOFOLLOW rejects a symlinked final component with ELOOP. This
            // is the single-point symlink detection: the marker was a symlink
            // at the moment we tried to open it relative to its parent
            // descriptor.
            Some(OpenedMarker::Rejected(Rejection::Symlink.message(display)))
        }
        Err(error) => {
            let raw = std::io::Error::from(error);
            if raw.kind() == std::io::ErrorKind::NotFound {
                None
            } else {
                Some(OpenedMarker::Rejected(format!(
                    "workspace ownership marker cannot be opened: {}: {error}: {}",
                    display.display(),
                    raw
                )))
            }
        }
    }
}

/// Classify an already-opened marker descriptor: fstat the *actual opened fd*,
/// require a regular file of bounded size, read the exact bytes, and compare
/// them to `expected_run_id`. The verdict is bound to the exact inode held
/// open, not a later path re-resolution.
fn classify_opened_marker(
    fd: OwnedFd,
    display: &Path,
    expected_run_id: &str,
) -> AnchoredMarkerVerdict {
    // fstat the actual opened descriptor so the verdict is bound to the exact
    // inode we hold open, not a later path re-resolution.
    let metadata = match fstat(fd.as_fd()) {
        Ok(meta) => meta,
        Err(error) => {
            return AnchoredMarkerVerdict::Rejected(format!(
                "workspace ownership marker cannot be stat-ed: {}: {error}",
                display.display()
            ));
        }
    };
    let mode = metadata.st_mode;
    if (mode & libc::S_IFMT) == libc::S_IFLNK {
        return AnchoredMarkerVerdict::Rejected(Rejection::Symlink.message(display));
    }
    if (mode & libc::S_IFMT) == libc::S_IFDIR {
        return AnchoredMarkerVerdict::Rejected(Rejection::Directory.message(display));
    }
    if (mode & libc::S_IFMT) != libc::S_IFREG {
        return AnchoredMarkerVerdict::Rejected(Rejection::NonRegular.message(display));
    }
    if marker_size_rejected(&metadata) {
        return AnchoredMarkerVerdict::Rejected(Rejection::Oversized.message(display));
    }
    let bytes = match read_fd_to_end(fd, MAX_MARKER_BYTES) {
        Ok(bytes) => bytes,
        Err(error) => {
            return AnchoredMarkerVerdict::Rejected(format!(
                "workspace ownership marker cannot be read: {}: {error}",
                display.display()
            ));
        }
    };
    if bytes.is_empty() {
        return AnchoredMarkerVerdict::Rejected(Rejection::Empty.message(display));
    }
    if bytes != expected_run_id.as_bytes() {
        // Include the observed foreign owner so diagnostics identify the
        // conflicting claim, mirroring the established verify_marker_file
        // message format. The foreign owner is workspace metadata, not a
        // secret.
        let observed = String::from_utf8_lossy(&bytes);
        return AnchoredMarkerVerdict::Rejected(format!(
            "workspace ownership marker belongs to run '{observed}' not '{expected_run_id}'"
        ));
    }
    AnchoredMarkerVerdict::Trusted
}

/// Whether the marker's stat-reported size exceeds the bounded maximum or is
/// not representable as `u64`.
fn marker_size_rejected(metadata: &rustix::fs::Stat) -> bool {
    u64::try_from(metadata.st_size).map_or(true, |size| size > MAX_MARKER_BYTES)
}

/// Whether any entry exists at the bootstrap or durable marker path. Used by
/// the graph-level verifier/promotion executors only to decide whether strict
/// anchored verification is required. The marker evidence itself is always
/// validated by [`snapshot_bootstrap_marker`]/[`snapshot_durable_marker`].
///
/// **Fail closed on inspection errors:** an uninspectable marker path (e.g.
/// `PermissionDenied`) is treated as "evidence exists" so the caller proceeds
/// to strict anchored verification (which then rejects), rather than silently
/// treating an unreadable marker as "no evidence".
pub(super) fn anchored_evidence_exists(workspace_fd: BorrowedFd<'_>) -> bool {
    let bootstrap = path_entry_exists_two_hops(
        workspace_fd,
        ".luther",
        "workspace-owner",
        Path::new(".luther/workspace-owner"),
    );
    if bootstrap != PathEntry::Absent {
        return true;
    }
    let durable = path_entry_exists_three_hops(
        workspace_fd,
        ".git",
        "luther",
        "workspace-owner",
        Path::new(".git/luther/workspace-owner"),
    );
    durable != PathEntry::Absent
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathEntry {
    Absent,
    Present,
}

/// Check whether a leaf entry exists after two anchored directory hops.
///
/// A rejected parent directory (e.g. `.luther` is a regular file or
/// uninspectable) is treated as "evidence exists" so the caller proceeds to
/// strict anchored verification, which rejects. Only an absent parent
/// (`NotFound`) means there is genuinely no evidence beneath it.
fn path_entry_exists_two_hops(
    workspace_fd: BorrowedFd<'_>,
    parent_name: &str,
    leaf_name: &str,
    display: &Path,
) -> PathEntry {
    let parent_display = display
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| display.to_path_buf());
    let parent_fd = match open_directory_relative(workspace_fd, parent_name, &parent_display) {
        DirectoryOpen::Opened(fd) => fd,
        DirectoryOpen::NotFound => return PathEntry::Absent,
        // A rejected parent (regular file, symlink, uninspectable) means there
        // is an unexpected entry where a directory was expected: treat it as
        // evidence exists so strict verification runs and rejects.
        DirectoryOpen::Rejected(_) => return PathEntry::Present,
    };
    path_entry_exists_one_hop(parent_fd.as_fd(), leaf_name, display)
}

/// Check whether a leaf entry exists after three anchored directory hops.
///
/// A rejected ancestor directory at any hop is treated as "evidence exists"
/// (fail-closed); only `NotFound` at any hop means there is genuinely no
/// evidence beneath it.
fn path_entry_exists_three_hops(
    workspace_fd: BorrowedFd<'_>,
    hop1: &str,
    hop2: &str,
    leaf_name: &str,
    display: &Path,
) -> PathEntry {
    let hop1_fd = match open_directory_relative(workspace_fd, hop1, Path::new(hop1)) {
        DirectoryOpen::Opened(fd) => fd,
        DirectoryOpen::NotFound => return PathEntry::Absent,
        DirectoryOpen::Rejected(_) => return PathEntry::Present,
    };
    let hop2_display = display
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| display.to_path_buf());
    let hop2_fd = match open_directory_relative(hop1_fd.as_fd(), hop2, &hop2_display) {
        DirectoryOpen::Opened(fd) => fd,
        DirectoryOpen::NotFound => return PathEntry::Absent,
        DirectoryOpen::Rejected(_) => return PathEntry::Present,
    };
    path_entry_exists_one_hop(hop2_fd.as_fd(), leaf_name, display)
}

/// Check whether `leaf_name` exists beneath `parent_fd` using `statat` with
/// `SYMLINK_NOFOLLOW`. Any error other than `NotFound` is treated as "present"
/// so the caller proceeds to strict anchored verification that rejects.
fn path_entry_exists_one_hop(
    parent_fd: BorrowedFd<'_>,
    leaf_name: &str,
    display: &Path,
) -> PathEntry {
    let c_name = match CString::new(leaf_name) {
        Ok(c) => c,
        Err(_) => return PathEntry::Present,
    };
    match statat(parent_fd, &c_name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => PathEntry::Present,
        Err(Errno::NOENT) => PathEntry::Absent,
        Err(_) => {
            // An uninspectable marker (PermissionDenied, etc.) must be treated
            // as "present" so the caller proceeds to strict anchored
            // verification, which rejects.
            let _ = display;
            PathEntry::Present
        }
    }
}
