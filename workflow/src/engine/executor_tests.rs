use super::*;
use std::collections::HashMap;
use std::path::PathBuf;

#[test]
fn default_registry_includes_git_config_publisher() {
    assert!(ExecutorRegistry::with_defaults().contains_step_type("git_config_publish"));
}

#[test]
fn issue_number_falls_back_to_primary_issue_number() {
    let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-1".to_string());
    context.set("primary_issue_number", "3");

    assert_eq!(context.get("issue_number").map(String::as_str), Some("3"));
    assert_eq!(
        interpolate_string("issue{issue_number}", &context),
        "issue3"
    );
}

#[test]
fn explicit_issue_number_takes_precedence_over_primary_issue_number() {
    let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-1".to_string());
    context.set("primary_issue_number", "3");
    context.set("issue_number", "4");

    assert_eq!(context.get("issue_number").map(String::as_str), Some("4"));
    assert_eq!(
        interpolate_string("issue{issue_number}", &context),
        "issue4"
    );
}

#[test]
fn checkpoint_values_round_trip_safe_context_and_redact_outputs_and_secrets() {
    let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-1".to_string());
    context.set_current_step_id("shell");
    context.set("issue_number", "137");
    context.set("issue_title", "Preserve failed identity");
    context.set("head_sha", "abcdef");
    context.set("base_sha", "123456");
    context.set("github_token", "flat-secret");
    context.set("api_key", "namespaced-secret");
    context.set("stdout", "raw output secret");
    context.set("stderr", "raw error secret");

    let values = context.checkpoint_values();
    let serialized = serde_json::to_string(&values).expect("serialize checkpoint context");
    assert!(serialized.contains("137"));
    assert!(!serialized.contains("flat-secret"));
    assert!(!serialized.contains("namespaced-secret"));
    assert!(!serialized.contains("raw output secret"));
    assert!(!serialized.contains("raw error secret"));

    let mut restored = StepContext::new(PathBuf::from("/tmp/other"), "run-2".to_string());
    restored
        .restore_checkpoint_values(values)
        .expect("restore checkpoint context");
    assert_eq!(
        restored.get("issue_number").map(String::as_str),
        Some("137")
    );
    assert_eq!(
        restored.get("shell.issue_number").map(String::as_str),
        Some("137")
    );
    assert_eq!(
        restored.get("issue_title").map(String::as_str),
        Some("Preserve failed identity")
    );
    assert_eq!(restored.get("head_sha").map(String::as_str), Some("abcdef"));
    assert_eq!(restored.get("base_sha").map(String::as_str), Some("123456"));
    assert_eq!(restored.work_dir(), &PathBuf::from("/tmp/other"));
    assert_eq!(restored.run_id(), "run-2");
    assert!(restored.get("github_token").is_none());
    assert!(restored.get("shell.api_key").is_none());
    assert!(restored.get("shell.stdout").is_none());
    assert!(restored.get("shell.stderr").is_none());
}

#[test]
fn extract_tokens_simple_and_namespaced() {
    assert_eq!(extract_tokens("{artifact_dir}"), vec!["artifact_dir"]);
    assert_eq!(
        extract_tokens("{setup_workspace.existing_pr_number}"),
        vec!["setup_workspace.existing_pr_number"]
    );
}

#[test]
fn extract_tokens_multiple_and_adjacent_text() {
    assert_eq!(
        extract_tokens("path/{artifact_dir}/x.json"),
        vec!["artifact_dir"]
    );
    assert_eq!(
        extract_tokens("{owner}/{repo}#{issue_number}"),
        vec!["owner", "repo", "issue_number"]
    );
}

#[test]
fn extract_tokens_none_when_no_tokens() {
    assert!(extract_tokens("no tokens here").is_empty());
    assert!(extract_tokens("").is_empty());
}

#[test]
fn extract_tokens_ignores_jq_object_braces() {
    // jq object construction contains spaces/commas/colons -> not tokens.
    assert!(extract_tokens("{number, title}").is_empty());
    assert!(extract_tokens("{title: .title, url: .url}").is_empty());
}

#[test]
fn extract_tokens_ignores_shell_style_dollar_brace() {
    // Shell-style `${VAR}` is env/shell interpolation, not a Luther token.
    assert!(extract_tokens("echo ${HOME}").is_empty());
    assert!(extract_tokens("${FOO}/${BAR}").is_empty());
}

#[test]
fn extract_tokens_distinguishes_dollar_brace_from_bare_brace() {
    // Bare `{VAR}` is still extracted; the adjacent `${VAR}` is skipped.
    assert_eq!(
        extract_tokens("${HOME}/{artifact_dir}/${USER}"),
        vec!["artifact_dir"]
    );
}

