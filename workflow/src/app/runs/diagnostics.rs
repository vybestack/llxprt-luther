//! LLxprt diagnostic projection for `runs show`.

use std::io::Read;
use std::path::Path;

use luther_workflow::persistence::EventRecord;

const PATH_KEYS: [&str; 3] = [
    "stdout_artifact_path",
    "stderr_artifact_path",
    "llxprt_diagnostic_manifest_path",
];
const MANIFEST_LIMIT: u64 = 64 * 1024;
const MANIFEST_COUNT_LIMIT: usize = 256;

pub(super) fn project(
    events: &[EventRecord],
    artifact_root: Option<&str>,
) -> Vec<serde_json::Value> {
    let mut diagnostics = events.iter().filter_map(project_event).collect::<Vec<_>>();
    append_manifest_fallbacks(&mut diagnostics, artifact_root);
    diagnostics
}

pub(super) fn print_human(events: &[EventRecord], artifact_root: Option<&str>) {
    let diagnostics = project(events, artifact_root);
    if diagnostics.is_empty() {
        return;
    }
    println!();
    println!("LLxprt diagnostics:");
    for diagnostic in diagnostics {
        println!("  {}", diagnostic["step_id"].as_str().unwrap_or("unknown"));
        for key in PATH_KEYS {
            if let Some(path) = diagnostic[key].as_str() {
                let size = std::fs::metadata(path).map_or(0, |metadata| metadata.len());
                println!("    {key}: {path} ({size} bytes)");
            }
        }
    }
}

fn project_event(event: &EventRecord) -> Option<serde_json::Value> {
    let details = event.details.as_deref()?;
    let details: serde_json::Value = serde_json::from_str(details).ok()?;
    project_paths(&event.step_id, &details, false)
}

fn project_paths(
    step_id: &str,
    details: &serde_json::Value,
    manifest_keys: bool,
) -> Option<serde_json::Value> {
    let mut diagnostic = serde_json::Map::new();
    diagnostic.insert(
        "step_id".to_string(),
        serde_json::Value::String(step_id.to_string()),
    );
    let mut found = false;
    for key in PATH_KEYS {
        let source_key = if manifest_keys {
            match key {
                "stdout_artifact_path" => "stdout_path",
                "stderr_artifact_path" => "stderr_path",
                _ => key,
            }
        } else {
            key
        };
        if let Some(path) = details.get(source_key).and_then(serde_json::Value::as_str) {
            diagnostic.insert(key.to_string(), serde_json::Value::String(path.to_string()));
            diagnostic.insert(
                format!("{key}_exists"),
                serde_json::Value::Bool(Path::new(path).exists()),
            );
            found = true;
        }
    }
    found.then_some(serde_json::Value::Object(diagnostic))
}

fn append_manifest_fallbacks(
    diagnostics: &mut Vec<serde_json::Value>,
    artifact_root: Option<&str>,
) {
    let Some(root) = artifact_root else {
        return;
    };
    let directory = Path::new(root).join("llxprt-diagnostics");
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten().take(MANIFEST_COUNT_LIMIT) {
        let path = entry.path();
        if !is_manifest_file(&path) {
            continue;
        }
        let Some(manifest) = read_manifest(&path) else {
            continue;
        };
        let Some(step_id) = manifest.get("step_id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if diagnostics.iter().any(|item| item["step_id"] == step_id) {
            continue;
        }
        if let Some(mut diagnostic) = project_paths(step_id, &manifest, true) {
            if let Some(object) = diagnostic.as_object_mut() {
                object.insert(
                    "llxprt_diagnostic_manifest_path".to_string(),
                    serde_json::Value::String(path.to_string_lossy().into_owned()),
                );
                object.insert(
                    "llxprt_diagnostic_manifest_path_exists".to_string(),
                    serde_json::Value::Bool(true),
                );
            }
            diagnostics.push(diagnostic);
        }
    }
}

fn is_manifest_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with("-manifest.json"))
        && std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
}

fn read_manifest(path: &Path) -> Option<serde_json::Value> {
    let file = std::fs::File::open(path).ok()?;
    let mut content = Vec::new();
    file.take(MANIFEST_LIMIT + 1)
        .read_to_end(&mut content)
        .ok()?;
    if content.len() > MANIFEST_LIMIT as usize {
        return None;
    }
    serde_json::from_slice(&content).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_fallback_projects_abruptly_stopped_step() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("llxprt-diagnostics");
        std::fs::create_dir_all(&directory).unwrap();
        let stdout = directory.join("create_plan-stdout.log");
        let stderr = directory.join("create_plan-stderr.log");
        std::fs::write(&stdout, "partial output").unwrap();
        std::fs::write(&stderr, "partial error").unwrap();
        std::fs::write(
            directory.join("create_plan-manifest.json"),
            serde_json::to_vec(&serde_json::json!({
                "step_id": "create_plan",
                "stdout_path": stdout,
                "stderr_path": stderr,
            }))
            .unwrap(),
        )
        .unwrap();

        let projected = project(&[], temp.path().to_str());
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0]["step_id"], "create_plan");
        assert_eq!(projected[0]["stdout_artifact_path_exists"], true);
    }
}
