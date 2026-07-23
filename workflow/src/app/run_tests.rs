use super::daemon_run::*;
use super::*;
use luther_workflow::cli::RunArgs;
use luther_workflow::workflow::schema::{WorkflowConfig, WorkflowType};

fn run_args() -> RunArgs {
    RunArgs {
        config: None,
        dry_run: false,
        skip_preflight: false,
        workflow_type: None,
        config_dir: None,
        run_id: None,
        repo: None,
        issue: None,
        work_dir: None,
        artifact_dir: None,
    }
}

fn workflow_type_from_json(steps: serde_json::Value) -> WorkflowType {
    serde_json::from_value(serde_json::json!({
        "workflow_type_id": "wf-test",
        "steps": steps,
    }))
    .expect("workflow type deserializes")
}

fn config_with_variables(vars: serde_json::Value) -> WorkflowConfig {
    serde_json::from_value(serde_json::json!({
        "config_id": "cfg-test",
        "workflow_type_id": "wf-test",
        "runtime": {"timeout_seconds": 60, "max_retries": 1},
        "repository": {"workspace_strategy": "reuse", "branch_template": "wf/{run_id}"},
        "guards": {},
        "variables": vars,
    }))
    .expect("workflow config deserializes")
}

#[test]
fn run_config_root_defaults_to_config_dir() {
    let args = run_args();
    assert_eq!(run_config_root(&args), std::path::PathBuf::from("config"));
}

#[test]
fn run_config_root_honors_explicit_config_dir() {
    let mut args = run_args();
    args.config_dir = Some(std::path::PathBuf::from("/custom/config"));
    assert_eq!(
        run_config_root(&args),
        std::path::PathBuf::from("/custom/config")
    );
}

#[test]
fn workflow_requires_github_detects_github_step_type() {
    let wt = workflow_type_from_json(serde_json::json!([
        {"step_id": "s1", "step_type": "github_create_pr"}
    ]));
    assert!(workflow_requires_github(&wt));
}

#[test]
fn workflow_requires_github_detects_gh_command_token() {
    let wt = workflow_type_from_json(serde_json::json!([
        {"step_id": "s1", "step_type": "shell", "parameters": {"command": "gh pr list"}}
    ]));
    assert!(workflow_requires_github(&wt));
}

#[test]
fn workflow_requires_github_false_for_offline_shell() {
    let wt = workflow_type_from_json(serde_json::json!([
        {"step_id": "s1", "step_type": "shell", "parameters": {"command": "echo hi"}}
    ]));
    assert!(!workflow_requires_github(&wt));
}

#[test]
fn workflow_requires_llxprt_true_for_spawning_step() {
    let wt = workflow_type_from_json(serde_json::json!([
        {"step_id": "s1", "step_type": "llxprt", "parameters": {"prompt": "do it"}}
    ]));
    assert!(workflow_requires_llxprt(&wt));
}

#[test]
fn workflow_requires_llxprt_false_for_static_content() {
    let wt = workflow_type_from_json(serde_json::json!([
        {"step_id": "s1", "step_type": "llxprt", "parameters": {"static_content": "hello"}}
    ]));
    assert!(!workflow_requires_llxprt(&wt));
}

#[test]
fn workflow_requires_llxprt_false_when_no_llxprt_steps() {
    let wt = workflow_type_from_json(serde_json::json!([
        {"step_id": "s1", "step_type": "shell", "parameters": {"command": "echo hi"}}
    ]));
    assert!(!workflow_requires_llxprt(&wt));
}

#[test]
fn report_dry_run_validation_clean_workflow_reports_no_errors() {
    let wt = workflow_type_from_json(serde_json::json!([
        {"step_id": "s1", "step_type": "shell", "parameters": {"command": "echo hi"}}
    ]));
    let config = config_with_variables(serde_json::json!({}));
    assert!(!report_dry_run_validation(&wt, &config));
}

