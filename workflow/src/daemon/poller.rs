use chrono::{DateTime, Duration, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::adapters::github::{GithubCommandRunner, GithubError, SystemGithubCommandRunner};
use crate::engine::executors::pr_check_wait::{
    check_bucket as shared_check_bucket, classify_api_error, classify_pr_checks,
    counters_from_value, status_payload, PrCheckObservation, PrCheckWaitConfig,
};
use crate::engine::executors::pr_followup_artifacts::{ClockSleeper, PrFollowupArtifactStore};
use crate::engine::executors::pr_followup_types::{PrFollowupBinding, PR_FOLLOWUP_SCHEMA_VERSION};
use crate::persistence::checkpoint::{set_resume_point, PersistenceError};
use crate::persistence::leases::{update_lease_status, LeaseStatus};
use crate::persistence::run_metadata::RunStatus;
use crate::persistence::sqlite::{get_run_with_conn, persist_run_with_conn};
use crate::persistence::wait_state::{
    delete_wait_state, update_wait_state_after_poll, WaitKind, WaitStateRecord,
};
use crate::persistence::{
    write_poll_result_artifact, write_resume_decision_artifact, write_wait_state_artifact,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PollClassification {
    StillWaiting,
    ReadyToResume,
    TerminalFailure,
    TransientFailure,
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PollDecision {
    pub run_id: String,
    pub classification: PollClassification,
    pub next_poll_at: Option<DateTime<Utc>>,
    pub observed_state: serde_json::Value,
}

/// Seam for polling one durable external wait record.
pub trait ExternalWaitPoller {
    fn poll(&self, record: &WaitStateRecord) -> PollDecision;
}

/// Production poller backed by statically constructed `gh` argv calls.
pub struct SystemExternalWaitPoller<R = SystemGithubCommandRunner> {
    runner: R,
}

impl SystemExternalWaitPoller<SystemGithubCommandRunner> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            runner: SystemGithubCommandRunner,
        }
    }
}

impl Default for SystemExternalWaitPoller<SystemGithubCommandRunner> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R> SystemExternalWaitPoller<R> {
    #[must_use]
    pub fn with_runner(runner: R) -> Self {
        Self { runner }
    }
}

impl<R: GithubCommandRunner> ExternalWaitPoller for SystemExternalWaitPoller<R> {
    fn poll(&self, record: &WaitStateRecord) -> PollDecision {
        if let Some(decision) = timeout_decision(record) {
            return decision;
        }
        match record.wait_kind {
            WaitKind::PrChecks => poll_pr_checks(record, &self.runner),
            WaitKind::CoderabbitReview => poll_coderabbit_review(record, &self.runner),
            WaitKind::PrMerge | WaitKind::DependencyChildMerge => {
                poll_pr_merge(record, &self.runner)
            }
            WaitKind::HumanReview => poll_human_review(record, &self.runner),
            WaitKind::RateLimitBackoff => PollDecision::still_waiting_with_state(
                record,
                json!({ "classification": "still_waiting", "wait_kind": record.wait_kind }),
            ),
        }
    }
}

impl PollDecision {
    #[must_use]
    pub fn still_waiting(record: &WaitStateRecord) -> Self {
        Self::still_waiting_with_state(record, record.last_observed_state.clone())
    }

    #[must_use]
    pub fn still_waiting_with_state(record: &WaitStateRecord, observed_state: Value) -> Self {
        Self {
            run_id: record.run_id.clone(),
            classification: PollClassification::StillWaiting,
            next_poll_at: Some(next_poll_time(record)),
            observed_state,
        }
    }

    #[must_use]
    pub fn ready(record: &WaitStateRecord, observed_state: serde_json::Value) -> Self {
        Self {
            run_id: record.run_id.clone(),
            classification: PollClassification::ReadyToResume,
            next_poll_at: None,
            observed_state,
        }
    }

