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
    /// The artifact belongs to a different remediation retry scope.
    StaleArtifact,
    /// The stale-artifact infrastructure retry cap was exhausted.
    StaleArtifactCapExhausted,
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
            ValidationState::StaleArtifact => "stale_artifact",
            ValidationState::StaleArtifactCapExhausted => "stale_artifact_cap_exhausted",
        }
    }
}

/// Sentinel used when a marker action has no remediation output head.
/// Empty strings are invalid and are never equivalent to this value.
pub const NO_REMEDIATION_OUTPUT_HEAD: &str = "none";

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

/// Describes whether two bindings refer to the same logical PR identity
/// (same run, repository, PR number, and branch topology), independent of
/// the current head revision. Two bindings that share PR identity but differ
/// in `head_sha` belong to the same PR at different revisions — exactly the
/// situation that arises after a remediation push advances the branch head.
///
/// This separation is the architectural invariant that lets the artifact
/// store distinguish "stale prior-head artifact for the same PR" from
/// "artifact for a genuinely different PR." Only the former may be
/// gracefully ignored; the latter is always a binding mismatch.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-002
impl PrFollowupBinding {
    /// Returns true when `self` and `other` identify the same PR within the
    /// same run (stable identity fields match and are non-empty/valid). The
    /// head revision (`head_sha`/`base_sha`) is intentionally excluded so
    /// that a prior-head artifact does not masquerade as a different PR.
    #[must_use]
    pub fn pr_identity_matches(&self, other: &PrFollowupBinding) -> bool {
        self.schema_version != 0
            && self.schema_version == other.schema_version
            && !self.run_id.is_empty()
            && self.run_id == other.run_id
            && !self.repository_owner.is_empty()
            && self.repository_owner == other.repository_owner
            && !self.repository_name.is_empty()
            && self.repository_name == other.repository_name
            && self.pr_number != 0
            && self.pr_number == other.pr_number
            && !self.head_ref.is_empty()
            && self.head_ref == other.head_ref
            && !self.base_ref.is_empty()
            && self.base_ref == other.base_ref
    }

    /// Returns true when `self` and `other` share the same head revision
    /// (head SHA and base SHA). Both bindings must also carry non-empty SHAs
    /// so that an uninitialized binding can never match a real one.
    #[must_use]
    pub fn head_revision_matches(&self, other: &PrFollowupBinding) -> bool {
        !self.head_sha.is_empty()
            && self.head_sha == other.head_sha
            && self.base_sha.as_ref().is_none_or(|sha| !sha.is_empty())
            && other.base_sha.as_ref().is_none_or(|sha| !sha.is_empty())
            && self.base_sha == other.base_sha
    }

    /// Returns true when `other` describes a different head revision of the
    /// **same** PR as `self`. This is the precise condition under which an
    /// artifact bound to `other` may be treated as stale (and for optional
    /// inputs, as absent) rather than as a fatal binding mismatch.
    ///
    /// Returns `false` for genuinely different PRs.
    /// @requirement:REQ-PRFU-002
    #[must_use]
    pub fn is_stale_prior_head_of(&self, other: &PrFollowupBinding) -> bool {
        self.pr_identity_matches(other) && !self.head_revision_matches(other)
    }
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

/// Unified immutable evidence reference carried by a `comment_fixed` pending
/// marker action. Every field is extracted from the action's JSON value by
/// [`FixedActionEvidenceRef::from_action_value`] and validated together, so
/// the cross-head evidence lookup enforces a complete, self-consistent
/// provenance anchor before any history read.
///
/// Fields:
/// - `result_sequence`: the `artifact_sequence`/`write_sequence`/`producer_step_id`
///   of the exact remediation-result artifact snapshot the action was derived
///   from. All three must be nonzero and non-empty.
/// - `plan_artifact_sequence`: the `artifact_sequence` of the remediation plan
///   that produced the result, anchoring retry-scope consistency.
/// - `remediation_attempt_index`: the attempt index from the result's retry
///   scope, anchoring the retry-scope family.
/// - `source_head_sha` / `output_head_sha`: the input/output heads of the
///   remediation cycle that produced this action. `output_head_sha` must be
///   non-empty for a cross-head `comment_fixed` action (no `none` wildcard).
///
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixedActionEvidenceRef {
    pub result_sequence: ArtifactSequenceMetadata,
    pub plan_artifact_sequence: u64,
    pub remediation_attempt_index: u64,
    pub source_head_sha: String,
    pub output_head_sha: String,
}

impl FixedActionEvidenceRef {
    /// Extracts and validates a complete evidence reference from a pending
    /// action JSON value. Returns `Ok` only when every field is present,
    /// nonzero/non-empty, and internally consistent. A missing or zero-valued
    /// field produces a descriptive error so the caller fails closed.
    /// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
    /// @requirement:REQ-PRFU-002
    pub fn from_action_value(value: &Value) -> Result<Self, String> {
        let artifact_sequence = value
            .get("remediation_result_artifact_sequence")
            .and_then(Value::as_u64)
            .ok_or("missing remediation_result_artifact_sequence")?;
        let write_sequence = value
            .get("remediation_result_write_sequence")
            .and_then(Value::as_u64)
            .ok_or("missing remediation_result_write_sequence")?;
        let producer_step_id = value
            .get("remediation_result_producer_step_id")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .ok_or("missing or empty remediation_result_producer_step_id")?;
        if artifact_sequence == 0 || write_sequence == 0 {
            return Err("zero remediation_result sequence value".to_string());
        }
        let plan_artifact_sequence = value
            .get("plan_artifact_sequence")
            .and_then(Value::as_u64)
            .ok_or("missing plan_artifact_sequence")?;
        if plan_artifact_sequence == 0 {
            return Err("zero plan_artifact_sequence".to_string());
        }
        let remediation_attempt_index = value
            .get("remediation_attempt_index")
            .and_then(Value::as_u64)
            .ok_or("missing remediation_attempt_index")?;
        let source_head_sha = value
            .get("source_head_sha")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .ok_or("missing or empty source_head_sha")?
            .to_string();
        let output_head = value.get("remediation_output_head").and_then(Value::as_str);
        let output_head_sha_field = value
            .get("remediation_output_head_sha")
            .and_then(Value::as_str);
        if matches!((output_head, output_head_sha_field), (Some(left), Some(right)) if left != right)
        {
            return Err("inconsistent remediation output head fields".to_string());
        }
        if value
            .get("remediation_input_head_sha")
            .and_then(Value::as_str)
            .is_some_and(|head| head != source_head_sha)
        {
            return Err("inconsistent remediation input head fields".to_string());
        }
        let output_head_sha = output_head
            .or(output_head_sha_field)
            .unwrap_or_default()
            .to_string();
        if output_head_sha.is_empty() || output_head_sha == NO_REMEDIATION_OUTPUT_HEAD {
            return Err(format!(
                "comment_fixed cross-head action must carry a non-empty remediation_output_head, got {output_head_sha:?}"
            ));
        }
        Ok(Self {
            result_sequence: ArtifactSequenceMetadata {
                artifact_sequence,
                write_sequence,
                producer_step_id: producer_step_id.to_string(),
            },
            plan_artifact_sequence,
            remediation_attempt_index,
            source_head_sha,
            output_head_sha,
        })
    }
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
    pub fatal_source: Option<Value>,
    /// Fatal source observed by the upstream check watcher.
    /// @requirement:REQ-PRFU-007
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
