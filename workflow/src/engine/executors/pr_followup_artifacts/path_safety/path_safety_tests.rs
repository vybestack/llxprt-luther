use super::*;
use std::os::unix::fs::symlink;

#[test]
fn oversized_publications_fail_before_creating_destination_directories() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("artifacts");
    fs::create_dir_all(&root).expect("artifact root");
    let bytes = vec![b'x'; usize::try_from(MAX_ARTIFACT_FILE_BYTES + 1).expect("test size")];

    for (path, publish) in [
        (
            root.join("create/history.json"),
            durable_create_new as fn(&Path, &Path, &[u8]) -> Result<(), EngineError>,
        ),
        (
            root.join("replace/current.json"),
            durable_replace as fn(&Path, &Path, &[u8]) -> Result<(), EngineError>,
        ),
    ] {
        let error = publish(&root, &path, &bytes).expect_err("oversized publication");
        assert!(error.to_string().contains("serialized artifact exceeds"));
        assert!(
            !path.parent().expect("destination parent").exists(),
            "size validation must precede every filesystem write"
        );
    }
}

#[test]
fn descriptor_traversal_skips_symlink_entries_without_following_them() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("artifacts");
    let scan_root = root.join("pr-followup/current/run-symlink");
    fs::create_dir_all(&scan_root).expect("scan tree");
    fs::write(scan_root.join("inside.json"), b"inside").expect("inside artifact");
    let outside = temp.path().join("outside.json");
    fs::write(&outside, b"outside sentinel").expect("outside artifact");
    symlink(&outside, scan_root.join("linked.json")).expect("symlink entry");

    let files =
        read_contained_json_files_with_budget(&root, &scan_root, &mut ReadBudget::default())
            .expect("symlink entries are ignored");

    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, scan_root.join("inside.json"));
    assert_eq!(files[0].content, "inside");
}

#[test]
fn history_candidate_traversal_skips_symlinked_directories() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("artifacts");
    let scan_root = root.join("pr-followup/history/run-symlink/family");
    fs::create_dir_all(&scan_root).expect("scan tree");
    fs::write(scan_root.join("inside.json"), b"inside").expect("inside artifact");
    let outside = temp.path().join("outside-history");
    fs::create_dir_all(&outside).expect("outside history");
    fs::write(outside.join("external.json"), b"outside sentinel").expect("outside artifact");
    symlink(&outside, scan_root.join("linked-directory")).expect("directory symlink entry");

    let files = read_contained_history_candidates_with_budget(
        &root,
        &scan_root,
        &mut ReadBudget::default(),
    )
    .expect("directory symlink entries are ignored");

    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, scan_root.join("inside.json"));
    assert_eq!(files[0].content, "inside");
}

#[test]
fn descriptor_traversal_rejects_ancestor_swap() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("artifacts");
    let scan_root = root.join("pr-followup/current/run-swap");
    let nested = scan_root.join("owner/repo/1");
    fs::create_dir_all(&nested).expect("scan tree");
    fs::write(nested.join("pr.json"), b"{}").expect("inside artifact");
    let outside = temp.path().join("outside");
    fs::create_dir_all(&outside).expect("outside");
    let outside_artifact = outside.join("pr.json");
    fs::write(&outside_artifact, b"outside sentinel").expect("outside sentinel");
    let moved = root.join("moved-run");

    let result = read_contained_files_after_open(
        &root,
        &scan_root,
        FileSelection::Named(OsStr::new("pr.json")),
        8,
        10,
        &mut ReadBudget::default(),
        || {
            fs::rename(&scan_root, &moved).expect("move opened ancestor");
            symlink(&outside, &scan_root).expect("hostile ancestor alias");
        },
    );

    assert!(result.is_err(), "ancestor replacement must fail closed");
    assert_eq!(
        fs::read_to_string(outside_artifact).expect("outside unchanged"),
        "outside sentinel"
    );
}

#[test]
fn current_directory_scan_rejects_real_directory_replacement() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("artifacts");
    let scan_root = root.join("pr-followup/current/run-swap");
    fs::create_dir_all(&scan_root).expect("current directory");
    fs::write(scan_root.join("inside.json"), b"inside").expect("current artifact");
    let moved = root.join("moved-current");

    let error = visit_current_json_after_open(
        &root,
        &scan_root,
        &mut ReadBudget::default(),
        || {
            fs::rename(&scan_root, &moved).expect("move opened current directory");
            fs::create_dir_all(&scan_root).expect("install replacement current directory");
            fs::write(scan_root.join("replacement.json"), b"replacement")
                .expect("replacement artifact");
        },
        |_| Ok(()),
    )
    .expect_err("current directory replacement must fail closed");

    assert!(error.to_string().contains("changed during traversal"));
}

#[test]
fn descriptor_json_traversal_cannot_enumerate_swapped_external_directory() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("artifacts");
    let scan_root = root.join("pr-followup/history/run-swap");
    fs::create_dir_all(&scan_root).expect("empty history tree");
    let outside = temp.path().join("outside-history");
    fs::create_dir_all(&outside).expect("outside history");
    fs::write(outside.join("external-secret.json"), b"outside sentinel").expect("outside artifact");
    let moved = root.join("moved-history");

    let error = read_contained_files_after_open(
        &root,
        &scan_root,
        FileSelection::Json,
        MAX_ARTIFACT_SCAN_DEPTH,
        MAX_ARTIFACT_SCAN_FILES,
        &mut ReadBudget::default(),
        || {
            fs::rename(&scan_root, &moved).expect("move opened history root");
            symlink(&outside, &scan_root).expect("hostile history alias");
        },
    )
    .expect_err("directory replacement must fail closed");

    assert!(
        error.to_string().contains("changed during traversal")
            || error.to_string().contains("symbolic link")
            || error.to_string().contains("Not a directory"),
        "unexpected closed-race error: {error}"
    );
    assert!(
        !error.to_string().contains("external-secret.json"),
        "descriptor-relative enumeration must never observe external filenames"
    );
}
