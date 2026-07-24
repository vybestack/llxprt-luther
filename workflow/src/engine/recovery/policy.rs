//! Step recovery policy: selects the recovery strategy for a canonical step.
//!
//! Each canonical [`StepDef`](crate::workflow::schema::StepDef) may carry an
//! explicit [`StepRecoveryPolicy`]. When present it takes precedence over the
//! `SAFE_RERUN_STEPS` classification. The policy is persisted in the canonical
//! workflow bytes (via [`canonicalize_workflow_type`](crate::persistence::launch_provenance::compute_workflow_type))
//! so the execution capsule envelope digest covers it. [C6/B7]
//!
//! This module owns the policy type, its serialization, and the concrete
//! strategy selection ([`policy_for_step`] / [`select_strategy`]) per the
//! locked P02 pseudocode. It re-exports [`RecoveryStrategy`] /
//! [`RefusalReason`] from [`super::protocol`] so there are no duplicate type
//! definitions. [C4]
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P06
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
//! @requirement:REQ-RP-005

use crate::engine::continuation::is_safe_rerun_step;
use crate::workflow::schema::StepDef;

pub use super::protocol::{RecoveryStrategy, RefusalReason};

/// Recovery policy declared on a canonical step definition.
///
/// When present on a [`StepDef`], takes precedence over the `SAFE_RERUN_STEPS`
/// classification. Persisted in canonical workflow bytes so the capsule
/// envelope digest covers it. [C6/B7]
///
/// The serialization is `snake_case` so it round-trips through TOML/JSON
/// workflow definitions and the canonical envelope. Unknown variants are
/// rejected at deserialization by `serde` (validation in P06 only needs the
/// type to exist; unknown-variant rejection is enforced by the derived
/// `Deserialize`).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P06
/// @requirement:REQ-RP-005
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepRecoveryPolicy {
    /// Safe to re-run from scratch (no side effects).
    PureReenter,
    /// Re-running yields identical effects.
    Idempotent,
    /// Reconcile observed state, then re-enter.
    ReconcileThenReenter,
    /// Resume in-place after exact verification.
    ContinueWorkspace,
    /// Undo prior partial effect, then retry.
    CompensateThenRetry,
    /// Fail closed; no recovery possible.
    NonRecoverable,
}

/// Resolve the policy for a canonical step. [C6/B7]
///
/// Resolution order (locked P02 pseudocode):
/// 1. An explicit [`StepRecoveryPolicy`] on the [`StepDef`] takes precedence.
/// 2. Otherwise, a `step_id` in `SAFE_RERUN_STEPS` (the canonical
///    classification from [`crate::engine::continuation`]) resolves to
///    [`StepRecoveryPolicy::Idempotent`].
/// 3. Everything else — generic `shell`/`write_file` and unknown step types —
///    defaults to [`StepRecoveryPolicy::NonRecoverable`] (fail closed). [C6]
///
/// The `step_id` is accepted explicitly because the caller resolves the
/// target step from the durable run state; the `StepDef` carries the canonical
/// declaration (including the optional `recovery_policy`). [C6/B7]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P06
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
/// @requirement:REQ-RP-005
pub fn policy_for_step(step_def: &StepDef, step_id: &str) -> StepRecoveryPolicy {
    if let Some(declared) = &step_def.recovery_policy {
        return declared.clone();
    }
    if is_safe_rerun_step(step_id) {
        return StepRecoveryPolicy::Idempotent;
    }
    StepRecoveryPolicy::NonRecoverable
}

/// Select the concrete recovery strategy from a policy. [C4/C6]
///
/// Maps each [`StepRecoveryPolicy`] to its [`RecoveryStrategy`] per the locked
/// P02 pseudocode. [`StepRecoveryPolicy::NonRecoverable`] is refused
/// ([`RecoveryStrategy::Refused`]`(`[`RefusalReason::NonRecoverable`]`)`).
/// Authorization is handled by the sealed `RecoveryAuthority`
/// (descriptor-bound `WorkspaceAuthorization`, verified during prepare and
/// revalidated during reserve), so there is no authorization parameter. [C4]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P06
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P11
/// @requirement:REQ-RP-005
pub fn select_strategy(policy: StepRecoveryPolicy) -> RecoveryStrategy {
    match policy {
        StepRecoveryPolicy::PureReenter => RecoveryStrategy::Reenter,
        StepRecoveryPolicy::Idempotent => RecoveryStrategy::Reenter,
        StepRecoveryPolicy::ReconcileThenReenter => RecoveryStrategy::ReconcileThenReenter,
        StepRecoveryPolicy::ContinueWorkspace => RecoveryStrategy::ContinueWorkspace,
        StepRecoveryPolicy::CompensateThenRetry => RecoveryStrategy::CompensateThenRetry,
        StepRecoveryPolicy::NonRecoverable => {
            RecoveryStrategy::Refused(RefusalReason::NonRecoverable)
        }
    }
}
