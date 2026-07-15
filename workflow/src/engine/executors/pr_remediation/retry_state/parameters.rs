//! Strict remediation retry parameter decoding.

use super::*;

pub(super) fn parameter(params: &Value, name: &str, default: u64) -> Result<u64, EngineError> {
    if !params.is_object() {
        return Err(EngineError::InvalidState(
            "remediation retry parameters must be a JSON object".to_string(),
        ));
    }
    match params.get(name) {
        None => Ok(default),
        Some(value) => value.as_u64().ok_or_else(|| {
            EngineError::InvalidState(format!(
                "remediation retry parameter {name} must be an unsigned integer"
            ))
        }),
    }
}