    #[must_use]
    pub fn transient(record: &WaitStateRecord, observed_state: Value) -> Self {
        Self {
            run_id: record.run_id.clone(),
            classification: PollClassification::TransientFailure,
            next_poll_at: Some(next_poll_time(record)),
            observed_state,
        }
    }
}

fn timeout_decision(record: &WaitStateRecord) -> Option<PollDecision> {
    let max_wait_seconds = record.max_wait_seconds?;
    let max_wait_seconds = i64::try_from(max_wait_seconds).unwrap_or(i64::MAX);
    if Utc::now()
        .signed_duration_since(record.created_at)
        .num_seconds()
        <= max_wait_seconds
    {
        return None;
    }
    Some(PollDecision {
        run_id: record.run_id.clone(),
        classification: PollClassification::TimedOut,
        next_poll_at: None,
        observed_state: json!({
            "classification": "timed_out",
            "wait_kind": record.wait_kind,
            "max_wait_seconds": record.max_wait_seconds,
        }),
    })
}

fn poll_pr_checks(record: &WaitStateRecord, runner: &dyn GithubCommandRunner) -> PollDecision {
    let Some(pr_number) = record.pr_number else {
        return terminal_failure(record, missing_identity_state(record));
    };
    let Some(head_sha) = record.head_sha.as_deref() else {
        return terminal_failure(record, missing_identity_state(record));
    };
    let config = pr_check_config(record);
    let counters = counters_from_value(&record.last_observed_state);
    let classification = match query_pr_checks(runner, &record.repository, pr_number, head_sha) {
        Ok(checks) => classify_pr_checks(
            head_sha,
            checks.into_iter().map(normalize_poll_check).collect(),
            &config,
            counters,
        ),
        Err(err) => classify_api_error(&config, counters, err.to_string()),
    };
    let observed_state = pr_check_observed_state(record, head_sha, &config, &classification);
    match classification.overall_state.as_str() {
        "passed" => PollDecision::ready(record, observed_state),
        "failed" | "fatal" => terminal_failure(record, observed_state),
        "pending_timeout" => PollDecision::still_waiting_with_state(record, observed_state),
        _ => PollDecision::transient(record, observed_state),
    }
}

