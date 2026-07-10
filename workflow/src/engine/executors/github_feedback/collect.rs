use super::*;
use crate::engine::executor::{StepContext, StepExecutor};
use crate::engine::executors::github_pr::GithubPrCommandRunner;
use crate::engine::executors::pr_followup_artifacts::{ClockSleeper, PrFollowupArtifactStore};
use crate::engine::executors::pr_followup_types::PrFollowupBinding;
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::thread;
use std::time::Duration;

pub(super) const DEFAULT_MAX_OBSERVATIONS: u64 = 6;
pub(super) const DEFAULT_REQUIRED_STABLE_OBSERVATIONS: u64 = 2;
pub(super) const DEFAULT_OBSERVATION_INTERVAL_SECONDS: u64 = 300;
pub(super) const MARKER_NAMESPACE: &str = "luther-pr-followup";
pub(super) const MARKER_ARTIFACT_FAMILY: &str = "pr-feedback-marker-report";
pub(super) const PENDING_MARKER_ACTIONS_FAMILY: &str = "pending-feedback-marker-actions";
/// Sentinel identity that, when present in the configured identity set, makes
/// the feedback collector accept review threads from any reviewer (not only
/// CodeRabbit). Selected via the `include_all_reviewers` step param.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-024
pub(super) const ALL_REVIEWERS_SENTINEL: &str = "*";
/// Real GraphQL mutation used to resolve a PR review thread.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-016
pub(super) const RESOLVE_REVIEW_THREAD_MUTATION: &str = "mutation resolveReviewThread($threadId:ID!){ resolveReviewThread(input:{threadId:$threadId}) { thread { id isResolved } } }";
/// Real GraphQL mutation used to post a reply on a PR review thread.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-016
pub(super) const ADD_REVIEW_THREAD_REPLY_MUTATION: &str = "mutation addPullRequestReviewThreadReply($threadId:ID!,$body:String!){ addPullRequestReviewThreadReply(input:{pullRequestReviewThreadId:$threadId,body:$body}) { comment { id databaseId url } } }";

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
pub(super) struct FeedbackItem {
    pub(super) item_id: String,
    pub(super) stable_marker_key: String,
    pub(super) thread_id: Option<String>,
    pub(super) comment_id: Option<String>,
    pub(super) comment_database_id: Option<i64>,
    pub(super) review_id: Option<String>,
    pub(super) author_login: String,
    pub(super) author_kind: Option<String>,
    pub(super) path: Option<String>,
    pub(super) line: Option<u64>,
    pub(super) side: Option<String>,
    pub(super) body: String,
    pub(super) body_hash: String,
    pub(super) url: Option<String>,
    pub(super) created_at: Option<String>,
    pub(super) updated_at: Option<String>,
    pub(super) resolved: bool,

    pub(super) outdated: bool,
    pub(super) resolution_state_available: bool,
    pub(super) source: String,
    pub(super) raw_node_id: Option<String>,
    pub(super) commit_sha: Option<String>,
    pub(super) stale: bool,
}

/// Single collector observation after querying documented GitHub feedback surfaces.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017
/// @pseudocode lines 4-19
#[derive(Clone, Debug, Default)]
pub(super) struct FeedbackObservation {
    pub(super) items: Vec<FeedbackItem>,
    pub(super) stale_items: Vec<Value>,
    pub(super) noise: Vec<Value>,
    pub(super) remote_markers: Vec<RemoteFeedbackMarker>,
    pub(super) malformed_remote_markers: Vec<Value>,
    pub(super) remote_marker_audit: Vec<Value>,
    pub(super) ready_signal: bool,
    pub(super) in_progress_signal: bool,
    pub(super) readiness_signals: Vec<Value>,
    pub(super) stale_signals: Vec<Value>,
    pub(super) matched_identities: BTreeSet<String>,
    pub(super) fatal: Option<Value>,
}

/// One pending feedback marker action consumed by `github_feedback_marker`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-026
/// @pseudocode lines 41-49
#[derive(Clone, Debug)]
pub(super) struct PendingMarkerAction {
    pub(super) value: Value,
    pub(super) action_kind: String,
    pub(super) item_id: String,
    pub(super) stable_marker_key: String,
    pub(super) source_head_sha: String,
    pub(super) remediation_output_head: String,
    pub(super) body_hash: String,
    pub(super) reason: String,
    pub(super) response_text: Option<String>,
    pub(super) thread_id: Option<String>,
    pub(super) comment_database_id: Option<i64>,
    pub(super) resolution_required: bool,
    pub(super) status: String,
}

