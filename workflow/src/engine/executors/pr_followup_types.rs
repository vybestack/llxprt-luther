//! Shared PR follow-through artifact schema contracts.
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
//! @requirement:REQ-PRFU-002,REQ-PRFU-020
//! @pseudocode lines 1-53

/// Shared PR follow-through artifact field schema version.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 1-53
pub const PR_FOLLOWUP_SCHEMA_VERSION: u32 = 1;

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
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 16-33
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PrCheckStatus {
    pub binding: PrFollowupBinding,
    pub sequence: ArtifactSequenceMetadata,
    pub overall_state: String,
}

/// CI failures artifact schema contract for `ci-failures.json`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 1-21
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct CiFailures {
    pub binding: PrFollowupBinding,
    pub sequence: ArtifactSequenceMetadata,
    pub collection_state: String,
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
    pub evaluation_state: String,
}

/// Remediation plan artifact schema contract for `pr-remediation-plan.json`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 1-11
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PrRemediationPlan {
    pub binding: PrFollowupBinding,
    pub sequence: ArtifactSequenceMetadata,
    pub plan_state: String,
}

/// Remediation result artifact schema contract for `pr-remediation-result.json`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 18-28
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PrRemediationResult {
    pub binding: PrFollowupBinding,
    pub sequence: ArtifactSequenceMetadata,
    pub validation_state: String,
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
