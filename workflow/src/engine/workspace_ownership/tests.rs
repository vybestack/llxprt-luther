//! Tests for the cohesive workspace ownership abstraction (two-phase evidence).

use std::path::Path;

use super::{
    ensure_durable_workspace_ownership, has_trusted_workspace_ownership,
    promote_workspace_owner_marker, provision_workspace_owner_marker, verify_workspace_ownership,
};

fn write_bootstrap_marker(workspace: &Path, run_id: &str) {
    let luther = workspace.join(".luther");
    std::fs::create_dir_all(&luther).unwrap();
    std::fs::write(luther.join("workspace-owner"), run_id).unwrap();
}

fn write_durable_marker(workspace: &Path, run_id: &str) {
    let durable = workspace.join(".git/luther");
    std::fs::create_dir_all(&durable).unwrap();
    std::fs::write(durable.join("workspace-owner"), run_id).unwrap();
}

fn init_git(workspace: &Path) {
    std::fs::create_dir_all(workspace.join(".git")).unwrap();
}

// ---------------------------------------------------------------------------
// Provision + verify (bootstrap-only)
// ---------------------------------------------------------------------------

#[test]
fn provision_writes_bootstrap_marker() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws");
    provision_workspace_owner_marker(&ws, "run-A").unwrap();
    let marker = ws.as_path().join(".luther/workspace-owner");
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "run-A");
}

#[test]
fn verify_accepts_bootstrap_only() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws");
    provision_workspace_owner_marker(&ws, "run-A").unwrap();
    assert_eq!(verify_workspace_ownership(ws.as_path(), "run-A"), None);
    assert!(has_trusted_workspace_ownership(ws.as_path(), "run-A"));
}

#[test]
fn verify_rejects_missing_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let reason = verify_workspace_ownership(dir.path(), "run-A");
    assert!(reason.is_some());
    assert!(reason.unwrap().contains("missing"));
    assert!(!has_trusted_workspace_ownership(dir.path(), "run-A"));
}

#[test]
fn verify_rejects_foreign_bootstrap() {
    let dir = tempfile::tempdir().unwrap();
    write_bootstrap_marker(dir.path(), "run-foreign");
    let reason = verify_workspace_ownership(dir.path(), "run-A");
    assert!(reason.is_some());
    assert!(reason.unwrap().contains("run-foreign"));
}

// ---------------------------------------------------------------------------
// Promotion (bootstrap -> durable)
// ---------------------------------------------------------------------------

#[test]
fn promote_creates_durable_from_verified_bootstrap() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws");
    provision_workspace_owner_marker(&ws, "run-A").unwrap();
    init_git(ws.as_path());
    promote_workspace_owner_marker(ws.as_path(), "run-A").unwrap();
    let durable = ws.as_path().join(".git/luther/workspace-owner");
    assert_eq!(std::fs::read_to_string(&durable).unwrap(), "run-A");
    // Bootstrap remains intact.
    let bootstrap = ws.as_path().join(".luther/workspace-owner");
    assert_eq!(std::fs::read_to_string(&bootstrap).unwrap(), "run-A");
}

#[test]
fn promote_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws");
    provision_workspace_owner_marker(&ws, "run-A").unwrap();
    init_git(ws.as_path());
    promote_workspace_owner_marker(ws.as_path(), "run-A").unwrap();
    promote_workspace_owner_marker(ws.as_path(), "run-A").unwrap();
    let durable = ws.as_path().join(".git/luther/workspace-owner");
    assert_eq!(std::fs::read_to_string(&durable).unwrap(), "run-A");
}

#[test]
fn promote_preserves_exact_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws");
    provision_workspace_owner_marker(&ws, "exact-bytes-123").unwrap();
    init_git(ws.as_path());
    promote_workspace_owner_marker(ws.as_path(), "exact-bytes-123").unwrap();
    let bootstrap = ws.as_path().join(".luther/workspace-owner");
    let durable = ws.as_path().join(".git/luther/workspace-owner");
    assert_eq!(
        std::fs::read(&bootstrap).unwrap(),
        std::fs::read(&durable).unwrap(),
    );
}

#[test]
fn promote_refuses_without_bootstrap() {
    let dir = tempfile::tempdir().unwrap();
    init_git(dir.path());
    let err = promote_workspace_owner_marker(dir.path(), "run-A").unwrap_err();
    assert!(err.to_string().contains("bootstrap"));
}