/// Result classification for one marker action attempt.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-026
/// @pseudocode lines 43-49
#[derive(Clone, Debug)]
pub(super) struct MarkerActionOutcome {
    pub(super) action: PendingMarkerAction,
    pub(super) status: String,
    pub(super) comment_key: String,
    pub(super) resolution_key: String,
    pub(super) posted_comment: Option<Value>,
    pub(super) resolved_thread: Option<Value>,
    pub(super) skipped: Vec<Value>,
    pub(super) partial: Option<Value>,
    pub(super) retryable: Option<Value>,
    pub(super) failed: Option<Value>,
    pub(super) audit: Value,
    pub(super) updated_action: Value,
}

pub(super) struct MarkerActionProcessor<'a> {
    pub(super) binding: &'a PrFollowupBinding,
    pub(super) store: &'a PrFollowupArtifactStore,
    pub(super) step_id: &'a str,
    pub(super) step_order: u64,
    pub(super) runner: &'a dyn GithubPrCommandRunner,
    pub(super) clock: &'a dyn ClockSleeper,
    pub(super) local_completed: &'a BTreeSet<String>,
    pub(super) remote_completed: &'a BTreeSet<String>,
    pub(super) params: &'a Value,
}

#[derive(Default)]
pub(super) struct MarkerActionMutationState {
    pub(super) posted_comment: Option<Value>,
    pub(super) resolved_thread: Option<Value>,
    pub(super) skipped: Vec<Value>,
    pub(super) partial: Option<Value>,
    pub(super) retryable: Option<Value>,
    pub(super) failed: Option<Value>,
    pub(super) resolve_attempted: bool,
    pub(super) resolve_succeeded: bool,
    pub(super) resolve_error: Option<String>,
    pub(super) final_thread_resolved_state: Option<bool>,
}

pub(super) struct FeedbackCollectionConfig<'a> {
    pub(super) store: &'a PrFollowupArtifactStore,
    pub(super) binding: &'a PrFollowupBinding,
    pub(super) step_id: String,
    pub(super) step_order: u64,
    pub(super) max_observations: u64,
    pub(super) required_stable: u64,
    pub(super) interval_seconds: u64,
    pub(super) identities: BTreeSet<String>,
    pub(super) clock: &'a dyn ClockSleeper,
}

