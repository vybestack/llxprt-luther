use super::*;

pub fn persist_external_wait_state(
    request: &luther_workflow::daemon::launcher::LaunchRequest,
    config: &WorkflowConfig,
    db_path: &std::path::Path,
    step_id: &str,
    reason: &str,
) -> Result<(), String> {
    let conn = rusqlite::Connection::open(db_path).map_err(|e| e.to_string())?;
    let checkpoint = load_checkpoint_with_conn(&conn, &request.run_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("missing waiting checkpoint for {}", request.run_id))?;
    let mut metadata = get_run_with_conn(&conn, &request.run_id).map_err(|e| e.to_string())?;
    let wait_kind = wait_kind_for_step(step_id);
    let identity = wait_poll_identity(request, config, metadata.as_ref(), wait_kind)?;
    if let Some(md) = metadata.as_mut() {
        persist_run_poll_identity(&conn, md, &identity)?;
    }
    let previous = get_wait_state(&conn, &request.run_id).map_err(|e| e.to_string())?;
    let mut record =
        previous.unwrap_or_else(|| WaitStateRecord::new(&request.run_id, &config.config_id));
    record.lease_id = lookup_lease_id(&conn, request)?;
    record.workflow_type = config.workflow_type_id.clone();
    record.config_id = config.config_id.clone();
    record.repository = request.repo.clone();
    record.issue_number = request.issue_number;
    record.pr_number = identity.pr_number;
    record.head_sha = identity.head_sha;
    record.wait_kind = wait_kind;
    let step_params = resolved_wait_step_parameters(config, step_id)?;
    record.wait_condition =
        wait_condition_payload(step_id, reason, request, wait_kind, &step_params)?;
    if wait_kind == WaitKind::DependencyChildWorkflow {
        if let Some(child_run_id) = record.head_sha.clone() {
            record.wait_condition["child_run_id"] = serde_json::Value::String(child_run_id);
        }
        if let Some(artifact_root) = wait_artifact_root(config, metadata.as_ref())? {
            if let Some(wait) = read_child_workflow_wait_artifact(&artifact_root)? {
                record.wait_condition["child_issue_number"] = wait
                    .get("child_issue_number")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                record.wait_condition["child_lease_id"] = wait
                    .get("child_lease_id")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                record.wait_condition["parent_run_id"] =
                    serde_json::Value::String(request.run_id.clone());
            }
        }
    }
    record.last_observed_state = serde_json::json!({
        "classification": "suspended",
        "step_id": step_id,
        "reason": reason
    });
    let poll_interval = config
        .discovery
        .as_ref()
        .and_then(|d| d.poll_interval_secs)
        .unwrap_or(300);
    record.poll_interval_seconds = poll_interval;
    record.max_wait_seconds = max_wait_seconds_for_wait(config, wait_kind);
    record.next_poll_at = luther_workflow::polling::next_poll_time(poll_interval);
    record.resume_step = checkpoint.step_id.clone();
    record.checkpoint_id = luther_workflow::engine::continuation::checkpoint_identity(&checkpoint);
    upsert_wait_state(&conn, &record).map_err(|e| e.to_string())?;
    if let Err(e) = write_wait_state_artifact(&request.run_id, &record) {
        eprintln!(
            "Warning: failed to write wait-state artifact for run {}: {e}",
            request.run_id
        );
    }
    Ok(())
}

