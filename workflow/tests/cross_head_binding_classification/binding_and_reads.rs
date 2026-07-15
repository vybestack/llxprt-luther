use super::super::*;

// ============================================================================
// PrFollowupBinding classification tests
// ============================================================================

#[test]
fn pr_identity_matches_same_pr_different_head() {
    let a = binding("aaa", Some("ccc"));
    let b = binding("bbb", Some("ccc"));
    assert!(
        a.pr_identity_matches(&b),
        "same PR identity must match even when head differs"
    );
}

#[test]
fn pr_identity_matches_same_head() {
    let a = binding("aaa", Some("ccc"));
    assert!(a.pr_identity_matches(&a));
}

#[test]
fn pr_identity_mismatch_different_pr_number() {
    let a = binding("aaa", Some("ccc"));
    let b = binding_different_pr("aaa");
    assert!(
        !a.pr_identity_matches(&b),
        "different PR number must not match"
    );
}

#[test]
fn pr_identity_mismatch_different_run() {
    let a = binding("aaa", Some("ccc"));
    let mut b = a.clone();
    b.run_id = "run-2".to_string();
    assert!(!a.pr_identity_matches(&b));
}

#[test]
fn pr_identity_mismatch_empty_run_id() {
    let a = binding("aaa", Some("ccc"));
    let mut b = a.clone();
    b.run_id = String::new();
    assert!(!a.pr_identity_matches(&b));
}

#[test]
fn pr_identity_mismatch_zero_schema_version() {
    let a = binding("aaa", Some("ccc"));
    let mut b = a.clone();
    b.schema_version = 0;
    assert!(!a.pr_identity_matches(&b));
}

#[test]
fn head_revision_matches_same_head_same_base() {
    let a = binding("aaa", Some("ccc"));
    let b = binding("aaa", Some("ccc"));
    assert!(a.head_revision_matches(&b));
}

#[test]
fn head_revision_mismatch_different_head() {
    let a = binding("aaa", Some("ccc"));
    let b = binding("bbb", Some("ccc"));
    assert!(!a.head_revision_matches(&b));
}

#[test]
fn head_revision_mismatch_empty_head() {
    let a = binding("aaa", Some("ccc"));
    let mut b = a.clone();
    b.head_sha = String::new();
    assert!(!a.head_revision_matches(&b));
}

#[test]
fn is_stale_prior_head_same_pr_different_head() {
    let a = binding("aaa", Some("ccc"));
    let b = binding("bbb", Some("ccc"));
    assert!(
        a.is_stale_prior_head_of(&b),
        "same PR, different head must be stale prior head"
    );
}

#[test]
fn is_not_stale_same_head() {
    let a = binding("aaa", Some("ccc"));
    assert!(!a.is_stale_prior_head_of(&a));
}

#[test]
fn is_not_stale_different_pr() {
    let a = binding("aaa", Some("ccc"));
    let b = binding_different_pr("bbb");
    assert!(
        !a.is_stale_prior_head_of(&b),
        "different PR must not be stale prior head — it is a mismatch"
    );
}

// ============================================================================
// Base-only change: head same, base differs
// ============================================================================

#[test]
fn base_only_change_is_stale_prior_head() {
    let a = binding("aaa", Some("ccc"));
    let b = binding("aaa", Some("ddd"));
    assert!(
        a.is_stale_prior_head_of(&b),
        "base-only change with same head must still be classified stale when base differs"
    );
}

#[test]
fn base_only_same_base_not_stale() {
    let a = binding("aaa", Some("ccc"));
    assert!(!a.is_stale_prior_head_of(&a));
}

// ============================================================================
// Legacy null base behavior
// ============================================================================

#[test]
fn legacy_null_base_matches_other_null_base() {
    let a = binding("aaa", None);
    let b = binding("aaa", None);
    assert!(
        a.head_revision_matches(&b),
        "legacy null base on both sides must match head revision"
    );
    assert!(!a.is_stale_prior_head_of(&b));
}

#[test]
fn legacy_null_base_vs_some_base_is_stale() {
    let a = binding("aaa", None);
    let b = binding("aaa", Some("ccc"));
    assert!(
        !a.head_revision_matches(&b),
        "null base vs non-null base must not match head revision"
    );
    assert!(
        a.is_stale_prior_head_of(&b),
        "null base vs non-null base with same PR identity is stale"
    );
}

#[test]
fn legacy_null_base_identity_still_matches() {
    let a = binding("aaa", None);
    let b = binding("bbb", None);
    assert!(a.pr_identity_matches(&b));
}

