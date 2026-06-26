//! Shared PR follow-through artifact schema contracts.
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
//! @requirement:REQ-PRFU-002,REQ-PRFU-007,REQ-PRFU-020
//! @pseudocode lines 1-53

use serde_json::Value;

/// Returns true when an optional routing field carries a non-null value.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-007
fn is_present_non_null(field: &Option<Value>) -> bool {
    matches!(field, Some(value) if !value.is_null())
}

/// Shared PR follow-through artifact field schema version.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 1-53
pub const PR_FOLLOWUP_SCHEMA_VERSION: u32 = 1;

/// Stable-marker-key prefix that identifies a CodeRabbit summary/walkthrough
/// issue comment. Summary items are informational readiness signals only: they
/// must never produce `mark_invalid` plan entries, pending marker actions, or
/// top-level PR comments. This single literal is the canonical discriminator
/// shared across the plan stage, the live-refresh path, and the mutation gate.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-020
pub const SUMMARY_MARKER_KEY_PREFIX: &str = "summary:";

/// Returns true when a stable marker key identifies a CodeRabbit
/// summary/walkthrough item (informational only).
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-020
#[must_use]
pub fn is_summary_marker_key(stable_marker_key: &str) -> bool {
    stable_marker_key.starts_with(SUMMARY_MARKER_KEY_PREFIX)
}

/// Returns true when a JSON value (a materialized plan item, a pending marker
/// action, or an evaluation result) carries a CodeRabbit summary/walkthrough
/// `stable_marker_key`. Such values are informational readiness signals only and
/// must never be routed into `mark_invalid`, materialized into a pending marker
/// action, posted, or resolved. This is the single canonical `Value`-level
/// summary discriminator shared across the plan stage and the mutation gate so
/// the two suppression layers can never drift apart.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-020
#[must_use]
pub fn value_has_summary_marker_key(value: &Value) -> bool {
    value
        .get("stable_marker_key")
        .and_then(Value::as_str)
        .is_some_and(is_summary_marker_key)
}

/// Typed terminal `overall_state` produced by PR check watching. Modeling the
/// routing state as an enum makes invalid states unrepresentable instead of
/// relying on ad hoc string comparisons against loosely validated JSON.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-007
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OverallState {
    /// All required checks reported success.
    Passed,
    /// At least one required check failed.
    Failed,
    /// Check state could not be determined.
    #[default]
    Unknown,
    /// Watching terminated fatally (e.g. a non-recoverable API error).
    Fatal,
    /// Pending checks never resolved within the configured timeout.
    PendingTimeout,
}

impl OverallState {
    /// Returns the canonical snake_case wire string for this state, matching
    /// the serde representation used in persisted artifacts.
    /// @requirement:REQ-PRFU-007
    pub fn as_str(self) -> &'static str {
        match self {
            OverallState::Passed => "passed",
            OverallState::Failed => "failed",
            OverallState::Unknown => "unknown",
            OverallState::Fatal => "fatal",
            OverallState::PendingTimeout => "pending_timeout",
        }
    }
}

/// Typed `collection_state` produced by CI failure collection.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-007
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionState {
    /// Failures (if any) were collected without a fatal upstream signal.
    #[default]
    Collected,
    /// Collection routed fatal because the upstream watcher was fatal.
    Fatal,
}

impl CollectionState {
    /// Returns the canonical snake_case wire string for this state.
    /// @requirement:REQ-PRFU-007
    pub fn as_str(self) -> &'static str {
        match self {
            CollectionState::Collected => "collected",
            CollectionState::Fatal => "fatal",
        }
    }
}

/// Typed `evaluation_state` produced by CodeRabbit feedback evaluation.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-007
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationState {
    /// Evaluation completed and a decision is available.
    #[default]
    Complete,
    /// The evaluation budget was exhausted before completion.
    BudgetExhausted,
    /// Evaluation could not complete but is neither budget-exhausted nor fatal.
    Incomplete,
    /// Evaluation terminated fatally.
    Fatal,
}

impl EvaluationState {
    /// Returns the canonical snake_case wire string for this state.
    /// @requirement:REQ-PRFU-007
    pub fn as_str(self) -> &'static str {
        match self {
            EvaluationState::Complete => "complete",
            EvaluationState::BudgetExhausted => "budget_exhausted",
            EvaluationState::Incomplete => "incomplete",
            EvaluationState::Fatal => "fatal",
        }
    }
}

