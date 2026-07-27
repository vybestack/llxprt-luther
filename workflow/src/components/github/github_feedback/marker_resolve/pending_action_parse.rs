use super::*;

struct PendingActionFields {
    action_id: String,
    action_kind: String,
    item_id: String,
    stable_marker_key: String,
    source_head_sha: String,
    remediation_output_head: String,
    body_hash: String,
    status: String,
    resolution_required: bool,
}

impl PendingActionFields {
    fn from_value(value: &Value) -> Result<Self, EngineError> {
        let action_id = required_pending_action_string(value, "action_id")?;
        let action_kind = required_pending_action_string(value, "action_kind")?;
        validate_action_kind(&action_kind, &action_id)?;
        let item_id = required_pending_action_string(value, "item_id")?;
        let body_hash = required_pending_action_string(value, "body_hash")?;
        if !valid_pending_body_hash_format(&body_hash) {
            return Err(invalid_pending_action(format!(
                "body_hash must use canonical fnv64, hash-, or legacy sha256 format for item {item_id}"
            )));
        }
        let status = required_pending_action_string(value, "status")?;
        validate_action_status(&status, &item_id)?;
        let resolution_required = required_resolution_flag(value, &item_id)?;
        validate_resolution_contract(&action_kind, resolution_required, &item_id)?;
        Ok(Self {
            action_id,
            action_kind,
            item_id,
            stable_marker_key: required_pending_action_string(value, "stable_marker_key")?,
            source_head_sha: required_pending_action_string(value, "source_head_sha")?,
            remediation_output_head: required_pending_action_string(
                value,
                "remediation_output_head",
            )?,
            body_hash,
            status,
            resolution_required,
        })
    }

    fn into_action(self, value: Value) -> PendingMarkerAction {
        PendingMarkerAction {
            action_id: self.action_id,
            action_kind: self.action_kind,
            item_id: self.item_id,
            stable_marker_key: self.stable_marker_key,
            source_head_sha: self.source_head_sha,
            remediation_output_head: self.remediation_output_head,
            body_hash: self.body_hash,
            reason: pending_action_reason(&value),
            response_text: pending_action_response_text(&value),
            thread_id: pending_action_thread_id(&value),
            comment_database_id: pending_action_comment_database_id(&value),
            status: self.status,
            resolution_required: self.resolution_required,
            value,
        }
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-026
/// @pseudocode lines 41-49
pub(crate) fn pending_marker_action_from_value(
    value: Value,
) -> Result<PendingMarkerAction, EngineError> {
    if !value.is_object() {
        return Err(invalid_pending_action("pending action must be an object"));
    }
    let fields = PendingActionFields::from_value(&value)?;
    validate_pending_action_optional_fields(&value, &fields.item_id)?;
    validate_output_routing(
        &value,
        &fields.item_id,
        &fields.action_kind,
        &fields.remediation_output_head,
    )?;
    Ok(fields.into_action(value))
}

fn validate_action_kind(action_kind: &str, action_id: &str) -> Result<(), EngineError> {
    if matches!(
        action_kind,
        "comment_fixed"
            | "comment_invalid"
            | "comment_out_of_scope"
            | "comment_needs_user_judgment"
            | "skip_needs_user_judgment"
    ) {
        return Ok(());
    }
    Err(invalid_pending_action(format!(
        "unsupported action_kind {action_kind} for action {action_id}"
    )))
}

fn validate_action_status(status: &str, item_id: &str) -> Result<(), EngineError> {
    if matches!(status, "pending" | "completed" | "failed" | "skipped") {
        return Ok(());
    }
    Err(invalid_pending_action(format!(
        "unsupported pending action status {status} for item {item_id}"
    )))
}

fn required_resolution_flag(value: &Value, item_id: &str) -> Result<bool, EngineError> {
    value
        .get("resolution_required")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            invalid_pending_action(format!(
                "resolution_required must be a boolean for item {item_id}"
            ))
        })
}

