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
use crate::engine::executors::feedback_eval_policy::{
    apply_low_confidence_accepted_policy, apply_low_confidence_needs_judgment_policy,
    feedback_response_from_value, is_forbidden_response_field, parse_feedback_evaluator_json,
    FeedbackEvaluationAdapter,
};
use crate::engine::executors::pr_followup_artifacts::{
    ArtifactWriter, ClockSleeper, PrFollowupArtifactStore, SystemPrFollowupFilesystem,
};
use crate::engine::executors::pr_followup_types::{
    EvaluationState, PrFollowupBinding, PR_FOLLOWUP_SCHEMA_VERSION, SUMMARY_MARKER_KEY_PREFIX,
};
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;

pub const DEFAULT_FEEDBACK_EVALUATOR_ARGV: &[&str] = &[
    "llxprt",
    "--profile-load",
    "gpt55high",
    "--set",
    "reasoning.includeInResponse=false",
    "--set",
    "maxTurnsPerPrompt=1",
    "-p",
    "Evaluate the single PR review feedback request JSON from stdin. Classify it using only the JSON provided; do not use any tools, do not run commands, and do not inspect the repository. Use needs_user_judgment only when the comment asks for a genuine product/scope/design choice that cannot be decided from the current PR. Speculative robustness suggestions, low-value nits, optional future hardening, and comments phrased as consider/if this becomes an issue should be invalid or out_of_scope unless they identify a concrete current defect. Respond with exactly one JSON object containing item_id, stable_marker_key, body_hash, head_sha, decision, reason, recommended_action, and response_text. The response_text must be a non-empty, reviewer-facing message that Luther will post verbatim on the original review thread explaining the decision; do not address the reviewer as yourself or claim to have posted it. Do not return arrays or extra item identities.",
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
pub(super) const DEFAULT_FEEDBACK_EVALUATOR_TIMEOUT_SECONDS: u64 = 300;

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
    pub author_login: String,
    pub author_kind: Option<String>,
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
    pub response_text: String,
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
            .unwrap_or_else(super::feedback_eval_timeout::default_evaluator_timeout)
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
    author_login: String,
    author_kind: Option<String>,
    body: String,
    path: Option<String>,
    url: Option<String>,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 6,14-21
#[derive(Clone, Debug, serde::Serialize)]
struct FeedbackEvaluationArtifact {
    evaluation_state: EvaluationState,
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
                EvaluationState::Fatal,
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
            EvaluationState::Fatal,
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
        EvaluationState::Complete
    } else if !budget_exhausted_items.is_empty() {
        EvaluationState::BudgetExhausted
    } else if !fatal_reuse_errors.is_empty() {
        EvaluationState::Fatal
    } else {
        EvaluationState::Incomplete
    };

    let payload = FeedbackEvaluationArtifact {
        evaluation_state,
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
            evaluation_state.as_str(),
            match evaluation_state {
                EvaluationState::BudgetExhausted => "evaluation_budget_exhausted",
                _ => "evaluation_incomplete_or_fatal",
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
pub(super) struct RejectReason {
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
        let Some(mut accepted) = entry.get("accepted_evaluation").cloned() else {
            return ReuseLookup::Fatal("missing_accepted_evaluation".to_string());
        };
        if validate_reusable_accepted(binding, item, &accepted).is_err() {
            return ReuseLookup::Fatal("malformed_or_unbindable_accepted_evaluation".to_string());
        }
        apply_low_confidence_accepted_policy(
            &item.body,
            &item.author_login,
            item.author_kind.as_deref(),
            &mut accepted,
        );
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
    reject_batch_response_fields(&value)?;

    let mut response = feedback_response_from_value(&value)?;
    apply_low_confidence_needs_judgment_policy(request, &mut response);
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
    if response.response_text.trim().is_empty() {
        return Err(reject("missing_response_text", &value));
    }
    Ok(response)
}

fn reject_batch_response_fields(value: &Value) -> Result<(), RejectReason> {
    if value.is_array() {
        return Err(reject("response_array_or_batch", value));
    }
    let object = value
        .as_object()
        .ok_or_else(|| reject("response_not_object", value))?;
    for (field, field_value) in object {
        if is_forbidden_response_field(field, field_value) {
            return Err(reject("batch_or_extra_item_ids", value));
        }
    }
    Ok(())
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
        "response_text": "This is an informational CodeRabbit summary/walkthrough comment rather than an actionable review item, so no code change is required.",
        "accepted_at": accepted_at,
        "attempt_count": 0,
        "source": "deterministic",
        "reuse_state": "not_reused"
    }))
}

