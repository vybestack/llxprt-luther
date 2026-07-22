//! Workspace anchor: the single descriptor kernel for workspace ownership.
//!
//! An `O_NOFOLLOW` workspace directory descriptor whose dev/inode identity is
//! captured at construction and retained through verify, durable inspection,
//! promotion, and child process spawning. This is the consolidated kernel:
//! both the ownership verification path and the shell executor's FD-anchored
//! process spawning use the same `WorkspaceAnchor`, eliminating the duplicate
//! `AnchoredWorkspaceFd` overlay that previously existed in
//! `executors/anchored_command.rs`.

use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::path::Path;
use std::process::Command;

use rustix::fs::fstat;
use rustix::io::dup;

use super::{open_workspace_directory, rustix_error};

/// Anchor a workspace verification to a workspace directory descriptor opened
/// with `O_NOFOLLOW`. Returns a [`WorkspaceAnchor`] that subsequent snapshot,
/// promotion, and child-spawning operations use, so a TOCTOU swap of the
/// workspace path cannot redirect marker resolution, durable writes, or the
/// child's working directory.
///
/// **Issue 158 root anchor:** the anchor captures the pre-open identity of
/// `workspace` via `symlink_metadata` **before** the `O_NOFOLLOW` open, then
/// `fstat`s the opened fd and compares the fd's dev/inode to that pre-open
/// identity. Capturing the identity before the open closes the TOCTOU window
/// in which a concurrent attacker could swap the workspace path between the
/// caller's `canonicalize()` and the open: the pre-open snapshot records what
/// the path WAS, the fd records what it became, and a mismatch fails closed.
/// The same anchor is then retained through verify, durable inspection, and
/// promotion so no reopen can be redirected.
#[derive(Debug)]
pub(crate) struct WorkspaceAnchor {
    fd: OwnedFd,
    identity: FileIdentity,
}

impl WorkspaceAnchor {
    /// Open `workspace` as a directory descriptor with `O_NOFOLLOW`, then
    /// `fstat` the opened fd and compare its dev/inode to the identity
    /// captured from `workspace` via `symlink_metadata` **before** the open.
    ///
    /// **TOCTOU invariant (issue 158):** the canonical identity is captured
    /// **before** the `O_NOFOLLOW` open. If an attacker swaps the workspace
    /// path between the pre-open capture and the open, the opened fd's
    /// identity will differ from the pre-open identity and the anchor
    /// construction fails closed.
    pub(crate) fn open(workspace: &Path) -> io::Result<Self> {
        // Reject a symlinked final component of the supplied root BEFORE any
        // identity capture or open.
        if let Ok(meta) = std::fs::symlink_metadata(workspace) {
            if meta.file_type().is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "workspace root is a symlink and must be a real directory",
                ));
            }
        }
        let pre_open_identity = FileIdentity::of_path(workspace)?;
        let fd = open_workspace_directory(workspace)?;
        let fd_identity = FileIdentity::of_fd(fd.as_fd())?;
        if fd_identity != pre_open_identity {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "workspace identity changed between pre-open capture and anchor open",
            ));
        }
        Ok(Self {
            fd,
            identity: fd_identity,
        })
    }

    /// Borrow the underlying workspace descriptor for relative operations.
    pub(crate) fn as_fd(&self) -> BorrowedFd<'_> {
        let owned: &OwnedFd = &self.fd;
        owned.as_fd()
    }

    /// The dev/inode identity captured at anchor construction. Exposed so the
    /// `workspace_ownership_verify` step can produce an immutable
    /// [`WorkspaceAuthorization`](super::super::WorkspaceAuthorization) from a
    /// verified anchor.
    pub(crate) fn identity(&self) -> FileIdentity {
        self.identity
    }

    /// Confirm the anchored descriptor still refers to the same dev/inode it
    /// was constructed with. Used after long-lived operations to detect a
    /// post-open swap of the workspace path.
    #[cfg(test)]
    pub(crate) fn revalidate_identity(&self) -> io::Result<()> {
        let current = FileIdentity::of_fd(self.as_fd())?;
        if current != self.identity {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "workspace identity changed after anchor construction",
            ));
        }
        Ok(())
    }

    /// Duplicate the workspace descriptor WITHOUT `CLOEXEC` so a child process
    /// inherits it across `exec`. The child's `pre_exec` hook calls `fchdir`
    /// on this descriptor to pin its cwd to the verified inode, preventing a
    /// root rename between verification and child startup from redirecting the
    /// child.
    pub(crate) fn prepare_child_fd(&self) -> io::Result<OwnedFd> {
        dup(self.as_fd()).map_err(rustix_to_io_error("dup workspace fd for child inheritance"))
    }
}

