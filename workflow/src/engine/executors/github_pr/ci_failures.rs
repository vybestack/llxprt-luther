//! CI failure collection helpers for GitHub PR follow-up executors.
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
//! @requirement:REQ-PRFU-007

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::engine::executor::StepContext;
use crate::engine::executors::pr_followup_artifacts::{ClockSleeper, PrFollowupArtifactStore};
use crate::engine::executors::pr_followup_types::{
    CollectionState, OverallState, PrCheckStatus, PrFollowupBinding,
};
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;

use super::{
    artifact_root, binding_from_artifact, extract_job_id, github_pr_error,
    read_or_capture_pr_identity, require_u64, step_order_index, watch_pr_checks,
    GithubPrCommandRunner,
};
/// Result of bounded Actions log collection.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 8-12
#[derive(Clone, Debug)]
struct LogCollectionResult {
    status: String,
    excerpt: String,
    raw_log_path: Option<String>,
    excerpt_path: Option<String>,
    artifact: Option<Value>,
    error: Option<Value>,
}

impl LogCollectionResult {
    fn not_applicable() -> Self {
        Self {
            status: "not_applicable".to_string(),
            excerpt: String::new(),
            raw_log_path: None,
            excerpt_path: None,
            artifact: None,
            error: None,
        }
    }

    fn unavailable_for_job_mapping(run_id: Option<u64>) -> Self {
        Self {
            status: "unavailable".to_string(),
            excerpt: String::new(),
            raw_log_path: None,
            excerpt_path: None,
            artifact: None,
            error: Some(json!({ "class": "job_mapping_unavailable", "run_id": run_id })),
        }
    }

    fn fetch_failed(err: EngineError, job_id: u64) -> Self {
        Self {
            status: "fetch_failed".to_string(),
            excerpt: String::new(),
            raw_log_path: None,
            excerpt_path: None,
            artifact: None,
            error: Some(
                json!({ "class": "fetch_failed", "message": err.to_string(), "job_id": job_id }),
            ),
        }
    }
}

/// Collected CI failure and uncertainty artifact fragments.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 4-18
#[derive(Clone, Debug, Default)]
struct CiFailureCollection {
    failures: Vec<Value>,
    pending_or_unknown: Vec<Value>,
    stale_checks: Vec<Value>,
    log_artifacts: Vec<Value>,
}

struct CiFailureCollectionInput<'a> {
    binding: &'a PrFollowupBinding,
    check_status: &'a Value,
    ignored_check_ids: BTreeSet<String>,
    source_sequence: u64,
    runner: &'a dyn GithubPrCommandRunner,
    store: &'a PrFollowupArtifactStore,
    clock: &'a dyn ClockSleeper,
}