#[test]
fn build_run_context_reads_variables() {
    let config = config_with_variables(serde_json::json!({
        "target_repo": "owner/repo",
        "primary_issue_number": "125",
        "work_dir": "/tmp/ws",
        "artifact_dir": "/tmp/art",
    }));
    let ctx = build_run_context(&config, "run-ctx");
    assert_eq!(ctx.repository.as_deref(), Some("owner/repo"));
    assert_eq!(ctx.issue_number, Some(125));
    assert_eq!(ctx.workspace_path.as_deref(), Some("/tmp/ws"));
    assert_eq!(ctx.artifact_root.as_deref(), Some("/tmp/art"));
    assert!(ctx
        .log_path
        .as_deref()
        .is_some_and(|p| p.ends_with("run-ctx.log")));
    assert!(ctx.pr_number.is_none());
    assert!(ctx.head_sha.is_none());
}

#[test]
fn build_run_context_falls_back_to_issue_number_variable() {
    let config = config_with_variables(serde_json::json!({
        "issue_number": "77",
    }));
    let ctx = build_run_context(&config, "run-fallback");
    assert_eq!(ctx.issue_number, Some(77));
    // No explicit work_dir/artifact_dir: defaults are derived, not empty.
    assert!(ctx.workspace_path.is_some());
    assert!(ctx.artifact_root.is_some());
    assert!(ctx.repository.is_none());
}

#[test]
fn build_run_context_ignores_non_numeric_issue() {
    let config = config_with_variables(serde_json::json!({
        "primary_issue_number": "not-a-number",
    }));
    let ctx = build_run_context(&config, "run-bad-issue");
    assert!(ctx.issue_number.is_none());
}

#[test]
fn ensure_daemon_run_dir_none_path_is_ok() {
    assert!(ensure_daemon_run_dir("work", None).is_ok());
}

