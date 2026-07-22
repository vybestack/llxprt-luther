//! Workspace owner marker and cleanup-failure-abandonment workspace ownership tests.

use std::path::Path;

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

// ---------------------------------------------------------------------------
// Crash-safe no-replace publication tests
// ---------------------------------------------------------------------------

/// Helper: count temp (`.workspace-owner.tmp.*`) files left inside `.luther`.
fn count_temp_files(workspace: &Path) -> usize {
    let luther = workspace.join(".luther");
    std::fs::read_dir(&luther)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .map(|name| name.starts_with(".workspace-owner.tmp."))
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0)
}

/// After a successful publication, no temp file must remain inside `.luther`.
/// A leftover temp would indicate the cleanup step was skipped.
#[test]
fn marker_publishes_without_leaving_temp_file() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    write_workspace_owner_marker(workspace, "run-cleanup").expect("publish");
    assert_eq!(
        count_temp_files(workspace),
        0,
        "no temp file must remain after a successful publish"
    );
    let marker = workspace.join(".luther").join("workspace-owner");
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "run-cleanup");
}

/// An interrupted temp (simulated by a pre-existing temp file in `.luther`) must
/// never result in an empty or partial final marker. The final marker must be
/// absent, and a subsequent clean publish must succeed with full content.
#[test]
fn marker_no_empty_final_on_interrupted_temp() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    let luther = workspace.join(".luther");
    std::fs::create_dir_all(&luther).unwrap();
    // Simulate a crash after temp creation but before hard-link: leave a
    // stale, partially-written temp file behind.
    let stale_temp = luther.join(".workspace-owner.tmp.deadbeef");
    std::fs::write(&stale_temp, "").unwrap();
    // The final marker must not exist yet.
    let marker = luther.join("workspace-owner");
    assert!(
        !marker.exists(),
        "final marker must be absent after an interrupted temp"
    );
    // A clean publish must succeed and write full content.
    write_workspace_owner_marker(workspace, "run-interrupted").expect("publish after stale temp");
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "run-interrupted");
}

/// Simulate a crash that leaves only a temp file: verify the final marker is a
/// regular file (not empty, not partial) once published, and that an interrupted
/// path never produces an empty final marker.
#[test]
fn marker_final_is_regular_file_after_publish() {
    use std::fs;
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    write_workspace_owner_marker(workspace, "run-regular").expect("publish");
    let marker = workspace.join(".luther").join("workspace-owner");
    let meta = fs::symlink_metadata(&marker).expect("marker metadata");
    assert!(
        meta.is_file(),
        "final marker must be a regular file, got {:?}",
        meta.file_type()
    );
    assert!(!meta.file_type().is_symlink());
    assert!(meta.len() > 0, "final marker must not be empty");
}

