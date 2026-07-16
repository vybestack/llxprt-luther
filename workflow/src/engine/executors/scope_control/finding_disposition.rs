//! Two-axis finding disposition (issue 142, slice 4).
//!
//! Every evaluated finding has two independent axes:
//! - correctness severity: `Blocker`, `High`, `Medium`, `Low`, `Invalid`;
//! - delivery scope: `RequiredAcceptanceCriterion`, `RegressionFromCurrentPatch`,
//!   `SmallAdjacentFix`, `FollowUpIssue`, `UserDecision`.
//!
//! The deterministic disposition matrix decides whether a finding must be
//! remediated in the current delivery, deferred to a follow-up issue, or
//! escalated to a user decision. Invalid findings are never remediated.
//! Follow-up findings are durably recorded and do not fail the delivery.
//! Required acceptance criteria and current-patch regressions are always
//! current-delivery work. Mandatory quality-gate failures (compile, lint,
//! test, security, coverage) cannot be waived because they map to
//! `RequiredAcceptanceCriterion` or `RegressionFromCurrentPatch` scope.
//!
//! Historical single-axis artifacts (decision: valid/invalid/out_of_scope/
//! needs_user_judgment) remain readable through [`project_legacy_decision`].

use serde::{Deserialize, Serialize};

/// Independent correctness severity of a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingCorrectness {
    Blocker,
    High,
    Medium,
    Low,
    Invalid,
}

impl FindingCorrectness {
    /// Parse a snake_case wire string into a [`FindingCorrectness`].
    /// Returns `None` for unrecognized values so callers fail closed.
    #[must_use]
    pub fn from_str_lossy(value: &str) -> Option<Self> {
        match value {
            "blocker" => Some(Self::Blocker),
            "high" => Some(Self::High),
            "medium" => Some(Self::Medium),
            "low" => Some(Self::Low),
            "invalid" => Some(Self::Invalid),
            _ => None,
        }
    }

    /// Returns the canonical snake_case wire string for this variant.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Blocker => "blocker",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::Invalid => "invalid",
        }
    }
}

/// Independent delivery scope classification of a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingDeliveryScope {
    RequiredAcceptanceCriterion,
    RegressionFromCurrentPatch,
    SmallAdjacentFix,
    FollowUpIssue,
    UserDecision,
}

impl FindingDeliveryScope {
    /// Parse a snake_case wire string into a [`FindingDeliveryScope`].
    /// Returns `None` for unrecognized values so callers fail closed.
    #[must_use]
    pub fn from_str_lossy(value: &str) -> Option<Self> {
        match value {
            "required_acceptance_criterion" => Some(Self::RequiredAcceptanceCriterion),
            "regression_from_current_patch" => Some(Self::RegressionFromCurrentPatch),
            "small_adjacent_fix" => Some(Self::SmallAdjacentFix),
            "follow_up_issue" => Some(Self::FollowUpIssue),
            "user_decision" => Some(Self::UserDecision),
            _ => None,
        }
    }

    /// Returns the canonical snake_case wire string for this variant.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RequiredAcceptanceCriterion => "required_acceptance_criterion",
            Self::RegressionFromCurrentPatch => "regression_from_current_patch",
            Self::SmallAdjacentFix => "small_adjacent_fix",
            Self::FollowUpIssue => "follow_up_issue",
            Self::UserDecision => "user_decision",
        }
    }
}

/// The two-axis disposition of a finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingDisposition {
    pub correctness: FindingCorrectness,
    pub delivery_scope: FindingDeliveryScope,
}

/// The deterministic action the workflow takes for a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispositionAction {
    /// Must be remediated in the current delivery.
    RemediateNow,
    /// Durably recorded but does not fail the current delivery.
    DeferToFollowUp,
    /// Block delivery pending a human decision.
    BlockForUserDecision,
    /// Ignore entirely (invalid findings).
    Ignore,
}

