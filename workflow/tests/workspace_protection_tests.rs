//! Regression tests for the `.llxprt` / user-workspace deletion guard (issue #53).
//!
//! These prove that Luther's single sanctioned destructive helper,
//! `guarded_remove_dir_all`, refuses to delete protected workspace state and
//! leaves it intact on disk.

use std::fs;
use std::path::Path;

use luther_workflow::repo::{
    guarded_remove_dir_all, is_protected_workspace_path, tree_contains_protected_workspace_path,
};

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

#[test]
fn guarded_remove_refuses_parent_containing_nested_llxprt() {
    // A run directory whose path component is *not* `.llxprt`, but which
    // contains a nested `.llxprt` descendant. Deleting the parent must be
    // refused so the protected state is not destroyed by deleting its ancestor.
    let tmp = tempfile::tempdir().expect("tempdir");
    let run_dir = tmp.path().join("run-001");
    let nested_llxprt = run_dir.join(".llxprt");
    fs::create_dir_all(&nested_llxprt).expect("create nested .llxprt");
    let keep = nested_llxprt.join("settings.json");
    fs::write(&keep, b"important user state").expect("write keep file");
    // Some unrelated state alongside the protected directory.
    fs::create_dir_all(run_dir.join("src")).expect("create src dir");
    fs::write(run_dir.join("src/main.rs"), b"fn main() {}").expect("write file");

    let result = guarded_remove_dir_all(&run_dir);

    assert!(
        result.is_err(),
        "guarded_remove_dir_all must refuse to delete a parent containing a nested .llxprt"
    );
    assert_eq!(
        result.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert!(
        run_dir.exists(),
        "parent run dir must be preserved when it contains protected state"
    );
    assert!(
        nested_llxprt.exists(),
        "nested .llxprt directory must be preserved"
    );
    assert!(keep.exists(), "nested .llxprt contents must be preserved");
}

#[test]
fn tree_contains_protected_detects_nested_and_ignores_clean_trees() {
    let tmp = tempfile::tempdir().expect("tempdir");

    // A clean tree with no protected descendants.
    let clean = tmp.path().join("clean");
    fs::create_dir_all(clean.join("src")).expect("create clean dir");
    fs::write(clean.join("src/main.rs"), b"fn main() {}").expect("write file");
    assert!(
        !tree_contains_protected_workspace_path(&clean),
        "clean tree must not be flagged as protected"
    );

    // A tree with a nested .llxprt descendant.
    let dirty = tmp.path().join("dirty");
    fs::create_dir_all(dirty.join("nested/.llxprt")).expect("create nested .llxprt");
    assert!(
        tree_contains_protected_workspace_path(&dirty),
        "tree containing a nested .llxprt must be flagged as protected"
    );

    // A non-existent path has nothing protected beneath it.
    let missing = tmp.path().join("does-not-exist");
    assert!(
        !tree_contains_protected_workspace_path(&missing),
        "non-existent path must not be flagged as protected"
    );
}
