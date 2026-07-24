//! V1 capsule adapter (schema version 1). [C8/B9]
//!
//! Implements [`super::CapsuleAdapter`] for `ExecutionCapsuleV1` capsules with
//! `schema_version == 1`. `version`, `envelope_digest`, `step_def_for`
//! (read-only canonical decoding required by P11 `prepare`), and
//! `build_instance` (capsule-driven `WorkflowInstance` reconstruction) are
//! implemented. The production
//! [`RecoveryExecutor`][crate::engine::recovery::RecoveryExecutor] wiring lives
//! in [`crate::engine::recovery::wiring`].
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14
//! @requirement:REQ-RP-009

use super::{AdapterError, CapsuleAdapter};
use crate::engine::instance::WorkflowInstance;
use crate::engine::recovery::capsule::ExecutionCapsuleV1;
use crate::workflow::schema::{StepDef, WorkflowConfig, WorkflowType};

/// Adapter for `ExecutionCapsuleV1` (schema version 1). [C8/B9]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08
/// @requirement:REQ-RP-009
#[derive(Debug, Clone, Copy, Default)]
pub struct V1Adapter;

impl CapsuleAdapter for V1Adapter {
    /// Returns `1` (object-safe version selector). [C8]
    fn version(&self) -> u32 {
        1
    }

    /// Resolve the canonical `StepDef` for a step id from a V1 capsule. [C8]
    ///
    /// Deserializes the capsule's `resolved_workflow_bytes` into a canonical
    /// `WorkflowType` (the same canonical form persisted by
    /// [`canonicalize_workflow_type`]) and returns the matching `StepDef`.
    /// This is a read-only canonical decoding that `prepare` needs to resolve
    /// the step recovery policy.
    ///
    /// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
    /// @requirement:REQ-RP-009
    fn step_def_for(
        &self,
        capsule: &ExecutionCapsuleV1,
        step_id: &str,
    ) -> Result<StepDef, AdapterError> {
        let workflow: WorkflowType = serde_json::from_slice(&capsule.resolved_workflow_bytes)
            .map_err(|e| AdapterError::Deserialization(e.to_string()))?;
        workflow
            .steps
            .into_iter()
            .find(|step| step.step_id == step_id)
            .ok_or_else(|| AdapterError::StepNotFound {
                step_id: step_id.to_string(),
            })
    }

    /// Reconstruct a `WorkflowInstance` from the immutable capsule bytes. [C8]
    ///
    /// Deserializes the capsule's `resolved_workflow_bytes` and
    /// `resolved_config_bytes` (the canonical forms persisted by
    /// [`canonicalize_workflow_type`] and [`canonicalize_workflow_config`])
    /// and binds them into a `WorkflowInstance` carrying the capsule's exact
    /// `run_id`. The instance starts at the workflow's first step; the
    /// recovery executor transitions it to the reserved step before
    /// execution.
    ///
    /// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P14
    /// @requirement:REQ-RP-009
    fn build_instance(
        &self,
        capsule: &ExecutionCapsuleV1,
    ) -> Result<WorkflowInstance, AdapterError> {
        let workflow: WorkflowType = serde_json::from_slice(&capsule.resolved_workflow_bytes)
            .map_err(|e| AdapterError::Deserialization(e.to_string()))?;
        let config: WorkflowConfig = serde_json::from_slice(&capsule.resolved_config_bytes)
            .map_err(|e| AdapterError::Deserialization(e.to_string()))?;
        Ok(WorkflowInstance::create_with_run_id(
            workflow,
            config,
            &capsule.run_id,
        ))
    }

    /// Borrow the envelope digest (THE authority) from a V1 capsule. [C8]
    fn envelope_digest<'a>(&'a self, capsule: &'a ExecutionCapsuleV1) -> &'a str {
        &capsule.envelope_digest
    }
}
