//! @plan:PLAN-20260408-STEP-EXEC.P03
//! @plan:PLAN-20260408-LLXPRT-FIRST.P06
//! @plan:PLAN-20260408-LLXPRT-FIRST.P15
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
//! @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-013,REQ-PRFU-014,REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017,REQ-PRFU-026
//! @pseudocode lines 12-17,29-49
//!
//! @requirement:REQ-PRFU-020
//! @pseudocode lines 1-53
//! Executors module - concrete step executor implementations.
pub mod feedback_eval;
pub mod feedback_eval_policy;
pub mod feedback_eval_timeout;
pub mod github_feedback;
pub mod github_pr;
pub mod merge_wait;
pub mod parent_orchestration;
pub mod pr_check_wait;
pub mod pr_followup_artifacts;
pub mod pr_followup_types;
mod pr_identity_params;
pub mod pr_remediation;
pub mod workflow_auth_preflight;

// Re-export executor implementations for tests
/// Interpreter for workflow-authored commands.
///
/// Workflow commands are written in bash dialect -- `set -o pipefail`, `[[`,
/// arrays -- so the interpreter has to be bash. Running them under `sh` is
/// silently correct on macOS, where `/bin/sh` is bash in POSIX mode, and
/// silently broken on Debian and Ubuntu, where `/bin/sh` is dash and rejects
/// `set -o pipefail` on the first line. That divergence hid the defect from
/// every local run and every dogfood run.
///
/// Naming the interpreter rather than the path lets `PATH` resolve it, since
/// bash is `/bin/bash` on Linux and may be `/opt/homebrew/bin/bash` on macOS.
pub const WORKFLOW_SHELL: &str = "bash";

pub use feedback_eval::{
    default_feedback_evaluator_argv, CommandFeedbackEvaluationAdapter, FeedbackEvaluationRequest,
    FeedbackEvaluationResponse, FeedbackEvaluatorCommandRunner, FeedbackEvaluatorExecutor,
    ProcessFeedbackEvaluatorCommandRunner,
};
pub use feedback_eval_policy::FeedbackEvaluationAdapter;

pub use crate::components::generic::change_detection::{
    ChangeDetectionMode, ChangedPathDetector, GitChangedPathDetector,
};
pub use crate::components::generic::command_manifest::{
    request_from_entry, run_manifest_command, ManifestCommandRequest, ManifestCommandResult,
};
pub use crate::components::generic::noop::NoOpExecutor;
pub use crate::components::generic::shell::ShellExecutor;
pub use crate::components::generic::write_file::WriteFileExecutor;
pub use crate::components::software_change::git_config_publish::GitConfigPublishExecutor;
pub use crate::components::software_change::llxprt::{LlxprtExecutor, LlxprtExecutorWithDetector};
pub use crate::components::software_change::scope_control::{
    normalize_charter, validate_draft_against_config, validate_scope_control, CanonicalBudget,
    CanonicalReviewCaps, CanonicalTaskCharter, DraftBudget, DraftReviewCaps, DraftSubsystem,
    MergeBaseError, MergeBaseProbe, PreLaunchReviewRequest, ReviewCheckOutcome, ScopeEvaluation,
    ScopeMeasureExecutor, ScopePersistenceError, ScopeStatus, SystemMergeBaseProbe,
    TaskCharterDraft, TaskCharterExecutor, Violation, ViolationCode, CHARTER_SCHEMA_VERSION,
};
pub use crate::components::software_change::verify::VerifyExecutor;
pub use crate::components::software_change::workspace_ownership::{
    WorkspaceOwnershipExecutor, WorkspaceOwnershipVerifyExecutor,
};
pub use github_feedback::{
    FeedbackMarkerParser, GithubCodeRabbitFeedbackExecutor,
    GithubCodeRabbitFeedbackExecutorWithRunner, GithubFeedbackMarkerExecutor,
    GithubFeedbackMarkerExecutorWithRunner, RemoteFeedbackMarker, SystemFeedbackClock,
};
pub use github_pr::{
    GithubCheckFailuresExecutor, GithubCheckFailuresExecutorWithRunner, GithubPrChecksExecutor,
    GithubPrChecksExecutorWithRunner, GithubPrCommandRunner, GithubPrIdentityExecutor,
    GithubPrIdentityExecutorWithRunner, SystemGithubPrCommandRunner,
};
pub use merge_wait::{MergeWaitExecutor, MergeWaitProbe, RemoteProbeMergeWaitAdapter};
pub use parent_orchestration::model::{
    classify_child, next_actionable_child, order_subissues, ChildIssueState, ChildIssueStatus,
    ParentIssueOrchestrationState,
};
pub use parent_orchestration::{
    missing_ordered_child_states, ParentOrchestrationExecutor, ParentOrchestrationExecutorWithQuery,
};
pub use pr_followup_artifacts::{
    ArtifactPublicationHook, ArtifactPublicationStage, ArtifactReplayKey, ArtifactWriteContext,
    ArtifactWriter, ClockSleeper, JsonArtifactWriteRequest, PrFollowupArtifactStore,
    PrFollowupFilesystem, RawTextArtifactWriteRequest, SystemClockSleeper,
    SystemPrFollowupFilesystem, MAX_ARTIFACT_FILE_BYTES, MAX_ARTIFACT_READ_BYTES,
};
pub use pr_followup_types::{
    ArtifactSequenceMetadata, CiFailures, CodeRabbitFeedback, CollectionState, EvaluationState,
    FeedbackEvaluations, FeedbackMarkerReport, FeedbackState, FixedActionEvidenceRef, OverallState,
    PlanState, PostPrFailureTerminal, PostPrFailureTerminalHistory, PostPrFailureTerminalSource,
    PostPrIterationGuard, PostPrTestResult, PrCheckStatus, PrFollowupBinding, PrIdentity,
    PrRemediationPlan, PrRemediationResult, PushRemediationResult, ValidationState,
    PR_FOLLOWUP_SCHEMA_VERSION,
};
pub use pr_remediation::{
    LlxprtInvocationRequest, LlxprtInvocationResult, PostPrFailureTerminalExecutor,
    PostPrFailureTerminalExecutorWithClock, PostPrIterationGuardExecutor, PostPrTestCommandRequest,
    PostPrTestCommandResult, PostPrTestCommandRunner, PrFollowupLlxprtCommandRunner,
    PrFollowupRemediationExecutor, PrFollowupRemediationExecutorWithRunner,
    PrRemediationPlanExecutor, PrRemediationResultExecutor, PushRemediationChangesExecutor,
    PushRemediationChangesExecutorWithRunner, PushRemediationCommandRequest,
    PushRemediationCommandResult, PushRemediationCommandRunner, RunPostPrTestsExecutor,
    RunPostPrTestsExecutorWithRunner, SystemPrFollowupLlxprtCommandRunner,
};
pub use workflow_auth_preflight::WorkflowAuthPreflightExecutor;

