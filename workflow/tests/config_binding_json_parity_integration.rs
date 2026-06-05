/// @plan:PLAN-20260404-INITIAL-RUNTIME.P04
/// Integration tests for TOML/JSON parity - both formats should produce equivalent structs.
///
/// These tests verify that workflow types and configs can be loaded from both
/// TOML and JSON formats and produce semantically equivalent results.
use std::fs;
use std::path::PathBuf;

use luther_workflow::workflow::{
    parse_workflow_config_json, parse_workflow_config_toml, parse_workflow_type_json,
    parse_workflow_type_toml,
};

/// Helper to get the fixtures root path.
fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Test: TOML and JSON fixtures produce equivalent `WorkflowType` structs.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P04
/// @requirement:REQ-EARS-WF-004
#[test]
fn test_toml_json_produce_equivalent_workflow_type() {
    // GIVEN: both TOML and JSON fixtures exist for issue-fix-v1
    let fixture_root = fixtures_root();
    let toml_path = fixture_root.join("workflows/valid/issue-fix-v1.toml");
    let json_path = fixture_root.join("workflows/valid/issue-fix-v1.json");

    assert!(toml_path.exists(), "TOML fixture should exist");
    assert!(json_path.exists(), "JSON fixture should exist");

    // Load the file contents
    let toml_content = fs::read_to_string(&toml_path).expect("Failed to read TOML fixture");
    let json_content = fs::read_to_string(&json_path).expect("Failed to read JSON fixture");

    // WHEN: parsing both
    let toml_result = parse_workflow_type_toml(&toml_content);
    let json_result = parse_workflow_type_json(&json_content);

    // THEN: both parse successfully
    assert!(
        toml_result.is_ok(),
        "TOML parsing should succeed: {:?}",
        toml_result.err()
    );
    assert!(
        json_result.is_ok(),
        "JSON parsing should succeed: {:?}",
        json_result.err()
    );

    let toml_workflow = toml_result.unwrap();
    let json_workflow = json_result.unwrap();

    // AND: resulting WorkflowType structs are equal
    assert_eq!(
        toml_workflow.workflow_type_id, json_workflow.workflow_type_id,
        "workflow_type_id should match"
    );
    assert_eq!(
        toml_workflow.steps.len(),
        json_workflow.steps.len(),
        "step count should match"
    );

    // Verify each step is equivalent
    for (i, (toml_step, json_step)) in toml_workflow
        .steps
        .iter()
        .zip(json_workflow.steps.iter())
        .enumerate()
    {
        assert_eq!(
            toml_step.step_id, json_step.step_id,
            "step_id mismatch at index {i}"
        );
        assert_eq!(
            toml_step.step_type, json_step.step_type,
            "step_type mismatch for step {}",
            toml_step.step_id
        );
        assert_eq!(
            toml_step.description, json_step.description,
            "description mismatch for step {}",
            toml_step.step_id
        );
    }

    assert_eq!(
        toml_workflow.transitions.len(),
        json_workflow.transitions.len(),
        "transition count should match"
    );

    // Verify each transition is equivalent
    for (i, (toml_trans, json_trans)) in toml_workflow
        .transitions
        .iter()
        .zip(json_workflow.transitions.iter())
        .enumerate()
    {
        assert_eq!(
            toml_trans.from, json_trans.from,
            "transition 'from' mismatch at index {i}"
        );
        assert_eq!(
            toml_trans.to, json_trans.to,
            "transition 'to' mismatch at index {i}"
        );
        assert_eq!(
            toml_trans.condition, json_trans.condition,
            "transition condition mismatch at index {i}"
        );
    }

    // Verify guard config
    assert_eq!(
        toml_workflow.guards.max_retries, json_workflow.guards.max_retries,
        "max_retries should match"
    );
    assert_eq!(
        toml_workflow.guards.timeout_seconds, json_workflow.guards.timeout_seconds,
        "timeout_seconds should match"
    );
    assert_eq!(
        toml_workflow.guards.require_approval, json_workflow.guards.require_approval,
        "require_approval should match"
    );
}

