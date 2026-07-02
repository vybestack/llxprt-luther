/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
/// Workflow module - schema definitions and config loading.
pub mod command_manifest;
pub mod config_loader;
pub mod schema;
pub mod target_paths;
pub mod target_profile;
pub mod validation;

pub use command_manifest::{
    ArtifactExpectation, ArtifactExpectations, ArtifactKind, CapturePolicy, CommandEntry,
    CommandManifest, FailureOutcome, RetryPolicy, StreamExpectations,
};
pub use config_loader::{
    build_available_variables, parse_workflow_config_json, parse_workflow_config_toml,
    parse_workflow_type_json, parse_workflow_type_toml, resolve_workflow, resolve_workflow_config,
    resolve_workflow_type, validate_artifact_dependencies, validate_config_matches_type,
    validate_step_tokens, validate_workflow_config, validate_workflow_tokens,
    validate_workflow_type, ConfigError, ConfigErrorKind, MissingArtifactProducer,
    Result as ConfigResult, UnresolvedToken,
};
pub use schema::{
    DiffPathNormalization, GuardConfig, GuardLimits, RepoConfig, RuntimeConfig, StepDef,
    TransitionDef, WorkflowConfig, WorkflowRunRef, WorkflowType,
};
pub use target_paths::TargetPathConfig;
pub use target_profile::{
    apply_target_profile_overrides, resolve_target_profile, target_profile_validation_required,
    validate_target_profile, TargetProfileOverrides,
};
pub use validation::{
    compute_reachable_steps, validate_workflow_graph, GraphErrorCategory, GraphValidationError,
};
