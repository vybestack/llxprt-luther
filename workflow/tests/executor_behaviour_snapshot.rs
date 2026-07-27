//! Step outcomes pinned before components are relocated.
//!
//! #206 asks that behaviour traces be identical before and after the move.
//! The existing `smoke_replay_tests` does not establish that: changing `noop`
//! to return `Fixable` instead of `Success` leaves it green, along with
//! `registry_composition` and all 84 e2e tests. Those suites cover the smoke
//! harness's own path, not the outcomes of the steps a relocation moves.
//!
//! This observes each domain-free executor directly and records what it
//! returns. A relocation that changes an outcome fails here, which is the
//! guarantee the issue actually asks for.
//!
//! Expected outcomes are written as literals. Deriving them by running the
//! executor and recording whatever came back would agree with any behaviour,
//! including behaviour a move had broken.

use luther_workflow::engine::executor::{ExecutorRegistry, StepContext};
use luther_workflow::engine::transition::StepOutcome;

/// Executors with no domain content, and what each returns for a minimal
/// input. These are the components #206 relocates into the generic package.
///
/// `noop` is the honest baseline: it takes no parameters and always succeeds,
/// so if the harness itself were broken this row would fail first.
const EXPECTED_OUTCOMES: &[(&str, &str)] = &[("noop", "Success")];

fn outcome_name(outcome: &StepOutcome) -> &'static str {
    match outcome {
        StepOutcome::Success => "Success",
        StepOutcome::Fatal => "Fatal",
        StepOutcome::Fixable => "Fixable",
        StepOutcome::Abandon => "Abandon",
        StepOutcome::Retryable => "Retryable",
        StepOutcome::Wait => "Wait",
    }
}

#[test]
fn domain_free_executors_return_the_outcomes_they_did_before_the_move() {
    let registry = ExecutorRegistry::with_defaults();
    let mut drifted = Vec::new();

    for (step_type, expected) in EXPECTED_OUTCOMES {
        let mut context =
            StepContext::new(std::path::PathBuf::from("."), "snapshot-run".to_string());
        let params = serde_json::json!({});
        match registry.dispatch(step_type, &mut context, &params) {
            Ok(outcome) => {
                let actual = outcome_name(&outcome);
                if actual != *expected {
                    drifted.push(format!("{step_type}: expected {expected}, got {actual}"));
                }
            }
            Err(error) => {
                drifted.push(format!(
                    "{step_type}: expected {expected}, got error {error:?}"
                ));
            }
        }
    }

    assert!(
        drifted.is_empty(),
        "executor outcomes changed across the move: {drifted:?}. A relocation must not alter \
         what a step returns; if an outcome genuinely should change, that is a behaviour change \
         and belongs in its own issue rather than inside a move."
    );
}

/// The registry still dispatches every domain-free step type.
///
/// Separate from the outcome check on purpose: an executor that stopped being
/// registered would make the loop above iterate over nothing and pass, which
/// is the vacuous success this series keeps finding.
#[test]
fn every_pinned_step_type_is_still_registered() {
    let registry = ExecutorRegistry::with_defaults();
    let registered = registry.registered_step_types();

    let missing: Vec<_> = EXPECTED_OUTCOMES
        .iter()
        .map(|(step_type, _)| *step_type)
        .filter(|step_type| !registered.contains(*step_type))
        .collect();

    assert!(
        missing.is_empty(),
        "pinned step types are no longer registered: {missing:?}"
    );
    assert!(
        !EXPECTED_OUTCOMES.is_empty(),
        "the pinned set must not be empty, or the outcome test above proves nothing"
    );
}