#[test]
fn promote_refuses_foreign_bootstrap() {
    let dir = tempfile::tempdir().unwrap();
    write_bootstrap_marker(dir.path(), "run-foreign");
    init_git(dir.path());
    let err = promote_workspace_owner_marker(dir.path(), "run-A").unwrap_err();
    assert!(err.to_string().contains("run-foreign"));
}

#[test]
fn promote_refuses_without_git() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws");
    provision_workspace_owner_marker(&ws, "run-A").unwrap();
    let err = promote_workspace_owner_marker(ws.as_path(), "run-A").unwrap_err();
    assert!(
        err.to_string().contains("Git") || err.to_string().contains(".git"),
        "expected a Git-related failure, got: {err}"
    );
}

#[test]
fn promote_refuses_symlinked_git() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws");
    provision_workspace_owner_marker(&ws, "run-A").unwrap();
    let evil = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(evil.path(), ws.as_path().join(".git")).unwrap();
    let err = promote_workspace_owner_marker(ws.as_path(), "run-A").unwrap_err();
    assert!(
        err.to_string().contains("symlink")
            || err.to_string().contains("Git")
            || err.to_string().contains(".git")
            || err.to_string().to_lowercase().contains("nofollow"),
        "expected a symlink/Git rejection, got: {err}"
    );
}

#[test]
fn promote_refuses_symlinked_durable_dir() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws");
    provision_workspace_owner_marker(&ws, "run-A").unwrap();
    init_git(ws.as_path());
    let evil = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(evil.path(), ws.as_path().join(".git/luther")).unwrap();
    let err = promote_workspace_owner_marker(ws.as_path(), "run-A").unwrap_err();
    assert!(
        err.to_string().contains("symlink")
            || err.to_string().contains("luther")
            || err.to_string().to_lowercase().contains("nofollow"),
        "expected a symlink/luther rejection, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Unified verification: at least one valid, none malformed
// ---------------------------------------------------------------------------

#[test]
fn verify_accepts_durable_only() {
    let dir = tempfile::tempdir().unwrap();
    init_git(dir.path());
    write_durable_marker(dir.path(), "run-A");
    // Bootstrap absent, durable present and valid.
    assert_eq!(verify_workspace_ownership(dir.path(), "run-A"), None);
}

#[test]
fn verify_accepts_both_present_and_valid() {
    let dir = tempfile::tempdir().unwrap();
    init_git(dir.path());
    write_bootstrap_marker(dir.path(), "run-A");
    write_durable_marker(dir.path(), "run-A");
    assert_eq!(verify_workspace_ownership(dir.path(), "run-A"), None);
}

#[test]
fn verify_rejects_when_bootstrap_foreign_durable_valid() {
    let dir = tempfile::tempdir().unwrap();
    init_git(dir.path());
    write_bootstrap_marker(dir.path(), "run-foreign");
    write_durable_marker(dir.path(), "run-A");
    let reason = verify_workspace_ownership(dir.path(), "run-A");
    assert!(reason.is_some());
    assert!(reason.unwrap().contains("run-foreign"));
}

#[test]
fn verify_rejects_when_durable_foreign_bootstrap_valid() {
    let dir = tempfile::tempdir().unwrap();
    init_git(dir.path());
    write_bootstrap_marker(dir.path(), "run-A");
    write_durable_marker(dir.path(), "run-foreign");
    let reason = verify_workspace_ownership(dir.path(), "run-A");
    assert!(reason.is_some());
}

#[test]
fn verify_rejects_bootstrap_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let luther = dir.path().join(".luther");
    std::fs::create_dir_all(&luther).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", luther.join("workspace-owner")).unwrap();
    let reason = verify_workspace_ownership(dir.path(), "run-A");
    assert!(reason.is_some());
    assert!(reason.unwrap().contains("symlink"));
}

#[test]
fn verify_rejects_durable_symlink() {
    let dir = tempfile::tempdir().unwrap();
    init_git(dir.path());
    let durable = dir.path().join(".git/luther");
    std::fs::create_dir_all(&durable).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", durable.join("workspace-owner")).unwrap();
    let reason = verify_workspace_ownership(dir.path(), "run-A");
    assert!(reason.is_some());
    assert!(reason.unwrap().contains("symlink"));
}

