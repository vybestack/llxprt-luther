use super::{artifact_error, binding_from_value, path_safety};
use crate::engine::runner::EngineError;
use serde_json::Value;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

const MAX_PR_ARTIFACT_DISCOVERY_DEPTH: usize = 8;
const MAX_PR_ARTIFACT_DISCOVERY_FILES: usize = 10_000;

pub(super) fn discover_current_pr_artifacts(
    store_root: &Path,
    current_root: &Path,
    expected_run_id: &str,
    budget: &mut path_safety::ReadBudget,
) -> Result<Vec<(PathBuf, Value)>, EngineError> {
    if !path_safety::validate_contained_directory(store_root, current_root)? {
        return Ok(Vec::new());
    }
    let files = path_safety::read_contained_named_files_with_budget(
        store_root,
        current_root,
        OsStr::new("pr.json"),
        MAX_PR_ARTIFACT_DISCOVERY_DEPTH,
        MAX_PR_ARTIFACT_DISCOVERY_FILES,
        budget,
    )?;
    let mut matches = Vec::new();
    for file in files {
        if let Some(candidate) = parse_current_pr_artifact(file, expected_run_id)? {
            matches.push(candidate);
        }
    }
    matches.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(matches)
}

fn parse_current_pr_artifact(
    file: path_safety::ContainedFile,
    expected_run_id: &str,
) -> Result<Option<(PathBuf, Value)>, EngineError> {
    let value = serde_json::from_str::<Value>(&file.content).map_err(|error| {
        artifact_error(format!(
            "parse exact-run PR identity candidate {}: {error}",
            file.path.display()
        ))
    })?;
    let run_id = value.get("run_id").and_then(Value::as_str);
    if run_id != Some(expected_run_id) {
        return Err(artifact_error(format!(
            "exact-run PR identity candidate {} carries run_id {:?}, expected {expected_run_id}",
            file.path.display(),
            run_id
        )));
    }
    binding_from_value(&value).map_err(|error| {
        artifact_error(format!(
            "malformed exact-run PR identity candidate {}: {error}",
            file.path.display()
        ))
    })?;
    if value.get("source").and_then(Value::as_str) == Some("legacy_harness") {
        return Ok(None);
    }
    Ok(Some((file.path, value)))
}

pub(super) fn is_current_pr_identity(value: &Value) -> bool {
    value.get("source").and_then(Value::as_str) != Some("legacy_harness")
        && binding_from_value(value).is_ok()
}
