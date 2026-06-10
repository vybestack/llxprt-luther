//! Feedback evaluator executor and adapter implementation.
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
//! @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017,REQ-PRFU-020
//! @pseudocode lines 1-23

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::engine::executor::{interpolate_string, StepContext, StepExecutor};
use crate::engine::executors::pr_followup_artifacts::{
    ArtifactWriter, ClockSleeper, PrFollowupArtifactStore, SystemPrFollowupFilesystem,
};
use crate::engine::executors::pr_followup_types::{PrFollowupBinding, PR_FOLLOWUP_SCHEMA_VERSION};
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;

pub const DEFAULT_FEEDBACK_EVALUATOR_ARGV: &[&str] = &[
    "llxprt",
    "--profile-load",
    "gpt55high",

    "--set",
    "reasoning.includeInResponse=false",
    "--yolo",
    "-p",
    "Evaluate the single CodeRabbit feedback request JSON from stdin. Respond with exactly one JSON object containing item_id, stable_marker_key, body_hash, head_sha, decision, reason, and recommended_action. Do not return arrays or extra item identities.",
];

#[must_use]
pub fn default_feedback_evaluator_argv() -> Vec<String> {
    DEFAULT_FEEDBACK_EVALUATOR_ARGV
        .iter()
        .map(|arg| (*arg).to_string())
        .collect()
}

const MAX_ATTEMPTS_PER_ITEM: u64 = 3;
const RAW_RESPONSE_LIMIT_BYTES: usize = 16 * 1024;
const DEFAULT_FEEDBACK_EVALUATOR_TIMEOUT_SECONDS: u64 = 300;

/// Single-item feedback evaluation request.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017,REQ-PRFU-020
/// @pseudocode lines 8-17
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct FeedbackEvaluationRequest {
    pub item_id: String,
    pub stable_marker_key: String,
    pub body_hash: String,
    pub head_sha: String,
    pub repository_owner: String,
    pub repository_name: String,
    pub pr_number: u64,
    pub body: String,
    pub path: Option<String>,
    pub url: Option<String>,
    pub allowed_decisions: Vec<String>,
}

/// Single-item feedback evaluation response.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017,REQ-PRFU-020
/// @pseudocode lines 10-17
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct FeedbackEvaluationResponse {
    pub item_id: String,
    pub stable_marker_key: String,
    pub body_hash: String,
    pub head_sha: String,
    pub decision: String,
    pub reason: String,
    pub recommended_action: Option<String>,
}

/// LLM invocation adapter seam for feedback evaluation behavior.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017,REQ-PRFU-020
/// @pseudocode lines 8-17
pub trait FeedbackEvaluationAdapter: Send + Sync {
    fn evaluate(&self, request: &FeedbackEvaluationRequest) -> Result<String, EngineError>;
}

/// Argv-safe command runner seam for the production feedback evaluator adapter.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 8-17
pub trait FeedbackEvaluatorCommandRunner: Send + Sync {
    fn run_feedback_evaluator_command(
        &self,
        argv: &[String],
        stdin_json: &str,
    ) -> Result<String, EngineError>;
}

/// Production command runner that passes structured request JSON on stdin and never invokes a shell.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 8-17
#[derive(Clone, Debug, Default)]
pub struct ProcessFeedbackEvaluatorCommandRunner {
    timeout: Option<Duration>,
}

impl ProcessFeedbackEvaluatorCommandRunner {
    #[must_use]
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            timeout: Some(timeout),
        }
    }

    fn timeout(&self) -> Duration {
        self.timeout
            .unwrap_or_else(|| Duration::from_secs(DEFAULT_FEEDBACK_EVALUATOR_TIMEOUT_SECONDS))
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 8-17
impl FeedbackEvaluatorCommandRunner for ProcessFeedbackEvaluatorCommandRunner {
    fn run_feedback_evaluator_command(
        &self,
        argv: &[String],
        stdin_json: &str,
    ) -> Result<String, EngineError> {
        let (program, args) = argv.split_first().ok_or_else(|| {
            feedback_eval_error("feedback evaluator command argv must not be empty")
        })?;
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| {
                feedback_eval_error(format!("spawn feedback evaluator command: {err}"))
            })?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| feedback_eval_error("feedback evaluator command stdin unavailable"))?;
        stdin
            .write_all(stdin_json.as_bytes())
            .map_err(|err| feedback_eval_error(format!("write feedback evaluator stdin: {err}")))?;
        drop(stdin);

        let status = wait_for_feedback_evaluator(&mut child, self.timeout())?;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        if let Some(mut pipe) = child.stdout.take() {
            pipe.read_to_end(&mut stdout).map_err(|err| {
                feedback_eval_error(format!("read feedback evaluator stdout: {err}"))
            })?;
        }
        if let Some(mut pipe) = child.stderr.take() {
            pipe.read_to_end(&mut stderr).map_err(|err| {
                feedback_eval_error(format!("read feedback evaluator stderr: {err}"))
            })?;
        }
        if !status.success() {
            return Err(feedback_eval_error(format!(
                "feedback evaluator command exited with status {}: {}",
                status,
                String::from_utf8_lossy(&stderr)
            )));
        }
        String::from_utf8(stdout).map_err(|err| {
            feedback_eval_error(format!("feedback evaluator stdout was not utf-8: {err}"))
        })
    }
}

