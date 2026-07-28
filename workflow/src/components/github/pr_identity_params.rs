use serde_json::Value;

use crate::engine::executor::{interpolate_string, StepContext};

pub(super) fn explicit_pr_number(
    context: &StepContext,
    params: &Value,
) -> Result<Option<u64>, String> {
    let resolved = match params.get("pr_number") {
        Some(Value::String(template)) => {
            let value = interpolate_string(template, context);
            if value.is_empty() || has_unresolved_template(&value) {
                context.get("pr_number").cloned()
            } else {
                Some(value)
            }
        }
        Some(Value::Number(number)) => number
            .as_u64()
            .map(|value| value.to_string())
            .ok_or_else(|| format!("invalid pr_number {number}: expected a positive integer"))
            .map(Some)?,
        Some(value) => {
            return Err(format!(
                "invalid pr_number {value}: expected an integer or numeric string"
            ));
        }
        None => context.get("pr_number").cloned(),
    };

    let Some(raw) = resolved.filter(|value| !value.is_empty() && !has_unresolved_template(value))
    else {
        return Ok(None);
    };
    let number = raw
        .parse::<u64>()
        .map_err(|error| format!("invalid pr_number '{raw}': {error}"))?;
    if number == 0 {
        return Err("invalid pr_number '0': expected a positive integer".to_string());
    }
    Ok(Some(number))
}

pub(super) fn string_identity_is_explicit(
    context: &StepContext,
    params: &Value,
    key: &str,
) -> bool {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(|value| interpolate_string(value, context))
        .filter(|value| !value.is_empty() && !has_unresolved_template(value))
        .or_else(|| context.get(key).cloned())
        .is_some_and(|value| !value.is_empty() && !has_unresolved_template(&value))
}

fn has_unresolved_template(value: &str) -> bool {
    value.contains('{') || value.contains('}')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn context() -> StepContext {
        StepContext::new(PathBuf::from("."), "run".to_string())
    }

    #[test]
    fn explicit_pr_number_accepts_integer_and_numeric_string() {
        let context = context();
        assert_eq!(
            explicit_pr_number(&context, &json!({"pr_number": 42})),
            Ok(Some(42))
        );
        assert_eq!(
            explicit_pr_number(&context, &json!({"pr_number": "42"})),
            Ok(Some(42))
        );
    }

    #[test]
    fn explicit_pr_number_rejects_invalid_json_types_and_values() {
        let context = context();
        for params in [
            json!({"pr_number": 0}),
            json!({"pr_number": -1}),
            json!({"pr_number": 1.5}),
            json!({"pr_number": "not-a-number"}),
            json!({"pr_number": true}),
        ] {
            assert!(explicit_pr_number(&context, &params).is_err(), "{params}");
        }
    }
}
