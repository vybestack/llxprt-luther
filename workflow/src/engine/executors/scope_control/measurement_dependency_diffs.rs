//! Dependency-manifest diff collection extracted from [`super::measurement`].
//!
//! These functions compute dependency additions by comparing manifest files
//! (TOML) between the charter's frozen merge base and the current worktree.

use std::path::Path;
use std::process::Command;

use super::MeasurementError;

/// Collect dependency manifest diffs: for each configured manifest, return the
/// path and the list of added dependency lines.
///
/// This compares the manifest at the merge base vs the current worktree/index
/// version using merge-base blobs. It fails closed on command, IO, UTF-8, and
/// TOML errors — except for a verified absent base file (the manifest is new),
/// which is treated as an empty base.
#[allow(clippy::too_many_arguments)]
pub(super) fn collect_dependency_diffs(
    work_dir: &Path,
    manifests: &[crate::workflow::schema::ScopeDependencyManifestConfig],
    merge_base: &str,
) -> Result<Vec<(String, Vec<String>)>, MeasurementError> {
    let mut result = Vec::new();
    for manifest in manifests {
        let added = diff_manifest(work_dir, &manifest.path, &manifest.sections, merge_base)?;
        if !added.is_empty() {
            result.push((manifest.path.clone(), added));
        }
    }
    Ok(result)
}

/// Diff a single dependency manifest between the merge base and the
/// worktree/index.
///
/// The merge-base blob is fetched via `git show <merge_base>:<path>`. If the
/// command fails because the file does not exist at the merge base (a new
/// manifest), the base is treated as empty. All other command failures, IO
/// errors, UTF-8 decoding errors, and TOML parse errors propagate as
/// [`MeasurementError`] so measurement fails closed.
fn diff_manifest(
    work_dir: &Path,
    manifest_path: &str,
    sections: &[String],
    merge_base: &str,
) -> Result<Vec<String>, MeasurementError> {
    let path = Path::new(manifest_path);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(MeasurementError::Parse(format!(
            "unsafe dependency manifest path: {manifest_path}"
        )));
    }
    // Get the merge-base version of the file.
    let base_spec = format!("{merge_base}:{manifest_path}");
    let base_output = Command::new("git")
        .args(["show", &base_spec])
        .current_dir(work_dir)
        .output()
        .map_err(|err| MeasurementError::Git {
            command: format!("show {base_spec}"),
            message: format!("failed to invoke git: {err}"),
        })?;

    let base_content = if base_output.status.success() {
        let content = String::from_utf8(base_output.stdout).map_err(|err| {
            MeasurementError::Parse(format!("non-UTF-8 in merge-base blob '{base_spec}': {err}"))
        })?;
        content
    } else {
        // Verify this is a "file not found" condition, not another error.
        let stderr = String::from_utf8_lossy(&base_output.stderr);
        let fatal_lower = stderr.to_ascii_lowercase();
        let not_found = fatal_lower.contains("does not exist")
            || fatal_lower.contains("exists on disk, but not in")
            || fatal_lower.contains("path does not exist");
        if !not_found {
            return Err(MeasurementError::Git {
                command: format!("show {base_spec}"),
                message: format!(
                    "exit {}: {}",
                    base_output.status.code().unwrap_or(-1),
                    stderr.trim()
                ),
            });
        }
        // Verified absent at merge base: treat as empty (new manifest).
        String::new()
    };

    let current_path = work_dir.join(manifest_path);
    let current_content =
        std::fs::read_to_string(&current_path).map_err(|err| MeasurementError::Git {
            command: format!("read {manifest_path}"),
            message: format!("failed to read manifest '{manifest_path}': {err}"),
        })?;

    let base_deps = extract_dependency_keys(&base_content, sections)?;
    let current_deps = extract_dependency_keys(&current_content, sections)?;

    let added: Vec<String> = current_deps
        .iter()
        .filter(|dep| !base_deps.contains(dep))
        .cloned()
        .collect();
    Ok(added)
}

/// Extract dependency keys from manifest content for the given sections.
///
/// For TOML manifests, this reads table headers and extracts dependency names
/// (the key before `=`). Only lines within the specified sections are
/// considered. TOML parse errors propagate as [`MeasurementError`] so
/// measurement fails closed.
pub(super) fn extract_dependency_keys(
    content: &str,
    sections: &[String],
) -> Result<Vec<String>, MeasurementError> {
    let parsed: toml::Value = toml::from_str(content).map_err(|err| {
        MeasurementError::Parse(format!("failed to parse manifest as TOML: {err}"))
    })?;

    let mut deps = Vec::new();
    if let toml::Value::Table(root) = &parsed {
        for section in sections {
            if let Some(toml::Value::Table(table)) = root.get(section) {
                for key in table.keys() {
                    deps.push(key.clone());
                }
            }
            // Also support dotted section paths like "target.cfg(unix).dependencies"
            collect_nested_keys(root, section, &mut deps);
        }
    }
    Ok(deps)
}

/// Collect keys from a nested TOML table using dotted-path notation.
fn collect_nested_keys(table: &toml::value::Table, path: &str, deps: &mut Vec<String>) {
    let segments = split_toml_path(path);
    if segments.len() < 2 {
        return;
    }
    let mut current = table;
    for segment in &segments[..segments.len() - 1] {
        match current.get(segment) {
            Some(toml::Value::Table(nested)) => current = nested,
            _ => return,
        }
    }
    if let Some(toml::Value::Table(final_table)) = current.get(&segments[segments.len() - 1]) {
        deps.extend(final_table.keys().cloned());
    }
}

fn split_toml_path(path: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut segment = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for character in path.chars() {
        if escaped {
            segment.push(character);
            escaped = false;
        } else if character == '\\' && quoted {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
        } else if character == '.' && !quoted {
            segments.push(std::mem::take(&mut segment));
        } else {
            segment.push(character);
        }
    }
    segments.push(segment);
    segments
}
