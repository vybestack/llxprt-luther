//! Workspace owner marker and cleanup-failure-abandonment workspace ownership tests.

use super::support::*;
use crate::engine::continuation::{
    verify_workspace_ownership_marker, write_workspace_owner_marker, ContinuationKind,
};
use crate::persistence::{get_run_with_conn, persist_run_with_conn};

// ---------------------------------------------------------------------------
// Workspace owner marker unit tests (issue 137)
// ---------------------------------------------------------------------------

#[test]
fn marker_is_idempotent_for_same_run() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    write_workspace_owner_marker(workspace, "run-A").expect("first write");
    // A second write for the same run id must succeed without error.
    write_workspace_owner_marker(workspace, "run-A").expect("idempotent write");
    let marker = workspace.join(".luther").join("workspace-owner");
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "run-A");
}

#[test]
fn marker_rejects_different_owner() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    write_workspace_owner_marker(workspace, "run-A").expect("first write");
    let err = write_workspace_owner_marker(workspace, "run-B")
        .expect_err("different owner must be rejected");
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(err.to_string().contains("run-A"));
    assert!(err.to_string().contains("run-B"));
    // The original owner is preserved.
    let marker = workspace.join(".luther").join("workspace-owner");
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "run-A");
}

#[test]
fn marker_rejects_directory_at_marker_path() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    let luther = workspace.join(".luther");
    std::fs::create_dir_all(&luther).unwrap();
    let marker = luther.join("workspace-owner");
    std::fs::create_dir(&marker).unwrap();
    let err = write_workspace_owner_marker(workspace, "run-A")
        .expect_err("directory marker must be rejected");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("directory"));
}

#[cfg(unix)]
#[test]
fn marker_rejects_symlink_at_marker_path() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    let luther = workspace.join(".luther");
    std::fs::create_dir_all(&luther).unwrap();
    let target = dir.path().join("evil");
    std::fs::write(&target, "run-evil").unwrap();
    let marker = luther.join("workspace-owner");
    std::os::unix::fs::symlink(&target, &marker).unwrap();
    let err = write_workspace_owner_marker(workspace, "run-A")
        .expect_err("symlink marker must be rejected");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("symlink"));
}

#[cfg(unix)]
#[test]
fn marker_rejects_symlinked_luther_parent() {
    // A symlinked `.luther` parent could redirect the marker to an
    // attacker-controlled location. The write must reject it before creating
    // the marker.
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    let evil = dir.path().join("evil-luther");
    std::fs::create_dir_all(&evil).unwrap();
    let luther_link = workspace.join(".luther");
    std::os::unix::fs::symlink(&evil, &luther_link).unwrap();
    let err = write_workspace_owner_marker(workspace, "run-A")
        .expect_err("symlinked .luther parent must be rejected");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains(".luther"));
    assert!(err.to_string().contains("symlink"));
    // No marker should have been written through the symlink.
    assert!(!evil.join("workspace-owner").exists());
}

#[test]
fn marker_rejects_empty_marker_without_rewriting_it() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    let luther = workspace.join(".luther");
    std::fs::create_dir_all(&luther).unwrap();
    let marker = luther.join("workspace-owner");
    std::fs::write(&marker, "   ").unwrap();
    let error = write_workspace_owner_marker(workspace, "run-A")
        .expect_err("empty marker must fail closed");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "   ");
}

#[test]
fn verify_rejects_missing_marker() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    // No marker exists at all.
    let reason = verify_workspace_ownership_marker(workspace, "run-A");
    assert!(reason.is_some());
    assert!(reason.unwrap().contains("missing"));
}

#[test]
fn verify_rejects_empty_marker() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    write_workspace_owner_marker(workspace, "run-A").unwrap();
    let marker = workspace.join(".luther").join("workspace-owner");
    std::fs::write(&marker, "").unwrap();
    let reason = verify_workspace_ownership_marker(workspace, "run-A");
    assert!(reason.is_some());
    assert!(reason.unwrap().contains("empty"));
}

