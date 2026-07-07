/// @plan:PLAN-20260404-INITIAL-RUNTIME.P06
/// Workflow transition logic - routes between steps based on outcomes.
use serde::{Deserialize, Serialize};

/// Outcome of executing a single workflow step.
/// Used by the engine to determine the next transition.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P06
/// @requirement:REQ-EARS-ROUTE-001
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepOutcome {
    /// Step completed successfully.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P06
    Success,
    /// Step encountered a retryable error.
    /// The engine should retry the step up to max_retries.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P06
    /// @requirement:REQ-EARS-ROUTE-004
    Retryable,
    /// Step encountered a fatal error.
    /// The engine should route to failure handling.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P06
    /// @requirement:REQ-EARS-ENG-003
    Fatal,
    /// Step indicates the issue is fixable by remediation.
    /// The engine should loop back to a prior step.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P06
    /// @requirement:REQ-EARS-ROUTE-002
    Fixable,
    /// Step indicates the workflow should be abandoned.
    /// Used when loop limits are reached.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P06
    /// @requirement:REQ-EARS-ROUTE-003
    Abandon,
    /// Step is waiting on a recoverable external condition (e.g. PR checks
    /// still pending when the watch window closed). The engine should pause
    /// at the current step with a resumable checkpoint rather than route to a
    /// terminal failure sink.
    /// @plan:PLAN-20260623-LUTHER-CONTINUATION
    Wait,
}

impl std::fmt::Display for StepOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_condition_str())
    }
}

impl StepOutcome {
    /// The canonical condition string used to match transitions.
    /// @plan:PLAN-20260623-LUTHER-CONTINUATION
    pub fn as_condition_str(&self) -> &'static str {
        match self {
            StepOutcome::Success => "success",
            StepOutcome::Retryable => "retryable",
            StepOutcome::Fatal => "fatal",
            StepOutcome::Fixable => "fixable",
            StepOutcome::Abandon => "abandon",
            StepOutcome::Wait => "wait",
        }
    }
}

impl std::str::FromStr for StepOutcome {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "success" => Ok(StepOutcome::Success),
            "retryable" => Ok(StepOutcome::Retryable),
            "fatal" => Ok(StepOutcome::Fatal),
            "fixable" => Ok(StepOutcome::Fixable),
            "abandon" => Ok(StepOutcome::Abandon),
            "wait" => Ok(StepOutcome::Wait),
            other => Err(format!("unknown step outcome: {other}")),
        }
    }
}

/// Definition of a transition in the workflow type.
/// Specifies where to go from a step based on an outcome.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P06
/// @plan:PLAN-20260408-LLXPRT-FIRST.P12
/// @requirement:REQ-EARS-ROUTE-001,REQ-EARS-ROUTE-002
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionDef {
    /// The source step (e.g., "build").
    pub from: String,
    /// The target step (e.g., "test").
    pub to: String,
    /// Optional condition - when this transition applies.
    /// Maps to StepOutcome as: "success", "retryable", "fatal", "fixable", "abandon".
    pub condition: Option<String>,
    /// Optional per-edge loop limit.
    pub max_iterations: Option<u32>,
}

/// A resolved transition from one step to another.
/// Contains the outcome that triggered it and the next step.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P06
/// @requirement:REQ-EARS-ROUTE-001
#[derive(Debug, Clone, PartialEq)]
pub struct Transition {
    /// The step that was just executed.
    pub step_id: String,
    /// The outcome of that step execution.
    pub outcome: StepOutcome,
    /// The next step to execute (if known).
    pub next_step: Option<String>,
}

impl Transition {
    /// Create a new transition record.
    /// @plan:PLAN-20260404-INITIAL-RUNTIME.P06
    pub fn new(
        step_id: impl Into<String>,
        outcome: StepOutcome,
        next_step: Option<String>,
    ) -> Self {
        Self {
            step_id: step_id.into(),
            outcome,
            next_step,
        }
    }
}