fn query_pr_checks(
    runner: &dyn GithubCommandRunner,
    repo: &str,
    pr_number: u64,
    head_sha: &str,
) -> Result<Vec<Value>, GithubError> {
    let preferred = runner.run(&[
        "gh".to_string(),
        "pr".to_string(),
        "checks".to_string(),
        pr_number.to_string(),
        "--repo".to_string(),
        repo.to_string(),
        "--json".to_string(),
        "name,state,bucket,link,workflow,startedAt,completedAt".to_string(),
    ])?;
    let mut checks =
        serde_json::from_str::<Vec<Value>>(&preferred).map_err(|e| GithubError::CommandFailed {
            argv: vec!["gh".to_string(), "pr".to_string(), "checks".to_string()],
            exit_code: None,
            stderr: format!("parse gh pr checks JSON: {e}"),
        })?;
    if let Some((owner, name)) = record_repo_parts(repo) {
        let rest = runner.run(&[
            "gh".to_string(),
            "api".to_string(),
            format!("repos/{owner}/{name}/commits/{head_sha}/check-runs"),
        ])?;
        let value =
            serde_json::from_str::<Value>(&rest).map_err(|e| GithubError::CommandFailed {
                argv: vec!["gh".to_string(), "api".to_string()],
                exit_code: None,
                stderr: format!("parse gh check-runs JSON: {e}"),
            })?;
        let Some(check_runs) = value.get("check_runs") else {
            return Err(GithubError::CommandFailed {
                argv: vec!["gh".to_string(), "api".to_string()],
                exit_code: None,
                stderr: format!("gh check-runs response missing check_runs: {value}"),
            });
        };
        checks.extend(check_runs.as_array().cloned().unwrap_or_default());
    }
    Ok(checks)
}
fn pr_check_config(record: &WaitStateRecord) -> PrCheckWaitConfig {
    record
        .wait_condition
        .get("check_policy")
        .or_else(|| record.wait_condition.get("pr_check_policy"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

fn pr_check_observed_state(
    record: &WaitStateRecord,
    head_sha: &str,
    config: &PrCheckWaitConfig,
    classification: &crate::engine::executors::pr_check_wait::PrCheckWaitClassification,
) -> Value {
    let mut state = status_payload(classification, config, head_sha, &Utc::now().to_rfc3339());
    state["classification"] = json!(classification.overall_state.as_str());
    state["wait_kind"] = json!(record.wait_kind);
    if let Err(err) = write_pr_check_status_snapshot(record, &state) {
        state["artifact_error"] = json!(err.to_string());
    }
    state
}

fn write_pr_check_status_snapshot(
    record: &WaitStateRecord,
    state: &Value,
) -> Result<(), crate::engine::runner::EngineError> {
    let artifact_root = record
        .wait_condition
        .get("artifact_root")
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from);
    let Some(artifact_root) = artifact_root else {
        return Ok(());
    };
    let Some(pr_number) = record.pr_number else {
        return Ok(());
    };
    let Some(head_sha) = record.head_sha.as_deref() else {
        return Ok(());
    };
    let Some((owner, repo)) = record_repo_parts(&record.repository) else {
        return Ok(());
    };
    let binding = PrFollowupBinding {
        schema_version: PR_FOLLOWUP_SCHEMA_VERSION,
        run_id: record.run_id.clone(),
        repository_owner: owner.to_string(),
        repository_name: repo.to_string(),
        pr_number,
        head_ref: record
            .wait_condition
            .get("head_ref")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        head_sha: head_sha.to_string(),
        base_ref: record
            .wait_condition
            .get("base_ref")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        base_sha: record
            .wait_condition
            .get("base_sha")
            .and_then(Value::as_str)
            .map(ToString::to_string),
    };
    let store = PrFollowupArtifactStore::new(artifact_root);
    let clock = PollerClock;
    store.write_json_artifact(
        &binding,
        "pr-check-status",
        "poll_pr_checks",
        3,
        state,
        None,
        &clock,
    )?;
    Ok(())
}

struct PollerClock;

impl ClockSleeper for PollerClock {
    fn now_rfc3339(&self) -> String {
        Utc::now().to_rfc3339()
    }

    fn sleep(&self, _duration: std::time::Duration) {}
}

fn normalize_poll_check(row: Value) -> PrCheckObservation {
    let name = row
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let state = row
        .get("state")
        .or_else(|| row.get("status"))
        .or_else(|| row.get("conclusion"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let status = row
        .get("status")
        .or_else(|| row.get("state"))
        .and_then(Value::as_str);
    let conclusion = row
        .get("conclusion")
        .or_else(|| row.get("state"))
        .and_then(Value::as_str);
    let bucket_raw = row.get("bucket").and_then(Value::as_str).unwrap_or(&state);
    PrCheckObservation {
        check_id: row
            .get("id")
            .and_then(Value::as_u64)
            .map_or_else(|| name.clone(), |id| id.to_string()),
        name,
        status: status.map(ToString::to_string),
        conclusion: conclusion.map(ToString::to_string),
        state: state.clone(),
        bucket: shared_check_bucket(status, conclusion, bucket_raw, &state),
        url: row
            .get("link")
            .or_else(|| row.get("html_url"))
            .or_else(|| row.get("details_url"))
            .and_then(Value::as_str)
            .map(ToString::to_string),
        workflow_name: row
            .get("workflow")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        run_id: row.pointer("/check_suite/id").and_then(Value::as_u64),
        job_id: None,
        started_at: row
            .get("startedAt")
            .or_else(|| row.get("started_at"))
            .and_then(Value::as_str)
            .map(ToString::to_string),
        completed_at: row
            .get("completedAt")
            .or_else(|| row.get("completed_at"))
            .and_then(Value::as_str)
            .map(ToString::to_string),
        head_sha: row
            .get("head_sha")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        app_slug: row
            .pointer("/app/slug")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        source: "daemon_poll".to_string(),
    }
}

#[cfg(test)]
fn classify_terminal_or_pending(record: &WaitStateRecord, rows: Vec<Value>) -> PollDecision {
    let config = pr_check_config(record);
    let head_sha = record.head_sha.as_deref().unwrap_or_default();
    let classification = classify_pr_checks(
        head_sha,
        rows.into_iter().map(normalize_poll_check).collect(),
        &config,
        counters_from_value(&record.last_observed_state),
    );
    let state = pr_check_observed_state(record, head_sha, &config, &classification);
    match classification.overall_state.as_str() {
        "passed" => PollDecision::ready(record, state),
        "failed" | "fatal" => terminal_failure(record, state),
        "pending_timeout" => PollDecision::still_waiting_with_state(record, state),
        _ => PollDecision::transient(record, state),
    }
}

fn poll_coderabbit_review(
    record: &WaitStateRecord,
    runner: &dyn GithubCommandRunner,
) -> PollDecision {
    let Some(pr_number) = record.pr_number else {
        return PollDecision::still_waiting_with_state(record, missing_identity_state(record));
    };
    match query_coderabbit_surfaces(runner, &record.repository, pr_number) {
        Ok(state) if coderabbit_is_ready(&state) => PollDecision::ready(record, state),
        Ok(state) => PollDecision::still_waiting_with_state(record, state),
        Err(err) => PollDecision::transient(record, github_error_state(err)),
    }
}

fn query_coderabbit_surfaces(
    runner: &dyn GithubCommandRunner,
    repo: &str,
    pr_number: u64,
) -> Result<Value, GithubError> {
    let issue_comments = runner.run(&[
        "gh".to_string(),
        "api".to_string(),
        format!("repos/{repo}/issues/{pr_number}/comments"),
    ])?;
    let review_comments = runner.run(&[
        "gh".to_string(),
        "api".to_string(),
        format!("repos/{repo}/pulls/{pr_number}/comments"),
    ])?;
    Ok(json!({
        "classification": "polled",
        "wait_kind": WaitKind::CoderabbitReview,
        "issue_comments": parse_json_or_null(&issue_comments),
        "review_comments": parse_json_or_null(&review_comments),
    }))
}

fn coderabbit_is_ready(state: &Value) -> bool {
    let text = comments_text(state);
    let coderabbit_text = coderabbit_comment_text(state);
    text.contains("coderabbit")
        && !coderabbit_has_blocking_error(&coderabbit_text)
        && contains_any(
            &text,
            &[
                "finished reviewing",
                "review completed",
                "summary by coderabbit",
                "walkthrough",
            ],
        )
}

fn coderabbit_has_blocking_error(text: &str) -> bool {
    contains_any(
        text,
        &[
            "rate limited",
            "rate limit",
            "review limit reached",
            "run out of usage credits",
            "usage credits",
            "quota",
            "encountered an error",
            "review failed",
        ],
    )
}

fn comments_text(state: &Value) -> String {
    let mut bodies = Vec::new();
    collect_comment_bodies(state, &mut bodies);
    bodies.join("\n").to_ascii_lowercase()
}

fn coderabbit_comment_text(state: &Value) -> String {
    let mut bodies = Vec::new();
    collect_coderabbit_comment_bodies(state, &mut bodies);
    bodies.join("\n").to_ascii_lowercase()
}

fn collect_comment_bodies(value: &Value, bodies: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if let Some(body) = map.get("body").and_then(Value::as_str) {
                bodies.push(body.to_string());
            }
            for child in map.values() {
                collect_comment_bodies(child, bodies);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_comment_bodies(item, bodies);
            }
        }
        _ => {}
    }
}

fn collect_coderabbit_comment_bodies(value: &Value, bodies: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if let Some(body) = map.get("body").and_then(Value::as_str) {
                if is_coderabbit_authored_comment(value) {
                    bodies.push(body.to_string());
                }
            }
            for child in map.values() {
                collect_coderabbit_comment_bodies(child, bodies);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_coderabbit_comment_bodies(item, bodies);
            }
        }
        _ => {}
    }
}

