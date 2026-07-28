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

use crate::engine::transition::StepOutcome;
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
    /// A loop-back transition omits the required explicit `max_iterations` cap.
    MissingLoopLimit,
    /// A terminal step declares an outgoing transition.
    TerminalHasOutgoing,
    /// A failure-cleanup step does not declare terminal semantics.
    InvalidFailureCleanup,
    /// A step parameter names an outcome that does not exist.
    UnknownOutcomeName,
    /// A `post_pr_iteration_guard` step omits a positive remediation cap.
    MissingRemediationCap,
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
    validate_loop_back_limits(workflow, &mut errors);
    validate_terminal_steps(workflow, &mut errors);
    validate_failure_cleanup_steps(workflow, &mut errors);
    validate_pr_remediation_caps(workflow, &mut errors);
    validate_configured_outcome_names(workflow, &mut errors);

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

/// The step_type marking the PR remediation iteration guard.
const POST_PR_ITERATION_GUARD: &str = "post_pr_iteration_guard";

/// The parameter that caps post-PR remediation loop iterations.
const REMEDIATION_CAP_PARAM: &str = "max_post_pr_remediation_iterations";

/// Whether `step` is a terminal step. A step is terminal when it explicitly
/// declares `terminal = true`, or (for back-compat) when its `step_type` is
/// `post_pr_failure_terminal`, the historically implicit terminal.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
fn is_terminal_step(step: &crate::workflow::schema::StepDef) -> bool {
    step.terminal == Some(true) || step.step_type == POST_PR_FAILURE_TERMINAL
}

/// Reject loop-back transitions that omit an explicit `max_iterations` cap.
///
/// A transition is a loop-back when its target appears at or before its source
/// in declaration order. This mirrors `EngineRunner::is_loop_back`
/// (`next_idx <= current_idx`) so static validation matches runtime behavior.
/// Loop-back edges must declare an explicit cap rather than silently falling
/// back to the global `max_iterations` default.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
fn validate_loop_back_limits(workflow: &WorkflowType, errors: &mut Vec<GraphValidationError>) {
    let index_of: HashMap<&str, usize> = workflow
        .steps
        .iter()
        .enumerate()
        .map(|(idx, step)| (step.step_id.as_str(), idx))
        .collect();

    for transition in &workflow.transitions {
        let (Some(&from_idx), Some(&to_idx)) = (
            index_of.get(transition.from.as_str()),
            index_of.get(transition.to.as_str()),
        ) else {
            // Dangling transitions are reported by another validator.
            continue;
        };
        let is_loop_back = to_idx <= from_idx;
        if is_loop_back && transition.max_iterations.is_none() {
            errors.push(GraphValidationError {
                step_id: Some(transition.from.clone()),
                detail: format!(
                    "loop-back transition {} --{}--> {} must declare an explicit max_iterations",
                    transition.from,
                    effective_condition(transition.condition.as_deref()),
                    transition.to
                ),
                category: GraphErrorCategory::MissingLoopLimit,
            });
        }
    }
}

/// Reject terminal steps that declare any outgoing transition.
///
/// Terminal steps must not route onward. A step is terminal per
/// `is_terminal_step` (explicit `terminal = true` or the implicit
/// `post_pr_failure_terminal` step_type).
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
fn validate_terminal_steps(workflow: &WorkflowType, errors: &mut Vec<GraphValidationError>) {
    for step in &workflow.steps {
        if !is_terminal_step(step) {
            continue;
        }
        for transition in workflow
            .transitions
            .iter()
            .filter(|transition| transition.from == step.step_id)
        {
            errors.push(GraphValidationError {
                step_id: Some(step.step_id.clone()),
                detail: format!(
                    "terminal step '{}' must not declare an outgoing transition to '{}'",
                    step.step_id, transition.to
                ),
                category: GraphErrorCategory::TerminalHasOutgoing,
            });
        }
    }
}

/// Require the typed failure-cleanup role to be terminal. Runtime preserves the
/// causal failure only for this exact contract, so accepting a non-terminal
/// declaration would allow cleanup success to erase the failed-run identity.
fn validate_failure_cleanup_steps(workflow: &WorkflowType, errors: &mut Vec<GraphValidationError>) {
    for step in &workflow.steps {
        if step.step_type != "failure_cleanup" {
            continue;
        }
        if step.terminal != Some(true) {
            errors.push(GraphValidationError {
                step_id: Some(step.step_id.clone()),
                detail: format!(
                    "failure_cleanup step '{}' must declare terminal = true",
                    step.step_id
                ),
                category: GraphErrorCategory::InvalidFailureCleanup,
            });
        }
        if workflow
            .steps
            .first()
            .is_some_and(|initial| initial.step_id == step.step_id)
        {
            errors.push(GraphValidationError {
                step_id: Some(step.step_id.clone()),
                detail: format!(
                    "failure_cleanup step '{}' must not be the initial step",
                    step.step_id
                ),
                category: GraphErrorCategory::InvalidFailureCleanup,
            });
        }
        let incoming = workflow
            .transitions
            .iter()
            .filter(|transition| transition.to == step.step_id)
            .collect::<Vec<_>>();
        if incoming.is_empty() {
            errors.push(GraphValidationError {
                step_id: Some(step.step_id.clone()),
                detail: format!(
                    "failure_cleanup step '{}' requires at least one incoming failure transition",
                    step.step_id
                ),
                category: GraphErrorCategory::InvalidFailureCleanup,
            });
        }
        for transition in incoming {
            if !matches!(
                transition.condition.as_deref(),
                Some("fatal" | "retryable" | "fixable")
            ) {
                errors.push(GraphValidationError {
                    step_id: Some(step.step_id.clone()),
                    detail: format!(
                        "failure_cleanup step '{}' requires an explicit failure outcome transition from '{}'",
                        step.step_id, transition.from
                    ),
                    category: GraphErrorCategory::InvalidFailureCleanup,
                });
            }
        }
    }
}