#[test]
fn restore_checkpoint_values_filters_disallowed_namespaced_keys() {
    // A malicious payload carrying secret-like keys in a namespaced map
    // must be filtered through the allowlist on restore, so secrets never
    // re-enter the live context even if they slipped into persisted state.
    let mut payload: HashMap<String, serde_json::Value> = HashMap::new();
    let mut malicious = HashMap::new();
    let mut inner = HashMap::new();
    inner.insert("issue_number".to_string(), "42".to_string());
    inner.insert("github_token".to_string(), "bearer-secret".to_string());
    inner.insert("api_key".to_string(), "key-secret".to_string());
    inner.insert("stdout".to_string(), "raw-output-secret".to_string());
    malicious.insert("shell".to_string(), inner);
    payload.insert(
        "__namespaced_vars".to_string(),
        serde_json::to_value(&malicious).unwrap(),
    );
    payload.insert(
        "issue_number".to_string(),
        serde_json::Value::String("42".into()),
    );

    let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-restore".to_string());
    context
        .restore_checkpoint_values(payload)
        .expect("restore must not error on a well-formed payload");

    // Allowed key survives.
    assert_eq!(
        context.get("shell.issue_number").map(String::as_str),
        Some("42")
    );
    // Disallowed keys are dropped.
    assert!(context.get("shell.github_token").is_none());
    assert!(context.get("shell.api_key").is_none());
    assert!(context.get("shell.stdout").is_none());
}

#[test]
fn restore_checkpoint_values_rejects_malformed_namespaced_payload() {
    // A structurally invalid `__namespaced_vars` value (inner map values
    // not strings) must fail closed with the serde error rather than
    // silently substituting an empty/default context.
    let mut payload: HashMap<String, serde_json::Value> = HashMap::new();
    let mut malformed = HashMap::new();
    let mut inner: HashMap<String, serde_json::Value> = HashMap::new();
    inner.insert(
        "issue_number".to_string(),
        serde_json::Value::Number(serde_json::Number::from(42u64)),
    );
    malformed.insert("shell".to_string(), serde_json::to_value(&inner).unwrap());
    payload.insert(
        "__namespaced_vars".to_string(),
        serde_json::to_value(&malformed).unwrap(),
    );

    let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-malformed".to_string());
    let result = context.restore_checkpoint_values(payload);
    assert!(
        result.is_err(),
        "malformed __namespaced_vars must fail closed, got: {result:?}"
    );
}

#[test]
fn restore_checkpoint_values_rejects_non_string_allowlisted_top_level_value() {
    // An allowlisted top-level checkpoint key (e.g. `issue_number`) must
    // only ever carry a string value: `checkpoint_values` only produces
    // strings for allowlisted keys. A non-string value (e.g. a JSON number)
    // is a corruption signal and must fail closed rather than be silently
    // dropped — otherwise a corrupted persisted payload could cause a run
    // to resume with a missing identity anchor.
    let mut payload: HashMap<String, serde_json::Value> = HashMap::new();
    payload.insert(
        "issue_number".to_string(),
        serde_json::Value::Number(serde_json::Number::from(42u64)),
    );

    let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-non-string".to_string());
    let result = context.restore_checkpoint_values(payload);
    let error =
        result.expect_err("a non-string value for an allowlisted checkpoint key must fail closed");
    assert!(
        error.to_string().contains("issue_number"),
        "error should name the offending key, got: {error}"
    );
    assert!(
        error.to_string().contains("not a string"),
        "error should explain the value is not a string, got: {error}"
    );
    // No partial restore: the corrupted key must not enter the live context.
    assert!(context.get("issue_number").is_none());
}

#[test]
fn restore_checkpoint_values_still_ignores_malformed_disallowed_keys() {
    // A malformed value under a *disallowed* key must still be ignored, so
    // a corrupted persisted payload cannot poison the restore path. Only
    // allowlisted keys are validated for type; disallowed keys are dropped
    // before any type check.
    let mut payload: HashMap<String, serde_json::Value> = HashMap::new();
    payload.insert(
        "github_token".to_string(),
        serde_json::Value::Number(serde_json::Number::from(42u64)),
    );
    payload.insert(
        "issue_number".to_string(),
        serde_json::Value::String("137".into()),
    );

    let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-disallowed".to_string());
    context
        .restore_checkpoint_values(payload)
        .expect("malformed disallowed keys must not fail the restore");

    // Allowlisted key is restored; disallowed key is dropped silently.
    assert_eq!(context.get("issue_number").map(String::as_str), Some("137"));
    assert!(context.get("github_token").is_none());
}

#[test]
fn restore_checkpoint_values_rejects_non_string_for_each_allowlisted_key() {
    // Every allowlisted top-level checkpoint key must reject a non-string
    // value, so the fail-closed behavior is uniform across the allowlist.
    for key in [
        "primary_issue_number",
        "issue_title",
        "pr_number",
        "owner",
        "repo",
        "repository",
        "current_branch",
        "base_branch",
        "existing_pr_number",
        "head_ref",
        "head_sha",
        "base_ref",
        "base_sha",
    ] {
        let mut payload: HashMap<String, serde_json::Value> = HashMap::new();
        payload.insert(
            key.to_string(),
            serde_json::Value::Number(serde_json::Number::from(42u64)),
        );
        let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-each-key".to_string());
        let result = context.restore_checkpoint_values(payload);
        assert!(
            result.is_err(),
            "non-string value for allowlisted key '{key}' must fail closed, got: {result:?}"
        );
    }
}

