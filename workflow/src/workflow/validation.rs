/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// Graph-structural validation for workflow types.
///
/// This module promotes the workflow-graph safety checks (previously living only
/// inside the integration tests) into production code so that invalid or unsafe
/// routing is rejected on the load path, before any engine is constructed.
///
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
use std::collections::{HashMap, HashSet};

use crate::workflow::schema::WorkflowType;

/// Entry step that anchors the post-PR follow-up sub-graph. The post-PR safety
/// validators only fire when the workflow contains this step, so generic
/// workflows are unaffected.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
const POST_PR_ENTRY: &str = "capture_pr_identity";

/// Required collectors that every post-PR fatal/retryable route must not bypass.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
const REQUIRED_COLLECTORS: [&str; 2] = ["collect_ci_failures", "collect_coderabbit_feedback"];

/// Classification of graph validation failures.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphErrorKind {
    /// A transition references a `from`/`to` step that is not in the step set.
    DanglingTarget,
    /// Two branches share the same `(from, effective_condition)` pair.
    DuplicateOutcome,
    /// A required step is unreachable (a true orphan with no incoming/outgoing edges).
    UnreachableStep,
    /// A post-PR route is unsafe (e.g. routes to `abandon_and_log` or bypasses the
    /// post-PR failure terminal).
    UnsafePostPrRoute,
    /// A required post-PR collector is missing or unreachable from the post-PR entry.
    MissingRequiredCollector,
}

/// A single graph validation error.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphValidationError {
    pub category: GraphErrorKind,
    pub step_id: Option<String>,
    pub message: String,
}

impl std::fmt::Display for GraphValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// Effective condition for a transition: `None` is treated as `"success"`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
fn effective_condition(condition: Option<&str>) -> &str {
    condition.unwrap_or("success")
}

/// Compute the set of steps reachable from `start` via forward transitions.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
pub fn compute_reachable_steps(workflow_type: &WorkflowType, start: &str) -> HashSet<String> {
    let mut stack = vec![start.to_string()];
    let mut seen = HashSet::new();
    while let Some(step) = stack.pop() {
        if !seen.insert(step.clone()) {
            continue;
        }
        for transition in workflow_type
            .transitions
            .iter()
            .filter(|transition| transition.from == step)
        {
            stack.push(transition.to.clone());
        }
    }
    seen
}

/// Reject transitions whose `from` or `to` is not a defined step.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
fn validate_transitions_reference_valid_steps(
    workflow_type: &WorkflowType,
) -> Vec<GraphValidationError> {
    let step_ids: HashSet<&str> = workflow_type
        .steps
        .iter()
        .map(|step| step.step_id.as_str())
        .collect();
    let mut errors = Vec::new();
    for transition in &workflow_type.transitions {
        if !step_ids.contains(transition.from.as_str()) {
            errors.push(GraphValidationError {
                category: GraphErrorKind::DanglingTarget,
                step_id: Some(transition.from.clone()),
                message: format!(
                    "transition source '{}' is not a defined step (target '{}')",
                    transition.from, transition.to
                ),
            });
        }
        if !step_ids.contains(transition.to.as_str()) {
            errors.push(GraphValidationError {
                category: GraphErrorKind::DanglingTarget,
                step_id: Some(transition.to.clone()),
                message: format!(
                    "transition target '{}' is not a defined step (source '{}')",
                    transition.to, transition.from
                ),
            });
        }
    }
    errors
}

/// Reject ambiguous outcome branches: two transitions sharing the same `from`
/// step and the same effective condition but routing to *different* targets.
/// Such a pair makes routing non-deterministic. Identical edges (same target)
/// are harmless redundancy and are not flagged.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
fn validate_no_duplicate_outcomes(workflow_type: &WorkflowType) -> Vec<GraphValidationError> {
    let mut seen: HashMap<(String, String), String> = HashMap::new();
    let mut errors = Vec::new();
    for transition in &workflow_type.transitions {
        let key = (
            transition.from.clone(),
            effective_condition(transition.condition.as_deref()).to_string(),
        );
        if let Some(previous) = seen.insert(key.clone(), transition.to.clone()) {
            if previous != transition.to {
                errors.push(GraphValidationError {
                    category: GraphErrorKind::DuplicateOutcome,
                    step_id: Some(key.0.clone()),
                    message: format!(
                        "duplicate transition branch for {} outcome {}: {} and {}",
                        key.0, key.1, previous, transition.to
                    ),
                });
            }
        }
    }
    errors
}

/// Reject unreachable steps: a step that is reachable from neither the entry
/// step nor any incoming transition (a genuine orphan). Legitimately terminal
/// steps that are referenced as a transition target are treated as reachable.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
fn validate_all_steps_reachable(workflow_type: &WorkflowType) -> Vec<GraphValidationError> {
    let mut errors = Vec::new();
    let Some(entry) = workflow_type.steps.first() else {
        return errors;
    };

    let reachable = compute_reachable_steps(workflow_type, &entry.step_id);

    // Steps that are referenced by any transition (as source or target).
    let mut referenced: HashSet<&str> = HashSet::new();
    for transition in &workflow_type.transitions {
        referenced.insert(transition.from.as_str());
        referenced.insert(transition.to.as_str());
    }

    for step in &workflow_type.steps {
        let id = step.step_id.as_str();
        if reachable.contains(id) || referenced.contains(id) {
            continue;
        }
        errors.push(GraphValidationError {
            category: GraphErrorKind::UnreachableStep,
            step_id: Some(step.step_id.clone()),
            message: format!(
                "step '{}' is unreachable: no incoming or outgoing transitions",
                step.step_id
            ),
        });
    }
    errors
}

