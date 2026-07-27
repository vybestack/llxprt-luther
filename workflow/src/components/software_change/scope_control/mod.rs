//! Scope-control domain (issue 142).
//!
//! This module implements the task-charter configuration, validation, model,
//! persistence, and executor registration for Luther's scope-control system.
//! A canonical task charter is created before a mutating implementation step.
//! The charter binds acceptance-criterion IDs, normalized path prefixes,
//! non-goals, file/line/module/dependency/API ceilings, mandatory gates, review
//! limits, and an immutable merge base.
//!
//! Slice 1: config schema + validation + canonical model + digest +
//! atomic immutable persistence + executor registration.
//!
//! Slice 2: deterministic patch measurement and scope evaluation. Measures
//! changed files, added lines (with documented binary semantics), new source
//! modules, dependencies added, and added public APIs against the charter's
//! frozen merge base. Produces stable violation codes and compares every metric
//! plus changed paths/subsystems to charter ceilings. Updates the status read
//! model crash-safely and exposes patch growth/head/divergence through context
//! outputs.
//!
//! @plan:PLAN-20260715-SCOPE-CONTROL

pub mod config_validation;
pub mod decision;
pub mod evaluation;
pub mod finding_disposition;
pub mod measurement;
pub mod model;
pub mod persistence;
pub mod review_state;
pub mod scope_measure;
pub mod status_projection;
pub mod task_charter;
pub mod timeout_recovery;

pub use config_validation::validate_scope_control;
pub use decision::{
    build_expansion_request, check_scope_gate, enforce_scope_barrier, measurement_digest,
    read_expansion_request, read_expansion_resolution, write_expansion_request,
    write_expansion_resolution, ScopeBarrierResult, ScopeExpansionDecision, ScopeExpansionRequest,
    ScopeExpansionResolution, ScopeGateOutcome, EXPANSION_REQUEST_FILENAME,
    EXPANSION_RESOLUTION_FILENAME,
};
pub use evaluation::{evaluate, ScopeEvaluation, Violation, ViolationCode};
pub use finding_disposition::{
    disposition_action, is_mandatory_gate, project_legacy_decision, DispositionAction,
    FindingCorrectness, FindingDeliveryScope, FindingDisposition,
};
pub use measurement::{
    collect_dependency_diffs, compute_measurement, file_change_from_path, test_measurement_config,
    total_added_lines, ChangeStatus, FileChange, GitPatchCollector, GitPatchData, MeasurementError,
    PatchMeasurement, SystemGitPatchCollector,
};
pub use model::{
    normalize_charter, validate_draft_against_config, CanonicalBudget, CanonicalReviewCaps,
    CanonicalTaskCharter, DraftBudget, DraftReviewCaps, DraftSubsystem, TaskCharterDraft,
    CHARTER_SCHEMA_VERSION,
};
pub use persistence::{
    charter_path, persist_charter_and_status, read_json, scope_control_dir, status_path,
    update_status_measurement, write_immutable_json, write_updatable_json,
    PersistenceError as ScopePersistenceError, ScopeStatus, CHARTER_FILENAME, SCOPE_CONTROL_DIR,
    STATUS_FILENAME,
};
pub use review_state::{
    check_delta, check_final, check_initial, count_by_kind, filter_changed_tests,
    last_reviewed_head, pre_launch_review_gate, read_exhaustion_summary, read_review_history,
    record_final_acceptance, record_review, write_exhaustion_summary, write_review_history,
    PreLaunchReviewRequest, ReviewCheckOutcome, ReviewExhaustionRouting, ReviewExhaustionSummary,
    ReviewHistory, ReviewKind, ReviewScope, EXHAUSTION_SUMMARY_FILENAME, REVIEW_HISTORY_FILENAME,
};
pub use scope_measure::ScopeMeasureExecutor;
pub use status_projection::{
    project_scope_status, scope_status_to_human, scope_status_to_json, PatchGrowthProjection,
    PatchProjection, ReviewPhase, ReviewProjection, ScopeControlProjection, ScopeControlStatus,
    ScopeDecisionProjection, ScopeDecisionState, ScopeResolutionProjection,
    TimeoutRecoveryProjection,
};
pub use task_charter::{MergeBaseError, MergeBaseProbe, SystemMergeBaseProbe, TaskCharterExecutor};
pub use timeout_recovery::{
    handle_timeout_recovery, read_timeout_snapshot, TimeoutRecoveryStatus, TimeoutSnapshot,
    TIMEOUT_SNAPSHOT_FILENAME,
};
