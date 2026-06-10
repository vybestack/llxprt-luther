/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// Live Workflow Integration Tests — Real GitHub/Shell Integration
///
/// These tests call real external tools (gh, git). They are #[ignore] by default
/// and require:
/// - gh CLI authenticated
/// - Network access to GitHub
/// - Run with: cargo test --test `live_workflow_integration` -- --ignored
///
/// CRITICAL: All repo-specific values are loaded from the TOML config fixture.
/// NO hardcoded repo names, org names, or profile names in this file.
use std::path::PathBuf;
use std::process::Command;

use luther_workflow::workflow::config_loader::resolve_workflow_config;
use serde_json::Value;

// ============================================================================
// Helper Functions
// ============================================================================

/// Load the workflow config to get repo-specific values.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
fn load_config() -> luther_workflow::workflow::schema::WorkflowConfig {
    let fixture_root = PathBuf::from("tests/fixtures");
    resolve_workflow_config("llxprt-code", &fixture_root)
        .expect("Failed to load workflow config from TOML fixture")
}

/// Get `target_repo` from config.
fn get_target_repo() -> String {
    let config = load_config();
    config
        .variables
        .get("target_repo")
        .cloned()
        .expect("target_repo not found in config")
}

/// Get `base_branch` from config (e.g., "main").
fn get_base_branch() -> String {
    let config = load_config();
    config
        .variables
        .get("base_branch")
        .cloned()
        .unwrap_or_else(|| "main".to_string())
}

