//! GitHub PR identity, check watching, and CI failure executor contracts.
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
//! @requirement:REQ-PRFU-020
//! @pseudocode lines 1-33

use std::process::Command;

use serde_json::{json, Value};

use crate::engine::executor::{interpolate_string, StepContext, StepExecutor};
use crate::engine::executors::pr_check_wait::{
    check_bucket as shared_check_bucket, classify_api_error, classify_pr_checks, config_from_value,
    counters_from_value, status_payload, PrCheckObservation, PrCheckWaitClassification,
};
use crate::engine::executors::pr_followup_artifacts::{
    ArtifactWriter, ClockSleeper, PrFollowupArtifactStore,
};
use crate::engine::executors::pr_followup_types::{
    OverallState, PrFollowupBinding, PR_FOLLOWUP_SCHEMA_VERSION,
};
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;

mod ci_failures;

/// Command-runner seam for GitHub PR/check API calls.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001,REQ-PRFU-004,REQ-PRFU-017
/// @pseudocode lines 3-4,17-18
pub trait GithubPrCommandRunner: Send + Sync {
    fn run_github_command(&self, argv: &[String]) -> Result<String, EngineError>;
}

/// Production argv-safe GitHub command runner.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-017
/// @pseudocode lines 3-4,17-18
#[derive(Debug, Default)]
pub struct SystemGithubPrCommandRunner;

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-017
/// @pseudocode lines 3-4,17-18
impl GithubPrCommandRunner for SystemGithubPrCommandRunner {
    fn run_github_command(&self, argv: &[String]) -> Result<String, EngineError> {
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| github_pr_error("github command argv must not be empty"))?;
        let output = Command::new(program)
            .args(args)
            .output()
            .map_err(|err| github_pr_error(format!("spawn github command: {err}")))?;
        if !output.status.success() {
            return Err(github_pr_error(format!(
                "github command failed with status {:?}: {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        String::from_utf8(output.stdout)
            .map_err(|err| github_pr_error(format!("github command stdout was not utf8: {err}")))
    }
}

/// PR identity capture executor for `github_pr_identity`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001,REQ-PRFU-002
/// @pseudocode lines 1-7
#[derive(Clone, Copy, Debug, Default)]
pub struct GithubPrIdentityExecutor;

/// Injectable PR identity capture executor for tests and alternate runners.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001,REQ-PRFU-002
/// @pseudocode lines 1-7
pub struct GithubPrIdentityExecutorWithRunner<R, C> {
    runner: R,
    clock: C,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001,REQ-PRFU-002
/// @pseudocode lines 1-7
impl<R, C> GithubPrIdentityExecutorWithRunner<R, C> {
    #[must_use]
    pub fn new(runner: R, clock: C) -> Self {
        Self { runner, clock }
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001,REQ-PRFU-002
/// @pseudocode lines 1-7
impl<R, C> StepExecutor for GithubPrIdentityExecutorWithRunner<R, C>
where
    R: GithubPrCommandRunner,
    C: ClockSleeper,
{
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        capture_pr_identity(context, params, &self.runner, &self.clock)
    }
}

/// PR check watcher executor for `github_pr_checks`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-004,REQ-PRFU-005,REQ-PRFU-006
/// @pseudocode lines 16-33
#[derive(Clone, Copy, Debug, Default)]
pub struct GithubPrChecksExecutor;

/// Injectable PR check watcher executor for tests and alternate runners.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-004,REQ-PRFU-005,REQ-PRFU-006
/// @pseudocode lines 16-33
pub struct GithubPrChecksExecutorWithRunner<R, C> {
    runner: R,
    clock: C,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-004,REQ-PRFU-005,REQ-PRFU-006
/// @pseudocode lines 16-33
impl<R, C> GithubPrChecksExecutorWithRunner<R, C> {
    #[must_use]
    pub fn new(runner: R, clock: C) -> Self {
        Self { runner, clock }
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-004,REQ-PRFU-005,REQ-PRFU-006
/// @pseudocode lines 16-33
impl<R, C> StepExecutor for GithubPrChecksExecutorWithRunner<R, C>
where
    R: GithubPrCommandRunner,
    C: ClockSleeper,
{
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        watch_pr_checks(context, params, &self.runner, &self.clock)
    }
}

/// CI failure collection executor for `github_check_failures`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 1-21
#[derive(Debug, Default)]
pub struct GithubCheckFailuresExecutor;

/// Injectable CI failure collection executor for tests and alternate runners.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 1-21
pub struct GithubCheckFailuresExecutorWithRunner<R, C> {
    runner: R,
    clock: C,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 1-21
impl<R, C> GithubCheckFailuresExecutorWithRunner<R, C> {
    #[must_use]
    pub fn new(runner: R, clock: C) -> Self {
        Self { runner, clock }
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 1-21
impl<R, C> StepExecutor for GithubCheckFailuresExecutorWithRunner<R, C>
where
    R: GithubPrCommandRunner,
    C: ClockSleeper,
{
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        ci_failures::collect_ci_failures(context, params, &self.runner, &self.clock)
    }
}

/// Normalized current-head PR identity from structured GitHub data.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001
/// @pseudocode lines 3-6
#[derive(Clone, Debug)]
struct CapturedPrIdentity {
    binding: PrFollowupBinding,
    pr_url: String,
    source_pr_node_id: Option<String>,
    source_head_repository_owner: Option<String>,
    source_head_repository_name: Option<String>,
}

/// Normalized check state for classification.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-004,REQ-PRFU-006
/// @pseudocode lines 18-23
#[derive(Clone, Debug, Eq, PartialEq)]
struct NormalizedCheck {
    check_id: String,
    name: String,
    status: Option<String>,
    conclusion: Option<String>,
    state: String,
    bucket: String,
    url: Option<String>,
    workflow_name: Option<String>,
    run_id: Option<u64>,
    job_id: Option<u64>,
    started_at: Option<String>,
    completed_at: Option<String>,
    head_sha: Option<String>,
    app_slug: Option<String>,
    source: String,
}

#[derive(Clone, Debug)]
struct PrViewTarget {
    owner: String,
    repo: String,
    selector: String,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001,REQ-PRFU-002
/// @pseudocode lines 1-7
fn capture_pr_identity(
    context: &mut StepContext,
    params: &Value,
    runner: &dyn GithubPrCommandRunner,
    clock: &dyn ClockSleeper,
) -> Result<StepOutcome, EngineError> {
    let artifact_root = artifact_root(context, params)?;
    let store = PrFollowupArtifactStore::new(artifact_root);
    let target = resolve_pr_view_target(context, params, &store)?;
    let identity = capture_pr_identity_via_gh(&target, context, params, runner, clock, &store)?;
    set_pr_identity_context(context, &identity.binding);
    Ok(StepOutcome::Success)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
fn capture_pr_identity_via_gh(
    target: &PrViewTarget,
    context: &StepContext,
    params: &Value,
    runner: &dyn GithubPrCommandRunner,
    clock: &dyn ClockSleeper,
    store: &PrFollowupArtifactStore,
) -> Result<CapturedPrIdentity, EngineError> {
    let argv = vec![
        "gh".to_string(),
        "pr".to_string(),
        "view".to_string(),
        target.selector.clone(),
        "--repo".to_string(),
        format!("{}/{}", target.owner, target.repo),
        "--json".to_string(),
        "number,url,headRefName,headRefOid,baseRefName,baseRefOid,state,isDraft,id".to_string(),
    ];
    let output = runner.run_github_command(&argv)?;
    let value = serde_json::from_str::<Value>(&output)
        .map_err(|err| github_pr_error(format!("parse pr identity json: {err}")))?;
    let identity = parse_pr_identity(&value, context.run_id(), &target.owner, &target.repo)?;
    let payload = json!({
        "pr_url": identity.pr_url,
        "capture_state": "captured",
        "captured_at": clock.now_rfc3339(),
        "source": "gh_pr_view",
        "source_pr_node_id": identity.source_pr_node_id,
        "source_head_repository_owner": identity.source_head_repository_owner,
        "source_head_repository_name": identity.source_head_repository_name
    });
    store.write_json_artifact(
        &identity.binding,
        "pr",
        current_step_id(context, "capture_pr_identity").as_str(),
        step_order_index(params, 1),
        &payload,
        None,
        clock,
    )?;
    Ok(identity)
}

/// @requirement:REQ-PRFU-004,REQ-PRFU-005,REQ-PRFU-006
/// @pseudocode lines 16-33
fn watch_pr_checks(
    context: &StepContext,
    params: &Value,
    runner: &dyn GithubPrCommandRunner,
    clock: &dyn ClockSleeper,
) -> Result<StepOutcome, EngineError> {
    let artifact_root = artifact_root(context, params)?;
    let store = PrFollowupArtifactStore::new(artifact_root);
    let pr_value = read_or_capture_pr_identity(context, params, runner, clock, &store)?;
    let binding = binding_from_artifact(&pr_value)?;
    let pr_url = pr_value
        .get("pr_url")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let config = config_from_value(params).map_err(github_pr_error)?;
    let counters = read_matching_check_status_counters(&store, &binding);
    let observed_at = clock.now_rfc3339();
    let classification = match query_checks(&binding, runner) {
        Ok(checks) => classify_pr_checks(
            &binding.head_sha,
            checks.into_iter().map(Into::into).collect(),
            &config,
            counters,
        ),
        Err(err) => classify_api_error(&config, counters, err.to_string()),
    };
    let status_artifact = CheckStatusArtifactWrite {
        store: &store,
        binding: &binding,
        pr_url: &pr_url,
        classification: &classification,
        config: &config,
        observed_at: &observed_at,
        clock,
        step_order_index: step_order_index(params, 3),
    };
    write_check_status_artifact(status_artifact)?;

    Ok(match classification.overall_state {
        OverallState::Passed => StepOutcome::Success,
        OverallState::Failed => StepOutcome::Fixable,
        OverallState::PendingTimeout => StepOutcome::Wait,
        OverallState::Unknown | OverallState::Fatal => StepOutcome::Fatal,
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001
/// @pseudocode lines 3-6
fn parse_pr_identity(
    value: &Value,
    run_id: &str,
    owner: &str,
    repo: &str,
) -> Result<CapturedPrIdentity, EngineError> {
    let number = require_u64(value, "number")?;
    let pr_url = require_string(value, "url")?;
    let head_ref = require_string(value, "headRefName")?;
    let head_sha = require_string(value, "headRefOid")?;
    let base_ref = require_string(value, "baseRefName")?;
    let base_sha = optional_string(value, "baseRefOid");
    let state = require_string(value, "state")?;
    let is_draft = value
        .get("isDraft")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if state != "OPEN" || is_draft {
        return Err(github_pr_error("PR identity is not an open non-draft PR"));
    }

    Ok(CapturedPrIdentity {
        binding: PrFollowupBinding {
            schema_version: PR_FOLLOWUP_SCHEMA_VERSION,
            run_id: run_id.to_string(),
            repository_owner: owner.to_string(),
            repository_name: repo.to_string(),
            pr_number: number,
            head_ref,
            head_sha,
            base_ref,
            base_sha,
        },
        pr_url,
        source_pr_node_id: optional_string(value, "id"),
        source_head_repository_owner: value
            .pointer("/headRepository/owner/login")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        source_head_repository_name: value
            .pointer("/headRepository/name")
            .and_then(Value::as_str)
            .map(ToString::to_string),
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001,REQ-PRFU-004
/// @pseudocode lines 16-18
fn read_or_capture_pr_identity(
    context: &StepContext,
    params: &Value,
    runner: &dyn GithubPrCommandRunner,
    clock: &dyn ClockSleeper,
    store: &PrFollowupArtifactStore,
) -> Result<Value, EngineError> {
    let target = resolve_pr_view_target(context, params, store)?;
    if let Ok(pr_number) = target.selector.parse::<u64>() {
        let binding = build_lookup_binding(
            context,
            params,
            target.owner.clone(),
            target.repo.clone(),
            pr_number,
        );
        if let Some(value) = store.find_current_pr_artifact_for_run(context.run_id(), &binding)? {
            return Ok(value);
        }
    }

    let identity = capture_pr_identity_via_gh(&target, context, params, runner, clock, store)?;
    read_json_without_store_validation(&store.canonical_path(&identity.binding, "pr"))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
fn resolve_pr_view_target(
    context: &StepContext,
    params: &Value,
    store: &PrFollowupArtifactStore,
) -> Result<PrViewTarget, EngineError> {
    let owner = param_or_context(context, params, "repository_owner");
    let repo = param_or_context(context, params, "repository_name");
    let selector = param_or_context(context, params, "pr_number");
    if let (Some(owner), Some(repo), Some(selector)) = (owner, repo, selector) {
        return Ok(PrViewTarget {
            owner,
            repo,
            selector,
        });
    }

    let fallback = fallback_pr_followup_binding(context, params);
    if let Some(value) = store.find_current_pr_artifact_for_run(context.run_id(), &fallback)? {
        let binding = binding_from_artifact(&value)?;
        return Ok(pr_view_target_from_binding(&binding));
    }

    Err(github_pr_error(
        "unable to resolve PR view target from params, context, or current PR artifacts",
    ))
}

fn pr_view_target_from_binding(binding: &PrFollowupBinding) -> PrViewTarget {
    PrViewTarget {
        owner: binding.repository_owner.clone(),
        repo: binding.repository_name.clone(),
        selector: binding.pr_number.to_string(),
    }
}

fn build_lookup_binding(
    context: &StepContext,
    params: &Value,
    owner: String,
    repo: String,
    pr_number: u64,
) -> PrFollowupBinding {
    PrFollowupBinding {
        schema_version: PR_FOLLOWUP_SCHEMA_VERSION,
        run_id: context.run_id().to_string(),
        repository_owner: owner,
        repository_name: repo,
        pr_number,
        head_ref: required_param_or_context(context, params, "head_ref", "feature"),
        head_sha: required_param_or_context(context, params, "head_sha", "head-a"),
        base_ref: required_param_or_context(context, params, "base_ref", "main"),
        base_sha: Some(required_param_or_context(
            context, params, "base_sha", "base-a",
        )),
    }
}

fn fallback_pr_followup_binding(context: &StepContext, params: &Value) -> PrFollowupBinding {
    let pr_number = param_or_context(context, params, "pr_number")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    build_lookup_binding(
        context,
        params,
        required_param_or_context(context, params, "repository_owner", "owner"),
        required_param_or_context(context, params, "repository_name", "repo"),
        pr_number,
    )
}

fn set_pr_identity_context(context: &mut StepContext, binding: &PrFollowupBinding) {
    context.set("repository_owner", &binding.repository_owner);
    context.set("repository_name", &binding.repository_name);
    context.set("pr_number", &binding.pr_number.to_string());
    context.set("head_ref", &binding.head_ref);
    context.set("head_sha", &binding.head_sha);
    context.set("base_ref", &binding.base_ref);
    if let Some(base_sha) = binding.base_sha.as_deref() {
        context.set("base_sha", base_sha);
    }
}

/// @requirement:REQ-PRFU-004,REQ-PRFU-006
/// @pseudocode lines 17-18
fn query_checks(
    binding: &PrFollowupBinding,
    runner: &dyn GithubPrCommandRunner,
) -> Result<Vec<NormalizedCheck>, EngineError> {
    let repo = format!("{}/{}", binding.repository_owner, binding.repository_name);
    let preferred = runner.run_github_command(&[
        "gh".to_string(),
        "pr".to_string(),
        "checks".to_string(),
        binding.pr_number.to_string(),
        "--repo".to_string(),
        repo.clone(),
        "--json".to_string(),
        "name,state,bucket,link,workflow,startedAt,completedAt".to_string(),
    ])?;
    let mut checks = normalize_gh_pr_checks(
        &serde_json::from_str::<Value>(&preferred)
            .map_err(|err| github_pr_error(format!("parse gh pr checks json: {err}")))?,
        &binding.head_sha,
    )?;
    let rest = runner.run_github_command(&[
        "gh".to_string(),
        "api".to_string(),
        format!(
            "repos/{}/{}/commits/{}/check-runs",
            binding.repository_owner, binding.repository_name, binding.head_sha
        ),
    ]);
    if let Ok(rest_output) = rest {
        checks.extend(normalize_rest_check_runs(
            &serde_json::from_str::<Value>(&rest_output)
                .map_err(|err| github_pr_error(format!("parse check-runs json: {err}")))?,
        )?);
    }
    Ok(checks)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-004,REQ-PRFU-006
/// @pseudocode lines 18-23
fn normalize_gh_pr_checks(
    value: &Value,
    head_sha: &str,
) -> Result<Vec<NormalizedCheck>, EngineError> {
    let rows = value
        .as_array()
        .ok_or_else(|| github_pr_error("gh pr checks response must be an array"))?;
    Ok(rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let name = row
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let state = row
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("UNKNOWN")
                .to_string();
            NormalizedCheck {
                check_id: format!("gh-pr-checks:{name}:{index}"),
                name,
                status: Some(state.clone()),
                conclusion: Some(state.to_lowercase()),
                state: state.clone(),
                bucket: row
                    .get("bucket")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                url: row
                    .get("link")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                workflow_name: row
                    .get("workflow")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                run_id: None,
                job_id: extract_job_id(row.get("link").and_then(Value::as_str)),
                started_at: row
                    .get("startedAt")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                completed_at: row
                    .get("completedAt")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                head_sha: Some(head_sha.to_string()),
                app_slug: None,
                source: "gh_pr_checks".to_string(),
            }
        })
        .collect())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-004,REQ-PRFU-006
/// @pseudocode lines 18-23
fn normalize_rest_check_runs(value: &Value) -> Result<Vec<NormalizedCheck>, EngineError> {
    let rows = value
        .get("check_runs")
        .and_then(Value::as_array)
        .ok_or_else(|| github_pr_error("check-runs response must include check_runs array"))?;
    Ok(rows
        .iter()
        .map(|row| {
            let id = row.get("id").and_then(Value::as_u64).unwrap_or_default();
            let name = row
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let status = row
                .get("status")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let conclusion = row
                .get("conclusion")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            NormalizedCheck {
                check_id: format!("check-run:{id}"),
                name,
                status: status.clone(),
                conclusion: conclusion.clone(),
                state: conclusion
                    .clone()
                    .or(status.clone())
                    .unwrap_or_else(|| "unknown".to_string()),
                bucket: conclusion
                    .clone()
                    .or(status.clone())
                    .unwrap_or_else(|| "unknown".to_string()),
                url: row
                    .get("html_url")
                    .or_else(|| row.get("details_url"))
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                workflow_name: row
                    .pointer("/check_suite/id")
                    .and_then(Value::as_u64)
                    .map(|id| format!("suite-{id}")),
                run_id: extract_run_id(
                    row.get("html_url")
                        .or_else(|| row.get("details_url"))
                        .and_then(Value::as_str),
                ),
                job_id: extract_job_id(row.get("details_url").and_then(Value::as_str)),
                started_at: row
                    .get("started_at")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                completed_at: row
                    .get("completed_at")
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
                source: "rest_check_runs".to_string(),
            }
        })
        .collect())
}

fn check_bucket(check: &NormalizedCheck) -> String {
    shared_check_bucket(
        check.status.as_deref(),
        check.conclusion.as_deref(),
        &check.bucket,
        &check.state,
    )
}

impl From<NormalizedCheck> for PrCheckObservation {
    fn from(check: NormalizedCheck) -> Self {
        let bucket = check_bucket(&check);
        Self {
            check_id: check.check_id,
            name: check.name,
            status: check.status,
            conclusion: check.conclusion,
            state: check.state,
            bucket,
            url: check.url,
            workflow_name: check.workflow_name,
            run_id: check.run_id,
            job_id: check.job_id,
            started_at: check.started_at,
            completed_at: check.completed_at,
            head_sha: check.head_sha,
            app_slug: check.app_slug,
            source: check.source,
        }
    }
}

struct CheckStatusArtifactWrite<'a> {
    store: &'a PrFollowupArtifactStore,
    binding: &'a PrFollowupBinding,
    pr_url: &'a str,
    classification: &'a PrCheckWaitClassification,
    config: &'a crate::engine::executors::pr_check_wait::PrCheckWaitConfig,
    observed_at: &'a str,
    clock: &'a dyn ClockSleeper,
    step_order_index: u64,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-002,REQ-PRFU-004
/// @pseudocode lines 25,31-32
// Pre-existing check status artifact shape; split in a dedicated refactor stage.
fn write_check_status_artifact(write: CheckStatusArtifactWrite<'_>) -> Result<(), EngineError> {
    let mut payload = status_payload(
        write.classification,
        write.config,
        &write.binding.head_sha,
        write.observed_at,
    );
    payload["pr_url"] = Value::String(write.pr_url.to_string());
    payload["poll_attempts"] = json!(write.classification.counters.poll_attempts);
    payload["max_attempts"] = json!(1);
    payload["poll_interval_seconds"] = json!(write.config.poll_interval_seconds);
    payload["max_duration_seconds"] = json!(write.config.max_wait_seconds);
    payload["fatal_source"] = if write.classification.overall_state == OverallState::Fatal {
        Value::String(write.classification.reason.clone())
    } else {
        Value::Null
    };
    let failure = match write.classification.overall_state {
        OverallState::Passed => None,
        other => Some((
            other.as_str(),
            other.as_str(),
            json!({ "terminal_counts": write.classification.terminal_counts }),
        )),
    };
    write.store.write_json_artifact(
        write.binding,
        "pr-check-status",
        "watch_pr_checks",
        write.step_order_index,
        &payload,
        failure,
        write.clock,
    )?;
    Ok(())
}

fn read_matching_check_status_counters(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
) -> crate::engine::executors::pr_check_wait::PrCheckWaitCounters {
    store
        .read_current_json(binding, "pr-check-status")
        .ok()
        .filter(|value| {
            value.get("head_sha").and_then(Value::as_str) == Some(binding.head_sha.as_str())
        })
        .map(|value| counters_from_value(&value))
        .unwrap_or_default()
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001,REQ-PRFU-004
/// @pseudocode lines 2,16
fn artifact_root(context: &StepContext, params: &Value) -> Result<std::path::PathBuf, EngineError> {
    let raw = params
        .get("artifact_root")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| github_pr_error("missing artifact_root"))?;
    let interpolated = interpolate_string(raw, context);
    if interpolated.contains('{') || interpolated.contains('}') {
        return Err(github_pr_error(format!(
            "artifact_root contains unresolved template token: {interpolated}"
        )));
    }
    let path = std::path::PathBuf::from(interpolated);
    let path = if path.is_absolute() {
        path
    } else {
        context.work_dir().join(path)
    };
    std::fs::create_dir_all(&path)
        .map_err(|err| github_pr_error(format!("create artifact_root: {err}")))?;
    path.canonicalize()
        .map_err(|err| github_pr_error(format!("canonicalize artifact_root: {err}")))
}

fn has_unresolved_template(value: &str) -> bool {
    value.contains('{') || value.contains('}')
}
fn param_or_context(context: &StepContext, params: &Value, name: &str) -> Option<String> {
    params
        .get(name)
        .and_then(Value::as_str)
        .map(|template| interpolate_string(template, context))
        .filter(|value| !value.is_empty() && !has_unresolved_template(value))
        .or_else(|| context.get(name).cloned())
        .filter(|value| !value.is_empty() && !has_unresolved_template(value))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001,REQ-PRFU-004
/// @pseudocode lines 1,16
fn required_param_or_context(
    context: &StepContext,
    params: &Value,
    name: &str,
    fallback: &str,
) -> String {
    params
        .get(name)
        .and_then(Value::as_str)
        .map(|template| interpolate_string(template, context))
        .filter(|value| !has_unresolved_template(value))
        .or_else(|| context.get(name).cloned())
        .filter(|value| !has_unresolved_template(value))
        .unwrap_or_else(|| fallback.to_string())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001,REQ-PRFU-004
/// @pseudocode lines 6,16
fn binding_from_artifact(value: &Value) -> Result<PrFollowupBinding, EngineError> {
    Ok(PrFollowupBinding {
        schema_version: u32::try_from(require_u64(value, "schema_version")?)
            .map_err(|err| github_pr_error(format!("schema_version out of range: {err}")))?,
        run_id: require_string(value, "run_id")?,
        repository_owner: require_string(value, "repository_owner")?,
        repository_name: require_string(value, "repository_name")?,
        pr_number: require_u64(value, "pr_number")?,
        head_ref: require_string(value, "head_ref")?,
        head_sha: require_string(value, "head_sha")?,
        base_ref: require_string(value, "base_ref")?,
        base_sha: optional_string(value, "base_sha"),
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001
/// @pseudocode lines 3-5
fn require_string(value: &Value, field: &str) -> Result<String, EngineError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| github_pr_error(format!("missing or invalid string field {field}")))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001
/// @pseudocode lines 3-5
fn optional_string(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001
/// @pseudocode lines 3-5
fn require_u64(value: &Value, field: &str) -> Result<u64, EngineError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| github_pr_error(format!("missing or invalid integer field {field}")))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001,REQ-PRFU-004
/// @pseudocode lines 6,16
fn read_json_without_store_validation(path: &std::path::Path) -> Result<Value, EngineError> {
    let content = std::fs::read_to_string(path)
        .map_err(|err| github_pr_error(format!("read {}: {err}", path.display())))?;
    serde_json::from_str(&content)
        .map_err(|err| github_pr_error(format!("parse {}: {err}", path.display())))
}

/// @requirement:REQ-PRFU-004
/// @pseudocode lines 18
fn extract_job_id(url: Option<&str>) -> Option<u64> {
    let url = url?;
    if !url.contains("/actions/runs/") || !url.contains("/job/") {
        return None;
    }
    url.rsplit('/').next()?.parse().ok()
}

fn extract_run_id(url: Option<&str>) -> Option<u64> {
    let url = url?;
    let marker = "/actions/runs/";
    let (_, after) = url.split_once(marker)?;
    after.split('/').next()?.parse().ok()
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001,REQ-PRFU-004
/// @pseudocode lines 1,16
fn current_step_id(context: &StepContext, fallback: &str) -> String {
    context
        .get("current_step_id")
        .cloned()
        .unwrap_or_else(|| fallback.to_string())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001,REQ-PRFU-004
/// @pseudocode lines 1,16
fn step_order_index(params: &Value, fallback: u64) -> u64 {
    params
        .get("step_order_index")
        .and_then(Value::as_u64)
        .unwrap_or(fallback)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001,REQ-PRFU-004
/// @pseudocode lines 5,24
fn github_pr_error(message: impl Into<String>) -> EngineError {
    EngineError::StepExecutionError {
        step_id: "github_pr_followup".to_string(),
        message: message.into(),
    }
}
