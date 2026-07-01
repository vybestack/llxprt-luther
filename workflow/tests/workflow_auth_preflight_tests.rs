use std::fs;
use std::process::Command;

use luther_workflow::adapters::workflow_auth_preflight::{
    build_report, classify_push_rejection, classify_remote_url, detect_workflow_paths,
    extract_workflow_paths_from_text, matches_workflow_pattern, parse_porcelain_paths,
    WorkflowAuthOutcome, WorkflowAuthPreflightConfig,
};
use luther_workflow::engine::executor::{StepContext, StepExecutor};
use luther_workflow::engine::executors::WorkflowAuthPreflightExecutor;
use luther_workflow::engine::transition::StepOutcome;
use serde_json::json;
use tempfile::TempDir;

#[test]
fn detects_workflow_paths_from_text_and_status() {
    let patterns = vec![".github/workflows/**".to_string()];
    let from_text = extract_workflow_paths_from_text(
        "Update .github/workflows/pr-quality.yml and src/lib.rs",
        "plan.md",
        &patterns,
    );
    assert_eq!(from_text.len(), 1);
    assert_eq!(from_text[0].path, ".github/workflows/pr-quality.yml");
    assert_eq!(from_text[0].source, "plan.md");

    let status = b" M .github/workflows/ci.yml\0?? src/lib.rs\0";
    let paths = parse_porcelain_paths(status);
    let detected = detect_workflow_paths(paths, "git_status", &patterns);
    assert_eq!(detected.len(), 1);
    assert_eq!(detected[0].path, ".github/workflows/ci.yml");

    let duplicate_detected = detect_workflow_paths(
        [".github/workflows/ci.yml", ".github/workflows/ci.yml"],
        "git_status",
        &patterns,
    );
    assert_eq!(duplicate_detected.len(), 1);
}

#[test]
fn parses_renamed_workflow_paths_from_porcelain_status() {
    let paths = parse_porcelain_paths(
        b"R100 .github/workflows/pr-quality.yml\0.github/workflows/old.yml\0",
    );

    assert_eq!(
        paths,
        vec![
            ".github/workflows/pr-quality.yml",
            ".github/workflows/old.yml"
        ]
    );
}

#[test]
fn classifies_remote_auth_modes_and_push_rejection() {
    assert_eq!(classify_remote_url("git@github.com:owner/repo.git"), "ssh");
    assert_eq!(
        classify_remote_url("ssh://git@github.com/owner/repo.git"),
        "ssh"
    );
    assert_eq!(
        classify_remote_url("git@github.com.evil.test:owner/repo.git"),
        "unknown"
    );
    assert_eq!(
        classify_remote_url("https://github.com/owner/repo.git"),
        "https_oauth"
    );
    assert_eq!(
        classify_remote_url("https://x-access-token:secret@github.com/owner/repo.git"),
        "https_oauth"
    );
    assert_eq!(
        classify_remote_url("https://example.test/repo.git"),
        "unknown_https"
    );
    assert_eq!(
        classify_remote_url("https://github.com.evil.test/owner/repo.git"),
        "unknown_https"
    );
    assert_eq!(classify_remote_url("file:///tmp/repo"), "unknown");
    assert!(classify_push_rejection(
        "refusing to allow an OAuth App to create or update workflow without workflow scope"
    )
    .is_some());
    assert!(classify_push_rejection("the workflow scope is not applicable here").is_none());
}

#[test]
fn matches_custom_single_segment_workflow_globs() {
    assert!(matches_workflow_pattern(
        ".github/workflows/ci.yml",
        ".github/workflows/*.yml"
    ));
    assert!(!matches_workflow_pattern(
        ".github/workflows/nested/ci.yml",
        ".github/workflows/*.yml"
    ));
    assert!(!matches_workflow_pattern(
        ".github/workflows-backdoor/ci.yml",
        ".github/workflows/**"
    ));
}

