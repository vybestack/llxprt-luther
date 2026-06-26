//! CodeRabbit feedback collection and remote marker discovery surfaces.
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
//! @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017,REQ-PRFU-024,REQ-PRFU-034
//! @pseudocode lines 1-29

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

use crate::engine::executor::{interpolate_string, StepContext, StepExecutor};
use crate::engine::executors::github_pr::GithubPrCommandRunner;
use crate::engine::executors::pr_followup_artifacts::{
    ArtifactWriter, ClockSleeper, PrFollowupArtifactStore,
};
use crate::engine::executors::pr_followup_types::{
    is_summary_marker_key, value_has_summary_marker_key, PrFollowupBinding,
    PR_FOLLOWUP_SCHEMA_VERSION,
};
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;

const DEFAULT_MAX_OBSERVATIONS: u64 = 6;
const DEFAULT_REQUIRED_STABLE_OBSERVATIONS: u64 = 2;
const DEFAULT_OBSERVATION_INTERVAL_SECONDS: u64 = 300;
const MARKER_NAMESPACE: &str = "luther-pr-followup";
const MARKER_ARTIFACT_FAMILY: &str = "pr-feedback-marker-report";
const PENDING_MARKER_ACTIONS_FAMILY: &str = "pending-feedback-marker-actions";
/// Sentinel identity that, when present in the configured identity set, makes
/// the feedback collector accept review threads from any reviewer (not only
/// CodeRabbit). Selected via the `include_all_reviewers` step param.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-024
const ALL_REVIEWERS_SENTINEL: &str = "*";
/// Real GraphQL mutation used to resolve a PR review thread.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-016
const RESOLVE_REVIEW_THREAD_MUTATION: &str = "mutation resolveReviewThread($threadId:ID!){ resolveReviewThread(input:{threadId:$threadId}) { thread { id isResolved } } }";

/// Remote marker discovery record for feedback marker idempotency.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-009,REQ-PRFU-016
/// @pseudocode lines 4-5,13,20,26
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RemoteFeedbackMarker {
    pub stable_marker_key: String,
    pub source_head_sha: String,
    pub remediation_output_head_sha: Option<String>,
    pub body_hash: String,
    pub action_kind: String,
    pub run_id: String,
    pub status: String,
}

/// Hidden feedback marker parser for exact Luther CodeRabbit marker comments.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-009,REQ-PRFU-016
/// @pseudocode lines 4-5,13,20,26
#[derive(Debug, Default)]
pub struct FeedbackMarkerParser;

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-009,REQ-PRFU-016
/// @pseudocode lines 4-5,13,20,26
impl FeedbackMarkerParser {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    #[must_use]
    pub fn parse_marker(&self, body: &str) -> Option<RemoteFeedbackMarker> {
        parse_hidden_marker(body).ok()
    }

    pub fn parse_marker_diagnostic(&self, body: &str) -> Result<RemoteFeedbackMarker, String> {
        parse_hidden_marker(body).map_err(|err| err.diagnostic)
    }
}

/// CodeRabbit feedback collection executor for `github_coderabbit_feedback`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017,REQ-PRFU-024,REQ-PRFU-034
/// @pseudocode lines 1-29
#[derive(Debug, Default)]
pub struct GithubCodeRabbitFeedbackExecutor;

/// Injectable CodeRabbit feedback collector for tests and alternate GitHub runners.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017,REQ-PRFU-024,REQ-PRFU-034
/// @pseudocode lines 1-29
#[derive(Debug)]
pub struct GithubCodeRabbitFeedbackExecutorWithRunner<R, C> {
    runner: R,
    clock: C,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017
/// @pseudocode lines 1-6,20-25
impl<R, C> GithubCodeRabbitFeedbackExecutorWithRunner<R, C> {
    #[must_use]
    pub fn new(runner: R, clock: C) -> Self {
        Self { runner, clock }
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017,REQ-PRFU-024,REQ-PRFU-034
/// @pseudocode lines 1-29
impl<R, C> StepExecutor for GithubCodeRabbitFeedbackExecutorWithRunner<R, C>
where
    R: GithubPrCommandRunner,
    C: ClockSleeper,
{
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        collect_coderabbit_feedback(context, params, &self.runner, &self.clock)
    }
}

/// Feedback marker executor for deterministic PR feedback comments and resolutions.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017,REQ-PRFU-020,REQ-PRFU-026
/// @pseudocode lines 41-49
#[derive(Debug, Default)]
pub struct GithubFeedbackMarkerExecutor;

/// Injectable feedback marker executor for tests and alternate GitHub runners.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017,REQ-PRFU-026
/// @pseudocode lines 41-49
#[derive(Debug)]
pub struct GithubFeedbackMarkerExecutorWithRunner<R, C> {
    runner: R,
    clock: C,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017,REQ-PRFU-026
/// @pseudocode lines 41-49
impl<R, C> GithubFeedbackMarkerExecutorWithRunner<R, C> {
    #[must_use]
    pub fn new(runner: R, clock: C) -> Self {
        Self { runner, clock }
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017,REQ-PRFU-020,REQ-PRFU-026
/// @pseudocode lines 41-49
/// @pseudocode lines 41-49
impl<R, C> StepExecutor for GithubFeedbackMarkerExecutorWithRunner<R, C>
where
    R: GithubPrCommandRunner,
    C: ClockSleeper,
{
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        mark_coderabbit_feedback(context, params, &self.runner, &self.clock)
    }
}
/// Production clock/sleeper for live CodeRabbit feedback polling.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-017
/// @pseudocode lines 3,24-25
#[derive(Debug, Default)]
pub struct SystemFeedbackClock;

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-017
/// @pseudocode lines 3,24-25
impl ClockSleeper for SystemFeedbackClock {
    fn now_rfc3339(&self) -> String {
        chrono::Utc::now().to_rfc3339()
    }

    fn sleep(&self, duration: Duration) {
        thread::sleep(duration);
    }
}

/// Normalized feedback item bound to one CodeRabbit observation.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009
/// @pseudocode lines 7-14,26-29
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
struct FeedbackItem {
    item_id: String,
    stable_marker_key: String,
    thread_id: Option<String>,
    comment_id: Option<String>,
    comment_database_id: Option<i64>,
    review_id: Option<String>,
    author_login: String,
    path: Option<String>,
    line: Option<u64>,
    side: Option<String>,
    body: String,
    body_hash: String,
    url: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    resolved: bool,

