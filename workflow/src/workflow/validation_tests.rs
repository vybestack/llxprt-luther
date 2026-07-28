//! Graph validation tests.
//!
//! Split from `validation.rs`, which reached the 1000-line hard limit when
//! outcome-name validation was added. Behavior is unchanged; this is a
//! move.

use super::*;

use crate::workflow::schema::{StepDef, TransitionDef, WorkflowType};

use super::POST_PR_STEPS;

fn step(id: &str) -> StepDef {
    StepDef {
        step_id: id.to_string(),
        step_type: "shell".to_string(),
        description: None,
        parameters: None,
        produces: None,
        consumes: None,
        terminal: None,
        recovery_policy: None,
    }
}

fn transition(from: &str, to: &str, condition: Option<&str>) -> TransitionDef {
    TransitionDef {
        from: from.to_string(),
        to: to.to_string(),
        condition: condition.map(|c| c.to_string()),
        max_iterations: None,
    }
}

fn workflow(steps: Vec<StepDef>, transitions: Vec<TransitionDef>) -> WorkflowType {
    WorkflowType {
        workflow_type_id: "test".to_string(),
        steps,
        transitions,
        guards: Default::default(),
    }
}

/// A minimal well-formed (non post-PR) graph validates successfully.
#[test]
fn well_formed_graph_is_ok() {
    let wf = workflow(vec![step("a"), step("b")], vec![transition("a", "b", None)]);
    assert!(validate_workflow_graph(&wf).is_ok());
}

#[test]
fn dangling_from_is_flagged() {
    let wf = workflow(vec![step("a")], vec![transition("ghost", "a", None)]);
    let errors = validate_workflow_graph(&wf).unwrap_err();
    assert!(errors
        .iter()
        .any(|e| e.category == GraphErrorCategory::DanglingTransition
            && e.detail.contains("dangling transition source")
            // Naming the offending step is the point of the message: without
            // it an operator knows only that some source is dangling.
            && e.detail.contains("ghost")));
}

#[test]
fn dangling_to_is_flagged() {
    let wf = workflow(vec![step("a")], vec![transition("a", "ghost", None)]);
    let errors = validate_workflow_graph(&wf).unwrap_err();
    assert!(errors.iter().any(|e| {
        e.category == GraphErrorCategory::DanglingTransition
            && e.detail.contains("dangling transition target")
            && e.detail.contains("ghost")
    }));
}

#[test]
fn duplicate_success_outcome_is_flagged() {
    let wf = workflow(
        vec![step("a"), step("b"), step("c")],
        vec![transition("a", "b", None), transition("a", "c", None)],
    );
    let errors = validate_workflow_graph(&wf).unwrap_err();
    assert!(errors.iter().any(|e| {
        e.category == GraphErrorCategory::DuplicateOutcome
            && e.detail.contains("outcome success")
            && e.detail.contains("b")
            && e.detail.contains("c")
    }));
}

#[test]
fn duplicate_fatal_outcome_is_flagged() {
    let wf = workflow(
        vec![step("a"), step("b"), step("c")],
        vec![
            transition("a", "b", Some("fatal")),
            transition("a", "c", Some("fatal")),
        ],
    );
    let errors = validate_workflow_graph(&wf).unwrap_err();
    assert!(errors.iter().any(|e| {
        e.category == GraphErrorCategory::DuplicateOutcome
            && e.detail.contains("outcome fatal")
            // The duplicate is only actionable if the message says which
            // targets collide, as the sibling success-outcome test asserts.
            // Matching "b and c" rather than the bare letters: single-char
            // needles occur in almost any message and would assert nothing.
            && e.detail.contains("b and c")
    }));
}

#[test]
fn orphaned_non_terminal_step_is_flagged() {
    // `c` has an outgoing edge but is unreachable from entry `a`.
    let wf = workflow(
        vec![step("a"), step("b"), step("c"), step("d")],
        vec![transition("a", "b", None), transition("c", "d", None)],
    );
    let errors = validate_workflow_graph(&wf).unwrap_err();
    assert!(errors.iter().any(|e| {
        e.category == GraphErrorCategory::UnreachableStep && e.detail.contains("'c'")
    }));
}