fn wait_for_feedback_evaluator(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Result<std::process::ExitStatus, EngineError> {
    let started = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|err| feedback_eval_error(format!("poll feedback evaluator command: {err}")))?
        {
            return Ok(status);
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(feedback_eval_error(format!(
                "feedback evaluator command timed out after {} seconds",
                timeout.as_secs()
            )));
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// Production adapter that serializes one structured request and invokes a configured argv vector.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 8-17
#[derive(Clone, Debug)]
pub struct CommandFeedbackEvaluationAdapter<R> {
    argv: Vec<String>,
    runner: R,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 8-17
impl<R> CommandFeedbackEvaluationAdapter<R> {
    #[must_use]
    pub fn new(argv: Vec<String>, runner: R) -> Self {
        Self { argv, runner }
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 8-17
impl<R: FeedbackEvaluatorCommandRunner> FeedbackEvaluationAdapter
    for CommandFeedbackEvaluationAdapter<R>
{
    fn evaluate(&self, request: &FeedbackEvaluationRequest) -> Result<String, EngineError> {
        let stdin_json = serde_json::to_string(request)
            .map_err(|err| feedback_eval_error(format!("serialize evaluator request: {err}")))?;
        self.runner
            .run_feedback_evaluator_command(&self.argv, &stdin_json)
    }
}

/// Feedback evaluator executor for `feedback_evaluator`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017,REQ-PRFU-020
/// @pseudocode lines 1-23
#[derive(Clone)]
pub struct FeedbackEvaluatorExecutor {
    adapter: Arc<dyn FeedbackEvaluationAdapter>,
    clock: Arc<dyn ClockSleeper>,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 1-23
impl std::fmt::Debug for FeedbackEvaluatorExecutor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("FeedbackEvaluatorExecutor").finish()
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 1-23
impl FeedbackEvaluatorExecutor {
    #[must_use]
    pub fn new(
        adapter: impl FeedbackEvaluationAdapter + 'static,
        clock: impl ClockSleeper + 'static,
    ) -> Self {
        Self {
            adapter: Arc::new(adapter),
            clock: Arc::new(clock),
        }
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017,REQ-PRFU-020
/// @pseudocode lines 1-23
impl StepExecutor for FeedbackEvaluatorExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        evaluate_coderabbit_feedback(context, params, self.adapter.as_ref(), self.clock.as_ref())
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 1-23
#[derive(Clone, Debug)]
struct FeedbackItem {
    item_id: String,
    stable_marker_key: String,
    body_hash: String,
    head_sha: String,
    body: String,
    path: Option<String>,
    url: Option<String>,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 6,14-21
#[derive(Clone, Debug, serde::Serialize)]
struct FeedbackEvaluationArtifact {
    evaluation_state: String,
    items_seen: u64,
    accepted_results: Vec<Value>,
    rejected_attempts: Vec<Value>,
    unevaluated_items: Vec<Value>,
    budget_exhausted_items: Vec<Value>,
    max_attempts_per_item: u64,
    reused_results_count: u64,
    source_artifacts: Vec<Value>,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 1-23
// Pre-existing orchestration flow; split in a dedicated refactor stage.
#[allow(clippy::too_many_lines)]
fn evaluate_coderabbit_feedback(
    context: &StepContext,
    params: &Value,
    adapter: &dyn FeedbackEvaluationAdapter,
    clock: &dyn ClockSleeper,
) -> Result<StepOutcome, EngineError> {
    let artifact_root = artifact_root(context, params)?;
    let store =
        PrFollowupArtifactStore::with_filesystem(&artifact_root, &SystemPrFollowupFilesystem)?;
    let binding = read_or_build_binding(context, params, &store)?;
    let step_id = current_step_id(context, "evaluate_coderabbit_feedback");
    let step_order = u64_param(params, "step_order_index", 6);
    let max_attempts = MAX_ATTEMPTS_PER_ITEM;

    let feedback = match store.read_current_json(&binding, "coderabbit-feedback") {
        Ok(value) => value,
        Err(err) => {
            let payload = empty_artifact(
                "fatal",
                0,
                max_attempts,
                vec![json!({
                    "artifact_family": "coderabbit-feedback",
                    "error": err.to_string()
                })],
            );
            write_evaluation_artifact(
                &store,
                &binding,
                &step_id,
                step_order,
                &payload,
                clock,
                Some((
                    "fatal",
                    "missing_or_unbindable_feedback",
                    json!({ "error": err.to_string() }),
                )),
            )?;
            return Ok(StepOutcome::Fatal);
        }
    };

    if feedback.get("readiness_state").and_then(Value::as_str) != Some("ready") {
        let payload = empty_artifact(
            "fatal",
            0,
            max_attempts,
            vec![source_artifact(&feedback, "coderabbit-feedback")],
        );
        write_evaluation_artifact(
            &store,
            &binding,
            &step_id,
            step_order,
            &payload,
            clock,
            Some((
                "fatal",
                "feedback_not_ready",
                json!({ "readiness_state": feedback.get("readiness_state").cloned().unwrap_or(Value::Null) }),
            )),
        )?;
        return Ok(StepOutcome::Fatal);
    }

    let mut items = feedback_items(&feedback)?;
    items.sort_by(|left, right| {
        (
            left.stable_marker_key.as_str(),
            left.body_hash.as_str(),
            left.item_id.as_str(),
        )
            .cmp(&(
                right.stable_marker_key.as_str(),
                right.body_hash.as_str(),
                right.item_id.as_str(),
            ))
    });

    let state_value = read_or_initialize_state(&store, &binding, params)?;
    let state_entries = state_value
        .get("state_entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut accepted_results = Vec::new();
    let mut rejected_attempts = Vec::new();
    let mut unevaluated_items = Vec::new();
    let mut budget_exhausted_items = Vec::new();
    let mut fatal_reuse_errors = Vec::new();
    let mut new_state_entries = state_entries.clone();
    let mut reused_results_count = 0;

    for item in &items {
        match reusable_evaluation(&binding, item, &state_entries) {
            ReuseLookup::Reuse(value) => {
                let mut reused = value;
                set_string_field(&mut reused, "source", "reused");
                set_string_field(&mut reused, "reuse_state", "reused_from_state");
                accepted_results.push(reused);
                reused_results_count += 1;
            }
            ReuseLookup::Fatal(reason) => {
                fatal_reuse_errors.push(json!({
                    "item_id": item.item_id,
                    "stable_marker_key": item.stable_marker_key,
                    "body_hash": item.body_hash,
                    "head_sha": item.head_sha,
                    "reason": reason
                }));
                unevaluated_items.push(unevaluated_item(item, "fatal_prior_state"));
            }
            ReuseLookup::NoMatch => {
                let mut accepted: Option<Value> =
                    deterministic_feedback_evaluation(item, clock.now_rfc3339());
                if accepted.is_some() {
                    if let Some(accepted_value) = accepted.as_ref() {
                        upsert_state_entry(
                            &mut new_state_entries,
                            &binding,
                            item,
                            accepted_value,
                            clock.now_rfc3339(),
                        );
                    }
                }
                for attempt in 1..=max_attempts {
                    if accepted.is_some() {
                        break;
                    }
                    let request = build_request(&binding, item);
                    let raw = match adapter.evaluate(&request) {
                        Ok(raw) => raw,
                        Err(err) => {
                            let raw = format!("feedback evaluator command error: {err}");
                            let raw_response_artifact_path = write_raw_response(
                                &store, &binding, &step_id, step_order, item, attempt, &raw, clock,
                            )?;
                            rejected_attempts.push(json!({
                                "attempt_number": attempt,
                                "item_id": item.item_id,
                                "stable_marker_key": item.stable_marker_key,
                                "body_hash": item.body_hash,
                                "raw_response_artifact_path": raw_response_artifact_path.display().to_string(),
                                "reject_reason": "command_error",
                                "command_error": err.to_string(),
                                "parsed_decision": null,
                                "observed_head_sha": null
                            }));
                            continue;
                        }
                    };
                    let raw_response_artifact_path = write_raw_response(
                        &store, &binding, &step_id, step_order, item, attempt, &raw, clock,
                    )?;

                    match validate_response(&raw, &request) {
                        Ok(response) => {
                            let accepted_value = accepted_result(
                                &response,
                                clock.now_rfc3339(),
                                attempt,
                                "new",
                                "not_reused",
                            );
                            accepted = Some(accepted_value.clone());
                            upsert_state_entry(
                                &mut new_state_entries,
                                &binding,
                                item,
                                &accepted_value,
                                clock.now_rfc3339(),
                            );
                            break;
                        }
                        Err(reject) => rejected_attempts.push(json!({
                            "attempt_number": attempt,
                            "item_id": item.item_id,
                            "stable_marker_key": item.stable_marker_key,
                            "body_hash": item.body_hash,
                            "raw_response_artifact_path": raw_response_artifact_path.display().to_string(),
                            "reject_reason": reject.reason,
                            "parsed_decision": reject.parsed_decision,
                            "observed_head_sha": reject.observed_head_sha
                        })),
                    }
                }
                if let Some(value) = accepted {
                    accepted_results.push(value);
                } else {
                    budget_exhausted_items.push(json!({
                        "item_id": item.item_id,
                        "stable_marker_key": item.stable_marker_key,
                        "body_hash": item.body_hash,
                        "head_sha": item.head_sha,
                        "attempts": max_attempts
                    }));
                }
            }
        }
    }

    let complete = fatal_reuse_errors.is_empty()
        && budget_exhausted_items.is_empty()
        && unevaluated_items.is_empty()
        && exactly_one_accepted_per_item(&items, &accepted_results);
    let evaluation_state = if complete {
        "complete"
    } else if !budget_exhausted_items.is_empty() {
        "budget_exhausted"
    } else if !fatal_reuse_errors.is_empty() {
        "fatal"
    } else {
        "incomplete"
    };

    let payload = FeedbackEvaluationArtifact {
        evaluation_state: evaluation_state.to_string(),
        items_seen: items.len() as u64,
        accepted_results,
        rejected_attempts,
        unevaluated_items,
        budget_exhausted_items,
        max_attempts_per_item: max_attempts,
        reused_results_count,
        source_artifacts: vec![source_artifact(&feedback, "coderabbit-feedback")],
    };

    if complete {
        write_state_artifact(
            &store,
            &binding,
            &step_id,
            step_order,
            new_state_entries,
            clock,
        )?;
    }

    let failure = if complete {
        None
    } else {
        Some((
            evaluation_state,
            if evaluation_state == "budget_exhausted" {
                "evaluation_budget_exhausted"
            } else {
                "evaluation_incomplete_or_fatal"
            },
            json!({
                "fatal_reuse_errors": fatal_reuse_errors,
                "items_seen": items.len()
            }),
        ))
    };
    write_evaluation_artifact(
        &store, &binding, &step_id, step_order, &payload, clock, failure,
    )?;

    Ok(if complete {
        StepOutcome::Success
    } else {
        StepOutcome::Fatal
    })
}

#[derive(Debug)]
struct RejectReason {
    reason: String,
    parsed_decision: Option<String>,
    observed_head_sha: Option<String>,
}

#[derive(Debug)]
enum ReuseLookup {
    Reuse(Value),
    NoMatch,
    Fatal(String),
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012
/// @pseudocode lines 5-7
fn reusable_evaluation(
    binding: &PrFollowupBinding,
    item: &FeedbackItem,
    state_entries: &[Value],
) -> ReuseLookup {
    let mut matching = Vec::new();
    let mut stale_or_malformed = Vec::new();
    for entry in state_entries {
        let Some(object) = entry.as_object() else {
            stale_or_malformed.push("non_object_state_entry".to_string());
            continue;
        };
        let key_matches = object.get("stable_marker_key").and_then(Value::as_str)
            == Some(item.stable_marker_key.as_str());
        if !key_matches {
            continue;
        }
        let body_matches =
            object.get("body_hash").and_then(Value::as_str) == Some(item.body_hash.as_str());
        let head_matches =
            object.get("head_sha").and_then(Value::as_str) == Some(item.head_sha.as_str());
        if !(body_matches && head_matches) {
            stale_or_malformed.push("stale_state_for_current_marker_key".to_string());
            continue;
        }
        matching.push(entry.clone());
    }
    if matching.len() > 1 {
        return ReuseLookup::Fatal("duplicate_accepted_evaluation_state".to_string());
    }
    if let Some(entry) = matching.pop() {
        if entry.get("reuse_eligible").and_then(Value::as_bool) != Some(true) {
            return ReuseLookup::NoMatch;
        }
        let Some(accepted) = entry.get("accepted_evaluation").cloned() else {
            return ReuseLookup::Fatal("missing_accepted_evaluation".to_string());
        };
        if validate_reusable_accepted(binding, item, &accepted).is_err() {
            return ReuseLookup::Fatal("malformed_or_unbindable_accepted_evaluation".to_string());
        }
        return ReuseLookup::Reuse(accepted);
    }
    ReuseLookup::NoMatch
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012
/// @pseudocode lines 10-13
fn validate_response(
    raw: &str,
    request: &FeedbackEvaluationRequest,
) -> Result<FeedbackEvaluationResponse, RejectReason> {
    let value = parse_feedback_evaluator_json(raw).map_err(|err| RejectReason {
        reason: format!("malformed_json: {err}"),
        parsed_decision: None,
        observed_head_sha: None,
    })?;
    if value.is_array() {
        return Err(reject("response_array_or_batch", &value));
    }
    let object = value
        .as_object()
        .ok_or_else(|| reject("response_not_object", &value))?;
    for (field, field_value) in object {
        let lower = field.to_ascii_lowercase();
        let is_allowed_identity = matches!(
            field.as_str(),
            "item_id" | "stable_marker_key" | "body_hash" | "head_sha"
        );
        let is_extra_identity = !is_allowed_identity
            && (lower.contains("item")
                || lower.contains("stable_marker")
                || lower.contains("body_hash")
                || lower.contains("head_sha")
                || lower.contains("marker_key"));
        let is_batch_field = matches!(
            field.as_str(),
            "items"
                | "item_ids"
                | "feedback_items"
                | "feedback_item_ids"
                | "batch"
                | "batches"
                | "results"
                | "evaluations"
        );
        if is_batch_field || is_extra_identity || field_value.is_array() {
            return Err(reject("batch_or_extra_item_ids", &value));
        }
    }

    let response = FeedbackEvaluationResponse {
        item_id: required_value_string(&value, "item_id")?,
        stable_marker_key: required_value_string(&value, "stable_marker_key")?,
        body_hash: required_value_string(&value, "body_hash")?,
        head_sha: required_value_string(&value, "head_sha")?,
        decision: required_value_string(&value, "decision")?,
        reason: value
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        recommended_action: value
            .get("recommended_action")
            .and_then(Value::as_str)
            .map(ToString::to_string),
    };
    if response.item_id != request.item_id {
        return Err(reject("wrong_item_id", &value));
    }
    if response.stable_marker_key != request.stable_marker_key {
        return Err(reject("wrong_stable_marker_key", &value));
    }
    if response.body_hash != request.body_hash {
        return Err(reject("wrong_body_hash", &value));
    }
    if response.head_sha != request.head_sha {
        return Err(reject("wrong_head_sha", &value));
    }
    if !matches!(
        response.decision.as_str(),
        "valid" | "invalid" | "out_of_scope" | "needs_user_judgment"
    ) {
        return Err(reject("unknown_decision", &value));
    }
    if response.decision != "valid" && response.reason.trim().is_empty() {
        return Err(reject("missing_required_reason", &value));
    }
    if response
        .recommended_action
        .as_deref()
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        return Err(reject("missing_recommended_action", &value));
    }
    Ok(response)
}

fn parse_feedback_evaluator_json(raw: &str) -> Result<Value, serde_json::Error> {
    match serde_json::from_str(raw) {
        Ok(value) => Ok(value),
        Err(original) => {
            for (index, _) in raw.match_indices('{') {
                let mut stream =
                    serde_json::Deserializer::from_str(&raw[index..]).into_iter::<Value>();
                if let Some(Ok(value)) = stream.next() {
                    if value.is_object() {
                        return Ok(value);
                    }
                }
            }
            Err(original)
        }
    }
}

fn deterministic_feedback_evaluation(item: &FeedbackItem, accepted_at: String) -> Option<Value> {
    if !is_coderabbit_summary_item(item) {
        return None;
    }
    Some(json!({
        "item_id": item.item_id,
        "stable_marker_key": item.stable_marker_key,
        "body_hash": item.body_hash,
        "head_sha": item.head_sha,
        "decision": "invalid",
        "reason": "CodeRabbit summary/walkthrough comments are informational and do not identify a specific actionable feedback item.",
        "recommended_action": "No code changes or review-thread response are required for the summary comment.",
        "accepted_at": accepted_at,
        "attempt_count": 0,
        "source": "deterministic",
        "reuse_state": "not_reused"
    }))
}

fn is_coderabbit_summary_item(item: &FeedbackItem) -> bool {
    let key = item.stable_marker_key.to_ascii_lowercase();
    let body = item.body.to_ascii_lowercase();
    key.starts_with("summary:")
        || body.contains("summary by coderabbit")
        || (body.contains("walkthrough") && body.contains("coderabbit"))
        || body.contains("coderabbit finished reviewing this pull request")
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012
/// @pseudocode lines 4,8
fn feedback_items(feedback: &Value) -> Result<Vec<FeedbackItem>, EngineError> {
    let array = feedback
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| feedback_eval_error("coderabbit-feedback missing items array"))?;
    array
        .iter()
        .map(|item| {
            Ok(FeedbackItem {
                item_id: require_string(item, "item_id")?,
                stable_marker_key: require_string(item, "stable_marker_key")?,
                body_hash: require_string(item, "body_hash")?,
                head_sha: item
                    .get("commit_sha")
                    .and_then(Value::as_str)
                    .or_else(|| item.get("head_sha").and_then(Value::as_str))
                    .ok_or_else(|| {
                        feedback_eval_error("feedback item missing commit_sha/head_sha")
                    })?
                    .to_string(),
                body: require_string(item, "body")?,
                path: item
                    .get("path")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                url: item
                    .get("url")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
            })
        })
        .collect()
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011
/// @pseudocode lines 8-9
fn build_request(binding: &PrFollowupBinding, item: &FeedbackItem) -> FeedbackEvaluationRequest {
    FeedbackEvaluationRequest {
        item_id: item.item_id.clone(),
        stable_marker_key: item.stable_marker_key.clone(),
        body_hash: item.body_hash.clone(),
        head_sha: item.head_sha.clone(),
        repository_owner: binding.repository_owner.clone(),
        repository_name: binding.repository_name.clone(),
        pr_number: binding.pr_number,
        body: item.body.clone(),
        path: item.path.clone(),
        url: item.url.clone(),
        allowed_decisions: vec![
            "valid".to_string(),
            "invalid".to_string(),
            "out_of_scope".to_string(),
            "needs_user_judgment".to_string(),
        ],
    }
}

fn accepted_result(
    response: &FeedbackEvaluationResponse,
    accepted_at: String,
    attempt_count: u64,
    source: &str,
    reuse_state: &str,
) -> Value {
    json!({
        "item_id": response.item_id,
        "stable_marker_key": response.stable_marker_key,
        "body_hash": response.body_hash,
        "head_sha": response.head_sha,
        "decision": response.decision,
        "reason": response.reason,
        "recommended_action": response.recommended_action.clone().unwrap_or_default(),
        "accepted_at": accepted_at,
        "attempt_count": attempt_count,
        "source": source,
        "reuse_state": reuse_state
    })
}

fn validate_reusable_accepted(
    binding: &PrFollowupBinding,
    item: &FeedbackItem,
    value: &Value,
) -> Result<(), EngineError> {
    let decision = require_string(value, "decision")?;
    if !matches!(
        decision.as_str(),
        "valid" | "invalid" | "out_of_scope" | "needs_user_judgment"
    ) {
        return Err(feedback_eval_error("unknown reusable decision"));
    }
    if require_string(value, "item_id")? != item.item_id
        || require_string(value, "stable_marker_key")? != item.stable_marker_key
        || require_string(value, "body_hash")? != item.body_hash
        || require_string(value, "head_sha")? != item.head_sha
    {
        return Err(feedback_eval_error("reusable evaluation identity mismatch"));
    }
    if require_string(value, "repository_owner")? != binding.repository_owner
        || require_string(value, "repository_name")? != binding.repository_name
        || require_u64(value, "pr_number")? != binding.pr_number
    {
        return Err(feedback_eval_error("reusable evaluation binding mismatch"));
    }

    if decision != "valid"
        && value
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .is_empty()
    {
        return Err(feedback_eval_error("missing reusable reason"));
    }
    Ok(())
}

fn upsert_state_entry(
    entries: &mut Vec<Value>,
    binding: &PrFollowupBinding,
    item: &FeedbackItem,
    accepted: &Value,
    timestamp: String,
) {
    entries.retain(|entry| {
        !(entry.get("stable_marker_key").and_then(Value::as_str)
            == Some(item.stable_marker_key.as_str())
            && entry.get("body_hash").and_then(Value::as_str) == Some(item.body_hash.as_str())
            && entry.get("head_sha").and_then(Value::as_str) == Some(item.head_sha.as_str()))
    });
    let mut accepted_with_binding = accepted.clone();
    if let Some(object) = accepted_with_binding.as_object_mut() {
        object.insert(
            "repository_owner".to_string(),
            Value::from(binding.repository_owner.clone()),
        );
        object.insert(
            "repository_name".to_string(),
            Value::from(binding.repository_name.clone()),
        );
        object.insert("pr_number".to_string(), Value::from(binding.pr_number));
    }
    entries.push(json!({
        "item_id": item.item_id,
        "stable_marker_key": item.stable_marker_key,
        "body_hash": item.body_hash,
        "head_sha": item.head_sha,
        "first_seen_at": timestamp,
        "last_seen_at": timestamp,
        "evaluation_status": "accepted",
        "accepted_evaluation": accepted_with_binding,
        "remediation_status": "pending",
        "marker_status": "pending",
        "resolution_status": "pending",
        "superseded": false,
        "stale": false,
        "reuse_eligible": true
    }));
}

fn exactly_one_accepted_per_item(items: &[FeedbackItem], accepted: &[Value]) -> bool {
    let mut counts: BTreeMap<(String, String, String), u64> = BTreeMap::new();
    for value in accepted {
        let key = (
            value
                .get("item_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            value
                .get("body_hash")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            value
                .get("head_sha")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        );
        *counts.entry(key).or_default() += 1;
    }
    items.iter().all(|item| {
        counts.get(&(
            item.item_id.clone(),
            item.body_hash.clone(),
            item.head_sha.clone(),
        )) == Some(&1)
    }) && counts.len() == items.len()
}

fn empty_artifact(
    state: &str,
    items_seen: u64,
    max_attempts: u64,
    source_artifacts: Vec<Value>,
) -> FeedbackEvaluationArtifact {
    FeedbackEvaluationArtifact {
        evaluation_state: state.to_string(),
        items_seen,
        accepted_results: Vec::new(),
        rejected_attempts: Vec::new(),
        unevaluated_items: Vec::new(),
        budget_exhausted_items: Vec::new(),
        max_attempts_per_item: max_attempts,
        reused_results_count: 0,
        source_artifacts,
    }
}

fn source_artifact(value: &Value, family: &str) -> Value {
    json!({
        "artifact_family": family,
        "artifact_sequence": value.get("artifact_sequence").cloned().unwrap_or(Value::Null),
        "write_sequence": value.get("write_sequence").cloned().unwrap_or(Value::Null),
        "producer_step_id": value.get("producer_step_id").cloned().unwrap_or(Value::Null)
    })
}

fn unevaluated_item(item: &FeedbackItem, reason: &str) -> Value {
    json!({
        "item_id": item.item_id,
        "stable_marker_key": item.stable_marker_key,
        "body_hash": item.body_hash,
        "head_sha": item.head_sha,
        "reason": reason
    })
}

fn reject(reason: &str, value: &Value) -> RejectReason {
    RejectReason {
        reason: reason.to_string(),
        parsed_decision: value
            .get("decision")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        observed_head_sha: value
            .get("head_sha")
            .and_then(Value::as_str)
            .map(ToString::to_string),
    }
}

fn required_value_string(value: &Value, field: &str) -> Result<String, RejectReason> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| reject(&format!("missing_{field}"), value))
}

fn set_string_field(value: &mut Value, field: &str, text: &str) {
    if let Some(object) = value.as_object_mut() {
        object.insert(field.to_string(), Value::from(text));
    }
}

fn write_evaluation_artifact(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    payload: &FeedbackEvaluationArtifact,
    clock: &dyn ClockSleeper,
    failure: Option<(&str, &str, Value)>,
) -> Result<(), EngineError> {
    store.write_json_artifact(
        binding,
        "feedback-evaluations",
        step_id,
        step_order,
        payload,
        failure,
        clock,
    )?;
    Ok(())
}

fn write_state_artifact(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    entries: Vec<Value>,
    clock: &dyn ClockSleeper,
) -> Result<(), EngineError> {
    let state_index_hash = stable_json_hash(&Value::Array(entries.clone()));
    store.write_json_artifact(
        binding,
        "coderabbit-feedback-state",
        step_id,
        step_order,
        &json!({
            "state_entries": entries,
            "state_index_hash": state_index_hash,
            "superseded_entries": []
        }),
        None,
        clock,
    )?;
    Ok(())
}

// Pre-existing artifact writer shape shared by follow-up executors.
#[allow(clippy::too_many_arguments)]
fn write_raw_response(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,

    item: &FeedbackItem,
    attempt: u64,
    raw: &str,
    clock: &dyn ClockSleeper,
) -> Result<PathBuf, EngineError> {
    let bounded = if raw.len() > RAW_RESPONSE_LIMIT_BYTES {
        &raw[..RAW_RESPONSE_LIMIT_BYTES]
    } else {
        raw
    };
    let record = store.write_raw_text_artifact(
        binding,
        "feedback-evaluator-raw-output",
        step_id,
        step_order,
        &format!(
            "{}-attempt-{}-raw-output",
            sanitize_path_segment(&item.item_id),
            attempt
        ),
        bounded,
        clock,
    )?;
    Ok(record.history_path)
}

fn read_or_initialize_state(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    params: &Value,
) -> Result<Value, EngineError> {
    let path = store.canonical_path(binding, "coderabbit-feedback-state");
    if path.exists() {
        return store.read_current_json(binding, "coderabbit-feedback-state");
    }
    Ok(json!({
        "schema_version": PR_FOLLOWUP_SCHEMA_VERSION,
        "run_id": binding.run_id,
        "repository_owner": binding.repository_owner,
        "repository_name": binding.repository_name,
        "pr_number": binding.pr_number,
        "head_ref": binding.head_ref,
        "head_sha": binding.head_sha,
        "base_ref": binding.base_ref,
        "base_sha": binding.base_sha,
        "state_entries": params.get("state_entries").cloned().unwrap_or_else(|| json!([])),
        "state_index_hash": "empty",
        "superseded_entries": []
    }))
}

fn read_or_build_binding(
    context: &StepContext,
    params: &Value,
    store: &PrFollowupArtifactStore,
) -> Result<PrFollowupBinding, EngineError> {
    let fallback = PrFollowupBinding {
        schema_version: PR_FOLLOWUP_SCHEMA_VERSION,
        run_id: context.run_id().to_string(),
        repository_owner: string_param(context, params, "repository_owner", "example"),
        repository_name: string_param(context, params, "repository_name", "workflow"),
        pr_number: string_param(context, params, "pr_number", "1910")
            .parse()
            .unwrap_or(1910),
        head_ref: string_param(context, params, "head_ref", "feature"),
        head_sha: string_param(
            context,
            params,
            "head_sha",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
        base_ref: string_param(context, params, "base_ref", "main"),
        base_sha: Some(string_param(context, params, "base_sha", "base-a")),
    };

    let pr_path = store.canonical_path(&fallback, "pr");
    if pr_path.exists() {
        let value = read_json_file(&pr_path)?;
        return binding_from_value(&value);
    }
    Ok(fallback)
}

fn artifact_root(context: &StepContext, params: &Value) -> Result<PathBuf, EngineError> {
    let raw = params
        .get("artifact_root")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| feedback_eval_error("missing artifact_root"))?;
    let interpolated = interpolate_string(raw, context);
    if has_unresolved_template(&interpolated) {
        return Err(feedback_eval_error(format!(
            "artifact_root contains unresolved template: {interpolated}"
        )));
    }
    let path = PathBuf::from(interpolated);
    Ok(if path.is_absolute() {
        path
    } else {
        context.work_dir().join(path)
    })
}

fn read_json_file(path: &std::path::Path) -> Result<Value, EngineError> {
    let content = std::fs::read_to_string(path)
        .map_err(|err| feedback_eval_error(format!("read {}: {err}", path.display())))?;
    serde_json::from_str(&content)
        .map_err(|err| feedback_eval_error(format!("parse {}: {err}", path.display())))
}

fn binding_from_value(value: &Value) -> Result<PrFollowupBinding, EngineError> {
    Ok(PrFollowupBinding {
        schema_version: u32::try_from(require_u64(value, "schema_version")?)
            .map_err(|err| feedback_eval_error(format!("schema_version out of range: {err}")))?,
        run_id: require_string(value, "run_id")?,
        repository_owner: require_string(value, "repository_owner")?,
        repository_name: require_string(value, "repository_name")?,
        pr_number: require_u64(value, "pr_number")?,
        head_ref: require_string(value, "head_ref")?,
        head_sha: require_string(value, "head_sha")?,
        base_ref: require_string(value, "base_ref")?,
        base_sha: value
            .get("base_sha")
            .and_then(Value::as_str)
            .map(ToString::to_string),
    })
}

fn require_string(value: &Value, field: &str) -> Result<String, EngineError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| feedback_eval_error(format!("missing string field {field}")))
}

fn require_u64(value: &Value, field: &str) -> Result<u64, EngineError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| feedback_eval_error(format!("missing integer field {field}")))
}

fn string_param(context: &StepContext, params: &Value, key: &str, default: &str) -> String {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(|value| interpolate_string(value, context))
        .filter(|value| !value.is_empty() && !has_unresolved_template(value))
        .or_else(|| {
            context
                .get(key)
                .filter(|value| !value.is_empty() && !has_unresolved_template(value))
                .cloned()
        })
        .unwrap_or_else(|| default.to_string())
}

fn has_unresolved_template(value: &str) -> bool {
    value.contains('{') || value.contains('}')
}

fn u64_param(params: &Value, key: &str, default: u64) -> u64 {
    params.get(key).and_then(Value::as_u64).unwrap_or(default)
}

fn current_step_id(context: &StepContext, default: &str) -> String {
    context
        .get("current_step_id")
        .cloned()
        .unwrap_or_else(|| default.to_string())
}

fn sanitize_path_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn stable_json_hash(value: &Value) -> String {
    let text = serde_json::to_string(value).unwrap_or_default();
    let mut hash = 14_695_981_039_346_656_037_u64;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    format!("fnv64:{hash:016x}")
}

fn feedback_eval_error(message: impl Into<String>) -> EngineError {
    EngineError::InvalidState(format!("feedback evaluator: {}", message.into()))
}
