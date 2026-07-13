//! Stale remediation retry-scope classification and field comparison.

use serde_json::{json, Value};

use super::{RemediationRetryScope, StaleScopeClassification};

pub(super) fn classify_stale_remediation_scope(
    result: &Value,
    expected: &RemediationRetryScope,
    advance_validation: bool,
) -> Option<StaleScopeClassification> {
    if is_stale_scope_validation_result(result) {
        return None;
    }
    let observed = observed_retry_scope(result);
    let errors = stale_scope_errors(result, &observed, expected);
    (!errors.is_empty()).then(|| StaleScopeClassification {
        expected_scope: expected.clone(),
        observed_scope: observed,
        errors,
        stale_artifact_retry_index: expected.stale_artifact_retry_index
            + u64::from(advance_validation),
        max_stale_artifact_retries: expected.max_stale_artifact_retries,
    })
}

fn is_stale_scope_validation_result(result: &Value) -> bool {
    matches!(
        result.get("validation_state").and_then(Value::as_str),
        Some("stale_artifact" | "stale_artifact_cap_exhausted")
    )
}

fn observed_retry_scope(result: &Value) -> Value {
    result
        .get("retry_scope")
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}))
}

fn stale_scope_errors(
    result: &Value,
    observed: &Value,
    expected: &RemediationRetryScope,
) -> Vec<String> {
    let mut errors = Vec::new();
    compare_required_scope_fields(observed, expected, &mut errors);
    compare_optional_scope_fields(result, observed, expected, &mut errors);
    errors
}

fn compare_required_scope_fields(
    observed: &Value,
    expected: &RemediationRetryScope,
    errors: &mut Vec<String>,
) {
    compare_scope_string(observed, "run_id", &expected.run_id, errors);
    compare_scope_string(
        observed,
        "repository_owner",
        &expected.repository_owner,
        errors,
    );
    compare_scope_string(
        observed,
        "repository_name",
        &expected.repository_name,
        errors,
    );
    compare_scope_u64(observed, "pr_number", expected.pr_number, errors);
    compare_scope_string(observed, "input_head_sha", &expected.input_head_sha, errors);
    compare_scope_u64(
        observed,
        "plan_artifact_sequence",
        expected.plan_artifact_sequence,
        errors,
    );
}

fn compare_optional_scope_fields(
    result: &Value,
    observed: &Value,
    expected: &RemediationRetryScope,
    errors: &mut Vec<String>,
) {
    if let Some(output_head_sha) = expected.output_head_sha.as_deref() {
        compare_scope_string_if_present(
            result,
            observed,
            "output_head_sha",
            output_head_sha,
            errors,
        );
    }
    compare_scope_u64_if_present(
        result,
        observed,
        "remediation_attempt_index",
        expected.remediation_attempt_index,
        errors,
    );
    compare_scope_u64_if_present(
        result,
        observed,
        "max_remediation_attempts",
        expected.max_remediation_attempts,
        errors,
    );
    compare_scope_u64_if_present(
        result,
        observed,
        "validation_retry_index",
        expected.validation_retry_index,
        errors,
    );
    compare_scope_u64_if_present(
        result,
        observed,
        "max_validation_retries",
        expected.max_validation_retries,
        errors,
    );
    compare_scope_string_if_present(result, observed, "scope_kind", &expected.scope_kind, errors);
}

fn compare_scope_string(observed: &Value, field: &str, expected: &str, errors: &mut Vec<String>) {
    let value = observed.get(field).and_then(Value::as_str);
    if value != Some(expected) {
        errors.push(format!(
            "retry_scope.{field} mismatch: expected {expected}, got {}",
            value.unwrap_or("<missing>")
        ));
    }
}

fn compare_scope_string_if_present(
    result: &Value,
    observed: &Value,
    field: &str,
    expected: &str,
    errors: &mut Vec<String>,
) {
    let value = observed
        .get(field)
        .or_else(|| result.get(field))
        .and_then(Value::as_str);
    if value.is_some() && value != Some(expected) {
        errors.push(format!(
            "retry_scope.{field} mismatch: expected {expected}, got {}",
            value.unwrap_or("<missing>")
        ));
    }
}

fn compare_scope_u64(observed: &Value, field: &str, expected: u64, errors: &mut Vec<String>) {
    let value = observed.get(field).and_then(Value::as_u64);
    if value != Some(expected) {
        errors.push(format!(
            "retry_scope.{field} mismatch: expected {expected}, got {}",
            value.map_or_else(|| "<missing>".to_string(), |value| value.to_string())
        ));
    }
}

fn compare_scope_u64_if_present(
    result: &Value,
    observed: &Value,
    field: &str,
    expected: u64,
    errors: &mut Vec<String>,
) {
    let value = observed
        .get(field)
        .or_else(|| result.get(field))
        .and_then(Value::as_u64);
    if value.is_some() && value != Some(expected) {
        errors.push(format!(
            "retry_scope.{field} mismatch: expected {expected}, got {}",
            value.map_or_else(|| "<missing>".to_string(), |value| value.to_string())
        ));
    }
}

pub(super) fn retry_scope_u64(result: &Value, field: &str) -> Option<u64> {
    result
        .get("retry_scope")
        .and_then(|scope| scope.get(field))
        .and_then(Value::as_u64)
        .or_else(|| result.get(field).and_then(Value::as_u64))
}

pub(super) fn retry_scope_json(scope: &RemediationRetryScope) -> Value {
    json!({
        "run_id": scope.run_id,
        "repository_owner": scope.repository_owner,
        "repository_name": scope.repository_name,
        "pr_number": scope.pr_number,
        "input_head_sha": scope.input_head_sha,
        "output_head_sha": scope.output_head_sha,
        "plan_artifact_sequence": scope.plan_artifact_sequence,
        "remediation_attempt_index": scope.remediation_attempt_index,
        "max_remediation_attempts": scope.max_remediation_attempts,
        "validation_retry_index": scope.validation_retry_index,
        "max_validation_retries": scope.max_validation_retries,
        "stale_artifact_retry_index": scope.stale_artifact_retry_index,
        "max_stale_artifact_retries": scope.max_stale_artifact_retries,
        "scope_kind": scope.scope_kind,
    })
}
