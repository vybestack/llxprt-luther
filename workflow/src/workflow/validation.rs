/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
/// Graph-structural validation for workflow types.
///
/// This module centralizes all graph-level safety checks that go beyond the
/// shallow field validation performed in `config_loader::validate_workflow_type`.
/// It rejects invalid or unsafe routing before the engine is ever constructed:
///
/// - Dangling transition targets (`from`/`to` referencing unknown steps).
/// - Duplicate outcome branches from a single step (ambiguous routing).
/// - Unreachable required steps.
/// - Direct fatal/retryable routes in the PR follow-up portion of the graph
///   that bypass the required collector steps and the post-PR failure terminal.
use std::collections::{HashMap, HashSet};

use crate::workflow::schema::WorkflowType;

/// Entry point of the post-PR portion of the graph.
const POST_PR_ENTRY: &str = "capture_pr_identity";

/// The pre-PR cleanup terminal that post-PR routes must never target.
const PRE_PR_CLEANUP_TERMINAL: &str = "abandon_and_log";

/// The post-PR failure terminal that fatal/retryable post-PR routes must target.
const POST_PR_FAILURE_TERMINAL: &str = "post_pr_failure_terminal";

/// Required collector steps that must exist and be reachable in the post-PR graph.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
const REQUIRED_COLLECTORS: [&str; 2] = ["collect_ci_failures", "collect_coderabbit_feedback"];

/// Classification of graph-structural validation errors.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphErrorCategory {
    /// A transition references a step ID that does not exist.
    DanglingTransition,
    /// A single step declares two transitions for the same effective outcome.
    DuplicateOutcome,
    /// A required/non-terminal step is unreachable from the entry point.
    UnreachableStep,
    /// A post-PR route is unsafe (e.g. bypasses required failure handling).
    UnsafePostPrRoute,
    /// A required collector step is missing or unreachable in the post-PR graph.
    MissingRequiredCollector,
}

/// A single graph-structural validation error.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphValidationError {
    /// The step the error is associated with, when applicable.
    pub step_id: Option<String>,
    /// Human-readable detail of the error. Contains stable, greppable substrings.
    pub detail: String,
    /// Category of the error.
    pub category: GraphErrorCategory,
}

impl std::fmt::Display for GraphValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.detail)
    }
}

/// The effective condition of a transition (`None` defaults to `success`).
fn effective_condition(condition: Option<&str>) -> &str {
    condition.unwrap_or("success")
}

