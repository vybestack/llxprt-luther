use super::*;

pub struct MarkerParseError {
    pub diagnostic: String,
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-009
/// @pseudocode lines 13,20
pub fn parse_marker_from_comment_body(
    body: &str,
) -> Result<RemoteFeedbackMarker, MarkerParseError> {
    let start = body
        .find("<!--")
        .ok_or_else(|| marker_parse_error("missing marker comment"))?;
    let rest = &body[start..];
    let end = rest
        .find("-->")
        .ok_or_else(|| marker_parse_error("unterminated marker comment"))?;
    parse_hidden_marker(&rest[..end + 3])
}

pub fn parse_hidden_marker(body: &str) -> Result<RemoteFeedbackMarker, MarkerParseError> {
    let marker = extract_exact_marker_body(body)?;
    let fields = marker
        .strip_prefix(MARKER_NAMESPACE)
        .ok_or_else(|| marker_parse_error("wrong marker namespace"))?
        .trim();
    if fields.is_empty() {
        return Err(marker_parse_error("missing marker fields"));
    }
    let mut map = BTreeMap::new();
    for part in fields.split_whitespace() {
        let (key, value) = part
            .split_once('=')
            .ok_or_else(|| marker_parse_error(format!("malformed field {part}")))?;
        if key.is_empty() || value.is_empty() {
            return Err(marker_parse_error(format!("empty field {part}")));
        }
        if map.insert(key, value).is_some() {
            return Err(marker_parse_error(format!("duplicate field {key}")));
        }
    }
    let stable_marker_key = required_marker_field(&map, "marker_key")?.to_string();
    let source_head_sha = required_marker_field(&map, "source_head")?.to_string();
    let remediation_output_head = required_marker_field(&map, "remediation_output_head")?;
    let body_hash = required_marker_field(&map, "body")?.to_string();
    let run_id = required_marker_field(&map, "run_id")?.to_string();
    let action_kind = required_marker_field(&map, "action")?.to_string();
    Ok(RemoteFeedbackMarker {
        stable_marker_key,
        source_head_sha,
        remediation_output_head_sha: (remediation_output_head != "none")
            .then(|| remediation_output_head.to_string()),
        body_hash,
        action_kind,
        run_id,
        status: "completed".to_string(),
    })
}

pub fn extract_exact_marker_body(body: &str) -> Result<&str, MarkerParseError> {
    let trimmed = body.trim();
    if !trimmed.starts_with("<!--") || !trimmed.ends_with("-->") {
        return Err(marker_parse_error(
            "marker must be a single exact hidden HTML comment",
        ));
    }
    let inner = &trimmed[4..trimmed.len() - 3];
    if inner.contains("<!--") || inner.contains("-->") {
        return Err(marker_parse_error("nested marker comment delimiter"));
    }
    let marker = inner.trim();
    if !marker.starts_with(MARKER_NAMESPACE) {
        return Err(marker_parse_error("wrong marker namespace"));
    }
    Ok(marker)
}

pub fn required_marker_field<'a>(
    map: &'a BTreeMap<&str, &str>,
    field: &str,
) -> Result<&'a str, MarkerParseError> {
    map.get(field)
        .copied()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| marker_parse_error(format!("missing field {field}")))
}

