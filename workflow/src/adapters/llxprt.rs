//! llxprt CLI preflight adapter.
//!
//! Provides a structured readiness gate for the `llxprt` agent binary before
//! any workflow state is created. The workflow runtime spawns `llxprt` from the
//! `LlxprtExecutor`; this adapter validates that the configured binary exists
//! and reports a usable `--version`, surfacing actionable diagnostics on
//! failure. It mirrors the GitHub `gh` preflight adapter
//! (`crate::adapters::github`) so both readiness gates share one shape.
//!
//! @plan:PLAN-20260404-INITIAL-RUNTIME.P10

use std::collections::HashMap;
use std::process::Command;

use serde_json::Value;
use thiserror::Error;

use crate::workflow::schema::WorkflowType;

/// Default binary name used when no path is configured.
pub const DEFAULT_LLXPRT_BINARY: &str = "llxprt";

/// Step parameter name that overrides the llxprt binary path for a single step.
pub const BINARY_PATH_PARAM: &str = "binary_path";

/// Workflow variable name that overrides the llxprt binary path globally.
pub const BINARY_PATH_VARIABLE: &str = "llxprt_binary_path";

/// Structured error for llxprt CLI preflight failures.
#[derive(Debug, Error)]
pub enum LlxprtError {
    /// The configured llxprt binary was not found (spawn `NotFound`).
    #[error(
        "llxprt binary not found at `{path}`; install llxprt or set `llxprt_binary_path` to a valid path"
    )]
    BinaryNotFound {
        /// The resolved binary path that could not be located.
        path: String,
    },
    /// The binary exists but `--version` failed, timed out, or was unparseable.
    #[error("llxprt binary at `{path}` failed version check: {message}")]
    VersionCheckFailed {
        /// The resolved binary path.
        path: String,
        /// Human-readable failure detail.
        message: String,
    },
    /// The binary path exists but could not be executed (permissions/other OS).
    #[error("llxprt binary at `{path}` is not executable: {message}")]
    NotExecutable {
        /// The resolved binary path.
        path: String,
        /// Human-readable failure detail.
        message: String,
    },
}

impl LlxprtError {
    /// Get structured diagnostics for this error.
    ///
    /// Mirrors [`crate::adapters::github::GithubError::get_diagnostics`]: every
    /// variant carries `error_type`, `message`, `timestamp`, and a
    /// `required_action`, plus the resolved `path`.
    pub fn get_diagnostics(&self) -> HashMap<String, String> {
        let mut diag = HashMap::new();
        diag.insert("error_type".to_string(), "LlxprtError".to_string());
        diag.insert("message".to_string(), self.to_string());
        diag.insert("timestamp".to_string(), chrono::Utc::now().to_rfc3339());

        match self {
            LlxprtError::BinaryNotFound { path } => {
                diag.insert("path".to_string(), path.clone());
                diag.insert(
                    "required_action".to_string(),
                    "install llxprt or set `llxprt_binary_path` to a valid path".to_string(),
                );
            }
            LlxprtError::VersionCheckFailed { path, .. } => {
                diag.insert("path".to_string(), path.clone());
                diag.insert(
                    "required_action".to_string(),
                    format!("verify `{path} --version` runs and exits successfully"),
                );
            }
            LlxprtError::NotExecutable { path, .. } => {
                diag.insert("path".to_string(), path.clone());
                diag.insert(
                    "required_action".to_string(),
                    format!("ensure `{path}` has execute permission"),
                );
            }
        }

        diag
    }
}

/// Parsed `llxprt --version` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlxprtVersion {
    /// The raw, trimmed `--version` stdout.
    pub raw: String,
}

/// Command-runner seam for `llxprt` preflight calls.
///
/// Implementations run `{binary} --version` and return captured stdout on
/// success. They are responsible for mapping a missing binary to
/// [`LlxprtError::BinaryNotFound`], a non-executable binary to
/// [`LlxprtError::NotExecutable`], and a non-zero / failed `--version` to
/// [`LlxprtError::VersionCheckFailed`].
pub trait LlxprtCommandRunner {
    /// Execute `{binary} --version` and return captured stdout.
    fn version(&self, binary: &str) -> Result<String, LlxprtError>;
}

/// Production runner that spawns `{binary} --version` via `std::process`.
#[derive(Debug, Default)]
pub struct SystemLlxprtCommandRunner;