// ============================================================================
// Artifact store: read_optional_current_json_for_head — stale-as-absent
// ============================================================================

#[test]
fn stale_prior_head_artifact_treated_as_absent() {
    let temp = TempDir::new().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let clock = FixedClock;

    let prior = binding("aaa", Some("ccc"));
    let current = binding("bbb", Some("ccc"));

    write_remediation_result(&store, &prior, "aaa", "bbb", &clock);

    let result = store
        .read_optional_current_json_for_head(&current, "pr-remediation-result")
        .expect("read should not error");
    assert!(
        result.is_none(),
        "stale prior-head artifact for same PR must be absent"
    );
}

#[test]
fn different_pr_artifact_errors_not_absent() {
    let temp = TempDir::new().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let clock = FixedClock;

    // Write with one PR identity, then read with a different PR identity that
    // maps to the same canonical path (same run/owner/name/pr_number path
    // components but different head_sha). Corrupt the file's embedded PR
    // identity to simulate a routing or identity corruption.
    let writer_binding = binding("aaa", Some("ccc"));
    write_remediation_result(&store, &writer_binding, "aaa", "bbb", &clock);

    // Now corrupt the canonical file to carry a different PR number.
    let canonical = store.canonical_path(&writer_binding, "pr-remediation-result");
    let mut value: Value =
        serde_json::from_str(&std::fs::read_to_string(&canonical).expect("read canonical"))
            .expect("parse canonical");
    if let Some(obj) = value.as_object_mut() {
        obj.insert("pr_number".to_string(), json!(99));
        obj.insert("head_ref".to_string(), json!("other-feature"));
    }
    std::fs::write(
        &canonical,
        serde_json::to_vec_pretty(&value).expect("serialize"),
    )
    .expect("write corrupt");

    let result =
        store.read_optional_current_json_for_head(&writer_binding, "pr-remediation-result");
    assert!(
        result.is_err(),
        "different-PR artifact at canonical path must error, not silently degrade to absent"
    );
}

#[test]
fn missing_artifact_is_absent() {
    let temp = TempDir::new().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let b = binding("aaa", Some("ccc"));

    let result = store
        .read_optional_current_json_for_head(&b, "pr-remediation-result")
        .expect("read should not error");
    assert!(result.is_none());
}

#[test]
fn empty_head_sha_binding_never_matches() {
    let a = binding("aaa", Some("ccc"));
    let mut b = a.clone();
    b.head_sha = String::new();
    assert!(
        !a.head_revision_matches(&b),
        "empty head_sha must never match"
    );
}

// ============================================================================
// Artifact store: read_history_json_by_head — cross-head evidence lookup
// ============================================================================

#[test]
fn history_lookup_finds_prior_head_evidence() {
    let temp = TempDir::new().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let clock = FixedClock;

    let prior = binding("aaa", Some("ccc"));
    let current = binding("bbb", Some("ccc"));

    write_remediation_result(&store, &prior, "aaa", "bbb", &clock);

    // Overwrite canonical with current head's result (simulating push advance)
    write_remediation_result(&store, &current, "bbb", "ccc", &clock);

    let result = store
        .read_history_json_by_head(&current, "pr-remediation-result", "aaa", Some("bbb"))
        .expect("history lookup should not error");
    assert!(
        result.is_some(),
        "must find prior-head evidence from immutable history"
    );
    let value = result.unwrap();
    assert_eq!(
        value.get("input_head_sha").and_then(Value::as_str),
        Some("aaa"),
        "history evidence must carry the source head_sha"
    );
}

#[test]
fn history_lookup_returns_none_when_no_match() {
    let temp = TempDir::new().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let clock = FixedClock;

    let b = binding("aaa", Some("ccc"));
    write_remediation_result(&store, &b, "aaa", "bbb", &clock);

    let result = store
        .read_history_json_by_head(&b, "pr-remediation-result", "zzz", None)
        .expect("lookup should not error");
    assert!(result.is_none());
}

#[test]
fn history_lookup_returns_none_when_history_absent() {
    let temp = TempDir::new().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let b = binding("aaa", Some("ccc"));

    let result = store
        .read_history_json_by_head(&b, "pr-remediation-result", "aaa", None)
        .expect("lookup should not error");
    assert!(result.is_none());
}

