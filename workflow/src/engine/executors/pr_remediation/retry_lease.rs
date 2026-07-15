//! Retry launch lease duration and expiry policy.

use crate::engine::executors::pr_followup_artifacts::ClockSleeper;
use crate::engine::runner::EngineError;

use super::retry_state::{LaunchPhase, RetryState};

/// Grace beyond the configured runner timeout before an active launch can be
/// reclaimed. The runner timeout is part of durable state, so a second process
/// cannot steal an ordinal from an invocation that is still allowed to run.
const LEASE_GRACE_SECONDS: u64 = 300;
pub(super) const DEFAULT_INVOCATION_TIMEOUT_SECONDS: u64 = 1800;

/// Returns an RFC3339 timestamp representing the lease expiry for a launch
/// reserved at the current clock time.
pub(super) fn lease_expiry_from_now(
    clock: &dyn ClockSleeper,
    invocation_timeout_seconds: u64,
) -> Result<String, EngineError> {
    let now = chrono::DateTime::parse_from_rfc3339(&clock.now_rfc3339()).map_err(|error| {
        EngineError::InvalidState(format!("retry lease clock is not RFC3339: {error}"))
    })?;
    let lease_seconds = invocation_timeout_seconds
        .checked_add(LEASE_GRACE_SECONDS)
        .ok_or_else(|| EngineError::InvalidState("retry lease duration overflowed".to_string()))?;
    let lease_seconds = i64::try_from(lease_seconds).map_err(|_| {
        EngineError::InvalidState("retry lease duration exceeds chrono range".to_string())
    })?;
    let expiry = now
        .checked_add_signed(chrono::Duration::seconds(lease_seconds))
        .ok_or_else(|| EngineError::InvalidState("retry lease timestamp overflowed".to_string()))?;
    Ok(expiry.to_rfc3339())
}

/// Returns `true` if the state's active lease has expired (the owning process
/// crashed or stalled). A `Completed` state never has an active lease. Missing
/// or malformed timestamps fail closed instead of being treated as expired.
pub(super) fn is_lease_expired(
    state: &RetryState,
    clock: &dyn ClockSleeper,
) -> Result<bool, EngineError> {
    if state.launch_phase == LaunchPhase::Completed {
        return Ok(true);
    }
    let expiry_str = state.lease_expiry.as_deref().ok_or_else(|| {
        EngineError::InvalidState("active retry state is missing lease expiry".to_string())
    })?;
    let expiry = chrono::DateTime::parse_from_rfc3339(expiry_str).map_err(|error| {
        EngineError::InvalidState(format!("retry lease expiry is not RFC3339: {error}"))
    })?;
    let now = chrono::DateTime::parse_from_rfc3339(&clock.now_rfc3339()).map_err(|error| {
        EngineError::InvalidState(format!("retry lease clock is not RFC3339: {error}"))
    })?;
    Ok(now >= expiry)
}
