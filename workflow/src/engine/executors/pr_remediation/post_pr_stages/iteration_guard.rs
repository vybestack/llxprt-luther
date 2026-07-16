//! Durable post-PR iteration guard execution and history validation.

use super::*;

/// Post-PR iteration guard executor for `post_pr_iteration_guard`.
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-020
/// @pseudocode lines 8-15
#[derive(Debug, Default)]
pub struct PostPrIterationGuardExecutor;

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-020
/// @pseudocode lines 8-15
impl StepExecutor for PostPrIterationGuardExecutor {
    fn execute(
        &self,
        context: &mut StepContext,
        params: &serde_json::Value,
    ) -> Result<StepOutcome, EngineError> {
        let artifact_root = artifact_root(context, params)?;
        let store = PrFollowupArtifactStore::new(artifact_root);
        let binding = binding_for_context(context, params, &store, &SystemClockSleeper)?;
        if !scope_review_gate(context, &binding)? {
            return Ok(StepOutcome::Fatal);
        }
        let max_iterations = u64_param(params, "max_post_pr_remediation_iterations", 3);
        let previous = latest_guard_for_current_run(&store, &binding)?;
        let predecessor_artifact_sequence = previous
            .as_ref()
            .and_then(|guard| guard.get("artifact_sequence"))
            .and_then(Value::as_u64);
        let (iteration_index, previous_head_sha, reason) = match previous.as_ref() {
            None => (0, Value::Null, "initial_entry"),
            Some(guard)
                if guard.get("head_sha").and_then(Value::as_str)
                    == Some(binding.head_sha.as_str()) =>
            {
                (
                    guard
                        .get("iteration_index")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    Value::String(binding.head_sha.clone()),
                    "same_head_reentry",
                )
            }
            Some(guard) => (
                guard
                    .get("iteration_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    + 1,
                guard.get("head_sha").cloned().unwrap_or(Value::Null),
                "head_sha_changed_after_remediation_push",
            ),
        };
        let exceeded = iteration_index > max_iterations;
        let payload = json!({
            "guard_state": if exceeded { "max_iterations_exceeded" } else { "proceed" },
            "iteration_index": iteration_index,
            "max_post_pr_remediation_iterations": max_iterations,
            "previous_head_sha": previous_head_sha,
            "predecessor_artifact_sequence": predecessor_artifact_sequence,
            "reason": if exceeded { "max_iterations_exceeded" } else { reason },
            "ignored_stale_artifacts": [],
            "updated_at": SystemClockSleeper.now_rfc3339()
        });
        let failure = exceeded.then(|| {
            (
                "fatal",
                "max_iterations_exceeded",
                json!({
                    "iteration_index": iteration_index,
                    "max_post_pr_remediation_iterations": max_iterations
                }),
            )
        });
        store.write_json_artifact(JsonArtifactWriteRequest::new(
            ArtifactWriteContext::new(
                &binding,
                "post-pr-iteration-guard",
                "post_pr_iteration_guard",
                u64_param(params, "step_order_index", 2),
                &SystemClockSleeper,
            ),
            &payload,
            failure,
        ))?;
        if exceeded {
            Ok(StepOutcome::Fatal)
        } else {
            Ok(StepOutcome::Success)
        }
    }
}

fn scope_review_gate(
    context: &mut StepContext,
    binding: &PrFollowupBinding,
) -> Result<bool, EngineError> {
    use crate::engine::executors::scope_control::{
        charter_path, filter_changed_tests, pre_launch_review_gate, read_json, scope_control_dir,
        CanonicalTaskCharter, PreLaunchReviewRequest,
    };

    let Some(policy_json) = context.get("scope_control_policy") else {
        return Ok(true);
    };
    let policy: crate::workflow::schema::ScopeControlConfig = serde_json::from_str(policy_json)
        .map_err(|err| EngineError::InvalidState(format!("invalid scope-control policy: {err}")))?;
    if !policy.enabled {
        return Ok(true);
    }
    let Some(artifact_dir) = context
        .get("artifact_dir")
        .or_else(|| context.get("artifact_root"))
        .map(PathBuf::from)
    else {
        return Err(EngineError::InvalidState(
            "scope review gate requires artifact_dir or artifact_root".into(),
        ));
    };
    let charter_file = charter_path(&scope_control_dir(&artifact_dir, context.run_id()));
    let charter: CanonicalTaskCharter = read_json(&charter_file).map_err(|err| {
        EngineError::InvalidState(format!("failed to read scope-control charter: {err}"))
    })?;
    let changed_files = review_changed_files(context.work_dir(), &charter.merge_base)?;
    let changed_tests = filter_changed_tests(&changed_files);
    let now = SystemClockSleeper.now_rfc3339();
    let request = PreLaunchReviewRequest {
        run_id: context.run_id(),
        head_sha: &binding.head_sha,
        merge_base: &charter.merge_base,
        changed_files: &changed_files,
        changed_tests: &changed_tests,
        charter_digest: &charter.digest,
        caps: &charter.review_caps,
        now_rfc3339: &now,
    };
    let outcome = pre_launch_review_gate(&artifact_dir, &request).map_err(|err| {
        EngineError::InvalidState(format!("scope-control review gate failed: {err}"))
    })?;
    context.set("task_charter_merge_base", &charter.merge_base);
    context.set("scope_review_from", &charter.merge_base);
    context.set("scope_review_to", &binding.head_sha);
    let changed_tests_json = serde_json::to_string(&changed_tests)
        .map_err(|err| EngineError::InvalidState(format!("serialize changed tests: {err}")))?;
    context.set("scope_review_changed_tests", &changed_tests_json);
    Ok(outcome.permits_review())
}

fn review_changed_files(work_dir: &Path, merge_base: &str) -> Result<Vec<String>, EngineError> {
    let output = std::process::Command::new("git")
        .args(["diff", "--name-only", "-z", merge_base, "HEAD"])
        .current_dir(work_dir)
        .output()
        .map_err(|err| EngineError::InvalidState(format!("failed to invoke git diff: {err}")))?;
    if !output.status.success() {
        return Err(EngineError::InvalidState(format!(
            "failed to collect review range: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let mut paths: Vec<String> = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn latest_guard_for_current_run(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
) -> Result<Option<Value>, EngineError> {
    let mut candidates =
        store.read_pr_identity_history_candidates(binding, "post-pr-iteration-guard")?;

    candidates.sort_by_key(|candidate| {
        candidate
            .value
            .as_ref()
            .and_then(|value| value.get("artifact_sequence"))
            .and_then(Value::as_u64)
            .unwrap_or_default()
    });
    let mut latest = None;
    let mut sequences = BTreeSet::new();
    for candidate in candidates {
        let value = candidate.value.ok_or_else(|| {
            EngineError::InvalidState(format!(
                "malformed post-PR iteration guard history {}: {}",
                candidate.path.display(),
                candidate
                    .validation_error
                    .as_deref()
                    .unwrap_or("JSON payload unavailable")
            ))
        })?;
        if let Some(error) = candidate.validation_error {
            return Err(EngineError::InvalidState(format!(
                "invalid post-PR iteration guard history {}: {error}",
                candidate.path.display()
            )));
        }
        let sequence = value
            .get("artifact_sequence")
            .and_then(Value::as_u64)
            .filter(|sequence| *sequence > 0)
            .ok_or_else(|| {
                EngineError::InvalidState(format!(
                    "post-PR iteration guard history {} has no positive artifact_sequence",
                    candidate.path.display()
                ))
            })?;
        if !sequences.insert(sequence) {
            return Err(EngineError::InvalidState(format!(
                "duplicate post-PR iteration guard history sequence {sequence}"
            )));
        }
        validate_guard_snapshot(&value, &candidate.path)?;
        let expected_predecessor = latest
            .as_ref()
            .and_then(|previous: &Value| previous.get("artifact_sequence"))
            .and_then(Value::as_u64);
        let actual_predecessor = value
            .get("predecessor_artifact_sequence")
            .and_then(Value::as_u64);
        if expected_predecessor.is_some() && actual_predecessor != expected_predecessor {
            return Err(EngineError::InvalidState(format!(
                "broken post-PR iteration guard history chain at {}: expected predecessor {:?}, found {:?}",
                candidate.path.display(),
                expected_predecessor,
                actual_predecessor
            )));
        }
        latest = Some(value);
    }
    Ok(latest)
}

fn validate_guard_snapshot(value: &Value, path: &Path) -> Result<(), EngineError> {
    let guard_state = value.get("guard_state").and_then(Value::as_str);
    if !matches!(guard_state, Some("proceed" | "max_iterations_exceeded"))
        || value
            .get("iteration_index")
            .and_then(Value::as_u64)
            .is_none()
        || value
            .get("max_post_pr_remediation_iterations")
            .and_then(Value::as_u64)
            .is_none()
        || value.get("head_sha").and_then(Value::as_str).is_none()
    {
        return Err(EngineError::InvalidState(format!(
            "malformed post-PR iteration guard snapshot {}",
            path.display()
        )));
    }
    Ok(())
}
