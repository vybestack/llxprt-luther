use crate::engine::executor::{interpolate_string, StepContext};
use crate::engine::executors::change_detection::{
    diff_requirements_met, new_changed_paths, ChangeDetectionMode, ChangedPathDetector,
};

/// Bundles the changed-path detector with its selected mode.
#[derive(Clone, Copy)]
pub(super) struct DiffDetection<'a> {
    pub(super) detector: &'a dyn ChangedPathDetector,
    pub(super) mode: ChangeDetectionMode,
}

impl DiffDetection<'_> {
    pub(super) fn detect(&self, context: &mut StepContext, label: &str) -> Option<Vec<String>> {
        let work_dir = context.work_dir().to_path_buf();
        match self.detector.detect_changed_paths(&work_dir, self.mode) {
            Ok(paths) => Some(paths),
            Err(err) => {
                context.set("diagnostic", &format!("{label}: {err}"));
                None
            }
        }
    }
}

pub(super) fn detect_initial_changed_paths(
    context: &mut StepContext,
    detection: DiffDetection<'_>,
) -> Vec<String> {
    detection
        .detect(context, "initial change detection failed")
        .unwrap_or_default()
}

pub(super) fn success_condition_met(
    context: &mut StepContext,
    detection: DiffDetection<'_>,
    success_file: Option<&str>,
    success_on_diff: bool,
    required_changed_paths: &[String],
    required_changed_path_patterns: &[String],
    initial_changed_paths: &[String],
) -> bool {
    if let Some(path) = success_file {
        let path = std::path::Path::new(path);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            context.work_dir().join(path)
        };
        if path.metadata().is_ok_and(|m| m.len() > 0) {
            return true;
        }
    }

    success_on_diff
        && detection
            .detect(context, "change detection failed")
            .is_some_and(|paths| {
                diff_requirements_met(
                    &new_changed_paths(&paths, initial_changed_paths),
                    required_changed_paths,
                    required_changed_path_patterns,
                )
            })
}

pub(super) fn string_array_param(
    params: &serde_json::Value,
    name: &str,
    context: &StepContext,
) -> Vec<String> {
    params
        .get(name)
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(|template| interpolate_string(template, context))
                .collect()
        })
        .unwrap_or_default()
}
