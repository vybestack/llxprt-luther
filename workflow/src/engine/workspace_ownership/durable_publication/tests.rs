//! Tests for descriptor-anchored durable marker publication and snapshot
//! verification. Split out of the main module to keep it under the file-size
//! gate without source stitching.

#![cfg(unix)]

use std::os::fd::{AsFd as _, OwnedFd};
use std::path::Path;

use rustix::fs::{open, Mode, OFlags};

use super::anchor::{FileIdentity, WorkspaceAnchor};
use super::{
    anchored_evidence_exists, open_git_directory, open_or_create_luther_directory,
    promote_via_anchor, promote_via_descriptor, publish_durable_marker, read_bootstrap_bytes,
    read_durable_marker_bytes, snapshot_bootstrap_marker, snapshot_durable_marker,
    AnchoredMarkerVerdict, MAX_MARKER_BYTES,
};

fn workspace_fd(workspace: &Path) -> OwnedFd {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    open(workspace, flags, Mode::empty()).expect("open workspace descriptor")
}

#[cfg(unix)]
#[test]
fn publish_and_read_back_durable_marker() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    let git_fd = open_git_directory(dir.path()).expect("open git dir");
    let luther_fd = open_or_create_luther_directory(git_fd.as_fd()).expect("open luther dir");
    publish_durable_marker(luther_fd.as_fd(), b"run-A").expect("publish");
    let bytes = read_durable_marker_bytes(luther_fd.as_fd()).expect("read back");
    assert_eq!(bytes, b"run-A");
}

#[cfg(unix)]
#[test]
fn publish_rejects_symlinked_git() {
    let dir = tempfile::tempdir().unwrap();
    let evil = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(evil.path(), dir.path().join(".git")).unwrap();
    let err = open_git_directory(dir.path()).unwrap_err();
    assert!(err.to_string().contains(".git"));
}

#[cfg(unix)]
#[test]
fn read_bootstrap_bytes_rejects_fifo() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".luther")).unwrap();
    let fifo = dir.path().join(".luther/workspace-owner");
    create_fifo(&fifo);
    let ws_fd = workspace_fd(dir.path());
    let err = read_bootstrap_bytes(ws_fd.as_fd()).unwrap_err();
    assert!(
        err.to_string().contains("not a regular file"),
        "fifo must be rejected, got: {err}"
    );
}

/// Create a FIFO at `path` for regression tests. The FIFO is never opened
/// for read/write, so the test cannot block.
#[cfg(unix)]
#[allow(unsafe_code)]
fn create_fifo(path: &Path) {
    // SAFETY: `mkfifo` creates a FIFO at a path inside a fresh temp
    // directory. The path is unique to this test invocation, so there is
    // no aliasing or data race. The C string is built from valid UTF-8
    // path bytes.
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
    assert_eq!(rc, 0, "mkfifo must succeed");
}

#[cfg(unix)]
#[test]
fn publish_concurrent_winner_returns_already_exists() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".git/luther")).unwrap();
    std::fs::write(dir.path().join(".git/luther/workspace-owner"), "run-first").unwrap();
    let git_fd = open_git_directory(dir.path()).expect("open git dir");
    let luther_fd = open_or_create_luther_directory(git_fd.as_fd()).expect("open luther dir");
    let err = publish_durable_marker(luther_fd.as_fd(), b"run-second").unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    // The existing content is untouched.
    assert_eq!(
        std::fs::read(dir.path().join(".git/luther/workspace-owner")).unwrap(),
        b"run-first"
    );
}

#[cfg(unix)]
#[test]
fn promote_via_descriptor_publishes_and_revalidates_bytes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".luther")).unwrap();
    std::fs::write(dir.path().join(".luther/workspace-owner"), "run-A").unwrap();
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    promote_via_descriptor(dir.path(), "run-A").expect("promotion succeeds");
    let durable = dir.path().join(".git/luther/workspace-owner");
    assert_eq!(std::fs::read(&durable).unwrap(), b"run-A");
}

