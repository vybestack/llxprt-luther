fn read_child_workflow_wait_artifact(
    artifact_root: &std::path::Path,
) -> Result<Option<serde_json::Value>, String> {
    let path = artifact_root.join("child-workflow-wait.json");
    if !path.exists() {
        return Ok(None);
    }
    read_json_path(&path).map(Some)
}

fn read_child_merge_wait_artifact(artifact_root: &std::path::Path) -> Result<Option<u64>, String> {
    let path = artifact_root.join("child-merge-wait.json");
    if !path.exists() {
        return Ok(None);
    }
    let value = read_json_path(&path)?;
    Ok(value
        .get("pr")
        .and_then(|pr| pr.get("number"))
        .and_then(serde_json::Value::as_u64))
}

const CONFIG_TOKEN_UNDERSCORE: u8 = 95;
const CONFIG_TOKEN_DOT: u8 = 46;

fn is_config_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == CONFIG_TOKEN_UNDERSCORE || byte == CONFIG_TOKEN_DOT
}

fn read_pr_identity_artifact(
    artifact_root: &std::path::Path,
    run_id: &str,
) -> Result<Option<serde_json::Value>, String> {
    let current_root = artifact_root
        .join("pr-followup")
        .join("current")
        .join(run_id);
    if !current_root.exists() {
        return Ok(None);
    }
    let mut matches = Vec::new();
    collect_pr_identity_artifacts(&current_root, run_id, &mut matches)?;
    matches.sort_by(|left, right| left.0.cmp(&right.0));
    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(matches.remove(0).1)),
        _ => Err(format!(
            "multiple PR identity artifacts found for run {run_id}; cannot choose poll identity"
        )),
    }
}

fn collect_pr_identity_artifacts(
    dir: &std::path::Path,
    run_id: &str,
    matches: &mut Vec<(std::path::PathBuf, serde_json::Value)>,
) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            collect_pr_identity_artifacts(&path, run_id, matches)?;
        } else if path.file_name().and_then(|name| name.to_str()) == Some("pr.json") {
            let value = read_json_path(&path)?;
            if value.get("run_id").and_then(serde_json::Value::as_str) == Some(run_id)
                && value
                    .get("pr_number")
                    .and_then(serde_json::Value::as_u64)
                    .is_some()
                && value
                    .get("head_sha")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|head| !head.is_empty())
            {
                matches.push((path, value));
            }
        }
    }
    Ok(())
}

fn read_json_path(path: &std::path::Path) -> Result<serde_json::Value, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    serde_json::from_str(&content)
        .map_err(|e| format!("failed to parse JSON at {}: {e}", path.display()))
}

fn string_field(value: &serde_json::Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
}

fn lookup_lease_id(
    conn: &rusqlite::Connection,
    request: &luther_workflow::daemon::launcher::LaunchRequest,
) -> Result<Option<String>, String> {
    luther_workflow::persistence::leases::get_lease_for_issue(
        conn,
        &request.repo,
        request.issue_number,
    )
    .map(|lease| lease.map(|lease| lease.lease_id))
    .map_err(|e| e.to_string())
}
fn resolved_wait_step_parameters(config: &WorkflowConfig, step_id: &str) -> Result<Value, String> {
    let config_root = std::path::PathBuf::from("config");
    let workflow_type = resolve_workflow_type(&config.workflow_type_id, &config_root)
        .map_err(|e| format!("resolve workflow type for wait state: {e}"))?;
    let step = workflow_type
        .steps
        .iter()
        .find(|step| step.step_id == step_id)
        .ok_or_else(|| format!("missing wait step {step_id}"))?;
    resolve_step_parameters(config, step)
}

fn resolve_step_parameters(config: &WorkflowConfig, step: &StepDef) -> Result<Value, String> {
    match step.parameters.clone().unwrap_or(Value::Null) {
        Value::Object(map) => Ok(Value::Object(resolve_parameter_map(config, map)?)),
        Value::Null => Ok(Value::Null),
        other => Ok(resolve_parameter_value(config, other)?),
    }
}

fn resolve_parameter_map(
    config: &WorkflowConfig,
    map: Map<String, Value>,
) -> Result<Map<String, Value>, String> {
    let mut resolved = Map::new();
    for (key, value) in map {
        resolved.insert(key, resolve_parameter_value(config, value)?);
    }
    Ok(resolved)
}

fn resolve_parameter_value(config: &WorkflowConfig, value: Value) -> Result<Value, String> {
    match value {
        Value::String(raw) => Ok(Value::String(interpolate_config_variables(&raw, config)?)),
        Value::Array(items) => items
            .into_iter()
            .map(|item| resolve_parameter_value(config, item))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(map) => resolve_parameter_map(config, map).map(Value::Object),
        other => Ok(other),
    }
}

fn wait_condition_payload(
    step_id: &str,
    reason: &str,
    request: &luther_workflow::daemon::launcher::LaunchRequest,
    wait_kind: WaitKind,
    step_params: &Value,
) -> Result<Value, String> {
    let mut payload = serde_json::json!({
        "step_id": step_id,
        "reason": reason,
        "repository": request.repo,
        "issue_number": request.issue_number,
    });
    match wait_kind {
        WaitKind::PrChecks => add_required_pr_check_wait_parameters(&mut payload, step_params)?,
        WaitKind::DependencyChildWorkflow => add_optional_wait_parameters(&mut payload, step_params),
        _ => add_optional_wait_parameters(&mut payload, step_params),
    }
    Ok(payload)
}

fn add_required_pr_check_wait_parameters(
    payload: &mut Value,
    step_params: &Value,
) -> Result<(), String> {
    set_required_wait_parameter(payload, step_params, "artifact_root")?;
    set_optional_wait_parameter(payload, step_params, "check_policy");
    set_optional_wait_parameter(payload, step_params, "pr_check_policy");
    set_required_wait_parameter(payload, step_params, "head_ref")?;
    set_required_wait_parameter(payload, step_params, "base_ref")?;
    set_required_wait_parameter(payload, step_params, "base_sha")?;
    Ok(())
}

fn add_optional_wait_parameters(payload: &mut Value, step_params: &Value) {
    for key in [
        "artifact_root",
        "check_policy",
        "pr_check_policy",
        "head_ref",
        "base_ref",
        "base_sha",
    ] {
        set_optional_wait_parameter(payload, step_params, key);
    }
}

fn set_required_wait_parameter(
    payload: &mut Value,
    step_params: &Value,
    key: &str,
) -> Result<(), String> {
    let value = step_params
        .get(key)
        .filter(|value| !value.is_null())
        .cloned()
        .ok_or_else(|| format!("missing resolved PR check wait parameter {key}"))?;
    if value.as_str().is_some_and(has_unresolved_config_token) {
        return Err(format!("unresolved PR check wait parameter {key}: {value}"));
    }
    payload[key] = value;
    Ok(())
}

fn set_optional_wait_parameter(payload: &mut Value, step_params: &Value, key: &str) {
    payload[key] = step_params.get(key).cloned().unwrap_or(Value::Null);
}