/// Concurrent writers racing to publish a brand-new marker must produce exactly
/// one winner and a single consistent final marker, with no leftover temp files
/// and no empty/partial final content.
#[test]
fn marker_concurrent_writers_one_consistent_final() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let dir = Arc::new(tempfile::tempdir().expect("workspace"));
    let workspace = dir.path().to_path_buf();
    let errors = Arc::new(AtomicUsize::new(0));
    let successes = Arc::new(AtomicUsize::new(0));

    // Many threads all publishing the *same* run id: at most one can perform the
    // initial hard-link; the rest either hit the AlreadyExists branch and
    // validate the same-owner content, or lose the link race and validate.
    let mut handles = Vec::new();
    for _ in 0..16 {
        let ws = workspace.clone();
        let errors = Arc::clone(&errors);
        let successes = Arc::clone(&successes);
        handles.push(std::thread::spawn(
            move || match write_workspace_owner_marker(&ws, "run-race") {
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
    // All writers publish the same owner, so every writer must succeed
    // (idempotent re-run for same owner).
    assert_eq!(
        successes.load(Ordering::SeqCst),
        16,
        "same-owner concurrent writes must all succeed idempotently"
    );
    assert_eq!(
        errors.load(Ordering::SeqCst),
        0,
        "no same-owner write must fail"
    );
    // Exactly one consistent final marker, no leftover temps.
    assert_eq!(count_temp_files(&workspace), 0, "no leftover temp files");
    let marker = workspace.join(".luther").join("workspace-owner");
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        "run-race",
        "final marker content must be the single winner"
    );
}

/// Concurrent writers with *different* run ids racing on a brand-new workspace:
/// exactly one wins and claims the marker; the rest are rejected with
/// `AlreadyExists`, and the final marker is non-empty and consistent with no
/// leftover temps.
#[test]
fn marker_concurrent_writers_distinct_owners_one_winner() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let dir = Arc::new(tempfile::tempdir().expect("workspace"));
    let workspace = dir.path().to_path_buf();
    let errors = Arc::new(AtomicUsize::new(0));
    let successes = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for i in 0..12 {
        let ws = workspace.clone();
        let run_id = format!("run-distinct-{i}");
        let errors = Arc::clone(&errors);
        let successes = Arc::clone(&successes);
        handles.push(std::thread::spawn(
            move || match write_workspace_owner_marker(&ws, &run_id) {
                Ok(()) => {
                    successes.fetch_add(1, Ordering::SeqCst);
                }
                Err(err) => {
                    assert_eq!(
                        err.kind(),
                        std::io::ErrorKind::AlreadyExists,
                        "loser must be rejected with AlreadyExists"
                    );
                    errors.fetch_add(1, Ordering::SeqCst);
                }
            },
        ));
    }
    for handle in handles {
        handle.join().expect("thread panic");
    }
    assert_eq!(
        successes.load(Ordering::SeqCst),
        1,
        "exactly one distinct-owner writer must win"
    );
    assert_eq!(
        errors.load(Ordering::SeqCst),
        11,
        "the losing writers must be rejected"
    );
    assert_eq!(count_temp_files(&workspace), 0, "no leftover temp files");
    let marker = workspace.join(".luther").join("workspace-owner");
    let contents = std::fs::read_to_string(&marker).unwrap();
    assert!(
        contents.starts_with("run-distinct-"),
        "final marker must belong to one of the writers: {contents}"
    );
    assert!(!contents.is_empty());
}

/// The durability path must complete without error: a successful publish should
/// return `Ok(())` and the marker should be readable and durable on disk. This
/// exercises the `write_all` + `sync_all` + `hard_link` + temp removal +
/// directory fsync path end-to-end.
#[test]
fn marker_durability_path_publishes_durable_content() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    // A realistic run id with surrounding whitespace must be written verbatim
    // (not trimmed) to the file.
    let run_id = "run-durable-001";
    write_workspace_owner_marker(workspace, run_id).expect("durable publish");
    let marker = workspace.join(".luther").join("workspace-owner");
    let on_disk = std::fs::read_to_string(&marker).unwrap();
    assert_eq!(on_disk, run_id, "marker content must match run id verbatim");
    // Re-verification must trust the durable marker.
    assert!(verify_workspace_ownership_marker(workspace, run_id).is_none());
    // No temp left behind.
    assert_eq!(count_temp_files(workspace), 0);
}

/// The publication must write the exact bytes of `run_id` without trimming,
/// padding, or transformation, so the durable record is byte-identical to the
/// claimed owner.
#[test]
fn marker_writes_exact_run_id_bytes() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    write_workspace_owner_marker(workspace, "exact-bytes-123").expect("publish");
    let marker = workspace.join(".luther").join("workspace-owner");
    let bytes = std::fs::read(&marker).unwrap();
    assert_eq!(
        bytes, b"exact-bytes-123",
        "marker bytes must equal the run id exactly"
    );
}