/// Test: TOML and JSON fixtures produce equivalent `WorkflowConfig` structs.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P04
/// @requirement:REQ-EARS-WF-004
#[test]
fn test_toml_json_produce_equivalent_config() {
    // GIVEN: both TOML and JSON fixtures exist for profile-0
    let fixture_root = fixtures_root();
    let toml_path = fixture_root.join("workflow-configs/valid/profile-0.toml");
    let json_path = fixture_root.join("workflow-configs/valid/profile-0.json");

    assert!(toml_path.exists(), "TOML fixture should exist");
    assert!(json_path.exists(), "JSON fixture should exist");

    // Load the file contents
    let toml_content = fs::read_to_string(&toml_path).expect("Failed to read TOML fixture");
    let json_content = fs::read_to_string(&json_path).expect("Failed to read JSON fixture");

    // WHEN: parsing both
    let toml_result = parse_workflow_config_toml(&toml_content);
    let json_result = parse_workflow_config_json(&json_content);

    // THEN: both parse successfully
    assert!(
        toml_result.is_ok(),
        "TOML parsing should succeed: {:?}",
        toml_result.err()
    );
    assert!(
        json_result.is_ok(),
        "JSON parsing should succeed: {:?}",
        json_result.err()
    );

    let toml_config = toml_result.unwrap();
    let json_config = json_result.unwrap();

    // AND: resulting WorkflowConfig structs are equal
    assert_eq!(
        toml_config.config_id, json_config.config_id,
        "config_id should match"
    );
    assert_eq!(
        toml_config.workflow_type_id, json_config.workflow_type_id,
        "workflow_type_id should match"
    );

    // Verify runtime config
    assert_eq!(
        toml_config.runtime.timeout_seconds, json_config.runtime.timeout_seconds,
        "runtime.timeout_seconds should match"
    );
    assert_eq!(
        toml_config.runtime.max_retries, json_config.runtime.max_retries,
        "runtime.max_retries should match"
    );
    assert_eq!(
        toml_config.runtime.parallel_steps, json_config.runtime.parallel_steps,
        "runtime.parallel_steps should match"
    );
    assert_eq!(
        toml_config.runtime.log_level, json_config.runtime.log_level,
        "runtime.log_level should match"
    );

    // Verify repo config
    assert_eq!(
        toml_config.repo.workspace_strategy, json_config.repo.workspace_strategy,
        "repo.workspace_strategy should match"
    );
    assert_eq!(
        toml_config.repo.branch_template, json_config.repo.branch_template,
        "repo.branch_template should match"
    );
    assert_eq!(
        toml_config.repo.base_branch, json_config.repo.base_branch,
        "repo.base_branch should match"
    );

    // Verify guard limits
    assert_eq!(
        toml_config.guard_limits.max_iterations, json_config.guard_limits.max_iterations,
        "guard_limits.max_iterations should match"
    );
    assert_eq!(
        toml_config.guard_limits.max_file_changes, json_config.guard_limits.max_file_changes,
        "guard_limits.max_file_changes should match"
    );
    assert_eq!(
        toml_config.guard_limits.max_tokens, json_config.guard_limits.max_tokens,
        "guard_limits.max_tokens should match"
    );
    assert_eq!(
        toml_config.guard_limits.max_cost, json_config.guard_limits.max_cost,
        "guard_limits.max_cost should match"
    );
}