#[test]
fn failure_cleanup_without_incoming_failure_route_is_rejected() {
    let mut cleanup = step("cleanup");
    cleanup.step_type = "failure_cleanup".to_string();
    cleanup.terminal = Some(true);
    let wf = workflow(vec![step("a"), cleanup], vec![]);
    let errors = validate_workflow_graph(&wf).unwrap_err();
    assert!(errors.iter().any(|error| {
        error.category == GraphErrorCategory::InvalidFailureCleanup
            && error
                .detail
                .contains("at least one incoming failure transition")
    }));
}

#[test]
fn an_isolated_step_is_not_flagged_as_unreachable() {
    // `isolated` has no edges at all and must not be flagged. Note this step is
    // NOT declared terminal - it is a plain step with no edges, which is the
    // case the unreachability logic has to tolerate. The declared-terminal
    // contract is covered separately below.
    let wf = workflow(
        vec![step("a"), step("b"), step("isolated")],
        vec![transition("a", "b", None)],
    );
    assert!(validate_workflow_graph(&wf).is_ok());
}

/// The same shape, but with the isolated step explicitly declared terminal.
///
/// The test above was previously named as though it covered this, so the
/// declared-terminal case had no coverage at all despite appearing to.
#[test]
fn an_isolated_declared_terminal_step_is_not_flagged() {
    let wf = workflow(
        vec![step("a"), step("b"), terminal_step("term")],
        vec![transition("a", "b", None)],
    );
    assert!(validate_workflow_graph(&wf).is_ok());
}

fn post_pr_steps_with(extra: Vec<StepDef>) -> Vec<StepDef> {
    let mut steps: Vec<StepDef> = POST_PR_STEPS.iter().map(|id| step(id)).collect();
    steps.push(step(PRE_PR_CLEANUP_TERMINAL));
    steps.extend(extra);
    steps
}

#[test]
fn post_pr_fatal_to_abandon_is_flagged() {
    let steps = post_pr_steps_with(vec![]);
    let transitions = vec![
        transition("capture_pr_identity", "watch_pr_checks", None),
        transition("watch_pr_checks", "collect_ci_failures", None),
        transition("collect_ci_failures", "collect_coderabbit_feedback", None),
        // Unsafe: post-PR fatal routed to the pre-PR cleanup terminal.
        transition("capture_pr_identity", "abandon_and_log", Some("fatal")),
    ];
    let wf = workflow(steps, transitions);
    let errors = validate_workflow_graph(&wf).unwrap_err();
    assert!(errors.iter().any(|e| {
        e.category == GraphErrorCategory::UnsafePostPrRoute
            && e.detail
                .contains("capture_pr_identity -> abandon_and_log is forbidden")
    }));
}

#[test]
fn post_pr_abandon_condition_is_flagged() {
    let steps = post_pr_steps_with(vec![]);
    let transitions = vec![
        transition("capture_pr_identity", "watch_pr_checks", None),
        transition("watch_pr_checks", "collect_ci_failures", None),
        transition("collect_ci_failures", "collect_coderabbit_feedback", None),
        transition(
            "capture_pr_identity",
            "post_pr_failure_terminal",
            Some("abandon"),
        ),
    ];
    let wf = workflow(steps, transitions);
    let errors = validate_workflow_graph(&wf).unwrap_err();
    assert!(errors.iter().any(|e| {
        e.category == GraphErrorCategory::UnsafePostPrRoute
            && e.detail.contains("uses abandon outcome")
    }));
}

#[test]
fn missing_required_collector_is_flagged() {
    // Build a post-PR graph that omits `collect_coderabbit_feedback`.
    let mut steps: Vec<StepDef> = POST_PR_STEPS
        .iter()
        .filter(|id| **id != "collect_coderabbit_feedback")
        .map(|id| step(id))
        .collect();
    steps.push(step(PRE_PR_CLEANUP_TERMINAL));
    let transitions = vec![
        transition("capture_pr_identity", "watch_pr_checks", None),
        transition("watch_pr_checks", "collect_ci_failures", None),
    ];
    let wf = workflow(steps, transitions);
    let errors = validate_workflow_graph(&wf).unwrap_err();
    assert!(errors.iter().any(|e| {
        e.category == GraphErrorCategory::MissingRequiredCollector
            && e.detail.contains("collect_coderabbit_feedback")
    }));
}