#[test]
fn report_blocks_https_oauth_when_workflow_scope_missing() {
    let report = build_report(
        "https_oauth".to_string(),
        WorkflowAuthPreflightConfig::default(),
        detect_workflow_paths(
            [".github/workflows/pr-quality.yml"],
            "git_status",
            &[".github/workflows/**".to_string()],
        ),
        vec!["repo".to_string()],
    );

    assert_eq!(report.outcome, WorkflowAuthOutcome::Fatal);
    assert_eq!(report.auth_method, "https_oauth");
    assert!(report
        .missing_capability
        .as_deref()
        .unwrap_or_default()
        .contains("workflow"));
    assert!(report
        .recommended_operator_action
        .as_deref()
        .unwrap_or_default()
        .contains("SSH"));
}

#[test]
fn report_blocks_https_oauth_when_scopes_are_empty() {
    let report = build_report(
        "https_oauth".to_string(),
        WorkflowAuthPreflightConfig::default(),
        detect_workflow_paths(
            [".github/workflows/pr-quality.yml"],
            "git_status",
            &[".github/workflows/**".to_string()],
        ),
        Vec::new(),
    );

    assert_eq!(report.outcome, WorkflowAuthOutcome::Fatal);
}

#[test]
fn report_blocks_unknown_auth_for_workflow_paths() {
    let report = build_report(
        "custom_unrecognized".to_string(),
        WorkflowAuthPreflightConfig::default(),
        detect_workflow_paths(
            [".github/workflows/pr-quality.yml"],
            "git_status",
            &[".github/workflows/**".to_string()],
        ),
        Vec::new(),
    );

    assert_eq!(report.outcome, WorkflowAuthOutcome::Fatal);
    assert_eq!(report.auth_method, "custom_unrecognized");
    assert!(report
        .missing_capability
        .as_deref()
        .unwrap_or_default()
        .contains("unable to prove credentials"));
}

#[test]
fn report_allows_ssh_for_workflow_paths() {
    let report = build_report(
        "ssh".to_string(),
        WorkflowAuthPreflightConfig::default(),
        detect_workflow_paths(
            [".github/workflows/pr-quality.yml"],
            "git_status",
            &[".github/workflows/**".to_string()],
        ),
        Vec::new(),
    );

    assert_eq!(report.outcome, WorkflowAuthOutcome::Pass);
    assert!(report.missing_capability.is_none());
}

#[test]
fn report_allows_https_oauth_when_required_scope_is_present() {
    let report = build_report(
        "https_oauth".to_string(),
        WorkflowAuthPreflightConfig::default(),
        detect_workflow_paths(
            [".github/workflows/pr-quality.yml"],
            "git_status",
            &[".github/workflows/**".to_string()],
        ),
        vec!["repo".to_string(), "workflow".to_string()],
    );

    assert_eq!(report.outcome, WorkflowAuthOutcome::Pass);
    assert!(report.missing_capability.is_none());
}

#[test]
fn report_allows_missing_scopes_when_no_workflow_paths_are_detected() {
    let report = build_report(
        "https_oauth".to_string(),
        WorkflowAuthPreflightConfig::default(),
        Vec::new(),
        Vec::new(),
    );

    assert_eq!(report.outcome, WorkflowAuthOutcome::Pass);
    assert!(report.missing_capability.is_none());
}

#[test]
fn report_matches_required_scopes_case_insensitively() {
    let report = build_report(
        "https_oauth".to_string(),
        WorkflowAuthPreflightConfig {
            workflow_path_patterns: vec![".github/workflows/**".to_string()],
            required_scopes: vec!["Workflow".to_string()],
        },
        detect_workflow_paths(
            [".github/workflows/pr-quality.yml"],
            "git_status",
            &[".github/workflows/**".to_string()],
        ),
        vec![" workflow ".to_string()],
    );

    assert_eq!(report.outcome, WorkflowAuthOutcome::Pass);
    assert!(report.missing_capability.is_none());
}