/// Reject unsafe post-PR routes. This is the production promotion of the
/// integration-test `post_pr_forbidden_route_errors` helper.
///
/// For every transition whose `from` is reachable from `capture_pr_identity`:
/// - reject `to == "abandon_and_log"`,
/// - reject `condition == Some("abandon")`,
/// - require `fatal`/`retryable` outcomes to target `post_pr_failure_terminal`
///   (exception: `from == "watch_pr_checks"`).
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
fn validate_post_pr_routes(workflow_type: &WorkflowType) -> Vec<GraphValidationError> {
    let reachable = compute_reachable_steps(workflow_type, POST_PR_ENTRY);
    let mut errors = Vec::new();
    for transition in workflow_type
        .transitions
        .iter()
        .filter(|transition| reachable.contains(&transition.from))
    {
        if transition.to == "abandon_and_log" {
            errors.push(GraphValidationError {
                category: GraphErrorKind::UnsafePostPrRoute,
                step_id: Some(transition.from.clone()),
                message: format!(
                    "post-PR route {} -> abandon_and_log is forbidden",
                    transition.from
                ),
            });
        }
        if transition.condition.as_deref() == Some("abandon") {
            errors.push(GraphValidationError {
                category: GraphErrorKind::UnsafePostPrRoute,
                step_id: Some(transition.from.clone()),
                message: format!("post-PR route {} uses abandon outcome", transition.from),
            });
        }
        if transition
            .condition
            .as_deref()
            .is_some_and(|condition| condition == "fatal" || condition == "retryable")
            && transition.to != "post_pr_failure_terminal"
            && transition.from != "watch_pr_checks"
        {
            errors.push(GraphValidationError {
                category: GraphErrorKind::UnsafePostPrRoute,
                step_id: Some(transition.from.clone()),
                message: format!(
                    "post-PR non-success route {} --{}--> {} must target post_pr_failure_terminal",
                    transition.from,
                    transition.condition.as_deref().unwrap_or("success"),
                    transition.to
                ),
            });
        }
    }
    errors
}

/// Reject post-PR graphs that bypass required collectors. Each required collector
/// must exist as a step and be reachable from the post-PR entry, guarding against
/// direct fatal routes that skip the collectors.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
fn validate_required_collectors_reachable(
    workflow_type: &WorkflowType,
) -> Vec<GraphValidationError> {
    let reachable = compute_reachable_steps(workflow_type, POST_PR_ENTRY);
    let step_ids: HashSet<&str> = workflow_type
        .steps
        .iter()
        .map(|step| step.step_id.as_str())
        .collect();
    let mut errors = Vec::new();
    for collector in REQUIRED_COLLECTORS {
        if !step_ids.contains(collector) {
            errors.push(GraphValidationError {
                category: GraphErrorKind::MissingRequiredCollector,
                step_id: Some(collector.to_string()),
                message: format!("required post-PR collector '{}' is missing", collector),
            });
            continue;
        }
        if !reachable.contains(collector) {
            errors.push(GraphValidationError {
                category: GraphErrorKind::MissingRequiredCollector,
                step_id: Some(collector.to_string()),
                message: format!(
                    "required post-PR collector '{}' is unreachable from {}",
                    collector, POST_PR_ENTRY
                ),
            });
        }
    }
    errors
}

/// True when the workflow contains the post-PR follow-up entry step.
fn has_post_pr_subgraph(workflow_type: &WorkflowType) -> bool {
    workflow_type
        .steps
        .iter()
        .any(|step| step.step_id == POST_PR_ENTRY)
}