/// Producer contract: remediation results emit `comment_fixed` with thread
/// resolution required; discounted and human-judgment actions are comments or
/// policy skips that must leave the thread open.
fn validate_resolution_contract(
    action_kind: &str,
    resolution_required: bool,
    item_id: &str,
) -> Result<(), EngineError> {
    let expected = action_kind == "comment_fixed";
    if resolution_required == expected {
        return Ok(());
    }
    Err(invalid_pending_action(format!(
        "resolution_required must be {expected} for action_kind {action_kind} on item {item_id}"
    )))
}

fn validate_pending_action_optional_fields(
    value: &Value,
    item_id: &str,
) -> Result<(), EngineError> {
    validate_nullable_pending_action_string(value, "thread_id", item_id)?;
    validate_nullable_pending_action_positive_i64(value, "comment_database_id", item_id)?;
    validate_nullable_pending_action_string(value, "response_text", item_id)?;
    validate_nullable_pending_action_string(value, "remediation_output_head_sha", item_id)
}

fn validate_output_routing(
    value: &Value,
    item_id: &str,
    action_kind: &str,
    remediation_output_head: &str,
) -> Result<(), EngineError> {
    let output_head_sha = value
        .get("remediation_output_head_sha")
        .and_then(Value::as_str);
    let valid = match remediation_output_head {
        NO_REMEDIATION_OUTPUT_HEAD => {
            // A fixed action is produced only from a successful remediation
            // result, which always binds it to that result's output head.
            output_head_sha.is_none() && action_kind != "comment_fixed"
        }
        output_head => output_head_sha == Some(output_head),
    };
    if valid {
        Ok(())
    } else {
        let expected_output_head_sha = match remediation_output_head {
            NO_REMEDIATION_OUTPUT_HEAD => "absent for non-fixed actions".to_string(),
            output_head => format!("{output_head:?}"),
        };
        Err(invalid_pending_action(format!(
            "inconsistent remediation output routing for item {item_id}: action_kind={action_kind:?}, remediation_output_head={remediation_output_head:?}, expected remediation_output_head_sha={expected_output_head_sha}, actual remediation_output_head_sha={output_head_sha:?}"
        )))
    }
}

fn required_pending_action_string(value: &Value, field: &str) -> Result<String, EngineError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| invalid_pending_action(format!("{field} must be a non-empty string")))
}

fn validate_nullable_pending_action_string(
    value: &Value,
    field: &str,
    item_id: &str,
) -> Result<(), EngineError> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(text)) if !text.trim().is_empty() => Ok(()),
        _ => Err(invalid_pending_action(format!(
            "{field} must be absent, null, or a non-empty string for item {item_id}"
        ))),
    }
}

fn validate_nullable_pending_action_positive_i64(
    value: &Value,
    field: &str,
    item_id: &str,
) -> Result<(), EngineError> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(()),
        Some(value) if value.as_i64().is_some_and(|number| number > 0) => Ok(()),
        _ => Err(invalid_pending_action(format!(
            "{field} must be absent, null, or a positive integer for item {item_id}"
        ))),
    }
}

fn valid_pending_body_hash_format(body_hash: &str) -> bool {
    if let Some(hex) = body_hash.strip_prefix("fnv64:") {
        return hex.len() == 16 && hex.bytes().all(|byte| byte.is_ascii_hexdigit());
    }
    // `sha256:` values in persisted pre-contract artifacts are opaque tokens
    // such as `sha256:body`, not digests. Tightening that legacy prefix to 64
    // hex characters would make carried-forward artifacts unreadable.
    ["hash-", "sha256:"]
        .iter()
        .find_map(|prefix| body_hash.strip_prefix(prefix))
        .is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        })
}

fn invalid_pending_action(message: impl Into<String>) -> EngineError {
    github_feedback_error(format!("invalid pending marker action: {}", message.into()))
}