impl DispositionAction {
    /// Whether this action blocks or fails the current delivery.
    #[must_use]
    pub const fn blocks_delivery(self) -> bool {
        matches!(self, Self::BlockForUserDecision)
    }

    /// Whether this action fails the current delivery (terminal).
    #[must_use]
    pub const fn fails_delivery(self) -> bool {
        matches!(self, Self::BlockForUserDecision)
    }
}

/// Determine the deterministic disposition action for a finding.
///
/// Rules (fail closed for delivery correctness):
/// - `Invalid` correctness → `Ignore` (never remediated, regardless of scope).
/// - `FollowUpIssue` scope → `DeferToFollowUp` (does not fail delivery).
/// - `UserDecision` scope → `BlockForUserDecision`.
/// - `RequiredAcceptanceCriterion` scope → `RemediateNow` (acceptance must hold
///   regardless of severity — even Low acceptance criteria must pass).
/// - `RegressionFromCurrentPatch` scope → `RemediateNow` (regressions must be
///   fixed regardless of severity).
/// - `SmallAdjacentFix` scope + `Low` correctness → `DeferToFollowUp`.
/// - `SmallAdjacentFix` scope + (`Blocker`/`High`/`Medium`) → `RemediateNow`.
///
/// Mandatory quality-gate failures (compile, lint, test, security, coverage)
/// are guaranteed un-waivable because callers classify them as
/// `RequiredAcceptanceCriterion` or `RegressionFromCurrentPatch`, both of
/// which deterministically produce `RemediateNow` for any non-Invalid
/// correctness.
#[must_use]
pub fn disposition_action(
    correctness: FindingCorrectness,
    delivery_scope: FindingDeliveryScope,
) -> DispositionAction {
    if correctness == FindingCorrectness::Invalid {
        return DispositionAction::Ignore;
    }
    match delivery_scope {
        FindingDeliveryScope::FollowUpIssue => DispositionAction::DeferToFollowUp,
        FindingDeliveryScope::UserDecision => DispositionAction::BlockForUserDecision,
        FindingDeliveryScope::RequiredAcceptanceCriterion
        | FindingDeliveryScope::RegressionFromCurrentPatch => DispositionAction::RemediateNow,
        FindingDeliveryScope::SmallAdjacentFix => {
            if correctness == FindingCorrectness::Low {
                DispositionAction::DeferToFollowUp
            } else {
                DispositionAction::RemediateNow
            }
        }
    }
}

/// Whether a finding source matches a mandatory quality gate.
///
/// Mandatory gate failures cannot be waived. When this returns `true`, callers
/// must classify the finding as `RequiredAcceptanceCriterion` or
/// `RegressionFromCurrentPatch` delivery scope, which deterministically
/// produces `RemediateNow`.
#[must_use]
pub fn is_mandatory_gate(source: &str, mandatory_gates: &[String]) -> bool {
    mandatory_gates
        .iter()
        .any(|gate| source.contains(gate.as_str()))
}

/// Project a historical single-axis evaluator decision onto the two-axis model.
///
/// Legacy decisions: `"valid"`, `"invalid"`, `"out_of_scope"`,
/// `"needs_user_judgment"`. The projection is lossy but conservative:
/// unrecognized decisions map to medium-severity user decisions so they are
/// never silently dropped.
#[must_use]
pub fn project_legacy_decision(legacy_decision: &str) -> FindingDisposition {
    match legacy_decision {
        "valid" => FindingDisposition {
            correctness: FindingCorrectness::High,
            delivery_scope: FindingDeliveryScope::RequiredAcceptanceCriterion,
        },
        "invalid" => FindingDisposition {
            correctness: FindingCorrectness::Invalid,
            delivery_scope: FindingDeliveryScope::FollowUpIssue,
        },
        "out_of_scope" => FindingDisposition {
            correctness: FindingCorrectness::Low,
            delivery_scope: FindingDeliveryScope::FollowUpIssue,
        },
        "needs_user_judgment" => FindingDisposition {
            correctness: FindingCorrectness::Medium,
            delivery_scope: FindingDeliveryScope::UserDecision,
        },
        _ => FindingDisposition {
            correctness: FindingCorrectness::Medium,
            delivery_scope: FindingDeliveryScope::UserDecision,
        },
    }
}