#[cfg(unix)]
#[test]
fn executor_writes_fatal_artifact_for_missing_workflow_scope() {
    let temp = TempDir::new().expect("tempdir");
    init_git_repo(temp.path());
    fs::write(
        temp.path().join(".github/workflows/pr-quality.yml"),
        "name: pr\n",
    )
    .expect("write workflow");
    let artifact_dir = temp.path().join("artifacts");
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir).expect("bin dir");
    let gh_path = write_fake_gh(&bin_dir);

    let mut context = StepContext::new(temp.path().to_path_buf(), "run-1".to_string());
    let params = json!({
        "artifact_dir": artifact_dir,
        "auth_method": "https_oauth",
        "gh_path": gh_path,
        "include_git_status": true,
        "text_artifacts": [],
        "workflow_path_patterns": [".github/workflows/**"],
        "required_scopes": ["workflow"]
    });
    let outcome = WorkflowAuthPreflightExecutor
        .execute(&mut context, &params)
        .expect("executor runs");

    assert_eq!(outcome, StepOutcome::Fatal);
    let report = fs::read_to_string(artifact_dir.join("workflow-auth-preflight.json"))
        .expect("report exists");
    assert!(report.contains("https_oauth"));
    assert!(report.contains(".github/workflows/pr-quality.yml"));
    assert!(report.contains("missing required OAuth scope"));

    let mut success_context = StepContext::new(temp.path().to_path_buf(), "run-1".to_string());
    let success_params = json!({
        "artifact_dir": artifact_dir,
        "auth_method": "ssh",
        "include_git_status": true,
        "text_artifacts": [],
        "workflow_path_patterns": [".github/workflows/**"],
        "required_scopes": ["workflow"]
    });
    let success_outcome = WorkflowAuthPreflightExecutor
        .execute(&mut success_context, &success_params)
        .expect("executor runs");
    assert_eq!(success_outcome, StepOutcome::Success);
}

#[cfg(unix)]
#[test]
fn executor_writes_fatal_artifact_when_auth_discovery_fails() {
    let temp = TempDir::new().expect("tempdir");
    init_git_repo(temp.path());
    fs::write(
        temp.path().join(".github/workflows/pr-quality.yml"),
        "name: pr
",
    )
    .expect("write workflow");
    let artifact_dir = temp.path().join("artifacts");
    let mut context = StepContext::new(temp.path().to_path_buf(), "run-1".to_string());
    let params = json!({
        "artifact_dir": artifact_dir,
        "remote_name": "../origin",
        "include_git_status": true,
        "text_artifacts": [],
        "workflow_path_patterns": [".github/workflows/**"],
        "required_scopes": ["workflow"]
    });

    let outcome = WorkflowAuthPreflightExecutor
        .execute(&mut context, &params)
        .expect("executor writes fatal report instead of aborting");

    assert_eq!(outcome, StepOutcome::Fatal);
    let report = fs::read_to_string(artifact_dir.join("workflow-auth-preflight.json"))
        .expect("report exists");
    assert!(report.contains("unknown"));
    assert!(report.contains("unable to prove credentials"));
}

#[cfg(unix)]
fn init_git_repo(path: &std::path::Path) {
    fs::create_dir_all(path.join(".github/workflows")).expect("workflow dir");
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "luther@example.test"]);
    run_git(path, &["config", "user.name", "Luther Test"]);
    run_git(
        path,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/owner/repo.git",
        ],
    );
}

#[cfg(unix)]
fn run_git(path: &std::path::Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .expect("git command spawns");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
fn write_fake_gh(bin_dir: &std::path::Path) -> std::path::PathBuf {
    let gh = bin_dir.join("gh");
    fs::write(
        &gh,
        "#!/bin/sh\necho \"Logged in to github.com\" >&2\necho \"  - Token scopes: 'repo'\" >&2\n",
    )
    .expect("write fake gh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&gh).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&gh, perms).expect("chmod");
    }
    gh
}
