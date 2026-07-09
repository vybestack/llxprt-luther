use std::fs;
use std::io::ErrorKind;

use serde_json::{json, Value};

use crate::engine::executors::pr_followup_artifacts::{ArtifactWriter, PrFollowupArtifactStore};
use crate::engine::executors::pr_followup_types::PrFollowupBinding;
use crate::engine::runner::EngineError;

use super::{artifact_sequence, source_artifact, stable_source_id, string_field};

#[derive(Clone, Debug)]
pub(super) struct PlanInputs {
    pub(super) ci_failures: Value,
    pub(super) coderabbit_feedback: Value,
    pub(super) evaluations: Value,
    pub(super) post_pr_test_result: Option<Value>,
}

pub(super) fn read_plan_inputs(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
) -> Result<PlanInputs, EngineError> {
    Ok(PlanInputs {
        ci_failures: store.read_current_json(binding, "ci-failures")?,
        coderabbit_feedback: store.read_current_json(binding, "coderabbit-feedback")?,
        evaluations: store.read_current_json(binding, "feedback-evaluations")?,
        post_pr_test_result: read_optional_current_json(store, binding, "post-pr-test-result")?,
    })
}

pub(super) fn read_optional_current_json(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    artifact_family: &str,
) -> Result<Option<Value>, EngineError> {
    let path = store.canonical_path(binding, artifact_family);
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(optional_artifact_error(format!(
                "read {}: {err}",
                path.display()
            )))
        }
    };
    let value: Value = serde_json::from_str(&content)
        .map_err(|err| optional_artifact_error(format!("parse {}: {err}", path.display())))?;
    store.validate_artifact_value(binding, artifact_family, &value)?;
    store.validate_artifact_invariants(artifact_family, &value)?;
    Ok(Some(value))
}

fn optional_artifact_error(message: impl Into<String>) -> EngineError {
    EngineError::StepExecutionError {
        step_id: "pr_followup_artifact_store".to_string(),
        message: message.into(),
    }
}

pub(super) fn remediation_plan_source_artifacts(pr: &Value, inputs: &PlanInputs) -> Vec<Value> {
    let mut source_artifacts = vec![
        source_artifact(pr, "pr"),
        source_artifact(&inputs.ci_failures, "ci-failures"),
        source_artifact(&inputs.coderabbit_feedback, "coderabbit-feedback"),
        source_artifact(&inputs.evaluations, "feedback-evaluations"),
    ];
    if let Some(post_pr_test_result) = inputs.post_pr_test_result.as_ref() {
        source_artifacts.push(source_artifact(post_pr_test_result, "post-pr-test-result"));
    }
    source_artifacts
}

pub(super) fn append_pending_ci_judgment(
    ci_failures: &Value,
    binding: &PrFollowupBinding,
    pending_or_unknown: &mut Vec<Value>,
    needs_user_judgment: &mut Vec<Value>,
) {
    if let Some(pending) = ci_failures
        .get("pending_or_unknown")
        .and_then(Value::as_array)
    {
        for entry in pending {
            pending_or_unknown.push(entry.clone());
            needs_user_judgment.push(json!({
                "source_type": "ci_pending_or_unknown",
                "source_id": stable_source_id(entry, "pending_or_unknown"),
                "reason": "pending_or_unknown_check_state",
                "recommended_action": "human_review_required",
                "input_head_sha": binding.head_sha,
                "source_artifact_sequence": artifact_sequence(ci_failures),
                "evidence": entry
            }));
        }
    }
}

pub(super) fn append_post_pr_test_failures(
    post_pr_test_result: &Value,
    binding: &PrFollowupBinding,
    must_fix: &mut Vec<Value>,
) {
    if post_pr_test_result
        .get("test_state")
        .and_then(Value::as_str)
        != Some("failed")
    {
        return;
    }
    for command in post_pr_test_result
        .get("commands")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|command| {
            matches!(
                command.get("status").and_then(Value::as_str),
                Some("failed") | Some("error")
            )
        })
    {
        must_fix.push(post_pr_test_must_fix_item(
            command,
            binding,
            post_pr_test_result,
        ));
    }
}

fn post_pr_test_must_fix_item(
    command: &Value,
    binding: &PrFollowupBinding,
    post_pr_test_result: &Value,
) -> Value {
    let command_id = string_field(command, "command_id", "unknown-command");
    json!({
        "source_type": "post_pr_test_failure",
        "source_id": format!("post-pr-test-{command_id}"),
        "stable_marker_key": Value::Null,
        "body_hash": Value::Null,
        "reason": command.get("failure_classification").and_then(Value::as_str).unwrap_or("post_pr_test_command_failed"),
        "recommended_action": "fix_post_pr_local_verification_failure",
        "input_head_sha": binding.head_sha,
        "source_artifact_sequence": artifact_sequence(post_pr_test_result),
        "evidence": command
    })
}

pub(super) fn remediation_plan_covers_current_post_pr_test_result(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
    plan: &Value,
) -> Result<bool, EngineError> {
    let Some(test_result) = read_optional_current_json(store, binding, "post-pr-test-result")?
    else {
        return Ok(true);
    };
    if test_result.get("test_state").and_then(Value::as_str) != Some("failed") {
        return Ok(true);
    }
    let test_sequence = artifact_sequence(&test_result);
    Ok(plan
        .get("source_artifacts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|source| {
            source.get("artifact_family").and_then(Value::as_str) == Some("post-pr-test-result")
                && source.get("artifact_sequence") == Some(&test_sequence)
        }))
}