#[test]
fn verify_rejects_bootstrap_directory() {
    let dir = tempfile::tempdir().unwrap();
    let luther = dir.path().join(".luther");
    std::fs::create_dir_all(luther.join("workspace-owner")).unwrap();
    let reason = verify_workspace_ownership(dir.path(), "run-A");
    assert!(reason.is_some());
    assert!(reason.unwrap().contains("directory"));
}

#[test]
fn verify_rejects_empty_bootstrap() {
    let dir = tempfile::tempdir().unwrap();
    write_bootstrap_marker(dir.path(), "");
    let reason = verify_workspace_ownership(dir.path(), "run-A");
    assert!(reason.is_some());
    assert!(reason.unwrap().contains("empty"));
}

#[test]
fn verify_rejects_symlinked_git_directory() {
    let dir = tempfile::tempdir().unwrap();
    write_bootstrap_marker(dir.path(), "run-A");
    let evil = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(evil.path(), dir.path().join(".git")).unwrap();
    let reason = verify_workspace_ownership(dir.path(), "run-A");
    // Bootstrap is valid, but .git is a symlink -> fail closed.
    assert!(reason.is_some());
}

#[test]
fn verify_rejects_git_not_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    write_bootstrap_marker(dir.path(), "run-A");
    std::fs::write(dir.path().join(".git"), "not a dir").unwrap();
    let reason = verify_workspace_ownership(dir.path(), "run-A");
    assert!(reason.is_some());
}

// ---------------------------------------------------------------------------
// Read-only: verify never creates evidence
// ---------------------------------------------------------------------------

#[test]
fn verify_is_read_only_and_never_creates_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let _ = verify_workspace_ownership(dir.path(), "run-A");
    assert!(!dir.path().join(".luther").exists());
    assert!(!dir.path().join(".git").exists());
}

#[test]
fn has_trusted_is_read_only_and_never_creates_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let _ = has_trusted_workspace_ownership(dir.path(), "run-A");
    assert!(!dir.path().join(".luther").exists());
}

// ---------------------------------------------------------------------------
// Legacy bootstrap-only promotion (pre-durable workspaces)
// ---------------------------------------------------------------------------

#[test]
fn legacy_bootstrap_only_workspace_promotes_to_durable() {
    // Simulate a legacy workspace that only ever had a bootstrap marker and no
    // durable record. Promotion must still work from verified bootstrap.
    let dir = tempfile::tempdir().unwrap();
    write_bootstrap_marker(dir.path(), "legacy-run");
    init_git(dir.path());
    promote_workspace_owner_marker(dir.path(), "legacy-run").unwrap();
    assert_eq!(verify_workspace_ownership(dir.path(), "legacy-run"), None);
}

// ---------------------------------------------------------------------------
// End-to-end two-phase flow
// ---------------------------------------------------------------------------

#[test]
fn two_phase_flow_provision_then_promote_then_verify() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws");
    // Phase 1: bootstrap before git init.
    provision_workspace_owner_marker(&ws, "run-end-to-end").unwrap();
    assert_eq!(
        verify_workspace_ownership(ws.as_path(), "run-end-to-end"),
        None,
    );
    assert!(!ws.as_path().join(".git").exists());
    // Phase 2: git init, then promote exact bytes to durable.
    init_git(ws.as_path());
    promote_workspace_owner_marker(ws.as_path(), "run-end-to-end").unwrap();
    assert_eq!(
        verify_workspace_ownership(ws.as_path(), "run-end-to-end"),
        None,
    );
    // Removing bootstrap still trusts via durable.
    std::fs::remove_file(ws.as_path().join(".luther/workspace-owner")).unwrap();
    assert_eq!(
        verify_workspace_ownership(ws.as_path(), "run-end-to-end"),
        None,
    );
}

#[test]
fn removing_durable_keeps_trust_via_bootstrap() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws");
    provision_workspace_owner_marker(&ws, "run-A").unwrap();
    init_git(ws.as_path());
    promote_workspace_owner_marker(ws.as_path(), "run-A").unwrap();
    std::fs::remove_file(ws.as_path().join(".git/luther/workspace-owner")).unwrap();
    assert_eq!(verify_workspace_ownership(ws.as_path(), "run-A"), None);
}