#[test]
fn unreachable_required_collector_is_flagged() {
    // Collector is declared but not reachable from `capture_pr_identity`.
    let steps = post_pr_steps_with(vec![]);
    let transitions = vec![
        transition("capture_pr_identity", "watch_pr_checks", None),
        transition("watch_pr_checks", "collect_ci_failures", None),
        // `collect_coderabbit_feedback` has only an outgoing edge from an
        // unrelated, unreachable source, so it is never reached.
        transition(
            "evaluate_coderabbit_feedback",
            "collect_coderabbit_feedback",
            None,
        ),
    ];
    let wf = workflow(steps, transitions);
    let errors = validate_workflow_graph(&wf).unwrap_err();
    assert!(errors.iter().any(|e| {
        e.category == GraphErrorCategory::MissingRequiredCollector
            && e.detail.contains("unreachable")
            && e.detail.contains("collect_coderabbit_feedback")
    }));
}

/// A loop-back transition with an explicit cap.
fn capped_loop_back(from: &str, to: &str, max: u32) -> TransitionDef {
    TransitionDef {
        from: from.to_string(),
        to: to.to_string(),
        condition: None,
        max_iterations: Some(max),
    }
}

fn terminal_step(id: &str) -> StepDef {
    StepDef {
        terminal: Some(true),
        ..step(id)
    }
}

fn guard_step(id: &str, params: Option<serde_json::Value>) -> StepDef {
    StepDef {
        step_type: "post_pr_iteration_guard".to_string(),
        parameters: params,
        ..step(id)
    }
}

#[test]
fn loop_back_without_max_iterations_is_flagged() {
    // `b -> a` is a backward edge (a precedes b) with no cap.
    let wf = workflow(
        vec![step("a"), step("b")],
        vec![transition("a", "b", None), transition("b", "a", None)],
    );
    let errors = validate_workflow_graph(&wf).unwrap_err();
    assert!(errors.iter().any(|e| {
        e.category == GraphErrorCategory::MissingLoopLimit
            && e.detail.contains("loop-back transition")
            && e.detail.contains("b --success--> a")
    }));
}

#[test]
fn loop_back_with_max_iterations_passes() {
    let wf = workflow(
        vec![step("a"), step("b")],
        vec![transition("a", "b", None), capped_loop_back("b", "a", 5)],
    );
    assert!(validate_workflow_graph(&wf).is_ok());
}

#[test]
fn forward_transition_without_max_iterations_passes() {
    // Only loop-backs require an explicit cap; forward edges do not.
    let wf = workflow(vec![step("a"), step("b")], vec![transition("a", "b", None)]);
    assert!(validate_workflow_graph(&wf).is_ok());
}

#[test]
fn terminal_step_with_outgoing_transition_is_flagged() {
    let wf = workflow(
        vec![step("a"), terminal_step("done")],
        vec![transition("a", "done", None), transition("done", "a", None)],
    );
    let errors = validate_workflow_graph(&wf).unwrap_err();
    assert!(errors.iter().any(|e| {
        e.category == GraphErrorCategory::TerminalHasOutgoing
            && e.detail.contains("terminal step 'done'")
    }));
}

#[test]
fn post_pr_failure_terminal_with_outgoing_transition_is_flagged() {
    // Implicit terminal recognized solely by step_type.
    let mut implicit = step("post_pr_failure_terminal");
    implicit.step_type = "post_pr_failure_terminal".to_string();
    let wf = workflow(
        vec![step("a"), implicit],
        vec![
            transition("a", "post_pr_failure_terminal", None),
            capped_loop_back("post_pr_failure_terminal", "a", 2),
        ],
    );
    let errors = validate_workflow_graph(&wf).unwrap_err();
    assert!(errors.iter().any(|e| {
        e.category == GraphErrorCategory::TerminalHasOutgoing
            && e.detail.contains("post_pr_failure_terminal")
    }));
}

