//! Default timeout resolution for the feedback evaluator command runner.
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
//! @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017

use std::sync::OnceLock;
use std::time::Duration;

use super::feedback_eval::DEFAULT_FEEDBACK_EVALUATOR_TIMEOUT_SECONDS;

/// Environment variable that overrides the evaluator command timeout so
/// operators can accommodate slower reasoning profiles without a rebuild.
pub const FEEDBACK_EVALUATOR_TIMEOUT_ENV: &str = "LUTHER_FEEDBACK_EVALUATOR_TIMEOUT_SECONDS";

static CACHED_EVALUATOR_TIMEOUT: OnceLock<Duration> = OnceLock::new();

/// Resolve the default evaluator timeout, honoring the environment
/// override. Invalid or non-positive values fall back to the built-in
/// default. The value is resolved once and cached so later environment
/// mutations cannot change behavior mid-process or race concurrent reads.
pub(super) fn default_evaluator_timeout() -> Duration {
    *CACHED_EVALUATOR_TIMEOUT.get_or_init(resolve_evaluator_timeout)
}

fn resolve_evaluator_timeout() -> Duration {
    Duration::from_secs(
        env_override_seconds().unwrap_or(DEFAULT_FEEDBACK_EVALUATOR_TIMEOUT_SECONDS),
    )
}

fn env_override_seconds() -> Option<u64> {
    std::env::var(FEEDBACK_EVALUATOR_TIMEOUT_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Env-var tests mutate process-global state; keep every assertion in
    // one test so they cannot race each other under the parallel test
    // runner, and restore the variable before returning. The cached
    // accessor is intentionally exercised only for the no-override case;
    // override parsing is covered against the uncached resolver.
    #[test]
    fn evaluator_timeout_resolution_honors_env_override_and_rejects_invalid() {
        std::env::remove_var(FEEDBACK_EVALUATOR_TIMEOUT_ENV);
        assert_eq!(
            resolve_evaluator_timeout(),
            Duration::from_secs(DEFAULT_FEEDBACK_EVALUATOR_TIMEOUT_SECONDS)
        );
        assert_eq!(
            default_evaluator_timeout(),
            Duration::from_secs(DEFAULT_FEEDBACK_EVALUATOR_TIMEOUT_SECONDS)
        );

        std::env::set_var(FEEDBACK_EVALUATOR_TIMEOUT_ENV, "1800");
        assert_eq!(resolve_evaluator_timeout(), Duration::from_secs(1800));

        std::env::set_var(FEEDBACK_EVALUATOR_TIMEOUT_ENV, "0");
        assert_eq!(
            resolve_evaluator_timeout(),
            Duration::from_secs(DEFAULT_FEEDBACK_EVALUATOR_TIMEOUT_SECONDS)
        );

        std::env::set_var(FEEDBACK_EVALUATOR_TIMEOUT_ENV, "not-a-number");
        assert_eq!(
            resolve_evaluator_timeout(),
            Duration::from_secs(DEFAULT_FEEDBACK_EVALUATOR_TIMEOUT_SECONDS)
        );

        std::env::remove_var(FEEDBACK_EVALUATOR_TIMEOUT_ENV);
    }
}