/// Typed `plan_state` produced by remediation planning.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-007
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanState {
    /// No remediation work is required.
    #[default]
    Clean,
    /// Remediation work is required and can proceed.
    NeedsRemediation,
    /// Remediation is blocked pending human judgment.
    BlockedNeedsUserJudgment,
    /// Planning terminated fatally.
    Fatal,
}

impl PlanState {
    /// Returns the canonical snake_case wire string for this state.
    /// @requirement:REQ-PRFU-007
    pub fn as_str(self) -> &'static str {
        match self {
            PlanState::Clean => "clean",
            PlanState::NeedsRemediation => "needs_remediation",
            PlanState::BlockedNeedsUserJudgment => "blocked_needs_user_judgment",
            PlanState::Fatal => "fatal",
        }
    }
}

/// Typed `validation_state` produced by remediation result validation.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-007
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationState {
    /// No validation has been performed yet (initial artifact state).
    #[default]
    Unvalidated,
    /// The remediation result validated successfully.
    Valid,
    /// The result was malformed but remediation can be retried.
    FixableMalformed,
    /// The malformed-result retry cap was exhausted.
    MalformedCapExhausted,
    /// The result was well-formed but remediation was unsuccessful.
    ValidButUnsuccessful,
    /// The unsuccessful-remediation attempt cap was exhausted.
    UnsuccessfulRemediationCapExhausted,
}

impl ValidationState {
    /// Returns the canonical snake_case wire string for this state.
    /// @requirement:REQ-PRFU-007
    pub fn as_str(self) -> &'static str {
        match self {
            ValidationState::Unvalidated => "unvalidated",
            ValidationState::Valid => "valid",
            ValidationState::FixableMalformed => "fixable_malformed",
            ValidationState::MalformedCapExhausted => "malformed_cap_exhausted",
            ValidationState::ValidButUnsuccessful => "valid_but_unsuccessful",
            ValidationState::UnsuccessfulRemediationCapExhausted => {
                "unsuccessful_remediation_cap_exhausted"
            }
        }
    }
}

/// Common PR artifact binding fields shared by PR follow-through artifacts.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 1-53
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PrFollowupBinding {
    pub schema_version: u32,
    pub run_id: String,
    pub repository_owner: String,
    pub repository_name: String,
    pub pr_number: u64,
    pub head_ref: String,
    pub head_sha: String,
    pub base_ref: String,
    pub base_sha: Option<String>,
}

/// Common artifact sequence metadata shared by PR follow-through artifacts.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 1-53
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ArtifactSequenceMetadata {
    pub artifact_sequence: u64,
    pub write_sequence: u64,
    pub producer_step_id: String,
}

/// PR identity artifact schema contract for `pr.json`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 1-7
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PrIdentity {
    pub binding: PrFollowupBinding,
    pub sequence: ArtifactSequenceMetadata,
    pub pr_url: String,
    pub capture_state: String,
}

