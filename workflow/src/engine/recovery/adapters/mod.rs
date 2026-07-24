//! Object-safe, versioned capsule execution adapters. [C8/B9]
//!
//! The [`CapsuleAdapter`] trait uses `fn version(&self) -> u32` (not `const
//! VERSION`) so it is object-safe and supports `dyn CapsuleAdapter` dispatch.
//! [C8] Dispatch is fail-closed: [`adapter_for`] rejects an unsupported
//! capsule schema version before any step executes. [B9]
//!
//! Method behavior: `step_def_for` is implemented in P11 (read-only canonical
//! decoding required by `prepare`); `build_instance` is owned by P12/P14
//! (capsule adapter wiring).
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
//! @requirement:REQ-RP-009

use thiserror::Error;

use super::capsule::ExecutionCapsuleV1;
use crate::engine::instance::WorkflowInstance;
use crate::workflow::schema::StepDef;

pub mod v1;

pub use v1::V1Adapter;

/// Object-safe, versioned capsule execution adapter. [C8/B9]
///
/// Uses `fn version(&self) -> u32` (not `const VERSION`) so the trait is
/// object-safe and supports `dyn CapsuleAdapter` dispatch. [C8]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08
/// @requirement:REQ-RP-009
pub trait CapsuleAdapter {
    /// The capsule schema version this adapter handles (object-safe). [C8]
    fn version(&self) -> u32;
    /// Resolve the canonical `StepDef` for a step id from the capsule. [C8]
    fn step_def_for(
        &self,
        capsule: &ExecutionCapsuleV1,
        step_id: &str,
    ) -> Result<StepDef, AdapterError>;
    /// Build a `WorkflowInstance` from the capsule. [C8]
    fn build_instance(
        &self,
        capsule: &ExecutionCapsuleV1,
    ) -> Result<WorkflowInstance, AdapterError>;
    /// Borrow the envelope digest (THE authority) from the capsule. [C8]
    fn envelope_digest<'a>(&'a self, capsule: &'a ExecutionCapsuleV1) -> &'a str;
}

/// Dispatch the object-safe adapter for a capsule's schema version. [C8/B9]
///
/// Fail-closed: an unsupported capsule schema version is rejected before any
/// step executes (adapters pseudocode lines 10–15). [B9]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08
/// @requirement:REQ-RP-009
pub fn adapter_for(capsule: &ExecutionCapsuleV1) -> Result<Box<dyn CapsuleAdapter>, AdapterError> {
    match capsule.schema_version {
        1 => Ok(Box::new(V1Adapter)),
        v => Err(AdapterError::UnsupportedCapsuleVersion(v)),
    }
}

/// Errors produced by capsule adapter operations. [C8/B9]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P08
/// @requirement:REQ-RP-009
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AdapterError {
    /// The capsule schema version has no registered adapter. [B9]
    #[error("unsupported capsule version: {0}")]
    UnsupportedCapsuleVersion(u32),
    /// The requested step id was not found in the capsule's workflow. [C8]
    #[error("step not found in capsule: {step_id}")]
    StepNotFound {
        /// The step id that was not found.
        step_id: String,
    },
    /// The capsule's resolved workflow bytes could not be deserialized into a
    /// canonical `WorkflowType`. [C8]
    #[error("capsule workflow deserialization error: {0}")]
    Deserialization(String),
}