#[cfg(unix)]
#[test]
fn promote_via_descriptor_idempotent_when_durable_matches_bootstrap() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".luther")).unwrap();
    std::fs::write(dir.path().join(".luther/workspace-owner"), "run-A").unwrap();
    std::fs::create_dir_all(dir.path().join(".git/luther")).unwrap();
    std::fs::write(dir.path().join(".git/luther/workspace-owner"), "run-A").unwrap();
    // Existing durable matches bootstrap bytes; promotion is idempotent.
    promote_via_descriptor(dir.path(), "run-A").expect("idempotent promotion");
    assert_eq!(
        std::fs::read(dir.path().join(".git/luther/workspace-owner")).unwrap(),
        b"run-A"
    );
}

#[cfg(unix)]
#[test]
fn promote_via_descriptor_rejects_mismatched_existing_durable() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".luther")).unwrap();
    std::fs::write(dir.path().join(".luther/workspace-owner"), "run-A").unwrap();
    std::fs::create_dir_all(dir.path().join(".git/luther")).unwrap();
    std::fs::write(
        dir.path().join(".git/luther/workspace-owner"),
        "run-foreign",
    )
    .unwrap();
    let err = promote_via_descriptor(dir.path(), "run-A").unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
}

// -----------------------------------------------------------------------
// promote_via_descriptor: bootstrap bytes must exactly match expected run
// id. A foreign, empty, or otherwise mismatched bootstrap entry must fail
// closed before any durable write.
// -----------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn promote_via_descriptor_rejects_foreign_bootstrap_bytes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".luther")).unwrap();
    std::fs::write(dir.path().join(".luther/workspace-owner"), "run-foreign").unwrap();
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    let err = promote_via_descriptor(dir.path(), "run-A").unwrap_err();
    assert!(
        err.to_string().contains("do not match expected run id"),
        "foreign bootstrap must be rejected, got: {err}"
    );
    // No durable marker was published.
    assert!(!dir.path().join(".git/luther/workspace-owner").exists());
}

#[cfg(unix)]
#[test]
fn promote_via_descriptor_rejects_empty_bootstrap_bytes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".luther")).unwrap();
    std::fs::write(dir.path().join(".luther/workspace-owner"), "").unwrap();
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    let err = promote_via_descriptor(dir.path(), "run-A").unwrap_err();
    assert!(
        err.to_string().contains("do not match expected run id"),
        "empty bootstrap must be rejected, got: {err}"
    );
    assert!(!dir.path().join(".git/luther/workspace-owner").exists());
}

#[cfg(unix)]
#[test]
fn promote_via_descriptor_rejects_bootstrap_with_trailing_newline() {
    // Exact comparison: even a trailing newline is a mismatch.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".luther")).unwrap();
    std::fs::write(dir.path().join(".luther/workspace-owner"), "run-A\n").unwrap();
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    let err = promote_via_descriptor(dir.path(), "run-A").unwrap_err();
    assert!(
        err.to_string().contains("do not match expected run id"),
        "trailing newline must be rejected, got: {err}"
    );
    assert!(!dir.path().join(".git/luther/workspace-owner").exists());
}

// -----------------------------------------------------------------------
// Existing durable entry types: a pre-existing foreign durable entry must
// not be accepted. The promotion path must compare descriptor-read durable
// bytes exactly.
// -----------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn promote_via_descriptor_rejects_empty_existing_durable() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".luther")).unwrap();
    std::fs::write(dir.path().join(".luther/workspace-owner"), "run-A").unwrap();
    std::fs::create_dir_all(dir.path().join(".git/luther")).unwrap();
    std::fs::write(dir.path().join(".git/luther/workspace-owner"), "").unwrap();
    let err = promote_via_descriptor(dir.path(), "run-A").unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
}