/// Validate the structural safety of a workflow graph, aggregating every error.
///
/// Generic validators (dangling targets, duplicate outcomes, unreachable steps)
/// apply to all workflows. The post-PR safety validators only fire when the
/// workflow contains the post-PR entry step `capture_pr_identity`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020
pub fn validate_workflow_graph(
    workflow_type: &WorkflowType,
) -> std::result::Result<(), Vec<GraphValidationError>> {
    let mut errors = Vec::new();
    errors.extend(validate_transitions_reference_valid_steps(workflow_type));
    errors.extend(validate_no_duplicate_outcomes(workflow_type));
    errors.extend(validate_all_steps_reachable(workflow_type));

    if has_post_pr_subgraph(workflow_type) {
        errors.extend(validate_post_pr_routes(workflow_type));
        errors.extend(validate_required_collectors_reachable(workflow_type));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::schema::{StepDef, TransitionDef, WorkflowType};

    /// Post-PR follow-up step ids, used to build minimal post-PR fixtures.
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
            step_type: "noop".to_string(),
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
            workflow_type_id: "test-wf".to_string(),
            steps,
            transitions,
            guards: Default::default(),
        }
    }

    fn categories(errors: &[GraphValidationError]) -> Vec<GraphErrorKind> {
        errors.iter().map(|e| e.category.clone()).collect()
    }

    #[test]
    fn dangling_to_target_is_rejected() {
        let wf = workflow(
            vec![step("a"), step("b")],
            vec![transition("a", "ghost", None), transition("a", "b", None)],
        );
        let err = validate_workflow_graph(&wf).unwrap_err();
        assert!(categories(&err).contains(&GraphErrorKind::DanglingTarget));
    }

    #[test]
    fn dangling_from_source_is_rejected() {
        let wf = workflow(
            vec![step("a"), step("b")],
            vec![transition("a", "b", None), transition("ghost", "b", None)],
        );
        let err = validate_workflow_graph(&wf).unwrap_err();
        assert!(categories(&err).contains(&GraphErrorKind::DanglingTarget));
    }

    #[test]
    fn duplicate_success_branch_is_rejected() {
        let wf = workflow(
            vec![step("a"), step("b"), step("c")],
            vec![transition("a", "b", None), transition("a", "c", None)],
        );
        let err = validate_workflow_graph(&wf).unwrap_err();
        assert!(categories(&err).contains(&GraphErrorKind::DuplicateOutcome));
    }

    #[test]
    fn duplicate_explicit_condition_branch_is_rejected() {
        let wf = workflow(
            vec![step("a"), step("b"), step("c")],
            vec![
                transition("a", "b", Some("fatal")),
                transition("a", "c", Some("fatal")),
            ],
        );
        let err = validate_workflow_graph(&wf).unwrap_err();
        assert!(categories(&err).contains(&GraphErrorKind::DuplicateOutcome));
    }

    #[test]
    fn orphan_step_is_rejected() {
        let wf = workflow(
            vec![step("a"), step("b"), step("orphan")],
            vec![transition("a", "b", None)],
        );
        let err = validate_workflow_graph(&wf).unwrap_err();
        assert!(categories(&err).contains(&GraphErrorKind::UnreachableStep));
    }

    #[test]
    fn valid_minimal_graph_passes() {
        let wf = workflow(
            vec![step("a"), step("b"), step("c")],
            vec![transition("a", "b", None), transition("b", "c", None)],
        );
        assert!(validate_workflow_graph(&wf).is_ok());
    }

    fn minimal_post_pr_steps() -> Vec<StepDef> {
        POST_PR_STEPS.iter().map(|id| step(id)).collect()
    }

    fn minimal_post_pr_transitions() -> Vec<TransitionDef> {
        vec![
            transition("capture_pr_identity", "post_pr_iteration_guard", None),
            transition("post_pr_iteration_guard", "watch_pr_checks", None),
            transition("watch_pr_checks", "collect_ci_failures", None),
            transition("collect_ci_failures", "collect_coderabbit_feedback", None),
            transition(
                "collect_coderabbit_feedback",
                "evaluate_coderabbit_feedback",
                None,
            ),
            transition(
                "evaluate_coderabbit_feedback",
                "build_remediation_plan",
                None,
            ),
            transition("build_remediation_plan", "remediate_pr_followup", None),
            transition(
                "remediate_pr_followup",
                "validate_remediation_result",
                None,
            ),
            transition("validate_remediation_result", "run_post_pr_tests", None),
            transition("run_post_pr_tests", "push_remediation_changes", None),
            transition(
                "push_remediation_changes",
                "mark_coderabbit_feedback",
                None,
            ),
            transition(
                "mark_coderabbit_feedback",
                "post_pr_failure_terminal",
                None,
            ),
        ]
    }

    #[test]
    fn post_pr_fatal_to_abandon_is_rejected() {
        let mut steps = minimal_post_pr_steps();
        steps.push(step("abandon_and_log"));
        let mut transitions = minimal_post_pr_transitions();
        transitions.push(transition(
            "capture_pr_identity",
            "abandon_and_log",
            Some("fatal"),
        ));
        let wf = workflow(steps, transitions);
        let err = validate_workflow_graph(&wf).unwrap_err();
        assert!(categories(&err).contains(&GraphErrorKind::UnsafePostPrRoute));
    }

    #[test]
    fn post_pr_missing_collector_is_rejected() {
        // Drop collect_ci_failures from the step set and its transitions.
        let steps: Vec<StepDef> = minimal_post_pr_steps()
            .into_iter()
            .filter(|s| s.step_id != "collect_ci_failures")
            .collect();
        let transitions: Vec<TransitionDef> = minimal_post_pr_transitions()
            .into_iter()
            .filter(|t| t.from != "collect_ci_failures" && t.to != "collect_ci_failures")
            .collect();
        let wf = workflow(steps, transitions);
        let err = validate_workflow_graph(&wf).unwrap_err();
        assert!(categories(&err).contains(&GraphErrorKind::MissingRequiredCollector));
    }
}
