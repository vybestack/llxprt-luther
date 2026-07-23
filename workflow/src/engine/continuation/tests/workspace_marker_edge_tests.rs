//! Workspace ownership swap, permission, and interrupted-publication tests.

use crate::engine::continuation::{
    verify_workspace_ownership_marker, write_workspace_owner_marker,
};

/// Swap-oriented write test: after writing to a real workspace, replacing the
/// workspace root with a symlink must cause a subsequent idempotent write to
/// fail (the root is now a symlink).
#[cfg(unix)]
#[test]
fn write_fails_after_root_replaced_with_symlink() {
    let real = tempfile::tempdir().expect("real workspace");
    write_workspace_owner_marker(real.path(), "run-swap-write").expect("initial write");

    let link_parent = tempfile::tempdir().expect("link parent");
    let link = link_parent.path().join("ws-swap-write-link");
    std::os::unix::fs::symlink(real.path(), &link).unwrap();
    let err = write_workspace_owner_marker(&link, "run-swap-write")
        .expect_err("write through a symlinked root must fail after a swap");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("symlink"));
}

/// Regression: a TOCTOU swap of the workspace root to a symlink that resolves
/// *back to the canonical root* must be rejected. The earlier implementation
/// revalidated with `metadata`, which follows symlinks. A symlink whose target
/// is the canonical root itself would be followed, compare equal to the
/// canonical root's inode, and silently pass. The fix uses `symlink_metadata`
/// on the observed path so the symlink is observed on the snapshot before any
/// identity comparison. This test constructs exactly that adversarial swap:
/// after a successful verify on the real root, the observed path is replaced
/// with a symlink to the canonical root, and verify must reject it.
#[cfg(unix)]
#[test]
fn verify_rejects_symlink_to_canonical_root_swap() {
    let real = tempfile::tempdir().expect("real workspace");
    let run_id = "run-symlink-to-canonical";
    write_workspace_owner_marker(real.path(), run_id).expect("write to real root");
    // Baseline: the real root verifies successfully.
    assert!(verify_workspace_ownership_marker(real.path(), run_id).is_none());

    // Canonicalize the real root so we can build a symlink that resolves back
    // to the canonical path, then swap the observed path for that symlink.
    let canonical = real
        .path()
        .canonicalize()
        .expect("canonicalize real workspace");
    // Replace the observed workspace path with a symlink to the canonical root.
    // `write_workspace_owner_marker`/`verify_workspace_ownership_marker` take
    // `&Path`, so to model an in-place TOCTOU swap of the path the caller
    // supplied, we create a symlink that points back at the canonical root and
    // verify through it. Because the symlink resolves to the canonical root,
    // `metadata` would follow it and compare equal to the canonical root — the
    // exact regression this test guards against. `symlink_metadata` observes
    // the symlink on the snapshot and rejects.
    let link_parent = tempfile::tempdir().expect("link parent");
    let symlink_to_canonical = link_parent.path().join("ws-symlink-to-canonical");
    std::os::unix::fs::symlink(&canonical, &symlink_to_canonical).unwrap();

    let reason = verify_workspace_ownership_marker(&symlink_to_canonical, run_id);
    assert!(
        reason.is_some(),
        "verify through a symlinked root must fail even when the symlink resolves to the canonical root"
    );
    let detail = reason.unwrap();
    assert!(
        detail.contains("symlink"),
        "expected symlink rejection, got: {detail}"
    );
}

/// Regression for the same TOCTOU fix applied to the directory-type revalidate:
/// if the observed workspace root is replaced with a non-directory entry during
/// verification, the snapshot observed via `symlink_metadata` must reject it.
#[cfg(unix)]
#[test]
fn verify_rejects_non_directory_workspace_root() {
    // Build a regular-file path and attempt to verify it: a regular file is not
    // a directory and must be rejected by the post-canonicalize revalidate.
    let file_parent = tempfile::tempdir().expect("file parent");
    let file_workspace = file_parent.path().join("ws-file");
    std::fs::write(&file_workspace, b"not-a-directory").expect("write file");

    let reason = verify_workspace_ownership_marker(&file_workspace, "run-non-dir");
    assert!(
        reason.is_some(),
        "verify through a non-directory workspace root must fail"
    );
}

// ---------------------------------------------------------------------------
// Typed marker inspection: only NotFound means absent; PermissionDenied and
// other inspection errors must reject, never be silently treated as "missing"
// (issue 158 review).
// ---------------------------------------------------------------------------

/// An unreadable marker (permission denied) must be rejected by verification,
/// not silently treated as "missing". A fail-open "missing" would allow an
/// attacker that strips read permission to bypass ownership verification.
///
/// Runs as root only when permissions are actually enforced (uid != 0). When
/// the test runs as root, root bypasses permission checks, so the test is
/// skipped to avoid a false negative.
#[cfg(unix)]
#[test]
fn verify_rejects_unreadable_marker_permission_denied() {
    if is_running_as_root() {
        eprintln!("skipping permission-denied test under root (root bypasses DAC)");
        return;
    }
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    write_workspace_owner_marker(workspace, "run-perm").expect("publish");
    let marker = workspace.join(".luther").join("workspace-owner");
    // Strip read permission so symlink_metadata still succeeds but read fails.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o000)).unwrap();
    let reason = verify_workspace_ownership_marker(workspace, "run-perm");
    assert!(
        reason.is_some(),
        "unreadable marker must be rejected, not silently treated as missing"
    );
    let detail = reason.unwrap();
    assert!(
        detail.contains("not readable") || detail.contains("cannot be inspected"),
        "expected a read/inspection failure, got: {detail}"
    );
}