#[cfg(unix)]
#[test]
fn promote_via_descriptor_rejects_symlinked_existing_durable() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".luther")).unwrap();
    std::fs::write(dir.path().join(".luther/workspace-owner"), "run-A").unwrap();
    std::fs::create_dir_all(dir.path().join(".git/luther")).unwrap();
    // A symlink durable marker must be rejected by the no-follow stat in
    // read_durable_marker_bytes before the bytes are ever compared.
    std::os::unix::fs::symlink(
        dir.path().join(".luther/workspace-owner"),
        dir.path().join(".git/luther/workspace-owner"),
    )
    .unwrap();
    let err = promote_via_descriptor(dir.path(), "run-A").unwrap_err();
    assert!(
        err.to_string().contains("not a regular file"),
        "symlink durable must be rejected, got: {err}"
    );
}

#[cfg(unix)]
#[test]
fn promote_via_descriptor_rejects_directory_existing_durable() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".luther")).unwrap();
    std::fs::write(dir.path().join(".luther/workspace-owner"), "run-A").unwrap();
    std::fs::create_dir_all(dir.path().join(".git/luther/workspace-owner")).unwrap();
    let err = promote_via_descriptor(dir.path(), "run-A").unwrap_err();
    assert!(
        err.to_string().contains("not a regular file"),
        "directory durable must be rejected, got: {err}"
    );
}

#[cfg(unix)]
#[test]
fn promote_via_descriptor_rejects_fifo_existing_durable() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".luther")).unwrap();
    std::fs::write(dir.path().join(".luther/workspace-owner"), "run-A").unwrap();
    std::fs::create_dir_all(dir.path().join(".git/luther")).unwrap();
    create_fifo(&dir.path().join(".git/luther/workspace-owner"));
    let err = promote_via_descriptor(dir.path(), "run-A").unwrap_err();
    assert!(
        err.to_string().contains("not a regular file"),
        "fifo durable must be rejected, got: {err}"
    );
}

// -----------------------------------------------------------------------
// Anchored snapshot verdicts: type and race safety for the read-only
// verification path used by cleanup/continuation/scope/precheck/resume.
// -----------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn snapshot_bootstrap_trusts_exact_owner() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".luther")).unwrap();
    std::fs::write(dir.path().join(".luther/workspace-owner"), "run-A").unwrap();
    let ws_fd = workspace_fd(dir.path());
    let verdict = snapshot_bootstrap_marker(ws_fd.as_fd(), "run-A");
    assert_eq!(verdict, AnchoredMarkerVerdict::Trusted);
}

#[cfg(unix)]
#[test]
fn snapshot_bootstrap_absent_when_marker_missing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".luther")).unwrap();
    let ws_fd = workspace_fd(dir.path());
    let verdict = snapshot_bootstrap_marker(ws_fd.as_fd(), "run-A");
    assert_eq!(verdict, AnchoredMarkerVerdict::Absent);
}

#[cfg(unix)]
#[test]
fn snapshot_bootstrap_absent_when_luther_missing() {
    let dir = tempfile::tempdir().unwrap();
    let ws_fd = workspace_fd(dir.path());
    let verdict = snapshot_bootstrap_marker(ws_fd.as_fd(), "run-A");
    assert_eq!(verdict, AnchoredMarkerVerdict::Absent);
}

#[cfg(unix)]
#[test]
fn snapshot_bootstrap_rejects_foreign() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".luther")).unwrap();
    std::fs::write(dir.path().join(".luther/workspace-owner"), "run-foreign").unwrap();
    let ws_fd = workspace_fd(dir.path());
    let verdict = snapshot_bootstrap_marker(ws_fd.as_fd(), "run-A");
    match verdict {
        AnchoredMarkerVerdict::Rejected(reason) => assert!(reason.contains("run-foreign")),
        other => panic!("expected rejected, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn snapshot_bootstrap_rejects_empty() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".luther")).unwrap();
    std::fs::write(dir.path().join(".luther/workspace-owner"), "").unwrap();
    let ws_fd = workspace_fd(dir.path());
    let verdict = snapshot_bootstrap_marker(ws_fd.as_fd(), "run-A");
    assert!(verdict.is_rejected(), "empty marker must be rejected");
}

#[cfg(unix)]
#[test]
fn snapshot_bootstrap_rejects_trailing_newline() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".luther")).unwrap();
    std::fs::write(dir.path().join(".luther/workspace-owner"), "run-A\n").unwrap();
    let ws_fd = workspace_fd(dir.path());
    let verdict = snapshot_bootstrap_marker(ws_fd.as_fd(), "run-A");
    assert!(
        verdict.is_rejected(),
        "trailing newline must be rejected (exact bytes), got {verdict:?}"
    );
}