fn is_coderabbit_authored_comment(value: &Value) -> bool {
    comment_author_logins(value)
        .iter()
        .any(|login| login.contains("coderabbit"))
}

fn comment_author_logins(value: &Value) -> Vec<String> {
    let mut logins = Vec::new();
    collect_login_fields(value, &mut logins);
    logins
}

fn collect_login_fields(value: &Value, logins: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if let Some(login) = map.get("login").and_then(Value::as_str) {
                logins.push(login.to_ascii_lowercase());
            }
            if let Some(slug) = map.get("slug").and_then(Value::as_str) {
                logins.push(slug.to_ascii_lowercase());
            }
            for child in map.values() {
                collect_login_fields(child, logins);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_login_fields(item, logins);
            }
        }
        _ => {}
    }
}

fn poll_pr_merge(record: &WaitStateRecord, runner: &dyn GithubCommandRunner) -> PollDecision {
    let Some(pr_number) = record.pr_number else {
        return PollDecision::still_waiting_with_state(record, missing_identity_state(record));
    };
    match query_pr_state(runner, &record.repository, pr_number) {
        Ok(state) => classify_pr_merge_state(record, state),
        Err(err) => PollDecision::transient(record, github_error_state(err)),
    }
}

fn classify_pr_merge_state(record: &WaitStateRecord, state: Value) -> PollDecision {
    if state.get("mergedAt").and_then(Value::as_str).is_some() {
        return PollDecision::ready(record, state);
    }
    if state.get("state").and_then(Value::as_str) == Some("CLOSED") {
        return PollDecision {
            run_id: record.run_id.clone(),
            classification: PollClassification::TerminalFailure,
            next_poll_at: None,
            observed_state: state,
        };
    }
    PollDecision::still_waiting_with_state(record, state)
}