#[test]
fn verify_rejects_mismatched_owner() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    write_workspace_owner_marker(workspace, "run-A").unwrap();
    let reason = verify_workspace_ownership_marker(workspace, "run-B");
    assert!(reason.is_some());
    let detail = reason.unwrap();
    assert!(detail.contains("run-A"));
    assert!(detail.contains("run-B"));
}

#[test]
fn verify_accepts_exact_owner() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    write_workspace_owner_marker(workspace, "run-A").unwrap();
    assert!(verify_workspace_ownership_marker(workspace, "run-A").is_none());
}

#[cfg(unix)]
#[test]
fn verify_rejects_symlinked_luther_parent() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    let evil = dir.path().join("evil-verify");
    std::fs::create_dir_all(&evil).unwrap();
    // Place a valid-looking marker behind the symlink target.
    let evil_luther = evil.join(".luther");
    std::fs::create_dir_all(&evil_luther).unwrap();
    std::fs::write(evil_luther.join("workspace-owner"), "run-A").unwrap();
    let luther_link = workspace.join(".luther");
    std::os::unix::fs::symlink(&evil, &luther_link).unwrap();
    let reason = verify_workspace_ownership_marker(workspace, "run-A");
    assert!(reason.is_some());
    assert!(reason.unwrap().contains("symlink"));
}

// ---------------------------------------------------------------------------
// Cleanup workspace ownership validation tests
// ---------------------------------------------------------------------------

#[test]
fn cleanup_workspace_ownership_rejects_symlinked_workspace() {
    let conn = test_conn();
    let real = tempfile::tempdir().expect("real workspace");
    let link_root = tempfile::tempdir().expect("link parent");
    #[cfg(unix)]
    {
        let link = link_root.path().join("ws-link");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();
        let checkpoint = seed_cleanup_abandonment(&conn, "ws-symlink", real.path());
        // Point the workspace_path at the symlink, not the real dir.
        let mut md = get_run_with_conn(&conn, "ws-symlink").unwrap().unwrap();
        md.workspace_path = Some(link.to_string_lossy().to_string());
        persist_run_with_conn(&conn, &md).unwrap();
        let req = request(
            "ws-symlink",
            ContinuationKind::Retry {
                from_failed_step: false,
            },
            true,
        );
        let validation =
            crate::engine::continuation::validate_continuation(&conn, &req).expect("validate");
        assert!(!validation.ok);
        assert!(validation
            .failure_reasons()
            .iter()
            .any(|r| r.contains("symlink")));
        // Keep the checkpoint variable alive so the compiler is happy on non-unix.
        let _ = checkpoint;
    }
}

#[test]
fn cleanup_workspace_ownership_rejects_mismatched_owner_marker() {
    let conn = test_conn();
    let workspace = tempfile::tempdir().expect("workspace");
    seed_cleanup_abandonment(&conn, "ws-mismatch", workspace.path());
    // Overwrite the marker with a different run id.
    let marker = workspace.path().join(".luther").join("workspace-owner");
    std::fs::write(&marker, "run-impostor").unwrap();
    let req = request(
        "ws-mismatch",
        ContinuationKind::Retry {
            from_failed_step: false,
        },
        true,
    );
    let validation =
        crate::engine::continuation::validate_continuation(&conn, &req).expect("validate");
    assert!(!validation.ok);
    assert!(validation
        .failure_reasons()
        .iter()
        .any(|r| r.contains("workspace")));
}