pub fn marker_parse_error(diagnostic: impl Into<String>) -> MarkerParseError {
    MarkerParseError {
        diagnostic: diagnostic.into(),
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 2
pub fn configured_identities(params: &Value) -> BTreeSet<String> {
    let mut identities = [
        "coderabbitai",
        "coderabbitai[bot]",
        "coderabbit[bot]",
        "coderabbit",
    ]
    .into_iter()
    .map(ToString::to_string)
    .collect::<BTreeSet<_>>();
    if let Some(extra) = params
        .get("coderabbit_bot_identities")
        .and_then(Value::as_array)
    {
        for identity in extra.iter().filter_map(Value::as_str) {
            identities.insert(identity.to_ascii_lowercase());
        }
    }
    // When include_all_reviewers is set, add the wildcard sentinel so any
    // reviewer's threads flow through the same deterministic mechanism.
    // @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
    // @requirement:REQ-PRFU-024
    if params
        .get("include_all_reviewers")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        identities.insert(ALL_REVIEWERS_SENTINEL.to_string());
    }
    identities
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-024
/// @pseudocode lines 10
pub fn is_coderabbit(author: &str, identities: &BTreeSet<String>) -> bool {
    if author.is_empty() {
        return false;
    }
    identities.contains(ALL_REVIEWERS_SENTINEL) || is_explicit_reviewer_identity(author, identities)
}

pub fn is_explicit_reviewer_identity(author: &str, identities: &BTreeSet<String>) -> bool {
    if author.is_empty() {
        return false;
    }
    identities.contains(&author.to_ascii_lowercase())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009
/// @pseudocode lines 13-14
pub fn item_set_hash(items: &[FeedbackItem]) -> String {
    let mut material = items
        .iter()
        .map(|item| {
            format!(
                "{}:{}:{}",
                item.stable_marker_key,
                item.body_hash,
                item.commit_sha.as_deref().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>();
    material.sort();
    stable_hash(&material.join("|"))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009
/// @pseudocode lines 13-14
/// FNV-1a is used here only for deterministic artifact deduplication, not security.
pub fn stable_hash(text: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv64:{hash:016x}")
}
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 14-19
pub fn readiness_stability_hash(observation: &FeedbackObservation) -> String {
    let material = json!({
        "feedback_item_set_hash": item_set_hash(&observation.items),
        "ready_signal": observation.ready_signal,
        "in_progress_signal": observation.in_progress_signal,
        "readiness_signals": observation.readiness_signals,
        "stale_signals": observation.stale_signals,
        "items": observation.items.iter().map(|item| json!({
            "stable_marker_key": item.stable_marker_key,
            "body_hash": item.body_hash,
            "commit_sha": item.commit_sha,
            "resolved": item.resolved,
            "outdated": item.outdated,
            "resolution_state_available": item.resolution_state_available,
            "updated_at": item.updated_at,
            "source": item.source,
        })).collect::<Vec<_>>(),
        "remote_markers": observation.remote_markers.iter().map(remote_marker_json).collect::<Vec<_>>(),
        "malformed_remote_markers": observation.malformed_remote_markers,
        "matched_identities": observation.matched_identities.iter().cloned().collect::<Vec<_>>(),
    });
    match serde_json::to_string(&material) {
        Ok(serialized) => stable_hash(&serialized),
        Err(err) => {
            eprintln!("warning: failed to serialize readiness stability material: {err}");
            stable_hash("")
        }
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 1
pub fn read_or_build_binding(
    context: &StepContext,
    params: &Value,
    store: &PrFollowupArtifactStore,
) -> Result<PrFollowupBinding, EngineError> {
    let requested = fallback_binding(context, params)?;
    if let Some(value) = store.find_current_pr_artifact_for_run(context.run_id(), &requested)? {
        return binding_from_value(&value);
    }
    require_binding_identity(context, params)
}

pub fn fallback_binding(
    context: &StepContext,
    params: &Value,
) -> Result<PrFollowupBinding, EngineError> {
    Ok(PrFollowupBinding {
        schema_version: PR_FOLLOWUP_SCHEMA_VERSION,
        run_id: context.run_id().to_string(),
        repository_owner: string_param(context, params, "repository_owner", "example"),
        repository_name: string_param(context, params, "repository_name", "workflow"),
        pr_number: {
            let raw = string_param(context, params, "pr_number", "1910");
            raw.parse()
                .map_err(|err| github_feedback_error(format!("invalid pr_number '{raw}': {err}")))?
        },
        head_ref: string_param(context, params, "head_ref", "feature"),
        head_sha: string_param(
            context,
            params,
            "head_sha",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
        base_ref: string_param(context, params, "base_ref", "main"),
        base_sha: Some(string_param(context, params, "base_sha", "base-a")),
    })
}

pub fn require_binding_identity(
    context: &StepContext,
    params: &Value,
) -> Result<PrFollowupBinding, EngineError> {
    Ok(PrFollowupBinding {
        schema_version: PR_FOLLOWUP_SCHEMA_VERSION,
        run_id: context.run_id().to_string(),
        repository_owner: required_string_param(context, params, "repository_owner")?,
        repository_name: required_string_param(context, params, "repository_name")?,
        pr_number: {
            let raw = required_string_param(context, params, "pr_number")?;
            raw.parse()
                .map_err(|err| github_feedback_error(format!("invalid pr_number '{raw}': {err}")))?
        },
        head_ref: required_string_param(context, params, "head_ref")?,
        head_sha: required_string_param(context, params, "head_sha")?,
        base_ref: required_string_param(context, params, "base_ref")?,
        base_sha: Some(required_string_param(context, params, "base_sha")?),
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 22
pub fn is_permission_or_schema_error(value: &Value) -> bool {
    value
        .get("errors")
        .and_then(Value::as_array)
        .is_some_and(|errors| !errors.is_empty())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 1
pub fn binding_from_value(value: &Value) -> Result<PrFollowupBinding, EngineError> {
    Ok(PrFollowupBinding {
        schema_version: u32::try_from(require_u64(value, "schema_version")?)
            .map_err(|err| github_feedback_error(format!("schema_version out of range: {err}")))?,
        run_id: require_string(value, "run_id")?,
        repository_owner: require_string(value, "repository_owner")?,
        repository_name: require_string(value, "repository_name")?,
        pr_number: require_u64(value, "pr_number")?,
        head_ref: require_string(value, "head_ref")?,
        head_sha: require_string(value, "head_sha")?,
        base_ref: require_string(value, "base_ref")?,
        base_sha: value
            .get("base_sha")
            .and_then(Value::as_str)
            .map(ToString::to_string),
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 1
pub fn require_string(value: &Value, field: &str) -> Result<String, EngineError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| github_feedback_error(format!("missing string field {field}")))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 1
pub fn require_u64(value: &Value, field: &str) -> Result<u64, EngineError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| github_feedback_error(format!("missing integer field {field}")))
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 1
pub fn string_param(context: &StepContext, params: &Value, key: &str, default: &str) -> String {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(|template| interpolate_string(template, context))
        .filter(|value| !has_unresolved_token(value) && !value.is_empty())
        .or_else(|| context.get(key).cloned())
        .filter(|value| !has_unresolved_token(value) && !value.is_empty())
        .unwrap_or_else(|| default.to_string())
}

pub fn required_string_param(
    context: &StepContext,
    params: &Value,
    key: &str,
) -> Result<String, EngineError> {
    let value = string_param(context, params, key, "");
    if value.is_empty() {
        Err(github_feedback_error(format!("missing {key}")))
    } else {
        Ok(value)
    }
}

pub fn has_unresolved_token(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'{' {
            let Some(close) = value[index + 1..].find('}') else {
                return true;
            };
            let token = &value[index + 1..index + 1 + close];
            if !token.is_empty()
                && token
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '.')
            {
                return true;
            }
            index += close + 2;
        } else {
            index += 1;
        }
    }
    false
}

pub fn artifact_root(context: &StepContext, params: &Value) -> Result<PathBuf, EngineError> {
    let raw = params
        .get("artifact_root")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| github_feedback_error("missing artifact_root"))?;
    let interpolated = interpolate_string(raw, context);
    if interpolated.contains('{') || interpolated.contains('}') {
        return Err(github_feedback_error(format!(
            "artifact_root contains unresolved template token: {interpolated}"
        )));
    }
    let path = PathBuf::from(interpolated);
    Ok(if path.is_absolute() {
        path
    } else {
        context.work_dir().join(path)
    })
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 3
pub fn u64_param(params: &Value, key: &str, default: u64) -> u64 {
    params.get(key).and_then(Value::as_u64).unwrap_or(default)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 7
pub fn string_field(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 7
pub fn opt_string(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 1,20
pub fn current_step_id(context: &StepContext, fallback: &str) -> String {
    context
        .get("current_step_id")
        .cloned()
        .unwrap_or_else(|| fallback.to_string())
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008
/// @pseudocode lines 22
pub fn github_feedback_error(message: impl Into<String>) -> EngineError {
    EngineError::StepExecutionError {
        step_id: "github_coderabbit_feedback".to_string(),
        message: message.into(),
    }
}
