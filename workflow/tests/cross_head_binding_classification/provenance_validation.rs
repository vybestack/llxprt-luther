use super::super::*;

// ============================================================================
// Negative tests: ambiguity, corruption, filename mismatch, exact-identity
// selection — the reviewer's fail-closed requirements.
// ============================================================================

/// Helper: write a second remediation result for the same PR+head into the
/// history directory, duplicating the existing identity to create ambiguity.
fn duplicate_history_snapshot(store: &PrFollowupArtifactStore, b: &PrFollowupBinding) -> PathBuf {
    let history_root = history_root_for(store, b, "pr-remediation-result");
    let mut files: Vec<_> = std::fs::read_dir(&history_root)
        .expect("read history dir")
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    let source = files.first().expect("existing history snapshot");
    let payload: Value = serde_json::from_str(
        &std::fs::read_to_string(source).expect("read existing history snapshot"),
    )
    .expect("parse existing history snapshot");
    store
        .write_json_artifact(
            b,
            "pr-remediation-result",
            "pr_remediation_result",
            9,
            &payload,
            None,
            &FixedClock,
        )
        .expect("write second valid history snapshot")
        .history_path
}

fn clone_history_with_updated_path(src: &std::path::Path, dest: &std::path::Path) {
    let content =
        std::fs::read_to_string(src).unwrap_or_else(|err| panic!("read {}: {err}", src.display()));
    let mut value: Value = serde_json::from_str(&content)
        .unwrap_or_else(|err| panic!("parse {}: {err}", src.display()));
    if let Some(meta) = value
        .get_mut("history_metadata")
        .and_then(Value::as_object_mut)
    {
        meta.insert(
            "history_path".to_string(),
            json!(dest.display().to_string()),
        );
    }
    std::fs::write(
        dest,
        serde_json::to_vec_pretty(&value)
            .unwrap_or_else(|err| panic!("serialize {}: {err}", dest.display())),
    )
    .unwrap_or_else(|err| panic!("write {}: {err}", dest.display()));
}

#[test]
fn ambiguous_head_identity_evidence_errors() {
    let temp = TempDir::new().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let clock = FixedClock;

    let b = binding("aaa", Some("ccc"));
    write_remediation_result(&store, &b, "aaa", "bbb", &clock);

    // Create a second snapshot with the same head identity but different
    // sequence numbers (so filename validation passes).
    duplicate_history_snapshot(&store, &b);

    let result = store.read_history_json_by_head(&b, "pr-remediation-result", "aaa", Some("bbb"));
    assert!(
        result.is_err(),
        "ambiguous head-identity evidence must error, not pick an arbitrary candidate"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("ambiguous"),
        "error must mention ambiguity: {err_msg}"
    );
}

#[test]
fn exact_sequence_lookup_selects_precise_snapshot() {
    let temp = TempDir::new().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let clock = FixedClock;

    let b = binding("aaa", Some("ccc"));
    let record = store
        .write_json_artifact(
            &b,
            "pr-remediation-result",
            "pr_remediation_result",
            9,
            &json!({
                "validation_state": "valid",
                "overall_status": "success",
                "input_head_sha": "aaa",
                "output_head_sha": "bbb",
                "results": [],
                "retry_scope": {}
            }),
            None,
            &clock,
        )
        .expect("write result");

    // Create a second snapshot with the same head identity (ambiguity for
    // head-only lookup, but exact-sequence lookup should select precisely).
    duplicate_history_snapshot(&store, &b);

    // Head-only lookup must error on ambiguity.
    let ambiguous =
        store.read_history_json_by_head(&b, "pr-remediation-result", "aaa", Some("bbb"));
    assert!(ambiguous.is_err(), "head-only lookup must be ambiguous");

    // Exact-sequence lookup must select the precise original snapshot.
    let result = store.read_history_evidence_by_sequence(
        &b,
        "pr-remediation-result",
        "aaa",
        Some("bbb"),
        &record.sequence,
    );
    assert!(
        result.is_ok(),
        "exact-sequence lookup must not error: {:?}",
        result.as_ref().err()
    );
    let value = result.unwrap().expect("must find exact evidence");
    assert_eq!(
        value.get("artifact_sequence").and_then(Value::as_u64),
        Some(record.sequence.artifact_sequence),
        "exact-sequence lookup must return the referenced snapshot"
    );
}