#[cfg(unix)]
#[test]
fn snapshot_bootstrap_rejects_symlink() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".luther")).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", dir.path().join(".luther/workspace-owner")).unwrap();
    let ws_fd = workspace_fd(dir.path());
    let verdict = snapshot_bootstrap_marker(ws_fd.as_fd(), "run-A");
    match verdict {
        AnchoredMarkerVerdict::Rejected(reason) => {
            assert!(
                reason.contains("symlink"),
                "symlink must be rejected: {reason}"
            );
        }
        other => panic!("expected rejected, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn snapshot_bootstrap_rejects_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".luther/workspace-owner")).unwrap();
    let ws_fd = workspace_fd(dir.path());
    let verdict = snapshot_bootstrap_marker(ws_fd.as_fd(), "run-A");
    match verdict {
        AnchoredMarkerVerdict::Rejected(reason) => {
            assert!(
                reason.contains("directory"),
                "directory must be rejected: {reason}"
            );
        }
        other => panic!("expected rejected, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn snapshot_bootstrap_rejects_fifo() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".luther")).unwrap();
    create_fifo(&dir.path().join(".luther/workspace-owner"));
    let ws_fd = workspace_fd(dir.path());
    let verdict = snapshot_bootstrap_marker(ws_fd.as_fd(), "run-A");
    match verdict {
        AnchoredMarkerVerdict::Rejected(reason) => {
            assert!(
                reason.contains("regular file") || reason.contains("not"),
                "fifo must be rejected: {reason}"
            );
        }
        other => panic!("expected rejected, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn snapshot_durable_trusts_exact_owner() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".git/luther")).unwrap();
    std::fs::write(dir.path().join(".git/luther/workspace-owner"), "run-A").unwrap();
    let ws_fd = workspace_fd(dir.path());
    let verdict = snapshot_durable_marker(ws_fd.as_fd(), "run-A");
    assert_eq!(verdict, AnchoredMarkerVerdict::Trusted);
}

#[cfg(unix)]
#[test]
fn snapshot_durable_absent_when_git_missing() {
    let dir = tempfile::tempdir().unwrap();
    let ws_fd = workspace_fd(dir.path());
    let verdict = snapshot_durable_marker(ws_fd.as_fd(), "run-A");
    assert_eq!(verdict, AnchoredMarkerVerdict::Absent);
}

#[cfg(unix)]
#[test]
fn snapshot_durable_rejects_symlinked_git() {
    let dir = tempfile::tempdir().unwrap();
    let evil = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(evil.path(), dir.path().join(".git")).unwrap();
    let ws_fd = workspace_fd(dir.path());
    let verdict = snapshot_durable_marker(ws_fd.as_fd(), "run-A");
    assert!(
        verdict.is_rejected(),
        "symlinked .git must be rejected, got {verdict:?}"
    );
}

#[cfg(unix)]
#[test]
fn snapshot_durable_rejects_symlinked_marker() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".git/luther")).unwrap();
    std::os::unix::fs::symlink(
        "/etc/passwd",
        dir.path().join(".git/luther/workspace-owner"),
    )
    .unwrap();
    let ws_fd = workspace_fd(dir.path());
    let verdict = snapshot_durable_marker(ws_fd.as_fd(), "run-A");
    match verdict {
        AnchoredMarkerVerdict::Rejected(reason) => {
            assert!(
                reason.contains("symlink"),
                "symlink must be rejected: {reason}"
            );
        }
        other => panic!("expected rejected, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn snapshot_durable_rejects_foreign() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".git/luther")).unwrap();
    std::fs::write(
        dir.path().join(".git/luther/workspace-owner"),
        "run-foreign",
    )
    .unwrap();
    let ws_fd = workspace_fd(dir.path());
    let verdict = snapshot_durable_marker(ws_fd.as_fd(), "run-A");
    match verdict {
        AnchoredMarkerVerdict::Rejected(reason) => assert!(reason.contains("run-foreign")),
        other => panic!("expected rejected, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn anchored_evidence_exists_detects_bootstrap() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".luther")).unwrap();
    std::fs::write(dir.path().join(".luther/workspace-owner"), "run-A").unwrap();
    let ws_fd = workspace_fd(dir.path());
    assert!(anchored_evidence_exists(ws_fd.as_fd()));
}

#[cfg(unix)]
#[test]
fn anchored_evidence_exists_detects_durable() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".git/luther")).unwrap();
    std::fs::write(dir.path().join(".git/luther/workspace-owner"), "run-A").unwrap();
    let ws_fd = workspace_fd(dir.path());
    assert!(anchored_evidence_exists(ws_fd.as_fd()));
}

#[cfg(unix)]
#[test]
fn anchored_evidence_exists_false_when_absent() {
    let dir = tempfile::tempdir().unwrap();
    let ws_fd = workspace_fd(dir.path());
    assert!(!anchored_evidence_exists(ws_fd.as_fd()));
}

#[cfg(unix)]
#[test]
fn anchored_evidence_exists_true_when_marker_uninspectable() {
    // A marker whose parent directory cannot be opened (e.g. because .luther
    // is a regular file instead of a directory) must be treated as
    // "evidence exists" so the caller proceeds to strict anchored
    // verification, which then rejects.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".luther"), "not a dir").unwrap();
    let ws_fd = workspace_fd(dir.path());
    assert!(anchored_evidence_exists(ws_fd.as_fd()));
}

#[cfg(unix)]
#[test]
fn snapshot_bootstrap_rejects_oversized_marker() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".luther")).unwrap();
    let oversized = vec![b'A'; (MAX_MARKER_BYTES + 1) as usize];
    std::fs::write(dir.path().join(".luther/workspace-owner"), &oversized).unwrap();
    let ws_fd = workspace_fd(dir.path());
    let verdict = snapshot_bootstrap_marker(ws_fd.as_fd(), "run-A");
    match verdict {
        AnchoredMarkerVerdict::Rejected(reason) => {
            assert!(
                reason.contains("size"),
                "oversized must be rejected: {reason}"
            );
        }
        other => panic!("expected rejected, got {other:?}"),
    }
}