#[test]
fn terminal_step_without_outgoing_transition_passes() {
    let wf = workflow(
        vec![step("a"), terminal_step("done")],
        vec![transition("a", "done", None)],
    );
    assert!(validate_workflow_graph(&wf).is_ok());
}

#[test]
fn non_terminal_step_with_outgoing_transition_passes() {
    let wf = workflow(
        vec![step("a"), step("b"), step("c")],
        vec![transition("a", "b", None), transition("b", "c", None)],
    );
    assert!(validate_workflow_graph(&wf).is_ok());
}

#[test]
fn iteration_guard_missing_cap_is_flagged() {
    let wf = workflow(
        vec![step("a"), guard_step("guard", None)],
        vec![transition("a", "guard", None)],
    );
    let errors = validate_workflow_graph(&wf).unwrap_err();
    assert!(errors.iter().any(|e| {
        e.category == GraphErrorCategory::MissingRemediationCap
            && e.detail.contains("post_pr_iteration_guard step 'guard'")
    }));
}

#[test]
fn iteration_guard_zero_cap_is_flagged() {
    let params = serde_json::json!({ "max_post_pr_remediation_iterations": 0 });
    let wf = workflow(
        vec![step("a"), guard_step("guard", Some(params))],
        vec![transition("a", "guard", None)],
    );
    let errors = validate_workflow_graph(&wf).unwrap_err();
    assert!(errors.iter().any(|e| {
        e.category == GraphErrorCategory::MissingRemediationCap
            && e.detail.contains("must declare a positive")
    }));
}

#[test]
fn iteration_guard_positive_cap_passes() {
    let params = serde_json::json!({ "max_post_pr_remediation_iterations": 3 });
    let wf = workflow(
        vec![step("a"), guard_step("guard", Some(params))],
        vec![transition("a", "guard", None)],
    );
    assert!(validate_workflow_graph(&wf).is_ok());
}

// Reuses the existing helpers so this module does not restate the shape of
// StepDef/WorkflowType, which have fields these tests do not care about.
fn step_with_params(id: &str, params: serde_json::Value) -> StepDef {
    StepDef {
        parameters: Some(params),
        ..step(id)
    }
}

/// A misspelled outcome name is rejected at load rather than defaulting.
///
/// Before this check the two executors disagreed about what an unknown
/// name meant - shell routed it to Success and llxprt to Fatal - so this
/// exact typo passed a run under one and failed it under the other, with
/// nothing reported. Neither default is reachable now.
#[test]
fn a_misspelled_outcome_name_is_rejected() {
    let wf = workflow(
        vec![step_with_params(
            "build",
            serde_json::json!({"exit_code_map": {"2": "fixxable"}}),
        )],
        Vec::new(),
    );

    let errors = validate_workflow_graph(&wf).expect_err("the typo must be rejected");
    let unknown: Vec<_> = errors
        .iter()
        .filter(|e| e.category == GraphErrorCategory::UnknownOutcomeName)
        .collect();
    assert_eq!(unknown.len(), 1, "expected exactly one unknown-name error");
    // The message must name the step and the offending value, or it sends
    // the reader hunting through the file for it.
    assert!(
        unknown[0].detail.contains("build") && unknown[0].detail.contains("fixxable"),
        "error must name the step and the bad value: {}",
        unknown[0].detail
    );
}

/// Case is not silently accepted either.
///
/// "Fixable" previously parsed in the shell executor, which lowercased,
/// and fell through to Fatal in llxprt, which did not.
#[test]
fn a_differently_cased_outcome_name_is_rejected() {
    let wf = workflow(
        vec![step_with_params(
            "build",
            serde_json::json!({"outcome_on_stdout": {"READY": "Fixable"}}),
        )],
        Vec::new(),
    );
    let errors = validate_workflow_graph(&wf).expect_err("case variants must be rejected");
    assert!(errors
        .iter()
        .any(|e| e.category == GraphErrorCategory::UnknownOutcomeName));
}

