//! Immutable remediation-result receipt validation.

use sha2::{Digest, Sha256};

use super::*;

pub(super) fn validated_result_source_id(result: &Value) -> Option<&str> {
    if result.get("producer_step_id").and_then(Value::as_str) != Some("validate_remediation_result")
    {
        return None;
    }
    result
        .get("agent_result_source_identity")
        .and_then(Value::as_str)
        .or_else(|| result.get("validation_source_id").and_then(Value::as_str))
}

pub(super) fn validated_result_launch_error(
    result: &Value,
    expected: Option<&ArtifactLaunchBinding<'_>>,
) -> Option<String> {
    let expected = expected?;
    let actual_transition = result
        .get("retry_launch_transition_id")
        .and_then(Value::as_str);
    let actual_ordinal = result.get("retry_launch_ordinal").and_then(Value::as_u64);
    if actual_transition == Some(expected.transition_id) && actual_ordinal == Some(expected.ordinal)
    {
        return None;
    }
    Some(format!(
        "validated remediation result belongs to launch transition {:?} ordinal {:?}, expected {} ordinal {}",
        actual_transition, actual_ordinal, expected.transition_id, expected.ordinal
    ))
}

pub(super) fn unique_receipt_for_source<'a>(
    histories: &'a [Value],
    source_identity: &str,
) -> Result<Option<&'a Value>, EngineError> {
    let matching = histories
        .iter()
        .filter(|receipt| {
            receipt
                .get("agent_result_source_identity")
                .and_then(Value::as_str)
                == Some(source_identity)
        })
        .collect::<Vec<_>>();
    match matching.as_slice() {
        [] => Ok(None),
        [receipt] => Ok(Some(*receipt)),
        _ => Err(artifact_error(
            "agent-result source has ambiguous immutable receipts",
        )),
    }
}

pub(super) fn immutable_source_identity(exact_payload: &[u8]) -> String {
    let digest = Sha256::digest(exact_payload);
    format!("sha256:{digest:x}")
}

pub(super) fn receipt_validation_source_id(receipt: &Value) -> Option<String> {
    receipt
        .get("agent_result_source_identity")?
        .as_str()
        .filter(|identity| identity.len() == 71 && identity.starts_with("sha256:"))
        .map(ToString::to_string)
}