/// PR check status artifact schema contract for `pr-check-status.json`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-002,REQ-PRFU-007
/// @pseudocode lines 16-33
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PrCheckStatus {
    #[serde(flatten)]
    pub binding: PrFollowupBinding,
    #[serde(flatten)]
    pub sequence: ArtifactSequenceMetadata,
    pub overall_state: OverallState,
    /// Watcher fatal source recorded when check polling failed fatally.
    /// @requirement:REQ-PRFU-007
    #[serde(default)]
    pub fatal_source: Option<Value>,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-007
impl PrCheckStatus {
    /// Validates routing invariants. `overall_state` is now an exhaustive enum,
    /// so the only remaining check is the "passed implies no fatal_source"
    /// cross-field contradiction guard.
    /// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
    /// @requirement:REQ-PRFU-007
    pub fn validate_invariants(&self) -> Result<(), String> {
        if matches!(self.overall_state, OverallState::Passed)
            && is_present_non_null(&self.fatal_source)
        {
            return Err(
                "pr-check-status overall_state 'passed' must not carry a non-null fatal_source"
                    .to_string(),
            );
        }
        Ok(())
    }
}

/// CI failures artifact schema contract for `ci-failures.json`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-002,REQ-PRFU-007
/// @pseudocode lines 1-21
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct CiFailures {
    #[serde(flatten)]
    pub binding: PrFollowupBinding,
    #[serde(flatten)]
    pub sequence: ArtifactSequenceMetadata,
    pub collection_state: CollectionState,
    /// Fatal source carried forward into the collection artifact.
    /// @requirement:REQ-PRFU-007
    #[serde(default)]
    pub fatal_source: Option<Value>,
    /// Fatal source observed by the upstream check watcher.
    /// @requirement:REQ-PRFU-007
    #[serde(default)]
    pub watcher_fatal_source: Option<Value>,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-007
impl CiFailures {
    /// Validates routing invariants for the collection artifact. `collection_state`
    /// is now an exhaustive enum, so the only remaining check is the cross-field
    /// contradiction guard: a non-fatal `collected` state must not carry an
    /// upstream `watcher_fatal_source`, because the collector only emits
    /// `collected` when the watcher fatal source was null; a stale value here is
    /// exactly the contradictory state that can misroute the workflow. (Note: a
    /// `collected` artifact may still carry a non-null `fatal_source` derived
    /// from `overall_state` when pending/unknown checks remain, so that field is
    /// not constrained here.)
    /// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
    /// @requirement:REQ-PRFU-007
    pub fn validate_invariants(&self) -> Result<(), String> {
        if matches!(self.collection_state, CollectionState::Collected)
            && is_present_non_null(&self.watcher_fatal_source)
        {
            return Err(
                "ci-failures collection_state 'collected' must not carry a non-null watcher_fatal_source"
                    .to_string(),
            );
        }
        Ok(())
    }
}

/// CodeRabbit feedback artifact schema contract for `coderabbit-feedback.json`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 1-29
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct CodeRabbitFeedback {
    pub binding: PrFollowupBinding,
    pub sequence: ArtifactSequenceMetadata,
    pub readiness_state: String,
}

/// Feedback state artifact schema contract for `coderabbit-feedback-state.json`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 26-28
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct FeedbackState {
    pub binding: PrFollowupBinding,
    pub sequence: ArtifactSequenceMetadata,
    pub state_index_hash: String,
}

/// Feedback evaluations artifact schema contract for `feedback-evaluations.json`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 1-23
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct FeedbackEvaluations {
    pub binding: PrFollowupBinding,
    pub sequence: ArtifactSequenceMetadata,
    pub evaluation_state: EvaluationState,
}

/// Remediation plan artifact schema contract for `pr-remediation-plan.json`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 1-11
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PrRemediationPlan {
    pub binding: PrFollowupBinding,
    pub sequence: ArtifactSequenceMetadata,
    pub plan_state: PlanState,
}

/// Remediation result artifact schema contract for `pr-remediation-result.json`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 18-28
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PrRemediationResult {
    pub binding: PrFollowupBinding,
    pub sequence: ArtifactSequenceMetadata,
    pub validation_state: ValidationState,
}

/// Post-PR test result artifact schema contract for `post-pr-test-result.json`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 29-33
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PostPrTestResult {
    pub binding: PrFollowupBinding,
    pub sequence: ArtifactSequenceMetadata,
    pub test_state: String,
}

/// Push remediation result artifact schema contract for `push-remediation-result.json`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 34-40
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PushRemediationResult {
    pub binding: PrFollowupBinding,
    pub sequence: ArtifactSequenceMetadata,
    pub push_state: String,
}

/// Feedback marker report artifact schema contract for `pr-feedback-marker-report.json`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 41-49
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct FeedbackMarkerReport {
    pub binding: PrFollowupBinding,
    pub sequence: ArtifactSequenceMetadata,
    pub marker_state: String,
}

/// Post-PR iteration guard artifact schema contract for `post-pr-iteration-guard.json`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 8-15
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PostPrIterationGuard {
    pub binding: PrFollowupBinding,
    pub sequence: ArtifactSequenceMetadata,
    pub guard_state: String,
}

/// Post-PR failure terminal artifact schema contract for `post-pr-failure-terminal.json`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 50-53
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PostPrFailureTerminal {
    pub binding: PrFollowupBinding,
    pub sequence: ArtifactSequenceMetadata,
    pub terminal_state: String,
}