#[derive(Clone, Debug)]
struct ArtifactFailure {
    state: String,
    reason: String,
    metadata: Value,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 1-21
pub(super) fn collect_ci_failures(
    context: &StepContext,
    params: &Value,
    runner: &dyn GithubPrCommandRunner,
    clock: &dyn ClockSleeper,
) -> Result<StepOutcome, EngineError> {
    let store = PrFollowupArtifactStore::new(artifact_root(context, params)?);
    let pr_value = read_or_capture_pr_identity(context, params, runner, clock, &store)?;
    let binding = binding_from_artifact(&pr_value)?;
    let check_status =
        read_check_status_artifact(context, params, runner, clock, &store, &binding)?;
    let source_sequence = require_u64(&check_status, "artifact_sequence")?;
    let typed_check_status = deserialize_check_status(check_status.clone())?;
    let watcher_fatal_source = watcher_fatal_source(&typed_check_status);
    let collection = collect_ci_failure_fragments(CiFailureCollectionInput {
        binding: &binding,
        check_status: &check_status,
        ignored_check_ids: ignored_check_ids(&check_status),
        source_sequence,
        runner,
        store: &store,
        clock,
    })?;
    let collection_state =
        ci_collection_state(typed_check_status.overall_state, &watcher_fatal_source);
    let payload = ci_failures_payload(
        collection,
        collection_state,
        typed_check_status.overall_state,
        watcher_fatal_source,
        &check_status,
        source_sequence,
        clock,
    );
    let failure =
        ci_failures_artifact_failure(&payload, collection_state, typed_check_status.overall_state);
    let is_fatal = matches!(collection_state, CollectionState::Fatal);
    let failure_ref = failure.as_ref().map(|item| {
        (
            item.state.as_str(),
            item.reason.as_str(),
            item.metadata.clone(),
        )
    });

    store.write_json_artifact(
        &binding,
        "ci-failures",
        "collect_ci_failures",
        step_order_index(params, 4),
        &payload,
        failure_ref,
        clock,
    )?;

    if is_fatal {
        Ok(StepOutcome::Fatal)
    } else {
        Ok(StepOutcome::Success)
    }
}

fn read_check_status_artifact(
    context: &StepContext,
    params: &Value,
    runner: &dyn GithubPrCommandRunner,
    clock: &dyn ClockSleeper,
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
) -> Result<Value, EngineError> {
    match store.read_current_json(binding, "pr-check-status") {
        Ok(value) => Ok(value),
        Err(_) => {
            watch_pr_checks(context, params, runner, clock)?;
            store.read_current_json(binding, "pr-check-status")
        }
    }
}

fn deserialize_check_status(value: Value) -> Result<PrCheckStatus, EngineError> {
    serde_json::from_value(value).map_err(|err| EngineError::StepExecutionError {
        step_id: "collect_ci_failures".to_string(),
        message: format!("deserialize pr-check-status artifact: {err}"),
    })
}

fn watcher_fatal_source(check_status: &PrCheckStatus) -> Value {
    check_status.fatal_source.clone().unwrap_or(Value::Null)
}

fn collect_ci_failure_fragments(
    input: CiFailureCollectionInput<'_>,
) -> Result<CiFailureCollection, EngineError> {
    let mut collection = collect_current_head_check_fragments(&input)?;
    collect_stale_check_fragments(
        input.check_status,
        &input.ignored_check_ids,
        input.source_sequence,
        &mut collection.stale_checks,
    );
    add_watcher_fatal_fragment(input.check_status, input.source_sequence, &mut collection);
    Ok(collection)
}

fn collect_current_head_check_fragments(
    input: &CiFailureCollectionInput<'_>,
) -> Result<CiFailureCollection, EngineError> {
    let mut collection = CiFailureCollection::default();
    for check in input
        .check_status
        .get("checks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if check_id(check).is_some_and(|check_id| input.ignored_check_ids.contains(check_id)) {
            continue;
        }
        match check_entry_bucket(check) {
            "failed" => collection.failures.push(ci_failure_json(
                input.binding,
                check,
                input.source_sequence,
                input.runner,
                input.store,
                input.clock,
                &mut collection.log_artifacts,
            )?),
            "pending" | "unknown" => collection.pending_or_unknown.push(pending_or_unknown_json(
                "current_head_check",
                check_entry_bucket(check),
                check,
                input.source_sequence,
            )),
            _ => {}
        }
    }
    Ok(collection)
}

fn collect_stale_check_fragments(
    check_status: &Value,
    ignored_check_ids: &BTreeSet<String>,
    source_sequence: u64,
    pending_or_unknown: &mut Vec<Value>,
) {
    for stale in check_status
        .get("stale_checks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if check_id(stale).is_some_and(|check_id| ignored_check_ids.contains(check_id)) {
            continue;
        }
        pending_or_unknown.push(pending_or_unknown_json(
            "stale_check",
            "stale_only",
            stale,
            source_sequence,
        ));
    }
}

fn add_watcher_fatal_fragment(
    check_status: &Value,
    source_sequence: u64,
    collection: &mut CiFailureCollection,
) {
    let watcher_fatal_source = check_status
        .get("fatal_source")
        .cloned()
        .unwrap_or(Value::Null);
    let overall_state = check_status
        .get("overall_state")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if watcher_fatal_source.is_null() && overall_state != OverallState::Fatal.as_str() {
        return;
    }

    collection.pending_or_unknown.push(json!({
        "source": "watch_pr_checks",
        "reason": "watcher_fatal",
        "watcher_fatal_source": watcher_fatal_source,
        "source_check_status_artifact_sequence": source_sequence,
        "source_artifact_path": check_status.pointer("/history_metadata/canonical_path").cloned().unwrap_or(Value::Null),
        "safe_error_metadata": check_status.get("failure_details").cloned().unwrap_or_else(|| json!({}))
    }));
}

