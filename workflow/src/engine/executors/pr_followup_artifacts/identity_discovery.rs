use super::{artifact_error, binding_from_value, read_json_file};
use crate::engine::runner::EngineError;
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn discover_current_pr_artifact(
    current_root: &Path,
    expected_run_id: &str,
) -> Result<Option<Value>, EngineError> {
    if !current_root.exists() {
        return Ok(None);
    }

    let mut matches = Vec::new();
    let mut visited = HashSet::new();
    collect_pr_artifacts(current_root, expected_run_id, &mut matches, &mut visited, 0)?;
    matches.sort_by(|left, right| left.0.cmp(&right.0));
    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(matches.remove(0).1)),
        _ => ambiguous_pr_artifacts_error(expected_run_id, &matches),
    }
}

fn ambiguous_pr_artifacts_error(
    expected_run_id: &str,
    matches: &[(PathBuf, Value)],
) -> Result<Option<Value>, EngineError> {
    let paths = matches
        .iter()
        .map(|(path, _)| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(artifact_error(format!(
        "multiple PR identity artifacts found for run {expected_run_id}; provide repository_owner, repository_name, and pr_number parameters; conflicting artifacts: {paths}"
    )))
}

fn collect_pr_artifacts(
    dir: &Path,
    expected_run_id: &str,
    matches: &mut Vec<(PathBuf, Value)>,
    visited: &mut HashSet<PathBuf>,
    depth: usize,
) -> Result<(), EngineError> {
    const MAX_PR_ARTIFACT_DISCOVERY_DEPTH: usize = 8;
    if depth > MAX_PR_ARTIFACT_DISCOVERY_DEPTH {
        return Ok(());
    }
    let canonical_dir = dir.canonicalize().map_err(|err| {
        artifact_error(format!(
            "canonicalize pr artifact directory {}: {err}",
            dir.display()
        ))
    })?;
    if !visited.insert(canonical_dir) {
        return Ok(());
    }

    for entry in fs::read_dir(dir)
        .map_err(|err| artifact_error(format!("read pr artifact directory: {err}")))?
    {
        collect_pr_artifact_entry(entry, expected_run_id, matches, visited, depth)?;
    }
    Ok(())
}

fn collect_pr_artifact_entry(
    entry: Result<fs::DirEntry, std::io::Error>,
    expected_run_id: &str,
    matches: &mut Vec<(PathBuf, Value)>,
    visited: &mut HashSet<PathBuf>,
    depth: usize,
) -> Result<(), EngineError> {
    let entry =
        entry.map_err(|err| artifact_error(format!("read pr artifact directory entry: {err}")))?;
    let file_type = entry
        .file_type()
        .map_err(|err| artifact_error(format!("read pr artifact directory entry type: {err}")))?;
    if file_type.is_symlink() {
        return Ok(());
    }
    let path = entry.path();
    if file_type.is_dir() {
        collect_pr_artifacts(&path, expected_run_id, matches, visited, depth + 1)
    } else if file_type.is_file()
        && path.file_name().and_then(|name| name.to_str()) == Some("pr.json")
    {
        collect_pr_json_artifact(&path, expected_run_id, matches);
        Ok(())
    } else {
        Ok(())
    }
}

fn collect_pr_json_artifact(
    path: &Path,
    expected_run_id: &str,
    matches: &mut Vec<(PathBuf, Value)>,
) {
    let value = match read_json_file(path) {
        Ok(value) => value,
        Err(err) => {
            eprintln!(
                "warning: failed to read PR identity artifact {} during discovery: {err}",
                path.display()
            );
            return;
        }
    };
    if value.get("run_id").and_then(Value::as_str) == Some(expected_run_id)
        && is_current_pr_identity(&value)
    {
        matches.push((path.to_path_buf(), value));
    }
}

pub(super) fn is_current_pr_identity(value: &Value) -> bool {
    value.get("source").and_then(Value::as_str) != Some("legacy_harness")
        && binding_from_value(value).is_ok()
}