/// Enforce the scope-decision barrier at a mutation entry point.
///
/// Returns `Some(StepOutcome::Wait)` when the patch is over-budget and must
/// be resolved before mutation proceeds, or `None` when mutation is allowed.
/// When no scope-control policy is active or no charter artifact exists, the
/// barrier is a no-op (`None`).
///
/// This is the shared compact barrier for broad mutation executors
/// (`llxprt`, `pr_followup_remediation`) and the pre-push executor
/// (`push_remediation_changes`).
pub(crate) fn scope_control_barrier(
    context: &mut crate::engine::executor::StepContext,
) -> Option<crate::engine::transition::StepOutcome> {
    scope_control_barrier_impl(context)
}

fn scope_control_barrier_impl(
    context: &mut crate::engine::executor::StepContext,
) -> Option<crate::engine::transition::StepOutcome> {
    use crate::components::software_change::scope_control::{
        enforce_scope_barrier, ScopeBarrierResult, SystemGitPatchCollector,
    };
    let scope_control = match resolve_scope_control_policy(context) {
        Ok(Some(config)) => config,
        Ok(None) => return None,
        Err(err) => {
            tracing::error!(run_id = %context.run_id(), error = %err, "invalid scope-control policy");
            return Some(crate::engine::transition::StepOutcome::Fatal);
        }
    };
    match enforce_scope_barrier(context, &SystemGitPatchCollector, &scope_control) {
        Ok(ScopeBarrierResult::Blocked) => {
            // Mark the context so the wait-state persistence layer classifies
            // this as a ScopeDecision wait rather than defaulting to
            // HumanReview. Without this marker, the originating step (e.g.
            // llxprt) would be misclassified and polled by the wrong poller.
            context.set("scope_barrier_wait", "true");
            Some(crate::engine::transition::StepOutcome::Wait)
        }
        Ok(ScopeBarrierResult::Allow) => None,
        Ok(ScopeBarrierResult::Denied(decision)) => {
            tracing::error!(run_id = %context.run_id(), %decision, "scope expansion denied");
            Some(crate::engine::transition::StepOutcome::Fatal)
        }
        Err(err) => {
            // Fail closed: a barrier persistence/measurement error means the
            // patch cannot be safely classified. Return Fatal rather than
            // allowing the mutation through unguarded.
            tracing::error!(
                run_id = %context.run_id(),
                error = %err,
                "scope-control barrier failed closed"
            );
            Some(crate::engine::transition::StepOutcome::Fatal)
        }
    }
}

/// Resolve the active scope-control config, distinguishing an absent policy
/// from malformed trusted context so mutation barriers cannot fail open.
fn resolve_scope_control_policy(
    context: &crate::engine::executor::StepContext,
) -> Result<Option<crate::workflow::schema::ScopeControlConfig>, serde_json::Error> {
    let Some(policy_json) = context.get("scope_control_policy") else {
        return Ok(None);
    };
    let config = serde_json::from_str::<crate::workflow::schema::ScopeControlConfig>(policy_json)?;
    Ok(config.enabled.then_some(config))
}