/// Validate the full workflow graph, aggregating every error found.
///
/// Returns `Ok(())` if the graph is well-formed and safe, otherwise returns a
/// non-empty `Vec` of every detected error.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
pub fn validate_workflow_graph(workflow: &WorkflowType) -> Result<(), Vec<GraphValidationError>> {
    let mut errors = Vec::new();

    validate_transitions_reference_valid_steps(workflow, &mut errors);
    validate_no_duplicate_outcomes(workflow, &mut errors);
    validate_all_steps_reachable(workflow, &mut errors);
    validate_post_pr_routes(workflow, &mut errors);
    validate_required_collectors_present_and_reachable(workflow, &mut errors);

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Build the set of declared step IDs.
fn step_id_set(workflow: &WorkflowType) -> HashSet<&str> {
    workflow
        .steps
        .iter()
        .map(|step| step.step_id.as_str())
        .collect()
}

/// Flag any transition whose `from`/`to` references a step that does not exist.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
fn validate_transitions_reference_valid_steps(
    workflow: &WorkflowType,
    errors: &mut Vec<GraphValidationError>,
) {
    let steps = step_id_set(workflow);
    for transition in &workflow.transitions {
        if !steps.contains(transition.from.as_str()) {
            errors.push(GraphValidationError {
                step_id: Some(transition.from.clone()),
                detail: format!(
                    "dangling transition source: step '{}' referenced by a transition does not exist",
                    transition.from
                ),
                category: GraphErrorCategory::DanglingTransition,
            });
        }
        if !steps.contains(transition.to.as_str()) {
            errors.push(GraphValidationError {
                step_id: Some(transition.from.clone()),
                detail: format!(
                    "dangling transition target: step '{}' referenced by transition from '{}' does not exist",
                    transition.to, transition.from
                ),
                category: GraphErrorCategory::DanglingTransition,
            });
        }
    }
}

/// Flag two transitions from the same step that share an effective outcome.
///
/// Mirrors the semantics of `post_pr_duplicate_transition_errors` in the e2e
/// test helpers so the existing expected substrings continue to match.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
fn validate_no_duplicate_outcomes(workflow: &WorkflowType, errors: &mut Vec<GraphValidationError>) {
    let mut seen: HashMap<(String, String), String> = HashMap::new();
    for transition in &workflow.transitions {
        let condition = effective_condition(transition.condition.as_deref()).to_string();
        let key = (transition.from.clone(), condition.clone());
        match seen.get(&key) {
            // A duplicate that routes to the *same* target is redundant but not
            // ambiguous — routing stays deterministic — so it is not an error.
            Some(previous) if previous == &transition.to => {}
            // A duplicate that routes to a *different* target makes the outcome
            // ambiguous and is rejected.
            Some(previous) => {
                errors.push(GraphValidationError {
                    step_id: Some(transition.from.clone()),
                    detail: format!(
                        "duplicate post-PR transition branch for {} outcome {}: {} and {}",
                        key.0, condition, previous, transition.to
                    ),
                    category: GraphErrorCategory::DuplicateOutcome,
                });
            }
            None => {
                seen.insert(key, transition.to.clone());
            }
        }
    }
}

/// Compute the set of steps reachable from `start` via outgoing transitions.
///
/// Port of the `reachable_steps` test helper.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
pub fn compute_reachable_steps(workflow: &WorkflowType, start: &str) -> HashSet<String> {
    let mut stack = vec![start.to_string()];
    let mut seen = HashSet::new();
    while let Some(step) = stack.pop() {
        if !seen.insert(step.clone()) {
            continue;
        }
        for transition in workflow
            .transitions
            .iter()
            .filter(|transition| transition.from == step)
        {
            stack.push(transition.to.clone());
        }
    }
    seen
}

/// Flag steps that are unreachable from the first declared step.
///
/// Pure terminal steps that have neither incoming nor outgoing edges (e.g. a
/// standalone cleanup terminal like `abandon_and_log`) are exempt, because they
/// are intentionally entered only via explicit fatal routes that may not exist
/// in every minimal graph. Any other orphaned step is flagged.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
fn validate_all_steps_reachable(workflow: &WorkflowType, errors: &mut Vec<GraphValidationError>) {
    let Some(entry) = workflow.steps.first() else {
        return;
    };
    let reachable = compute_reachable_steps(workflow, &entry.step_id);

    let mut has_incoming: HashSet<&str> = HashSet::new();
    let mut has_outgoing: HashSet<&str> = HashSet::new();
    for transition in &workflow.transitions {
        has_outgoing.insert(transition.from.as_str());
        has_incoming.insert(transition.to.as_str());
    }

    for step in &workflow.steps {
        let id = step.step_id.as_str();
        if reachable.contains(id) {
            continue;
        }
        // Exempt fully-disconnected terminal steps (no edges at all): these are
        // entered only through explicit routes elsewhere and are not "required".
        let is_isolated_terminal = !has_incoming.contains(id) && !has_outgoing.contains(id);
        if is_isolated_terminal {
            continue;
        }
        errors.push(GraphValidationError {
            step_id: Some(step.step_id.clone()),
            detail: format!(
                "unreachable required step: '{}' cannot be reached from entry step '{}'",
                step.step_id, entry.step_id
            ),
            category: GraphErrorCategory::UnreachableStep,
        });
    }
}

/// Compute the set of post-PR steps reachable from the post-PR entry step.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
fn compute_post_pr_reachable_steps(workflow: &WorkflowType) -> HashSet<String> {
    compute_reachable_steps(workflow, POST_PR_ENTRY)
}

/// Whether this graph contains a PR follow-up (post-PR) section. True if the
/// post-PR entry step is declared, or if any transition references it. This is
/// intentionally broad so that graphs which route into the post-PR entry are
/// still subject to post-PR safety rules even if a step declaration is missing.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
fn is_post_pr_graph(workflow: &WorkflowType) -> bool {
    workflow
        .steps
        .iter()
        .any(|step| step.step_id == POST_PR_ENTRY)
        || workflow
            .transitions
            .iter()
            .any(|transition| transition.from == POST_PR_ENTRY || transition.to == POST_PR_ENTRY)
}

/// Reject unsafe routing in the PR follow-up portion of the graph.
///
/// Port of `post_pr_forbidden_route_errors` in the e2e test helpers. The same
/// message substrings are preserved so existing expectations keep matching.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
fn validate_post_pr_routes(workflow: &WorkflowType, errors: &mut Vec<GraphValidationError>) {
    // Only enforce post-PR rules if this graph contains a post-PR section;
    // pre-PR-only graphs are unaffected.
    if !is_post_pr_graph(workflow) {
        return;
    }

    let reachable = compute_post_pr_reachable_steps(workflow);
    for transition in workflow
        .transitions
        .iter()
        .filter(|transition| reachable.contains(&transition.from))
    {
        if transition.to == PRE_PR_CLEANUP_TERMINAL {
            errors.push(GraphValidationError {
                step_id: Some(transition.from.clone()),
                detail: format!(
                    "post-PR route {} -> abandon_and_log is forbidden",
                    transition.from
                ),
                category: GraphErrorCategory::UnsafePostPrRoute,
            });
        }
        if transition.condition.as_deref() == Some("abandon") {
            errors.push(GraphValidationError {
                step_id: Some(transition.from.clone()),
                detail: format!("post-PR route {} uses abandon outcome", transition.from),
                category: GraphErrorCategory::UnsafePostPrRoute,
            });
        }
        if transition
            .condition
            .as_deref()
            .is_some_and(|condition| condition == "fatal" || condition == "retryable")
            && transition.to != POST_PR_FAILURE_TERMINAL
            && transition.from != "watch_pr_checks"
        {
            errors.push(GraphValidationError {
                step_id: Some(transition.from.clone()),
                detail: format!(
                    "post-PR non-success route {} --{}--> {} must target post_pr_failure_terminal",
                    transition.from,
                    transition.condition.as_deref().unwrap_or("success"),
                    transition.to
                ),
                category: GraphErrorCategory::UnsafePostPrRoute,
            });
        }
    }
}

/// Ensure each required collector exists and is reachable in the post-PR graph.
///
/// This directly satisfies the "direct fatal routes that bypass required
/// collectors" acceptance criterion: if a collector cannot be reached from the
/// post-PR entry, the route bypasses it and the graph is rejected.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
fn validate_required_collectors_present_and_reachable(
    workflow: &WorkflowType,
    errors: &mut Vec<GraphValidationError>,
) {
    // Only enforce when this graph is a post-PR graph.
    if !is_post_pr_graph(workflow) {
        return;
    }

    let declared = step_id_set(workflow);
    let reachable = compute_post_pr_reachable_steps(workflow);

    for collector in REQUIRED_COLLECTORS {
        if !declared.contains(collector) {
            errors.push(GraphValidationError {
                step_id: Some(collector.to_string()),
                detail: format!(
                    "required collector step '{}' is missing from the post-PR graph",
                    collector
                ),
                category: GraphErrorCategory::MissingRequiredCollector,
            });
        } else if !reachable.contains(collector) {
            errors.push(GraphValidationError {
                step_id: Some(collector.to_string()),
                detail: format!(
                    "required collector step '{}' is unreachable from post-PR entry '{}'",
                    collector, POST_PR_ENTRY
                ),
                category: GraphErrorCategory::MissingRequiredCollector,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::schema::{StepDef, TransitionDef, WorkflowType};

    /// The canonical set of post-PR (PR follow-up) step IDs. Kept in sync with
    /// the `POST_PR_STEPS` constant in `tests/e2e_workflow_integration.rs`.
    const POST_PR_STEPS: [&str; 13] = [
        "capture_pr_identity",
        "post_pr_iteration_guard",
        "watch_pr_checks",
        "collect_ci_failures",
        "collect_coderabbit_feedback",
        "evaluate_coderabbit_feedback",
        "build_remediation_plan",
        "remediate_pr_followup",
        "validate_remediation_result",
        "run_post_pr_tests",
        "push_remediation_changes",
        "mark_coderabbit_feedback",
        "post_pr_failure_terminal",
    ];

    fn step(id: &str) -> StepDef {
        StepDef {
            step_id: id.to_string(),
            step_type: "shell".to_string(),
            description: None,
            parameters: None,
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
                && e.detail.contains("dangling transition source")));
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
            e.category == GraphErrorCategory::DuplicateOutcome && e.detail.contains("outcome fatal")
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
    fn isolated_terminal_step_is_not_flagged() {
        // `term` has no edges at all and must not be flagged as unreachable.
        let wf = workflow(
            vec![step("a"), step("b"), step("term")],
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
}