#[test]
fn foreign_after_promote_replaces_durable_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws");
    provision_workspace_owner_marker(&ws, "run-A").unwrap();
    init_git(ws.as_path());
    promote_workspace_owner_marker(ws.as_path(), "run-A").unwrap();
    // Tamper: overwrite durable with a foreign owner.
    std::fs::write(
        ws.as_path().join(".git/luther/workspace-owner"),
        "run-foreign",
    )
    .unwrap();
    let reason = verify_workspace_ownership(ws.as_path(), "run-A");
    assert!(reason.is_some());
}

// ---------------------------------------------------------------------------
// ensure_durable_workspace_ownership: bootstrap-only pre-Git success (Fix 2)
// ---------------------------------------------------------------------------

#[test]
fn ensure_durable_succeeds_for_bootstrap_only_pre_git_state() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws");
    provision_workspace_owner_marker(&ws, "run-A").unwrap();
    // No .git directory exists; bootstrap-only evidence is trusted.
    assert!(!ws.as_path().join(".git").exists());
    ensure_durable_workspace_ownership(ws.as_path(), "run-A")
        .expect("verified bootstrap-only pre-Git state must succeed");
    // No durable evidence was created.
    assert!(!ws.as_path().join(".git/luther/workspace-owner").exists());
}

#[test]
fn ensure_durable_promotes_when_git_exists_and_no_durable() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws");
    provision_workspace_owner_marker(&ws, "run-A").unwrap();
    init_git(ws.as_path());
    ensure_durable_workspace_ownership(ws.as_path(), "run-A")
        .expect("promotes bootstrap to durable when .git exists");
    assert_eq!(
        std::fs::read(ws.as_path().join(".git/luther/workspace-owner")).unwrap(),
        b"run-A"
    );
}