fn ci_collection_state(
    overall_state: OverallState,
    watcher_fatal_source: &Value,
) -> CollectionState {
    if matches!(overall_state, OverallState::Fatal) || !watcher_fatal_source.is_null() {
        CollectionState::Fatal
    } else {
        CollectionState::Collected
    }
}

fn ci_failures_payload(
    collection: CiFailureCollection,
    collection_state: CollectionState,
    overall_state: OverallState,
    watcher_fatal_source: Value,
    check_status: &Value,
    source_sequence: u64,
    clock: &dyn ClockSleeper,
) -> Value {
    let fatal_source = ci_failure_fatal_source(
        collection_state,
        overall_state,
        &watcher_fatal_source,
        collection.pending_or_unknown.is_empty(),
    );
    json!({
        "collection_state": collection_state,
        "failures": collection.failures,
        "pending_or_unknown": collection.pending_or_unknown,
        "stale_checks": collection.stale_checks,
        "watcher_fatal_source": watcher_fatal_source,
        "fatal_source": fatal_source,
        "log_artifacts": collection.log_artifacts,
        "source_check_status_artifact_sequence": source_sequence,
        "source_check_status_artifact_path": check_status.pointer("/history_metadata/canonical_path").cloned().unwrap_or(Value::Null),
        "collected_at": clock.now_rfc3339()
    })
}

fn ci_failure_fatal_source(
    collection_state: CollectionState,
    overall_state: OverallState,
    watcher_fatal_source: &Value,
    pending_is_empty: bool,
) -> Value {
    if matches!(collection_state, CollectionState::Fatal) {
        watcher_fatal_source.clone()
    } else if pending_is_empty {
        Value::Null
    } else {
        Value::String(overall_state.as_str().to_string())
    }
}

fn ci_failures_artifact_failure(
    payload: &Value,
    collection_state: CollectionState,
    overall_state: OverallState,
) -> Option<ArtifactFailure> {
    let pending_count = pending_or_unknown_count(payload);
    if !matches!(collection_state, CollectionState::Fatal) && pending_count == 0 {
        return None;
    }

    let collection_is_fatal = matches!(collection_state, CollectionState::Fatal);
    Some(ArtifactFailure {
        state: if collection_is_fatal {
            "fatal".to_string()
        } else {
            overall_state.as_str().to_string()
        },
        reason: if collection_is_fatal {
            "watcher_fatal".to_string()
        } else {
            overall_state.as_str().to_string()
        },
        metadata: json!({
            "watcher_fatal_source": payload.get("watcher_fatal_source").cloned().unwrap_or(Value::Null),
            "pending_or_unknown_count": pending_count,
            "source_check_status_artifact_sequence": payload
                .get("source_check_status_artifact_sequence")
                .cloned()
                .unwrap_or(Value::Null)
        }),
    })
}