/// Resolve the two-axis disposition for an accepted feedback-evaluation result.
///
/// New evaluator artifacts carry explicit `correctness` and `delivery_scope`
/// string fields. Historical (single-axis) artifacts lack them; for those, the
/// legacy `decision` is projected via [`project_legacy_decision`].
///
/// When only one axis is present the legacy `decision` fills the missing axis,
/// preserving backward compatibility for transitional artifacts.
///
/// Returns a conservative `Medium`/`UserDecision` disposition for completely
/// unrecognized inputs so findings are never silently dropped.
#[must_use]
pub fn disposition_from_accepted_result(result: &serde_json::Value) -> FindingDisposition {
    let legacy_decision = result
        .get("decision")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let projected = project_legacy_decision(legacy_decision);
    let correctness = result
        .get("correctness")
        .and_then(serde_json::Value::as_str)
        .and_then(FindingCorrectness::from_str_lossy)
        .unwrap_or(projected.correctness);
    let delivery_scope = result
        .get("delivery_scope")
        .and_then(serde_json::Value::as_str)
        .and_then(FindingDeliveryScope::from_str_lossy)
        .unwrap_or(projected.delivery_scope);
    FindingDisposition {
        correctness,
        delivery_scope,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_finding_is_ignored() {
        for scope in [
            FindingDeliveryScope::RequiredAcceptanceCriterion,
            FindingDeliveryScope::RegressionFromCurrentPatch,
            FindingDeliveryScope::SmallAdjacentFix,
            FindingDeliveryScope::FollowUpIssue,
            FindingDeliveryScope::UserDecision,
        ] {
            assert_eq!(
                disposition_action(FindingCorrectness::Invalid, scope),
                DispositionAction::Ignore
            );
        }
    }

    #[test]
    fn follow_up_issue_does_not_fail_delivery() {
        for correctness in [
            FindingCorrectness::Blocker,
            FindingCorrectness::High,
            FindingCorrectness::Medium,
            FindingCorrectness::Low,
        ] {
            assert_eq!(
                disposition_action(correctness, FindingDeliveryScope::FollowUpIssue),
                DispositionAction::DeferToFollowUp
            );
            assert!(!DispositionAction::DeferToFollowUp.fails_delivery());
        }
    }

    #[test]
    fn user_decision_blocks() {
        assert_eq!(
            disposition_action(FindingCorrectness::High, FindingDeliveryScope::UserDecision),
            DispositionAction::BlockForUserDecision
        );
        assert!(DispositionAction::BlockForUserDecision.blocks_delivery());
        assert!(DispositionAction::BlockForUserDecision.fails_delivery());
    }

    #[test]
    fn required_acceptance_always_remediates() {
        for correctness in [
            FindingCorrectness::Blocker,
            FindingCorrectness::High,
            FindingCorrectness::Medium,
            FindingCorrectness::Low,
        ] {
            assert_eq!(
                disposition_action(
                    correctness,
                    FindingDeliveryScope::RequiredAcceptanceCriterion
                ),
                DispositionAction::RemediateNow
            );
        }
    }

    #[test]
    fn regression_always_remediates() {
        for correctness in [
            FindingCorrectness::Blocker,
            FindingCorrectness::High,
            FindingCorrectness::Medium,
            FindingCorrectness::Low,
        ] {
            assert_eq!(
                disposition_action(
                    correctness,
                    FindingDeliveryScope::RegressionFromCurrentPatch
                ),
                DispositionAction::RemediateNow
            );
        }
    }

    #[test]
    fn small_adjacent_fix_low_deferred() {
        assert_eq!(
            disposition_action(
                FindingCorrectness::Low,
                FindingDeliveryScope::SmallAdjacentFix
            ),
            DispositionAction::DeferToFollowUp
        );
    }

    #[test]
    fn small_adjacent_fix_high_remediates() {
        assert_eq!(
            disposition_action(
                FindingCorrectness::High,
                FindingDeliveryScope::SmallAdjacentFix
            ),
            DispositionAction::RemediateNow
        );
        assert_eq!(
            disposition_action(
                FindingCorrectness::Blocker,
                FindingDeliveryScope::SmallAdjacentFix
            ),
            DispositionAction::RemediateNow
        );
        assert_eq!(
            disposition_action(
                FindingCorrectness::Medium,
                FindingDeliveryScope::SmallAdjacentFix
            ),
            DispositionAction::RemediateNow
        );
    }

    #[test]
    fn mandatory_gate_detection() {
        let gates = vec![
            "cargo test".to_string(),
            "cargo clippy".to_string(),
            "cargo fmt".to_string(),
        ];
        assert!(is_mandatory_gate("cargo test failed", &gates));
        assert!(is_mandatory_gate("cargo clippy -- -D warnings", &gates));
        assert!(!is_mandatory_gate("tarpaulin coverage", &gates));
    }

    #[test]
    fn mandatory_gate_cannot_be_waived() {
        // A mandatory gate failure classified as RequiredAcceptanceCriterion
        // always produces RemediateNow regardless of severity.
        let action = disposition_action(
            FindingCorrectness::Low,
            FindingDeliveryScope::RequiredAcceptanceCriterion,
        );
        assert_eq!(action, DispositionAction::RemediateNow);
    }

    #[test]
    fn legacy_valid_maps_to_high_required() {
        let d = project_legacy_decision("valid");
        assert_eq!(d.correctness, FindingCorrectness::High);
        assert_eq!(
            d.delivery_scope,
            FindingDeliveryScope::RequiredAcceptanceCriterion
        );
    }

    #[test]
    fn legacy_invalid_maps_to_invalid_followup() {
        let d = project_legacy_decision("invalid");
        assert_eq!(d.correctness, FindingCorrectness::Invalid);
        assert_eq!(d.delivery_scope, FindingDeliveryScope::FollowUpIssue);
    }

    #[test]
    fn legacy_out_of_scope_maps_to_low_followup() {
        let d = project_legacy_decision("out_of_scope");
        assert_eq!(d.correctness, FindingCorrectness::Low);
        assert_eq!(d.delivery_scope, FindingDeliveryScope::FollowUpIssue);
    }

    #[test]
    fn legacy_needs_judgment_maps_to_medium_user() {
        let d = project_legacy_decision("needs_user_judgment");
        assert_eq!(d.correctness, FindingCorrectness::Medium);
        assert_eq!(d.delivery_scope, FindingDeliveryScope::UserDecision);
    }

    #[test]
    fn legacy_unknown_maps_conservatively() {
        let d = project_legacy_decision("totally_unknown");
        assert_eq!(d.correctness, FindingCorrectness::Medium);
        assert_eq!(d.delivery_scope, FindingDeliveryScope::UserDecision);
    }

    #[test]
    fn disposition_matrix_is_exhaustive_and_deterministic() {
        // Every combination produces a deterministic, known action.
        for correctness in [
            FindingCorrectness::Blocker,
            FindingCorrectness::High,
            FindingCorrectness::Medium,
            FindingCorrectness::Low,
            FindingCorrectness::Invalid,
        ] {
            for scope in [
                FindingDeliveryScope::RequiredAcceptanceCriterion,
                FindingDeliveryScope::RegressionFromCurrentPatch,
                FindingDeliveryScope::SmallAdjacentFix,
                FindingDeliveryScope::FollowUpIssue,
                FindingDeliveryScope::UserDecision,
            ] {
                let action = disposition_action(correctness, scope);
                // Every action is one of the four known variants.
                assert!(matches!(
                    action,
                    DispositionAction::RemediateNow
                        | DispositionAction::DeferToFollowUp
                        | DispositionAction::BlockForUserDecision
                        | DispositionAction::Ignore
                ));
                // Deterministic: same inputs → same output.
                assert_eq!(action, disposition_action(correctness, scope));
            }
        }
    }

    #[test]
    fn correctness_round_trips_wire_strings() {
        for variant in [
            FindingCorrectness::Blocker,
            FindingCorrectness::High,
            FindingCorrectness::Medium,
            FindingCorrectness::Low,
            FindingCorrectness::Invalid,
        ] {
            assert_eq!(
                FindingCorrectness::from_str_lossy(variant.as_str()),
                Some(variant)
            );
        }
        assert_eq!(FindingCorrectness::from_str_lossy("unknown"), None);
    }

    #[test]
    fn delivery_scope_round_trips_wire_strings() {
        for variant in [
            FindingDeliveryScope::RequiredAcceptanceCriterion,
            FindingDeliveryScope::RegressionFromCurrentPatch,
            FindingDeliveryScope::SmallAdjacentFix,
            FindingDeliveryScope::FollowUpIssue,
            FindingDeliveryScope::UserDecision,
        ] {
            assert_eq!(
                FindingDeliveryScope::from_str_lossy(variant.as_str()),
                Some(variant)
            );
        }
        assert_eq!(FindingDeliveryScope::from_str_lossy("unknown"), None);
    }

    #[test]
    fn disposition_from_two_axis_result_uses_explicit_fields() {
        let result = serde_json::json!({
            "decision": "valid",
            "correctness": "low",
            "delivery_scope": "small_adjacent_fix"
        });
        let d = disposition_from_accepted_result(&result);
        assert_eq!(d.correctness, FindingCorrectness::Low);
        assert_eq!(d.delivery_scope, FindingDeliveryScope::SmallAdjacentFix);
        // Low + small_adjacent_fix → deferred, not remediated.
        assert_eq!(
            disposition_action(d.correctness, d.delivery_scope),
            DispositionAction::DeferToFollowUp
        );
    }

    #[test]
    fn disposition_from_legacy_result_projects_decision() {
        let result = serde_json::json!({ "decision": "valid" });
        let d = disposition_from_accepted_result(&result);
        assert_eq!(d.correctness, FindingCorrectness::High);
        assert_eq!(
            d.delivery_scope,
            FindingDeliveryScope::RequiredAcceptanceCriterion
        );
    }

    #[test]
    fn disposition_from_partial_result_fills_missing_axis_from_decision() {
        // Only correctness present → delivery_scope projected from decision.
        let result = serde_json::json!({
            "decision": "invalid",
            "correctness": "blocker"
        });
        let d = disposition_from_accepted_result(&result);
        assert_eq!(d.correctness, FindingCorrectness::Blocker);
        // Legacy "invalid" projects to FollowUpIssue for delivery_scope.
        assert_eq!(d.delivery_scope, FindingDeliveryScope::FollowUpIssue);
    }

    #[test]
    fn disposition_from_missing_decision_is_conservative() {
        let result = serde_json::json!({});
        let d = disposition_from_accepted_result(&result);
        assert_eq!(d.correctness, FindingCorrectness::Medium);
        assert_eq!(d.delivery_scope, FindingDeliveryScope::UserDecision);
    }

    #[test]
    fn disposition_from_garbage_axis_falls_back_to_legacy() {
        let result = serde_json::json!({
            "decision": "out_of_scope",
            "correctness": "totally_bogus",
            "delivery_scope": "nonsense"
        });
        let d = disposition_from_accepted_result(&result);
        // Unrecognized axis values fall back to the projected legacy decision.
        assert_eq!(d.correctness, FindingCorrectness::Low);
        assert_eq!(d.delivery_scope, FindingDeliveryScope::FollowUpIssue);
    }
}
