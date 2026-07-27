use super::*;

pub fn required_context(context: &StepContext, key: &str) -> Result<String, EngineError> {
    context
        .get(key)
        .cloned()
        .ok_or_else(|| parent_error(format!("missing context value '{key}'")))
}

pub fn context_value_with_warned_default(
    context: &StepContext,
    primary: &str,
    fallback: &str,
    default: &str,
) -> String {
    context
        .get(primary)
        .or_else(|| context.get(fallback))
        .cloned()
        .unwrap_or_else(|| {
            warn!(
                primary = primary,
                fallback = fallback,
                default = default,
                "parent orchestration context missing; using compatibility default"
            );
            default.to_string()
        })
}

pub fn optional_u64_context(
    context: &StepContext,
    primary: &str,
    fallback: &str,
) -> Result<Option<u64>, EngineError> {
    let Some(value) = context.get(primary).or_else(|| context.get(fallback)) else {
        return Ok(None);
    };
    value.parse::<u64>().map(Some).map_err(|err| {
        parent_error(format!(
            "invalid numeric parent orchestration context value for {primary}/{fallback}: {err}"
        ))
    })
}

pub fn parent_config_root(context: &StepContext) -> Result<PathBuf, EngineError> {
    context
        .get("config_root")
        .or_else(|| context.get("config_dir"))
        .map(PathBuf::from)
        .or_else(|| {
            context
                .get("work_dir")
                .map(|work_dir| PathBuf::from(work_dir).join("config"))
        })
        .ok_or_else(|| {
            parent_error(
                "parent orchestration requires config_root, config_dir, or work_dir to resolve child workflow config"
                    .to_string(),
            )
        })
}

pub fn parent_issue_number(context: &StepContext) -> Result<u64, EngineError> {
    let number = context
        .get("primary_issue_number")
        .or_else(|| context.get("issue_number"))
        .ok_or_else(|| parent_error("missing context value 'primary_issue_number'".to_string()))?
        .parse::<u64>()
        .map_err(|err| {
            parent_error(format!("invalid numeric parent issue context value: {err}"))
        })?;
    if number == 0 {
        return Err(parent_error(
            "parent orchestration requires launcher-injected primary_issue_number/issue_number; placeholder 0 is invalid at runtime".to_string(),
        ));
    }
    Ok(number)
}

pub fn artifact_root(context: &StepContext, params: &Value) -> Result<PathBuf, EngineError> {
    let template = params
        .get("artifact_root")
        .and_then(Value::as_str)
        .or_else(|| context.get("artifact_root").map(String::as_str))
        .or_else(|| context.get("artifact_dir").map(String::as_str))
        .unwrap_or("{work_dir}/.luther-parent-orchestration");
    let interpolated = interpolate_string(template, context);
    if interpolated.contains('{') {
        return Err(parent_error(format!(
            "artifact_root contains unresolved template token: {interpolated}"
        )));
    }
    Ok(PathBuf::from(interpolated))
}

pub fn write_json<T: serde::Serialize>(
    artifact_root: &Path,
    name: &str,
    value: &T,
) -> Result<(), EngineError> {
    fs::create_dir_all(artifact_root)
        .map_err(|err| parent_error(format!("create artifact root: {err}")))?;
    let path = artifact_root.join(name);
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|err| parent_error(format!("serialize {name}: {err}")))?;
    let write_id = ARTIFACT_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp_path = path.with_extension(format!("{}.{}.tmp", std::process::id(), write_id));
    fs::write(&temp_path, bytes)
        .map_err(|err| parent_error(format!("write {}: {err}", temp_path.display())))?;
    fs::rename(&temp_path, &path).map_err(|err| {
        let cleanup_error = fs::remove_file(&temp_path).err();
        let cleanup_context = cleanup_error
            .map(|cleanup| format!("; additionally failed to remove temp file: {cleanup}"))
            .unwrap_or_default();
        parent_error(format!(
            "rename {} to {}: {err}{cleanup_context}",
            temp_path.display(),
            path.display()
        ))
    })
}

pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, EngineError> {
    let bytes =
        fs::read(path).map_err(|err| parent_error(format!("read {}: {err}", path.display())))?;
    serde_json::from_slice(&bytes)
        .map_err(|err| parent_error(format!("parse {}: {err}", path.display())))
}

pub fn clear_selected_child(artifact_root: &Path) -> Result<(), EngineError> {
    let path = artifact_root.join("selected-child.json");
    // Remove directly and treat a missing file as success. An exists-then-remove
    // sequence races with concurrent removal and would surface a spurious
    // NotFound error as an orchestration failure.
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(parent_error(format!("remove {}: {err}", path.display()))),
    }
}

pub fn read_children(
    artifact_root: &Path,
) -> Result<Vec<crate::adapters::github_issues::GithubSubIssue>, EngineError> {
    read_json(&artifact_root.join("parent-subissues.json"))
}

pub fn selected_child(artifact_root: &Path) -> Result<Option<u64>, EngineError> {
    let selected: Value = read_json(&artifact_root.join("selected-child.json"))?;
    Ok(selected.get("issue_number").and_then(Value::as_u64))
}

pub fn github_error(err: GithubError) -> EngineError {
    parent_error(err.to_string())
}

pub fn parent_error(message: String) -> EngineError {
    EngineError::StepExecutionError {
        step_id: "parent_orchestration".to_string(),
        message,
    }
}
