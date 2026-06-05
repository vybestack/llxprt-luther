/// @plan:PLAN-20260408-LLXPRT-FIRST.P10
/// Namespaced Context TDD Tests
/// Tests compile but fail until Phase 11 implementation.
use std::path::PathBuf;

// Import from the executor module
use luther_workflow::engine::executor::{interpolate_string, StepContext};

/// @plan:PLAN-20260408-LLXPRT-FIRST.P10
/// @requirement:REQ-LF-CTX-003
/// Test 1: set with `step_id` stores variable under that step's namespace
#[test]
fn test_set_with_step_id_stores_namespaced_variable() {
    let mut ctx = StepContext::new(PathBuf::from("/tmp/test"), "run-abc".to_string());

    // Set current step, then set a variable
    ctx.set_current_step_id("fetch_issue");
    ctx.set("issue_title", "Fix bug");

    // Namespaced access should find the variable
    assert_eq!(
        ctx.get("fetch_issue.issue_title"),
        Some(&"Fix bug".to_string())
    );
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P10
/// @requirement:REQ-LF-CTX-001
/// Test 2: namespaced get returns value from specific step (multiple steps)
#[test]
fn test_namespaced_get_returns_value_from_specific_step() {
    let mut ctx = StepContext::new(PathBuf::from("/tmp/test"), "run-abc".to_string());

    // Step A sets result
    ctx.set_current_step_id("step_a");
    ctx.set("result", "aaa");

    // Step B sets result (different namespace)
    ctx.set_current_step_id("step_b");
    ctx.set("result", "bbb");

    // Namespaced access returns correct values
    assert_eq!(ctx.get("step_a.result"), Some(&"aaa".to_string()));
    assert_eq!(ctx.get("step_b.result"), Some(&"bbb".to_string()));
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P10
/// @requirement:REQ-LF-CTX-002
/// Test 3: unnamespaced get returns most recent value (most-recent-writer-first)
#[test]
fn test_unnamespaced_get_returns_most_recent_value() {
    let mut ctx = StepContext::new(PathBuf::from("/tmp/test"), "run-abc".to_string());

    // Step A sets stdout
    ctx.set_current_step_id("step_a");
    ctx.set("stdout", "first");

    // Step B sets stdout (later in order)
    ctx.set_current_step_id("step_b");
    ctx.set("stdout", "second");

    // Unnamespaced access should return most recent (step_b)
    assert_eq!(ctx.get("stdout"), Some(&"second".to_string()));
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P10
/// @requirement:REQ-LF-CTX-001
/// Test 4: `interpolate_string` with namespaced template token
#[test]
fn test_interpolate_namespaced_template_token() {
    let mut ctx = StepContext::new(PathBuf::from("/tmp/test"), "run-abc".to_string());

    // Simulate: fetch_issue step set issue_number = "42"
    ctx.set_current_step_id("fetch_issue");
    ctx.set("issue_number", "42");

    // Interpolate using namespaced template token
    let template = "Fixes #{fetch_issue.issue_number}";
    let result = interpolate_string(template, &ctx);

    assert_eq!(result, "Fixes #42");
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P10
/// @requirement:REQ-LF-CTX-002
/// Test 5: interpolate with unnamespaced template token still works
#[test]
fn test_interpolate_unnamespaced_template_token_still_works() {
    let mut ctx = StepContext::new(PathBuf::from("/tmp/test"), "run-abc".to_string());

    // Step A sets greeting
    ctx.set_current_step_id("step_a");
    ctx.set("greeting", "hello");

    // Interpolate using unnamespaced template token
    let template = "{greeting} world";
    let result = interpolate_string(template, &ctx);

    assert_eq!(result, "hello world");
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P10
/// @requirement:REQ-LF-CTX-001,REQ-LF-CTX-002
/// Test 6: interpolate mixed namespaced and unnamespaced template tokens
#[test]
fn test_interpolate_mixed_namespaced_and_unnamespaced() {
    let mut ctx = StepContext::new(PathBuf::from("/tmp/test"), "run-abc".to_string());

    // fetch_issue step sets issue_number
    ctx.set_current_step_id("fetch_issue");
    ctx.set("issue_number", "42");

    // setup step sets branch
    ctx.set_current_step_id("setup");
    ctx.set("branch", "issue42");

    // Mixed interpolation: namespaced {setup.branch} and unnamespaced {issue_number}
    let template = "branch {setup.branch} for issue {issue_number}";
    let result = interpolate_string(template, &ctx);

    assert_eq!(result, "branch issue42 for issue 42");
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P10
/// @requirement:REQ-LF-CTX-004
/// Test 7: built-in `work_dir` resolves without namespace
#[test]
fn test_builtin_work_dir_resolves_without_namespace() {
    let ctx = StepContext::new(PathBuf::from("/tmp/test"), "run-abc".to_string());

    // Built-in {work_dir} should resolve
    let template = "{work_dir}/output.txt";
    let result = interpolate_string(template, &ctx);

    assert_eq!(result, "/tmp/test/output.txt");
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P10
/// @requirement:REQ-LF-CTX-004
/// Test 8: built-in `run_id` resolves without namespace
#[test]
fn test_builtin_run_id_resolves_without_namespace() {
    let ctx = StepContext::new(PathBuf::from("/tmp/test"), "run-abc".to_string());

    // Built-in {run_id} should resolve
    let template = "Run: {run_id}";
    let result = interpolate_string(template, &ctx);

    assert_eq!(result, "Run: run-abc");
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P10
/// @requirement:REQ-LF-CTX-003
/// Test 9: set without `step_id` stores bare key only (backward compat)
#[test]
fn test_set_without_step_id_stores_bare_key_only() {
    let mut ctx = StepContext::new(PathBuf::from("/tmp/test"), "run-abc".to_string());

    // Set WITHOUT calling set_current_step_id first
    ctx.set("foo", "bar");

    // Bare key access should work
    assert_eq!(ctx.get("foo"), Some(&"bar".to_string()));

    // Namespaced access with fake step should NOT find it (no step set)
    // No "None.foo" or "null.foo" should exist
    assert!(ctx.get("None.foo").is_none());
    assert!(ctx.get("null.foo").is_none());
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P10
/// @requirement:REQ-LF-CTX-001,REQ-LF-CTX-002
/// Test 10: namespaced and bare keys coexist correctly
#[test]
fn test_namespaced_and_bare_keys_coexist() {
    let mut ctx = StepContext::new(PathBuf::from("/tmp/test"), "run-abc".to_string());

    // Step A sets val
    ctx.set_current_step_id("step_a");
    ctx.set("val", "namespaced");

    // Direct set without step_id (different key)
    ctx.set("other_key", "bare");

    // Namespaced access should work
    assert_eq!(ctx.get("step_a.val"), Some(&"namespaced".to_string()));

    // Unnamespaced access should find step_a's value via most-recent-first
    assert_eq!(ctx.get("val"), Some(&"namespaced".to_string()));

    // Bare key access should work
    assert_eq!(ctx.get("other_key"), Some(&"bare".to_string()));
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P10
/// @requirement:REQ-LF-CTX-001
/// Test 11: unknown namespaced key returns None
#[test]
fn test_unknown_namespaced_key_returns_none() {
    let ctx = StepContext::new(PathBuf::from("/tmp/test"), "run-abc".to_string());

    // Non-existent step and variable
    assert!(ctx.get("nonexistent_step.var").is_none());
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P10
/// @requirement:REQ-LF-CTX-002
/// Test 12: undefined template token left as-is (backward compat)
#[test]
fn test_interpolate_undefined_template_token_left_as_is() {
    let ctx = StepContext::new(PathBuf::from("/tmp/test"), "run-abc".to_string());

    // Undefined variable should be left unchanged
    let template = "{undefined_var}";
    let result = interpolate_string(template, &ctx);

    assert_eq!(result, "{undefined_var}");
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P10
/// @requirement:REQ-LF-CTX-002,REQ-LF-CTX-004
/// Test 13: config-seeded variable resolves as bare name (config namespace fallback)
#[test]
fn test_config_seeded_variable_resolves_as_bare_name() {
    let mut ctx = StepContext::new(PathBuf::from("/tmp/test"), "run-abc".to_string());

    // Simulate config-seeded variable loaded into namespaced_vars["config"]
    // This simulates Phase 15 behavior
    ctx.namespaced_vars
        .entry("config".to_string())
        .or_default()
        .insert("target_repo".to_string(), "owner/repo".to_string());

    // Bare name access should find config-seeded value via fallback
    assert_eq!(ctx.get("target_repo"), Some(&"owner/repo".to_string()));

    // Set a step context (but don't set target_repo in it)
    ctx.set_current_step_id("step_a");
    ctx.set("foo", "bar");

    // target_repo should still resolve to config value (step doesn't override)
    assert_eq!(ctx.get("target_repo"), Some(&"owner/repo".to_string()));
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P10
/// @requirement:REQ-LF-CTX-002,REQ-LF-CTX-004
/// Test 14: step output overrides config-seeded variable (most-recent-writer-first)
#[test]
fn test_step_output_overrides_config_seeded_variable() {
    let mut ctx = StepContext::new(PathBuf::from("/tmp/test"), "run-abc".to_string());

    // Simulate config-seeded variable
    ctx.namespaced_vars
        .entry("config".to_string())
        .or_default()
        .insert("target_repo".to_string(), "owner/repo".to_string());

    // Setup step overrides target_repo
    ctx.set_current_step_id("setup");
    ctx.set("target_repo", "other/repo");

    // Bare name access should find step's value (most recent writer)
    assert_eq!(ctx.get("target_repo"), Some(&"other/repo".to_string()));

    // Explicit namespaced access should return step's value
    assert_eq!(
        ctx.get("setup.target_repo"),
        Some(&"other/repo".to_string())
    );
}

/// @plan:PLAN-20260408-LLXPRT-FIRST.P10
/// @requirement:REQ-LF-CTX-001,REQ-LF-CTX-004
/// Test 15: qualified config variable access {`config.variable_name`}
#[test]
fn test_config_variable_qualified_access() {
    let mut ctx = StepContext::new(PathBuf::from("/tmp/test"), "run-abc".to_string());

    // Simulate config-seeded variable in "config" namespace
    ctx.namespaced_vars
        .entry("config".to_string())
        .or_default()
        .insert("target_repo".to_string(), "owner/repo".to_string());

    // Qualified access to config namespace
    assert_eq!(
        ctx.get("config.target_repo"),
        Some(&"owner/repo".to_string())
    );
}
