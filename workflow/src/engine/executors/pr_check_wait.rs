use std::collections::BTreeSet;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::engine::executors::pr_followup_types::OverallState;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrCheckMatchMode {
    Exact,
    Prefix,
    Regex,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PrCheckDefinition {
    #[serde(default = "default_match_mode")]
    pub mode: PrCheckMatchMode,
    pub pattern: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_skipped: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PrCheckWaitConfig {
    #[serde(default)]
    pub required: Vec<PrCheckDefinition>,
    #[serde(default)]
    pub optional: Vec<PrCheckDefinition>,
    #[serde(default)]
    pub ignored: Vec<PrCheckDefinition>,
    #[serde(default = "default_allow_unmatched_success")]
    pub allow_unmatched_success: bool,
    #[serde(default)]
    pub default_allow_skipped: bool,
    #[serde(default)]
    pub block_optional_failures: bool,
    #[serde(default = "default_missing_retry_attempts")]
    pub missing_retry_attempts: u32,
    #[serde(default = "default_api_error_retry_attempts")]
    pub api_error_retry_attempts: u32,
    #[serde(default = "default_max_wait_seconds")]
    pub max_wait_seconds: u64,
    #[serde(default = "default_poll_interval_seconds")]
    pub poll_interval_seconds: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PrCheckWaitCounters {
    #[serde(default)]
    pub missing_attempts: u32,
    #[serde(default)]
    pub api_error_attempts: u32,
    #[serde(default)]
    pub poll_attempts: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PrCheckObservation {
    pub check_id: String,
    pub name: String,
    pub status: Option<String>,
    pub conclusion: Option<String>,
    pub state: String,
    pub bucket: String,
    pub url: Option<String>,
    pub workflow_name: Option<String>,
    pub run_id: Option<u64>,
    pub job_id: Option<u64>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub head_sha: Option<String>,
    pub app_slug: Option<String>,
    pub source: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PrCheckMatchResult {
    pub definition: PrCheckDefinition,
    pub matched_check_ids: Vec<String>,
    pub state: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PrCheckWaitClassification {
    pub overall_state: OverallState,
    pub current_checks: Vec<PrCheckObservation>,
    pub stale_checks: Vec<PrCheckObservation>,
    pub required: Vec<PrCheckMatchResult>,
    pub optional: Vec<PrCheckMatchResult>,
    pub ignored_check_ids: Vec<String>,
    pub missing_required: Vec<String>,
    pub terminal_counts: Value,
    pub reason: String,
    pub counters: PrCheckWaitCounters,
}

impl Default for PrCheckWaitConfig {
    fn default() -> Self {
        Self {
            required: Vec::new(),
            optional: Vec::new(),
            ignored: Vec::new(),
            allow_unmatched_success: true,
            default_allow_skipped: false,
            block_optional_failures: false,
            missing_retry_attempts: default_missing_retry_attempts(),
            api_error_retry_attempts: default_api_error_retry_attempts(),
            max_wait_seconds: default_max_wait_seconds(),
            poll_interval_seconds: default_poll_interval_seconds(),
        }
    }
}

#[must_use]
pub fn classify_pr_checks(
    head_sha: &str,
    checks: Vec<PrCheckObservation>,
    config: &PrCheckWaitConfig,
    mut counters: PrCheckWaitCounters,
) -> PrCheckWaitClassification {
    counters.poll_attempts = counters.poll_attempts.saturating_add(1);
    counters.api_error_attempts = 0;
    let (current_checks, stale_checks): (Vec<_>, Vec<_>) = checks
        .into_iter()
        .partition(|check| check.head_sha.as_deref().unwrap_or(head_sha) == head_sha);
    let ignored_check_ids = ignored_check_ids(&current_checks, &stale_checks, &config.ignored);
    let considered = current_checks
        .iter()
        .filter(|check| !ignored_check_ids.contains(&check.check_id))
        .collect::<Vec<_>>();
    let required = match_definitions(&considered, &config.required, config.default_allow_skipped);
    let optional = match_definitions(&considered, &config.optional, config.default_allow_skipped);
    let matched_check_ids = matched_check_ids(&required, &optional);
    let unmatched = considered
        .iter()
        .copied()
        .filter(|check| !matched_check_ids.contains(&check.check_id))
        .collect::<Vec<_>>();
    let missing_required = required
        .iter()
        .filter(|result| result.matched_check_ids.is_empty())
        .map(|result| result.definition.pattern.clone())
        .collect::<Vec<_>>();
    counters.missing_attempts = if missing_required.is_empty() {
        0
    } else {
        counters.missing_attempts.saturating_add(1)
    };
    let terminal_counts = terminal_counts(&considered, &missing_required, &ignored_check_ids);
    let (overall_state, reason) = classify_state(
        &required,
        &optional,
        &unmatched,
        &missing_required,
        config,
        &counters,
    );
    PrCheckWaitClassification {
        overall_state,
        current_checks,
        stale_checks,
        required,
        optional,
        ignored_check_ids,
        missing_required,
        terminal_counts,
        reason,
        counters,
    }
}

#[must_use]
pub fn classify_api_error(
    config: &PrCheckWaitConfig,
    mut counters: PrCheckWaitCounters,
    message: String,
) -> PrCheckWaitClassification {
    counters.poll_attempts = counters.poll_attempts.saturating_add(1);
    counters.api_error_attempts = counters.api_error_attempts.saturating_add(1);
    let overall_state = if counters.api_error_attempts <= config.api_error_retry_attempts {
        OverallState::PendingTimeout
    } else {
        OverallState::Fatal
    };
    PrCheckWaitClassification {
        overall_state,
        current_checks: Vec::new(),
        stale_checks: Vec::new(),
        required: Vec::new(),
        optional: Vec::new(),
        ignored_check_ids: Vec::new(),
        missing_required: Vec::new(),
        terminal_counts: json!({
            "passed": 0,
            "failed": 0,
            "pending": 0,
            "unknown": 0,
            "missing": 0,
            "ignored": 0,
            "api_errors": counters.api_error_attempts
        }),
        reason: message,
        counters,
    }
}

#[must_use]
pub fn status_payload(
    classification: &PrCheckWaitClassification,
    config: &PrCheckWaitConfig,
    head_sha: &str,
    observed_at: &str,
) -> Value {
    json!({
        "head_sha": head_sha,
        "overall_state": classification.overall_state,
        "checks": classification.current_checks,
        "stale_checks": classification.stale_checks,
        "required": classification.required,
        "optional": classification.optional,
        "ignored_check_ids": classification.ignored_check_ids,
        "missing_required": classification.missing_required,
        "terminal_counts": classification.terminal_counts,
        "classification_reason": classification.reason,
        "poll_state": classification.counters,
        "policy": config,
        "observed_at": observed_at
    })
}

pub fn config_from_value(params: &Value) -> Result<PrCheckWaitConfig, String> {
    let Some(value) = params
        .get("check_policy")
        .or_else(|| params.get("pr_check_policy"))
        .filter(|value| !value.is_null())
    else {
        return Ok(PrCheckWaitConfig::default());
    };
    serde_json::from_value(value.clone()).map_err(|err| format!("invalid PR check policy: {err}"))
}

#[must_use]
pub fn counters_from_value(value: &Value) -> PrCheckWaitCounters {
    value
        .get("poll_state")
        .or_else(|| value.get("counters"))
        .and_then(|state| serde_json::from_value(state.clone()).ok())
        .unwrap_or_default()
}

#[must_use]
pub fn check_bucket(
    status: Option<&str>,
    conclusion: Option<&str>,
    bucket: &str,
    state: &str,
) -> String {
    let status = status.unwrap_or_default().to_ascii_lowercase();
    let conclusion = conclusion.unwrap_or_default().to_ascii_lowercase();
    let bucket = bucket.to_ascii_lowercase();
    let state = state.to_ascii_lowercase();
    if matches!(bucket.as_str(), "pass" | "passed")
        || matches!(conclusion.as_str(), "success" | "neutral")
        || matches!(state.as_str(), "success" | "neutral")
    {
        "passed".to_string()
    } else if matches!(conclusion.as_str(), "skipped") || matches!(state.as_str(), "skipped") {
        "skipped".to_string()
    } else if matches!(
        conclusion.as_str(),
        "failure" | "startup_failure" | "timed_out" | "action_required" | "cancelled" | "stale"
    ) || matches!(
        state.as_str(),
        "failure"
            | "failed"
            | "startup_failure"
            | "timed_out"
            | "action_required"
            | "cancelled"
            | "stale"
    ) || matches!(bucket.as_str(), "fail" | "failed")
    {
        "failed".to_string()
    } else if matches!(
        status.as_str(),
        "queued" | "requested" | "waiting" | "pending" | "in_progress"
    ) || matches!(
        state.as_str(),
        "queued" | "requested" | "waiting" | "pending" | "in_progress"
    ) || matches!(bucket.as_str(), "pending")
    {
        "pending".to_string()
    } else {
        "unknown".to_string()
    }
}

fn classify_state(
    required: &[PrCheckMatchResult],
    optional: &[PrCheckMatchResult],
    unmatched: &[&PrCheckObservation],
    missing_required: &[String],
    config: &PrCheckWaitConfig,
    counters: &PrCheckWaitCounters,
) -> (OverallState, String) {
    if required.iter().any(|result| result.state == "failed") {
        return (OverallState::Failed, "required_check_failed".to_string());
    }
    if !missing_required.is_empty() {
        return missing_state(config, counters);
    }
    if required.iter().any(|result| result.state == "pending") {
        return (
            OverallState::PendingTimeout,
            "required_check_pending".to_string(),
        );
    }
    if config.block_optional_failures && optional.iter().any(|result| result.state == "failed") {
        return (OverallState::Failed, "optional_check_failed".to_string());
    }
    if unmatched.iter().any(|check| check.bucket == "pending") {
        return (
            OverallState::PendingTimeout,
            "unmatched_check_pending".to_string(),
        );
    }
    if required.iter().any(|result| result.state == "unknown") {
        return (OverallState::Unknown, "required_check_unknown".to_string());
    }
    if unmatched
        .iter()
        .any(|check| classify_check(check, config.default_allow_skipped) == "failed")
    {
        return (OverallState::Failed, "unmatched_check_failed".to_string());
    }
    if unmatched.iter().any(|check| check.bucket == "unknown") {
        return (OverallState::Unknown, "unmatched_check_unknown".to_string());
    }
    if required.is_empty() && !config.allow_unmatched_success {
        return (
            OverallState::Unknown,
            "no_required_checks_configured".to_string(),
        );
    }
    (OverallState::Passed, "configured_checks_passed".to_string())
}

fn matched_check_ids(
    required: &[PrCheckMatchResult],
    optional: &[PrCheckMatchResult],
) -> BTreeSet<String> {
    required
        .iter()
        .chain(optional.iter())
        .flat_map(|result| result.matched_check_ids.iter().cloned())
        .collect()
}

fn missing_state(
    config: &PrCheckWaitConfig,
    counters: &PrCheckWaitCounters,
) -> (OverallState, String) {
    if counters.missing_attempts <= config.missing_retry_attempts {
        (
            OverallState::PendingTimeout,
            "required_check_missing".to_string(),
        )
    } else {
        (
            OverallState::Fatal,
            "required_check_missing_exhausted".to_string(),
        )
    }
}

fn ignored_check_ids(
    current_checks: &[PrCheckObservation],
    stale_checks: &[PrCheckObservation],
    ignored: &[PrCheckDefinition],
) -> Vec<String> {
    current_checks
        .iter()
        .chain(stale_checks.iter())
        .filter(|check| {
            ignored
                .iter()
                .any(|definition| definition_matches(definition, check))
        })
        .map(|check| check.check_id.clone())
        .collect()
}

fn match_definitions(
    checks: &[&PrCheckObservation],
    definitions: &[PrCheckDefinition],
    default_allow_skipped: bool,
) -> Vec<PrCheckMatchResult> {
    definitions
        .iter()
        .map(|definition| {
            let matched = checks
                .iter()
                .filter(|check| definition_matches(definition, check))
                .copied()
                .collect::<Vec<_>>();
            let state = aggregate_match_state(&matched, definition, default_allow_skipped);
            PrCheckMatchResult {
                definition: definition.clone(),
                matched_check_ids: matched.iter().map(|check| check.check_id.clone()).collect(),
                state,
            }
        })
        .collect()
}

fn aggregate_match_state(
    checks: &[&PrCheckObservation],
    definition: &PrCheckDefinition,
    default_allow_skipped: bool,
) -> String {
    if checks.is_empty() {
        return "missing".to_string();
    }
    let allow_skipped = definition.allow_skipped.unwrap_or(default_allow_skipped);
    let buckets = checks
        .iter()
        .map(|check| classify_check(check, allow_skipped))
        .collect::<Vec<_>>();
    if buckets.iter().any(|bucket| bucket == "failed") {
        "failed".to_string()
    } else if buckets.iter().any(|bucket| bucket == "pending") {
        "pending".to_string()
    } else if buckets.iter().any(|bucket| bucket == "unknown") {
        "unknown".to_string()
    } else {
        "passed".to_string()
    }
}

fn classify_check(check: &PrCheckObservation, allow_skipped: bool) -> String {
    if check.bucket == "skipped" {
        if allow_skipped {
            "passed".to_string()
        } else {
            "failed".to_string()
        }
    } else {
        check.bucket.clone()
    }
}

fn definition_matches(definition: &PrCheckDefinition, check: &PrCheckObservation) -> bool {
    check_value_matches(definition, &check.name)
        || check
            .workflow_name
            .as_deref()
            .is_some_and(|workflow| check_value_matches(definition, workflow))
}

fn check_value_matches(definition: &PrCheckDefinition, value: &str) -> bool {
    match definition.mode {
        PrCheckMatchMode::Exact => value == definition.pattern,
        PrCheckMatchMode::Prefix => value.starts_with(&definition.pattern),
        PrCheckMatchMode::Regex => {
            Regex::new(&definition.pattern).is_ok_and(|regex| regex.is_match(value))
        }
    }
}

fn terminal_counts(
    checks: &[&PrCheckObservation],
    missing_required: &[String],
    ignored_check_ids: &[String],
) -> Value {
    json!({
        "passed": checks.iter().filter(|check| check.bucket == "passed").count(),
        "failed": checks.iter().filter(|check| check.bucket == "failed" || check.bucket == "skipped").count(),
        "pending": checks.iter().filter(|check| check.bucket == "pending").count(),
        "unknown": checks.iter().filter(|check| check.bucket == "unknown").count(),
        "missing": missing_required.len(),
        "ignored": ignored_check_ids.len()
    })
}

const fn default_match_mode() -> PrCheckMatchMode {
    PrCheckMatchMode::Exact
}

const fn default_allow_unmatched_success() -> bool {
    true
}

const fn default_missing_retry_attempts() -> u32 {
    3
}

const fn default_api_error_retry_attempts() -> u32 {
    3
}

const fn default_max_wait_seconds() -> u64 {
    1_800
}

const fn default_poll_interval_seconds() -> u64 {
    60
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_required_is_retryable_then_fatal() {
        let config = PrCheckWaitConfig {
            required: vec![definition("ci")],
            missing_retry_attempts: 1,
            ..PrCheckWaitConfig::default()
        };
        let first = classify_pr_checks("h", Vec::new(), &config, PrCheckWaitCounters::default());
        assert_eq!(first.overall_state, OverallState::PendingTimeout);
        let second = classify_pr_checks("h", Vec::new(), &config, first.counters);
        assert_eq!(second.overall_state, OverallState::Fatal);
    }

    #[test]
    fn ignored_checks_do_not_satisfy_required_checks() {
        let config = PrCheckWaitConfig {
            required: vec![definition("ci")],
            ignored: vec![definition("ci")],
            ..PrCheckWaitConfig::default()
        };
        let check = check("ci", "passed");
        let result = classify_pr_checks("h", vec![check], &config, PrCheckWaitCounters::default());
        assert_eq!(result.overall_state, OverallState::PendingTimeout);
        assert_eq!(result.ignored_check_ids, vec!["ci".to_string()]);
    }

    #[test]
    fn stale_ignored_checks_are_reported_for_collectors() {
        let config = PrCheckWaitConfig {
            ignored: vec![definition("CodeRabbit")],
            ..PrCheckWaitConfig::default()
        };
        let mut stale = check("coderabbit-old", "pending");
        stale.name = "CodeRabbit".to_string();
        stale.head_sha = Some("old".to_string());

        let result = classify_pr_checks("h", vec![stale], &config, PrCheckWaitCounters::default());

        assert_eq!(result.ignored_check_ids, vec!["coderabbit-old".to_string()]);
        assert_eq!(result.stale_checks.len(), 1);
    }

    #[test]
    fn skipped_requires_explicit_allow_skipped() {
        let blocking_config = PrCheckWaitConfig {
            required: vec![definition("ci")],
            ..PrCheckWaitConfig::default()
        };
        let blocking_result = classify_pr_checks(
            "h",
            vec![check("ci", "skipped")],
            &blocking_config,
            PrCheckWaitCounters::default(),
        );
        assert_eq!(blocking_result.overall_state, OverallState::Failed);

        let allowed_config = PrCheckWaitConfig {
            required: vec![PrCheckDefinition {
                allow_skipped: Some(true),
                ..definition("ci")
            }],
            ..PrCheckWaitConfig::default()
        };
        let allowed_result = classify_pr_checks(
            "h",
            vec![check("ci", "skipped")],
            &allowed_config,
            PrCheckWaitCounters::default(),
        );
        assert_eq!(allowed_result.overall_state, OverallState::Passed);
    }

    fn definition(pattern: &str) -> PrCheckDefinition {
        PrCheckDefinition {
            mode: PrCheckMatchMode::Exact,
            pattern: pattern.to_string(),
            allow_skipped: None,
        }
    }

    fn check(name: &str, bucket: &str) -> PrCheckObservation {
        PrCheckObservation {
            check_id: name.to_string(),
            name: name.to_string(),
            status: Some(bucket.to_string()),
            conclusion: Some(bucket.to_string()),
            state: bucket.to_string(),
            bucket: bucket.to_string(),
            url: None,
            workflow_name: None,
            run_id: None,
            job_id: None,
            started_at: None,
            completed_at: None,
            head_sha: Some("h".to_string()),
            app_slug: None,
            source: "test".to_string(),
        }
    }
}
