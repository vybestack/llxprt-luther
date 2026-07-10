use super::*;
use crate::engine::executor::StepContext;
use crate::engine::executors::pr_followup_types::PR_FOLLOWUP_SCHEMA_VERSION;
use crate::engine::runner::EngineError;
use serde_json::json;

fn marker_comment(fields: &str) -> String {
    format!("<!-- {MARKER_NAMESPACE} {fields} -->")
}

fn valid_marker_fields() -> String {
    "marker_key=thread:PRRT_1 source_head=abc remediation_output_head=def \
     body=fnv64:1 run_id=run-1 action=reply"
        .to_string()
}

fn feedback_item(marker_key: &str, body_hash: &str, commit: Option<&str>) -> FeedbackItem {
    FeedbackItem {
        item_id: format!("id:{marker_key}"),
        stable_marker_key: marker_key.to_string(),
        thread_id: None,
        comment_id: None,
        comment_database_id: None,
        review_id: None,
        author_login: "coderabbitai".to_string(),
        author_kind: None,
        path: None,
        line: None,
        side: None,
        body: "body".to_string(),
        body_hash: body_hash.to_string(),
        url: None,
        created_at: None,
        updated_at: None,
        resolved: false,
        outdated: false,
        resolution_state_available: true,
        source: "graphql_review_thread".to_string(),
        raw_node_id: None,
        commit_sha: commit.map(ToString::to_string),
        stale: false,
    }
}

fn unwrap_marker(result: Result<RemoteFeedbackMarker, MarkerParseError>) -> RemoteFeedbackMarker {
    match result {
        Ok(marker) => marker,
        Err(err) => panic!("expected valid marker, got error: {}", err.diagnostic),
    }
}

#[test]
fn parse_hidden_marker_reads_all_fields() {
    let body = marker_comment(&valid_marker_fields());
    let marker = unwrap_marker(parse_hidden_marker(&body));
    assert_eq!(marker.stable_marker_key, "thread:PRRT_1");
    assert_eq!(marker.source_head_sha, "abc");
    assert_eq!(marker.remediation_output_head_sha, Some("def".to_string()));
    assert_eq!(marker.body_hash, "fnv64:1");
    assert_eq!(marker.run_id, "run-1");
    assert_eq!(marker.action_kind, "reply");
    assert_eq!(marker.status, "completed");
}

#[test]
fn parse_hidden_marker_none_output_head_becomes_none() {
    let fields = "marker_key=k source_head=s remediation_output_head=none \
                  body=b run_id=r action=a";
    let marker = unwrap_marker(parse_hidden_marker(&marker_comment(fields)));
    assert!(marker.remediation_output_head_sha.is_none());
}

#[test]
fn parse_hidden_marker_rejects_wrong_namespace() {
    let body = "<!-- other-namespace marker_key=k -->";
    assert!(parse_hidden_marker(body).is_err());
}

#[test]
fn parse_hidden_marker_rejects_duplicate_field() {
    let fields = "marker_key=k marker_key=k2 source_head=s \
                  remediation_output_head=none body=b run_id=r action=a";
    assert!(parse_hidden_marker(&marker_comment(fields)).is_err());
}

#[test]
fn parse_hidden_marker_rejects_malformed_field() {
    let fields = "marker_key source_head=s";
    assert!(parse_hidden_marker(&marker_comment(fields)).is_err());
}

#[test]
fn parse_hidden_marker_rejects_missing_required_field() {
    // Missing action field.
    let fields = "marker_key=k source_head=s remediation_output_head=none body=b run_id=r";
    assert!(parse_hidden_marker(&marker_comment(fields)).is_err());
}

#[test]
fn parse_marker_from_comment_body_finds_embedded_marker() {
    let body = format!(
        "prefix text\n{}\ntrailing",
        marker_comment(&valid_marker_fields())
    );
    let marker = unwrap_marker(parse_marker_from_comment_body(&body));
    assert_eq!(marker.run_id, "run-1");
}

#[test]
fn parse_marker_from_comment_body_missing_marker_errors() {
    assert!(parse_marker_from_comment_body("no marker here").is_err());
}