#[test]
fn exact_history_filename_rejects_sequence_collision_suffix() {
    let temp = TempDir::new().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let clock = FixedClock;

    let b = binding("aaa", Some("ccc"));
    write_remediation_result(&store, &b, "aaa", "bbb", &clock);

    // Manually clone the history file with the exact same embedded
    // artifact_sequence/write_sequence/producer_step_id and a matching
    // filename stem, creating a genuine sequence collision.
    let history_root = history_root_for(&store, &b, "pr-remediation-result");
    let files: Vec<_> = std::fs::read_dir(&history_root)
        .expect("read history dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();
    assert!(!files.is_empty());
    let src = &files[0];
    let stem = src.file_stem().and_then(|n| n.to_str()).expect("stem");
    // Write the clone with a different directory-level name but identical
    // stem prefix (starts_with match) — use a suffix to differentiate.
    let clone_path = history_root.join(format!("{stem}-clone.json"));
    clone_history_with_updated_path(src, &clone_path);

    let seq = ArtifactSequenceMetadata {
        artifact_sequence: 1,
        write_sequence: 1,
        producer_step_id: "pr_remediation_result".to_string(),
    };
    let result = store.read_history_evidence_by_sequence(
        &b,
        "pr-remediation-result",
        "aaa",
        Some("bbb"),
        &seq,
    );
    let error = result.expect_err("sequence-collision suffix must be rejected");
    let message = error.to_string();
    assert!(
        message.contains("filename mismatch") || message.contains("ambiguous"),
        "error must identify filename corruption or ambiguity: {message}"
    );
}

#[test]
fn history_filename_mismatch_errors() {
    let temp = TempDir::new().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let clock = FixedClock;

    let b = binding("aaa", Some("ccc"));
    write_remediation_result(&store, &b, "aaa", "bbb", &clock);

    // Rename the history file to a mismatched name.
    let history_root = history_root_for(&store, &b, "pr-remediation-result");
    let files: Vec<_> = std::fs::read_dir(&history_root)
        .expect("read history dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();
    assert!(!files.is_empty());
    let src = &files[0];
    let mismatched = history_root.join("999-999-bad_producer.json");
    std::fs::rename(src, &mismatched).expect("rename");

    let result = store.read_history_json_by_head(&b, "pr-remediation-result", "aaa", Some("bbb"));
    assert!(
        result.is_err(),
        "filename mismatch must error, not silently degrade to absent"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("filename mismatch"),
        "error must mention filename mismatch: {err_msg}"
    );
}

#[test]
fn history_missing_metadata_errors() {
    let temp = TempDir::new().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let clock = FixedClock;

    let b = binding("aaa", Some("ccc"));
    write_remediation_result(&store, &b, "aaa", "bbb", &clock);

    // Corrupt history file: remove history_metadata entirely.
    let history_root = history_root_for(&store, &b, "pr-remediation-result");
    let files: Vec<_> = std::fs::read_dir(&history_root)
        .expect("read history dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();
    assert!(!files.is_empty());
    let target = &files[0];
    let mut value: Value =
        serde_json::from_str(&std::fs::read_to_string(target).expect("read")).expect("parse");
    if let Some(obj) = value.as_object_mut() {
        obj.remove("history_metadata");
    }
    std::fs::write(
        target,
        serde_json::to_vec_pretty(&value).expect("serialize"),
    )
    .expect("write corrupt");

    let result = store.read_history_json_by_head(&b, "pr-remediation-result", "aaa", Some("bbb"));
    assert!(
        result.is_err(),
        "missing history_metadata must error, not silently degrade to absent"
    );
}

#[test]
fn history_different_pr_under_pr_keyed_directory_is_fatal() {
    let temp = TempDir::new().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let clock = FixedClock;

    // Write a result for PR 42 (same history root path).
    let b = binding("aaa", Some("ccc"));
    write_remediation_result(&store, &b, "aaa", "bbb", &clock);

    // Manually inject a different-PR artifact into the same history directory.
    // The history directory path is keyed by run/owner/name/pr_number, so a
    // different PR naturally lives in a different directory. But we simulate
    // cross-contamination by writing a file with different PR identity into
    // the same directory.
    let history_root = history_root_for(&store, &b, "pr-remediation-result");
    let other_payload = json!({
        "schema_version": PR_FOLLOWUP_SCHEMA_VERSION,
        "run_id": "run-1",
        "repository_owner": "example",
        "repository_name": "workflow",
        "pr_number": 99,
        "head_ref": "other",
        "head_sha": "zzz",
        "base_ref": "main",
        "base_sha": "ccc",
        "artifact_sequence": 500,
        "write_sequence": 500,
        "producer_step_id": "pr_remediation_result",
        "step_order_index": 9,
        "history_metadata": {
            "canonical_path": "/dev/null",
            "history_path": "/dev/null",
            "artifact_family": "pr-remediation-result",
            "is_canonical": true,
            "history_written_at": "2026-07-12T00:00:00Z"
        },
        "validation_state": "valid",
        "input_head_sha": "aaa",
        "output_head_sha": "bbb"
    });
    let other_path = history_root.join("500-500-pr_remediation_result.json");
    std::fs::write(
        &other_path,
        serde_json::to_vec_pretty(&other_payload).expect("serialize"),
    )
    .expect("write other-pr artifact");

    // A PR-keyed directory is an identity boundary. Cross-contamination is
    // fatal even when another otherwise-valid candidate would match.
    let result = store.read_history_json_by_head(&b, "pr-remediation-result", "aaa", Some("bbb"));
    assert!(
        result.is_err(),
        "different-PR history contamination must be fatal"
    );
    assert!(
        result.unwrap_err().to_string().contains("binding mismatch"),
        "error must identify the PR binding corruption"
    );
}

pub(crate) fn history_files(store: &PrFollowupArtifactStore) -> Vec<PathBuf> {
    let root = history_root_for(store, &binding("aaa", Some("ccc")), "pr-remediation-result");
    let mut files: Vec<_> = std::fs::read_dir(root)
        .expect("read history")
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    files
}

fn rewrite_history_sequence(path: &PathBuf, artifact_sequence: u64, write_sequence: u64) {
    let mut value: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read history snapshot"))
            .expect("parse history snapshot");
    let producer = value
        .get("producer_step_id")
        .and_then(Value::as_str)
        .expect("producer")
        .to_string();
    let new_path = path.parent().expect("history parent").join(format!(
        "{artifact_sequence}-{write_sequence}-{producer}.json"
    ));
    value["artifact_sequence"] = json!(artifact_sequence);
    value["write_sequence"] = json!(write_sequence);
    value["history_metadata"]["history_path"] = json!(new_path.display().to_string());
    std::fs::remove_file(path).expect("remove original history snapshot");
    std::fs::write(
        &new_path,
        serde_json::to_vec_pretty(&value).expect("serialize rewritten history"),
    )
    .expect("write rewritten history snapshot");
}

#[test]
fn history_sequence_gap_is_fatal_for_evidence_lookup() {
    let temp = TempDir::new().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let b = binding("aaa", Some("ccc"));
    write_remediation_result(&store, &b, "aaa", "bbb", &FixedClock);
    duplicate_history_snapshot(&store, &b);
    std::fs::remove_file(&history_files(&store)[0]).expect("remove first sequence");

    let result = store.read_history_json_by_head(&b, "pr-remediation-result", "aaa", Some("bbb"));
    assert!(result.is_err(), "a sequence gap must be fatal");
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("missing sequence 1"));
}

#[test]
fn duplicate_artifact_sequence_is_fatal_for_evidence_lookup() {
    let temp = TempDir::new().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let b = binding("aaa", Some("ccc"));
    write_remediation_result(&store, &b, "aaa", "bbb", &FixedClock);
    duplicate_history_snapshot(&store, &b);
    let second = history_files(&store)[1].clone();
    rewrite_history_sequence(&second, 1, 2);

    let result = store.read_history_json_by_head(&b, "pr-remediation-result", "aaa", Some("bbb"));
    assert!(result.is_err(), "duplicate artifact_sequence must be fatal");
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("duplicate artifact_sequence"));
}

#[test]
fn duplicate_write_sequence_is_fatal_for_evidence_lookup() {
    let temp = TempDir::new().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let b = binding("aaa", Some("ccc"));
    write_remediation_result(&store, &b, "aaa", "bbb", &FixedClock);
    duplicate_history_snapshot(&store, &b);
    let second = history_files(&store)[1].clone();
    rewrite_history_sequence(&second, 2, 1);

    let result = store.read_history_json_by_head(&b, "pr-remediation-result", "aaa", Some("bbb"));
    assert!(
        result.is_err(),
        "duplicate family write_sequence must be fatal"
    );
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("duplicate write_sequence"));
}