fn poll_human_review(record: &WaitStateRecord, runner: &dyn GithubCommandRunner) -> PollDecision {
    let Some(pr_number) = record.pr_number else {
        return PollDecision::still_waiting_with_state(record, missing_identity_state(record));
    };
    match query_pr_state(runner, &record.repository, pr_number) {
        Ok(state) if review_decision_ready(&state) => PollDecision::ready(record, state),
        Ok(state) => PollDecision::still_waiting_with_state(record, state),
        Err(err) => PollDecision::transient(record, github_error_state(err)),
    }
}

fn query_pr_state(
    runner: &dyn GithubCommandRunner,
    repo: &str,
    pr_number: u64,
) -> Result<Value, GithubError> {
    let output = runner.run(&[
        "gh".to_string(),
        "pr".to_string(),
        "view".to_string(),
        pr_number.to_string(),
        "--repo".to_string(),
        repo.to_string(),
        "--json".to_string(),
        "state,mergedAt,reviewDecision,isDraft".to_string(),
    ])?;
    Ok(parse_json_or_null(&output))
}

fn review_decision_ready(state: &Value) -> bool {
    matches!(
        state.get("reviewDecision").and_then(Value::as_str),
        Some("APPROVED")
    )
}

fn record_repo_parts(repo: &str) -> Option<(&str, &str)> {
    repo.split_once('/')
}