fn pending_or_unknown_count(payload: &Value) -> usize {
    payload
        .get("pending_or_unknown")
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

fn ignored_check_ids(check_status: &Value) -> BTreeSet<String> {
    check_status
        .get("ignored_check_ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn check_id(check: &Value) -> Option<&str> {
    check.get("check_id").and_then(Value::as_str)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 6-12
fn ci_failure_json(
    binding: &PrFollowupBinding,
    check: &Value,
    source_sequence: u64,
    runner: &dyn GithubPrCommandRunner,
    store: &PrFollowupArtifactStore,
    clock: &dyn ClockSleeper,
    log_artifacts: &mut Vec<Value>,
) -> Result<Value, EngineError> {
    let check_id = check
        .get("check_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let check_name = check
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let run_id = check
        .get("run_id")
        .and_then(Value::as_u64)
        .or_else(|| extract_run_id(check.get("url").and_then(Value::as_str)));
    let job_id = check
        .get("job_id")
        .and_then(Value::as_u64)
        .or_else(|| extract_job_id(check.get("url").and_then(Value::as_str)));
    let log_result = collect_log_for_check(binding, check, run_id, job_id, runner, store, clock)?;
    let resolved_job_id = job_id.or_else(|| {
        log_result
            .artifact
            .as_ref()
            .and_then(|artifact| artifact.get("job_id"))
            .and_then(Value::as_u64)
    });
    if let Some(artifact) = log_result.artifact.clone() {
        log_artifacts.push(artifact);
    }
    Ok(json!({
        "failure_id": stable_failure_id(check_id, check_name, &binding.head_sha),
        "check_id": check_id,
        "check_name": check_name,
        "state": check.get("state").cloned().unwrap_or(Value::Null),
        "conclusion": check.get("conclusion").cloned().unwrap_or(Value::Null),
        "url": check.get("url").cloned().unwrap_or(Value::Null),
        "run_id": run_id,
        "job_id": resolved_job_id,
        "workflow_name": check.get("workflow_name").cloned().unwrap_or(Value::Null),
        "source_check_status_artifact_sequence": source_sequence,
        "log_status": log_result.status,
        "log_excerpt": log_result.excerpt,
        "log_excerpt_path": log_result.excerpt_path,
        "raw_log_path": log_result.raw_log_path,
        "collection_error": log_result.error
    }))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 8-12
fn collect_log_for_check(
    binding: &PrFollowupBinding,
    check: &Value,
    run_id: Option<u64>,
    job_id: Option<u64>,
    runner: &dyn GithubPrCommandRunner,
    store: &PrFollowupArtifactStore,
    clock: &dyn ClockSleeper,
) -> Result<LogCollectionResult, EngineError> {
    if !is_actions_check(check) {
        return Ok(LogCollectionResult::not_applicable());
    }
    let Some(resolved_job_id) = resolve_job_id(binding, check, run_id, job_id, runner)? else {
        return Ok(LogCollectionResult::unavailable_for_job_mapping(run_id));
    };
    let argv = vec![
        "gh".to_string(),
        "api".to_string(),
        format!(
            "repos/{}/{}/actions/jobs/{}/logs",
            binding.repository_owner, binding.repository_name, resolved_job_id
        ),
    ];
    match runner.run_github_command(&argv) {
        Ok(log_text) => {
            log_collection_available(binding, run_id, resolved_job_id, store, clock, log_text)
        }
        Err(err) => Ok(LogCollectionResult::fetch_failed(err, resolved_job_id)),
    }
}

fn is_actions_check(check: &Value) -> bool {
    check.get("app_slug").and_then(Value::as_str) == Some("github-actions")
        || check
            .get("url")
            .and_then(Value::as_str)
            .is_some_and(|url| url.contains("/actions/"))
}

fn log_collection_available(
    binding: &PrFollowupBinding,
    run_id: Option<u64>,
    job_id: u64,
    store: &PrFollowupArtifactStore,
    clock: &dyn ClockSleeper,
    log_text: String,
) -> Result<LogCollectionResult, EngineError> {
    let excerpt = bounded_excerpt(&log_text);
    let raw_path = ci_job_log_path(binding, store, job_id, ".log");
    let excerpt_path = ci_job_log_path(binding, store, job_id, "-excerpt.log");
    write_bounded_log_artifact(&raw_path, &log_text)?;
    write_bounded_log_artifact(&excerpt_path, &excerpt)?;
    Ok(LogCollectionResult {
        status: "available".to_string(),
        excerpt,
        raw_log_path: Some(raw_path.display().to_string()),
        excerpt_path: Some(excerpt_path.display().to_string()),
        artifact: Some(json!({
            "job_id": job_id,
            "run_id": run_id,
            "log_status": "available",
            "raw_log_path": raw_path.display().to_string(),
            "log_excerpt_path": excerpt_path.display().to_string(),
            "collected_at": clock.now_rfc3339()
        })),
        error: None,
    })
}

fn ci_job_log_path(
    binding: &PrFollowupBinding,
    store: &PrFollowupArtifactStore,
    job_id: u64,
    suffix: &str,
) -> PathBuf {
    store
        .root()
        .join("pr-followup")
        .join("logs")
        .join(&binding.run_id)
        .join(format!("ci-job-{job_id}{suffix}"))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 8-14
fn resolve_job_id(
    binding: &PrFollowupBinding,
    check: &Value,
    run_id: Option<u64>,
    job_id: Option<u64>,
    runner: &dyn GithubPrCommandRunner,
) -> Result<Option<u64>, EngineError> {
    if job_id.is_some() {
        return Ok(job_id);
    }
    let Some(run_id) = run_id else {
        return Ok(None);
    };
    let mut jobs_seen = 0_u64;
    let mut page = 1_u64;
    loop {
        let argv = vec![
            "gh".to_string(),
            "api".to_string(),
            format!(
                "repos/{}/{}/actions/runs/{}/jobs?per_page=100&page={}",
                binding.repository_owner, binding.repository_name, run_id, page
            ),
        ];
        let output = runner.run_github_command(&argv)?;
        let value = serde_json::from_str::<Value>(&output)
            .map_err(|err| github_pr_error(format!("parse actions jobs json: {err}")))?;
        let jobs = value.get("jobs").and_then(Value::as_array);
        if let Some(job) = jobs
            .into_iter()
            .flatten()
            .find(|job| job_matches_check(job, check, &binding.head_sha))
        {
            return Ok(job.get("id").and_then(Value::as_u64));
        }

        let jobs_len = jobs.map_or(0, Vec::len) as u64;
        jobs_seen = jobs_seen.saturating_add(jobs_len);
        let total_count = value
            .get("total_count")
            .and_then(Value::as_u64)
            .unwrap_or(jobs_seen);
        if jobs_len == 0 || jobs_seen >= total_count {
            break;
        }
        page = page.saturating_add(1);
    }
    Ok(None)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 9-10
fn job_matches_check(job: &Value, check: &Value, head_sha: &str) -> bool {
    let job_head = job
        .get("head_sha")
        .and_then(Value::as_str)
        .unwrap_or(head_sha);
    let job_name = job.get("name").and_then(Value::as_str).unwrap_or_default();
    let check_name = check
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    job_head == head_sha
        && (job_name == check_name
            || check_name.contains(job_name)
            || job_name.contains(check_name))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 6-14
fn check_entry_bucket(check: &Value) -> &'static str {
    let bucket = check
        .get("bucket")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let state = check
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let conclusion = check
        .get("conclusion")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(bucket.as_str(), "failed" | "fail")
        || matches!(
            state.as_str(),
            "failure"
                | "failed"
                | "startup_failure"
                | "timed_out"
                | "action_required"
                | "cancelled"
        )
        || matches!(
            conclusion.as_str(),
            "failure" | "startup_failure" | "timed_out" | "action_required" | "cancelled"
        )
    {
        "failed"
    } else if matches!(bucket.as_str(), "pending")
        || matches!(
            state.as_str(),
            "queued" | "requested" | "waiting" | "pending" | "in_progress"
        )
    {
        "pending"
    } else if matches!(bucket.as_str(), "passed" | "pass")
        || matches!(state.as_str(), "success" | "neutral" | "skipped")
        || matches!(conclusion.as_str(), "success" | "neutral" | "skipped")
    {
        "passed"
    } else {
        "unknown"
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 13-14
fn pending_or_unknown_json(
    source: &str,
    reason: &str,
    evidence: &Value,
    source_sequence: u64,
) -> Value {
    json!({
        "source": source,
        "reason": reason,
        "check_id": evidence.get("check_id").cloned().unwrap_or(Value::Null),
        "check_name": evidence.get("name").cloned().unwrap_or(Value::Null),
        "state": evidence.get("state").cloned().unwrap_or(Value::Null),
        "conclusion": evidence.get("conclusion").cloned().unwrap_or(Value::Null),
        "url": evidence.get("url").cloned().unwrap_or(Value::Null),
        "run_id": evidence.get("run_id").cloned().unwrap_or(Value::Null),
        "job_id": evidence.get("job_id").cloned().unwrap_or(Value::Null),
        "source_check_status_artifact_sequence": source_sequence
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 6-7
fn stable_failure_id(check_id: &str, check_name: &str, head_sha: &str) -> String {
    format!("ci:{head_sha}:{check_id}:{check_name}").replace(['/', ' ', ':'], "-")
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 8-10
fn extract_run_id(url: Option<&str>) -> Option<u64> {
    let url = url?;
    let marker = "/actions/runs/";
    let (_, after) = url.split_once(marker)?;
    after.split('/').next()?.parse().ok()
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 10-12
fn bounded_excerpt(text: &str) -> String {
    text.chars().take(4096).collect()
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 10-12,20
fn write_bounded_log_artifact(path: &PathBuf, text: &str) -> Result<(), EngineError> {
    let parent = path
        .parent()
        .ok_or_else(|| github_pr_error(format!("missing parent for {}", path.display())))?;
    fs::create_dir_all(parent)
        .map_err(|err| github_pr_error(format!("create log artifact parent: {err}")))?;
    fs::write(path, text)
        .map_err(|err| github_pr_error(format!("write log artifact {}: {err}", path.display())))
}