#[test]
fn correctly_spelled_outcome_names_pass() {
    let wf = workflow(
        vec![step_with_params(
            "build",
            serde_json::json!({
                "exit_code_map": {"2": "fixable", "3": "abandon"},
                "outcome_on_stdout": {"READY": "retryable"}
            }),
        )],
        Vec::new(),
    );
    let unknown = validate_workflow_graph(&wf)
        .err()
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.category == GraphErrorCategory::UnknownOutcomeName)
        .count();
    assert_eq!(unknown, 0, "valid names must not be flagged");
}

/// Every shipped workflow parameter whose values are outcome names is covered
/// by `OUTCOME_VALUED_PARAMS`.
///
/// The list is hand-maintained, so a new outcome-bearing parameter would
/// silently bypass validation and reach an executor's runtime default. This
/// scans the shipped workflows for any parameter whose values look like
/// outcome names and asserts the list already knows about it.
#[test]
fn no_shipped_parameter_carries_outcome_names_unvalidated() {
    // The validator's own list, not a copy of it. A second copy could be
    // updated to silence this test while leaving the validator unchanged,
    // which is the drift this test exists to catch.
    let known = super::OUTCOME_VALUED_PARAMS;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("config/workflows");
    let mut unknown: Vec<String> = Vec::new();
    let mut scanned = 0usize;

    for entry in std::fs::read_dir(&root).expect("the shipped workflow directory exists") {
        let path = entry.expect("a readable dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("a readable workflow");
        let parsed: toml::Value = toml::from_str(&text).expect("a parseable workflow");
        scanned += 1;

        let Some(steps) = parsed.get("steps").and_then(|s| s.as_array()) else {
            continue;
        };
        for step in steps {
            let Some(params) = step.get("parameters").and_then(|p| p.as_table()) else {
                continue;
            };
            for (name, value) in params {
                if known.contains(&name.as_str()) {
                    continue;
                }
                // A parameter carries outcome names if it is a table whose
                // values are all strings that parse as outcomes.
                let Some(table) = value.as_table() else {
                    continue;
                };
                if table.is_empty() {
                    continue;
                }
                // ANY outcome-valued entry makes the parameter suspect, not
                // every entry. Requiring all of them let a mixed table - one
                // outcome name beside an unrelated string - slip past the
                // check entirely, which is the shape a real parameter is most
                // likely to have.
                let carries_outcomes = table.values().any(|v| {
                    v.as_str()
                        .is_some_and(|s| StepOutcome::parse_condition_str(s).is_some())
                });
                if carries_outcomes {
                    unknown.push(format!("{}: {name}", path.display()));
                }
            }
        }
    }

    assert!(
        scanned > 0,
        "no workflows scanned; this would pass vacuously"
    );
    assert!(
        unknown.is_empty(),
        "these parameters carry outcome names but are not in OUTCOME_VALUED_PARAMS, \
         so their values bypass validation and reach executor defaults:\n  {}",
        unknown.join("\n  ")
    );
}

/// A non-string value in an outcome map is rejected, not ignored.
///
/// Skipping it would let `"2" = 3` reach the executor, which is the silent
/// path this validation exists to close.
#[test]
fn a_non_string_outcome_value_is_rejected() {
    for bad in [
        serde_json::json!({"exit_code_map": {"2": 3}}),
        serde_json::json!({"exit_code_map": {"2": {"nested": "fixable"}}}),
        serde_json::json!({"outcome_on_stdout": {"READY": true}}),
    ] {
        let wf = workflow(vec![step_with_params("build", bad.clone())], Vec::new());
        let errors =
            validate_workflow_graph(&wf).expect_err("a non-string outcome value must be rejected");
        assert!(
            errors
                .iter()
                .any(|e| e.category == GraphErrorCategory::UnknownOutcomeName),
            "expected an unknown-name error for {bad}"
        );
    }
}

/// `POST_PR_STEPS` match the post-PR steps the shipped workflow declares.
///
/// The list is duplicated here and in `tests/e2e_workflow_integration.rs`, so a
/// post-PR step added to the workflow but not to the list would leave both
/// copies asserting against a graph that no longer exists - passing while
/// covering nothing. Anchoring to the shipped file makes that divergence fail
/// here instead of going unnoticed.
#[test]
fn post_pr_steps_matches_the_shipped_workflow() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("config/workflows/llxprt-issue-fix-v1.toml");
    let parsed: toml::Value =
        toml::from_str(&std::fs::read_to_string(&path).expect("the shipped workflow is readable"))
            .expect("the shipped workflow parses");

    let declared: std::collections::BTreeSet<String> = parsed
        .get("steps")
        .and_then(|s| s.as_array())
        .expect("the workflow declares steps")
        .iter()
        .filter_map(|s| s.get("step_id").and_then(|v| v.as_str()))
        .map(str::to_string)
        .collect();

    assert!(
        !declared.is_empty(),
        "no steps parsed; this would pass vacuously"
    );

    let missing: Vec<&str> = POST_PR_STEPS
        .iter()
        .copied()
        .filter(|id| !declared.contains(*id))
        .collect();
    assert!(
        missing.is_empty(),
        "POST_PR_STEPS names steps the shipped workflow no longer declares: {missing:?}\n\
         Update both this list and the copy in tests/e2e_workflow_integration.rs."
    );

    // The reverse direction, which is the dangerous one. A post-PR step added
    // to the workflow but not to this list is not merely uncovered: the post-PR
    // validators (unsafe-route rejection, collector reachability, iteration cap)
    // would not treat it as post-PR at all, so those checks would silently skip
    // it while this test still passed.
    //
    // "Post-PR" is defined by reachability from POST_PR_ENTRY, not by a name
    // convention, so the set is computed the same way validation computes it
    // rather than guessed at here.
    let workflow: WorkflowType =
        toml::from_str(&std::fs::read_to_string(&path).expect("the shipped workflow is readable"))
            .expect("the shipped workflow deserialises");
    let reachable = compute_reachable_steps(&workflow, "capture_pr_identity");
    assert!(
        !reachable.is_empty(),
        "no post-PR steps reachable; this half would pass vacuously"
    );

    // `log_completion` is reachable from the post-PR entry but is deliberately
    // not in POST_PR_STEPS, because it does not satisfy the post-PR step
    // contract: it declares no `artifact_root`, has no outgoing transitions,
    // and is not marked terminal. Adding it makes two e2e contract tests fail
    // for real reasons rather than spuriously.
    //
    // That is a defect in the shipped workflow, not in this list, and fixing
    // the workflow is a routing change that does not belong in a parser
    // change. Tracked as #280; this exception keeps the reverse check
    // useful in the meantime instead of deleting it.
    const KNOWN_UNCONTRACTED: [&str; 1] = ["log_completion"];

    let unlisted: Vec<&str> = reachable
        .iter()
        .map(String::as_str)
        .filter(|id| !POST_PR_STEPS.contains(id))
        .filter(|id| !KNOWN_UNCONTRACTED.contains(id))
        .collect();
    assert!(
        unlisted.is_empty(),
        "the shipped workflow has post-PR steps missing from POST_PR_STEPS: {unlisted:?}\n\
         Post-PR validation would skip these entirely. Add them here and to the copy in \
         tests/e2e_workflow_integration.rs."
    );
}

