//! llxprt binary preflight adapter tests.
//!
//! Fixture-driven coverage for the readiness gate: binary availability,
//! version-check failures, custom `binary_path` resolution, and the
//! orchestrating `run_preflight`. No live `llxprt` invocation occurs.
//!
//! @plan:PLAN-20260404-INITIAL-RUNTIME.P10

use std::collections::HashMap;

use luther_workflow::adapters::llxprt::{run_preflight, LlxprtCommandRunner, LlxprtError};
use luther_workflow::workflow::schema::{GuardConfig, StepDef, WorkflowType};

/// Fixture runner: maps a binary path to a canned `version()` result.
#[derive(Default)]
struct FixtureLlxprtCommandRunner {
    responses: HashMap<String, Result<String, LlxprtError>>,
}

impl FixtureLlxprtCommandRunner {
    fn with_ok(mut self, binary: &str, version: &str) -> Self {
        self.responses
            .insert(binary.to_string(), Ok(version.to_string()));
        self
    }

    fn with_err(mut self, binary: &str, err: LlxprtError) -> Self {
        self.responses.insert(binary.to_string(), Err(err));
        self
    }
}

impl LlxprtCommandRunner for FixtureLlxprtCommandRunner {
    fn version(&self, binary: &str) -> Result<String, LlxprtError> {
        match self.responses.get(binary) {
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

fn llxprt_step(step_id: &str, params: Option<serde_json::Value>) -> StepDef {
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
        workflow_type_id: "preflight-test".to_string(),
        steps,
        transitions: Vec::new(),
        guards: GuardConfig::default(),
    }
}

#[test]
fn preflight_ok_returns_resolved_paths() {
    let runner = FixtureLlxprtCommandRunner::default().with_ok("llxprt", "llxprt 1.0.0");
    let wt = workflow(vec![llxprt_step("agent", None)]);
    let paths = run_preflight(&runner, &wt, &HashMap::new()).expect("preflight should pass");
    assert_eq!(paths, vec!["llxprt".to_string()]);
}

#[test]
fn preflight_missing_binary_reports_actionable_diagnostics() {
    let runner = FixtureLlxprtCommandRunner::default();
    let wt = workflow(vec![llxprt_step("agent", None)]);
    let err = run_preflight(&runner, &wt, &HashMap::new()).expect_err("missing binary");
    match &err {
        LlxprtError::BinaryNotFound { path } => assert_eq!(path, "llxprt"),
        other => panic!("unexpected: {other:?}"),
    }
    let diag = err.get_diagnostics();
    assert!(diag.contains_key("required_action"));
}

#[test]
fn preflight_resolves_custom_binary_path() {
    let runner =
        FixtureLlxprtCommandRunner::default().with_ok("/opt/llxprt/bin/llxprt", "llxprt 2.0.0");
    let wt = workflow(vec![llxprt_step(
        "agent",
        Some(serde_json::json!({ "binary_path": "/opt/llxprt/bin/llxprt" })),
    )]);
    let paths = run_preflight(&runner, &wt, &HashMap::new()).expect("custom path validates");
    assert_eq!(paths, vec!["/opt/llxprt/bin/llxprt".to_string()]);
}

#[test]
fn preflight_resolves_variable_binary_path() {
    let runner = FixtureLlxprtCommandRunner::default().with_ok("/var/llxprt", "llxprt 3.0.0");
    let mut vars = HashMap::new();
    vars.insert("llxprt_binary_path".to_string(), "/var/llxprt".to_string());
    let wt = workflow(vec![llxprt_step("agent", None)]);
    let paths = run_preflight(&runner, &wt, &vars).expect("variable path validates");
    assert_eq!(paths, vec!["/var/llxprt".to_string()]);
}

#[test]
fn preflight_surfaces_version_check_failure() {
    let runner = FixtureLlxprtCommandRunner::default().with_err(
        "llxprt",
        LlxprtError::VersionCheckFailed {
            path: "llxprt".to_string(),
            message: "exit code Some(1)".to_string(),
        },
    );
    let wt = workflow(vec![llxprt_step("agent", None)]);
    let err = run_preflight(&runner, &wt, &HashMap::new()).expect_err("version failure");
    assert!(matches!(err, LlxprtError::VersionCheckFailed { .. }));
}