fn parse_json_or_null(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

fn missing_identity_state(record: &WaitStateRecord) -> Value {
    json!({
        "classification": "still_waiting",
        "reason": "missing_poll_identity",
        "wait_kind": record.wait_kind,
        "has_pr_number": record.pr_number.is_some(),
        "has_head_sha": record.head_sha.is_some(),
    })
}

fn terminal_failure(record: &WaitStateRecord, observed_state: Value) -> PollDecision {
    PollDecision {
        run_id: record.run_id.clone(),
        classification: PollClassification::TerminalFailure,
        next_poll_at: None,
        observed_state,
    }
}

fn github_error_state(err: GithubError) -> Value {
    json!({ "classification": "transient_failure", "error": err.to_string() })
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

pub fn apply_poll_decision(
    conn: &Connection,
    record: &WaitStateRecord,
    decision: &PollDecision,
) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    match decision.classification {
        PollClassification::ReadyToResume => {
            set_resume_point(&tx, &record.run_id, &record.resume_step)
                .map_err(persistence_to_sqlite)?;
            mark_run_status(
                &tx,
                &record.run_id,
                RunStatus::ReadyToResume,
                &record.resume_step,
            )?;
            if let Some(lease_id) = record.lease_id.as_deref() {
                update_lease_status(
                    &tx,
                    lease_id,
                    LeaseStatus::ReadyToResume,
                    Some(&record.run_id),
                )?;
            }
            delete_wait_state(&tx, &record.run_id)?;
        }
        PollClassification::TerminalFailure | PollClassification::TimedOut => {
            mark_run_status(&tx, &record.run_id, RunStatus::Failed, &record.resume_step)?;
            if let Some(lease_id) = record.lease_id.as_deref() {
                update_lease_status(&tx, lease_id, LeaseStatus::Failed, Some(&record.run_id))?;
            }
            delete_wait_state(&tx, &record.run_id)?;
        }
        PollClassification::StillWaiting | PollClassification::TransientFailure => {
            mark_run_status(
                &tx,
                &record.run_id,
                RunStatus::WaitingExternal,
                &record.resume_step,
            )?;
            let next_poll_at = decision
                .next_poll_at
                .unwrap_or_else(|| next_poll_time(record));
            if !update_wait_state_after_poll(
                &tx,
                &record.run_id,
                &decision.observed_state,
                next_poll_at,
            )? {
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
            if let Some(lease_id) = record.lease_id.as_deref() {
                update_lease_status(
                    &tx,
                    lease_id,
                    LeaseStatus::WaitingExternal,
                    Some(&record.run_id),
                )?;
            }
        }
    }
    tx.commit()?;
    if let Err(e) = persist_poll_artifacts(record, decision) {
        eprintln!(
            "Warning: failed to persist poll artifact for run {}: {e}",
            record.run_id
        );
    }
    Ok(())
}

fn persist_poll_artifacts(
    record: &WaitStateRecord,
    decision: &PollDecision,
) -> rusqlite::Result<()> {
    write_poll_result_artifact(&record.run_id, &json!(decision)).map_err(persistence_to_sqlite)?;
    if decision.classification == PollClassification::ReadyToResume {
        write_wait_state_artifact(&record.run_id, record).map_err(persistence_to_sqlite)?;
        write_resume_decision_artifact(&record.run_id, decision).map_err(persistence_to_sqlite)?;
    }
    Ok(())
}

fn persistence_to_sqlite(error: PersistenceError) -> rusqlite::Error {
    sqlite_other_error(error)
}

fn sqlite_other_error(error: impl std::fmt::Display) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(error.to_string())))
}

fn mark_run_status(
    conn: &Connection,
    run_id: &str,
    status: RunStatus,
    step_id: &str,
) -> rusqlite::Result<()> {
    let Some(mut metadata) = get_run_with_conn(conn, run_id)? else {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    };
    metadata.status = status;
    metadata.set_current_step(step_id.to_string());
    persist_run_with_conn(conn, &metadata)?;
    Ok(())
}

fn next_poll_time(record: &WaitStateRecord) -> DateTime<Utc> {
    const MAX_POLL_INTERVAL_SECONDS: i64 = 86_400;
    let seconds = i64::try_from(record.poll_interval_seconds)
        .unwrap_or(MAX_POLL_INTERVAL_SECONDS)
        .clamp(1, MAX_POLL_INTERVAL_SECONDS);
    Utc::now() + Duration::seconds(seconds)
}

#[cfg(test)]
mod tests;