#[test]
fn ensure_daemon_run_dir_creates_directory() {
    let base = std::env::temp_dir().join(format!(
        "run-ensure-dir-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let nested = base.join("nested/child");
    assert!(ensure_daemon_run_dir("artifact", Some(&nested)).is_ok());
    assert!(nested.is_dir());
    let _ = std::fs::remove_dir_all(&base);
}

fn daemon_launch_request(
    base: &std::path::Path,
    run_id: &str,
) -> luther_workflow::daemon::launcher::LaunchRequest {
    luther_workflow::daemon::launcher::LaunchRequest {
        config_id: "llxprt-luther".to_string(),
        workflow_type_id: Some("llxprt-luther-dogfood-v1".to_string()),
        run_id: run_id.to_string(),
        repo: "vybestack/llxprt-luther".to_string(),
        issue_number: 150,
        daemon_managed_claim: true,
        claim_assignment_added: true,
        claim_label_added: true,
        work_dir: Some(base.join("work")),
        artifact_dir: Some(base.join("artifacts")),
        config_root: std::path::PathBuf::from("config"),
    }
}

fn unique_daemon_test_dir(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "{name}-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ))
}

#[test]
fn ensure_daemon_run_dirs_creates_owned_marker_workspace() {
    let base = unique_daemon_test_dir("daemon-owned-workspace");
    let request = daemon_launch_request(&base, "run-owned");
    ensure_daemon_run_dirs(&request).expect("prepare owned daemon workspace");
    let work = request.work_dir.as_deref().unwrap();
    assert_eq!(
        std::fs::read_to_string(work.join(".luther/workspace-owner")).unwrap(),
        "run-owned"
    );
    assert!(!work.join(".git").exists());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn ensure_daemon_run_dirs_rejects_preexisting_empty_workspace() {
    // Issue 158 finding 3: no auto-adopt. A pre-existing empty workspace
    // directory created by some other actor must NOT be claimed by this
    // launch, because it carries no provenance tying it to this run. Only an
    // atomically created-by-this-launch directory, or a directory with exact
    // interrupted-publication evidence (`.luther` with same-run temp files),
    // may be first-claimed.
    let base = unique_daemon_test_dir("daemon-empty-workspace");
    let request = daemon_launch_request(&base, "run-owned");
    let work = request.work_dir.as_deref().unwrap();
    std::fs::create_dir_all(work).unwrap();
    let error = ensure_daemon_run_dirs(&request).unwrap_err();
    assert!(
        error.contains("refusing to adopt pre-existing workspace")
            || error.contains("without ownership marker"),
        "expected rejection of pre-existing empty workspace, got: {error}"
    );
    assert!(!work.join(".luther/workspace-owner").exists());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn ensure_daemon_run_dirs_claims_interrupted_publication_evidence() {
    // Issue 158 finding 3: a workspace carrying exact interrupted-publication
    // evidence (`.luther` dir containing only a same-run temp marker file)
    // IS claimable, because it proves a prior attempt by THIS launch was
    // interrupted. This is the recovery path for a crashed provision.
    let base = unique_daemon_test_dir("daemon-interrupted-pub");
    let request = daemon_launch_request(&base, "run-owned");
    let work = request.work_dir.as_deref().unwrap();
    std::fs::create_dir_all(work.join(".luther")).unwrap();
    std::fs::write(
        work.join(".luther/.workspace-owner.tmp.interrupted"),
        "run-owned",
    )
    .unwrap();
    ensure_daemon_run_dirs(&request).expect("interrupted-publication evidence allows claim");
    assert_eq!(
        std::fs::read_to_string(work.join(".luther/workspace-owner")).unwrap(),
        "run-owned"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn ensure_daemon_run_dirs_rejects_foreign_temp_publication_evidence() {
    // Issue 158 finding 3: a `.luther` dir whose temp marker belongs to a
    // DIFFERENT run must NOT be claimable by this run.
    let base = unique_daemon_test_dir("daemon-foreign-temp");
    let request = daemon_launch_request(&base, "run-owned");
    let work = request.work_dir.as_deref().unwrap();
    std::fs::create_dir_all(work.join(".luther")).unwrap();
    std::fs::write(
        work.join(".luther/.workspace-owner.tmp.interrupted"),
        "run-foreign",
    )
    .unwrap();
    let error = ensure_daemon_run_dirs(&request).unwrap_err();
    assert!(
        error.contains("refusing to adopt pre-existing workspace")
            || error.contains("without ownership marker"),
        "expected rejection for foreign temp evidence, got: {error}"
    );
    assert!(!work.join(".luther/workspace-owner").exists());
    let _ = std::fs::remove_dir_all(&base);
}

// ---------------------------------------------------------------------------
// Resume workspace mismatch rejection (task 4 ordering invariant)
// ---------------------------------------------------------------------------

#[test]
fn resolve_resume_workspace_path_rejects_mismatch() {
    // A resume whose request work_dir differs from the persisted workspace_path
    // must be rejected before any ownership evidence is written.
    let persisted = "/tmp/luther-persisted-workspace";
    let mut request = daemon_launch_request(std::path::Path::new("/tmp/luther-base"), "run-A");
    request.work_dir = Some(std::path::PathBuf::from("/tmp/luther-other-workspace"));
    let err = resolve_resume_workspace_path(persisted, &request).unwrap_err();
    assert!(
        err.contains("mismatch"),
        "expected mismatch rejection, got: {err}"
    );
}

#[test]
fn resolve_resume_workspace_path_accepts_match() {
    let persisted = "/tmp/luther-persisted-workspace";
    let mut request = daemon_launch_request(std::path::Path::new("/tmp/luther-base"), "run-A");
    request.work_dir = Some(std::path::PathBuf::from(persisted));
    let resolved = resolve_resume_workspace_path(persisted, &request).unwrap();
    assert_eq!(resolved, std::path::Path::new(persisted));
}

#[test]
fn resolve_resume_workspace_path_accepts_none_request_work_dir() {
    // When the request carries no explicit work_dir, the persisted path is
    // trusted (the mismatch check only applies when both are present).
    let persisted = "/tmp/luther-persisted-workspace";
    let mut request = daemon_launch_request(std::path::Path::new("/tmp/luther-base"), "run-A");
    request.work_dir = None;
    let resolved = resolve_resume_workspace_path(persisted, &request).unwrap();
    assert_eq!(resolved, std::path::Path::new(persisted));
}

// ---------------------------------------------------------------------------
// Bootstrap-only pre-Git resume: ensure_durable succeeds without .git (task 4)
// ---------------------------------------------------------------------------

#[test]
fn ensure_durable_succeeds_for_bootstrap_only_pre_git_resume() {
    // A workspace that was provisioned (bootstrap marker) but never reached
    // `git init` (interrupted before workspace_ownership step) must resume
    // successfully: ensure_durable is a no-op when .git is absent.
    let base = unique_daemon_test_dir("resume-pre-git");
    let work = base.join("work");
    std::fs::create_dir_all(work.join(".luther")).unwrap();
    std::fs::write(work.join(".luther/workspace-owner"), "run-pre-git").unwrap();
    assert!(!work.join(".git").exists());
    luther_workflow::engine::workspace_ownership::ensure_durable_workspace_ownership(
        &work,
        "run-pre-git",
    )
    .expect("bootstrap-only pre-Git resume must succeed");
    // No durable evidence was created.
    assert!(!work.join(".git/luther/workspace-owner").exists());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn ensure_daemon_run_dirs_rejects_foreign_nonempty_workspace() {
    let base = unique_daemon_test_dir("daemon-foreign-repo");
    let request = daemon_launch_request(&base, "run-owned");
    let work = request.work_dir.as_deref().unwrap();
    std::fs::create_dir_all(work).unwrap();
    std::fs::write(work.join("foreign.txt"), "not Luther-owned").unwrap();
    let error = ensure_daemon_run_dirs(&request).unwrap_err();
    assert!(error.contains("without ownership marker"), "{error}");
    assert!(!work.join(".luther/workspace-owner").exists());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn ensure_daemon_run_dirs_rejects_mismatched_owner() {
    let base = unique_daemon_test_dir("daemon-owner-mismatch");
    let request = daemon_launch_request(&base, "run-expected");
    let work = request.work_dir.as_deref().unwrap();
    std::fs::create_dir_all(work.join(".luther")).unwrap();
    std::fs::write(work.join(".luther/workspace-owner"), "run-foreign").unwrap();
    let error = ensure_daemon_run_dirs(&request).unwrap_err();
    assert!(error.contains("run-foreign"), "{error}");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn ensure_daemon_run_dirs_reuses_exact_owned_workspace() {
    let base = unique_daemon_test_dir("daemon-owned-reuse");
    let request = daemon_launch_request(&base, "run-owned");
    ensure_daemon_run_dirs(&request).unwrap();
    ensure_daemon_run_dirs(&request).expect("same owner can reuse workspace");
    let work = request.work_dir.as_deref().unwrap();
    assert_eq!(
        std::fs::read_to_string(work.join(".luther/workspace-owner")).unwrap(),
        "run-owned"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn ensure_daemon_run_dirs_recovers_marker_publication_temp() {
    let base = unique_daemon_test_dir("daemon-owner-temp");
    let request = daemon_launch_request(&base, "run-owned");
    let work = request.work_dir.as_deref().unwrap();
    std::fs::create_dir_all(work.join(".luther")).unwrap();
    std::fs::write(
        work.join(".luther/.workspace-owner.tmp.interrupted"),
        "run-owned",
    )
    .unwrap();
    ensure_daemon_run_dirs(&request).expect("recover marker publication temp");
    assert_eq!(
        std::fs::read_to_string(work.join(".luther/workspace-owner")).unwrap(),
        "run-owned"
    );
    let _ = std::fs::remove_dir_all(&base);
}

// ---------------------------------------------------------------------------
// validate_resume_identity_against_metadata: all persisted identity fields
// must match before any ownership promotion or mutation (issue 158).
// ---------------------------------------------------------------------------

fn resume_metadata_for_request(
    request: &luther_workflow::daemon::launcher::LaunchRequest,
) -> luther_workflow::persistence::RunMetadata {
    let mut metadata = luther_workflow::persistence::RunMetadata::new(
        request.run_id.clone(),
        request.workflow_type_id.clone().unwrap_or_default(),
        request.config_id.clone(),
    );
    metadata.repository = Some(request.repo.clone());
    metadata.issue_number = Some(i64::try_from(request.issue_number).unwrap_or(0));
    if let Some(work_dir) = request.work_dir.as_deref() {
        metadata.workspace_path = Some(work_dir.to_string_lossy().to_string());
    }
    if let Some(artifact_dir) = request.artifact_dir.as_deref() {
        metadata.artifact_root = Some(artifact_dir.to_string_lossy().to_string());
    }
    metadata
}

#[test]
fn validate_resume_identity_accepts_matching_request() {
    let base = unique_daemon_test_dir("resume-identity-match");
    let request = daemon_launch_request(&base, "run-match");
    let metadata = resume_metadata_for_request(&request);
    validate_resume_identity_against_metadata(&request, &metadata)
        .expect("matching request identity must be accepted");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn validate_resume_identity_rejects_config_id_mismatch() {
    let base = unique_daemon_test_dir("resume-config-id-mismatch");
    let request = daemon_launch_request(&base, "run-config");
    let mut metadata = resume_metadata_for_request(&request);
    metadata.config_id = "different-config".to_string();
    let err = validate_resume_identity_against_metadata(&request, &metadata).unwrap_err();
    assert!(
        err.contains("config_id") && err.contains("different-config"),
        "expected config_id mismatch rejection, got: {err}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn validate_resume_identity_rejects_workflow_type_id_mismatch() {
    let base = unique_daemon_test_dir("resume-wf-type-mismatch");
    let request = daemon_launch_request(&base, "run-wf");
    let mut metadata = resume_metadata_for_request(&request);
    metadata.workflow_type_id = "different-workflow-type".to_string();
    let err = validate_resume_identity_against_metadata(&request, &metadata).unwrap_err();
    assert!(
        err.contains("workflow_type_id") && err.contains("different-workflow-type"),
        "expected workflow_type_id mismatch rejection, got: {err}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn validate_resume_identity_rejects_artifact_dir_mismatch() {
    let base = unique_daemon_test_dir("resume-artifact-mismatch");
    let request = daemon_launch_request(&base, "run-artifact");
    let mut metadata = resume_metadata_for_request(&request);
    metadata.artifact_root = Some("/tmp/different-artifacts".to_string());
    let err = validate_resume_identity_against_metadata(&request, &metadata).unwrap_err();
    assert!(
        err.contains("artifact_dir") || err.contains("artifact_root"),
        "expected artifact mismatch rejection, got: {err}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn validate_resume_identity_rejects_run_id_mismatch() {
    let base = unique_daemon_test_dir("resume-run-id-mismatch");
    let request = daemon_launch_request(&base, "run-expected");
    let mut metadata = resume_metadata_for_request(&request);
    metadata.run_id = "run-different".to_string();
    let err = validate_resume_identity_against_metadata(&request, &metadata).unwrap_err();
    assert!(
        err.contains("run_id") && err.contains("run-different"),
        "expected run_id mismatch rejection, got: {err}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn validate_resume_identity_rejects_repo_mismatch() {
    let base = unique_daemon_test_dir("resume-repo-mismatch");
    let request = daemon_launch_request(&base, "run-repo");
    let mut metadata = resume_metadata_for_request(&request);
    metadata.repository = Some("different/repo".to_string());
    let err = validate_resume_identity_against_metadata(&request, &metadata).unwrap_err();
    assert!(
        err.contains("repo"),
        "expected repo mismatch rejection, got: {err}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn validate_resume_identity_rejects_issue_number_mismatch() {
    let base = unique_daemon_test_dir("resume-issue-mismatch");
    let request = daemon_launch_request(&base, "run-issue");
    let mut metadata = resume_metadata_for_request(&request);
    metadata.issue_number = Some(999);
    let err = validate_resume_identity_against_metadata(&request, &metadata).unwrap_err();
    assert!(
        err.contains("issue_number"),
        "expected issue_number mismatch rejection, got: {err}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

// ---------------------------------------------------------------------------
// Issue 158 finding 3: exact typed Option<PathBuf> artifact comparison
// ---------------------------------------------------------------------------

#[test]
fn validate_resume_identity_rejects_artifact_dir_mismatch_exact_typed() {
    // The artifact_dir comparison must be an exact typed Option<PathBuf>
    // comparison, not a lossy to_str()/str comparison. A mismatch must be
    // rejected.
    let base = unique_daemon_test_dir("resume-artifact-mismatch");
    let request = daemon_launch_request(&base, "run-artifact");
    let mut metadata = resume_metadata_for_request(&request);
    metadata.artifact_root = Some("/different/artifact/root".to_string());
    let err = validate_resume_identity_against_metadata(&request, &metadata).unwrap_err();
    assert!(
        err.contains("artifact_dir") && err.contains("artifact_root"),
        "expected artifact mismatch rejection, got: {err}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn validate_resume_identity_accepts_matching_artifact_dir_exact_typed() {
    let base = unique_daemon_test_dir("resume-artifact-match");
    let request = daemon_launch_request(&base, "run-artifact-ok");
    let metadata = resume_metadata_for_request(&request);
    // The metadata's artifact_root matches the request's artifact_dir exactly.
    validate_resume_identity_against_metadata(&request, &metadata)
        .expect("matching artifact_dir must pass");
    let _ = std::fs::remove_dir_all(&base);
}

// ---------------------------------------------------------------------------
// Issue 158 finding 4: non-daemon fresh workspace created after precheck
// ---------------------------------------------------------------------------

#[test]
fn ensure_non_daemon_workspace_creates_directory() {
    let base = unique_daemon_test_dir("non-daemon-workspace");
    let work_dir = base.join("work");
    let mut config = config_with_variables(serde_json::json!({
        "work_dir": work_dir.to_string_lossy(),
    }));
    config.config_id = "cfg-test".to_string();
    ensure_non_daemon_workspace(&config, "run-non-daemon").expect("create workspace");
    assert!(
        work_dir.is_dir(),
        "non-daemon workspace must be created after precheck"
    );
    assert_eq!(
        std::fs::read_to_string(work_dir.join(".luther/workspace-owner")).unwrap(),
        "run-non-daemon"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn ensure_non_daemon_workspace_uses_runtime_default_when_unset() {
    let config = config_with_variables(serde_json::json!({}));
    // When work_dir is unset, the runtime default per-run directory is used.
    // This must not panic and must create a directory.
    ensure_non_daemon_workspace(&config, "run-default-ws").expect("create workspace");
    // The default path is under the runtime data dir; verify it exists.
    let default_path = luther_workflow::runtime_paths::get_run_dir("run-default-ws");
    assert!(
        default_path.is_dir(),
        "default non-daemon workspace must be created"
    );
    let _ = std::fs::remove_dir_all(&default_path);
}

// ---------------------------------------------------------------------------
// Issue 158 finding 6: daemon workspace provisioning before artifact mkdir
// ---------------------------------------------------------------------------

#[test]
fn ensure_daemon_run_dirs_provisions_workspace_before_artifact() {
    // The workspace ownership provisioning must be the FIRST mutation,
    // before the artifact directory is created. If provisioning fails, the
    // artifact directory must NOT be created.
    let base = unique_daemon_test_dir("daemon-ordering-fail");
    let work_dir = base.join("work");
    let artifact_dir = base.join("artifacts");
    // Pre-create the work_dir with a foreign ownership marker so provisioning
    // fails. The workspace provisioning must fail before the artifact dir is
    // created.
    std::fs::create_dir_all(work_dir.join(".luther")).unwrap();
    std::fs::write(work_dir.join(".luther/workspace-owner"), "foreign-run").unwrap();
    let request = luther_workflow::daemon::launcher::LaunchRequest {
        config_id: "cfg".to_string(),
        workflow_type_id: Some("wf".to_string()),
        run_id: "run-ordering".to_string(),
        repo: "o/r".to_string(),
        issue_number: 200,
        daemon_managed_claim: true,
        claim_assignment_added: true,
        claim_label_added: true,
        work_dir: Some(work_dir.clone()),
        artifact_dir: Some(artifact_dir.clone()),
        config_root: std::path::PathBuf::from("config"),
    };
    let err = ensure_daemon_run_dirs(&request).unwrap_err();
    assert!(
        err.contains("provision workspace ownership"),
        "expected provisioning failure, got: {err}"
    );
    // The artifact directory must NOT have been created because workspace
    // provisioning failed first.
    assert!(
        !artifact_dir.exists(),
        "artifact dir must not be created when workspace provisioning fails"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn ensure_daemon_run_dirs_provisions_workspace_then_artifact_on_success() {
    // On success, both the workspace ownership and the artifact directory are
    // created, with workspace ownership as the first mutation.
    let base = unique_daemon_test_dir("daemon-ordering-success");
    let request = daemon_launch_request(&base, "run-ordering-ok");
    ensure_daemon_run_dirs(&request).expect("workspace + artifact created");
    let work = request.work_dir.as_deref().unwrap();
    let artifact = request.artifact_dir.as_deref().unwrap();
    assert_eq!(
        std::fs::read_to_string(work.join(".luther/workspace-owner")).unwrap(),
        "run-ordering-ok"
    );
    assert!(artifact.is_dir());
    let _ = std::fs::remove_dir_all(&base);
}
