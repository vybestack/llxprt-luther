//! Execution capsule and recovery-protocol scaffolding. [C8]
//!
//! This module groups the immutable canonical execution capsule, the
//! object-safe versioned adapter registry, the step recovery policy, the
//! recovery protocol itself (phased prepare → reserve → execute → finalize),
//! and the production capsule-backed recovery wiring skeleton (P12). The
//! protocol is owned by P09–P11; the wiring skeleton is owned by P12.
//! [C5/C12]
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P06
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P09
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P12

/// Immutable canonical execution capsule with envelope digest. [C8/B9]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P06
/// @requirement:REQ-RP-002
pub mod capsule;

/// Object-safe, versioned capsule execution adapters. [C8/B9]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P06
/// @requirement:REQ-RP-009
pub mod adapters;

/// Step recovery policy: selects the recovery strategy for a canonical step.
/// [C6/B7]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P06
/// @requirement:REQ-RP-005
pub mod policy;

/// Single typed recovery abstraction owning the phased model. [C1/C2/C4/C5/C12]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P09
/// @requirement:REQ-RP-001
pub mod protocol;

/// Production capsule-backed recovery wiring skeleton. [C8/B8]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P12
/// @requirement:REQ-RP-009
pub mod wiring;

/// Legacy salvage lineage. [C9/B10]
///
/// Every run without a valid pre-execution V1 capsule is salvage-only.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P15
/// @requirement:REQ-RP-007
pub mod salvage;

/// Typed verified merge with strategy-specific reachability proof and atomic
/// artifact+status transaction. [C10/C11/B11/B12]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
pub mod typed_merge;

/// Typed merge completion orchestrator: production wiring that transitions a
/// `ReviewReady` merge-required run to `Merged` via the typed merge API.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
pub mod merge_completion;

pub use adapters::{adapter_for, AdapterError, CapsuleAdapter, V1Adapter};
pub use capsule::{
    build_capsule_v1, build_envelope_frame, verify_envelope_digest, CapsuleAuthorityFields,
    CapsuleAuthorityFieldsRef, CapsuleError, ExecutionCapsuleV1, CURRENT_CANONICALIZATION_VERSION,
    CURRENT_DOMAIN_VERSION, CURRENT_PROVENANCE_VERSION, CURRENT_SCHEMA_VERSION,
    SUPPORTED_CANONICALIZATION_VERSIONS, SUPPORTED_DOMAIN_VERSIONS, SUPPORTED_PROVENANCE_VERSIONS,
    SUPPORTED_SCHEMA_VERSIONS,
};
pub use policy::{policy_for_step, select_strategy, StepRecoveryPolicy};
pub use protocol::{
    normalize_operator_verb, CheckpointIdentity, NoOpRecoveryPhaseObserver, OperatorVerb,
    PreparedRecovery, RecoveryAuthority, RecoveryError, RecoveryExecutionError,
    RecoveryExecutionInvocation, RecoveryExecutionResult, RecoveryExecutor, RecoveryOutcome,
    RecoveryPhaseObserver, RecoveryProtocolV1, RecoveryRequest, RecoveryStrategy, RefusalReason,
    UnavailableRecoveryExecutor,
};
pub use wiring::{RecoveryWiring, RunnerRecoveryExecutor};

pub use salvage::{
    append_salvage_record, classify_run, init_salvage_lineage_table, salvage_recover,
    RunClassification, SalvageError, SALVAGE_LINEAGE_TABLE,
};

pub use typed_merge::{
    build_reachability_proof, complete_merge_from_observation, complete_typed_merge,
    completion_satisfied, init_merge_artifacts_table, load_merge_artifact_conn,
    runner_completion_for_merge_required, verify_capsule_binding, ALLOWED_MERGE_PREDECESSOR,
    MERGE_ARTIFACTS_TABLE,
};
pub use typed_merge::{
    MergeError, MergeGitProbe, MergeObservation, MergeReachabilityProof, MergeRemoteProbe,
    MergeStrategy, MergeVerifier, SystemMergeGitProbe, SystemMergeRemoteProbe, TypedMergeArtifact,
};

pub use merge_completion::{
    complete_merge_required_run, needs_merge_completion, resolve_declared_strategy,
    MergeCompletionOutcome, MergeProbeFactory, SystemMergeProbeFactory,
};
