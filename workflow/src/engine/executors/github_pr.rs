//! GitHub PR identity, check watching, and CI failure executor contracts.
//! @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
//! @requirement:REQ-PRFU-020
//! @pseudocode lines 1-33

use std::fs;
use std::path::PathBuf;
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
    CollectionState, OverallState, PrCheckStatus, PrFollowupBinding, PR_FOLLOWUP_SCHEMA_VERSION,
};
use crate::engine::runner::EngineError;
use crate::engine::transition::StepOutcome;

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
        collect_ci_failures(context, params, &self.runner, &self.clock)
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

/// Collected CI failure and uncertainty artifact fragments.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 4-18
#[derive(Clone, Debug, Default)]
struct CiFailureCollection {
    failures: Vec<Value>,
    pending_or_unknown: Vec<Value>,
    log_artifacts: Vec<Value>,
}

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
    let owner = required_param_or_context(context, params, "repository_owner", "owner");
    let repo = required_param_or_context(context, params, "repository_name", "repo");
    let pr_selector = required_param_or_context(context, params, "pr_number", "42");
    let store = PrFollowupArtifactStore::new(artifact_root);
    let argv = vec![
        "gh".to_string(),
        "pr".to_string(),
        "view".to_string(),
        pr_selector,
        "--repo".to_string(),
        format!("{owner}/{repo}"),
        "--json".to_string(),
        "number,url,headRefName,headRefOid,baseRefName,baseRefOid,state,isDraft,id".to_string(),
    ];
    let output = runner.run_github_command(&argv)?;
    let value = serde_json::from_str::<Value>(&output)
        .map_err(|err| github_pr_error(format!("parse pr identity json: {err}")))?;
    let identity = parse_pr_identity(&value, context.run_id(), &owner, &repo)?;
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
    context.set("repository_owner", &identity.binding.repository_owner);
    context.set("repository_name", &identity.binding.repository_name);
    context.set("pr_number", &identity.binding.pr_number.to_string());
    context.set("head_ref", &identity.binding.head_ref);
    context.set("head_sha", &identity.binding.head_sha);
    context.set("base_ref", &identity.binding.base_ref);
    if let Some(base_sha) = identity.binding.base_sha.as_deref() {
        context.set("base_sha", base_sha);
    }
    Ok(StepOutcome::Success)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
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
    let config = config_from_value(params);
    let counters = counters_from_value(&pr_value);
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
    let owner = required_param_or_context(context, params, "repository_owner", "owner");
    let repo = required_param_or_context(context, params, "repository_name", "repo");
    let pr_selector = required_param_or_context(context, params, "pr_number", "42");
    let binding = PrFollowupBinding {
        schema_version: PR_FOLLOWUP_SCHEMA_VERSION,
        run_id: context.run_id().to_string(),
        repository_owner: owner.clone(),
        repository_name: repo.clone(),
        pr_number: pr_selector.parse().unwrap_or(42),
        head_ref: required_param_or_context(context, params, "head_ref", "feature"),
        head_sha: required_param_or_context(context, params, "head_sha", "head-a"),
        base_ref: required_param_or_context(context, params, "base_ref", "main"),
        base_sha: Some(required_param_or_context(
            context, params, "base_sha", "base-a",
        )),
    };
    let canonical_pr_path = store.canonical_path(&binding, "pr");
    match read_json_without_store_validation(&canonical_pr_path) {
        Ok(value) => Ok(value),
        Err(_) => {
            let argv = vec![
                "gh".to_string(),
                "pr".to_string(),
                "view".to_string(),
                pr_selector,
                "--repo".to_string(),
                format!("{owner}/{repo}"),
                "--json".to_string(),
                "number,url,headRefName,headRefOid,baseRefName,baseRefOid,state,isDraft,id"
                    .to_string(),
            ];
            let output = runner.run_github_command(&argv)?;
            let value = serde_json::from_str::<Value>(&output)
                .map_err(|err| github_pr_error(format!("parse pr identity json: {err}")))?;
            let identity = parse_pr_identity(&value, context.run_id(), &owner, &repo)?;
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
                "capture_pr_identity",
                1,
                &payload,
                None,
                clock,
            )?;
            read_json_without_store_validation(&store.canonical_path(&identity.binding, "pr"))
        }
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
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
                run_id: row.pointer("/check_suite/id").and_then(Value::as_u64),
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

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007
/// @pseudocode lines 1-21
// Pre-existing CI failure collection flow; split in a dedicated refactor stage.
#[allow(clippy::too_many_lines)]
fn collect_ci_failures(
    context: &StepContext,
    params: &Value,
    runner: &dyn GithubPrCommandRunner,
    clock: &dyn ClockSleeper,
) -> Result<StepOutcome, EngineError> {
    let artifact_root = artifact_root(context, params)?;
    let store = PrFollowupArtifactStore::new(artifact_root);
    let pr_value = read_or_capture_pr_identity(context, params, runner, clock, &store)?;
    let binding = binding_from_artifact(&pr_value)?;
    let check_status = match store.read_current_json(&binding, "pr-check-status") {
        Ok(value) => value,
        Err(_) => {
            watch_pr_checks(context, params, runner, clock)?;
            store.read_current_json(&binding, "pr-check-status")?
        }
    };
    let source_sequence = require_u64(&check_status, "artifact_sequence")?;
    let typed_check_status: PrCheckStatus =
        serde_json::from_value(check_status.clone()).map_err(|err| {
            EngineError::StepExecutionError {
                step_id: "collect_ci_failures".to_string(),
                message: format!("deserialize pr-check-status artifact: {err}"),
            }
        })?;
    let overall_state = typed_check_status.overall_state;
    let watcher_fatal_source = typed_check_status
        .fatal_source
        .clone()
        .unwrap_or(Value::Null);
    let mut collection = CiFailureCollection::default();

    for check in check_status
        .get("checks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match check_entry_bucket(check) {
            "failed" => collection.failures.push(ci_failure_json(
                &binding,
                check,
                source_sequence,
                runner,
                &store,
                clock,
                &mut collection.log_artifacts,
            )?),
            "pending" | "unknown" => collection.pending_or_unknown.push(pending_or_unknown_json(
                "current_head_check",
                check_entry_bucket(check),
                check,
                source_sequence,
            )),
            _ => {}
        }
    }
    for stale in check_status
        .get("stale_checks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        collection.pending_or_unknown.push(pending_or_unknown_json(
            "stale_check",
            "stale_only",
            stale,
            source_sequence,
        ));
    }
    let overall_is_fatal = matches!(overall_state, OverallState::Fatal);
    if !watcher_fatal_source.is_null() || overall_is_fatal {
        collection.pending_or_unknown.push(json!({
            "source": "watch_pr_checks",
            "reason": "watcher_fatal",
            "watcher_fatal_source": watcher_fatal_source,
            "source_check_status_artifact_sequence": source_sequence,
            "source_artifact_path": check_status.pointer("/history_metadata/canonical_path").cloned().unwrap_or(Value::Null),
            "safe_error_metadata": check_status.get("failure_details").cloned().unwrap_or_else(|| json!({}))
        }));
    }

    // Route from the typed `overall_state`. A `passed` artifact is never fatal,
    // even if a stale `fatal_source` slipped through, because the invariant
    // validator rejects that contradiction before we reach here.
    // @requirement:REQ-PRFU-007
    let collection_state = if overall_is_fatal || !watcher_fatal_source.is_null() {
        CollectionState::Fatal
    } else {
        CollectionState::Collected
    };
    let collection_is_fatal = matches!(collection_state, CollectionState::Fatal);
    let fatal_source = if collection_is_fatal {
        watcher_fatal_source.clone()
    } else if collection.pending_or_unknown.is_empty() {
        Value::Null
    } else {
        Value::String(overall_state.as_str().to_string())
    };
    let payload = json!({
        "collection_state": collection_state,
        "failures": collection.failures,
        "pending_or_unknown": collection.pending_or_unknown,
        "watcher_fatal_source": watcher_fatal_source,
        "fatal_source": fatal_source,
        "log_artifacts": collection.log_artifacts,
        "source_check_status_artifact_sequence": source_sequence,
        "source_check_status_artifact_path": check_status.pointer("/history_metadata/canonical_path").cloned().unwrap_or(Value::Null),
        "collected_at": clock.now_rfc3339()
    });
    let pending_count = payload
        .get("pending_or_unknown")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let failure = if collection_is_fatal || pending_count > 0 {
        Some((
            if collection_is_fatal {
                "fatal"
            } else {
                overall_state.as_str()
            },
            if collection_is_fatal {
                "watcher_fatal"
            } else {
                overall_state.as_str()
            },
            json!({
                "watcher_fatal_source": payload.get("watcher_fatal_source").cloned().unwrap_or(Value::Null),
                "pending_or_unknown_count": pending_count,
                "source_check_status_artifact_sequence": source_sequence
            }),
        ))
    } else {
        None
    };
    store.write_json_artifact(
        &binding,
        "ci-failures",
        "collect_ci_failures",
        step_order_index(params, 4),
        &payload,
        failure,
        clock,
    )?;

    if collection_is_fatal || pending_count > 0 {
        Ok(StepOutcome::Fatal)
    } else {
        Ok(StepOutcome::Success)
    }
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
    let actions_check = check.get("app_slug").and_then(Value::as_str) == Some("github-actions")
        || check
            .get("url")
            .and_then(Value::as_str)
            .is_some_and(|url| url.contains("/actions/"));
    if !actions_check {
        return Ok(LogCollectionResult {
            status: "not_applicable".to_string(),
            excerpt: String::new(),
            raw_log_path: None,
            excerpt_path: None,
            artifact: None,
            error: None,
        });
    }
    let Some(resolved_job_id) = resolve_job_id(binding, check, run_id, job_id, runner)? else {
        return Ok(LogCollectionResult {
            status: "unavailable".to_string(),
            excerpt: String::new(),
            raw_log_path: None,
            excerpt_path: None,
            artifact: None,
            error: Some(json!({ "class": "job_mapping_unavailable", "run_id": run_id })),
        });
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
            let excerpt = bounded_excerpt(&log_text);
            let raw_path = store
                .root()
                .join("pr-followup")
                .join("logs")
                .join(&binding.run_id)
                .join(format!("ci-job-{resolved_job_id}.log"));
            let excerpt_path = store
                .root()
                .join("pr-followup")
                .join("logs")
                .join(&binding.run_id)
                .join(format!("ci-job-{resolved_job_id}-excerpt.log"));
            write_bounded_log_artifact(&raw_path, &log_text)?;
            write_bounded_log_artifact(&excerpt_path, &excerpt)?;
            Ok(LogCollectionResult {
                status: "available".to_string(),
                excerpt,
                raw_log_path: Some(raw_path.display().to_string()),
                excerpt_path: Some(excerpt_path.display().to_string()),
                artifact: Some(json!({
                    "job_id": resolved_job_id,
                    "run_id": run_id,
                    "log_status": "available",
                    "raw_log_path": raw_path.display().to_string(),
                    "log_excerpt_path": excerpt_path.display().to_string(),
                    "collected_at": clock.now_rfc3339()
                })),
                error: None,
            })
        }
        Err(err) => Ok(LogCollectionResult {
            status: "fetch_failed".to_string(),
            excerpt: String::new(),
            raw_log_path: None,
            excerpt_path: None,
            artifact: None,
            error: Some(
                json!({ "class": "fetch_failed", "message": err.to_string(), "job_id": resolved_job_id }),
            ),
        }),
    }
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
    for page in 1..=2 {
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
        if jobs_len == 0 || (jobs_seen >= total_count && jobs_len >= 100) {
            break;
        }
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
