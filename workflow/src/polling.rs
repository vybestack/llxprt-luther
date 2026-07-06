use chrono::{DateTime, Duration, Utc};

const MAX_POLL_INTERVAL_SECONDS: i64 = 86_400;

pub fn next_poll_time(poll_interval_seconds: u64) -> DateTime<Utc> {
    let seconds = bounded_poll_interval_seconds(poll_interval_seconds);
    Utc::now() + Duration::seconds(seconds)
}

fn bounded_poll_interval_seconds(poll_interval_seconds: u64) -> i64 {
    match i64::try_from(poll_interval_seconds) {
        Ok(seconds) => seconds.clamp(1, MAX_POLL_INTERVAL_SECONDS),
        Err(err) => {
            tracing::warn!(
                poll_interval_seconds,
                max_poll_interval_seconds = MAX_POLL_INTERVAL_SECONDS,
                error = %err,
                "poll interval conversion failed; using bounded fallback"
            );
            MAX_POLL_INTERVAL_SECONDS
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_poll_interval_uses_observable_fallback_for_overflow() {
        assert_eq!(
            bounded_poll_interval_seconds(u64::MAX),
            MAX_POLL_INTERVAL_SECONDS
        );
    }

    #[test]
    fn bounded_poll_interval_clamps_zero_and_large_values() {
        assert_eq!(bounded_poll_interval_seconds(0), 1);
        assert_eq!(
            bounded_poll_interval_seconds((MAX_POLL_INTERVAL_SECONDS as u64) + 1),
            MAX_POLL_INTERVAL_SECONDS
        );
    }
}