#[test]
fn corrupt_nonmatching_history_candidate_is_fatal() {
    let temp = TempDir::new().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let b = binding("aaa", Some("ccc"));
    write_remediation_result(&store, &b, "aaa", "bbb", &FixedClock);
    duplicate_history_snapshot(&store, &b);
    let second = history_files(&store)[1].clone();
    let corrupt = second.with_file_name("2-2-pr_remediation_result-corrupt.json");
    std::fs::rename(second, corrupt).expect("rename nonmatching candidate");

    let result =
        store.read_history_json_by_head(&b, "pr-remediation-result", "no-match", Some("bbb"));
    assert!(
        result.is_err(),
        "corruption must be fatal before head filtering"
    );
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("filename mismatch"));
}

#[test]
fn stale_optional_wrong_family_errors() {
    let temp = TempDir::new().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let clock = FixedClock;

    let prior = binding("aaa", Some("ccc"));
    let current = binding("bbb", Some("ccc"));

    write_remediation_result(&store, &prior, "aaa", "bbb", &clock);

    // Corrupt the canonical file's artifact_family to a wrong family. Even
    // though the artifact is a stale prior head, the wrong-family metadata
    // must be detected before the stale-as-absent shortcut.
    let canonical = store.canonical_path(&prior, "pr-remediation-result");
    let mut value: Value =
        serde_json::from_str(&std::fs::read_to_string(&canonical).expect("read")).expect("parse");
    if let Some(meta) = value
        .get_mut("history_metadata")
        .and_then(Value::as_object_mut)
    {
        meta.insert("artifact_family".to_string(), json!("ci-failures"));
    }
    std::fs::write(
        &canonical,
        serde_json::to_vec_pretty(&value).expect("serialize"),
    )
    .expect("write corrupt");

    let result = store.read_optional_current_json_for_head(&current, "pr-remediation-result");
    assert!(
        result.is_err(),
        "wrong-family metadata must error even for a stale prior-head artifact"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("family mismatch"),
        "error must mention family mismatch: {err_msg}"
    );
}

#[cfg(unix)]
#[test]
fn history_traversal_skips_cyclic_symlink() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let b = binding("aaa", Some("ccc"));
    write_remediation_result(&store, &b, "aaa", "bbb", &FixedClock);
    let history_root = history_root_for(&store, &b, "pr-remediation-result");
    symlink(&history_root, history_root.join("cycle")).expect("create cyclic history symlink");

    let result = store
        .read_history_json_by_head(&b, "pr-remediation-result", "aaa", Some("bbb"))
        .expect("cyclic symlink must be skipped");
    assert!(
        result.is_some(),
        "the regular history artifact remains readable"
    );
}