/// A rejected container value is described, not reproduced.
///
/// Details are joined with `; ` into one line, so a deeply nested object
/// printed in full would bury every other error from the same load.
#[test]
fn a_rejected_container_value_is_summarized_not_dumped() {
    let wf = workflow(
        vec![step_with_params(
            "build",
            serde_json::json!({"exit_code_map": {"2": {"deeply": {"nested": ["a", "b", "c"]}}}}),
        )],
        Vec::new(),
    );
    let errors = validate_workflow_graph(&wf).expect_err("a table value must be rejected");
    let detail = &errors
        .iter()
        .find(|e| e.category == GraphErrorCategory::UnknownOutcomeName)
        .expect("an unknown-name error")
        .detail;

    // Assert the contract - the contents are not reproduced - rather than the
    // exact wording, which describe_value is free to improve.
    // `[` cannot be used as a proxy for "contains JSON": the message itself
    // renders the key as exit_code_map['2'].
    assert!(
        !detail.contains("nested") && !detail.contains("deeply"),
        "the value's contents must not be reproduced into the message: {detail}"
    );
    assert!(
        detail.contains("table"),
        "the value should still be identified by shape: {detail}"
    );
    // The step and key still have to be there, or the author cannot find it.
    assert!(detail.contains("build") && detail.contains("exit_code_map"));
}