/// Configure a `Command`'s `pre_exec` hook to `fchdir` to the inherited
/// descriptor, anchoring the child's cwd to the verified inode.
///
/// The `child_fd` is duplicated without `CLOEXEC` so it survives into the
/// child. The `pre_exec` closure calls `rustix::process::fchdir` (a safe
/// wrapper) to pin the child's cwd before `exec`.
///
/// # Safety justification
///
/// `pre_exec` and `BorrowedFd::borrow_raw` require `unsafe` because they
/// interact with raw fds and run in a child process after fork. The closure
/// body is async-signal-safe: it only calls `fchdir` (POSIX async-signal-safe)
/// on a valid inherited directory descriptor.
#[cfg(unix)]
#[allow(unsafe_code)]
pub(crate) fn configure_fchdir_pre_exec(
    command: &mut Command,
    child_fd: &OwnedFd,
) -> io::Result<()> {
    use std::os::unix::process::CommandExt;
    let raw_fd = child_fd.as_raw_fd();
    // SAFETY: `pre_exec` requires an unsafe block because the closure runs in a
    // child process after fork, where only async-signal-safe operations are
    // permitted. The closure body calls `rustix::process::fchdir` (a safe
    // wrapper over the fchdir syscall, which is async-signal-safe per POSIX)
    // and constructs a `BorrowedFd` from a raw fd that is a valid, inherited
    // directory descriptor (duplicated without CLOEXEC, identity verified
    // before spawn).
    unsafe {
        command.pre_exec(move || {
            let borrowed = std::os::fd::BorrowedFd::borrow_raw(raw_fd);
            rustix::process::fchdir(borrowed).map_err(io::Error::from)
        });
    }
    Ok(())
}

/// Non-Unix stub: no descriptor-anchored cwd is available.
#[cfg(not(unix))]
pub(crate) fn configure_fchdir_pre_exec(
    _command: &mut Command,
    _child_fd: &OwnedFd,
) -> io::Result<()> {
    Ok(())
}

/// Dev/inode identity pair for a file descriptor, used to detect a TOCTOU swap
/// of the workspace path between canonicalization and the descriptor open, or
/// after a long-lived operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileIdentity {
    dev: u64,
    ino: u64,
}

impl FileIdentity {
    /// Capture the dev/inode identity of an open descriptor via `fstat`.
    pub(crate) fn of_fd(fd: BorrowedFd<'_>) -> io::Result<Self> {
        let stat = fstat(fd).map_err(rustix_error("stat workspace anchor", Path::new(".")))?;
        Ok(Self::from_stat(&stat))
    }

    /// Capture the dev/inode identity of an arbitrary open descriptor via
    /// `fstat`. Exposed so the shell executor can compare the workspace
    /// descriptor it opens to the authorization captured by the verify step,
    /// without re-reading the path (descriptor-relative).
    pub(crate) fn of_descriptor(fd: BorrowedFd<'_>) -> io::Result<Self> {
        Self::of_fd(fd)
    }

    /// Build a [`FileIdentity`] from a `rustix::fs::Stat`, normalizing the
    /// platform-specific dev/inode widths to `u64`.
    fn from_stat(stat: &rustix::fs::Stat) -> Self {
        Self {
            dev: stat.st_dev as u64,
            ino: stat.st_ino,
        }
    }

    /// Capture the dev/inode identity of a path via `symlink_metadata`.
    ///
    /// Only Unix is supported: the descriptor-anchored ownership kernel relies
    /// on `O_NOFOLLOW` and `fstat` semantics that non-Unix targets lack.
    #[cfg(unix)]
    pub(crate) fn of_path(path: &Path) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::symlink_metadata(path)?;
        Ok(Self {
            dev: meta.dev(),
            ino: meta.ino(),
        })
    }

    /// The device number of the authorized workspace.
    pub(crate) const fn dev(self) -> u64 {
        self.dev
    }

    /// The inode number of the authorized workspace.
    pub(crate) const fn ino(self) -> u64 {
        self.ino
    }
}

fn rustix_to_io_error(action: &str) -> impl Fn(rustix::io::Errno) -> io::Error + '_ {
    move |error: rustix::io::Errno| {
        let raw = io::Error::from(error);
        io::Error::new(raw.kind(), format!("{action}: {error}"))
    }
}