// -----------------------------------------------------------------------
// WorkspaceAnchor identity capture (issue 158): the anchor captures the
// dev/inode of the opened descriptor via fstat at construction. Promotion
// via the same anchor cannot be redirected by a post-open swap because no
// reopen of the workspace path occurs.
// -----------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn anchor_captures_workspace_inode_identity() {
    // The anchor's identity must match the path's dev/inode, and the same
    // anchor's identity must remain stable across revalidation.
    let dir = tempfile::tempdir().unwrap();
    let canonical = dir.path().canonicalize().unwrap();
    let anchor = WorkspaceAnchor::open(&canonical).expect("open anchor");
    let path_identity = FileIdentity::of_path(&canonical).expect("path identity");
    assert_eq!(
        anchor.identity(),
        path_identity,
        "anchor identity must match the canonical path dev/inode"
    );
    anchor
        .revalidate_identity()
        .expect("identity stable without a swap");
}

#[cfg(unix)]
#[test]
fn promote_via_anchor_retains_same_descriptor_through_promotion() {
    // Issue 158 root anchor: ensure_durable_with_anchor opens ONE anchor
    // and retains it through verify, durable inspection, and promotion.
    // After promotion, the durable marker must exist and the anchor's
    // identity must be unchanged (no reopen, no swap). This exercises the
    // promote_via_anchor seam directly.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".luther")).unwrap();
    std::fs::write(dir.path().join(".luther/workspace-owner"), "run-A").unwrap();
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    let canonical = dir.path().canonicalize().unwrap();
    let anchor = WorkspaceAnchor::open(&canonical).expect("open anchor");
    let identity_before = anchor.identity();
    promote_via_anchor(&anchor, "run-A").expect("promotion via anchor succeeds");
    let durable = dir.path().join(".git/luther/workspace-owner");
    assert_eq!(std::fs::read(&durable).unwrap(), b"run-A");
    // The anchor's identity must be unchanged: promotion did not reopen
    // the workspace path.
    assert_eq!(
        identity_before,
        anchor.identity(),
        "the anchor identity must be unchanged after promotion (no reopen)"
    );
}

