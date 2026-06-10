/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// Integration coverage for production workflow graph validation.
///
/// These tests exercise the production load path (`resolve_workflow_type` and
/// `validate_workflow_type` -> `validate_workflow_graph`) to prove that invalid
/// or unsafe routing is rejected before an engine is ever constructed, while the
/// real workflow continues to load successfully.
///
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
use std::path::PathBuf;

use luther_workflow::workflow::config_loader::{
    parse_workflow_type_toml, resolve_workflow_type, validate_workflow_type, ConfigErrorKind,
};
use luther_workflow::workflow::validation::{validate_workflow_graph, GraphErrorKind};

/// Load and validate an invalid fixture through the production validator,
/// returning the resulting `ConfigError`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
fn validate_invalid_fixture(name: &str) -> luther_workflow::workflow::config_loader::ConfigError {
    let path = format!("tests/fixtures/workflows/invalid/{name}");
    let content =
        std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {path}: {err}"));
    let workflow_type =
        parse_workflow_type_toml(&content).unwrap_or_else(|err| panic!("parse {path}: {err}"));
    validate_workflow_type(&workflow_type)
        .expect_err(&format!("expected {name} to fail graph validation"))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
#[test]
fn valid_production_workflow_passes_graph_validation() {
    let fixture_root = PathBuf::from("tests/fixtures");
    let workflow_type = resolve_workflow_type("llxprt-issue-fix-v1", &fixture_root)
        .expect("valid production workflow must pass graph validation on the load path");
    // Sanity: the post-PR sub-graph is present, so the post-PR validators ran.
    assert!(
        workflow_type
            .steps
            .iter()
            .any(|step| step.step_id == "capture_pr_identity"),
        "expected the valid fixture to contain the post-PR entry step"
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
#[test]
fn resolve_rejects_duplicate_create_pr_success() {
    let err = validate_invalid_fixture("p16-duplicate-create-pr-success.toml");
    assert_eq!(err.kind, ConfigErrorKind::ValidationError);
    assert!(
        err.message.contains("create_pr") && err.message.contains("success"),
        "expected duplicate create_pr/success message, got: {}",
        err.message
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
#[test]
fn resolve_rejects_duplicate_watch_pr_checks_fatal() {
    let err = validate_invalid_fixture("p16-duplicate-watch-pr-checks-fatal.toml");
    assert_eq!(err.kind, ConfigErrorKind::ValidationError);
    assert!(
        err.message.contains("watch_pr_checks") && err.message.contains("fatal"),
        "expected duplicate watch_pr_checks/fatal message, got: {}",
        err.message
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
#[test]
fn resolve_rejects_duplicate_build_remediation_plan_success() {
    let err = validate_invalid_fixture("p16-duplicate-build-remediation-plan-success.toml");
    assert_eq!(err.kind, ConfigErrorKind::ValidationError);
    assert!(
        err.message.contains("build_remediation_plan") && err.message.contains("success"),
        "expected duplicate build_remediation_plan/success message, got: {}",
        err.message
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
#[test]
fn resolve_rejects_post_pr_fatal_to_abandon() {
    let err = validate_invalid_fixture("p16-post-pr-fatal-to-abandon.toml");
    assert_eq!(err.kind, ConfigErrorKind::ValidationError);
    assert!(
        err.message.contains("abandon_and_log"),
        "expected unsafe post-PR route message mentioning abandon_and_log, got: {}",
        err.message
    );
}

/// Direct graph-validator coverage for the invalid PR follow-up variants, so the
/// error categorization (not just the load-path mapping) is asserted.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
#[test]
fn graph_validator_categorizes_duplicate_and_unsafe_routes() {
    for name in [
        "p16-duplicate-create-pr-success.toml",
        "p16-duplicate-watch-pr-checks-fatal.toml",
        "p16-duplicate-build-remediation-plan-success.toml",
    ] {
        let path = format!("tests/fixtures/workflows/invalid/{name}");
        let content =
            std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {path}: {err}"));
        let workflow_type =
            parse_workflow_type_toml(&content).unwrap_or_else(|err| panic!("parse {path}: {err}"));
        let errors = validate_workflow_graph(&workflow_type)
            .expect_err(&format!("expected {name} to produce graph errors"));
        assert!(
            errors
                .iter()
                .any(|e| e.category == GraphErrorKind::DuplicateOutcome),
            "{name} should produce a DuplicateOutcome error; got {errors:?}"
        );
    }

    let path = "tests/fixtures/workflows/invalid/p16-post-pr-fatal-to-abandon.toml";
    let content =
        std::fs::read_to_string(path).unwrap_or_else(|err| panic!("read {path}: {err}"));
    let workflow_type =
        parse_workflow_type_toml(&content).unwrap_or_else(|err| panic!("parse {path}: {err}"));
    let errors = validate_workflow_graph(&workflow_type)
        .expect_err("expected fatal-to-abandon fixture to produce graph errors");
    // This fixture routes a post-PR step toward abandon_and_log instead of the
    // post-PR failure terminal. Depending on whether the post-PR entry step is
    // defined, that surfaces either as an unsafe post-PR route or as a dangling
    // reference, but it must always be rejected.
    assert!(
        errors.iter().any(|e| matches!(
            e.category,
            GraphErrorKind::UnsafePostPrRoute | GraphErrorKind::DanglingTarget
        )),
        "fatal-to-abandon fixture should be rejected as unsafe/dangling; got {errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("abandon_and_log")
                || e.message.contains("capture_pr_identity")),
        "fatal-to-abandon fixture rejection should reference the offending route; got {errors:?}"
    );
}