// -----------------------------------------------------------------------
// Shell-safe token validation (issue 158 finding F)
// -----------------------------------------------------------------------

#[test]
fn validate_accepts_safe_numeric_issue_number() {
    let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-1".to_string());
    context.set("issue_number", "137");
    assert!(validate_shell_safe_tokens(&context).is_ok());
}

#[test]
fn validate_accepts_safe_primary_issue_number_fallback() {
    let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-1".to_string());
    context.set("primary_issue_number", "42");
    assert!(validate_shell_safe_tokens(&context).is_ok());
}

#[test]
fn validate_accepts_safe_base_branch() {
    let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-1".to_string());
    context.set("base_branch", "main");
    assert!(validate_shell_safe_tokens(&context).is_ok());
}

#[test]
fn validate_accepts_safe_slashed_base_branch() {
    let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-1".to_string());
    context.set("base_branch", "release/v1.2");
    assert!(validate_shell_safe_tokens(&context).is_ok());
}

#[test]
fn validate_accepts_safe_artifact_dir() {
    let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-1".to_string());
    context.set("artifact_dir", "/tmp/artifacts/run-1");
    assert!(validate_shell_safe_tokens(&context).is_ok());
}

#[test]
fn validate_accepts_context_without_identity_tokens() {
    let context = StepContext::new(PathBuf::from("/tmp/work"), "run-1".to_string());
    assert!(validate_shell_safe_tokens(&context).is_ok());
}

#[test]
fn validate_rejects_semicolon_in_issue_number() {
    let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-1".to_string());
    context.set("issue_number", "137; touch pwned");
    let err = validate_shell_safe_tokens(&context).expect_err("semicolon must be rejected");
    assert!(err.contains("issue_number"), "error: {err}");
    assert!(err.contains("non-digit"), "error: {err}");
}

#[test]
fn validate_rejects_backtick_in_issue_number() {
    let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-1".to_string());
    context.set("issue_number", "137`touch pwned`");
    assert!(validate_shell_safe_tokens(&context).is_err());
}

#[test]
fn validate_rejects_command_substitution_in_issue_number() {
    let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-1".to_string());
    // The $() would be literal here since we set a string value, but the
    // presence of non-digit characters must still be rejected.
    context.set("issue_number", "1$(touch pwned)");
    assert!(validate_shell_safe_tokens(&context).is_err());
}

#[test]
fn validate_rejects_empty_issue_number() {
    let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-1".to_string());
    context.set("issue_number", "");
    let err = validate_shell_safe_tokens(&context).expect_err("empty must be rejected");
    assert!(err.contains("empty"), "error: {err}");
}

#[test]
fn validate_rejects_non_digit_alpha_in_issue_number() {
    let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-1".to_string());
    context.set("issue_number", "13a");
    assert!(validate_shell_safe_tokens(&context).is_err());
}

#[test]
fn validate_rejects_semicolon_in_base_branch() {
    let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-1".to_string());
    context.set("base_branch", "main; touch pwned");
    let err = validate_shell_safe_tokens(&context).expect_err("semicolon must be rejected");
    assert!(err.contains("base_branch"), "error: {err}");
    assert!(err.contains("unsafe refname"), "error: {err}");
}

#[test]
fn validate_rejects_dollar_in_base_branch() {
    let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-1".to_string());
    context.set("base_branch", "main$(touch)");
    assert!(validate_shell_safe_tokens(&context).is_err());
}

#[test]
fn validate_rejects_space_in_base_branch() {
    let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-1".to_string());
    context.set("base_branch", "main feature");
    assert!(validate_shell_safe_tokens(&context).is_err());
}

#[test]
fn validate_rejects_shell_metacharacter_in_artifact_dir() {
    let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-1".to_string());
    context.set("artifact_dir", "/tmp/artifacts; rm -rf /");
    let err = validate_shell_safe_tokens(&context).expect_err("semicolon must be rejected");
    assert!(err.contains("artifact_dir"), "error: {err}");
    assert!(err.contains("metacharacter"), "error: {err}");
}

#[test]
fn validate_rejects_backtick_in_artifact_dir() {
    let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-1".to_string());
    context.set("artifact_dir", "/tmp/`touch pwned`");
    assert!(validate_shell_safe_tokens(&context).is_err());
}

#[test]
fn validate_rejects_pipe_in_work_dir() {
    let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-1".to_string());
    // work_dir is a built-in, so override it after construction.
    context.set("work_dir", "/tmp/work|cat /etc/passwd");
    assert!(validate_shell_safe_tokens(&context).is_err());
}
