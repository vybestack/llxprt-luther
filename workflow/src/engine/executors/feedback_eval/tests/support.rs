//! Shared fixtures for feedback evaluator tests.

use super::super::*;

pub(super) fn item(id: &str) -> FeedbackItem {
    FeedbackItem {
        item_id: id.to_string(),
        stable_marker_key: format!("thread:{id}"),
        body_hash: format!("hash-{id}"),
        head_sha: "sha-head".to_string(),
        author_login: "coderabbitai".to_string(),
        author_kind: Some("bot".to_string()),
        body: "some feedback body".to_string(),
        path: Some("src/foo.rs".to_string()),
        url: Some("https://example/1".to_string()),
    }
}

pub(super) fn binding() -> PrFollowupBinding {
    PrFollowupBinding {
        schema_version: PR_FOLLOWUP_SCHEMA_VERSION,
        run_id: "run-1".to_string(),
        repository_owner: "acme".to_string(),
        repository_name: "widget".to_string(),
        pr_number: 42,
        head_ref: "feature".to_string(),
        head_sha: "sha-head".to_string(),
        base_ref: "main".to_string(),
        base_sha: Some("base-sha".to_string()),
    }
}

pub(super) fn request(it: &FeedbackItem, b: &PrFollowupBinding) -> FeedbackEvaluationRequest {
    build_request(b, it)
}
