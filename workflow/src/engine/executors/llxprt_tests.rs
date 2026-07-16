//! Tests for [`super::llxprt`].

use super::*;
use serde_json::json;
use std::path::PathBuf;

#[test]
fn parse_outcome_name_maps_known_names() {
    assert!(matches!(
        parse_outcome_name("success"),
        StepOutcome::Success
    ));
    assert!(matches!(
        parse_outcome_name("fixable"),
        StepOutcome::Fixable
    ));
    assert!(matches!(parse_outcome_name("fatal"), StepOutcome::Fatal));
    assert!(matches!(
        parse_outcome_name("retryable"),
        StepOutcome::Retryable
    ));
    assert!(matches!(
        parse_outcome_name("abandon"),
        StepOutcome::Abandon
    ));
}

#[test]
fn parse_outcome_name_unknown_defaults_to_fatal() {
    assert!(matches!(parse_outcome_name("nonsense"), StepOutcome::Fatal));
    assert!(matches!(parse_outcome_name(""), StepOutcome::Fatal));
}

#[test]
fn contains_outcome_marker_line_matches_trimmed_line() {
    let stdout = "noise\n   MARKER_DONE   \nmore";
    assert!(contains_outcome_marker_line(stdout, "MARKER_DONE"));
}

#[test]
fn contains_outcome_marker_line_requires_full_line_match() {
    let stdout = "prefix MARKER suffix";
    assert!(!contains_outcome_marker_line(stdout, "MARKER"));
}

#[test]
fn match_exit_code_outcome_maps_configured_code() {
    let params = json!({"exit_code_map": {"2": "fixable", "3": "abandon"}});
    assert!(matches!(
        match_exit_code_outcome(&params, Some(2)),
        Some(StepOutcome::Fixable)
    ));
    assert!(matches!(
        match_exit_code_outcome(&params, Some(3)),
        Some(StepOutcome::Abandon)
    ));
}

#[test]
fn match_exit_code_outcome_none_when_unmapped_or_missing() {
    let params = json!({"exit_code_map": {"2": "fixable"}});
    assert!(match_exit_code_outcome(&params, Some(9)).is_none());
    assert!(match_exit_code_outcome(&params, None).is_none());
    assert!(match_exit_code_outcome(&json!({}), Some(2)).is_none());
}

#[test]
fn match_static_stdout_outcome_matches_marker_line() {
    let params = json!({"outcome_on_stdout": {"ALL_DONE": "success"}});
    let outcome = match_static_stdout_outcome(&params, "log\nALL_DONE\n");
    assert!(matches!(outcome, Some(StepOutcome::Success)));
}

#[test]
fn match_static_stdout_outcome_none_without_match() {
    let params = json!({"outcome_on_stdout": {"ALL_DONE": "success"}});
    assert!(match_static_stdout_outcome(&params, "nothing here").is_none());
    assert!(match_static_stdout_outcome(&json!({}), "ALL_DONE").is_none());
}

#[test]
fn match_stdout_outcome_reads_shared_buffer() {
    let params = json!({"outcome_on_stdout": {"READY": "retryable"}});
    let buffer = Arc::new(Mutex::new("prelude\nREADY\n".to_string()));
    let outcome = match_stdout_outcome(&params, &buffer);
    assert!(matches!(outcome, Some(StepOutcome::Retryable)));
}

#[test]
fn string_array_param_interpolates_and_defaults() {
    let mut context = StepContext::new(PathBuf::from("/tmp/work"), "run-1".to_string());
    context.set("name", "world");
    let params = json!({"args": ["hello-{name}", "static"]});
    let out = string_array_param(&params, "args", &context);
    assert_eq!(out, vec!["hello-world".to_string(), "static".to_string()]);
    // Missing key yields empty vec.
    assert!(string_array_param(&params, "missing", &context).is_empty());
}