#[test]
fn history_lookup_wrong_family_errors() {
    let temp = TempDir::new().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let clock = FixedClock;

    let b = binding("aaa", Some("ccc"));

    // Write a valid remediation result, then corrupt the history file's
    // artifact_family metadata to simulate a wrong-family artifact in the
    // pr-remediation-result history directory.
    write_remediation_result(&store, &b, "aaa", "bbb", &clock);

    // Find the history file and corrupt its artifact_family.
    let history_root = history_root_for(&store, &b, "pr-remediation-result");
    assert!(history_root.exists(), "history directory must exist");
    let mut history_files: Vec<_> = std::fs::read_dir(&history_root)
        .expect("read history dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();
    assert!(
        !history_files.is_empty(),
        "at least one history file must exist"
    );
    history_files.sort();
    let history_file = &history_files[0];
    let mut value: Value =
        serde_json::from_str(&std::fs::read_to_string(history_file).expect("read history"))
            .expect("parse history");
    if let Some(obj) = value.as_object_mut() {
        if let Some(meta) = obj
            .get_mut("history_metadata")
            .and_then(Value::as_object_mut)
        {
            meta.insert("artifact_family".to_string(), json!("ci-failures"));
        }
    }
    std::fs::write(
        history_file,
        serde_json::to_vec_pretty(&value).expect("serialize"),
    )
    .expect("write corrupt history");

    let result = store.read_history_json_by_head(&b, "pr-remediation-result", "aaa", Some("bbb"));
    assert!(
        result.is_err(),
        "wrong-family artifact in history must error, not silently degrade to absent"
    );
}

// ============================================================================
// Artifact store: read_carried_forward_json — pending actions carry-forward
// ============================================================================

#[test]
fn carried_forward_returns_prior_head_artifact_for_same_pr() {
    let temp = TempDir::new().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let clock = FixedClock;

    let prior = binding("aaa", Some("ccc"));
    let current = binding("bbb", Some("ccc"));

    let payload = json!({
        "pending_actions": [{"action_id": "act-1", "source_head_sha": "aaa"}],
        "carry_forward_from_artifact_sequence": null,
        "marker_policy": {},
        "updated_at": "2026-07-12T00:00:00Z"
    });
    store
        .write_json_artifact(JsonArtifactWriteRequest::new(
            ArtifactWriteContext::new(
                &prior,
                "pending-feedback-marker-actions",
                "github_feedback_marker",
                12,
                &clock,
            ),
            &payload,
            None,
        ))
        .expect("write pending actions");

    let result = store
        .read_carried_forward_json(&current, "pending-feedback-marker-actions")
        .expect("carry-forward read should not error");
    assert!(
        result.is_some(),
        "prior-head pending actions for same PR must be carried forward"
    );
    let value = result.unwrap();
    let actions = value
        .get("pending_actions")
        .and_then(Value::as_array)
        .expect("pending_actions array");
    assert_eq!(actions.len(), 1);
    assert_eq!(
        actions[0].get("source_head_sha").and_then(Value::as_str),
        Some("aaa"),
        "carried-forward action retains its source_head_sha binding"
    );
}

#[test]
fn carried_forward_errors_for_different_pr() {
    let temp = TempDir::new().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let clock = FixedClock;

    let writer_binding = binding("aaa", Some("ccc"));
    let payload = json!({
        "pending_actions": [],
        "carry_forward_from_artifact_sequence": null,
        "marker_policy": {},
        "updated_at": null
    });
    store
        .write_json_artifact(JsonArtifactWriteRequest::new(
            ArtifactWriteContext::new(
                &writer_binding,
                "pending-feedback-marker-actions",
                "github_feedback_marker",
                12,
                &clock,
            ),
            &payload,
            None,
        ))
        .expect("write pending actions");

    // Corrupt the canonical file to carry a different PR identity.
    let canonical = store.canonical_path(&writer_binding, "pending-feedback-marker-actions");
    let mut value: Value =
        serde_json::from_str(&std::fs::read_to_string(&canonical).expect("read canonical"))
            .expect("parse canonical");
    if let Some(obj) = value.as_object_mut() {
        obj.insert("pr_number".to_string(), json!(99));
        obj.insert("head_ref".to_string(), json!("other-feature"));
    }
    std::fs::write(
        &canonical,
        serde_json::to_vec_pretty(&value).expect("serialize"),
    )
    .expect("write corrupt");

    let result =
        store.read_carried_forward_json(&writer_binding, "pending-feedback-marker-actions");
    assert!(
        result.is_err(),
        "different-PR carry-forward must error, not silently degrade"
    );
}

#[test]
fn carried_forward_returns_none_when_absent() {
    let temp = TempDir::new().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().to_path_buf());
    let b = binding("aaa", Some("ccc"));

    let result = store
        .read_carried_forward_json(&b, "pending-feedback-marker-actions")
        .expect("read should not error");
    assert!(result.is_none());
}
