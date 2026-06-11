//! Integration tests for graph-structural workflow validation.
//!
//! These exercise the production validation path (`parse_workflow_type_toml`
//! followed by `validate_workflow_type`, which now runs `validate_workflow_graph`),
//! not the test-local helpers. They cover both valid and invalid PR follow-up
//! graph variants.
//!
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
//! @requirement:REQ-PRFU-018,REQ-PRFU-020

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

/// The canonical valid workflow passes full validation with no errors.
#[test]
fn valid_workflow_passes_full_validation() {
    let fixture_root = std::path::PathBuf::from("tests/fixtures");
    // resolve_workflow_type now runs full validation internally; success here
    // proves the valid graph is not falsely rejected.
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

/// Each p16 duplicate fixture is rejected with the duplicate-branch message.
#[test]
fn duplicate_branch_fixtures_are_rejected() {
    let cases = [
        (
            "tests/fixtures/workflows/invalid/p16-duplicate-create-pr-success.toml",
            "duplicate post-PR transition branch for create_pr outcome success",
        ),
        (
            "tests/fixtures/workflows/invalid/p16-duplicate-watch-pr-checks-fatal.toml",
            "duplicate post-PR transition branch for watch_pr_checks outcome fatal",
        ),
        (
            "tests/fixtures/workflows/invalid/p16-duplicate-build-remediation-plan-success.toml",
            "duplicate post-PR transition branch for build_remediation_plan outcome success",
        ),
    ];

    for (path, expected_substring) in cases {
        let workflow = parse_fixture(path);

        // Direct graph validation surfaces the duplicate-outcome error.
        let graph_errors = validate_workflow_graph(&workflow)
            .expect_err(&format!("{path} must fail graph validation"));
        assert!(
            graph_errors.iter().any(|error| {
                error.category == GraphErrorCategory::DuplicateOutcome
                    && error.detail.contains(expected_substring)
            }),
            "{path}: expected duplicate-outcome error containing {expected_substring:?}, got {graph_errors:?}"
        );

        // The config-level validator surfaces it as a ValidationError too.
        let config_error = validate_workflow_type(&workflow)
            .expect_err(&format!("{path} must fail validate_workflow_type"));
        assert_eq!(config_error.kind, ConfigErrorKind::ValidationError);
        assert!(
            config_error.message.contains(expected_substring),
            "{path}: ConfigError message must contain {expected_substring:?}, got {:?}",
            config_error.message
        );
    }
}

/// The post-PR fatal-to-abandon fixture is rejected as an unsafe route.
#[test]
fn post_pr_fatal_to_abandon_fixture_is_rejected() {
    let path = "tests/fixtures/workflows/invalid/p16-post-pr-fatal-to-abandon.toml";
    let workflow = parse_fixture(path);

    let graph_errors = validate_workflow_graph(&workflow)
        .expect_err("p16-post-pr-fatal-to-abandon must fail graph validation");
    assert!(
        graph_errors.iter().any(|error| {
            error.category == GraphErrorCategory::UnsafePostPrRoute
                && error
                    .detail
                    .contains("capture_pr_identity -> abandon_and_log is forbidden")
        }),
        "expected unsafe post-PR route error, got {graph_errors:?}"
    );

    let config_error =
        validate_workflow_type(&workflow).expect_err("must fail validate_workflow_type");
    assert_eq!(config_error.kind, ConfigErrorKind::ValidationError);
}

/// The new dangling-transition-target fixture is rejected.
#[test]
fn dangling_transition_target_fixture_is_rejected() {
    let path = "tests/fixtures/workflows/invalid/p10-dangling-transition-target.toml";
    let workflow = parse_fixture(path);

    let graph_errors = validate_workflow_graph(&workflow)
        .expect_err("p10-dangling-transition-target must fail graph validation");
    assert!(
        graph_errors.iter().any(|error| {
            error.category == GraphErrorCategory::DanglingTransition
                && error.detail.contains("does_not_exist")
        }),
        "expected dangling-transition error naming the missing step, got {graph_errors:?}"
    );

    let config_error =
        validate_workflow_type(&workflow).expect_err("must fail validate_workflow_type");
    assert_eq!(config_error.kind, ConfigErrorKind::ValidationError);
}

/// The new unreachable-required-collector fixture is rejected.
#[test]
fn unreachable_required_collector_fixture_is_rejected() {
    let path = "tests/fixtures/workflows/invalid/p10-unreachable-required-step.toml";
    let workflow = parse_fixture(path);

    let graph_errors = validate_workflow_graph(&workflow)
        .expect_err("p10-unreachable-required-step must fail graph validation");
    assert!(
        graph_errors.iter().any(|error| {
            error.category == GraphErrorCategory::MissingRequiredCollector
                && error.detail.contains("collect_coderabbit_feedback")
        }),
        "expected missing/unreachable required-collector error, got {graph_errors:?}"
    );

    let config_error =
        validate_workflow_type(&workflow).expect_err("must fail validate_workflow_type");
    assert_eq!(config_error.kind, ConfigErrorKind::ValidationError);
}