/// Test: All valid workflow fixtures have both TOML and JSON versions.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P04
/// @requirement:REQ-EARS-WF-004
#[test]
fn test_workflow_fixtures_have_both_formats() {
    // GIVEN: valid workflow fixtures exist
    let fixture_root = fixtures_root();
    let valid_dir = fixture_root.join("workflows/valid");

    // WHEN: scanning the valid directory
    let entries = fs::read_dir(&valid_dir).expect("Failed to read valid workflows directory");

    let mut toml_files = Vec::new();
    let mut json_files = Vec::new();

    for entry in entries {
        let entry = entry.expect("Failed to read directory entry");
        let path = entry.path();
        if let Some(ext) = path.extension() {
            let name = path.file_stem().unwrap().to_str().unwrap();
            if ext == "toml" {
                toml_files.push(name.to_string());
            } else if ext == "json" {
                json_files.push(name.to_string());
            }
        }
    }

    // THEN: every TOML file has a corresponding JSON file
    for toml_name in &toml_files {
        assert!(
            json_files.contains(toml_name),
            "Missing JSON fixture for {toml_name}.toml"
        );
    }

    // AND: every JSON file has a corresponding TOML file
    for json_name in &json_files {
        assert!(
            toml_files.contains(json_name),
            "Missing TOML fixture for {json_name}.json"
        );
    }
}

/// Test: All valid config fixtures have both TOML and JSON versions.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P04
/// @requirement:REQ-EARS-WF-004
#[test]
fn test_config_fixtures_have_both_formats() {
    // GIVEN: valid config fixtures exist
    let fixture_root = fixtures_root();
    let valid_dir = fixture_root.join("workflow-configs/valid");

    // WHEN: scanning the valid directory
    let entries =
        fs::read_dir(&valid_dir).expect("Failed to read valid workflow-configs directory");

    let mut toml_files = Vec::new();
    let mut json_files = Vec::new();

    for entry in entries {
        let entry = entry.expect("Failed to read directory entry");
        let path = entry.path();
        if let Some(ext) = path.extension() {
            let name = path.file_stem().unwrap().to_str().unwrap();
            if ext == "toml" {
                toml_files.push(name.to_string());
            } else if ext == "json" {
                json_files.push(name.to_string());
            }
        }
    }

    // THEN: every TOML file has a corresponding JSON file
    for toml_name in &toml_files {
        assert!(
            json_files.contains(toml_name),
            "Missing JSON fixture for {toml_name}.toml"
        );
    }

    // AND: every JSON file has a corresponding TOML file
    for json_name in &json_files {
        assert!(
            toml_files.contains(json_name),
            "Missing TOML fixture for {json_name}.json"
        );
    }
}

/// Test: JSON parsing handles missing optional fields gracefully.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P04
/// @requirement:REQ-EARS-WF-004
#[test]
fn test_json_parsing_handles_optional_fields() {
    // GIVEN: minimal JSON with only required fields
    let minimal_json = r#"{
        "workflow_type_id": "minimal-test",
        "steps": [
            {"step_id": "step1", "step_type": "test"}
        ],
        "transitions": [],
        "guards": {}
    }"#;

    // WHEN: parsing
    let result = parse_workflow_type_json(minimal_json);

    // THEN: parsing succeeds with optional fields as None
    assert!(
        result.is_ok(),
        "Minimal JSON should parse: {:?}",
        result.err()
    );

    let workflow = result.unwrap();
    assert_eq!(workflow.steps.len(), 1);
    assert_eq!(workflow.steps[0].step_id, "step1");
    // description and parameters are optional and should be None
}

/// Test: TOML parsing handles missing optional fields gracefully.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P04
/// @requirement:REQ-EARS-WF-004
#[test]
fn test_toml_parsing_handles_optional_fields() {
    // GIVEN: minimal TOML with only required fields
    let minimal_toml = r#"
workflow_type_id = "minimal-test"

[[steps]]
step_id = "step1"
step_type = "test"

[guards]
"#;

    // WHEN: parsing
    let result = parse_workflow_type_toml(minimal_toml);

    // THEN: parsing succeeds with optional fields as None
    assert!(
        result.is_ok(),
        "Minimal TOML should parse: {:?}",
        result.err()
    );

    let workflow = result.unwrap();
    assert_eq!(workflow.steps.len(), 1);
    assert_eq!(workflow.steps[0].step_id, "step1");
    // description should be None
    assert!(workflow.steps[0].description.is_none());
}
