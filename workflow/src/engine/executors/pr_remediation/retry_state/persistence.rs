//! Durable retry-state writes and transition identity generation.

use super::*;

pub(in crate::engine::executors::pr_remediation) fn persist(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    producer_step_id: &str,
    step_order: u64,
    state: &RetryState,
    clock: &dyn ClockSleeper,
) -> Result<(), EngineError> {
    store.write_json_artifact_locked(JsonArtifactWriteRequest::new(
        ArtifactWriteContext::new(
            binding,
            RETRY_STATE_FAMILY,
            producer_step_id,
            step_order,
            clock,
        ),
        state,
        None,
    ))?;
    Ok(())
}

pub(super) fn launch_transition_id(
    binding: &PrFollowupBinding,
    plan: &Value,
    ordinal: u64,
) -> String {
    let identity = format!(
        "{}:{}/{}:{}:{}:{}:launch:{ordinal}",
        binding.run_id,
        binding.repository_owner,
        binding.repository_name,
        binding.pr_number,
        binding.head_sha,
        plan.get("artifact_sequence")
            .and_then(Value::as_u64)
            .unwrap_or_default()
    );
    format!("fnv64:{:016x}", fnv64(identity.as_bytes()))
}

pub(in crate::engine::executors::pr_remediation) fn fnv64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}