/// An unreadable `.luther` parent directory (permission denied on the parent)
/// must be rejected by verification, not silently treated as "the marker is
/// absent". This guards the parent-metadata path.
#[cfg(unix)]
#[test]
fn verify_rejects_unreadable_luther_parent_permission_denied() {
    if is_running_as_root() {
        eprintln!("skipping permission-denied test under root (root bypasses DAC)");
        return;
    }
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    write_workspace_owner_marker(workspace, "run-parent-perm").expect("publish");
    let luther = workspace.join(".luther");
    // Strip execute+read permission on the .luther directory so listing and
    // stat through it fail. The marker file itself remains readable in
    // isolation, but it cannot be reached.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&luther, std::fs::Permissions::from_mode(0o000)).unwrap();
    let reason = verify_workspace_ownership_marker(workspace, "run-parent-perm");
    assert!(
        reason.is_some(),
        "unreadable .luther parent must be rejected, not silently treated as symlink-absent"
    );
    // Restore so tempdir cleanup can remove the directory.
    let _ = std::fs::set_permissions(&luther, std::fs::Permissions::from_mode(0o755));
}

/// A marker path whose `symlink_metadata` fails with PermissionDenied (e.g. the
/// parent directory is mode 0o000 so even stat is denied) must be rejected, not
/// treated as "missing". This directly exercises the typed inspection change in
/// `verify_marker_file`: only `NotFound` means absent.
#[cfg(unix)]
#[test]
fn verify_marker_file_rejects_permission_denied_metadata() {
    if is_running_as_root() {
        eprintln!("skipping permission-denied test under root (root bypasses DAC)");
        return;
    }
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    let luther = workspace.join(".luther");
    std::fs::create_dir_all(&luther).unwrap();
    let marker = luther.join("workspace-owner");
    std::fs::write(&marker, "run-stat-perm").unwrap();
    // Make the parent .luther unreadable so stat of the child is denied.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&luther, std::fs::Permissions::from_mode(0o000)).unwrap();
    // verify_marker_file is pub(crate); reach it via the public verify path
    // which canonicalizes first. Because .luther is mode 000, canonicalize of
    // the marker path fails and verification rejects.
    let reason = verify_workspace_ownership_marker(workspace, "run-stat-perm");
    assert!(reason.is_some(), "permission-denied marker must reject");
    let _ = std::fs::set_permissions(&luther, std::fs::Permissions::from_mode(0o755));
}

/// Whether the test process is running as the superuser (uid 0), in which case
/// Unix DAC permission checks are bypassed and permission-denied tests are not
/// meaningful.
#[cfg(unix)]
fn is_running_as_root() -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .is_some_and(|output| output.status.success() && output.stdout == b"0\n")
}

// ---------------------------------------------------------------------------
// Issue 158 finding 3: an empty `.luther` directory is NOT interrupted
// publication evidence. Only a `.luther` containing same-run temp marker
// files proves a prior attempt by THIS launch was interrupted. An empty
// `.luther` carries no provenance tying the workspace to this run, so a
// pre-existing workspace with an empty `.luther` must NOT be silently
// adopted/claimed.
// ---------------------------------------------------------------------------

#[test]
fn provision_rejects_preexisting_workspace_with_empty_luther_dir() {
    // A pre-existing workspace directory containing an empty `.luther` (no
    // temp marker files) must NOT be claimable, because an empty `.luther`
    // is not interrupted-publication evidence.
    let dir = tempfile::tempdir().expect("workspace parent");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("create pre-existing workspace");
    std::fs::create_dir_all(workspace.join(".luther")).expect("create empty .luther");
    let error = crate::engine::continuation::provision_workspace_owner_marker(
        &workspace,
        "run-empty-luther",
    )
    .expect_err("empty .luther must not be claimable");
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(
        error.to_string().contains("refusing to adopt")
            || error.to_string().contains("without ownership marker"),
        "empty .luther must be rejected as non-evidence, got: {error}"
    );
    // No marker was written.
    assert!(!workspace.join(".luther/workspace-owner").exists());
}

#[test]
fn provision_claims_workspace_with_luther_containing_same_run_temp() {
    // A `.luther` containing only a same-run temp marker file IS claimable:
    // it proves a prior attempt by THIS launch was interrupted.
    let dir = tempfile::tempdir().expect("workspace parent");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("create pre-existing workspace");
    let luther = workspace.join(".luther");
    std::fs::create_dir_all(&luther).expect("create .luther");
    std::fs::write(
        luther.join(".workspace-owner.tmp.interrupted"),
        "run-recovery",
    )
    .expect("write same-run temp marker");
    crate::engine::continuation::provision_workspace_owner_marker(&workspace, "run-recovery")
        .expect("same-run interrupted-publication evidence allows claim");
    assert_eq!(
        std::fs::read_to_string(workspace.join(".luther/workspace-owner")).unwrap(),
        "run-recovery"
    );
}

#[test]
fn provision_rejects_workspace_with_luther_containing_foreign_run_temp() {
    // A `.luther` containing a temp marker that belongs to a DIFFERENT run
    // must NOT be claimable by this run.
    let dir = tempfile::tempdir().expect("workspace parent");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("create pre-existing workspace");
    let luther = workspace.join(".luther");
    std::fs::create_dir_all(&luther).expect("create .luther");
    std::fs::write(
        luther.join(".workspace-owner.tmp.interrupted"),
        "run-foreign",
    )
    .expect("write foreign-run temp marker");
    let error =
        crate::engine::continuation::provision_workspace_owner_marker(&workspace, "run-this")
            .expect_err("foreign-run temp must not be claimable");
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    // No marker was written.
    assert!(!workspace.join(".luther/workspace-owner").exists());
}
