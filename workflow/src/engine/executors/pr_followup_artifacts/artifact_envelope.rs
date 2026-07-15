//! Artifact envelope construction, decoding, and invariant validation.

use super::*;

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002,REQ-PRFU-004
/// @pseudocode lines 5-7
pub(super) fn artifact_write_record(
    value: &Value,
    canonical_path: PathBuf,
) -> Result<ArtifactWriteRecord, EngineError> {
    let sequence = ArtifactSequenceMetadata {
        artifact_sequence: require_u64(value, "artifact_sequence")?,
        write_sequence: require_u64(value, "write_sequence")?,
        producer_step_id: require_string(value, "producer_step_id")?,
    };
    if sequence.artifact_sequence == 0 || sequence.write_sequence == 0 {
        return Err(artifact_error(
            "replayed artifact sequences must be positive",
        ));
    }
    let history_path = value
        .pointer("/history_metadata/history_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| artifact_error("replayed artifact is missing history path"))?;
    Ok(ArtifactWriteRecord {
        sequence,
        canonical_path,
        history_path,
        failure_sequence: value.get("failure_sequence").and_then(Value::as_u64),
    })
}

pub(super) fn binding_from_value(value: &Value) -> Result<PrFollowupBinding, EngineError> {
    Ok(PrFollowupBinding {
        schema_version: u32::try_from(require_u64(value, "schema_version")?)
            .map_err(|err| artifact_error(format!("schema_version out of range: {err}")))?,
        run_id: require_string(value, "run_id")?,
        repository_owner: require_string(value, "repository_owner")?,
        repository_name: require_string(value, "repository_name")?,
        pr_number: require_u64(value, "pr_number")?,
        head_ref: require_string(value, "head_ref")?,
        head_sha: require_string(value, "head_sha")?,
        base_ref: require_string(value, "base_ref")?,
        base_sha: value
            .get("base_sha")
            .map(|base_sha| {
                if base_sha.is_null() {
                    Ok(None)
                } else {
                    base_sha
                        .as_str()
                        .map(|value| Some(value.to_string()))
                        .ok_or_else(|| artifact_error("base_sha must be string or null"))
                }
            })
            .transpose()?
            .flatten(),
    })
}

/// Dispatches per-family typed invariant validation for the artifact families
/// that participate in workflow routing decisions. Unknown families pass
/// through (only generic envelope/binding checks apply to them).
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-007
pub(super) fn validate_family_invariants(
    artifact_family: &str,
    value: &Value,
) -> Result<(), EngineError> {
    match artifact_family {
        "pr-check-status" => {
            let typed: PrCheckStatus = serde_json::from_value(value.clone()).map_err(|err| {
                artifact_error(format!("deserialize pr-check-status artifact: {err}"))
            })?;
            typed.validate_invariants().map_err(artifact_error)
        }
        "ci-failures" => {
            let typed: CiFailures = serde_json::from_value(value.clone()).map_err(|err| {
                artifact_error(format!("deserialize ci-failures artifact: {err}"))
            })?;
            typed.validate_invariants().map_err(artifact_error)
        }
        "post-pr-failure-terminal" => terminal_validation::validate_terminal_artifact(value),
        _ => Ok(()),
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
pub(super) fn artifact_family_from_value(value: &Value) -> Option<String> {
    value
        .get("history_metadata")?
        .get("artifact_family")?
        .as_str()
        .map(ToString::to_string)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002,REQ-PRFU-004
/// @pseudocode lines 5-7
pub(super) fn insert_binding_fields(object: &mut Map<String, Value>, binding: &PrFollowupBinding) {
    object.insert(
        "schema_version".to_string(),
        Value::from(binding.schema_version),
    );
    object.insert("run_id".to_string(), Value::from(binding.run_id.clone()));
    object.insert(
        "repository_owner".to_string(),
        Value::from(binding.repository_owner.clone()),
    );
    object.insert(
        "repository_name".to_string(),
        Value::from(binding.repository_name.clone()),
    );
    object.insert("pr_number".to_string(), Value::from(binding.pr_number));
    object.insert(
        "head_ref".to_string(),
        Value::from(binding.head_ref.clone()),
    );
    object.insert(
        "head_sha".to_string(),
        Value::from(binding.head_sha.clone()),
    );
    object.insert(
        "base_ref".to_string(),
        Value::from(binding.base_ref.clone()),
    );
    object.insert(
        "base_sha".to_string(),
        binding
            .base_sha
            .as_ref()
            .map_or(Value::Null, |base_sha| Value::from(base_sha.clone())),
    );
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
pub(super) fn require_u64(value: &Value, field: &str) -> Result<u64, EngineError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| artifact_error(format!("missing or invalid integer field {field}")))
}

/// @pseudocode lines 5-7
pub(super) fn require_string(value: &Value, field: &str) -> Result<String, EngineError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| artifact_error(format!("missing or invalid string field {field}")))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
pub(super) fn require_string_from_object(
    object: &Map<String, Value>,
    field: &str,
) -> Result<String, EngineError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| artifact_error(format!("missing or invalid string field {field}")))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
pub(super) fn require_bool_from_object(
    object: &Map<String, Value>,
    field: &str,
) -> Result<bool, EngineError> {
    object
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| artifact_error(format!("missing or invalid bool field {field}")))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
pub(super) fn artifact_error(message: impl Into<String>) -> EngineError {
    EngineError::StepExecutionError {
        step_id: "pr_followup_artifact_store".to_string(),

        message: message.into(),
    }
}

pub(super) fn checked_next_sequence(current: u64, sequence_name: &str) -> Result<u64, EngineError> {
    current.checked_add(1).ok_or_else(|| {
        artifact_error(format!(
            "cannot allocate {sequence_name}: sequence space is exhausted"
        ))
    })
}