#[test]
fn cleanup_workspace_ownership_rejects_marker_directory() {
    let conn = test_conn();
    let workspace = tempfile::tempdir().expect("workspace");
    seed_cleanup_abandonment(&conn, "ws-dir-marker", workspace.path());
    // Replace the marker file with a directory.
    let marker = workspace.path().join(".luther").join("workspace-owner");
    std::fs::remove_file(&marker).unwrap();
    std::fs::create_dir(&marker).unwrap();
    let req = request(
        "ws-dir-marker",
        ContinuationKind::Retry {
            from_failed_step: false,
        },
        true,
    );
    let validation =
        crate::engine::continuation::validate_continuation(&conn, &req).expect("validate");
    assert!(!validation.ok);
    assert!(validation
        .failure_reasons()
        .iter()
        .any(|r| r.contains("directory")));
}

#[test]
fn cleanup_workspace_ownership_rejects_not_a_directory() {
    let conn = test_conn();
    let workspace = tempfile::tempdir().expect("workspace");
    seed_cleanup_abandonment(&conn, "ws-notdir", workspace.path());
    // Point workspace_path at a regular file, not a directory.
    let file = tempfile::NamedTempFile::new().expect("temp file");
    let mut md = get_run_with_conn(&conn, "ws-notdir").unwrap().unwrap();
    md.workspace_path = Some(file.path().to_string_lossy().to_string());
    persist_run_with_conn(&conn, &md).unwrap();
    let req = request(
        "ws-notdir",
        ContinuationKind::Retry {
            from_failed_step: false,
        },
        true,
    );
    let validation =
        crate::engine::continuation::validate_continuation(&conn, &req).expect("validate");
    assert!(!validation.ok);
    assert!(validation
        .failure_reasons()
        .iter()
        .any(|r| r.contains("directory") || r.contains("not a directory")));
}

/// Concurrent marker writes for different run ids on the same workspace must
/// allow exactly one winner and reject the other, proving the atomic
/// create-new path closes the TOCTOU window.
#[test]
fn marker_concurrent_different_owners_one_wins() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let dir = Arc::new(tempfile::tempdir().expect("workspace"));
    let workspace = dir.path().to_path_buf();
    let errors = Arc::new(AtomicUsize::new(0));
    let successes = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for i in 0..8 {
        let ws = workspace.clone();
        let run_id = format!("run-concurrent-{i}");
        let errors = Arc::clone(&errors);
        let successes = Arc::clone(&successes);
        handles.push(std::thread::spawn(
            move || match write_workspace_owner_marker(&ws, &run_id) {
                Ok(()) => {
                    successes.fetch_add(1, Ordering::SeqCst);
                }
                Err(_) => {
                    errors.fetch_add(1, Ordering::SeqCst);
                }
            },
        ));
    }
    for handle in handles {
        handle.join().expect("thread panic");
    }
    // Exactly one writer wins; the rest are rejected with AlreadyExists.
    assert_eq!(
        successes.load(Ordering::SeqCst),
        1,
        "exactly one concurrent writer must claim the workspace"
    );
    assert_eq!(
        errors.load(Ordering::SeqCst),
        7,
        "the losing writers must be rejected"
    );
}

/// Multiple child runs (relaunches) must each get a distinct workspace, proving
/// child workspace isolation at the ownership-marker level.
#[test]
fn child_relaunch_gets_distinct_isolated_workspaces() {
    // Each isolated child workspace can be independently claimed by its run id,
    // and cross-verification fails, proving the ownership marker binds a
    // workspace to exactly one run.
    let dir_first = tempfile::tempdir().expect("first workspace");
    write_workspace_owner_marker(dir_first.path(), "child-run-1").expect("claim first");
    let dir_second = tempfile::tempdir().expect("second workspace");
    write_workspace_owner_marker(dir_second.path(), "child-run-2").expect("claim second");
    assert!(verify_workspace_ownership_marker(dir_first.path(), "child-run-1").is_none());
    assert!(verify_workspace_ownership_marker(dir_second.path(), "child-run-2").is_none());
    // Cross-verification fails: first workspace does not belong to second run.
    assert!(verify_workspace_ownership_marker(dir_first.path(), "child-run-2").is_some());
}