/// Resolve the next step based on current step, outcome, and transition definitions.
/// Returns Some(next_step_id) if a transition is found, None otherwise.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
/// @requirement:REQ-EARS-ROUTE-001,REQ-EARS-ROUTE-002
pub fn resolve_transition(
    step: &str,
    outcome: &StepOutcome,
    transitions: &[TransitionDef],
) -> Option<String> {
    // Convert outcome to condition string for matching
    let outcome_str = outcome.as_condition_str();

    // Look for a transition from the current step with matching condition
    for t in transitions {
        if t.from == step {
            // Check if condition matches outcome
            if let Some(ref cond) = t.condition {
                if cond == outcome_str {
                    return Some(t.to.clone());
                }
            } else if *outcome == StepOutcome::Success {
                // Default: no condition means success transition
                return Some(t.to.clone());
            }
        }
    }

    // No matching transition found
    None
}

/// Resolve the next step using schema transitions.
/// This is a convenience function that works with schema::TransitionDef.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P08
/// @requirement:REQ-EARS-ROUTE-001,REQ-EARS-ROUTE-002
pub fn resolve_transition_schema(
    step: &str,
    outcome: &StepOutcome,
    transitions: &[crate::workflow::schema::TransitionDef],
) -> Option<String> {
    // Convert outcome to condition string for matching
    let outcome_str = outcome.as_condition_str();

    // Look for a transition from the current step with matching condition
    for t in transitions {
        if t.from == step {
            // Check if condition matches outcome
            if let Some(ref cond) = t.condition {
                if cond == outcome_str {
                    return Some(t.to.clone());
                }
            } else if *outcome == StepOutcome::Success {
                // Default: no condition means success transition
                return Some(t.to.clone());
            }
        }
    }

    // No matching transition found
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_outcome_variants_exist() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P06
        let _success = StepOutcome::Success;
        let _retryable = StepOutcome::Retryable;
        let _fatal = StepOutcome::Fatal;
        let _fixable = StepOutcome::Fixable;
        let _abandon = StepOutcome::Abandon;
    }

    #[test]
    fn transition_def_can_be_created() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P06
        let t = TransitionDef {
            from: "build".to_string(),
            to: "test".to_string(),
            condition: Some("success".to_string()),
            max_iterations: None,
        };
        assert_eq!(t.from, "build");
        assert_eq!(t.to, "test");
    }

    #[test]
    fn transition_can_be_created() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P06
        let t = Transition::new("step1", StepOutcome::Success, Some("step2".to_string()));
        assert_eq!(t.step_id, "step1");
        assert_eq!(t.outcome, StepOutcome::Success);
        assert_eq!(t.next_step, Some("step2".to_string()));
    }

    #[test]
    fn step_outcome_display_formats_correctly() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P06
        assert_eq!(StepOutcome::Success.to_string(), "success");
        assert_eq!(StepOutcome::Retryable.to_string(), "retryable");
        assert_eq!(StepOutcome::Fatal.to_string(), "fatal");
        assert_eq!(StepOutcome::Fixable.to_string(), "fixable");
        assert_eq!(StepOutcome::Abandon.to_string(), "abandon");
        assert_eq!(StepOutcome::Wait.to_string(), "wait");
    }

    #[test]
    fn step_outcome_wait_round_trips_through_str() {
        // @plan:PLAN-20260623-LUTHER-CONTINUATION
        let parsed: StepOutcome = "wait".parse().expect("wait parses");
        assert_eq!(parsed, StepOutcome::Wait);
        assert_eq!(parsed.as_condition_str(), "wait");
    }

    #[test]
    fn step_outcome_from_str_rejects_unknown() {
        // @plan:PLAN-20260623-LUTHER-CONTINUATION
        let err = "nonsense".parse::<StepOutcome>().unwrap_err();
        assert!(err.contains("nonsense"));
    }

    #[test]
    fn resolve_transition_matches_wait_condition() {
        // @plan:PLAN-20260623-LUTHER-CONTINUATION
        let transitions = vec![TransitionDef {
            from: "watch".to_string(),
            to: "paused".to_string(),
            condition: Some("wait".to_string()),
            max_iterations: None,
        }];
        let next = resolve_transition("watch", &StepOutcome::Wait, &transitions);
        assert_eq!(next, Some("paused".to_string()));
    }

    #[test]
    fn resolve_transition_wait_without_edge_returns_none() {
        // @plan:PLAN-20260623-LUTHER-CONTINUATION
        let transitions = vec![TransitionDef {
            from: "watch".to_string(),
            to: "collect".to_string(),
            condition: Some("fatal".to_string()),
            max_iterations: None,
        }];
        let next = resolve_transition("watch", &StepOutcome::Wait, &transitions);
        assert_eq!(next, None);
    }
}
