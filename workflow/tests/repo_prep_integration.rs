//! Repository preparation integration tests.
//!
//! Tests for repository configuration deserialization, workspace strategies,
//! branch preparation, and failure diagnostics.

/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
/// @requirement:REQ-EARS-REPO-001
/// Test: Repository config deserialization from TOML/JSON
#[tokio::test]
async fn test_repository_config_deserialization() {
    // GIVEN: valid repository configuration TOML
    let toml_str = r#"
workspace_strategy = "shared"
branch_template = "luther-fix-{issue_number}"
base_branch = "main"
cleanup_on_success = true
cleanup_on_failure = false
"#;

    // WHEN: deserializing to RepositoryConfig
    let config = luther_workflow::repo::RepositoryConfig::from_toml(toml_str)
        .expect("Failed to deserialize repository config");

    // THEN: all fields are correctly parsed
    assert_eq!(config.workspace_strategy, "shared");
    assert_eq!(config.branch_template, "luther-fix-{issue_number}");
    assert_eq!(config.base_branch.as_deref(), Some("main"));
    assert!(config.cleanup_on_success);
    assert!(!config.cleanup_on_failure);
}

/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
/// @requirement:REQ-EARS-REPO-002, REPO-003
/// Test: Shared workspace strategy for multiple runs
#[tokio::test]
async fn test_shared_workspace_strategy() {
    // GIVEN: a repository config with shared workspace strategy
    let config = luther_workflow::repo::RepositoryConfig {
        workspace_strategy: "shared".to_string(),
        branch_template: "luther-fix-{issue_number}".to_string(),
        base_branch: Some("main".to_string()),
        cleanup_on_success: false,
        cleanup_on_failure: false,
    };

    // WHEN: preparing workspace for multiple runs
    let workspace = luther_workflow::repo::Workspace::prepare(&config, "/tmp/test-repo")
        .await
        .expect("Failed to prepare workspace");

    // THEN: same workspace path is returned for all runs
    let run_id_1 = "run-001";
    let run_id_2 = "run-002";
    let path_1 = workspace.path_for_run(run_id_1);
    let path_2 = workspace.path_for_run(run_id_2);
    
    assert_eq!(path_1, path_2, "Shared workspace should return same path for all runs");
    assert!(workspace.is_shared());
    assert!(!workspace.is_temp());
}

/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
/// @requirement:REQ-EARS-REPO-004
/// Test: Per-run workspace strategy (temporary/isolated)
#[tokio::test]
async fn test_per_run_workspace_strategy() {
    // GIVEN: a repository config with temp_clone workspace strategy
    let config = luther_workflow::repo::RepositoryConfig {
        workspace_strategy: "temp_clone".to_string(),
        branch_template: "luther-fix-{issue_number}".to_string(),
        base_branch: Some("main".to_string()),
        cleanup_on_success: true,
        cleanup_on_failure: false,
    };

    // WHEN: preparing workspace for each run
    let workspace = luther_workflow::repo::Workspace::prepare(&config, "/tmp/test-repo")
        .await
        .expect("Failed to prepare workspace");

    // THEN: different workspace paths are returned for each run
    let run_id_1 = "run-001";
    let run_id_2 = "run-002";
    let path_1 = workspace.path_for_run(run_id_1);
    let path_2 = workspace.path_for_run(run_id_2);
    
    assert_ne!(path_1, path_2, "Per-run workspace should return different paths");
    assert!(!workspace.is_shared());
    assert!(workspace.is_temp());
}

/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
/// @requirement:REQ-EARS-REPO-005
/// Test: Branch preparation (checkout existing branch)
#[tokio::test]
async fn test_branch_preparation() {
    // GIVEN: a workspace with existing target branch
    let config = luther_workflow::repo::RepositoryConfig {
        workspace_strategy: "temp_clone".to_string(),
        branch_template: "luther-fix-{issue_number}".to_string(),
        base_branch: Some("main".to_string()),
        cleanup_on_success: true,
        cleanup_on_failure: false,
    };

    let mut branch_manager = luther_workflow::repo::BranchManager::new(&config);
    
    let params = luther_workflow::repo::BranchParams {
        issue_number: 123,
        run_id: "run-001".to_string(),
    };

    // WHEN: preparing branch
    let result = branch_manager
        .prepare_branch(&params, "/tmp/test-repo")
        .await;

    // THEN: existing branch is checked out
    assert!(result.is_ok(), "Branch preparation should succeed: {:?}", result.err());
    let branch_result = result.unwrap();
    assert_eq!(branch_result.branch_name, "luther-fix-123");
    assert!(!branch_result.created);
}

/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
/// @requirement:REQ-EARS-REPO-006
/// Test: Branch creation when target branch doesn't exist
#[tokio::test]
async fn test_branch_create_if_missing() {
    // GIVEN: a workspace where target branch doesn't exist
    let config = luther_workflow::repo::RepositoryConfig {
        workspace_strategy: "temp_clone".to_string(),
        branch_template: "luther-fix-{issue_number}".to_string(),
        base_branch: Some("main".to_string()),
        cleanup_on_success: true,
        cleanup_on_failure: false,
    };

    let mut branch_manager = luther_workflow::repo::BranchManager::new(&config);
    
    let params = luther_workflow::repo::BranchParams {
        issue_number: 999, // Non-existent issue
        run_id: "run-002".to_string(),
    };

    // WHEN: preparing branch (should create if missing)
    let result = branch_manager
        .prepare_branch(&params, "/tmp/test-repo")
        .await;

    // THEN: new branch is created from base_branch
    assert!(result.is_ok(), "Branch creation should succeed: {:?}", result.err());
    let branch_result = result.unwrap();
    assert_eq!(branch_result.branch_name, "luther-fix-999");
    assert!(branch_result.created);
    assert_eq!(branch_result.base_branch, "main");
}

/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
/// @requirement:REQ-EARS-REPO-008
/// Test: Repository preparation failure diagnostics
#[tokio::test]
async fn test_repo_prep_failure_diagnostics() {
    // GIVEN: a non-existent or invalid repository path
    let config = luther_workflow::repo::RepositoryConfig {
        workspace_strategy: "temp_clone".to_string(),
        branch_template: "luther-fix-{issue_number}".to_string(),
        base_branch: Some("main".to_string()),
        cleanup_on_success: true,
        cleanup_on_failure: false,
    };

    // WHEN: attempting to prepare invalid repository
    let result = luther_workflow::repo::Workspace::prepare(&config, "/nonexistent/path")
        .await;

    // THEN: returns descriptive error with diagnostics
    assert!(result.is_err(), "Should fail for invalid repository path");
    match result {
        Err(e) => {
            // Verify error contains diagnostic information
            assert!(e.to_string().contains("repository") || e.to_string().contains("path"),
                "Error should mention repository or path: {}", e);
            
            // Verify error type supports structured diagnostics
            let diag = e.get_diagnostics();
            assert!(!diag.is_empty(), "Error should provide structured diagnostics");
        }
        Ok(_) => panic!("Expected workspace preparation to fail"),
    }
}