#[test]
fn extract_exact_marker_body_rejects_nested_delimiters() {
    let body = format!("<!-- {MARKER_NAMESPACE} a=b <!-- nested -->");
    assert!(extract_exact_marker_body(&body).is_err());
}

#[test]
fn configured_identities_includes_defaults_and_extras() {
    let params = json!({
        "coderabbit_bot_identities": ["MyBot", "Another"]
    });
    let identities = configured_identities(&params);
    assert!(identities.contains("coderabbitai"));
    assert!(identities.contains("mybot"));
    assert!(identities.contains("another"));
    assert!(!identities.contains(ALL_REVIEWERS_SENTINEL));
}

#[test]
fn configured_identities_wildcard_when_include_all_reviewers() {
    let params = json!({"include_all_reviewers": true});
    let identities = configured_identities(&params);
    assert!(identities.contains(ALL_REVIEWERS_SENTINEL));
}

#[test]
fn is_coderabbit_matches_identity_and_wildcard() {
    let params = json!({});
    let identities = configured_identities(&params);
    assert!(is_coderabbit("coderabbitai", &identities));
    assert!(is_coderabbit("CodeRabbitAI", &identities));
    assert!(!is_coderabbit("randomuser", &identities));
    assert!(!is_coderabbit("", &identities));

    let wildcard = configured_identities(&json!({"include_all_reviewers": true}));
    assert!(is_coderabbit("anyone", &wildcard));
    assert!(!is_coderabbit("", &wildcard));
}

#[test]
fn is_explicit_reviewer_identity_is_case_insensitive() {
    let identities = configured_identities(&json!({}));
    assert!(is_explicit_reviewer_identity("CODERABBIT", &identities));
    assert!(!is_explicit_reviewer_identity("", &identities));
}

#[test]
fn stable_hash_is_deterministic_and_prefixed() {
    let a = stable_hash("hello");
    let b = stable_hash("hello");
    let c = stable_hash("world");
    assert_eq!(a, b);
    assert_ne!(a, c);
    assert!(a.starts_with("fnv64:"));
}

#[test]
fn item_set_hash_is_order_independent() {
    let items_a = vec![
        feedback_item("k1", "h1", Some("sha1")),
        feedback_item("k2", "h2", None),
    ];
    let items_b = vec![
        feedback_item("k2", "h2", None),
        feedback_item("k1", "h1", Some("sha1")),
    ];
    assert_eq!(item_set_hash(&items_a), item_set_hash(&items_b));
    let items_c = vec![feedback_item("k3", "h3", None)];
    assert_ne!(item_set_hash(&items_a), item_set_hash(&items_c));
}

#[test]
fn readiness_stability_hash_changes_with_signal() {
    let mut obs = FeedbackObservation {
        items: vec![feedback_item("k1", "h1", Some("sha1"))],
        ready_signal: false,
        ..Default::default()
    };
    let before = readiness_stability_hash(&obs);
    obs.ready_signal = true;
    let after = readiness_stability_hash(&obs);
    assert_ne!(before, after);
}

#[test]
fn has_unresolved_token_detects_real_tokens_only() {
    assert!(has_unresolved_token("path/{artifact_dir}/x"));
    assert!(has_unresolved_token("open {only"));
    assert!(!has_unresolved_token("no tokens"));
    assert!(!has_unresolved_token("literal {with space} text"));
}

#[test]
fn require_string_and_u64_validate_presence() {
    let value = json!({"name": "abc", "count": 5, "empty": ""});
    assert_eq!(require_string(&value, "name").unwrap(), "abc");
    assert!(require_string(&value, "empty").is_err());
    assert!(require_string(&value, "missing").is_err());
    assert_eq!(require_u64(&value, "count").unwrap(), 5);
    assert!(require_u64(&value, "missing").is_err());
}

#[test]
fn string_field_and_opt_string_read_values() {
    let value = json!({"a": "x"});
    assert_eq!(string_field(&value, "a"), "x");
    assert_eq!(string_field(&value, "missing"), "");
    assert_eq!(opt_string(&value, "a"), Some("x".to_string()));
    assert_eq!(opt_string(&value, "missing"), None);
}