/// A parameter of the wrong shape is rejected, not skipped.
///
/// The executor calls `.as_object()` too, so a string or array here reaches
/// runtime and silently maps nothing - the same fail-open behavior that
/// unknown outcome names used to have.
#[test]
fn an_outcome_parameter_that_is_not_a_table_is_rejected() {
    for bad in [
        serde_json::json!({"exit_code_map": "fixable"}),
        serde_json::json!({"outcome_on_stdout": ["fixable"]}),
    ] {
        let wf = workflow(vec![step_with_params("build", bad.clone())], Vec::new());
        let errors = validate_workflow_graph(&wf)
            .expect_err(&format!("{bad} must be rejected at validation time"));
        assert!(
            errors
                .iter()
                .any(|e| e.category == GraphErrorCategory::UnknownOutcomeName
                    && e.detail.contains("must be a table")),
            "{bad} should be rejected for its shape, got: {errors:?}"
        );
    }
}

/// Singular counts read correctly.
#[test]
fn a_single_entry_is_described_in_the_singular() {
    let wf = workflow(
        vec![step_with_params(
            "build",
            serde_json::json!({"exit_code_map": {"2": {"a": 1}}}),
        )],
        Vec::new(),
    );
    let errors = validate_workflow_graph(&wf).expect_err("a table value must be rejected");
    let detail = &errors
        .iter()
        .find(|e| e.category == GraphErrorCategory::UnknownOutcomeName)
        .expect("an unknown-name error")
        .detail;
    assert!(
        detail.contains("1 entry") && !detail.contains("1 entries"),
        "a count of one must not read as a plural: {detail}"
    );
}

/// A name that differs only in case names its canonical spelling.
///
/// Outcome names matched case-insensitively before this change, so "Fatal"
/// loaded and ran. Rejecting it is correct, but the message must say why or it
/// reads as an unrecognised name rather than a capitalised one.
#[test]
fn a_miscased_outcome_name_is_told_the_canonical_spelling() {
    let mut wf = workflow(vec![step("a")], vec![]);
    wf.steps[0].parameters = Some(serde_json::json!({"exit_code_map": {"2": "Fatal"}}));
    let errors = validate_workflow_graph(&wf).unwrap_err();
    let detail = errors
        .iter()
        .find(|e| e.category == GraphErrorCategory::UnknownOutcomeName)
        .map(|e| e.detail.clone())
        .expect("a miscased name must be rejected");
    assert!(
        detail.contains("outcome names are lowercase: write fatal"),
        "message must name the canonical spelling, got: {detail}"
    );
}

/// A genuinely unknown name gets no case hint, which would be misleading.
#[test]
fn an_unknown_outcome_name_gets_no_case_hint() {
    let mut wf = workflow(vec![step("a")], vec![]);
    wf.steps[0].parameters = Some(serde_json::json!({"exit_code_map": {"2": "explode"}}));
    let errors = validate_workflow_graph(&wf).unwrap_err();
    let detail = errors
        .iter()
        .find(|e| e.category == GraphErrorCategory::UnknownOutcomeName)
        .map(|e| e.detail.clone())
        .expect("an unknown name must be rejected");
    assert!(
        !detail.contains("lowercase"),
        "no case hint for a name that is not a miscased outcome, got: {detail}"
    );
}