/// Reject `post_pr_iteration_guard` steps without a positive remediation cap.
///
/// Every outcome name a step configures must name a real outcome.
///
/// Executors read these names at runtime and previously each supplied their own
/// default for an unrecognised one. The defaults disagreed - `Success` in the
/// shell executor, `Fatal` in the llxprt executor - so a typo passed a run under
/// one and failed it under the other, with nothing reported either way.
///
/// Validating here removes the need for any runtime default: an unknown name is
/// rejected at load, naming the step and the key, so the two divergent
/// fallbacks become unreachable rather than merely reconciled.
fn validate_configured_outcome_names(
    workflow: &WorkflowType,
    errors: &mut Vec<GraphValidationError>,
) {
    // Both keys map a condition to an outcome name; `exit_code_map` is keyed by
    // exit code and `outcome_on_stdout` by a stdout pattern, so only the values
    // are outcome names in each.
    //
    // ANY NEW STEP PARAMETER WHOSE VALUES ARE OUTCOME NAMES MUST BE ADDED HERE.
    // A parameter that is missing from this list is not validated, and the
    // executor reading it will fall back to whatever its own default is - which
    // is the divergence this function exists to remove. `outcome_names_in`
    // below is the single place that knows how these parameters are shaped.
    const OUTCOME_VALUED_PARAMS: [&str; 2] = ["exit_code_map", "outcome_on_stdout"];

    for step in &workflow.steps {
        let Some(parameters) = step.parameters.as_ref() else {
            continue;
        };
        for param in OUTCOME_VALUED_PARAMS {
            let Some(map) = parameters.get(param).and_then(serde_json::Value::as_object) else {
                continue;
            };
            for (key, value) in map {
                // A non-string value is rejected rather than skipped. Skipping
                // it would let `"2" = 3` or a nested table through load-time
                // validation and into the executor, which is the silent path
                // this function exists to close.
                let Some(name) = value.as_str() else {
                    errors.push(GraphValidationError {
                        step_id: Some(step.step_id.clone()),
                        detail: format!(
                            "step '{}' maps {param}['{key}'] to {value}, which is not a string; \
                             outcome names must be one of success, retryable, fatal, fixable, \
                             abandon, wait",
                            step.step_id
                        ),
                        category: GraphErrorCategory::UnknownOutcomeName,
                    });
                    continue;
                };
                if StepOutcome::parse_condition_str(name).is_some() {
                    continue;
                }
                errors.push(GraphValidationError {
                    step_id: Some(step.step_id.clone()),
                    detail: format!(
                        "step '{}' maps {param}['{key}'] to '{name}', which is not an outcome; \
                         expected one of success, retryable, fatal, fixable, abandon, wait",
                        step.step_id
                    ),
                    category: GraphErrorCategory::UnknownOutcomeName,
                });
            }
        }
    }
}

/// The `max_post_pr_remediation_iterations` parameter must be present and a
/// positive integer so PR remediation loops have a configured cap validated
/// before execution, rather than silently defaulting at runtime.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
fn validate_pr_remediation_caps(workflow: &WorkflowType, errors: &mut Vec<GraphValidationError>) {
    for step in &workflow.steps {
        if step.step_type != POST_PR_ITERATION_GUARD {
            continue;
        }
        let cap = step
            .parameters
            .as_ref()
            .and_then(|params| params.get(REMEDIATION_CAP_PARAM))
            .and_then(serde_json::Value::as_u64);
        let valid = matches!(cap, Some(value) if value > 0);
        if !valid {
            errors.push(GraphValidationError {
                step_id: Some(step.step_id.clone()),
                detail: format!(
                    "post_pr_iteration_guard step '{}' must declare a positive {}",
                    step.step_id, REMEDIATION_CAP_PARAM
                ),
                category: GraphErrorCategory::MissingRemediationCap,
            });
        }
    }
}

#[cfg(test)]
#[path = "validation_tests.rs"]
mod tests;
