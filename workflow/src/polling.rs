use chrono::{DateTime, Duration, Utc};

const MAX_POLL_INTERVAL_SECONDS: i64 = 86_400;

pub fn next_poll_time(poll_interval_seconds: u64) -> DateTime<Utc> {
    let seconds = i64::try_from(poll_interval_seconds)
        .unwrap_or(MAX_POLL_INTERVAL_SECONDS)
        .clamp(1, MAX_POLL_INTERVAL_SECONDS);
    Utc::now() + Duration::seconds(seconds)
}
