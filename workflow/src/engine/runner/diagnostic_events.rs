//! Path-only diagnostic event projection.
//!
//! Reads diagnostic artifact paths strictly from the executing step's own
//! namespace so that later non-llxprt steps never inherit a prior step's
//! diagnostic paths.

use crate::engine::executor::StepContext;

const DIAGNOSTIC_PATH_KEYS: [&str; 3] = [
    "stdout_artifact_path",
    "stderr_artifact_path",
    "llxprt_diagnostic_manifest_path",
];

/// Project diagnostic artifact paths for `step_id`'s own namespace only.
///
/// Accepts the explicit `step_id` and reads each key via the
/// `"{step_id}.{key}"` namespaced form, which resolves against that single
/// namespace and never falls back to bare variables or other steps' scopes.
pub(super) fn details(context: &StepContext, step_id: &str) -> Option<String> {
    let mut value = serde_json::Map::new();
    for key in DIAGNOSTIC_PATH_KEYS {
        let namespaced_key = format!("{step_id}.{key}");
        if let Some(path) = context.get(&namespaced_key) {
            value.insert(key.to_string(), serde_json::Value::String(path.clone()));
        }
    }
    if value.is_empty() {
        None
    } else {
        serde_json::to_string(&value).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_only_diagnostic_paths() {
        let mut context = StepContext::new("/tmp/work".into(), "run".to_string());
        context.set_current_step_id("create_plan");
        context.set("stdout", "secret output");
        context.set("stdout_artifact_path", "/tmp/stdout.log");
        let projected = details(&context, "create_plan").unwrap();
        assert!(projected.contains("/tmp/stdout.log"));
        assert!(!projected.contains("secret output"));
    }

    #[test]
    fn later_non_llxprt_step_does_not_inherit_prior_diagnostic_paths() {
        let mut context = StepContext::new("/tmp/work".into(), "run".to_string());
        context.set_current_step_id("create_plan");
        context.set("stdout_artifact_path", "/tmp/create_plan-stdout.log");
        context.set("stderr_artifact_path", "/tmp/create_plan-stderr.log");
        context.set(
            "llxprt_diagnostic_manifest_path",
            "/tmp/create_plan-manifest.json",
        );

        // A later non-llxprt step runs; it has no diagnostic paths of its own.
        context.set_current_step_id("verify_changes");
        context.set("stdout", "verify output");

        let projected = details(&context, "verify_changes");
        assert!(
            projected.is_none(),
            "non-llxprt step must not inherit prior step diagnostic paths"
        );
    }

    #[test]
    fn exact_step_namespace_is_read_without_bare_fallback() {
        let mut context = StepContext::new("/tmp/work".into(), "run".to_string());
        // With no current_step_id, `set` stores only in flat variables, leaving
        // the "evaluate_plan" namespace empty while a bare value exists.
        context.set("stdout_artifact_path", "/tmp/leaked-stdout.log");

        let projected = details(&context, "evaluate_plan");
        assert!(
            projected.is_none(),
            "must not read bare fallback when the step namespace is empty"
        );
    }
}
