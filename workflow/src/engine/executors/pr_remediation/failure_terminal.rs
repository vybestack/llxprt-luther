//! Post-PR terminal failure projection.

use std::fs;

use super::retry_state::{load_current_state, RetryScopeKey, RetryState, RETRY_STATE_FAMILY};
use super::{artifact_root, binding_for_context, current_step_id, u64_param};
use crate::engine::executor::{StepContext, StepExecutor};
use crate::engine::executors::pr_followup_artifacts::{
    ArtifactWriter, PrFollowupArtifactStore, SystemClockSleeper, SystemPrFollowupFilesystem,
};
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;
use serde_json::{json, Value};

#[derive(Debug, Default)]
pub struct PostPrFailureTerminalExecutor;

impl StepExecutor for PostPrFailureTerminalExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        params: &Value,
    ) -> Result<StepOutcome, EngineError> {
        let clock = SystemClockSleeper;
        let artifact_root = artifact_root(context, params)?;
        let store =
            PrFollowupArtifactStore::with_filesystem(&artifact_root, &SystemPrFollowupFilesystem)?;
        let binding = binding_for_context(context, params, &store, &clock)?;
        let retry_state = matching_retry_state(&store, &binding)?;
        let payload = terminal_payload(retry_state.as_ref());
        let step_id = current_step_id(context, "post_pr_failure_terminal");
        let reason = payload["terminal_reason"]
            .as_str()
            .unwrap_or("post_pr_failure");

        store.write_json_artifact(
            &binding,
            "post-pr-failure-terminal",
            &step_id,
            u64_param(params, "step_order_index", 13),
            &payload,
            Some(("post_pr_failure_terminal", reason, payload.clone())),
            &clock,
        )?;
        Ok(StepOutcome::Fatal)
    }
}

fn matching_retry_state(
    store: &PrFollowupArtifactStore,
    binding: &crate::engine::executors::pr_followup_types::PrFollowupBinding,
) -> Result<Option<RetryState>, EngineError> {
    if !store
        .canonical_path(binding, "pr-remediation-plan")
        .exists()
    {
        return Ok(None);
    }
    let plan = store.read_current_json(binding, "pr-remediation-plan")?;
    let scope = RetryScopeKey::new(binding, &plan)?;
    // The terminal step must always be able to record the failure artifact.
    // A corrupt retry-state file would poison the store's sequence-recovery
    // scan and block the terminal write, so quarantine it before proceeding.
    // The corrupt state is already useless for accounting; quarantine
    // preserves the evidence for diagnosis while unblocking the write.
    match load_current_state(store, binding, &scope) {
        Ok(state) => Ok(state),
        Err(_) => {
            quarantine_corrupt_retry_state(store, binding)?;
            Ok(None)
        }
    }
}

fn quarantine_corrupt_retry_state(
    store: &PrFollowupArtifactStore,
    binding: &crate::engine::executors::pr_followup_types::PrFollowupBinding,
) -> Result<(), EngineError> {
    let path = store.canonical_path(binding, RETRY_STATE_FAMILY);
    if path.exists() {
        let quarantined = path.with_extension("json.corrupt");
        fs::rename(&path, &quarantined).map_err(|error| {
            EngineError::InvalidState(format!("quarantine corrupt retry state: {error}"))
        })?;
    }
    Ok(())
}

fn terminal_payload(state: Option<&RetryState>) -> Value {
    let reason = state
        .and_then(exhausted_budget)
        .unwrap_or("post_pr_failure");
    json!({
        "terminal_state": "failed",
        "terminal_reason": reason,
        "exhausted_budget": (reason != "post_pr_failure").then_some(reason),
        "remediation_attempt_index": state.map(|state| state.counters.remediation_attempt_index),
        "max_remediation_attempts": state.map(|state| state.budget.max_remediation_attempts),
        "validation_retry_index": state.map(|state| state.counters.validation_retry_index),
        "max_validation_retries": state.map(|state| state.budget.max_validation_retries),
        "stale_artifact_retry_index": state.map(|state| state.counters.stale_artifact_retry_index),
        "max_stale_artifact_retries": state.map(|state| state.budget.max_stale_artifact_retries),
        "retry_transition_id": state.map(|state| state.transition_id.as_str()),
        "retry_launch_phase": state.map(|state| state.launch_phase),
    })
}

fn exhausted_budget(state: &RetryState) -> Option<&'static str> {
    if state.counters.remediation_attempt_index >= state.budget.max_remediation_attempts {
        Some("remediation_attempts_exhausted")
    } else if state.counters.validation_retry_index >= state.budget.max_validation_retries {
        Some("validation_retries_exhausted")
    } else if state.counters.stale_artifact_retry_index >= state.budget.max_stale_artifact_retries {
        Some("stale_artifact_retries_exhausted")
    } else {
        None
    }
}
