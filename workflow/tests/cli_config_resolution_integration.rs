/// @plan:PLAN-20260408-LLXPRT-FIRST.P20
/// Integration tests for CLI config resolution with production and test fixture layouts
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

use luther_workflow::workflow::config_loader::{resolve_workflow_config, resolve_workflow_type};

const TEST_WORKFLOW_TOML: &str = r#"
workflow_type_id = "test"

[[steps]]
step_id = "step1"
step_type = "shell"

[steps.parameters]
command = "echo hello"
"#;

const TEST_CONFIG_TOML: &str = r#"
config_id = "test"
workflow_type_id = "test"

[runtime]
timeout_seconds = 3600
max_retries = 3

[repository]
workspace_strategy = "temp_clone"
branch_template = "issue{issue_number}"
base_branch = "main"

[guards]
max_iterations = 10
max_file_changes = 200
max_tokens = 500000
max_cost = 50.0
"#;

/// Test resolving workflow type from flat production layout (config/workflows/)
#[test]
fn test_resolve_workflow_type_from_config_dir() {
    let temp_dir = TempDir::new().unwrap();
    let workflows_dir = temp_dir.path().join("workflows");
    fs::create_dir_all(&workflows_dir).unwrap();

    let workflow_path = workflows_dir.join("test.toml");
    fs::write(&workflow_path, TEST_WORKFLOW_TOML).unwrap();

    let result = resolve_workflow_type("test", temp_dir.path());

    assert!(
        result.is_ok(),
        "Expected workflow type to resolve from flat workflows dir: {:?}",
        result.err()
    );
    let workflow_type = result.unwrap();
    assert_eq!(workflow_type.workflow_type_id, "test");
}

/// Test resolving workflow type from test fixture layout (config/workflows/valid/)
#[test]
fn test_resolve_workflow_type_from_valid_subdir() {
    let temp_dir = TempDir::new().unwrap();
    let valid_dir = temp_dir.path().join("workflows/valid");
    fs::create_dir_all(&valid_dir).unwrap();

    let workflow_path = valid_dir.join("test.toml");
    fs::write(&workflow_path, TEST_WORKFLOW_TOML).unwrap();

    let result = resolve_workflow_type("test", temp_dir.path());

    assert!(
        result.is_ok(),
        "Expected workflow type to resolve from valid/ subdirectory: {:?}",
        result.err()
    );
    let workflow_type = result.unwrap();
    assert_eq!(workflow_type.workflow_type_id, "test");
}

/// Test resolving workflow config from flat production layout (config/workflow-configs/)
#[test]
fn test_resolve_workflow_config_from_config_dir() {
    let temp_dir = TempDir::new().unwrap();
    let configs_dir = temp_dir.path().join("workflow-configs");
    fs::create_dir_all(&configs_dir).unwrap();

    let config_path = configs_dir.join("test.toml");
    fs::write(&config_path, TEST_CONFIG_TOML).unwrap();

    let result = resolve_workflow_config("test", temp_dir.path());

    assert!(
        result.is_ok(),
        "Expected workflow config to resolve from flat workflow-configs dir: {:?}",
        result.err()
    );
    let config = result.unwrap();
    assert_eq!(config.config_id, "test");
}

/// Test resolving production workflows from actual config/ directory
#[test]
fn test_resolve_production_workflows_from_config() {
    let config_root = PathBuf::from("config");

    for workflow_type_id in [
        "issue-fix-v1",
        "llxprt-issue-fix-v1",
        "llxprt-luther-dogfood-v1",
    ] {
        let workflow_type =
            resolve_workflow_type(workflow_type_id, &config_root).unwrap_or_else(|err| {
                panic!("Expected production workflow type {workflow_type_id} to resolve: {err:?}")
            });
        assert_eq!(workflow_type.workflow_type_id, workflow_type_id);
    }

    for (config_id, expected_workflow_type_id) in [
        ("profile-0", "issue-fix-v1"),
        ("llxprt-code", "llxprt-issue-fix-v1"),
        ("llxprt-luther", "llxprt-luther-dogfood-v1"),
    ] {
        let config = resolve_workflow_config(config_id, &config_root).unwrap_or_else(|err| {
            panic!("Expected production workflow config {config_id} to resolve: {err:?}")
        });
        assert_eq!(config.config_id, config_id);
        assert_eq!(config.workflow_type_id, expected_workflow_type_id);
    }
}