    outdated: bool,
    resolution_state_available: bool,
    source: String,
    raw_node_id: Option<String>,
    commit_sha: Option<String>,
    stale: bool,
}

/// Single collector observation after querying documented GitHub feedback surfaces.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017
/// @pseudocode lines 4-19
#[derive(Clone, Debug, Default)]
struct FeedbackObservation {
    items: Vec<FeedbackItem>,
    stale_items: Vec<Value>,
    noise: Vec<Value>,
    remote_markers: Vec<RemoteFeedbackMarker>,
    malformed_remote_markers: Vec<Value>,
    remote_marker_audit: Vec<Value>,
    ready_signal: bool,
    in_progress_signal: bool,
    readiness_signals: Vec<Value>,
    stale_signals: Vec<Value>,
    matched_identities: BTreeSet<String>,
    fatal: Option<Value>,
}

/// One pending feedback marker action consumed by `github_feedback_marker`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-026
/// @pseudocode lines 41-49
#[derive(Clone, Debug)]
struct PendingMarkerAction {
    value: Value,
    action_kind: String,
    item_id: String,
    stable_marker_key: String,
    source_head_sha: String,
    remediation_output_head: String,
    body_hash: String,
    reason: String,
    response_text: Option<String>,
    thread_id: Option<String>,
    comment_database_id: Option<i64>,
    resolution_required: bool,
    status: String,
}

/// Result classification for one marker action attempt.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-026
/// @pseudocode lines 43-49
#[derive(Clone, Debug)]
struct MarkerActionOutcome {
    action: PendingMarkerAction,
    status: String,
    comment_key: String,
    resolution_key: String,
    posted_comment: Option<Value>,
    resolved_thread: Option<Value>,
    skipped: Vec<Value>,
    partial: Option<Value>,
    retryable: Option<Value>,
    failed: Option<Value>,
    audit: Value,
    updated_action: Value,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017,REQ-PRFU-024,REQ-PRFU-034
/// @pseudocode lines 1-29
// Pre-existing GitHub feedback collection flow; split in a dedicated refactor stage.
#[allow(clippy::too_many_lines)]
fn collect_coderabbit_feedback(
    context: &mut StepContext,
    params: &Value,
    runner: &dyn GithubPrCommandRunner,
    clock: &dyn ClockSleeper,
) -> Result<StepOutcome, EngineError> {
    let artifact_root = artifact_root(context, params)?;
    let store = PrFollowupArtifactStore::new(artifact_root);

    let binding = read_or_build_binding(context, params, &store)?;
    let step_id = current_step_id(context, "collect_coderabbit_feedback");
    let step_order = u64_param(params, "step_order_index", 5);
    let max_observations = u64_param(params, "max_observations", DEFAULT_MAX_OBSERVATIONS);
    let required_stable = u64_param(
        params,
        "required_stable_observations",
        DEFAULT_REQUIRED_STABLE_OBSERVATIONS,
    );
    let interval_seconds = u64_param(
        params,
        "coderabbit_readiness_observation_interval_seconds",
        DEFAULT_OBSERVATION_INTERVAL_SECONDS,
    );
    let identities = configured_identities(params);
    let mut observations = Vec::new();
    let mut previous_ready_hash: Option<String> = None;
    let mut stable_count = 0;
    let mut final_observation = FeedbackObservation::default();

    for attempt in 1..=max_observations {
        let observed_at = clock.now_rfc3339();
        let observation = observe_coderabbit_feedback(runner, &binding, &identities)?;
        if let Some(fatal) = observation.fatal.clone() {
            let payload = feedback_payload(
                &binding,
                "fatal",
                stable_count,
                required_stable,
                max_observations,
                interval_seconds,
                &observations,
                &observation,
                &identities,
                attempt,
                observed_at,
                "fatal_api_or_schema",
            );
            write_feedback_artifacts(
                &store,
                &binding,
                &step_id,
                step_order,
                &payload,
                clock,
                Some(("fatal", "api_auth_schema_or_ambiguity", fatal)),
            )?;
            return Ok(StepOutcome::Fatal);
        }

        let item_set_hash = item_set_hash(&observation.items);
        let readiness_hash = readiness_stability_hash(&observation);
        let materially_ready = observation.ready_signal && !observation.in_progress_signal;
        if materially_ready && previous_ready_hash.as_deref() == Some(readiness_hash.as_str()) {
            stable_count += 1;
        } else if materially_ready {
            stable_count = 1;
        } else {
            stable_count = 0;
        }
        previous_ready_hash = materially_ready.then_some(readiness_hash.clone());
        let outcome_reason = if stable_count >= required_stable {
            if observation.items.is_empty() {
                "stable_ready_empty"
            } else {
                "stable_ready_feedback"
            }
        } else if observation.in_progress_signal {
            "in_progress_overrides_ready"
        } else if observation.ready_signal {
            "waiting_for_stable_observation"
        } else {
            "no_current_head_ready_signal"
        };
        let observation_json = observation_json(
            &observation,
            &item_set_hash,
            attempt,
            max_observations,
            &observed_at,
            outcome_reason,
        );
        observations.push(observation_json);
        final_observation = observation.clone();

        let readiness_state = if stable_count >= required_stable {
            "ready"
        } else {
            "not_ready"
        };
        let payload = feedback_payload(
            &binding,
            readiness_state,
            stable_count,
            required_stable,
            max_observations,
            interval_seconds,
            &observations,
            &final_observation,
            &identities,
            attempt,
            observed_at,
            outcome_reason,
        );
        write_feedback_artifacts(
            &store, &binding, &step_id, step_order, &payload, clock, None,
        )?;
        if stable_count >= required_stable {
            return Ok(StepOutcome::Success);
        }
        if attempt < max_observations {
            clock.sleep(Duration::from_secs(interval_seconds));
        }
    }

    let payload = feedback_payload(
        &binding,
        "timeout",
        stable_count,
        required_stable,
        max_observations,
        interval_seconds,
        &observations,
        &final_observation,
        &identities,
        max_observations,
        clock.now_rfc3339(),
        "readiness_budget_exhausted",
    );
    write_feedback_artifacts(
        &store,
        &binding,
        &step_id,
        step_order,
        &payload,
        clock,
        Some((
            "timeout",
            "readiness_budget_exhausted",
            json!({ "max_observations": max_observations }),
        )),
    )?;
    Ok(StepOutcome::Fatal)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017
/// @pseudocode lines 4-17
fn observe_coderabbit_feedback(
    runner: &dyn GithubPrCommandRunner,
    binding: &PrFollowupBinding,
    identities: &BTreeSet<String>,
) -> Result<FeedbackObservation, EngineError> {
    let mut observation = FeedbackObservation::default();
    let graph = query_review_threads(runner, binding)?;
    if is_permission_or_schema_error(&graph) {
        observation.fatal = Some(json!({ "surface": "graphql_review_threads", "response": graph }));
        return Ok(observation);
    }
    normalize_graphql_threads(&graph, binding, identities, &mut observation);
    for rest_comment in query_rest_review_comments(runner, binding)? {
        normalize_rest_review_comment(&rest_comment, binding, identities, &mut observation);
    }
    for issue_comment in query_issue_comments(runner, binding)? {
        normalize_issue_comment(&issue_comment, binding, identities, &mut observation);
    }
    resolve_remote_markers(binding, &mut observation);
    for signal in query_readiness_signals(runner, binding)? {
        normalize_readiness_signal(&signal, binding, identities, &mut observation);
    }
    if observation
        .items
        .iter()
        .any(is_coderabbit_summary_feedback_item)
    {
        observation.readiness_signals.push(json!({
            "source": "issue_comment_summary",
            "head_sha": binding.head_sha,
            "bot_login": "coderabbitai[bot]",
            "status": "completed",
            "conclusion": "success",
            "summary_body": "CodeRabbit summary comment observed on current PR head"
        }));
        observation.ready_signal = true;
    }

    observation.items.sort_by(|left, right| {
        left.stable_marker_key
            .cmp(&right.stable_marker_key)
            .then(left.body_hash.cmp(&right.body_hash))
    });
    observation.items.dedup_by(|left, right| {
        left.stable_marker_key == right.stable_marker_key
            && left.body_hash == right.body_hash
            && left.commit_sha == right.commit_sha
    });
    Ok(observation)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-017
/// @pseudocode lines 4,7,11-13
fn normalize_graphql_threads(
    value: &Value,
    binding: &PrFollowupBinding,
    identities: &BTreeSet<String>,
    observation: &mut FeedbackObservation,
) {
    let nodes = value
        .pointer("/data/repository/pullRequest/reviewThreads/nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for thread in nodes {
        let resolved = thread
            .get("isResolved")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let outdated = thread
            .get("isOutdated")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        for comment in thread
            .pointer("/comments/nodes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let author = comment
                .pointer("/author/login")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !is_coderabbit(author, identities) {
                observation
                    .noise
                    .push(json!({ "source": "graphql_review_thread", "author_login": author }));
                continue;
            }
            observation.matched_identities.insert(author.to_string());
            if resolved {
                continue;
            }
            let body = string_field(&comment, "body");
            let commit_sha = comment
                .pointer("/commit/oid")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let stale = commit_sha.as_deref() != Some(binding.head_sha.as_str()) || outdated;
            let item = FeedbackItem {
                item_id: format!(
                    "graphql:{}:{}",
                    string_field(&thread, "id"),
                    string_field(&comment, "id")
                ),
                stable_marker_key: format!("thread:{}", string_field(&thread, "id")),
                thread_id: opt_string(&thread, "id"),
                comment_id: opt_string(&comment, "id"),
                comment_database_id: comment.get("databaseId").and_then(Value::as_i64),
                review_id: None,
                author_login: author.to_string(),
                path: opt_string(&comment, "path").or_else(|| opt_string(&thread, "path")),
                line: comment
                    .get("line")
                    .and_then(Value::as_u64)
                    .or_else(|| thread.get("line").and_then(Value::as_u64)),
                side: None,
                body_hash: stable_hash(&body),
                body,
                url: opt_string(&comment, "url"),
                created_at: opt_string(&comment, "createdAt"),
                updated_at: opt_string(&comment, "updatedAt"),
                resolved,
                outdated,
                resolution_state_available: true,
                source: "graphql_review_thread".to_string(),
                raw_node_id: opt_string(&comment, "id"),
                commit_sha,
                stale,
            };
            push_current_or_stale(item, observation);
        }
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017
/// @pseudocode lines 5,8,12-14
fn normalize_rest_review_comment(
    comment: &Value,
    binding: &PrFollowupBinding,
    identities: &BTreeSet<String>,
    observation: &mut FeedbackObservation,
) {
    let author = comment
        .pointer("/user/login")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !is_coderabbit(author, identities) {
        observation
            .noise
            .push(json!({ "source": "rest_review_comment", "author_login": author }));
        return;
    }
    observation.matched_identities.insert(author.to_string());
    let body = string_field(comment, "body");
    record_remote_marker_parse(
        &body,
        "rest_review_comment",
        comment.get("id").cloned().unwrap_or(Value::Null),
        observation,
    );
    let commit_sha = opt_string(comment, "commit_id");
    let stale = commit_sha.as_deref() != Some(binding.head_sha.as_str());
    let item = FeedbackItem {
        item_id: format!("rest-review:{}", string_field(comment, "node_id")),
        stable_marker_key: format!("review-comment:{}", string_field(comment, "node_id")),
        thread_id: None,
        comment_id: opt_string(comment, "node_id").or_else(|| {
            comment
                .get("id")
                .and_then(Value::as_u64)
                .map(|id| id.to_string())
        }),
        comment_database_id: comment.get("id").and_then(Value::as_i64),
        review_id: comment
            .get("pull_request_review_id")
            .and_then(Value::as_u64)
            .map(|id| id.to_string()),
        author_login: author.to_string(),
        path: opt_string(comment, "path"),
        line: comment
            .get("line")
            .and_then(Value::as_u64)
            .or_else(|| comment.get("original_line").and_then(Value::as_u64)),
        side: opt_string(comment, "side"),
        body_hash: stable_hash(&body),
        body,
        url: opt_string(comment, "html_url"),
        created_at: opt_string(comment, "created_at"),
        updated_at: opt_string(comment, "updated_at"),
        resolved: false,
        outdated: false,
        resolution_state_available: false,
        source: "rest_review_comment".to_string(),
        raw_node_id: opt_string(comment, "node_id"),
        commit_sha,
        stale,
    };
    push_current_or_stale(item, observation);
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017
/// @pseudocode lines 5,9-13
fn normalize_issue_comment(
    comment: &Value,
    binding: &PrFollowupBinding,
    identities: &BTreeSet<String>,
    observation: &mut FeedbackObservation,
) {
    let body = string_field(comment, "body");
    record_remote_marker_parse(
        &body,
        "issue_comment",
        comment.get("id").cloned().unwrap_or(Value::Null),
        observation,
    );
    let author = comment
        .pointer("/user/login")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !is_coderabbit(author, identities) {
        observation
            .noise
            .push(json!({ "source": "issue_comment", "author_login": author }));
        return;
    }
    observation.matched_identities.insert(author.to_string());
    let stale = comment
        .get("head_sha")
        .and_then(Value::as_str)
        .is_some_and(|head| head != binding.head_sha);
    let item = FeedbackItem {
        item_id: format!("issue-comment:{}", string_field(comment, "node_id")),
        stable_marker_key: format!(
            "summary:{}:{}",
            string_field(comment, "node_id"),
            stable_hash(&body)
        ),
        thread_id: None,
        comment_id: opt_string(comment, "node_id").or_else(|| {
            comment
                .get("id")
                .and_then(Value::as_u64)
                .map(|id| id.to_string())
        }),
        comment_database_id: None,
        review_id: None,
        author_login: author.to_string(),
        path: None,
        line: None,
        side: None,
        body_hash: stable_hash(&body),
        body,
        url: opt_string(comment, "html_url"),
        created_at: opt_string(comment, "created_at"),
        updated_at: opt_string(comment, "updated_at"),
        resolved: false,
        outdated: false,
        resolution_state_available: false,
        source: "issue_comment".to_string(),
        raw_node_id: opt_string(comment, "node_id"),
        commit_sha: Some(binding.head_sha.clone()),
        stale,
    };
    push_current_or_stale(item, observation);
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 6,15-19
fn normalize_readiness_signal(
    signal: &Value,
    binding: &PrFollowupBinding,
    identities: &BTreeSet<String>,
    observation: &mut FeedbackObservation,
) {
    let bot = signal
        .get("bot_login")
        .or_else(|| signal.get("app_slug"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !is_coderabbit(bot, identities) {
        return;
    }
    observation.matched_identities.insert(bot.to_string());
    let signal_head = signal
        .get("head_sha")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if signal_head != binding.head_sha {
        observation.stale_signals.push(signal.clone());
        return;
    }
    observation.readiness_signals.push(signal.clone());
    let status = signal
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let conclusion = signal
        .get("conclusion")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let review_state = signal
        .get("review_state")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let summary = signal
        .get("summary_body")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if status.contains("progress") || status == "queued" || review_state == "pending" {
        observation.in_progress_signal = true;
    }
    if (status == "completed"
        && matches!(conclusion.as_str(), "success" | "neutral" | "skipped" | ""))
        || matches!(
            review_state.as_str(),
            "commented" | "approved" | "changes_requested"
        )
        || summary.contains("finished")
        || summary.contains("review completed")
    {
        observation.ready_signal = true;
    }
}

fn is_coderabbit_summary_feedback_item(item: &FeedbackItem) -> bool {
    if item.source != "issue_comment" || item.stale {
        return false;
    }
    coderabbit_body_is_non_actionable_notice(&item.body)
}

/// Recognizes CodeRabbit auto-generated issue comments that signal a completed
/// (or unavailable) review and therefore carry no actionable feedback: the
/// summary/walkthrough comment, the "finished reviewing" notice, and the
/// rate-limit / out-of-credits "review limit reached" warning. Any of these
/// confirms CodeRabbit has reported on the current head, so readiness can be
/// satisfied without an actual review thread.
fn coderabbit_body_is_non_actionable_notice(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    body.contains("summary by coderabbit")
        || body.contains("summarize by coderabbit")
        || (body.contains("walkthrough") && body.contains("coderabbit"))
        || body.contains("coderabbit finished reviewing this pull request")
        || body.contains("rate limited by coderabbit")
        || body.contains("review limit reached")
        || (body.contains("coderabbit") && body.contains("run out of usage credits"))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009
/// @pseudocode lines 7-14
fn push_current_or_stale(item: FeedbackItem, observation: &mut FeedbackObservation) {
    if item.stale {
        observation.stale_items.push(item_json(&item));
    } else {
        observation.items.push(item);
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017
/// @pseudocode lines 4-6
fn query_review_threads(
    runner: &dyn GithubPrCommandRunner,
    binding: &PrFollowupBinding,
) -> Result<Value, EngineError> {
    let graphql_review_threads_query = r#"
query($owner: String!, $name: String!, $number: Int!, $page: String) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      reviewThreads(first: 100, after: $page) {
        nodes {
          id
          isResolved
          isOutdated
          path
          line
          startLine
          comments(first: 100) {
            nodes {
              id
              databaseId
              body
              author { login }
              url
              path
              line
              originalLine
              createdAt
              updatedAt
              commit { oid }
            }
          }
        }
        pageInfo {
          hasNextPage
          endCursor
        }
      }
    }
  }
}
"#;

    let mut all_nodes = Vec::new();
    let mut cursor: Option<String> = None;
    let mut page_count = 0;
    loop {
        page_count += 1;
        let mut argv = vec![
            "gh".to_string(),
            "api".to_string(),
            "graphql".to_string(),
            "-f".to_string(),
            format!("query={graphql_review_threads_query}"),
            "-f".to_string(),
            format!("owner={}", binding.repository_owner),
            "-f".to_string(),
            format!("name={}", binding.repository_name),
            "-F".to_string(),
            format!("number={}", binding.pr_number),
        ];
        if let Some(cursor) = cursor.as_deref() {
            argv.push("-f".to_string());
            argv.push(format!("page={cursor}"));
        }

        let output = runner.run_github_command(&argv)?;
        let value: Value = serde_json::from_str(&output).map_err(|err| {
            github_feedback_error(format!("parse review threads response: {err}"))
        })?;
        if is_permission_or_schema_error(&value) {
            return Ok(value);
        }
        let thread_value = value
            .pointer("/data/repository/pullRequest/reviewThreads")
            .cloned()
            .unwrap_or_else(|| json!({}));
        all_nodes.extend(
            thread_value
                .get("nodes")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
        let has_next = thread_value
            .pointer("/pageInfo/hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !has_next || page_count >= 20 {
            break;
        }
        cursor = thread_value
            .pointer("/pageInfo/endCursor")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        if cursor.is_none() {
            break;
        }
    }

    Ok(
        json!({ "data": { "repository": { "pullRequest": { "reviewThreads": { "nodes": all_nodes } } } } }),
    )
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017
/// @pseudocode lines 5,8-9
fn query_rest_review_comments(
    runner: &dyn GithubPrCommandRunner,
    binding: &PrFollowupBinding,
) -> Result<Vec<Value>, EngineError> {
    query_paginated_array(
        runner,
        &format!(
            "/repos/{}/{}/pulls/{}/comments?per_page=100&page=",
            binding.repository_owner, binding.repository_name, binding.pr_number
        ),
    )
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017
/// @pseudocode lines 5,9
fn query_issue_comments(
    runner: &dyn GithubPrCommandRunner,
    binding: &PrFollowupBinding,
) -> Result<Vec<Value>, EngineError> {
    query_paginated_array(
        runner,
        &format!(
            "/repos/{}/{}/issues/{}/comments?per_page=100&page=",
            binding.repository_owner, binding.repository_name, binding.pr_number
        ),
    )
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-017
/// @pseudocode lines 6,15-17
fn query_readiness_signals(
    runner: &dyn GithubPrCommandRunner,
    binding: &PrFollowupBinding,
) -> Result<Vec<Value>, EngineError> {
    let mut signals = Vec::new();
    let output = runner.run_github_command(&[
        "gh".to_string(),
        "api".to_string(),
        format!(
            "/repos/{}/{}/commits/{}/check-runs?per_page=100&page=1",
            binding.repository_owner, binding.repository_name, binding.head_sha
        ),
    ])?;
    let value: Value = serde_json::from_str(&output).map_err(|err| {
        github_feedback_error(format!("parse readiness check-runs response: {err}"))
    })?;
    if let Some(checks) = value.get("check_runs").and_then(Value::as_array) {
        for check in checks {
            signals.push(json!({
                "source": "check_run",
                "head_sha": readiness_signal_head_sha(check, binding),
                "bot_login": check.pointer("/app/slug").and_then(Value::as_str).unwrap_or_default(),
                "status": check.get("status").and_then(Value::as_str).unwrap_or_default(),
                "conclusion": check.get("conclusion").cloned().unwrap_or(Value::Null),
                "summary_body": check.get("output").and_then(|output| output.get("summary")).and_then(Value::as_str).unwrap_or_default()
            }));
        }
    }
    Ok(signals)
}

fn readiness_signal_head_sha(check: &Value, binding: &PrFollowupBinding) -> String {
    check
        .get("head_sha")
        .and_then(Value::as_str)
        .or_else(|| {
            let name = check
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let check_run_url = check
                .get("check_run_url")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let html_url = check
                .get("html_url")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if name.eq_ignore_ascii_case("coderabbit")
                || name.to_ascii_lowercase().contains("coderabbit")
                || check_run_url.contains("/commits/")
                || html_url.contains("/checks")
            {
                Some(binding.head_sha.as_str())
            } else {
                None
            }
        })
        .unwrap_or_default()
        .to_string()
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017
/// @pseudocode lines 5
fn query_paginated_array(
    runner: &dyn GithubPrCommandRunner,
    endpoint_prefix: &str,
) -> Result<Vec<Value>, EngineError> {
    let mut page = 1;
    let mut values = Vec::new();
    loop {
        let output = runner.run_github_command(&[
            "gh".to_string(),
            "api".to_string(),
            format!("{endpoint_prefix}{page}"),
        ])?;
        let page_values: Value = serde_json::from_str(&output)
            .map_err(|err| github_feedback_error(format!("parse paginated response: {err}")))?;
        let Some(array) = page_values.as_array() else {
            break;
        };
        if array.is_empty() {
            break;
        }
        values.extend(array.iter().cloned());
        if array.len() < 100 || page >= 20 {
            break;
        }
        page += 1;
    }
    Ok(values)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-009,REQ-PRFU-016
/// @pseudocode lines 4-5,13,20,26
fn record_remote_marker_parse(
    body: &str,
    source: &str,
    comment_id: Value,
    observation: &mut FeedbackObservation,
) {
    if !body.contains(MARKER_NAMESPACE) {
        return;
    }
    match parse_hidden_marker(body) {
        Ok(marker) => observation.remote_markers.push(marker),
        Err(err) => observation.malformed_remote_markers.push(json!({
            "source": source,
            "comment_id": comment_id,
            "diagnostic": err.diagnostic,
        })),
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-009,REQ-PRFU-016
/// @pseudocode lines 13,20,26-28
fn resolve_remote_markers(binding: &PrFollowupBinding, observation: &mut FeedbackObservation) {
    let mut by_identity: BTreeMap<String, Vec<RemoteFeedbackMarker>> = BTreeMap::new();
    for marker in observation.remote_markers.iter().cloned() {
        let identity = remote_marker_identity(&marker);
        by_identity.entry(identity).or_default().push(marker);
    }
    let mut accepted = Vec::new();
    for (identity, markers) in by_identity {
        let first = markers.first().expect("non-empty marker group").clone();
        let all_identical = markers.iter().all(|marker| marker == &first);
        let same_action_duplicates = markers.iter().all(|marker| {
            marker.stable_marker_key == first.stable_marker_key
                && marker.source_head_sha == first.source_head_sha
                && marker.run_id == first.run_id
                && marker.action_kind == first.action_kind
        });
        if !all_identical && same_action_duplicates {
            observation.fatal = Some(json!({
                "class": "conflicting_remote_marker_duplicates",
                "identity": identity,
                "markers": markers.iter().map(remote_marker_json).collect::<Vec<_>>(),
            }));
            return;
        }
        if !all_identical {
            observation.fatal = Some(json!({
                "class": "conflicting_remote_marker_duplicates",
                "identity": identity,
                "markers": markers.iter().map(remote_marker_json).collect::<Vec<_>>(),
            }));
            return;
        }
        if markers.len() > 1 {
            observation.remote_marker_audit.push(json!({
                "event": "duplicate_identical_remote_marker_already_complete",
                "identity": identity,
                "count": markers.len(),
                "marker": remote_marker_json(&first),
            }));
        }
        if first.status == "completed" && first.source_head_sha == binding.head_sha {
            observation.remote_marker_audit.push(json!({
                "event": "matching_remote_completed_marker_current_head",
                "identity": identity,
                "marker": remote_marker_json(&first),
            }));
        }
        accepted.push(first);
    }
    observation.remote_markers = accepted;
    let completed_keys = observation
        .remote_markers
        .iter()
        .filter(|marker| marker.status == "completed" && marker.source_head_sha == binding.head_sha)
        .map(|marker| (marker.stable_marker_key.clone(), marker.body_hash.clone()))
        .collect::<BTreeSet<_>>();
    if completed_keys.is_empty() {
        return;
    }
    observation.items.retain(|item| {
        let complete =
            completed_keys.contains(&(item.stable_marker_key.clone(), item.body_hash.clone()));
        if complete {
            observation.remote_marker_audit.push(json!({
                "event": "local_feedback_ignored_remote_completed",
                "stable_marker_key": item.stable_marker_key,
                "body_hash": item.body_hash,
                "source_head_sha": binding.head_sha,
            }));
        }
        !complete
    });
    observation.stale_items.retain(|item| {
        let key = item
            .get("stable_marker_key")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let hash = item
            .get("body_hash")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let complete = completed_keys.contains(&(key.clone(), hash.clone()));
        if complete {
            observation.remote_marker_audit.push(json!({
                "event": "stale_local_feedback_ignored_remote_completed",
                "stable_marker_key": key,
                "body_hash": hash,
                "source_head_sha": binding.head_sha,
            }));
        }
        !complete
    });
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-009,REQ-PRFU-016
/// @pseudocode lines 13,20,26
fn remote_marker_identity(marker: &RemoteFeedbackMarker) -> String {
    format!(
        "{}|{}|{}|{}",
        marker.stable_marker_key, marker.source_head_sha, marker.run_id, marker.action_kind
    )
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017,REQ-PRFU-026
/// @pseudocode lines 41-49
// Pre-existing marker orchestration flow; split in a dedicated refactor stage.
#[allow(clippy::too_many_lines)]
fn mark_coderabbit_feedback(
    context: &mut StepContext,
    params: &Value,
    runner: &dyn GithubPrCommandRunner,
    clock: &dyn ClockSleeper,
) -> Result<StepOutcome, EngineError> {
    let artifact_root = artifact_root(context, params)?;
    let store = PrFollowupArtifactStore::new(artifact_root);

    let binding = read_or_build_binding(context, params, &store)?;
    let step_id = current_step_id(context, "mark_coderabbit_feedback");
    let step_order = u64_param(params, "step_order_index", 15);
    let mut pending_artifact = read_pending_marker_artifact(&store, &binding)?;
    refresh_pending_marker_actions_from_current_artifacts(
        &store,
        &binding,
        &mut pending_artifact,
        params,
        clock,
    );
    let thread_identifiers = collect_thread_identifiers_by_action_key(&store, &binding);
    let pending_actions = pending_artifact
        .get("pending_actions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|value| backfill_thread_identifiers(value, &thread_identifiers))
        .filter_map(|value| pending_marker_action_from_value(value).ok())
        .collect::<Vec<_>>();
    if let Some(violations) = validate_marker_actions_before_mutation(&pending_actions) {
        let report = json!({
            "schema_version": PR_FOLLOWUP_SCHEMA_VERSION,
            "marker_state": "fatal",
            "validation_state": "invalid",
            "validation_violations": violations,
            "github_side_effects_performed": false,
            "generated_at": clock.now_rfc3339()
        });
        store.write_json_artifact(
            &binding,
            MARKER_ARTIFACT_FAMILY,
            &step_id,
            step_order,
            &report,
            Some((
                "fatal",
                "marker_actions_failed_pre_mutation_validation",
                json!({ "validation_violations": report["validation_violations"].clone() }),
            )),
            clock,
        )?;
        return Ok(StepOutcome::Fatal);
    }
    let local_completed = read_local_marker_completions(&store, &binding);
    let remote_comments = discover_marker_remote_comments(runner, &binding).unwrap_or_default();
    let mut remote_completed = BTreeSet::new();
    let mut malformed_remote_markers = Vec::new();
    for comment in remote_comments {
        let body = comment
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match parse_marker_from_comment_body(body) {
            Ok(marker) => {
                if marker.status == "completed" {
                    remote_completed
                        .insert(marker_action_key_from_marker(&binding, &marker, "comment"));
                    remote_completed.insert(marker_action_key_from_marker(
                        &binding,
                        &marker,
                        "resolution",
                    ));
                }
            }
            Err(err) if body.contains(MARKER_NAMESPACE) => malformed_remote_markers.push(json!({
                "comment_id": comment.get("id").cloned().unwrap_or(Value::Null),
                "diagnostic": err.diagnostic
            })),
            Err(_) => {}
        }
    }

    let mut outcomes = Vec::new();
    for action in pending_actions {
        outcomes.push(process_marker_action(
            &binding,
            &store,
            &step_id,
            step_order,
            runner,
            clock,
            action,
            &local_completed,
            &remote_completed,
            params,
        )?);
    }
    write_updated_pending_actions(
        &store,
        &binding,
        &step_id,
        step_order,
        &mut pending_artifact,
        &outcomes,
        clock,
    )?;
    let mut report = marker_report_payload(
        &binding,
        &outcomes,
        malformed_remote_markers,
        clock.now_rfc3339(),
    );
    let has_failure = report
        .get("failed_actions")
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty());
    let has_partial = report
        .get("partial_actions")
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty());
    let state = if has_failure || has_partial {
        "partial"
    } else {
        "complete"
    };
    report["marker_state"] = json!(state);
    let failure = (state != "complete").then(|| {
        (
            state,
            "marker_actions_incomplete",
            json!({
                "partial_actions": report["partial_actions"].clone(),
                "failed_actions": report["failed_actions"].clone()
            }),
        )
    });
    store.write_json_artifact(
        &binding,
        MARKER_ARTIFACT_FAMILY,
        &step_id,
        step_order,
        &report,
        failure,
        clock,
    )?;
    Ok(if state == "complete" {
        StepOutcome::Success
    } else {
        StepOutcome::Fatal
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017,REQ-PRFU-026
/// @pseudocode lines 41-49
fn read_pending_marker_artifact(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
) -> Result<Value, EngineError> {
    match store.read_current_json(binding, PENDING_MARKER_ACTIONS_FAMILY) {
        Ok(value) => Ok(value),
        Err(_) => Ok(json!({
            "pending_actions": [],
            "carry_forward_from_artifact_sequence": null,
            "marker_policy": {},
            "updated_at": null
        })),
    }
}

fn refresh_pending_marker_actions_from_current_artifacts(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    pending_artifact: &mut Value,
    params: &Value,
    clock: &dyn ClockSleeper,
) {
    let Ok(feedback) = store.read_current_json(binding, "coderabbit-feedback") else {
        return;
    };
    let Ok(evaluations) = store.read_current_json(binding, "feedback-evaluations") else {
        return;
    };
    let feedback_items = feedback_items_by_identity(&feedback);
    let mut actions = pending_artifact
        .get("pending_actions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // Carry-forward pruning: drop any stale summary-keyed action loaded from a
    // pre-fix pending-feedback-marker-actions.json so reruns never re-persist or
    // post an informational summary marker.
    actions.retain(|action| !value_has_summary_marker_key(action));
    let mut seen = actions
        .iter()
        .map(pending_action_collision_key)
        .collect::<BTreeSet<_>>();
    for evaluation in evaluations
        .get("accepted_results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(action) = current_evaluation_marker_action(
            binding,
            evaluation,
            feedback_items.get(&evaluation_identity_key(evaluation)),
            params,
            clock,
        ) else {
            continue;
        };
        let key = pending_action_collision_key(&action);
        if seen.insert(key) {
            actions.push(action);
        }
    }
    pending_artifact["pending_actions"] = json!(actions);
    pending_artifact["refreshed_from_current_artifacts_at"] = json!(clock.now_rfc3339());
}

fn feedback_items_by_identity(feedback: &Value) -> BTreeMap<String, Value> {
    feedback
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|item| (evaluation_identity_key(item), item.clone()))
        .collect()
}

/// Index collected review-thread identifiers (thread id + numeric comment id)
/// by the most specific stable item identity available. Item-level keys avoid
/// collisions when several comments share the same GraphQL review thread marker.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016
fn collect_thread_identifiers_by_action_key(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
) -> BTreeMap<String, (Option<String>, Option<i64>)> {
    let mut identifiers = BTreeMap::new();
    let Ok(feedback) = store.read_current_json(binding, "coderabbit-feedback") else {
        return identifiers;
    };
    let items = feedback
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut stable_marker_key_counts: BTreeMap<String, usize> = BTreeMap::new();
    for item in &items {
        if let Some(stable_marker_key) = item.get("stable_marker_key").and_then(Value::as_str) {
            *stable_marker_key_counts
                .entry(stable_marker_key.to_string())
                .or_default() += 1;
        }
    }
    for item in &items {
        let thread_id = item
            .get("thread_id")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let comment_database_id = item.get("comment_database_id").and_then(Value::as_i64);
        if thread_id.is_none() && comment_database_id.is_none() {
            continue;
        }
        let key = thread_identifier_action_key(item, &stable_marker_key_counts);
        if !key.is_empty() {
            identifiers.insert(key, (thread_id, comment_database_id));
        }
    }
    identifiers
}

/// Fill in missing `thread_id`/`comment_database_id` on a pending marker action
/// from the collected review-thread index, without overwriting present values.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016
fn backfill_thread_identifiers(
    mut value: Value,
    identifiers: &BTreeMap<String, (Option<String>, Option<i64>)>,
) -> Value {
    let key = pending_action_thread_identifier_key(&value);
    if key.is_empty() {
        return value;
    }
    let Some((thread_id, comment_database_id)) = identifiers.get(&key) else {
        return value;
    };
    if let Some(object) = value.as_object_mut() {
        if object.get("thread_id").and_then(Value::as_str).is_none() {
            if let Some(thread_id) = thread_id {
                object.insert("thread_id".to_string(), json!(thread_id));
            }
        }
        if object
            .get("comment_database_id")
            .and_then(Value::as_i64)
            .is_none()
        {
            if let Some(comment_database_id) = comment_database_id {
                object.insert(
                    "comment_database_id".to_string(),
                    json!(comment_database_id),
                );
            }
        }
    }
    value
}

fn thread_identifier_action_key(
    item: &Value,
    stable_marker_key_counts: &BTreeMap<String, usize>,
) -> String {
    let item_id = string_field(item, "item_id");
    if !item_id.is_empty() {
        return format!("item_id:{item_id}");
    }
    let body_hash = string_field(item, "body_hash");
    if !body_hash.is_empty() {
        return format!("body_hash:{body_hash}");
    }
    let stable_marker_key = string_field(item, "stable_marker_key");
    if stable_marker_key_counts
        .get(&stable_marker_key)
        .copied()
        .unwrap_or_default()
        == 1
    {
        return format!("stable_marker_key:{stable_marker_key}");
    }
    String::new()
}

fn pending_action_thread_identifier_key(value: &Value) -> String {
    let item_id = string_field(value, "item_id");
    if !item_id.is_empty() {
        return format!("item_id:{item_id}");
    }
    let body_hash = string_field(value, "body_hash");
    if !body_hash.is_empty() {
        return format!("body_hash:{body_hash}");
    }
    let stable_marker_key = string_field(value, "stable_marker_key");
    if !stable_marker_key.is_empty() {
        return format!("stable_marker_key:{stable_marker_key}");
    }
    String::new()
}

fn evaluation_identity_key(value: &Value) -> String {
    format!(
        "{}:{}:{}",
        string_field(value, "item_id"),
        string_field(value, "stable_marker_key"),
        string_field(value, "body_hash")
    )
}

fn pending_action_collision_key(action: &Value) -> String {
    action
        .get("idempotency_key")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| {
            format!(
                "{}:{}:{}:{}:{}",
                string_field(action, "source_head_sha"),
                action
                    .get("remediation_output_head")
                    .and_then(Value::as_str)
                    .or_else(|| action
                        .get("remediation_output_head_sha")
                        .and_then(Value::as_str))
                    .unwrap_or("none"),
                string_field(action, "body_hash"),
                string_field(action, "action_kind"),
                string_field(action, "stable_marker_key")
            )
        })
}

fn current_evaluation_marker_action(
    binding: &PrFollowupBinding,
    evaluation: &Value,
    feedback_item: Option<&Value>,
    params: &Value,
    clock: &dyn ClockSleeper,
) -> Option<Value> {
    // CodeRabbit summary/walkthrough evaluations are deterministically classified
    // "invalid" purely as a readiness signal. Never derive a live marker action
    // for them on the refresh path, or they would post a top-level PR comment.
    if value_has_summary_marker_key(evaluation) {
        return None;
    }
    let decision = evaluation.get("decision").and_then(Value::as_str)?;
    let action_kind = match decision {
        "invalid" => "comment_invalid",
        "out_of_scope" => "comment_out_of_scope",
        "needs_user_judgment" => {
            if params
                .get("post_needs_user_judgment_comments")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                "comment_needs_user_judgment"
            } else {
                "skip_needs_user_judgment"
            }
        }
        _ => return None,
    };
    let item_id = string_field(evaluation, "item_id");
    let stable_marker_key = string_field(evaluation, "stable_marker_key");
    let body_hash = string_field(evaluation, "body_hash");
    let source_head_sha = evaluation
        .get("head_sha")
        .and_then(Value::as_str)
        .unwrap_or(&binding.head_sha);
    let reason = evaluation
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or(decision);
    let response_text = evaluation
        .get("response_text")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty());
    let thread_id = feedback_item
        .and_then(|item| item.get("thread_id"))
        .and_then(Value::as_str);
    let comment_database_id = feedback_item
        .and_then(|item| item.get("comment_database_id"))
        .and_then(Value::as_i64);
    let idempotency_key = format!(
        "{}:{}:{}:{}:{}:{}:{}:{}",
        binding.run_id,
        binding.repository_owner,
        binding.repository_name,
        binding.pr_number,
        source_head_sha,
        "none",
        stable_marker_key,
        action_kind
    );
    Some(json!({
        "action_id": format!("{action_kind}:{stable_marker_key}:{body_hash}:none"),
        "action_kind": action_kind,
        "item_id": item_id,
        "original_feedback_identity": {
            "item_id": item_id,
            "stable_marker_key": stable_marker_key,
            "body_hash": body_hash,
            "source_head_sha": source_head_sha,
            "thread_id": thread_id,
            "comment_database_id": comment_database_id
        },
        "stable_marker_key": stable_marker_key,
        "source_head_sha": source_head_sha,
        "remediation_input_head_sha": source_head_sha,
        "remediation_output_head_sha": Value::Null,
        "remediation_output_head": "none",
        "body_hash": body_hash,
        "idempotency_key": idempotency_key,
        "comment_body_template_id": action_kind,
        "comment_body_artifact_path": Value::Null,
        "resolution_required": false,
        "status": "pending",
        "reason": reason,
        "response_text": response_text,
        "thread_id": thread_id,
        "comment_database_id": comment_database_id,
        "evidence": evaluation,
        "derived_from_current_artifacts": true,
        "derived_at": clock.now_rfc3339()
    }))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-026
/// @pseudocode lines 41-49
fn pending_marker_action_from_value(value: Value) -> Result<PendingMarkerAction, EngineError> {
    let action_kind = string_field(&value, "action_kind");
    let item_id = string_field(&value, "item_id");
    let stable_marker_key = string_field(&value, "stable_marker_key");
    let source_head_sha = string_field(&value, "source_head_sha");
    let remediation_output_head = value
        .get("remediation_output_head")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            value
                .get("remediation_output_head_sha")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| "none".to_string());
    let body_hash = string_field(&value, "body_hash");
    let reason = value
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("recorded marker action")
        .to_string();
    let response_text = value
        .get("response_text")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(ToString::to_string);
    let thread_id = value
        .get("thread_id")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            value
                .pointer("/evidence/thread_id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            value
                .pointer("/original_feedback_identity/thread_id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        });
    let comment_database_id = value
        .get("comment_database_id")
        .and_then(Value::as_i64)
        .or_else(|| {
            value
                .pointer("/evidence/comment_database_id")
                .and_then(Value::as_i64)
        })
        .or_else(|| {
            value
                .pointer("/original_feedback_identity/comment_database_id")
                .and_then(Value::as_i64)
        });
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("pending")
        .to_string();
    let resolution_required = value
        .get("resolution_required")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| unified_status_requires_resolution(&action_kind, &value));
    Ok(PendingMarkerAction {
        value,
        action_kind,
        item_id,
        stable_marker_key,
        source_head_sha,
        remediation_output_head,
        body_hash,
        reason,
        response_text,
        thread_id,
        comment_database_id,
        resolution_required,
        status,
    })
}

/// Derive whether Luther must resolve the review thread from the unified
/// per-item status implied by the marker action and its remediation result.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-026
fn unified_status_requires_resolution(action_kind: &str, value: &Value) -> bool {
    let remediation_status = value
        .get("remediation_result_status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    matches!(action_kind, "comment_fixed")
        && matches!(
            remediation_status,
            "" | "fixed" | "changed" | "already_satisfied" | "not_reproduced"
        )
}

/// Post the agent-authored reply on the original review thread when a numeric
/// review comment id is available. Only issue-comment marker actions may post a
/// top-level PR comment.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016
fn post_marker_reply(
    binding: &PrFollowupBinding,
    runner: &dyn GithubPrCommandRunner,
    action: &PendingMarkerAction,
    comment_key: &str,
    body: &str,
    body_path: &Path,
) -> Result<Value, EngineError> {
    let (endpoint, in_thread_reply) = match action.comment_database_id {
        Some(comment_database_id) => (
            format!(
                "/repos/{}/{}/pulls/{}/comments/{}/replies",
                binding.repository_owner,
                binding.repository_name,
                binding.pr_number,
                comment_database_id
            ),
            true,
        ),
        None if !marker_action_requires_review_thread_reply(action) => (
            format!(
                "/repos/{}/{}/issues/{}/comments",
                binding.repository_owner, binding.repository_name, binding.pr_number
            ),
            false,
        ),
        None => {
            return Err(github_feedback_error(format!(
                "review-thread marker action {} missing comment_database_id",
                action
                    .value
                    .get("action_id")
                    .and_then(Value::as_str)
                    .unwrap_or(&action.stable_marker_key)
            )));
        }
    };
    let response = runner.run_github_command(&[
        "gh".to_string(),
        "api".to_string(),
        endpoint,
        "--method".to_string(),
        "POST".to_string(),
        "--field".to_string(),
        format!("body=@{}", body_path.display()),
    ])?;
    let parsed: Value =
        serde_json::from_str(&response).unwrap_or_else(|_| json!({ "raw_response": response }));
    Ok(json!({
        "idempotency_key": comment_key,
        "comment_id": parsed.get("id").cloned().unwrap_or(Value::Null),
        "comment_url": parsed.get("html_url").cloned().unwrap_or(Value::Null),
        "in_thread_reply": in_thread_reply,
        "in_reply_to_id": parsed.get("in_reply_to_id").cloned().unwrap_or(Value::Null),
        "body_hash": stable_hash(body),
        "body_path": body_path.display().to_string(),
        "action_id": action.value.get("action_id").cloned().unwrap_or(Value::Null),
        "warnings": if in_thread_reply { json!([]) } else { json!(["no_comment_database_id_posted_top_level_comment"]) },
        "source": "posted"
    }))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-016,REQ-PRFU-026
/// @pseudocode lines 42-44,47-49
fn read_local_marker_completions(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    if let Ok(report) = store.read_current_json(binding, MARKER_ARTIFACT_FAMILY) {
        for section in ["posted_comments", "resolved_threads", "skipped_actions"] {
            for entry in report
                .get(section)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(key) = entry.get("idempotency_key").and_then(Value::as_str) {
                    keys.insert(key.to_string());
                }
            }
        }
    }
    keys
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-016,REQ-PRFU-017
/// @pseudocode lines 42-45
fn discover_marker_remote_comments(
    runner: &dyn GithubPrCommandRunner,
    binding: &PrFollowupBinding,
) -> Result<Vec<Value>, EngineError> {
    let mut comments = query_issue_comments(runner, binding)?;
    // Also scan in-thread review (pull) comments so previously posted in-thread
    // reply markers are detected for idempotency on retry/resume.
    comments.extend(query_pull_review_comments(runner, binding).unwrap_or_default());
    Ok(comments)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-016,REQ-PRFU-017
fn query_pull_review_comments(
    runner: &dyn GithubPrCommandRunner,
    binding: &PrFollowupBinding,
) -> Result<Vec<Value>, EngineError> {
    query_paginated_array(
        runner,
        &format!(
            "/repos/{}/{}/pulls/{}/comments?per_page=100&page=",
            binding.repository_owner, binding.repository_name, binding.pr_number
        ),
    )
}

/// Deterministically validate every pending marker action before issuing any
/// GitHub side effect. Returns `Some(violations)` when at least one action is
/// malformed so the caller can stop before mutating GitHub; `None` when all
/// actions are safe to execute.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-026
fn validate_marker_actions_before_mutation(actions: &[PendingMarkerAction]) -> Option<Vec<Value>> {
    let mut violations = Vec::new();
    let mut seen_item_ids = BTreeSet::new();
    for action in actions {
        if action.action_kind == "skip_needs_user_judgment" {
            continue;
        }
        // Informational summary/walkthrough markers are skipped at the mutation
        // gate (post nothing, resolve nothing), so they cannot violate any
        // mutation precondition.
        if is_summary_marker_key(&action.stable_marker_key) {
            continue;
        }
        if !action.item_id.is_empty() && !seen_item_ids.insert(action.item_id.clone()) {
            violations.push(json!({
                "item_id": action.item_id,
                "stable_marker_key": action.stable_marker_key,
                "violation": "duplicate_result_for_item"
            }));
        }
        let response_text_present = action
            .response_text
            .as_deref()
            .map(str::trim)
            .is_some_and(|text| !text.is_empty());
        if !response_text_present {
            violations.push(json!({
                "item_id": action.item_id,
                "stable_marker_key": action.stable_marker_key,
                "action_kind": action.action_kind,
                "violation": "missing_response_text"
            }));
        }
        if action.resolution_required && action.thread_id.is_none() {
            violations.push(json!({
                "item_id": action.item_id,
                "stable_marker_key": action.stable_marker_key,
                "action_kind": action.action_kind,
                "violation": "resolution_required_without_thread_id"
            }));
        }
        if marker_action_requires_review_thread_reply(action)
            && action.comment_database_id.is_none()
        {
            violations.push(json!({
                "item_id": action.item_id,
                "stable_marker_key": action.stable_marker_key,
                "action_kind": action.action_kind,
                "violation": "review_thread_reply_without_comment_database_id"
            }));
        }
    }
    if violations.is_empty() {
        None
    } else {
        Some(violations)
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017,REQ-PRFU-026
/// @pseudocode lines 43-49
fn marker_action_requires_review_thread_reply(action: &PendingMarkerAction) -> bool {
    matches!(
        action.action_kind.as_str(),
        "comment_fixed"
            | "comment_invalid"
            | "comment_out_of_scope"
            | "comment_needs_user_judgment"
    ) && (action.thread_id.is_some()
        || action.stable_marker_key.starts_with("review-comment:")
        || action.stable_marker_key.starts_with("thread:"))
}

#[allow(clippy::too_many_arguments)]
// Pre-existing marker action workflow; split in a dedicated refactor stage.
#[allow(clippy::too_many_lines)]
fn process_marker_action(
    binding: &PrFollowupBinding,
    store: &PrFollowupArtifactStore,
    step_id: &str,
    step_order: u64,
    runner: &dyn GithubPrCommandRunner,
    clock: &dyn ClockSleeper,
    action: PendingMarkerAction,
    local_completed: &BTreeSet<String>,
    remote_completed: &BTreeSet<String>,
    params: &Value,
) -> Result<MarkerActionOutcome, EngineError> {
    let comment_key = marker_action_key(binding, &action, "comment");
    let resolution_key = marker_action_key(binding, &action, "resolution");
    // Final safety net: a CodeRabbit summary/walkthrough marker is informational
    // only. Even if a stale/pre-fix summary action slips past the earlier gates,
    // it must post nothing and resolve nothing here.
    if is_summary_marker_key(&action.stable_marker_key) {
        return Ok(skipped_summary_marker_outcome(
            action,
            comment_key,
            resolution_key,
            clock,
        ));
    }
    if action.action_kind == "skip_needs_user_judgment" {
        return Ok(skipped_needs_user_judgment_outcome(
            action,
            comment_key,
            resolution_key,
            clock,
        ));
    }
    if let Some(outcome) = validate_marker_action_evidence(
        binding,
        store,
        action.clone(),
        comment_key.clone(),
        resolution_key.clone(),
        clock,
    )? {
        return Ok(outcome);
    }
    let comment_already_done = local_completed.contains(&comment_key)
        || remote_completed.contains(&comment_key)
        || action.status == "completed";
    let mut posted_comment = None;
    let mut skipped = Vec::new();
    let mut partial = None;
    let mut retryable = None;
    let mut failed = None;
    let body = render_marker_comment_body(binding, &action);
    if !comment_already_done {
        let body_path = write_marker_comment_body_file(
            store, binding, step_id, step_order, &action, &body, clock,
        )?;
        posted_comment = Some(post_marker_reply(
            binding,
            runner,
            &action,
            &comment_key,
            &body,
            &body_path,
        )?);
    } else {
        skipped.push(json!({
            "idempotency_key": comment_key,
            "action_id": action.value.get("action_id").cloned().unwrap_or(Value::Null),
            "reason": if remote_completed.contains(&comment_key) { "already_completed_remote" } else { "already_completed_local" },
            "action_kind": action.action_kind
        }));
    }

    let resolution_policy = resolution_policy(&action, params);
    let mut resolved_thread = None;
    let mut resolve_attempted = false;
    let mut resolve_succeeded = false;
    let mut resolve_error: Option<String> = None;
    let mut final_thread_resolved_state: Option<bool> = None;
    if resolution_policy == "skip" {
        skipped.push(json!({
            "idempotency_key": resolution_key,
            "action_id": action.value.get("action_id").cloned().unwrap_or(Value::Null),
            "reason": "resolution_skipped_by_policy",
            "action_kind": "resolve_thread"
        }));
    } else if action.thread_id.is_none() {
        if resolution_policy == "required" {
            partial = Some(json!({
                "idempotency_key": resolution_key,
                "action_id": action.value.get("action_id").cloned().unwrap_or(Value::Null),
                "reason": "resolution_unavailable",
                "partial_state": "comment_posted_resolution_pending"
            }));
            retryable = partial.clone();
        } else {
            skipped.push(json!({
                "idempotency_key": resolution_key,
                "action_id": action.value.get("action_id").cloned().unwrap_or(Value::Null),
                "reason": "handled_comment_only",
                "action_kind": "resolve_thread"
            }));
        }
    } else if local_completed.contains(&resolution_key)
        || remote_completed.contains(&resolution_key)
    {
        skipped.push(json!({
            "idempotency_key": resolution_key,
            "action_id": action.value.get("action_id").cloned().unwrap_or(Value::Null),
            "reason": "resolution_already_completed",
            "action_kind": "resolve_thread"
        }));
    } else {
        let thread_id = action.thread_id.clone().unwrap_or_default();
        resolve_attempted = true;
        let response = runner.run_github_command(&[
            "gh".to_string(),
            "api".to_string(),
            "graphql".to_string(),
            "-f".to_string(),
            format!("query={RESOLVE_REVIEW_THREAD_MUTATION}"),
            "-f".to_string(),
            format!("threadId={thread_id}"),
        ]);
        match response {
            Ok(output) => {
                let parsed: Value = serde_json::from_str(&output)
                    .unwrap_or_else(|_| json!({ "raw_response": output }));
                final_thread_resolved_state = parsed
                    .pointer("/data/resolveReviewThread/thread/isResolved")
                    .and_then(Value::as_bool);
                resolve_succeeded =
                    parsed.get("errors").is_none() && final_thread_resolved_state == Some(true);
                let resolution_record = json!({
                    "idempotency_key": resolution_key,
                    "thread_id": thread_id,
                    "response": parsed,
                    "final_thread_resolved_state": final_thread_resolved_state,
                    "action_id": action.value.get("action_id").cloned().unwrap_or(Value::Null)
                });
                if resolve_succeeded {
                    resolved_thread = Some(resolution_record);
                } else {
                    let error = "resolution_failed_after_comment".to_string();
                    resolve_error = Some(error.clone());
                    partial = Some(json!({
                        "idempotency_key": resolution_key,
                        "action_id": action.value.get("action_id").cloned().unwrap_or(Value::Null),
                        "reason": "resolution_failed_after_comment",
                        "error": error,
                        "partial_state": "comment_posted_resolution_pending",
                        "resolve_response": resolution_record
                    }));
                    retryable = partial.clone();
                }
            }
            Err(err) => {
                resolve_error = Some(err.to_string());
                partial = Some(json!({
                    "idempotency_key": resolution_key,
                    "action_id": action.value.get("action_id").cloned().unwrap_or(Value::Null),
                    "reason": "resolution_failed_after_comment",
                    "error": err.to_string(),
                    "partial_state": "comment_posted_resolution_pending"
                }));
                retryable = partial.clone();
            }
        }
    }

    if action.action_kind == "comment_needs_user_judgment" {
        partial = Some(json!({
            "idempotency_key": comment_key,
            "action_id": action.value.get("action_id").cloned().unwrap_or(Value::Null),
            "reason": "unhandled_needs_user_judgment",
            "partial_state": "unhandled_needs_user_judgment"
        }));
    }
    if failed.is_none()
        && partial.is_none()
        && posted_comment.is_none()
        && skipped.is_empty()
        && resolved_thread.is_none()
    {
        failed = Some(json!({
            "idempotency_key": comment_key,
            "action_id": action.value.get("action_id").cloned().unwrap_or(Value::Null),
            "reason": "marker_action_not_handled"
        }));
    }

    let status = if failed.is_some() || partial.is_some() {
        "failed"
    } else {
        "completed"
    };
    let mut updated_action = action.value.clone();
    if let Some(object) = updated_action.as_object_mut() {
        object.insert("status".to_string(), json!(status));
        object.insert("comment_idempotency_key".to_string(), json!(comment_key));
        object.insert(
            "resolution_idempotency_key".to_string(),
            json!(resolution_key),
        );
        object.insert("updated_at".to_string(), json!(clock.now_rfc3339()));
    }
    let audit = marker_action_audit(
        &action,
        status,
        &comment_key,
        posted_comment.as_ref(),
        &ResolveAudit {
            resolve_attempted,
            resolve_succeeded,
            resolve_error: resolve_error.as_deref(),
            final_thread_resolved_state,
        },
    );
    Ok(MarkerActionOutcome {
        action,
        status: status.to_string(),
        comment_key,
        resolution_key,
        posted_comment,
        resolved_thread,
        skipped,
        partial,
        retryable,
        failed,
        audit,
        updated_action,
    })
}

/// Build the per-item idempotent audit record linking the feedback item,
/// review thread, reply, and resolve result.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-016,REQ-PRFU-026
/// Resolve-side audit fields grouped to keep the audit builder within argument
/// limits.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-016,REQ-PRFU-026
struct ResolveAudit<'a> {
    resolve_attempted: bool,
    resolve_succeeded: bool,
    resolve_error: Option<&'a str>,
    final_thread_resolved_state: Option<bool>,
}

fn marker_action_audit(
    action: &PendingMarkerAction,
    status: &str,
    comment_key: &str,
    posted_comment: Option<&Value>,
    resolve: &ResolveAudit,
) -> Value {
    json!({
        "item_id": action.item_id,
        "stable_marker_key": action.stable_marker_key,
        "review_thread_id": action.thread_id,
        "comment_database_id": action.comment_database_id,
        "action_kind": action.action_kind,
        "status": status,
        "idempotency_key": comment_key,
        "reply_comment_id": posted_comment
            .and_then(|comment| comment.get("comment_id").cloned())
            .unwrap_or(Value::Null),
        "reply_comment_url": posted_comment
            .and_then(|comment| comment.get("comment_url").cloned())
            .unwrap_or(Value::Null),
        "in_thread_reply": posted_comment
            .and_then(|comment| comment.get("in_thread_reply").and_then(Value::as_bool))
            .unwrap_or(false),
        "resolve_attempted": resolve.resolve_attempted,
        "resolve_succeeded": resolve.resolve_succeeded,
        "resolve_error": resolve.resolve_error,
        "final_thread_resolved_state": resolve.final_thread_resolved_state
    })
}

/// Fallback visible reply body used only when the agent supplied no
/// `response_text` for the item.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017
fn render_default_marker_visible(action: &PendingMarkerAction) -> String {
    match action.action_kind.as_str() {
        "comment_fixed" => format!(
            "Luther follow-up: fixed CodeRabbit feedback `{}`. Evidence: {}.",
            action.stable_marker_key,
            sanitize_visible_text(&action.reason)
        ),
        "comment_out_of_scope" => format!(
            "Luther follow-up: CodeRabbit feedback `{}` is out of scope. Reason: {}.",
            action.stable_marker_key,
            sanitize_visible_text(&action.reason)
        ),
        "comment_needs_user_judgment" => format!(
            "Luther follow-up: CodeRabbit feedback `{}` needs user judgment. Reason: {}.",
            action.stable_marker_key,
            sanitize_visible_text(&action.reason)
        ),
        _ => format!(
            "Luther follow-up: CodeRabbit feedback `{}` is not valid. Reason: {}.",
            action.stable_marker_key,
            sanitize_visible_text(&action.reason)
        ),
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017
/// @pseudocode lines 43-45
fn render_marker_comment_body(binding: &PrFollowupBinding, action: &PendingMarkerAction) -> String {
    let visible = action
        .response_text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(sanitize_visible_text)
        .unwrap_or_else(|| render_default_marker_visible(action));
    let marker = format!(
        "<!-- {MARKER_NAMESPACE} marker_key={} source_head={} remediation_output_head={} body={} action={} run_id={} -->",
        action.stable_marker_key,
        action.source_head_sha,
        action.remediation_output_head,
        action.body_hash,
        marker_comment_action_kind(&action.action_kind),
        binding.run_id
    );
    format!("{visible}\n\n{marker}\n")
}

/// Final-safety-net outcome for an informational CodeRabbit summary/walkthrough
/// marker that reached the mutation gate. Posts nothing and resolves nothing,
/// recording a clean `skipped` entry so the marker report stays complete and no
/// top-level PR comment is ever created (even on reruns).
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-020
fn skipped_summary_marker_outcome(
    action: PendingMarkerAction,
    comment_key: String,
    resolution_key: String,
    clock: &dyn ClockSleeper,
) -> MarkerActionOutcome {
    let skipped = vec![
        json!({
            "idempotency_key": comment_key,
            "action_id": action.value.get("action_id").cloned().unwrap_or(Value::Null),
            "reason": "coderabbit_summary_informational_only",
            "action_kind": action.action_kind
        }),
        json!({
            "idempotency_key": resolution_key,
            "action_id": action.value.get("action_id").cloned().unwrap_or(Value::Null),
            "reason": "coderabbit_summary_informational_only",
            "action_kind": "resolve_thread"
        }),
    ];
    let mut updated_action = action.value.clone();
    if let Some(object) = updated_action.as_object_mut() {
        object.insert("status".to_string(), json!("skipped"));
        object.insert("comment_idempotency_key".to_string(), json!(comment_key));
        object.insert(
            "resolution_idempotency_key".to_string(),
            json!(resolution_key),
        );
        object.insert("updated_at".to_string(), json!(clock.now_rfc3339()));
        object.insert(
            "skipped_reason".to_string(),
            json!("coderabbit_summary_informational_only"),
        );
    }
    let audit = marker_action_audit(
        &action,
        "skipped",
        &comment_key,
        None,
        &ResolveAudit {
            resolve_attempted: false,
            resolve_succeeded: false,
            resolve_error: None,
            final_thread_resolved_state: None,
        },
    );
    MarkerActionOutcome {
        action,
        status: "skipped".to_string(),
        comment_key,
        resolution_key,
        posted_comment: None,
        resolved_thread: None,
        skipped,
        partial: None,
        retryable: None,
        failed: None,
        audit,
        updated_action,
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-017
/// @pseudocode lines 45
fn skipped_needs_user_judgment_outcome(
    action: PendingMarkerAction,
    comment_key: String,
    resolution_key: String,
    clock: &dyn ClockSleeper,
) -> MarkerActionOutcome {
    let skipped = vec![
        json!({
            "idempotency_key": comment_key,
            "action_id": action.value.get("action_id").cloned().unwrap_or(Value::Null),
            "reason": "needs_user_judgment_comments_disabled",
            "action_kind": "comment_needs_user_judgment"
        }),
        json!({
            "idempotency_key": resolution_key,
            "action_id": action.value.get("action_id").cloned().unwrap_or(Value::Null),
            "reason": "resolution_skipped_needs_user_judgment",
            "action_kind": "resolve_thread"
        }),
    ];
    let partial = Some(json!({
        "idempotency_key": comment_key,
        "action_id": action.value.get("action_id").cloned().unwrap_or(Value::Null),
        "reason": "unhandled_needs_user_judgment",
        "partial_state": "unhandled_needs_user_judgment"
    }));
    let mut updated_action = action.value.clone();
    if let Some(object) = updated_action.as_object_mut() {
        object.insert("status".to_string(), json!("failed"));
        object.insert("comment_idempotency_key".to_string(), json!(comment_key));
        object.insert(
            "resolution_idempotency_key".to_string(),
            json!(resolution_key),
        );
        object.insert("updated_at".to_string(), json!(clock.now_rfc3339()));
        object.insert(
            "skipped_reason".to_string(),
            json!("needs_user_judgment_comments_disabled"),
        );
    }
    let audit = marker_action_audit(
        &action,
        "failed",
        &comment_key,
        None,
        &ResolveAudit {
            resolve_attempted: false,
            resolve_succeeded: false,
            resolve_error: None,
            final_thread_resolved_state: None,
        },
    );
    MarkerActionOutcome {
        action,
        status: "failed".to_string(),
        comment_key,
        resolution_key,
        posted_comment: None,
        resolved_thread: None,
        skipped,
        partial: partial.clone(),
        retryable: None,
        failed: None,
        audit,
        updated_action,
    }
}

fn validate_marker_action_evidence(
    binding: &PrFollowupBinding,
    store: &PrFollowupArtifactStore,
    action: PendingMarkerAction,
    comment_key: String,
    resolution_key: String,
    clock: &dyn ClockSleeper,
) -> Result<Option<MarkerActionOutcome>, EngineError> {
    if !matches!(
        action.action_kind.as_str(),
        "comment_fixed" | "resolve_thread"
    ) {
        return Ok(None);
    }
    let result = store.read_current_json(binding, "pr-remediation-result");
    let valid = result
        .as_ref()
        .ok()
        .is_some_and(|payload| marker_action_has_validator_success(binding, &action, payload));
    if valid {
        return Ok(None);
    }
    let failed = json!({
        "idempotency_key": comment_key,
        "resolution_idempotency_key": resolution_key,
        "action_id": action.value.get("action_id").cloned().unwrap_or(Value::Null),
        "reason": "missing_validator_success_evidence",
        "failure_state": "failed_fatal"
    });
    let mut updated_action = action.value.clone();
    if let Some(object) = updated_action.as_object_mut() {
        object.insert("status".to_string(), json!("failed"));
        object.insert("comment_idempotency_key".to_string(), json!(comment_key));
        object.insert(
            "resolution_idempotency_key".to_string(),
            json!(resolution_key),
        );
        object.insert(
            "failure_reason".to_string(),
            json!("missing_validator_success_evidence"),
        );
        object.insert("updated_at".to_string(), json!(clock.now_rfc3339()));
    }
    let audit = marker_action_audit(
        &action,
        "failed",
        &comment_key,
        None,
        &ResolveAudit {
            resolve_attempted: false,
            resolve_succeeded: false,
            resolve_error: None,
            final_thread_resolved_state: None,
        },
    );
    Ok(Some(MarkerActionOutcome {
        action,
        status: "failed".to_string(),
        comment_key,
        resolution_key,
        posted_comment: None,
        resolved_thread: None,
        skipped: Vec::new(),
        partial: None,
        retryable: None,
        failed: Some(failed),
        audit,
        updated_action,
    }))
}

fn marker_action_has_validator_success(
    binding: &PrFollowupBinding,
    action: &PendingMarkerAction,
    result: &Value,
) -> bool {
    if result.get("validation_state").and_then(Value::as_str) != Some("valid") {
        return false;
    }
    if action
        .value
        .get("remediation_result_evidence")
        .is_none_or(|evidence| evidence.is_null())
    {
        return false;
    }
    let result_input_head = result
        .get("input_head_sha")
        .and_then(Value::as_str)
        .unwrap_or(&binding.head_sha);
    if result_input_head != action.source_head_sha {
        return false;
    }
    if action.remediation_output_head != "none"
        && result.get("output_head_sha").and_then(Value::as_str)
            != Some(action.remediation_output_head.as_str())
    {
        return false;
    }
    result
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|item| {
            item.get("source_type").and_then(Value::as_str) == Some("coderabbit_feedback")
                && item.get("source_id").and_then(Value::as_str) == Some(action.item_id.as_str())
                && matches!(
                    item.get("status").and_then(Value::as_str),
                    Some("fixed" | "changed" | "already_satisfied" | "not_reproduced")
                )
                && item.get("input_head_sha").and_then(Value::as_str)
                    == Some(action.source_head_sha.as_str())
                && item.get("body_hash").and_then(Value::as_str) == Some(action.body_hash.as_str())
                && item.get("stable_marker_key").and_then(Value::as_str)
                    == Some(action.stable_marker_key.as_str())
                && item
                    .get("evidence")
                    .is_some_and(|evidence| !evidence.is_null())
                && result
                    .get("run_id")
                    .and_then(Value::as_str)
                    .unwrap_or(&binding.run_id)
                    == binding.run_id
        })
}

fn write_marker_comment_body_file(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    action: &PendingMarkerAction,
    body: &str,
    clock: &dyn ClockSleeper,
) -> Result<PathBuf, EngineError> {
    let record = store.write_raw_text_artifact(
        binding,
        "feedback-marker-comment-body",
        step_id,
        step_order,
        &format!(
            "{}-{}",
            action.action_kind,
            stable_hash(&action.stable_marker_key)
        ),
        body,
        clock,
    )?;
    let body_path = record.history_path.with_extension("body.md");
    std::fs::write(&body_path, body).map_err(|err| {
        github_feedback_error(format!(
            "write marker comment body file {}: {err}",
            body_path.display()
        ))
    })?;
    Ok(body_path)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-026
/// @pseudocode lines 46-49
fn resolution_policy(action: &PendingMarkerAction, params: &Value) -> &'static str {
    // needs_user_judgment is always left open for a human to decide.
    if action.action_kind == "comment_needs_user_judgment" {
        return "skip";
    }
    // Legacy explicit overrides remain honored for fixed feedback only.
    if action.action_kind == "comment_fixed" {
        if let Some(resolve_fixed) = params.get("resolve_fixed").and_then(Value::as_bool) {
            return if resolve_fixed { "required" } else { "skip" };
        }
    }
    if action.action_kind == "comment_out_of_scope" {
        return "skip";
    }
    // Default policy: resolve fixed/changed/already_satisfied/not_reproduced
    // (carried via resolution_required); leave discounted items open.
    if action.resolution_required {
        "required"
    } else {
        "skip"
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-016,REQ-PRFU-026
/// @pseudocode lines 43-49
fn marker_action_key(
    binding: &PrFollowupBinding,
    action: &PendingMarkerAction,
    operation: &str,
) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
        operation,
        binding.run_id,
        binding.repository_owner,
        binding.repository_name,
        binding.pr_number,
        action.source_head_sha,
        action.remediation_output_head,
        action.body_hash,
        marker_comment_action_kind(&action.action_kind),
        action.stable_marker_key
    )
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-016
/// @pseudocode lines 43-44
fn marker_action_key_from_marker(
    binding: &PrFollowupBinding,
    marker: &RemoteFeedbackMarker,
    operation: &str,
) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
        operation,
        marker.run_id,
        binding.repository_owner,
        binding.repository_name,
        binding.pr_number,
        marker.source_head_sha,
        marker
            .remediation_output_head_sha
            .clone()
            .unwrap_or_else(|| "none".to_string()),
        marker.body_hash,
        marker.action_kind,
        marker.stable_marker_key
    )
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-016
/// @pseudocode lines 43-44
fn marker_comment_action_kind(action_kind: &str) -> &str {
    match action_kind {
        "resolve_thread" => "resolve_thread",
        other => other,
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-026
/// @pseudocode lines 47-49
fn marker_report_payload(
    binding: &PrFollowupBinding,
    outcomes: &[MarkerActionOutcome],
    malformed_remote_markers: Vec<Value>,
    marked_at: String,
) -> Value {
    let posted_comments = outcomes
        .iter()
        .filter_map(|outcome| outcome.posted_comment.clone())
        .collect::<Vec<_>>();
    let resolved_threads = outcomes
        .iter()
        .filter_map(|outcome| outcome.resolved_thread.clone())
        .collect::<Vec<_>>();
    let skipped_actions = outcomes
        .iter()
        .flat_map(|outcome| outcome.skipped.clone())
        .collect::<Vec<_>>();
    let partial_actions = outcomes
        .iter()
        .filter_map(|outcome| outcome.partial.clone())
        .collect::<Vec<_>>();
    let retryable_actions = outcomes
        .iter()
        .filter_map(|outcome| outcome.retryable.clone())
        .collect::<Vec<_>>();
    let failed_actions = outcomes
        .iter()
        .filter_map(|outcome| outcome.failed.clone())
        .collect::<Vec<_>>();
    json!({
        "schema_version": PR_FOLLOWUP_SCHEMA_VERSION,
        "run_id": binding.run_id,
        "repository_owner": binding.repository_owner,
        "repository_name": binding.repository_name,
        "pr_number": binding.pr_number,
        "head_ref": binding.head_ref,
        "head_sha": binding.head_sha,
        "base_ref": binding.base_ref,
        "base_sha": binding.base_sha,
        "marked_at": marked_at,
        "posted_comments": posted_comments,
        "resolved_threads": resolved_threads,
        "skipped_actions": skipped_actions,
        "partial_actions": partial_actions,
        "retryable_actions": retryable_actions,
        "failed_actions": failed_actions,
        "malformed_remote_markers": malformed_remote_markers,
        "action_statuses": outcomes.iter().map(|outcome| json!({
            "action_id": outcome.action.value.get("action_id").cloned().unwrap_or(Value::Null),
            "item_id": outcome.action.item_id,
            "status": outcome.status,
            "comment_idempotency_key": outcome.comment_key,
            "resolution_idempotency_key": outcome.resolution_key
        })).collect::<Vec<_>>(),
        "action_audit": outcomes.iter().map(|outcome| outcome.audit.clone()).collect::<Vec<_>>()
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-016,REQ-PRFU-026
/// @pseudocode lines 47-49
fn write_updated_pending_actions(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    pending_artifact: &mut Value,
    outcomes: &[MarkerActionOutcome],
    clock: &dyn ClockSleeper,
) -> Result<(), EngineError> {
    let mut actions = pending_artifact
        .get("pending_actions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for outcome in outcomes {
        if let Some(action_id) = outcome
            .action
            .value
            .get("action_id")
            .and_then(Value::as_str)
        {
            if let Some(existing) = actions
                .iter_mut()
                .find(|action| action.get("action_id").and_then(Value::as_str) == Some(action_id))
            {
                *existing = outcome.updated_action.clone();
            }
        }
    }
    let payload = json!({
        "pending_actions": actions,
        "carry_forward_from_artifact_sequence": pending_artifact.get("artifact_sequence").cloned().unwrap_or(Value::Null),
        "marker_policy": pending_artifact.get("marker_policy").cloned().unwrap_or_else(|| json!({})),
        "updated_at": clock.now_rfc3339()
    });
    store.write_json_artifact(
        binding,
        PENDING_MARKER_ACTIONS_FAMILY,
        step_id,
        step_order,
        &payload,
        None,
        clock,
    )?;
    Ok(())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015
/// @pseudocode lines 45
fn sanitize_visible_text(text: &str) -> String {
    text.replace('`', "'")
        .replace("<!--", "&lt;!--")
        .replace("-->", "--&gt;")
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009
/// @pseudocode lines 20,26-28
fn write_feedback_artifacts(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    step_id: &str,
    step_order: u64,
    payload: &Value,
    clock: &dyn ClockSleeper,
    failure: Option<(&str, &str, Value)>,
) -> Result<(), EngineError> {
    store.write_json_artifact(
        binding,
        "coderabbit-feedback",
        step_id,
        step_order,
        payload,
        failure,
        clock,
    )?;
    let state_entries = payload
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|item| {
            json!({
                "item_id": item.get("item_id").cloned().unwrap_or(Value::Null),
                "stable_marker_key": item.get("stable_marker_key").cloned().unwrap_or(Value::Null),
                "body_hash": item.get("body_hash").cloned().unwrap_or(Value::Null),
                "head_sha": binding.head_sha,
                "first_seen_at": payload.get("observed_at").cloned().unwrap_or(Value::Null),
                "last_seen_at": payload.get("observed_at").cloned().unwrap_or(Value::Null),
                "evaluation_status": "unevaluated",
                "accepted_evaluation": null,
                "remediation_status": null,
                "marker_status": "pending",
                "resolution_status": null,
                "superseded": false,
                "stale": false,
                "reuse_eligible": false
            })
        })
        .collect::<Vec<_>>();
    store.write_json_artifact(binding, "coderabbit-feedback-state", step_id, step_order, &json!({
        "state_entries": state_entries,
        "state_index_hash": stable_hash(&serde_json::to_string(payload.get("items").unwrap_or(&Value::Null)).unwrap_or_default()),
        "superseded_entries": []
    }), None, clock)?;
    Ok(())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009
/// @pseudocode lines 14,20-23
#[allow(clippy::too_many_arguments)]
fn feedback_payload(
    binding: &PrFollowupBinding,
    readiness_state: &str,
    stable_count: u64,
    required_stable: u64,
    max_observations: u64,
    interval_seconds: u64,
    observations: &[Value],
    final_observation: &FeedbackObservation,
    identities: &BTreeSet<String>,
    budget_used: u64,
    observed_at: String,
    outcome_reason: &str,
) -> Value {
    let items = final_observation
        .items
        .iter()
        .map(item_json)
        .collect::<Vec<_>>();
    json!({
        "readiness_state": readiness_state,
        "stable_observation_count": stable_count,
        "required_stable_observations": required_stable,
        "max_observations": max_observations,
        "observation_interval_seconds": interval_seconds,
        "observations": observations,
        "items": items,
        "included_bot_identities": identities.iter().cloned().collect::<Vec<_>>(),
        "feedback_item_set_hash": item_set_hash(&final_observation.items),
        "items_count": final_observation.items.len(),
        "remote_marker_comments_seen": final_observation.remote_markers.iter().map(remote_marker_json).collect::<Vec<_>>(),
        "remote_marker_audit": final_observation.remote_marker_audit,
        "malformed_remote_markers": final_observation.malformed_remote_markers,
        "stale_feedback_items": final_observation.stale_items,
        "noise": final_observation.noise,
        "observed_at": observed_at,
        "budget_used": budget_used,
        "budget_remaining": max_observations.saturating_sub(budget_used),
        "outcome_reason": outcome_reason,
        "binding_head_sha": binding.head_sha
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 14-19
fn observation_json(
    observation: &FeedbackObservation,
    item_hash: &str,
    budget_used: u64,
    max_observations: u64,
    observed_at: &str,
    outcome_reason: &str,
) -> Value {
    json!({
        "observed_at": observed_at,
        "signals_seen": {
            "ready": observation.ready_signal,
            "in_progress": observation.in_progress_signal,
        },
        "bot_identities_matched": observation.matched_identities.iter().cloned().collect::<Vec<_>>(),
        "observation_hash": readiness_stability_hash(observation),
        "feedback_item_set_hash": item_hash,
        "readiness_stability_hash": readiness_stability_hash(observation),
        "budget_used": budget_used,
        "budget_remaining": max_observations.saturating_sub(budget_used),
        "items_count": observation.items.len(),
        "outcome_reason": outcome_reason,
        "current_head_ready_signal_seen": observation.ready_signal && !observation.in_progress_signal,
        "in_progress_signal_seen": observation.in_progress_signal,
        "readiness_signals": observation.readiness_signals,
        "stale_signals": observation.stale_signals,
        "noise_count": observation.noise.len()
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009
/// @pseudocode lines 7-14
fn item_json(item: &FeedbackItem) -> Value {
    json!({
        "item_id": item.item_id,
        "stable_marker_key": item.stable_marker_key,
        "thread_id": item.thread_id,
        "comment_id": item.comment_id,
        "comment_database_id": item.comment_database_id,
        "review_id": item.review_id,
        "author_login": item.author_login,
        "author_association": null,
        "bot_identity": item.author_login,
        "path": item.path,
        "line": item.line,
        "side": item.side,
        "body": item.body,
        "body_hash": item.body_hash,
        "url": item.url,
        "created_at": item.created_at,
        "updated_at": item.updated_at,
        "resolved": item.resolved,
        "outdated": item.outdated,
        "resolution_state_available": item.resolution_state_available,
        "source": item.source,
        "raw_node_id": item.raw_node_id,
        "commit_sha": item.commit_sha,
        "stale": item.stale
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-009
/// @pseudocode lines 13,20
fn remote_marker_json(marker: &RemoteFeedbackMarker) -> Value {
    json!({
        "stable_marker_key": marker.stable_marker_key,
        "source_head_sha": marker.source_head_sha,
        "remediation_output_head_sha": marker.remediation_output_head_sha,
        "body_hash": marker.body_hash,
        "action_kind": marker.action_kind,
        "run_id": marker.run_id,
        "status": marker.status
    })
}

#[derive(Debug)]
struct MarkerParseError {
    diagnostic: String,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-009
/// @pseudocode lines 13,20
fn parse_marker_from_comment_body(body: &str) -> Result<RemoteFeedbackMarker, MarkerParseError> {
    let start = body
        .find("<!--")
        .ok_or_else(|| marker_parse_error("missing marker comment"))?;
    let rest = &body[start..];
    let end = rest
        .find("-->")
        .ok_or_else(|| marker_parse_error("unterminated marker comment"))?;
    parse_hidden_marker(&rest[..end + 3])
}

fn parse_hidden_marker(body: &str) -> Result<RemoteFeedbackMarker, MarkerParseError> {
    let marker = extract_exact_marker_body(body)?;
    let fields = marker
        .strip_prefix(MARKER_NAMESPACE)
        .ok_or_else(|| marker_parse_error("wrong marker namespace"))?
        .trim();
    if fields.is_empty() {
        return Err(marker_parse_error("missing marker fields"));
    }
    let mut map = BTreeMap::new();
    for part in fields.split_whitespace() {
        let (key, value) = part
            .split_once('=')
            .ok_or_else(|| marker_parse_error(format!("malformed field {part}")))?;
        if key.is_empty() || value.is_empty() {
            return Err(marker_parse_error(format!("empty field {part}")));
        }
        if map.insert(key, value).is_some() {
            return Err(marker_parse_error(format!("duplicate field {key}")));
        }
    }
    let stable_marker_key = required_marker_field(&map, "marker_key")?.to_string();
    let source_head_sha = required_marker_field(&map, "source_head")?.to_string();
    let remediation_output_head = required_marker_field(&map, "remediation_output_head")?;
    let body_hash = required_marker_field(&map, "body")?.to_string();
    let run_id = required_marker_field(&map, "run_id")?.to_string();
    let action_kind = required_marker_field(&map, "action")?.to_string();
    Ok(RemoteFeedbackMarker {
        stable_marker_key,
        source_head_sha,
        remediation_output_head_sha: (remediation_output_head != "none")
            .then(|| remediation_output_head.to_string()),
        body_hash,
        action_kind,
        run_id,
        status: "completed".to_string(),
    })
}

fn extract_exact_marker_body(body: &str) -> Result<&str, MarkerParseError> {
    let trimmed = body.trim();
    if !trimmed.starts_with("<!--") || !trimmed.ends_with("-->") {
        return Err(marker_parse_error(
            "marker must be a single exact hidden HTML comment",
        ));
    }
    let inner = &trimmed[4..trimmed.len() - 3];
    if inner.contains("<!--") || inner.contains("-->") {
        return Err(marker_parse_error("nested marker comment delimiter"));
    }
    let marker = inner.trim();
    if !marker.starts_with(MARKER_NAMESPACE) {
        return Err(marker_parse_error("wrong marker namespace"));
    }
    Ok(marker)
}

fn required_marker_field<'a>(
    map: &'a BTreeMap<&str, &str>,
    field: &str,
) -> Result<&'a str, MarkerParseError> {
    map.get(field)
        .copied()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| marker_parse_error(format!("missing field {field}")))
}

fn marker_parse_error(diagnostic: impl Into<String>) -> MarkerParseError {
    MarkerParseError {
        diagnostic: diagnostic.into(),
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 2
fn configured_identities(params: &Value) -> BTreeSet<String> {
    let mut identities = [
        "coderabbitai",
        "coderabbitai[bot]",
        "coderabbit[bot]",
        "coderabbit",
    ]
    .into_iter()
    .map(ToString::to_string)
    .collect::<BTreeSet<_>>();
    if let Some(extra) = params
        .get("coderabbit_bot_identities")
        .and_then(Value::as_array)
    {
        for identity in extra.iter().filter_map(Value::as_str) {
            identities.insert(identity.to_ascii_lowercase());
        }
    }
    // When include_all_reviewers is set, add the wildcard sentinel so any
    // reviewer's threads flow through the same deterministic mechanism.
    // @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
    // @requirement:REQ-PRFU-024
    if params
        .get("include_all_reviewers")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        identities.insert(ALL_REVIEWERS_SENTINEL.to_string());
    }
    identities
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-024
/// @pseudocode lines 10
fn is_coderabbit(author: &str, identities: &BTreeSet<String>) -> bool {
    if author.is_empty() {
        return false;
    }
    identities.contains(ALL_REVIEWERS_SENTINEL) || identities.contains(&author.to_ascii_lowercase())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009
/// @pseudocode lines 13-14
fn item_set_hash(items: &[FeedbackItem]) -> String {
    let mut material = items
        .iter()
        .map(|item| {
            format!(
                "{}:{}:{}",
                item.stable_marker_key,
                item.body_hash,
                item.commit_sha.as_deref().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>();
    material.sort();
    stable_hash(&material.join("|"))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009
/// @pseudocode lines 13-14
fn stable_hash(text: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv64:{hash:016x}")
}
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 14-19
fn readiness_stability_hash(observation: &FeedbackObservation) -> String {
    let material = json!({
        "feedback_item_set_hash": item_set_hash(&observation.items),
        "ready_signal": observation.ready_signal,
        "in_progress_signal": observation.in_progress_signal,
        "readiness_signals": observation.readiness_signals,
        "stale_signals": observation.stale_signals,
        "items": observation.items.iter().map(|item| json!({
            "stable_marker_key": item.stable_marker_key,
            "body_hash": item.body_hash,
            "commit_sha": item.commit_sha,
            "resolved": item.resolved,
            "outdated": item.outdated,
            "resolution_state_available": item.resolution_state_available,
            "updated_at": item.updated_at,
            "source": item.source,
        })).collect::<Vec<_>>(),
        "remote_markers": observation.remote_markers.iter().map(remote_marker_json).collect::<Vec<_>>(),
        "malformed_remote_markers": observation.malformed_remote_markers,
        "matched_identities": observation.matched_identities.iter().cloned().collect::<Vec<_>>(),
    });
    stable_hash(&serde_json::to_string(&material).unwrap_or_default())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 1
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
    if let Some(value) = find_current_pr_artifact(context, store, &requested)? {
        return binding_from_value(&value);
    }
    Ok(requested)
}

fn find_current_pr_artifact(
    context: &StepContext,
    store: &PrFollowupArtifactStore,
    requested: &PrFollowupBinding,
) -> Result<Option<Value>, EngineError> {
    let requested_path = store.canonical_path(requested, "pr");
    if requested_path.exists() {
        return read_json_file(&requested_path).map(Some);
    }

    let current_root = requested_path
        .ancestors()
        .nth(4)
        .ok_or_else(|| github_feedback_error("invalid pr-followup artifact path"))?;
    if !current_root.exists() {
        return Ok(None);
    }

    let expected_run_id = context.run_id();
    let mut matches = Vec::new();
    collect_pr_artifacts(current_root, expected_run_id, &mut matches)?;
    matches.sort_by(|left, right| left.0.cmp(&right.0));
    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(matches.remove(0).1)),
        _ => Err(github_feedback_error(format!(
            "multiple PR identity artifacts found for run {expected_run_id}; provide repository_owner, repository_name, and pr_number parameters"
        ))),
    }
}

fn collect_pr_artifacts(
    dir: &Path,
    expected_run_id: &str,
    matches: &mut Vec<(PathBuf, Value)>,
) -> Result<(), EngineError> {
    for entry in std::fs::read_dir(dir)
        .map_err(|err| github_feedback_error(format!("read pr artifact directory: {err}")))?
    {
        let entry = entry.map_err(|err| {
            github_feedback_error(format!("read pr artifact directory entry: {err}"))
        })?;
        let path = entry.path();
        if path.is_dir() {
            collect_pr_artifacts(&path, expected_run_id, matches)?;
        } else if path.file_name().and_then(|name| name.to_str()) == Some("pr.json") {
            let value = read_json_file(&path)?;
            if value.get("run_id").and_then(Value::as_str) == Some(expected_run_id)
                && binding_from_value(&value).is_ok()
            {
                matches.push((path, value));
            }
        }
    }
    Ok(())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 22
fn is_permission_or_schema_error(value: &Value) -> bool {
    value.get("errors").is_some()
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 6,15
/// @pseudocode lines 1
fn read_json_file(path: &std::path::Path) -> Result<Value, EngineError> {
    let content = std::fs::read_to_string(path)
        .map_err(|err| github_feedback_error(format!("read {}: {err}", path.display())))?;
    serde_json::from_str(&content)
        .map_err(|err| github_feedback_error(format!("parse {}: {err}", path.display())))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 1
fn binding_from_value(value: &Value) -> Result<PrFollowupBinding, EngineError> {
    Ok(PrFollowupBinding {
        schema_version: u32::try_from(require_u64(value, "schema_version")?)
            .map_err(|err| github_feedback_error(format!("schema_version out of range: {err}")))?,
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

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 1
fn require_string(value: &Value, field: &str) -> Result<String, EngineError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| github_feedback_error(format!("missing string field {field}")))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 1
fn require_u64(value: &Value, field: &str) -> Result<u64, EngineError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| github_feedback_error(format!("missing integer field {field}")))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 1
fn string_param(context: &StepContext, params: &Value, key: &str, default: &str) -> String {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(|template| interpolate_string(template, context))
        .filter(|value| !value.contains('{') && !value.contains('}') && !value.is_empty())
        .or_else(|| context.get(key).cloned())
        .filter(|value| !value.contains('{') && !value.contains('}') && !value.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn artifact_root(context: &StepContext, params: &Value) -> Result<PathBuf, EngineError> {
    let raw = params
        .get("artifact_root")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| github_feedback_error("missing artifact_root"))?;
    let interpolated = interpolate_string(raw, context);
    if interpolated.contains('{') || interpolated.contains('}') {
        return Err(github_feedback_error(format!(
            "artifact_root contains unresolved template token: {interpolated}"
        )));
    }
    let path = PathBuf::from(interpolated);
    Ok(if path.is_absolute() {
        path
    } else {
        context.work_dir().join(path)
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 3
fn u64_param(params: &Value, key: &str, default: u64) -> u64 {
    params.get(key).and_then(Value::as_u64).unwrap_or(default)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 7
fn string_field(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 7
fn opt_string(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 1,20
fn current_step_id(context: &StepContext, fallback: &str) -> String {
    context
        .get("current_step_id")
        .cloned()
        .unwrap_or_else(|| fallback.to_string())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 22
fn github_feedback_error(message: impl Into<String>) -> EngineError {
    EngineError::StepExecutionError {
        step_id: "github_coderabbit_feedback".to_string(),
        message: message.into(),
    }
}