#[derive(Default)]
pub(super) struct FeedbackReadinessState {
    pub(super) observations: Vec<Value>,
    pub(super) previous_ready_hash: Option<String>,
    pub(super) stable_count: u64,
    pub(super) final_observation: FeedbackObservation,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017,REQ-PRFU-024,REQ-PRFU-034
/// @pseudocode lines 1-29
pub(super) fn collect_coderabbit_feedback(
    context: &mut StepContext,
    params: &Value,
    runner: &dyn GithubPrCommandRunner,
    clock: &dyn ClockSleeper,
) -> Result<StepOutcome, EngineError> {
    let artifact_root = artifact_root(context, params)?;
    let store = PrFollowupArtifactStore::new(artifact_root);
    let binding = read_or_build_binding(context, params, &store)?;
    let config = FeedbackCollectionConfig {
        store: &store,
        binding: &binding,
        step_id: current_step_id(context, "collect_coderabbit_feedback"),
        step_order: u64_param(params, "step_order_index", 5),
        max_observations: u64_param(params, "max_observations", DEFAULT_MAX_OBSERVATIONS),
        required_stable: u64_param(
            params,
            "required_stable_observations",
            DEFAULT_REQUIRED_STABLE_OBSERVATIONS,
        ),
        interval_seconds: u64_param(
            params,
            "coderabbit_readiness_observation_interval_seconds",
            DEFAULT_OBSERVATION_INTERVAL_SECONDS,
        ),
        identities: configured_identities(params),
        clock,
    };
    observe_until_coderabbit_ready(&config, runner)
}

pub(super) fn observe_until_coderabbit_ready(
    config: &FeedbackCollectionConfig<'_>,
    runner: &dyn GithubPrCommandRunner,
) -> Result<StepOutcome, EngineError> {
    let mut state = FeedbackReadinessState::default();
    for attempt in 1..=config.max_observations {
        let observed_at = config.clock.now_rfc3339();
        let observation = observe_coderabbit_feedback(runner, config.binding, &config.identities)?;
        if let Some(fatal) = observation.fatal.clone() {
            write_fatal_feedback_artifacts(
                config,
                &state,
                &observation,
                attempt,
                observed_at,
                fatal,
            )?;
            return Ok(StepOutcome::Fatal);
        }
        if record_feedback_observation(config, &mut state, observation, attempt, observed_at)? {
            return Ok(StepOutcome::Success);
        }
        if attempt < config.max_observations {
            config
                .clock
                .sleep(Duration::from_secs(config.interval_seconds));
        }
    }
    write_feedback_timeout_artifacts(config, &state)?;
    Ok(StepOutcome::Wait)
}

pub(super) fn write_fatal_feedback_artifacts(
    config: &FeedbackCollectionConfig<'_>,
    state: &FeedbackReadinessState,
    observation: &FeedbackObservation,
    attempt: u64,
    observed_at: String,
    fatal: Value,
) -> Result<(), EngineError> {
    let payload = feedback_payload(
        config.binding,
        "fatal",
        state.stable_count,
        config.required_stable,
        config.max_observations,
        config.interval_seconds,
        &state.observations,
        observation,
        &config.identities,
        attempt,
        observed_at,
        "fatal_api_or_schema",
    );
    write_feedback_artifacts(
        config.store,
        config.binding,
        &config.step_id,
        config.step_order,
        &payload,
        config.clock,
        Some(("fatal", "api_auth_schema_or_ambiguity", fatal)),
    )
}

pub(super) fn record_feedback_observation(
    config: &FeedbackCollectionConfig<'_>,
    state: &mut FeedbackReadinessState,
    observation: FeedbackObservation,
    attempt: u64,
    observed_at: String,
) -> Result<bool, EngineError> {
    let item_set_hash = item_set_hash(&observation.items);
    let readiness_hash = readiness_stability_hash(&observation);
    let materially_ready = observation.ready_signal && !observation.in_progress_signal;
    state.stable_count = next_stable_count(
        state.previous_ready_hash.as_deref(),
        &readiness_hash,
        materially_ready,
        state.stable_count,
    );
    state.previous_ready_hash = materially_ready.then_some(readiness_hash);
    let outcome_reason =
        feedback_outcome_reason(state.stable_count, config.required_stable, &observation);
    let observation_json = observation_json(
        &observation,
        &item_set_hash,
        attempt,
        config.max_observations,
        &observed_at,
        outcome_reason,
    );
    state.observations.push(observation_json);
    state.final_observation = observation;
    write_ready_feedback_artifacts(config, state, attempt, observed_at, outcome_reason)?;
    Ok(state.stable_count >= config.required_stable)
}

pub(super) fn next_stable_count(
    previous_ready_hash: Option<&str>,
    readiness_hash: &str,
    materially_ready: bool,
    stable_count: u64,
) -> u64 {
    if materially_ready && previous_ready_hash == Some(readiness_hash) {
        stable_count + 1
    } else if materially_ready {
        1
    } else {
        0
    }
}

pub(super) fn feedback_outcome_reason(
    stable_count: u64,
    required_stable: u64,
    observation: &FeedbackObservation,
) -> &'static str {
    if stable_count >= required_stable {
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
    }
}

pub(super) fn write_ready_feedback_artifacts(
    config: &FeedbackCollectionConfig<'_>,
    state: &FeedbackReadinessState,
    attempt: u64,
    observed_at: String,
    outcome_reason: &str,
) -> Result<(), EngineError> {
    let readiness_state = if state.stable_count >= config.required_stable {
        "ready"
    } else {
        "not_ready"
    };
    let payload = feedback_payload(
        config.binding,
        readiness_state,
        state.stable_count,
        config.required_stable,
        config.max_observations,
        config.interval_seconds,
        &state.observations,
        &state.final_observation,
        &config.identities,
        attempt,
        observed_at,
        outcome_reason,
    );
    write_feedback_artifacts(
        config.store,
        config.binding,
        &config.step_id,
        config.step_order,
        &payload,
        config.clock,
        None,
    )
}

