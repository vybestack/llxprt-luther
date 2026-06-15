//! Regression tests for the `.llxprt` / user-workspace deletion guard (issue #53).
//!
//! These prove that Luther's single sanctioned destructive helper,
//! `guarded_remove_dir_all`, refuses to delete protected workspace state and
//! leaves it intact on disk.

use std::fs;
use std::path::Path;

use luther_workflow::repo::{guarded_remove_dir_all, is_protected_workspace_path};

#[test]
fn protected_predicate_accepts_legitimate_paths() {
    assert!(!is_protected_workspace_path(Path::new("/tmp/run-001")));
    assert!(!is_protected_workspace_path(Path::new(
        "/tmp/run-001/src/main.rs"
    )));
    assert!(!is_protected_workspace_path(Path::new("workspace/llxprt")));
}

#[test]
fn protected_predicate_rejects_llxprt_variants() {
    assert!(is_protected_workspace_path(Path::new(".llxprt")));
    assert!(is_protected_workspace_path(Path::new(
        ".llxprt/settings.json"
    )));
    assert!(is_protected_workspace_path(Path::new(
        "some/dir/.llxprt/file"
    )));
}

#[test]
fn guarded_remove_refuses_and_preserves_llxprt() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let llxprt_dir = tmp.path().join(".llxprt");
    fs::create_dir_all(&llxprt_dir).expect("create .llxprt");
    let keep = llxprt_dir.join("keep");
    fs::write(&keep, b"important user state").expect("write keep file");

    let result = guarded_remove_dir_all(&llxprt_dir);

    assert!(
        result.is_err(),
        "guarded_remove_dir_all must refuse to delete a .llxprt path"
    );
    assert_eq!(
        result.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert!(llxprt_dir.exists(), ".llxprt directory must be preserved");
    assert!(keep.exists(), ".llxprt contents must be preserved");
}

#[test]
fn guarded_remove_deletes_unprotected_directory() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let run_dir = tmp.path().join("run-001");
    fs::create_dir_all(run_dir.join("src")).expect("create run dir");
    fs::write(run_dir.join("src/main.rs"), b"fn main() {}").expect("write file");

    guarded_remove_dir_all(&run_dir).expect("unprotected dir should be removable");

    assert!(!run_dir.exists(), "unprotected run dir should be deleted");
}
