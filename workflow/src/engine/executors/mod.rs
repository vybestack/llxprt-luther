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
pub mod change_detection;
pub mod command_manifest;
pub mod feedback_eval;
pub mod github_feedback;
pub mod github_pr;
pub mod llxprt;
pub mod noop;
pub mod pr_check_wait;
pub mod pr_followup_artifacts;
pub mod pr_followup_types;
pub mod pr_remediation;
pub mod shell;
pub mod verify;
pub mod workflow_auth_preflight;
pub mod write_file;

// Re-export executor implementations for tests
pub use feedback_eval::{
    default_feedback_evaluator_argv, CommandFeedbackEvaluationAdapter, FeedbackEvaluationAdapter,
    FeedbackEvaluationRequest, FeedbackEvaluationResponse, FeedbackEvaluatorCommandRunner,
    FeedbackEvaluatorExecutor, ProcessFeedbackEvaluatorCommandRunner,
};

pub use change_detection::{ChangeDetectionMode, ChangedPathDetector, GitChangedPathDetector};
pub use command_manifest::{
    request_from_entry, run_manifest_command, ManifestCommandRequest, ManifestCommandResult,
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
pub use llxprt::{LlxprtExecutor, LlxprtExecutorWithDetector};
pub use noop::NoOpExecutor;
pub use pr_followup_artifacts::{
    ArtifactWriter, ClockSleeper, PrFollowupArtifactStore, PrFollowupFilesystem,
    SystemClockSleeper, SystemPrFollowupFilesystem,
};
pub use pr_followup_types::{
    ArtifactSequenceMetadata, CiFailures, CodeRabbitFeedback, CollectionState, EvaluationState,
    FeedbackEvaluations, FeedbackMarkerReport, FeedbackState, OverallState, PlanState,
    PostPrFailureTerminal, PostPrIterationGuard, PostPrTestResult, PrCheckStatus,
    PrFollowupBinding, PrIdentity, PrRemediationPlan, PrRemediationResult, PushRemediationResult,
    ValidationState, PR_FOLLOWUP_SCHEMA_VERSION,
};
pub use pr_remediation::{
    LlxprtInvocationRequest, LlxprtInvocationResult, PostPrFailureTerminalExecutor,
    PostPrIterationGuardExecutor, PostPrTestCommandRequest, PostPrTestCommandResult,
    PostPrTestCommandRunner, PrFollowupLlxprtCommandRunner, PrFollowupRemediationExecutor,
    PrFollowupRemediationExecutorWithRunner, PrRemediationPlanExecutor,
    PrRemediationResultExecutor, PushRemediationChangesExecutor,
    PushRemediationChangesExecutorWithRunner, PushRemediationCommandRequest,
    PushRemediationCommandResult, PushRemediationCommandRunner, RunPostPrTestsExecutor,
    RunPostPrTestsExecutorWithRunner, SystemPrFollowupLlxprtCommandRunner,
};
pub use shell::ShellExecutor;
pub use verify::VerifyExecutor;
pub use workflow_auth_preflight::WorkflowAuthPreflightExecutor;
pub use write_file::WriteFileExecutor;