fn is_coderabbit_summary_item(item: &FeedbackItem) -> bool {
    let key = item.stable_marker_key.to_ascii_lowercase();
    let body = item.body.to_ascii_lowercase();
    key.starts_with(SUMMARY_MARKER_KEY_PREFIX)
        || body.contains("summary by coderabbit")
        || body.contains("summarize by coderabbit")
        || (body.contains("walkthrough") && body.contains("coderabbit"))
        || body.contains("coderabbit finished reviewing this pull request")
        || body.contains("rate limited by coderabbit")
        || body.contains("review limit reached")
        || (body.contains("coderabbit") && body.contains("run out of usage credits"))
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
                author_login: item
                    .get("author_login")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                author_kind: item
                    .get("author_kind")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
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
        author_kind: item.author_kind.clone(),
        pr_number: binding.pr_number,
        author_login: item.author_login.clone(),
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
        "response_text": response.response_text,
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
    if value
        .get("response_text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        return Err(feedback_eval_error("missing reusable response_text"));
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
    state: EvaluationState,
    items_seen: u64,
    max_attempts: u64,
    source_artifacts: Vec<Value>,
) -> FeedbackEvaluationArtifact {
    FeedbackEvaluationArtifact {
        evaluation_state: state,
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

pub(super) fn required_value_string(value: &Value, field: &str) -> Result<String, RejectReason> {
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
    let requested = PrFollowupBinding {
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

    if let Some(value) = store.find_current_pr_artifact_for_run(context.run_id(), &requested)? {
        return binding_from_value(&value);
    }
    Ok(requested)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> FeedbackItem {
        FeedbackItem {
            item_id: id.to_string(),
            stable_marker_key: format!("thread:{id}"),
            body_hash: format!("hash-{id}"),
            head_sha: "sha-head".to_string(),
            author_login: "coderabbitai".to_string(),
            author_kind: Some("bot".to_string()),
            body: "some feedback body".to_string(),
            path: Some("src/foo.rs".to_string()),
            url: Some("https://example/1".to_string()),
        }
    }

    fn binding() -> PrFollowupBinding {
        PrFollowupBinding {
            schema_version: PR_FOLLOWUP_SCHEMA_VERSION,
            run_id: "run-1".to_string(),
            repository_owner: "acme".to_string(),
            repository_name: "widget".to_string(),
            pr_number: 42,
            head_ref: "feature".to_string(),
            head_sha: "sha-head".to_string(),
            base_ref: "main".to_string(),
            base_sha: Some("base-sha".to_string()),
        }
    }

    fn request(it: &FeedbackItem, b: &PrFollowupBinding) -> FeedbackEvaluationRequest {
        build_request(b, it)
    }

    #[test]
    fn default_argv_is_nonempty_and_starts_with_llxprt() {
        let argv = default_feedback_evaluator_argv();
        assert!(!argv.is_empty());
        assert_eq!(argv[0], "llxprt");
    }

    #[test]
    fn build_request_copies_identity_and_binding() {
        let it = item("a");
        let b = binding();
        let req = build_request(&b, &it);
        assert_eq!(req.item_id, "a");
        assert_eq!(req.stable_marker_key, "thread:a");
        assert_eq!(req.body_hash, "hash-a");
        assert_eq!(req.head_sha, "sha-head");
        assert_eq!(req.repository_owner, "acme");
        assert_eq!(req.repository_name, "widget");
        assert_eq!(req.pr_number, 42);
        assert_eq!(req.author_login, "coderabbitai");
        assert_eq!(
            req.allowed_decisions,
            vec!["valid", "invalid", "out_of_scope", "needs_user_judgment"]
        );
    }

    #[test]
    fn feedback_items_parses_valid_items() {
        let feedback = json!({
            "items": [
                {
                    "item_id": "i1",
                    "stable_marker_key": "thread:i1",
                    "body_hash": "h1",
                    "commit_sha": "sha1",
                    "author_login": "coderabbitai",
                    "author_kind": "bot",
                    "body": "body text",
                    "path": "a.rs",
                    "url": "u"
                }
            ]
        });
        let items = feedback_items(&feedback).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].item_id, "i1");
        assert_eq!(items[0].head_sha, "sha1");
        assert_eq!(items[0].path.as_deref(), Some("a.rs"));
    }

    #[test]
    fn feedback_items_prefers_commit_sha_then_head_sha() {
        let feedback = json!({
            "items": [
                {"item_id":"i","stable_marker_key":"k","body_hash":"h","head_sha":"fallback","body":"b"}
            ]
        });
        let items = feedback_items(&feedback).unwrap();
        assert_eq!(items[0].head_sha, "fallback");
    }

    #[test]
    fn feedback_items_missing_array_errors() {
        let err = feedback_items(&json!({})).unwrap_err();
        assert!(format!("{err:?}").contains("missing items array"));
    }

    #[test]
    fn feedback_items_missing_sha_errors() {
        let feedback =
            json!({"items":[{"item_id":"i","stable_marker_key":"k","body_hash":"h","body":"b"}]});
        let err = feedback_items(&feedback).unwrap_err();
        assert!(format!("{err:?}").contains("commit_sha/head_sha"));
    }

    #[test]
    fn is_coderabbit_summary_item_detects_summary_prefix() {
        let mut it = item("s");
        it.stable_marker_key = "summary:xyz".to_string();
        assert!(is_coderabbit_summary_item(&it));
    }

    #[test]
    fn is_coderabbit_summary_item_detects_body_markers() {
        let mut it = item("s");
        it.stable_marker_key = "thread:s".to_string();
        it.body = "Summary by CodeRabbit".to_string();
        assert!(is_coderabbit_summary_item(&it));
        it.body = "review limit reached".to_string();
        assert!(is_coderabbit_summary_item(&it));
        it.body = "Walkthrough from coderabbit here".to_string();
        assert!(is_coderabbit_summary_item(&it));
    }

    #[test]
    fn is_coderabbit_summary_item_rejects_regular_feedback() {
        let it = item("s");
        assert!(!is_coderabbit_summary_item(&it));
    }

    #[test]
    fn deterministic_evaluation_returns_invalid_for_summary() {
        let mut it = item("s");
        it.stable_marker_key = "summary:x".to_string();
        let value = deterministic_feedback_evaluation(&it, "2026-01-01T00:00:00Z".to_string())
            .expect("summary yields deterministic result");
        assert_eq!(value.get("decision").unwrap(), "invalid");
        assert_eq!(value.get("source").unwrap(), "deterministic");
        assert_eq!(value.get("attempt_count").unwrap(), 0);
    }

    #[test]
    fn deterministic_evaluation_none_for_regular_item() {
        let it = item("s");
        assert!(deterministic_feedback_evaluation(&it, "t".to_string()).is_none());
    }

    #[test]
    fn validate_response_accepts_well_formed_valid_decision() {
        let it = item("a");
        let b = binding();
        let req = request(&it, &b);
        let raw = json!({
            "item_id": "a",
            "stable_marker_key": "thread:a",
            "body_hash": "hash-a",
            "head_sha": "sha-head",
            "decision": "valid",
            "reason": "",
            "recommended_action": "do the thing",
            "response_text": "Valid finding."
        })
        .to_string();
        let resp = validate_response(&raw, &req).expect("valid response");
        assert_eq!(resp.decision, "valid");
        assert_eq!(resp.item_id, "a");
    }

    #[test]
    fn validate_response_rejects_wrong_item_id() {
        let it = item("a");
        let b = binding();
        let req = request(&it, &b);
        let raw = json!({
            "item_id": "WRONG",
            "stable_marker_key": "thread:a",
            "body_hash": "hash-a",
            "head_sha": "sha-head",
            "decision": "invalid",
            "reason": "r",
            "recommended_action": "x",
            "response_text": "y"
        })
        .to_string();
        let err = validate_response(&raw, &req).unwrap_err();
        assert_eq!(err.reason, "wrong_item_id");
    }

    #[test]
    fn validate_response_rejects_missing_reason_for_non_valid() {
        let it = item("a");
        let b = binding();
        let req = request(&it, &b);
        let raw = json!({
            "item_id": "a",
            "stable_marker_key": "thread:a",
            "body_hash": "hash-a",
            "head_sha": "sha-head",
            "decision": "invalid",
            "reason": "   ",
            "recommended_action": "x",
            "response_text": "y"
        })
        .to_string();
        let err = validate_response(&raw, &req).unwrap_err();
        assert_eq!(err.reason, "missing_required_reason");
    }

    #[test]
    fn validate_response_rejects_missing_response_text() {
        let it = item("a");
        let b = binding();
        let req = request(&it, &b);
        let raw = json!({
            "item_id": "a",
            "stable_marker_key": "thread:a",
            "body_hash": "hash-a",
            "head_sha": "sha-head",
            "decision": "valid",
            "reason": "",
            "recommended_action": "x",
            "response_text": "  "
        })
        .to_string();
        let err = validate_response(&raw, &req).unwrap_err();
        assert_eq!(err.reason, "missing_response_text");
    }

    #[test]
    fn validate_response_rejects_array_batch() {
        let it = item("a");
        let b = binding();
        let req = request(&it, &b);
        let raw = json!([{"item_id":"a"}]).to_string();
        let err = validate_response(&raw, &req).unwrap_err();
        assert_eq!(err.reason, "response_array_or_batch");
    }

    #[test]
    fn validate_response_rejects_unknown_decision() {
        let it = item("a");
        let b = binding();
        let req = request(&it, &b);
        let raw = json!({
            "item_id": "a",
            "stable_marker_key": "thread:a",
            "body_hash": "hash-a",
            "head_sha": "sha-head",
            "decision": "maybe",
            "reason": "r",
            "recommended_action": "x",
            "response_text": "y"
        })
        .to_string();
        let err = validate_response(&raw, &req).unwrap_err();
        assert_eq!(err.reason, "unknown_decision");
    }

    #[test]
    fn validate_reusable_accepted_checks_identity_and_binding() {
        let it = item("a");
        let b = binding();
        let value = json!({
            "item_id": "a",
            "stable_marker_key": "thread:a",
            "body_hash": "hash-a",
            "head_sha": "sha-head",
            "decision": "invalid",
            "reason": "r",
            "response_text": "y",
            "repository_owner": "acme",
            "repository_name": "widget",
            "pr_number": 42
        });
        assert!(validate_reusable_accepted(&b, &it, &value).is_ok());
    }

    #[test]
    fn validate_reusable_accepted_rejects_binding_mismatch() {
        let it = item("a");
        let b = binding();
        let value = json!({
            "item_id": "a",
            "stable_marker_key": "thread:a",
            "body_hash": "hash-a",
            "head_sha": "sha-head",
            "decision": "invalid",
            "reason": "r",
            "response_text": "y",
            "repository_owner": "acme",
            "repository_name": "widget",
            "pr_number": 999
        });
        assert!(validate_reusable_accepted(&b, &it, &value).is_err());
    }

    #[test]
    fn exactly_one_accepted_per_item_true_when_balanced() {
        let items = vec![item("a"), item("b")];
        let accepted = vec![
            json!({"item_id":"a","body_hash":"hash-a","head_sha":"sha-head"}),
            json!({"item_id":"b","body_hash":"hash-b","head_sha":"sha-head"}),
        ];
        assert!(exactly_one_accepted_per_item(&items, &accepted));
    }

    #[test]
    fn exactly_one_accepted_per_item_false_on_duplicate() {
        let items = vec![item("a")];
        let accepted = vec![
            json!({"item_id":"a","body_hash":"hash-a","head_sha":"sha-head"}),
            json!({"item_id":"a","body_hash":"hash-a","head_sha":"sha-head"}),
        ];
        assert!(!exactly_one_accepted_per_item(&items, &accepted));
    }

    #[test]
    fn exactly_one_accepted_per_item_false_on_missing() {
        let items = vec![item("a"), item("b")];
        let accepted = vec![json!({"item_id":"a","body_hash":"hash-a","head_sha":"sha-head"})];
        assert!(!exactly_one_accepted_per_item(&items, &accepted));
    }

    #[test]
    fn upsert_state_entry_replaces_matching_entry() {
        let it = item("a");
        let b = binding();
        let mut entries = vec![json!({
            "stable_marker_key": "thread:a",
            "body_hash": "hash-a",
            "head_sha": "sha-head",
            "evaluation_status": "stale"
        })];
        let accepted = accepted_result(
            &FeedbackEvaluationResponse {
                item_id: "a".to_string(),
                stable_marker_key: "thread:a".to_string(),
                body_hash: "hash-a".to_string(),
                head_sha: "sha-head".to_string(),
                decision: "valid".to_string(),
                reason: String::new(),
                recommended_action: Some("x".to_string()),
                response_text: "ok".to_string(),
            },
            "t".to_string(),
            1,
            "llm",
            "not_reused",
        );
        upsert_state_entry(&mut entries, &b, &it, &accepted, "t2".to_string());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].get("evaluation_status").unwrap(), "accepted");
        let acc = entries[0].get("accepted_evaluation").unwrap();
        assert_eq!(acc.get("repository_owner").unwrap(), "acme");
        assert_eq!(acc.get("pr_number").unwrap(), 42);
    }

    #[test]
    fn accepted_result_shape_has_expected_fields() {
        let resp = FeedbackEvaluationResponse {
            item_id: "a".to_string(),
            stable_marker_key: "thread:a".to_string(),
            body_hash: "hash-a".to_string(),
            head_sha: "sha-head".to_string(),
            decision: "invalid".to_string(),
            reason: "because".to_string(),
            recommended_action: None,
            response_text: "resp".to_string(),
        };
        let value = accepted_result(&resp, "t".to_string(), 2, "llm", "reused");
        assert_eq!(value.get("recommended_action").unwrap(), "");
        assert_eq!(value.get("attempt_count").unwrap(), 2);
        assert_eq!(value.get("source").unwrap(), "llm");
        assert_eq!(value.get("reuse_state").unwrap(), "reused");
    }

    #[test]
    fn unevaluated_item_shape() {
        let it = item("a");
        let value = unevaluated_item(&it, "budget");
        assert_eq!(value.get("item_id").unwrap(), "a");
        assert_eq!(value.get("reason").unwrap(), "budget");
    }

    #[test]
    fn source_artifact_extracts_sequence_fields() {
        let value = json!({
            "artifact_sequence": 5,
            "write_sequence": 1,
            "producer_step_id": "collect"
        });
        let art = source_artifact(&value, "coderabbit-feedback");
        assert_eq!(art.get("artifact_family").unwrap(), "coderabbit-feedback");
        assert_eq!(art.get("artifact_sequence").unwrap(), 5);
        assert_eq!(art.get("producer_step_id").unwrap(), "collect");
    }

    #[test]
    fn require_string_and_u64() {
        let value = json!({"name":"x","count":7});
        assert_eq!(require_string(&value, "name").unwrap(), "x");
        assert!(require_string(&value, "missing").is_err());
        assert!(require_string(&json!({"name":""}), "name").is_err());
        assert_eq!(require_u64(&value, "count").unwrap(), 7);
        assert!(require_u64(&value, "name").is_err());
    }

    #[test]
    fn required_value_string_rejects_empty() {
        let value = json!({"a":"", "b":"ok"});
        assert!(required_value_string(&value, "a").is_err());
        assert_eq!(required_value_string(&value, "b").unwrap(), "ok");
    }

    #[test]
    fn has_unresolved_template_detects_braces() {
        assert!(has_unresolved_template("path/{var}"));
        assert!(has_unresolved_template("open{"));
        assert!(!has_unresolved_template("plain/path"));
    }

    #[test]
    fn u64_param_uses_default_when_absent() {
        let params = json!({"a": 9});
        assert_eq!(u64_param(&params, "a", 1), 9);
        assert_eq!(u64_param(&params, "missing", 3), 3);
    }

    #[test]
    fn sanitize_path_segment_replaces_unsafe_chars() {
        assert_eq!(sanitize_path_segment("a/b:c d"), "a_b_c_d");
        assert_eq!(sanitize_path_segment("keep-._09AZ"), "keep-._09AZ");
    }

    #[test]
    fn stable_json_hash_is_deterministic_and_prefixed() {
        let a = stable_json_hash(&json!({"x":1,"y":2}));
        let b = stable_json_hash(&json!({"x":1,"y":2}));
        assert_eq!(a, b);
        assert!(a.starts_with("fnv64:"));
        let c = stable_json_hash(&json!({"x":2}));
        assert_ne!(a, c);
    }

    #[test]
    fn binding_from_value_roundtrip() {
        let value = json!({
            "schema_version": PR_FOLLOWUP_SCHEMA_VERSION,
            "run_id": "r",
            "repository_owner": "o",
            "repository_name": "n",
            "pr_number": 3,
            "head_ref": "h",
            "head_sha": "hs",
            "base_ref": "b",
            "base_sha": "bs"
        });
        let b = binding_from_value(&value).unwrap();
        assert_eq!(b.run_id, "r");
        assert_eq!(b.pr_number, 3);
        assert_eq!(b.base_sha.as_deref(), Some("bs"));
    }

    #[test]
    fn binding_from_value_missing_field_errors() {
        let value = json!({"schema_version": PR_FOLLOWUP_SCHEMA_VERSION});
        assert!(binding_from_value(&value).is_err());
    }

    #[test]
    fn empty_artifact_defaults() {
        let art = empty_artifact(EvaluationState::Complete, 4, 3, vec![]);
        assert_eq!(art.items_seen, 4);
        assert_eq!(art.max_attempts_per_item, 3);
        assert!(art.accepted_results.is_empty());
        assert_eq!(art.reused_results_count, 0);
    }

    #[test]
    fn reject_batch_response_fields_rejects_non_object() {
        let err = reject_batch_response_fields(&json!("string")).unwrap_err();
        assert_eq!(err.reason, "response_not_object");
    }

    #[test]
    fn reject_captures_decision_and_head_sha() {
        let value = json!({"decision":"invalid","head_sha":"abc"});
        let r = reject("some_reason", &value);
        assert_eq!(r.reason, "some_reason");
        assert_eq!(r.parsed_decision.as_deref(), Some("invalid"));
        assert_eq!(r.observed_head_sha.as_deref(), Some("abc"));
    }
}