#[test]
fn u64_param_uses_default_when_absent() {
    let params = json!({"limit": 9});
    assert_eq!(u64_param(&params, "limit", 3), 9);
    assert_eq!(u64_param(&params, "missing", 3), 3);
}

#[test]
fn is_permission_or_schema_error_detects_errors_array() {
    assert!(is_permission_or_schema_error(
        &json!({"errors": [{"message": "x"}]})
    ));
    assert!(!is_permission_or_schema_error(&json!({"errors": []})));
    assert!(!is_permission_or_schema_error(&json!({"data": {}})));
}

#[test]
fn binding_from_value_roundtrips_fields() {
    let value = json!({
        "schema_version": PR_FOLLOWUP_SCHEMA_VERSION,
        "run_id": "run-1",
        "repository_owner": "owner",
        "repository_name": "repo",
        "pr_number": 42,
        "head_ref": "feature",
        "head_sha": "abc",
        "base_ref": "main",
        "base_sha": "base"
    });
    let binding = binding_from_value(&value).unwrap();
    assert_eq!(binding.run_id, "run-1");
    assert_eq!(binding.pr_number, 42);
    assert_eq!(binding.base_sha, Some("base".to_string()));
}

#[test]
fn binding_from_value_missing_field_errors() {
    let value = json!({"run_id": "r"});
    assert!(binding_from_value(&value).is_err());
}

#[test]
fn current_step_id_prefers_context_value() {
    let mut context = StepContext::new(std::path::PathBuf::from("/tmp"), "run-1".to_string());
    assert_eq!(current_step_id(&context, "fallback"), "fallback");
    context.set("current_step_id", "real_step");
    assert_eq!(current_step_id(&context, "fallback"), "real_step");
}

#[test]
fn string_param_falls_back_to_default_then_context() {
    let mut context = StepContext::new(std::path::PathBuf::from("/tmp"), "run-1".to_string());
    let params = json!({"explicit": "from_params"});
    assert_eq!(
        string_param(&context, &params, "explicit", "def"),
        "from_params"
    );
    assert_eq!(string_param(&context, &params, "missing", "def"), "def");
    context.set("ctxkey", "from_ctx");
    assert_eq!(
        string_param(&context, &json!({}), "ctxkey", "def"),
        "from_ctx"
    );
}

#[test]
fn required_string_param_errors_when_empty() {
    let context = StepContext::new(std::path::PathBuf::from("/tmp"), "run-1".to_string());
    assert!(required_string_param(&context, &json!({}), "missing").is_err());
    assert_eq!(
        required_string_param(&context, &json!({"k": "v"}), "k").unwrap(),
        "v"
    );
}

#[test]
fn fallback_binding_uses_documented_defaults() {
    let context = StepContext::new(std::path::PathBuf::from("/tmp"), "run-xyz".to_string());
    let binding = fallback_binding(&context, &json!({})).unwrap();
    assert_eq!(binding.run_id, "run-xyz");
    assert_eq!(binding.repository_owner, "example");
    assert_eq!(binding.repository_name, "workflow");
    assert_eq!(binding.pr_number, 1910);
}

#[test]
fn artifact_root_rejects_unresolved_template() {
    let context = StepContext::new(std::path::PathBuf::from("/tmp/work"), "run-1".to_string());
    let params = json!({"artifact_root": "{unresolved}/dir"});
    assert!(artifact_root(&context, &params).is_err());
}

#[test]
fn artifact_root_joins_relative_paths_to_work_dir() {
    let context = StepContext::new(std::path::PathBuf::from("/tmp/work"), "run-1".to_string());
    let params = json!({"artifact_root": "artifacts/sub"});
    let root = artifact_root(&context, &params).unwrap();
    assert_eq!(root, std::path::PathBuf::from("/tmp/work/artifacts/sub"));
}

#[test]
fn github_feedback_error_carries_message() {
    let err = github_feedback_error("boom");
    match err {
        EngineError::StepExecutionError { message, .. } => assert_eq!(message, "boom"),
        other => panic!("unexpected error variant: {other:?}"),
    }
}