#[test]
fn ensure_durable_is_idempotent_when_durable_exists() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws");
    provision_workspace_owner_marker(&ws, "run-A").unwrap();
    init_git(ws.as_path());
    promote_workspace_owner_marker(ws.as_path(), "run-A").unwrap();
    let mtime_before = std::fs::metadata(ws.as_path().join(".git/luther/workspace-owner"))
        .unwrap()
        .modified()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    ensure_durable_workspace_ownership(ws.as_path(), "run-A")
        .expect("idempotent when durable already exists");
    let mtime_after = std::fs::metadata(ws.as_path().join(".git/luther/workspace-owner"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(
        mtime_before, mtime_after,
        "durable marker must not be rewritten"
    );
}

#[test]
fn ensure_durable_fails_closed_for_foreign_bootstrap_pre_git() {
    let dir = tempfile::tempdir().unwrap();
    write_bootstrap_marker(dir.path(), "run-foreign");
    let err = ensure_durable_workspace_ownership(dir.path(), "run-A").unwrap_err();
    assert!(err.to_string().contains("run-foreign"));
}

// ---------------------------------------------------------------------------
// Non-regular marker rejection: FIFO regression (Fix 5)
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn verify_rejects_bootstrap_fifo() {
    let dir = tempfile::tempdir().unwrap();
    let luther = dir.path().join(".luther");
    std::fs::create_dir_all(&luther).unwrap();
    create_fifo(&luther.join("workspace-owner"));
    let reason = verify_workspace_ownership(dir.path(), "run-A");
    assert!(reason.is_some(), "fifo marker must fail closed");
}

/// Create a FIFO at `path` for regression tests. The FIFO is never opened for
/// read/write, so the test cannot block.
#[cfg(unix)]
#[allow(unsafe_code)]
fn create_fifo(path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
    assert_eq!(rc, 0, "mkfifo must succeed");
}

// ---------------------------------------------------------------------------
// promote_workspace_owner_marker: never accept an existing durable entry
// without anchored exact validation (issue 158 review).
//
// Each case sets up a valid bootstrap marker for run-A and a *foreign* durable
// entry of a different type. Promotion must fail closed rather than silently
// accepting the existing durable record because "it exists".
// ---------------------------------------------------------------------------

fn bootstrap_valid_for_run_a(workspace: &Path) {
    write_bootstrap_marker(workspace, "run-A");
}

#[test]
fn promote_refuses_existing_foreign_durable() {
    let dir = tempfile::tempdir().unwrap();
    bootstrap_valid_for_run_a(dir.path());
    init_git(dir.path());
    write_durable_marker(dir.path(), "run-foreign");
    let err = promote_workspace_owner_marker(dir.path(), "run-A").unwrap_err();
    assert!(
        err.to_string().contains("run-foreign") || err.kind() == std::io::ErrorKind::AlreadyExists,
        "foreign durable must be rejected, got: {err}"
    );
    // The foreign durable must not be overwritten by the promotion.
    assert_eq!(
        std::fs::read(dir.path().join(".git/luther/workspace-owner")).unwrap(),
        b"run-foreign"
    );
}

#[test]
fn promote_refuses_existing_empty_durable() {
    let dir = tempfile::tempdir().unwrap();
    bootstrap_valid_for_run_a(dir.path());
    init_git(dir.path());
    write_durable_marker(dir.path(), "");
    let err = promote_workspace_owner_marker(dir.path(), "run-A").unwrap_err();
    // The promotion descriptor path re-reads the existing durable and rejects
    // it because the empty bytes do not match the expected run id.
    assert!(
        err.to_string().contains("does not match")
            || err.kind() == std::io::ErrorKind::AlreadyExists,
        "empty durable must be rejected, got: {err}"
    );
    assert_eq!(
        std::fs::read(dir.path().join(".git/luther/workspace-owner")).unwrap(),
        b""
    );
}

#[cfg(unix)]
#[test]
fn promote_refuses_existing_symlinked_durable() {
    let dir = tempfile::tempdir().unwrap();
    bootstrap_valid_for_run_a(dir.path());
    init_git(dir.path());
    let durable_dir = dir.path().join(".git/luther");
    std::fs::create_dir_all(&durable_dir).unwrap();
    // Symlink the durable marker to an attacker-controlled target.
    std::os::unix::fs::symlink("/etc/passwd", durable_dir.join("workspace-owner")).unwrap();
    let err = promote_workspace_owner_marker(dir.path(), "run-A").unwrap_err();
    // The promotion descriptor path re-stats the existing durable with no-follow
    // and rejects it because a symlink is not a regular file.
    assert!(
        err.to_string().contains("not a regular file"),
        "symlink durable must be rejected, got: {err}"
    );
}

#[cfg(unix)]
#[test]
fn promote_refuses_existing_directory_durable() {
    let dir = tempfile::tempdir().unwrap();
    bootstrap_valid_for_run_a(dir.path());
    init_git(dir.path());
    let durable_dir = dir.path().join(".git/luther");
    std::fs::create_dir_all(durable_dir.join("workspace-owner")).unwrap();
    let err = promote_workspace_owner_marker(dir.path(), "run-A").unwrap_err();
    // The promotion descriptor path re-stats the existing durable with no-follow
    // and rejects it because a directory is not a regular file.
    assert!(
        err.to_string().contains("not a regular file"),
        "directory durable must be rejected, got: {err}"
    );
}

#[cfg(unix)]
#[test]
fn promote_refuses_existing_fifo_durable() {
    let dir = tempfile::tempdir().unwrap();
    bootstrap_valid_for_run_a(dir.path());
    init_git(dir.path());
    let durable_dir = dir.path().join(".git/luther");
    std::fs::create_dir_all(&durable_dir).unwrap();
    create_fifo(&durable_dir.join("workspace-owner"));
    let err = promote_workspace_owner_marker(dir.path(), "run-A").unwrap_err();
    // The promotion descriptor path re-stats the existing durable with no-follow
    // and rejects it because a FIFO is not a regular file.
    assert!(
        err.to_string().contains("not a regular file"),
        "fifo durable must be rejected, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// ensure_durable_workspace_ownership: never accept an existing durable entry
// without anchored exact validation.
// ---------------------------------------------------------------------------

#[test]
fn ensure_durable_refuses_existing_foreign_durable() {
    let dir = tempfile::tempdir().unwrap();
    bootstrap_valid_for_run_a(dir.path());
    init_git(dir.path());
    write_durable_marker(dir.path(), "run-foreign");
    let err = ensure_durable_workspace_ownership(dir.path(), "run-A").unwrap_err();
    assert!(err.to_string().contains("run-foreign"));
}

#[test]
fn ensure_durable_refuses_existing_empty_durable() {
    let dir = tempfile::tempdir().unwrap();
    bootstrap_valid_for_run_a(dir.path());
    init_git(dir.path());
    write_durable_marker(dir.path(), "");
    let err = ensure_durable_workspace_ownership(dir.path(), "run-A").unwrap_err();
    assert!(err.to_string().contains("empty"));
}

#[cfg(unix)]
#[test]
fn ensure_durable_refuses_existing_symlinked_durable() {
    let dir = tempfile::tempdir().unwrap();
    bootstrap_valid_for_run_a(dir.path());
    init_git(dir.path());
    let durable_dir = dir.path().join(".git/luther");
    std::fs::create_dir_all(&durable_dir).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", durable_dir.join("workspace-owner")).unwrap();
    let err = ensure_durable_workspace_ownership(dir.path(), "run-A").unwrap_err();
    assert!(err.to_string().contains("symlink"));
}

#[cfg(unix)]
#[test]
fn ensure_durable_refuses_existing_directory_durable() {
    let dir = tempfile::tempdir().unwrap();
    bootstrap_valid_for_run_a(dir.path());
    init_git(dir.path());
    let durable_dir = dir.path().join(".git/luther");
    std::fs::create_dir_all(durable_dir.join("workspace-owner")).unwrap();
    let err = ensure_durable_workspace_ownership(dir.path(), "run-A").unwrap_err();
    assert!(err.to_string().contains("directory"));
}

#[cfg(unix)]
#[test]
fn ensure_durable_refuses_existing_fifo_durable() {
    let dir = tempfile::tempdir().unwrap();
    bootstrap_valid_for_run_a(dir.path());
    init_git(dir.path());
    let durable_dir = dir.path().join(".git/luther");
    std::fs::create_dir_all(&durable_dir).unwrap();
    create_fifo(&durable_dir.join("workspace-owner"));
    let err = ensure_durable_workspace_ownership(dir.path(), "run-A").unwrap_err();
    assert!(err.to_string().contains("not a regular file"));
}

// ---------------------------------------------------------------------------
// Race-oriented: concurrent promotion must result in exactly one durable
// record with exact same-owner bytes.
// ---------------------------------------------------------------------------

#[test]
fn concurrent_promotion_results_in_single_exact_durable() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let dir = Arc::new(tempfile::tempdir().unwrap());
    let ws = dir.path().join("ws");
    provision_workspace_owner_marker(&ws, "run-race").unwrap();
    init_git(&ws);

    let ws = Arc::new(ws);
    let errors = Arc::new(AtomicUsize::new(0));
    let successes = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..8 {
        let ws = Arc::clone(&ws);
        let errors = Arc::clone(&errors);
        let successes = Arc::clone(&successes);
        handles.push(std::thread::spawn(move || {
            match promote_workspace_owner_marker(&ws, "run-race") {
                Ok(()) => {
                    successes.fetch_add(1, Ordering::SeqCst);
                }
                Err(err) => {
                    // The only acceptable "error" is a benign AlreadyExists
                    // from a concurrent winner; anything else is a real bug.
                    assert!(
                        err.kind() == std::io::ErrorKind::AlreadyExists
                            || err.to_string().contains("does not match"),
                        "unexpected race error: {err}"
                    );
                    errors.fetch_add(1, Ordering::SeqCst);
                }
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    // At least one promotion must have succeeded.
    assert!(successes.load(Ordering::SeqCst) >= 1);
    // Exactly one durable record exists, and it records the same owner.
    let durable = ws.join(".git/luther/workspace-owner");
    assert_eq!(std::fs::read(&durable).unwrap(), b"run-race");
    // Final verification passes.
    assert_eq!(verify_workspace_ownership(&ws, "run-race"), None);
}

#[test]
fn concurrent_ensure_durable_results_in_single_exact_durable() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws");
    provision_workspace_owner_marker(&ws, "run-race2").unwrap();
    init_git(&ws);

    let ws = Arc::new(ws);
    let successes = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..8 {
        let ws = Arc::clone(&ws);
        let successes = Arc::clone(&successes);
        handles.push(std::thread::spawn(move || {
            ensure_durable_workspace_ownership(&ws, "run-race2").unwrap();
            successes.fetch_add(1, Ordering::SeqCst);
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(successes.load(Ordering::SeqCst), 8);
    let durable = ws.join(".git/luther/workspace-owner");
    assert_eq!(std::fs::read(&durable).unwrap(), b"run-race2");
    assert_eq!(verify_workspace_ownership(&ws, "run-race2"), None);
}