/// Test that flat layout takes precedence over valid/ subdirectory
#[test]
fn test_flat_layout_takes_precedence() {
    let temp_dir = TempDir::new().unwrap();

    // Create both flat and valid/ versions with different content
    let flat_dir = temp_dir.path().join("workflows");
    let valid_dir = temp_dir.path().join("workflows/valid");
    fs::create_dir_all(&flat_dir).unwrap();
    fs::create_dir_all(&valid_dir).unwrap();

    let flat_workflow = r#"
workflow_type_id = "flat-version"

[[steps]]
step_id = "step1"
step_type = "shell"

[steps.parameters]
command = "echo flat"
"#;

    let valid_workflow = r#"
workflow_type_id = "valid-version"

[[steps]]
step_id = "step1"
step_type = "shell"

[steps.parameters]
command = "echo valid"
"#;

    fs::write(flat_dir.join("test.toml"), flat_workflow).unwrap();
    fs::write(valid_dir.join("test.toml"), valid_workflow).unwrap();

    let result = resolve_workflow_type("test", temp_dir.path()).unwrap();
    // Flat layout should win (checked first)
    assert_eq!(result.workflow_type_id, "flat-version");
}

/// Test JSON fallback in production layout
#[test]
fn test_resolve_json_from_config_dir() {
    let temp_dir = TempDir::new().unwrap();
    let workflows_dir = temp_dir.path().join("workflows");
    fs::create_dir_all(&workflows_dir).unwrap();

    let workflow_json = r#"
{
  "workflow_type_id": "json-test",
  "steps": [
    {
      "step_id": "step1",
      "step_type": "shell",
      "parameters": {
        "command": "echo hello"
      }
    }
  ]
}
"#;

    fs::write(workflows_dir.join("json-test.json"), workflow_json).unwrap();

    let result = resolve_workflow_type("json-test", temp_dir.path());

    assert!(
        result.is_ok(),
        "Expected JSON workflow type to resolve: {:?}",
        result.err()
    );
    let workflow_type = result.unwrap();
    assert_eq!(workflow_type.workflow_type_id, "json-test");
}

/// Test JSON fallback in valid/ subdirectory
#[test]
fn test_resolve_json_from_valid_subdir() {
    let temp_dir = TempDir::new().unwrap();
    let valid_dir = temp_dir.path().join("workflows/valid");
    fs::create_dir_all(&valid_dir).unwrap();

    let workflow_json = r#"
{
  "workflow_type_id": "json-valid-test",
  "steps": [
    {
      "step_id": "step1",
      "step_type": "shell",
      "parameters": {
        "command": "echo hello"
      }
    }
  ]
}
"#;

    fs::write(valid_dir.join("json-valid-test.json"), workflow_json).unwrap();

    let result = resolve_workflow_type("json-valid-test", temp_dir.path());

    assert!(
        result.is_ok(),
        "Expected JSON workflow type to resolve from valid/: {:?}",
        result.err()
    );
    let workflow_type = result.unwrap();
    assert_eq!(workflow_type.workflow_type_id, "json-valid-test");
}

/// Test that existing test fixtures still work using valid/ subdirectory fallback
#[test]
fn test_existing_test_fixtures_resolve() {
    let fixture_root = PathBuf::from("tests/fixtures");

    for workflow_type_id in ["issue-fix-v1", "hello-world-v1", "llxprt-issue-fix-v1"] {
        let workflow_type =
            resolve_workflow_type(workflow_type_id, &fixture_root).unwrap_or_else(|err| {
                panic!("Expected {workflow_type_id} to resolve from fixtures: {err:?}")
            });
        assert_eq!(workflow_type.workflow_type_id, workflow_type_id);
    }

    for (config_id, expected_workflow_type_id) in [
        ("profile-0", "issue-fix-v1"),
        ("hello-world-config", "hello-world-v1"),
        ("llxprt-code", "llxprt-issue-fix-v1"),
    ] {
        let config = resolve_workflow_config(config_id, &fixture_root)
            .unwrap_or_else(|err| panic!("Expected {config_id} to resolve from fixtures: {err:?}"));
        assert_eq!(config.config_id, config_id);
        assert_eq!(config.workflow_type_id, expected_workflow_type_id);
    }
}