impl LlxprtCommandRunner for SystemLlxprtCommandRunner {
    fn version(&self, binary: &str) -> Result<String, LlxprtError> {
        let output = Command::new(binary)
            .arg("--version")
            .output()
            .map_err(|err| match err.kind() {
                std::io::ErrorKind::NotFound => LlxprtError::BinaryNotFound {
                    path: binary.to_string(),
                },
                _ => LlxprtError::NotExecutable {
                    path: binary.to_string(),
                    message: err.to_string(),
                },
            })?;
        if !output.status.success() {
            return Err(LlxprtError::VersionCheckFailed {
                path: binary.to_string(),
                message: format!(
                    "exit code {:?}: {}",
                    output.status.code(),
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// Resolve the llxprt binary path for a step.
///
/// Precedence: step parameter `binary_path` → workflow variable
/// `llxprt_binary_path` → default `"llxprt"`. Used by BOTH preflight and the
/// executor so they never diverge.
pub fn resolve_binary_path(params: Option<&Value>, variables: &HashMap<String, String>) -> String {
    if let Some(path) = params
        .and_then(|p| p.get(BINARY_PATH_PARAM))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return path.to_string();
    }
    if let Some(path) = variables
        .get(BINARY_PATH_VARIABLE)
        .filter(|s| !s.is_empty())
    {
        return path.clone();
    }
    DEFAULT_LLXPRT_BINARY.to_string()
}

/// Verify that the binary is available by running `{binary} --version`.
pub fn check_binary_available(
    runner: &dyn LlxprtCommandRunner,
    binary: &str,
) -> Result<LlxprtVersion, LlxprtError> {
    let raw = runner.version(binary)?;
    Ok(LlxprtVersion {
        raw: raw.trim().to_string(),
    })
}

/// True when a step spawns the llxprt binary (i.e. is not a pure
/// `static_content` / `static_stdout` short-circuit step).
fn step_spawns_binary(params: Option<&Value>) -> bool {
    let Some(params) = params else {
        return true;
    };
    let has_static = params
        .get("static_content")
        .and_then(Value::as_str)
        .is_some()
        || params
            .get("static_stdout")
            .and_then(Value::as_str)
            .is_some();
    !has_static
}

/// Run the full llxprt preflight gate over a workflow.
///
/// Collects the unique resolved binary paths across every `step_type ==
/// "llxprt"` step that actually spawns the binary, validates each via
/// [`check_binary_available`], and returns the validated paths. Aborts on the
/// first failure with a structured [`LlxprtError`].
pub fn run_preflight(
    runner: &dyn LlxprtCommandRunner,
    workflow_type: &WorkflowType,
    variables: &HashMap<String, String>,
) -> Result<Vec<String>, LlxprtError> {
    let mut validated: Vec<String> = Vec::new();
    for step in &workflow_type.steps {
        if step.step_type != "llxprt" {
            continue;
        }
        if !step_spawns_binary(step.parameters.as_ref()) {
            continue;
        }
        let binary = resolve_binary_path(step.parameters.as_ref(), variables);
        if validated.contains(&binary) {
            continue;
        }
        check_binary_available(runner, &binary)?;
        validated.push(binary);
    }
    Ok(validated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::schema::StepDef;

    /// Fixture runner mapping binary name → canned `version()` result.
    struct FixtureLlxprtCommandRunner {
        results: HashMap<String, Result<String, LlxprtError>>,
    }

    impl LlxprtCommandRunner for FixtureLlxprtCommandRunner {
        fn version(&self, binary: &str) -> Result<String, LlxprtError> {
            match self.results.get(binary) {
                Some(Ok(out)) => Ok(out.clone()),
                Some(Err(LlxprtError::BinaryNotFound { path })) => {
                    Err(LlxprtError::BinaryNotFound { path: path.clone() })
                }
                Some(Err(LlxprtError::VersionCheckFailed { path, message })) => {
                    Err(LlxprtError::VersionCheckFailed {
                        path: path.clone(),
                        message: message.clone(),
                    })
                }
                Some(Err(LlxprtError::NotExecutable { path, message })) => {
                    Err(LlxprtError::NotExecutable {
                        path: path.clone(),
                        message: message.clone(),
                    })
                }
                None => Err(LlxprtError::BinaryNotFound {
                    path: binary.to_string(),
                }),
            }
        }
    }

    fn ok_runner(binary: &str, version: &str) -> FixtureLlxprtCommandRunner {
        let mut results = HashMap::new();
        results.insert(binary.to_string(), Ok(version.to_string()));
        FixtureLlxprtCommandRunner { results }
    }

    fn step(step_id: &str, params: Option<Value>) -> StepDef {
        StepDef {
            step_id: step_id.to_string(),
            step_type: "llxprt".to_string(),
            description: None,
            parameters: params,
            produces: None,
            consumes: None,
            terminal: None,
        }
    }

    fn workflow(steps: Vec<StepDef>) -> WorkflowType {
        WorkflowType {
            workflow_type_id: "t".to_string(),
            steps,
            transitions: Vec::new(),
            guards: crate::workflow::schema::GuardConfig::default(),
        }
    }

    #[test]
    fn resolve_binary_path_prefers_param() {
        let mut vars = HashMap::new();
        vars.insert(BINARY_PATH_VARIABLE.to_string(), "/from/var".to_string());
        let params = serde_json::json!({ "binary_path": "/from/param" });
        assert_eq!(resolve_binary_path(Some(&params), &vars), "/from/param");
    }

    #[test]
    fn resolve_binary_path_falls_back_to_variable() {
        let mut vars = HashMap::new();
        vars.insert(BINARY_PATH_VARIABLE.to_string(), "/from/var".to_string());
        assert_eq!(resolve_binary_path(None, &vars), "/from/var");
    }

    #[test]
    fn resolve_binary_path_defaults_to_llxprt() {
        let vars = HashMap::new();
        assert_eq!(resolve_binary_path(None, &vars), "llxprt");
    }

    #[test]
    fn check_binary_available_returns_version() {
        let runner = ok_runner("llxprt", "llxprt 1.2.3\n");
        let version = check_binary_available(&runner, "llxprt").unwrap();
        assert_eq!(version.raw, "llxprt 1.2.3");
    }

    #[test]
    fn check_binary_available_maps_missing_binary() {
        let runner = FixtureLlxprtCommandRunner {
            results: HashMap::new(),
        };
        let err = check_binary_available(&runner, "/no/such/llxprt").unwrap_err();
        match err {
            LlxprtError::BinaryNotFound { path } => assert_eq!(path, "/no/such/llxprt"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn check_binary_available_maps_nonzero_version() {
        let mut results = HashMap::new();
        results.insert(
            "llxprt".to_string(),
            Err(LlxprtError::VersionCheckFailed {
                path: "llxprt".to_string(),
                message: "exit code Some(1): boom".to_string(),
            }),
        );
        let runner = FixtureLlxprtCommandRunner { results };
        let err = check_binary_available(&runner, "llxprt").unwrap_err();
        assert!(matches!(err, LlxprtError::VersionCheckFailed { .. }));
    }

    #[test]
    fn run_preflight_validates_each_unique_path() {
        let mut results = HashMap::new();
        results.insert("llxprt".to_string(), Ok("llxprt 1.0".to_string()));
        results.insert("/opt/llxprt".to_string(), Ok("llxprt 2.0".to_string()));
        let runner = FixtureLlxprtCommandRunner { results };

        let wt = workflow(vec![
            step("a", None),
            step(
                "b",
                Some(serde_json::json!({ "binary_path": "/opt/llxprt" })),
            ),
            step(
                "c",
                Some(serde_json::json!({ "binary_path": "/opt/llxprt" })),
            ),
        ]);
        let paths = run_preflight(&runner, &wt, &HashMap::new()).unwrap();
        assert_eq!(paths, vec!["llxprt".to_string(), "/opt/llxprt".to_string()]);
    }

    #[test]
    fn run_preflight_skips_static_steps() {
        let runner = FixtureLlxprtCommandRunner {
            results: HashMap::new(),
        };
        let wt = workflow(vec![step(
            "static",
            Some(serde_json::json!({
                "static_content": "hi",
                "success_file": "out.txt"
            })),
        )]);
        let paths = run_preflight(&runner, &wt, &HashMap::new()).unwrap();
        assert!(paths.is_empty());
    }

    #[test]
    fn run_preflight_aggregates_first_failure() {
        let runner = FixtureLlxprtCommandRunner {
            results: HashMap::new(),
        };
        let wt = workflow(vec![step("a", None)]);
        let err = run_preflight(&runner, &wt, &HashMap::new()).unwrap_err();
        assert!(matches!(err, LlxprtError::BinaryNotFound { .. }));
    }

    #[test]
    fn diagnostics_include_required_action_per_variant() {
        for err in [
            LlxprtError::BinaryNotFound {
                path: "llxprt".to_string(),
            },
            LlxprtError::VersionCheckFailed {
                path: "llxprt".to_string(),
                message: "boom".to_string(),
            },
            LlxprtError::NotExecutable {
                path: "llxprt".to_string(),
                message: "denied".to_string(),
            },
        ] {
            let diag = err.get_diagnostics();
            assert_eq!(diag.get("error_type").unwrap(), "LlxprtError");
            assert!(diag.contains_key("message"));
            assert!(diag.contains_key("required_action"));
            assert_eq!(diag.get("path").unwrap(), "llxprt");
        }
    }
}
