//! Integration tests for the loop-limit and terminal-routing workflow contract.
//!
//! These exercise the production validation path (`parse_workflow_type_toml`
//! followed by `validate_workflow_graph` / `validate_workflow_type`) for the
//! three new structural invariants:
//!
//! 1. Loop-back transitions must declare an explicit `max_iterations` cap.
//! 2. Terminal steps must not declare outgoing transitions.
//! 3. `post_pr_iteration_guard` steps must declare a positive remediation cap.
//!
//! @plan:PLAN-20260404-INITIAL-RUNTIME.P03

use luther_workflow::workflow::config_loader::{
    parse_workflow_type_toml, resolve_workflow_type, validate_workflow_type, ConfigErrorKind,
};
use luther_workflow::workflow::validation::{validate_workflow_graph, GraphErrorCategory};

/// Load and parse a workflow TOML fixture *without* running validation, so the
/// invalid fixtures (which fail validation) can still be parsed for testing.
fn parse_fixture(path: &str) -> luther_workflow::workflow::WorkflowType {
    let content = std::fs::read_to_string(path).unwrap_or_else(|err| panic!("read {path}: {err}"));
    parse_workflow_type_toml(&content).unwrap_or_else(|err| panic!("parse {path}: {err}"))
}

/// Assert a fixture is rejected by both validators with the expected category
/// and a greppable substring in the config error message.
fn assert_fixture_rejected(path: &str, category: GraphErrorCategory, substring: &str) {
    let workflow = parse_fixture(path);

    let graph_errors = validate_workflow_graph(&workflow)
        .expect_err(&format!("{path} must fail graph validation"));
    assert!(
        graph_errors
            .iter()
            .any(|error| error.category == category && error.detail.contains(substring)),
        "{path}: expected {category:?} error containing {substring:?}, got {graph_errors:?}"
    );

    let config_error =
        validate_workflow_type(&workflow).expect_err(&format!("{path} must fail validation"));
    assert_eq!(config_error.kind, ConfigErrorKind::ValidationError);
    assert!(
        config_error.message.contains(substring),
        "{path}: ConfigError message must contain {substring:?}, got {:?}",
        config_error.message
    );
}

#[test]
fn loopback_missing_max_iterations_fixture_is_rejected() {
    assert_fixture_rejected(
        "tests/fixtures/workflows/invalid/loopback-missing-max-iterations.toml",
        GraphErrorCategory::MissingLoopLimit,
        "loop-back transition retry --fixable--> start",
    );
}

#[test]
fn terminal_step_with_outgoing_transition_fixture_is_rejected() {
    assert_fixture_rejected(
        "tests/fixtures/workflows/invalid/terminal-step-has-outgoing-transition.toml",
        GraphErrorCategory::TerminalHasOutgoing,
        "terminal step 'done' must not declare an outgoing transition",
    );
}

#[test]
fn pr_iteration_guard_missing_cap_fixture_is_rejected() {
    assert_fixture_rejected(
        "tests/fixtures/workflows/invalid/pr-iteration-guard-missing-cap.toml",
        GraphErrorCategory::MissingRemediationCap,
        "post_pr_iteration_guard step 'guard' must declare a positive",
    );
}

/// The canonical shipped workflow still passes the new contract: its loop-backs
/// are explicitly capped, its terminal has no outgoing route, and its
/// `post_pr_iteration_guard` declares a positive cap. This guards against false
/// rejection of production configs by the new validators.
#[test]
fn canonical_valid_workflow_still_passes_new_contract() {
    let fixture_root = std::path::PathBuf::from("tests/fixtures");
    let workflow = resolve_workflow_type("llxprt-issue-fix-v1", &fixture_root)
        .expect("valid workflow must resolve and validate");
    assert!(
        validate_workflow_graph(&workflow).is_ok(),
        "valid workflow must pass graph validation without errors"
    );
    assert!(
        validate_workflow_type(&workflow).is_ok(),
        "valid workflow must pass full validation without errors"
    );
}