pub fn max_wait_seconds_for_wait(config: &WorkflowConfig, wait_kind: WaitKind) -> Option<u64> {
    match wait_kind {
        WaitKind::DependencyChildMerge | WaitKind::DependencyChildWorkflow => Some(
            config
                .parent_orchestration
                .max_child_merge_wait_seconds
                .unwrap_or(DEFAULT_MAX_CHILD_MERGE_WAIT_SECONDS),
        ),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitPollIdentity {
    pub pr_number: Option<u64>,
    pub head_sha: Option<String>,
}

pub fn persist_run_poll_identity(
    conn: &rusqlite::Connection,
    metadata: &mut RunMetadata,
    identity: &WaitPollIdentity,
) -> Result<(), String> {
    let mut changed = false;
    if let Some(pr_number) = identity.pr_number {
        let pr_number = i64::try_from(pr_number).map_err(|e| e.to_string())?;
        if metadata.pr_number != Some(pr_number) {
            metadata.pr_number = Some(pr_number);
            changed = true;
        }
    }
    if let Some(head_sha) = identity.head_sha.as_ref() {
        if metadata.head_sha.as_deref() != Some(head_sha.as_str()) {
            metadata.head_sha = Some(head_sha.clone());
            changed = true;
        }
    }
    if changed {
        persist_run_with_conn(conn, metadata).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn wait_poll_identity(
    request: &luther_workflow::daemon::launcher::LaunchRequest,
    config: &WorkflowConfig,
    metadata: Option<&RunMetadata>,
    wait_kind: WaitKind,
) -> Result<WaitPollIdentity, String> {
    let artifact_root = wait_artifact_root(config, metadata)?;
    let artifact_identity = artifact_root
        .as_deref()
        .map(|root| read_pr_identity_artifact(root, &request.run_id))
        .transpose()?
        .flatten();
    let artifact_pr_number = artifact_identity
        .as_ref()
        .and_then(|value| value.get("pr_number").and_then(serde_json::Value::as_u64));
    let artifact_head_sha = artifact_identity
        .as_ref()
        .and_then(|value| string_field(value, "head_sha"));
    let mut identity = WaitPollIdentity {
        pr_number: artifact_pr_number.or_else(|| metadata_pr_number(metadata)),
        head_sha: artifact_head_sha.or_else(|| metadata.and_then(|md| md.head_sha.clone())),
    };
    if matches!(
        wait_kind,
        WaitKind::DependencyChildMerge | WaitKind::DependencyChildWorkflow
    ) {
        fill_parent_dependency_wait_identity(wait_kind, &mut identity, artifact_root.as_deref())?;
    }
    validate_wait_poll_identity(wait_kind, &identity)?;
    Ok(identity)
}

pub fn validate_wait_poll_identity(
    wait_kind: WaitKind,
    identity: &WaitPollIdentity,
) -> Result<(), String> {
    match wait_kind {
        WaitKind::PrChecks => {
            if identity.pr_number.is_none() || identity.head_sha.is_none() {
                return Err("missing PR number or head SHA for PR checks wait state".to_string());
            }
        }
        WaitKind::CoderabbitReview
        | WaitKind::HumanReview
        | WaitKind::PrMerge
        | WaitKind::DependencyChildMerge => {
            if identity.pr_number.is_none() {
                return Err(format!("missing PR number for {wait_kind} wait state"));
            }
        }
        WaitKind::DependencyChildWorkflow => {
            if identity.head_sha.is_none() {
                return Err(format!("missing child run ID for {wait_kind} wait state"));
            }
        }
        WaitKind::RateLimitBackoff => {}
    }
    Ok(())
}

pub fn metadata_pr_number(metadata: Option<&RunMetadata>) -> Option<u64> {
    metadata
        .and_then(|md| md.pr_number)
        .and_then(|number| u64::try_from(number).ok())
}

pub fn fill_parent_dependency_wait_identity(
    wait_kind: WaitKind,
    identity: &mut WaitPollIdentity,
    artifact_root: Option<&std::path::Path>,
) -> Result<(), String> {
    match wait_kind {
        WaitKind::DependencyChildMerge if identity.pr_number.is_none() => {
            identity.pr_number = artifact_root
                .map(read_child_merge_wait_artifact)
                .transpose()?
                .flatten();
        }
        WaitKind::DependencyChildWorkflow => {
            if let Some(value) = artifact_root
                .map(read_child_workflow_wait_artifact)
                .transpose()?
                .flatten()
            {
                identity.pr_number = value.get("pr_number").and_then(serde_json::Value::as_u64);
                identity.head_sha = string_field(&value, "child_run_id");
            }
        }
        _ => {}
    }
    Ok(())
}

pub fn wait_artifact_root(
    config: &WorkflowConfig,
    metadata: Option<&RunMetadata>,
) -> Result<Option<std::path::PathBuf>, String> {
    let Some(raw) = metadata
        .and_then(|md| md.artifact_root.clone())
        .or_else(|| config.variables.get("artifact_dir").cloned())
    else {
        return Ok(None);
    };
    let path = std::path::PathBuf::from(interpolate_config_variables(&raw, config)?);
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    Ok(Some(path))
}

pub fn read_child_workflow_wait_artifact(
    artifact_root: &std::path::Path,
) -> Result<Option<serde_json::Value>, String> {
    let path = artifact_root.join("child-workflow-wait.json");
    if !path.exists() {
        return Ok(None);
    }
    read_json_path(&path).map(Some)
}

pub fn read_child_merge_wait_artifact(
    artifact_root: &std::path::Path,
) -> Result<Option<u64>, String> {
    let path = artifact_root.join("child-merge-wait.json");
    if !path.exists() {
        return Ok(None);
    }
    let value = read_json_path(&path)?;
    value
        .get("pr")
        .and_then(|pr| pr.get("number"))
        .and_then(serde_json::Value::as_u64)
        .map(Some)
        .ok_or_else(|| {
            format!(
                "malformed child merge wait artifact at {}: missing numeric pr.number",
                path.display()
            )
        })
}

pub fn read_pr_identity_artifact(
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
            "multiple PR identity artifacts found for run {run_id}; cannot choose poll identity; matched paths: {}",
            matches
                .iter()
                .map(|(path, _)| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

pub fn collect_pr_identity_artifacts(
    dir: &std::path::Path,
    run_id: &str,
    matches: &mut Vec<(std::path::PathBuf, serde_json::Value)>,
) -> Result<(), String> {
    collect_pr_identity_artifacts_at_depth(dir, run_id, matches, 0)
}

pub fn collect_pr_identity_artifacts_at_depth(
    dir: &std::path::Path,
    run_id: &str,
    matches: &mut Vec<(std::path::PathBuf, serde_json::Value)>,
    depth: usize,
) -> Result<(), String> {
    if depth > 32 {
        return Err(format!(
            "PR identity artifact traversal exceeded depth limit at {}",
            dir.display()
        ));
    }
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_pr_identity_artifacts_at_depth(&path, run_id, matches, depth + 1)?;
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

pub fn read_json_path(path: &std::path::Path) -> Result<serde_json::Value, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    serde_json::from_str(&content)
        .map_err(|e| format!("failed to parse JSON at {}: {e}", path.display()))
}

pub fn string_field(value: &serde_json::Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
}

pub fn lookup_lease_id(
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
pub fn resolved_wait_step_parameters(
    config: &WorkflowConfig,
    step_id: &str,
) -> Result<Value, String> {
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

pub fn resolve_step_parameters(config: &WorkflowConfig, step: &StepDef) -> Result<Value, String> {
    match step.parameters.clone().unwrap_or(Value::Null) {
        Value::Object(map) => Ok(Value::Object(resolve_parameter_map(config, map)?)),
        Value::Null => Ok(Value::Null),
        other => Ok(resolve_parameter_value(config, other)?),
    }
}

pub fn resolve_parameter_map(
    config: &WorkflowConfig,
    map: Map<String, Value>,
) -> Result<Map<String, Value>, String> {
    let mut resolved = Map::new();
    for (key, value) in map {
        resolved.insert(key, resolve_parameter_value(config, value)?);
    }
    Ok(resolved)
}

pub fn resolve_parameter_value(config: &WorkflowConfig, value: Value) -> Result<Value, String> {
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

pub fn wait_condition_payload(
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
        WaitKind::DependencyChildWorkflow => {
            add_optional_wait_parameters(&mut payload, step_params)
        }
        _ => add_optional_wait_parameters(&mut payload, step_params),
    }
    Ok(payload)
}

pub fn add_required_pr_check_wait_parameters(
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

pub fn add_optional_wait_parameters(payload: &mut Value, step_params: &Value) {
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

pub fn set_required_wait_parameter(
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

pub fn set_optional_wait_parameter(payload: &mut Value, step_params: &Value, key: &str) {
    payload[key] = step_params.get(key).cloned().unwrap_or(Value::Null);
}

#[cfg(test)]
#[path = "wait_state_tests.rs"]
mod wait_state_tests;
