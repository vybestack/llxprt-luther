//! Merge-wait executor: suspends a merge-required run until the PR is merged.
//!
//! This executor is the **production merge-wait step** that makes the typed
//! merge completion path reachable. It uses the existing `wait_for_merge` step
//! ID → `WaitKind::PrMerge` mapping so the daemon poller polls for PR merge.
//!
//! Flow:
//! 1. The executor checks whether the PR is merged via an injected merge probe.
//! 2. If NOT merged → returns [`StepOutcome::Wait`], which the engine turns
//!    into `WaitingExternal` with `WaitKind::PrMerge`.
//! 3. The daemon poller polls for merge; when merged → `ReadyToResume`.
//! 4. On resume, the executor runs again; this time the probe reports merged
//!    → returns [`StepOutcome::Success`].
//! 5. Remaining steps complete → the runner writes `ReviewReady` (not
//!    `Completed`, because `merge_required=true`).
//! 6. A post-completion orchestration detects `ReviewReady` + `merge_required`
//!    and invokes `complete_typed_merge` to atomically reach `Merged`.
//!
//! The executor does NOT write `Merged` itself — that is the job of
//! [`complete_typed_merge`][crate::engine::recovery::typed_merge::complete_typed_merge],
//! which requires `ReviewReady` as the predecessor. The executor merely gates
//! the wait; the typed completion happens after the runner writes
//! `ReviewReady`.
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
//! @requirement:REQ-RP-010

use crate::engine::executor::{StepContext, StepExecutor};
use crate::engine::recovery::typed_merge::{MergeError, MergeRemoteProbe};
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;

/// A merge-wait probe: checks whether a PR is merged without computing
/// reachability evidence. The full typed merge verification happens later in
/// [`complete_typed_merge`][crate::engine::recovery::typed_merge::complete_typed_merge].
///
/// This is a subset of [`MergeRemoteProbe`] that only needs the `merged` flag.
/// Production uses [`SystemMergeWaitProbe`]; tests inject a stub.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
pub trait MergeWaitProbe: Send + Sync {
    /// Returns `true` if the PR is observed as merged.
    ///
    /// # Errors
    /// Returns [`MergeError`] on probe failure (network, parse, etc.).
    fn is_merged(&self, repo: &str, pr_number: i64) -> Result<bool, MergeError>;
}

/// Adapter that wraps a [`MergeRemoteProbe`] as a [`MergeWaitProbe`].
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
pub struct RemoteProbeMergeWaitAdapter {
    probe: Box<dyn MergeRemoteProbe>,
}

impl RemoteProbeMergeWaitAdapter {
    /// Create a merge-wait probe backed by a full [`MergeRemoteProbe`].
    #[must_use]
    pub fn new(probe: Box<dyn MergeRemoteProbe>) -> Self {
        Self { probe }
    }
}

impl MergeWaitProbe for RemoteProbeMergeWaitAdapter {
    fn is_merged(&self, repo: &str, pr_number: i64) -> Result<bool, MergeError> {
        Ok(self.probe.observe_merge(repo, pr_number)?.merged)
    }
}

/// Merge-wait executor that suspends a merge-required run until the PR is
/// merged.
///
/// Uses the `wait_for_merge` step ID so the daemon poller classifies the wait
/// as `WaitKind::PrMerge` and polls for merge.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
pub struct MergeWaitExecutor {
    probe: Box<dyn MergeWaitProbe>,
}

impl MergeWaitExecutor {
    /// Create a merge-wait executor with a custom probe (for tests).
    #[must_use]
    pub fn new(probe: Box<dyn MergeWaitProbe>) -> Self {
        Self { probe }
    }

    /// Resolve the repo and PR number from the step context.
    fn resolve_identity(context: &StepContext) -> Result<(String, i64), EngineError> {
        let repo = context
            .get("target_repo")
            .or_else(|| context.get("repository"))
            .ok_or_else(|| EngineError::StepExecutionError {
                step_id: step_id_from_context(context),
                message: "merge_wait: missing 'target_repo'/'repository' in context".to_string(),
            })?;
        let pr_str = context
            .get("pr_number")
            .ok_or_else(|| EngineError::StepExecutionError {
                step_id: step_id_from_context(context),
                message: "merge_wait: missing 'pr_number' in context".to_string(),
            })?;
        let pr_number: i64 = pr_str
            .parse()
            .map_err(|_| EngineError::StepExecutionError {
                step_id: step_id_from_context(context),
                message: format!("merge_wait: invalid pr_number '{pr_str}' (expected integer)"),
            })?;
        Ok((repo.clone(), pr_number))
    }
}

/// Helper to extract the current step ID from the context.
fn step_id_from_context(context: &StepContext) -> String {
    context.get("current_step_id").cloned().unwrap_or_default()
}

impl StepExecutor for MergeWaitExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        _params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        let (repo, pr_number) = Self::resolve_identity(context)?;
        match self.probe.is_merged(&repo, pr_number) {
            Ok(true) => {
                // PR is merged → step succeeds. The runner will write
                // ReviewReady after all steps complete. The typed merge
                // completion orchestrator then transitions ReviewReady → Merged.
                Ok(StepOutcome::Success)
            }
            Ok(false) => {
                // PR is NOT merged → suspend as WaitingExternal (PrMerge).
                // The daemon poller will poll for merge and resume when ready.
                Ok(StepOutcome::Wait)
            }
            Err(e) => {
                // Probe failure → fail closed (Fatal), NOT fake success.
                // The run remains in a durable state (ReviewReady if it was
                // already reached, or the step's prior checkpoint) with a
                // clear diagnostic.
                Err(EngineError::StepExecutionError {
                    step_id: step_id_from_context(context),
                    message: format!(
                        "merge_wait: probe failed for repo='{repo}' pr={pr_number}: {e}"
                    ),
                })
            }
        }
    }
}