#[cfg(unix)]
#[test]
fn anchor_identity_detects_workspace_swap() {
    // Deterministic swap detection (issue 158): two distinct workspace
    // directories must have distinct dev/inode identities. The anchor
    // captures the identity of the workspace it was opened from; a
    // different workspace must have a different identity. This is the
    // deterministic foundation for swap detection: even if the workspace
    // path is later replaced (rename swap), the captured identity will
    // differ from a fresh anchor opened on the swapped path.
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let canonical_a = dir_a.path().canonicalize().unwrap();
    let canonical_b = dir_b.path().canonicalize().unwrap();
    let anchor_a = WorkspaceAnchor::open(&canonical_a).expect("open anchor a");
    let anchor_b = WorkspaceAnchor::open(&canonical_b).expect("open anchor b");
    assert_ne!(
        anchor_a.identity(),
        anchor_b.identity(),
        "distinct workspace directories must have distinct dev/inode identities"
    );
    // Each anchor's identity must match its own path.
    assert_eq!(
        anchor_a.identity(),
        FileIdentity::of_path(&canonical_a).unwrap()
    );
    assert_eq!(
        anchor_b.identity(),
        FileIdentity::of_path(&canonical_b).unwrap()
    );
}

#[cfg(unix)]
#[test]
fn anchor_identity_detects_inode_mismatch_after_rename_swap() {
    // Deterministic inode-mismatch test (issue 158): capture an anchor's
    // identity, then perform a rename-swap so the workspace path now
    // refers to a different inode. A fresh anchor opened on the same path
    // must report a different identity than the original anchor captured.
    // This proves the anchor's captured identity would detect a TOCTOU
    // swap between canonicalization and the descriptor open (or after a
    // long-lived operation).
    let original = tempfile::tempdir().unwrap();
    let replacement = tempfile::tempdir().unwrap();
    let canonical_original = original.path().canonicalize().unwrap();
    // Capture the identity of the original workspace.
    let original_identity = FileIdentity::of_path(&canonical_original).expect("original identity");
    // Perform a rename swap: move the replacement into a sibling path,
    // then rename original away and replacement into the original's path.
    // Use a temp sibling to hold the original aside.
    let parent = canonical_original.parent().unwrap().to_path_buf();
    let parked = parent.join(format!(
        "luther-anchor-test-parked-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::rename(&canonical_original, &parked).expect("park original");
    std::fs::rename(replacement.path(), &canonical_original).expect("swap replacement in");
    // A fresh path identity on the same canonical path must now differ.
    let swapped_identity = FileIdentity::of_path(&canonical_original).expect("swapped identity");
    assert_ne!(
        original_identity, swapped_identity,
        "after a rename swap, the same path must report a different inode"
    );
}

// -----------------------------------------------------------------------
// WorkspaceAnchor::open rejects a symlinked final component of the
// supplied root before the O_NOFOLLOW open (issue 158 finding 1). A
// symlinked root must never be silently resolved.
// -----------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn anchor_open_rejects_symlinked_supplied_root() {
    let real = tempfile::tempdir().unwrap();
    let link_parent = tempfile::tempdir().unwrap();
    let link = link_parent.path().join("workspace-link");
    std::os::unix::fs::symlink(real.path(), &link).unwrap();
    match WorkspaceAnchor::open(&link) {
        Ok(_) => panic!("a symlinked supplied root must be rejected"),
        Err(err) => {
            assert!(
                err.to_string().contains("symlink"),
                "a symlinked supplied root must be rejected with a symlink message, got: {err}"
            );
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        }
    }
}

// -----------------------------------------------------------------------
// WorkspaceAnchor::open captures fd identity and canonical identity and
// compares them, failing closed on mismatch (issue 158 finding 1). The
// canonical identity is read from the same path immediately after the
// open, so a swap between the caller's canonicalize() and this open
// produces an identity mismatch.
// -----------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn anchor_open_fails_closed_on_fd_canonical_identity_mismatch() {
    // A workspace that is a real directory opens normally: the fd identity
    // matches the canonical identity read from the same path. This is the
    // baseline that proves the comparison is in effect.
    let dir = tempfile::tempdir().unwrap();
    let canonical = dir.path().canonicalize().unwrap();
    let anchor = WorkspaceAnchor::open(&canonical).expect("open succeeds for real dir");
    assert_eq!(
        anchor.identity(),
        FileIdentity::of_fd(anchor.as_fd()).unwrap(),
        "the anchor identity must match a fresh fstat of the retained fd"
    );
    assert_eq!(
        anchor.identity(),
        FileIdentity::of_path(&canonical).unwrap(),
        "the anchor identity must match the canonical path identity"
    );
}