/// A re-publication for the same owner must be idempotent at the byte level:
/// the marker content and length are unchanged, and no temp file is produced.
#[test]
fn marker_republication_same_owner_preserves_bytes() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    write_workspace_owner_marker(workspace, "preserve-bytes").expect("first publish");
    let marker = workspace.join(".luther").join("workspace-owner");
    let before = std::fs::read(&marker).unwrap();
    write_workspace_owner_marker(workspace, "preserve-bytes").expect("idempotent republication");
    let after = std::fs::read(&marker).unwrap();
    assert_eq!(before, after, "marker bytes must be unchanged");
    assert_eq!(count_temp_files(workspace), 0, "no temp on idempotent path");
}

#[test]
fn marker_republication_rejects_trailing_newline() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    let marker = workspace.join(".luther").join("workspace-owner");
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::write(&marker, "exact-owner\n").unwrap();
    let err = write_workspace_owner_marker(workspace, "exact-owner")
        .expect_err("non-exact marker bytes must be rejected");
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    let reason = verify_workspace_ownership_marker(workspace, "exact-owner")
        .expect("verification must reject non-exact marker bytes");
    assert!(reason.contains("belongs to run"), "{reason}");
}

/// After publication, a foreign-marker substitution (different owner) must be
/// detected by re-publication and rejected without overwriting the foreign
/// marker.
#[test]
fn marker_republication_rejects_foreign_owner_without_overwrite() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    write_workspace_owner_marker(workspace, "owner-original").expect("publish");
    let marker = workspace.join(".luther").join("workspace-owner");
    // Tamper: overwrite with a foreign owner.
    std::fs::write(&marker, "owner-impostor").unwrap();
    let err = write_workspace_owner_marker(workspace, "owner-original")
        .expect_err("foreign marker must be rejected");
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    // The foreign content is preserved (no overwrite).
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "owner-impostor");
    assert_eq!(count_temp_files(workspace), 0, "no temp on rejection path");
}

#[test]
fn provision_marker_is_concurrently_idempotent_for_same_owner() {
    // Issue 158 finding 3: provision targets a non-existent workspace subdir
    // so exactly one thread observes the atomic `create_dir` and the rest see
    // either an interrupted-publication (`.luther` temp) or a completed marker.
    let dir = tempfile::tempdir().expect("workspace parent");
    let workspace = std::sync::Arc::new(dir.path().join("ws"));
    let mut threads = Vec::new();
    for _ in 0..12 {
        let workspace = std::sync::Arc::clone(&workspace);
        threads.push(std::thread::spawn(move || {
            crate::engine::continuation::provision_workspace_owner_marker(
                &workspace,
                "concurrent-owner",
            )
        }));
    }
    for thread in threads {
        thread
            .join()
            .expect("provision thread")
            .expect("same-owner provision");
    }
    assert!(verify_workspace_ownership_marker(&workspace, "concurrent-owner").is_none());
}

#[cfg(unix)]
#[test]
fn provision_rejects_symlinked_ancestor() {
    let root = tempfile::tempdir().expect("root");
    let redirected = tempfile::tempdir().expect("redirect target");
    let link = root.path().join("redirect");
    std::os::unix::fs::symlink(redirected.path(), &link).unwrap();
    let workspace = link.join("child");
    let error =
        crate::engine::continuation::provision_workspace_owner_marker(&workspace, "ancestor-owner")
            .expect_err("symlinked ancestor must be rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(!redirected
        .path()
        .join("child/.luther/workspace-owner")
        .exists());
}

// ---------------------------------------------------------------------------
// Symlinked workspace root rejection (Blocker 2 final)
// ---------------------------------------------------------------------------

/// A symlinked workspace root must be rejected by `write_workspace_owner_marker`
/// before canonicalization, so the marker is never written through a redirected
/// root.
#[cfg(unix)]
#[test]
fn write_rejects_symlinked_workspace_root() {
    let real = tempfile::tempdir().expect("real workspace");
    let link_parent = tempfile::tempdir().expect("link parent");
    let link = link_parent.path().join("ws-link");
    std::os::unix::fs::symlink(real.path(), &link).unwrap();

    let err = write_workspace_owner_marker(&link, "run-root-symlink")
        .expect_err("symlinked workspace root must be rejected on write");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(
        err.to_string().contains("symlink"),
        "expected symlink rejection, got: {err}"
    );
    // No marker must have been written through the symlink.
    assert!(!real.path().join(".luther").join("workspace-owner").exists());
}