pub(super) fn write_feedback_timeout_artifacts(
    config: &FeedbackCollectionConfig<'_>,
    state: &FeedbackReadinessState,
) -> Result<(), EngineError> {
    let payload = feedback_payload(
        config.binding,
        "timeout",
        state.stable_count,
        config.required_stable,
        config.max_observations,
        config.interval_seconds,
        &state.observations,
        &state.final_observation,
        &config.identities,
        config.max_observations,
        config.clock.now_rfc3339(),
        "readiness_budget_exhausted",
    );
    write_feedback_artifacts(
        config.store,
        config.binding,
        &config.step_id,
        config.step_order,
        &payload,
        config.clock,
        Some((
            "timeout",
            "readiness_budget_exhausted",
            json!({ "max_observations": config.max_observations }),
        )),
    )
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017
/// @pseudocode lines 4-17
pub(super) fn observe_coderabbit_feedback(
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
        scan_rest_review_comment_marker(&rest_comment, identities, &mut observation);
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
pub(super) fn normalize_graphql_threads(
    value: &Value,
    binding: &PrFollowupBinding,
    identities: &BTreeSet<String>,
    observation: &mut FeedbackObservation,
) {
    for thread in graphql_thread_nodes(value) {
        normalize_graphql_thread(&thread, binding, identities, observation);
    }
}

pub(super) fn graphql_thread_nodes(value: &Value) -> Vec<Value> {
    value
        .pointer("/data/repository/pullRequest/reviewThreads/nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

pub(super) fn normalize_graphql_thread(
    thread: &Value,
    binding: &PrFollowupBinding,
    identities: &BTreeSet<String>,
    observation: &mut FeedbackObservation,
) {
    let resolved = thread
        .get("isResolved")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let outdated = thread
        .get("isOutdated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // Review-thread actions are anchored to the root comment; REST comment scans cover individual replies separately.
    let Some(comment) = thread
        .pointer("/comments/nodes")
        .and_then(Value::as_array)
        .and_then(|nodes| nodes.first())
    else {
        return;
    };
    normalize_graphql_thread_comment(
        thread,
        comment,
        GraphqlThreadState { resolved, outdated },
        binding,
        identities,
        observation,
    );
}

#[derive(Clone, Copy)]
pub(super) struct GraphqlThreadState {
    pub(super) resolved: bool,
    pub(super) outdated: bool,
}

pub(super) fn normalize_graphql_thread_comment(
    thread: &Value,
    comment: &Value,
    thread_state: GraphqlThreadState,
    binding: &PrFollowupBinding,
    identities: &BTreeSet<String>,
    observation: &mut FeedbackObservation,
) {
    let author = comment
        .pointer("/author/login")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !is_coderabbit(author, identities) {
        observation
            .noise
            .push(json!({ "source": "graphql_review_thread", "author_login": author }));
        return;
    }
    observation.matched_identities.insert(author.to_string());
    let body = string_field(comment, "body");
    record_remote_marker_parse(
        &body,
        "graphql_review_thread",
        comment.get("id").cloned().unwrap_or(Value::Null),
        observation,
    );
    if thread_state.resolved {
        return;
    }
    push_current_or_stale(
        graphql_feedback_item(thread, comment, thread_state, binding, author),
        observation,
    );
}

pub(super) fn graphql_feedback_item(
    thread: &Value,
    comment: &Value,
    thread_state: GraphqlThreadState,
    binding: &PrFollowupBinding,
    author: &str,
) -> FeedbackItem {
    let body = string_field(comment, "body");
    let commit_sha = comment
        .pointer("/commit/oid")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let stale = commit_sha.as_deref() != Some(binding.head_sha.as_str()) || thread_state.outdated;
    FeedbackItem {
        item_id: format!(
            "graphql:{}:{}",
            string_field(thread, "id"),
            string_field(comment, "id")
        ),
        stable_marker_key: format!("thread:{}", string_field(thread, "id")),
        thread_id: opt_string(thread, "id"),
        comment_id: opt_string(comment, "id"),
        comment_database_id: comment.get("databaseId").and_then(Value::as_i64),
        review_id: None,
        author_login: author.to_string(),
        author_kind: comment
            .pointer("/author/__typename")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        path: opt_string(comment, "path").or_else(|| opt_string(thread, "path")),
        line: comment
            .get("line")
            .and_then(Value::as_u64)
            .or_else(|| thread.get("line").and_then(Value::as_u64)),
        side: None,
        body_hash: stable_hash(&body),
        body,
        url: opt_string(comment, "url"),
        created_at: opt_string(comment, "createdAt"),
        updated_at: opt_string(comment, "updatedAt"),
        resolved: thread_state.resolved,
        outdated: thread_state.outdated,
        resolution_state_available: true,
        source: "graphql_review_thread".to_string(),
        raw_node_id: opt_string(comment, "id"),
        commit_sha,
        stale,
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017
/// @pseudocode lines 5,8,12-14
pub(super) fn scan_rest_review_comment_marker(
    comment: &Value,
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
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017
/// @pseudocode lines 5,9-13
pub(super) fn normalize_issue_comment(
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
    // Issue comments are bot status/summary surfaces, not review-thread feedback;
    // keep them limited to explicitly configured bot identities.
    if !is_explicit_reviewer_identity(author, identities) {
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
        author_kind: comment
            .pointer("/user/type")
            .and_then(Value::as_str)
            .map(ToString::to_string),
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
pub(super) fn normalize_readiness_signal(
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
    if (status == "completed" && matches!(conclusion.as_str(), "success" | "neutral" | "skipped"))
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