// -----------------------------------------------------------------------
// Child process spawning (issue 158 finding 1): the workspace anchor pins a
// child's cwd to the verified inode via fchdir in pre_exec. A root rename
// between verification and child startup cannot redirect the child.
// -----------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn anchored_child_cwd_matches_verified_descriptor() {
    use std::process::Command;

    use super::configure_fchdir_pre_exec;

    let dir = tempfile::tempdir().unwrap();
    let canonical = dir.path().canonicalize().unwrap();
    let anchor = WorkspaceAnchor::open(&canonical).expect("open anchor");
    let child_fd = anchor.prepare_child_fd().expect("prepare child fd");
    let mut command = Command::new("sh");
    configure_fchdir_pre_exec(&mut command, &child_fd).expect("configure pre_exec");
    command.arg("-c").arg("pwd");
    let output = command.output().expect("spawn anchored child");
    drop(child_fd);
    let child_pwd = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert_eq!(
        child_pwd,
        canonical.to_str().unwrap(),
        "child cwd must match verified descriptor"
    );
}

#[cfg(unix)]
#[test]
fn anchored_child_unaffected_by_post_open_rename() {
    use std::process::Command;

    use super::configure_fchdir_pre_exec;

    let original = tempfile::tempdir().unwrap();
    let replacement = tempfile::tempdir().unwrap();
    let canonical_original = original.path().canonicalize().unwrap();
    let anchor = WorkspaceAnchor::open(&canonical_original).expect("open anchor");
    let child_fd = anchor.prepare_child_fd().expect("prepare child fd");
    let parent = canonical_original.parent().unwrap().to_path_buf();
    let parked = parent.join(format!(
        "anchored-rename-parked-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::rename(&canonical_original, &parked).expect("park original");
    std::fs::rename(replacement.path(), &canonical_original).expect("swap replacement in");
    std::fs::write(parked.join("anchored-sentinel"), b"original").unwrap();
    let mut command = Command::new("sh");
    configure_fchdir_pre_exec(&mut command, &child_fd).expect("configure pre_exec");
    command
        .arg("-c")
        .arg("test -f anchored-sentinel && echo ORIGINAL || echo REDIRECTED");
    let output = command.output().expect("spawn anchored child");
    drop(child_fd);
    let result = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert_eq!(
        result, "ORIGINAL",
        "fchdir-anchored child must land in the original directory despite post-open rename"
    );
}