/// A symlinked workspace root must be rejected by
/// `verify_workspace_ownership_marker` before canonicalization, so verification
/// never trusts a marker that lives behind a redirected root.
#[cfg(unix)]
#[test]
fn verify_rejects_symlinked_workspace_root() {
    let real = tempfile::tempdir().expect("real workspace");
    // Set up a valid-looking marker in the real workspace.
    write_workspace_owner_marker(real.path(), "run-root-symlink-verify").unwrap();
    let link_parent = tempfile::tempdir().expect("link parent");
    let link = link_parent.path().join("ws-link");
    std::os::unix::fs::symlink(real.path(), &link).unwrap();

    let reason = verify_workspace_ownership_marker(&link, "run-root-symlink-verify");
    assert!(
        reason.is_some(),
        "symlinked workspace root must be rejected on verify"
    );
    let detail = reason.unwrap();
    assert!(
        detail.contains("symlink"),
        "expected symlink rejection, got: {detail}"
    );
}

/// A normal (non-symlink, owned) workspace verifies successfully: the marker
/// matches and no symlink is present.
#[cfg(unix)]
#[test]
fn verify_accepts_owned_normal_workspace() {
    let dir = tempfile::tempdir().expect("workspace");
    write_workspace_owner_marker(dir.path(), "run-revalidate").unwrap();
    assert!(verify_workspace_ownership_marker(dir.path(), "run-revalidate").is_none());
}

/// A child workspace reached via a symlinked parent directory in the path must
/// be rejected: the root symlink check inspects the leaf path directly.
#[cfg(unix)]
#[test]
fn write_rejects_symlinked_root_with_nested_path() {
    let real = tempfile::tempdir().expect("real workspace");
    let link_parent = tempfile::tempdir().expect("link parent");
    let link = link_parent.path().join("ws-nested-link");
    std::os::unix::fs::symlink(real.path(), &link).unwrap();
    // The workspace root passed is the symlink itself.
    let err = write_workspace_owner_marker(&link, "run-nested")
        .expect_err("symlinked root must be rejected even with nested structure");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("symlink"));
}

/// A real directory (not a symlink) that contains a valid marker must pass both
/// write (idempotent) and verify, proving the new root check does not produce
/// false positives for the normal case.
#[test]
fn real_directory_root_passes_write_and_verify() {
    let dir = tempfile::tempdir().expect("workspace");
    let workspace = dir.path();
    write_workspace_owner_marker(workspace, "run-real-root").expect("write");
    assert!(verify_workspace_ownership_marker(workspace, "run-real-root").is_none());
    // Idempotent re-write on a real root succeeds.
    write_workspace_owner_marker(workspace, "run-real-root").expect("idempotent write");
}

/// Swap-oriented test: after a successful write to a real workspace, replacing
/// the workspace root with a symlink must cause verification to fail (the marker
/// now lives behind a redirected root).
#[cfg(unix)]
#[test]
fn verify_fails_after_root_replaced_with_symlink() {
    let real = tempfile::tempdir().expect("real workspace");
    write_workspace_owner_marker(real.path(), "run-swap").expect("write to real root");
    // Verification succeeds on the real root.
    assert!(verify_workspace_ownership_marker(real.path(), "run-swap").is_none());

    // Create a symlink to the real workspace and verify through it: must fail.
    let link_parent = tempfile::tempdir().expect("link parent");
    let link = link_parent.path().join("ws-swap-link");
    std::os::unix::fs::symlink(real.path(), &link).unwrap();
    let reason = verify_workspace_ownership_marker(&link, "run-swap");
    assert!(
        reason.is_some(),
        "verify through a symlinked root must fail after a swap"
    );
    assert!(reason.unwrap().contains("symlink"));
}