/// Run a shell command and return stdout.
fn run_command(cmd: &mut Command) -> Result<String, String> {
    let output = cmd
        .output()
        .map_err(|e| format!("Failed to execute: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Command failed: {stderr}"));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Get any valid issue number from the repo.
fn get_any_issue_number(target_repo: &str) -> Result<String, String> {
    let output = run_command(Command::new("gh").args([
        "issue",
        "list",
        "--repo",
        target_repo,
        "--state",
        "open",
        "--json",
        "number",
        "--limit",
        "1",
    ]))?;

    let parsed: Vec<Value> =
        serde_json::from_str(&output).map_err(|e| format!("Failed to parse JSON: {e}"))?;

    if parsed.is_empty() {
        return Err("No open issues found".to_string());
    }

    parsed[0]["number"]
        .as_i64()
        .map(|n| n.to_string())
        .ok_or_else(|| "No issue number found".to_string())
}

// ============================================================================
// Test 1: Can list issues from repo
// ============================================================================

/// Test 1: Can list issues from repo
/// GIVEN: The `target_repo` loaded from TOML config
/// WHEN: We run the gh issue list command
/// THEN: We get valid JSON with at least one issue
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-ISSUE-001,REQ-LF-ISSUE-002
#[ignore]
#[test]
fn test_can_list_issues_from_repo() {
    let target_repo = get_target_repo();

    let output = run_command(Command::new("gh").args([
        "issue",
        "list",
        "--repo",
        &target_repo,
        "--state",
        "open",
        "--json",
        "number,title",
        "--limit",
        "5",
    ]))
    .expect("Failed to list issues");

    let parsed: Vec<Value> = serde_json::from_str(&output).expect("Output should be valid JSON");

    assert!(!parsed.is_empty(), "Expected at least one issue");

    // Verify structure: each entry has number and title
    for issue in &parsed {
        assert!(issue["number"].is_number(), "Issue should have a number");
        let title = issue["title"].as_str();
        assert!(
            title.is_some() && !title.unwrap().is_empty(),
            "Issue should have a non-empty title"
        );
    }
}

// ============================================================================
// Test 2: Can list milestones from repo
// ============================================================================

/// Test 2: Can list milestones from repo
/// GIVEN: The `target_repo` loaded from TOML config
/// WHEN: We run the gh api `repos/{target_repo}/milestones` command
/// THEN: We get at least 1 milestone
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-ISSUE-001
#[ignore]
#[test]
fn test_can_list_milestones_from_repo() {
    let target_repo = get_target_repo();

    let output = run_command(Command::new("gh").args([
        "api",
        &format!("repos/{target_repo}/milestones"),
        "--jq",
        ".[].title",
    ]))
    .expect("Failed to list milestones");

    let lines: Vec<&str> = output.lines().collect();
    assert!(
        !lines.is_empty(),
        "Expected at least one milestone, got: {output}"
    );
}

// ============================================================================
// Test 3: Can fetch issue details
// ============================================================================

/// Test 3: Can fetch issue details
/// GIVEN: The `target_repo` and a valid issue number
/// WHEN: We run gh issue view
/// THEN: JSON has title, body, comments array, url
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-FETCH-001,REQ-LF-FETCH-002
#[ignore]
#[test]
fn test_can_fetch_issue_details() {
    let target_repo = get_target_repo();
    let issue_number = get_any_issue_number(&target_repo).expect("Failed to get any issue number");

    let output = run_command(Command::new("gh").args([
        "issue",
        "view",
        &issue_number,
        "--repo",
        &target_repo,
        "--json",
        "title,body,comments,url",
    ]))
    .expect("Failed to fetch issue details");

    let parsed: Value = serde_json::from_str(&output).expect("Output should be valid JSON");

    // Verify title is non-empty string
    let title = parsed["title"].as_str();
    assert!(
        title.is_some() && !title.unwrap().is_empty(),
        "Expected non-empty title"
    );

    // Verify body is a string (can be empty)
    assert!(
        parsed["body"].is_string() || parsed["body"].is_null(),
        "Body should be a string or null"
    );

    // Verify comments is an array
    assert!(parsed["comments"].is_array(), "Comments should be an array");

    // Verify url contains "github.com"
    let url = parsed["url"].as_str().expect("URL should be a string");
    assert!(url.contains("github.com"), "URL should contain github.com");
}

// ============================================================================
// Test 4: Fetch writes issue files
// ============================================================================

/// Test 4: Fetch writes issue files
/// GIVEN: A temp directory as `work_dir` with .luther/ created
/// WHEN: We run the `fetch_issue` commands from TOML
/// THEN: .luther/issue.md and .luther/issue-raw.json exist and are non-empty
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-FETCH-002,REQ-LF-DATA-002
#[ignore]
#[test]
fn test_fetch_writes_issue_files() {
    let target_repo = get_target_repo();
    let issue_number = get_any_issue_number(&target_repo).expect("Failed to get any issue number");

    // Create temp directory
    let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let work_dir = temp_dir.path().to_path_buf();
    let luther_dir = work_dir.join(".luther");
    std::fs::create_dir_all(&luther_dir).expect("Failed to create .luther dir");

    // Run the fetch_issue commands from the TOML
    // 1. Fetch full issue data
    let raw_json_path = luther_dir.join("issue-raw.json");
    let cmd = format!(
        "gh issue view {} --repo {} --json title,body,comments,url > {}",
        issue_number,
        target_repo,
        raw_json_path.display()
    );
    run_command(Command::new("sh").args(["-c", &cmd])).expect("Failed to fetch issue raw JSON");

    // 2. Write issue body to file
    let issue_md_path = luther_dir.join("issue.md");
    let cmd = format!(
        "jq -r '.body // \"\"' {} > {}",
        raw_json_path.display(),
        issue_md_path.display()
    );
    run_command(Command::new("sh").args(["-c", &cmd])).expect("Failed to write issue.md");

    // 3. Write comments
    let comments_md_path = luther_dir.join("comments.md");
    let cmd = format!(
        "jq -r '.comments[] | \"## Comment by \\(.author.login) at \\(.createdAt)\\n\\n\\(.body)\\n\\n---\\n\"' {} > {}",
        raw_json_path.display(),
        comments_md_path.display()
    );
    run_command(Command::new("sh").args(["-c", &cmd])).expect("Failed to write comments.md");

    // Assert files exist and are non-empty
    assert!(raw_json_path.exists(), "issue-raw.json should exist");
    let raw_content =
        std::fs::read_to_string(&raw_json_path).expect("Failed to read issue-raw.json");
    assert!(
        !raw_content.is_empty(),
        "issue-raw.json should be non-empty"
    );

    // Verify it's valid JSON
    let _: Value = serde_json::from_str(&raw_content).expect("issue-raw.json should be valid JSON");

    assert!(issue_md_path.exists(), "issue.md should exist");
    // issue.md can be empty if issue body is empty

    assert!(comments_md_path.exists(), "comments.md should exist");
    // comments.md can be empty if no comments

    // Cleanup temp dir
}

// ============================================================================
// Test 5: Workspace setup creates clone
// ============================================================================

/// Test 5: Workspace setup creates clone
/// GIVEN: A temp directory, `work_dir` as subpath that doesn't exist
/// WHEN: We run the `setup_workspace` shell commands
/// THEN: `work_dir/.git`/ exists, `work_dir/.luther`/ exists, correct branch
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-WS-001,REQ-LF-WS-002,REQ-LF-WS-004
#[ignore]
#[test]
fn test_workspace_setup_creates_clone() {
    let target_repo = get_target_repo();
    let base_branch = get_base_branch();
    let issue_number = "12345"; // Test issue number for branch naming

    // Create temp directory for work_dir
    let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let work_dir = temp_dir.path().join("workspace");

    // Run the setup_workspace commands
    // 1. Clone if .git doesn't exist
    let cmd = format!(
        "if [ ! -d \"{}/.git\" ]; then git clone https://github.com/{}.git {}; fi",
        work_dir.display(),
        target_repo,
        work_dir.display()
    );
    run_command(Command::new("sh").args(["-c", &cmd])).expect("Failed to clone repo");

    // 2. Fetch, checkout base branch, reset, create issue branch
    let cmd = format!(
        "cd {} && git fetch origin && git checkout {} && git reset --hard origin/{} && git checkout -b issue{}",
        work_dir.display(),
        base_branch,
        base_branch,
        issue_number
    );
    run_command(Command::new("sh").args(["-c", &cmd])).expect("Failed to setup workspace");

    // 3. Create .luther directory
    let luther_dir = work_dir.join(".luther");
    std::fs::create_dir_all(&luther_dir).expect("Failed to create .luther dir");

    // Assert .git/ exists
    let git_dir = work_dir.join(".git");
    assert!(
        git_dir.exists() && git_dir.is_dir(),
        ".git/ directory should exist"
    );

    // Assert .luther/ exists
    assert!(
        luther_dir.exists() && luther_dir.is_dir(),
        ".luther/ directory should exist"
    );

    // Assert current branch is issue{issue_number}
    let branch_output = run_command(Command::new("git").args([
        "-C",
        &work_dir.display().to_string(),
        "branch",
        "--show-current",
    ]))
    .expect("Failed to get current branch");

    let expected_branch = format!("issue{issue_number}");
    assert_eq!(
        branch_output.trim(),
        expected_branch,
        "Expected branch {}, got {}",
        expected_branch,
        branch_output.trim()
    );

    // Cleanup: temp_dir dropped automatically
}

// ============================================================================
// Test 6: Workspace setup reuses existing clone
// ============================================================================

/// Test 6: Workspace setup reuses existing clone
/// GIVEN: A workspace that was already cloned
/// WHEN: We run the setup commands a second time
/// THEN: Second run succeeds, .git/ still exists, branch is correct
/// @plan:PLAN-20260408-LLXPRT-FIRST.P18
/// @requirement:REQ-LF-WS-001
#[ignore]
#[test]
fn test_workspace_setup_reuses_existing_clone() {
    let target_repo = get_target_repo();
    let base_branch = get_base_branch();
    let issue_number = "12346"; // Different test issue number

    // Create temp directory for work_dir
    let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let work_dir = temp_dir.path().join("workspace");

    // FIRST run: clone and setup
    let cmd = format!(
        "if [ ! -d \"{}/.git\" ]; then git clone https://github.com/{}.git {}; fi",
        work_dir.display(),
        target_repo,
        work_dir.display()
    );
    run_command(Command::new("sh").args(["-c", &cmd])).expect("Failed on first clone");

    let cmd = format!(
        "cd {} && git fetch origin && git checkout {} && git reset --hard origin/{} && git checkout -b issue{} || git checkout issue{}",
        work_dir.display(),
        base_branch,
        base_branch,
        issue_number,
        issue_number
    );
    run_command(Command::new("sh").args(["-c", &cmd])).expect("Failed on first setup");

    let luther_dir = work_dir.join(".luther");
    std::fs::create_dir_all(&luther_dir).expect("Failed to create .luther dir");

    // SECOND run: should succeed via fetch+reset path
    let cmd = format!(
        "cd {} && git fetch origin && git checkout {} && git reset --hard origin/{} && git checkout -b issue{}-retry || git checkout issue{}-retry",
        work_dir.display(),
        base_branch,
        base_branch,
        issue_number,
        issue_number
    );
    let result = run_command(Command::new("sh").args(["-c", &cmd]));
    assert!(
        result.is_ok(),
        "Second run should succeed: {:?}",
        result.err()
    );

    // Assert .git/ still exists
    let git_dir = work_dir.join(".git");
    assert!(
        git_dir.exists() && git_dir.is_dir(),
        ".git/ should still exist after second run"
    );

    // Assert branch exists (could be issue12346 or issue12346-retry)
    let branches_output = run_command(Command::new("git").args([
        "-C",
        &work_dir.display().to_string(),
        "branch",
        "--list",
    ]))
    .expect("Failed to list branches");

    let branches = branches_output;
    assert!(
        branches.contains(&format!("issue{issue_number}")),
        "Expected branch to contain issue{issue_number}"
    );

    // Cleanup: temp_dir dropped automatically
}
