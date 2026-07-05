use super::*;
use std::collections::HashMap;

fn workflow_config(artifact_dir: &std::path::Path) -> WorkflowConfig {
    WorkflowConfig {
        config_id: "cfg".to_string(),
        workflow_type_id: "wf".to_string(),
        runtime: luther_workflow::workflow::schema::RuntimeConfig {
            timeout_seconds: 1,
            max_retries: 0,
            parallel_steps: None,
            log_level: None,
        },
        repo: luther_workflow::workflow::schema::RepoConfig {
            workspace_strategy: "reuse".to_string(),
            branch_template: "issue{issue_number}".to_string(),
            base_branch: Some("main".to_string()),
            workspace_root: None,
            project_subdir: None,
            artifact_path_base: None,
            diff_path_base: None,
            diff_path_normalization:
                luther_workflow::workflow::schema::DiffPathNormalization::RepoRelative,
        },
        guard_limits: luther_workflow::workflow::schema::GuardLimits {
            max_iterations: None,
            max_file_changes: None,
            max_tokens: None,
            max_cost: None,
        },
        variables: HashMap::from([(
            "artifact_dir".to_string(),
            artifact_dir.to_string_lossy().to_string(),
        )]),
        discovery: None,
        parent_orchestration: Default::default(),
        command_manifest: None,
        target_profile: None,
    }
}

#[test]
fn wait_poll_identity_reads_captured_pr_artifact_when_metadata_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let pr_dir = tmp
        .path()
        .join("pr-followup")
        .join("current")
        .join("run-identity")
        .join("owner")
        .join("repo")
        .join("62");
    std::fs::create_dir_all(&pr_dir).unwrap();
    std::fs::write(
        pr_dir.join("pr.json"),
        serde_json::to_vec(&serde_json::json!({
            "run_id": "run-identity",
            "pr_number": 62,
            "head_sha": "abcdef123456",
            "repository_owner": "owner",
            "repository_name": "repo"
        }))
        .unwrap(),
    )
    .unwrap();
    let request = luther_workflow::daemon::launcher::LaunchRequest {
        config_id: "cfg".to_string(),
        workflow_type_id: None,
        run_id: "run-identity".to_string(),
        repo: "owner/repo".to_string(),
        issue_number: 62,
    };
    let identity = wait_poll_identity(
        &request,
        &workflow_config(tmp.path()),
        None,
        WaitKind::PrChecks,
    )
    .unwrap();

    assert_eq!(identity.pr_number, Some(62));
    assert_eq!(identity.head_sha.as_deref(), Some("abcdef123456"));
}

#[test]
fn wait_poll_identity_rejects_missing_pr_check_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let request = luther_workflow::daemon::launcher::LaunchRequest {
        config_id: "cfg".to_string(),
        workflow_type_id: None,
        run_id: "run-missing".to_string(),
        repo: "owner/repo".to_string(),
        issue_number: 62,
    };

    let err = wait_poll_identity(
        &request,
        &workflow_config(tmp.path()),
        None,
        WaitKind::PrChecks,
    )
    .unwrap_err();

    assert!(err.contains("missing PR number or head SHA"));
}


#[test]
fn wait_poll_identity_requires_child_run_id_for_child_workflow_wait() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("child-workflow-wait.json"),
        serde_json::to_vec(&serde_json::json!({
            "waiting": true,
            "child_issue_number": 63,
            "child_run_id": null
        }))
        .unwrap(),
    )
    .unwrap();
    let request = luther_workflow::daemon::launcher::LaunchRequest {
        config_id: "cfg".to_string(),
        workflow_type_id: Some("parent-issue-orchestrator-v1".to_string()),
        run_id: "run-parent".to_string(),
        repo: "owner/repo".to_string(),
        issue_number: 62,
    };

    let err = wait_poll_identity(
        &request,
        &workflow_config(tmp.path()),
        None,
        WaitKind::DependencyChildWorkflow,
    )
    .unwrap_err();

    assert!(err.contains("missing child run ID"));
}

#[test]
fn wait_poll_identity_reads_child_workflow_wait_run_id() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("child-workflow-wait.json"),
        serde_json::to_vec(&serde_json::json!({
            "waiting": true,
            "child_issue_number": 63,
            "child_lease_id": "lease-63",
            "child_run_id": "child-run-63"
        }))
        .unwrap(),
    )
    .unwrap();
    let request = luther_workflow::daemon::launcher::LaunchRequest {
        config_id: "cfg".to_string(),
        workflow_type_id: Some("parent-issue-orchestrator-v1".to_string()),
        run_id: "run-parent".to_string(),
        repo: "owner/repo".to_string(),
        issue_number: 62,
    };

    let identity = wait_poll_identity(
        &request,
        &workflow_config(tmp.path()),
        None,
        WaitKind::DependencyChildWorkflow,
    )
    .unwrap();

    assert_eq!(identity.head_sha.as_deref(), Some("child-run-63"));
}

struct ChildWorkflowWaitFixture {
    _tmp: tempfile::TempDir,
    db_path: std::path::PathBuf,
    conn: rusqlite::Connection,
    config: WorkflowConfig,
    request: luther_workflow::daemon::launcher::LaunchRequest,
}

fn child_workflow_wait_fixture() -> ChildWorkflowWaitFixture {
    let tmp = tempfile::tempdir().unwrap();
    let artifact_root = tmp.path().join("artifacts");
    std::fs::create_dir_all(&artifact_root).unwrap();
    write_child_workflow_wait_artifact(&artifact_root);
    let db_path = tmp.path().join("checkpoints.db");
    luther_workflow::persistence::init_database(&db_path).unwrap();
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    luther_workflow::persistence::sqlite::init_runs_schema(&conn).unwrap();
    save_child_workflow_checkpoint(&conn, &artifact_root);
    let mut config = workflow_config(&artifact_root);
    config.workflow_type_id = "parent-issue-orchestrator-v1".to_string();
    config.config_id = "parent-orchestrator-luther".to_string();
    let request = child_workflow_wait_request();
    ChildWorkflowWaitFixture {
        _tmp: tmp,
        db_path,
        conn,
        config,
        request,
    }
}

fn write_child_workflow_wait_artifact(artifact_root: &std::path::Path) {
    std::fs::write(
        artifact_root.join("child-workflow-wait.json"),
        serde_json::to_vec(&serde_json::json!({
            "waiting": true,
            "child_issue_number": 63,
            "child_lease_id": "lease-63",
            "child_run_id": "child-run-63"
        }))
        .unwrap(),
    )
    .unwrap();
}

fn save_child_workflow_checkpoint(conn: &rusqlite::Connection, artifact_root: &std::path::Path) {
    let checkpoint = luther_workflow::persistence::checkpoint::Checkpoint::new(
        "parent-run-62",
        "launch_or_resume_child_workflow",
    );
    luther_workflow::persistence::checkpoint::save_checkpoint_with_conn(conn, &checkpoint).unwrap();
    let mut metadata = RunMetadata::new(
        "parent-run-62",
        "parent-issue-orchestrator-v1",
        "parent-orchestrator-luther",
    );
    metadata.artifact_root = Some(artifact_root.to_string_lossy().to_string());
    persist_run_with_conn(conn, &metadata).unwrap();
}

fn child_workflow_wait_request() -> luther_workflow::daemon::launcher::LaunchRequest {
    luther_workflow::daemon::launcher::LaunchRequest {
        config_id: "parent-orchestrator-luther".to_string(),
        workflow_type_id: Some("parent-issue-orchestrator-v1".to_string()),
        run_id: "parent-run-62".to_string(),
        repo: "owner/repo".to_string(),
        issue_number: 62,
    }
}

#[test]
fn persist_external_wait_state_stores_child_run_id_identity() {
    let fixture = child_workflow_wait_fixture();

    persist_external_wait_state(
        &fixture.request,
        &fixture.config,
        &fixture.db_path,
        "launch_or_resume_child_workflow",
        "child workflow waiting",
    )
    .unwrap();

    let record = get_wait_state(&fixture.conn, "parent-run-62")
        .unwrap()
        .unwrap();
    assert_child_workflow_wait_record(&record);
}

fn assert_child_workflow_wait_record(
    record: &luther_workflow::persistence::wait_state::WaitStateRecord,
) {
    assert_eq!(record.wait_kind, WaitKind::DependencyChildWorkflow);
    assert_eq!(record.head_sha.as_deref(), Some("child-run-63"));
    assert_eq!(
        record
            .wait_condition
            .get("child_run_id")
            .and_then(serde_json::Value::as_str),
        Some("child-run-63")
    );
    assert_eq!(
        record
            .wait_condition
            .get("child_issue_number")
            .and_then(serde_json::Value::as_u64),
        Some(63)
    );
    assert_eq!(
        record
            .wait_condition
            .get("child_lease_id")
            .and_then(serde_json::Value::as_str),
        Some("lease-63")
    );
    assert_eq!(
        record
            .wait_condition
            .get("parent_run_id")
            .and_then(serde_json::Value::as_str),
        Some("parent-run-62")
    );
}
#[test]
fn persist_run_poll_identity_updates_stale_or_empty_metadata() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    luther_workflow::persistence::sqlite::init_runs_schema(&conn).unwrap();
    let mut metadata = RunMetadata::new("run-identity", "wf", "cfg");
    metadata.pr_number = Some(1);
    metadata.head_sha = Some("old".to_string());
    persist_run_with_conn(&conn, &metadata).unwrap();
    let identity = WaitPollIdentity {
        pr_number: Some(62),
        head_sha: Some("new".to_string()),
    };

    persist_run_poll_identity(&conn, &mut metadata, &identity).unwrap();

    let loaded = get_run_with_conn(&conn, "run-identity").unwrap().unwrap();
    assert_eq!(loaded.pr_number, Some(62));
    assert_eq!(loaded.head_sha.as_deref(), Some("new"));
}

#[test]
fn wait_poll_identity_prefers_captured_pr_artifact_over_stale_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let pr_dir = tmp
        .path()
        .join("pr-followup")
        .join("current")
        .join("run-stale")
        .join("owner")
        .join("repo")
        .join("62");
    std::fs::create_dir_all(&pr_dir).unwrap();
    std::fs::write(
        pr_dir.join("pr.json"),
        serde_json::to_vec(&serde_json::json!({
            "run_id": "run-stale",
            "pr_number": 62,
            "head_sha": "fresh-head",
            "repository_owner": "owner",
            "repository_name": "repo"
        }))
        .unwrap(),
    )
    .unwrap();
    let request = luther_workflow::daemon::launcher::LaunchRequest {
        config_id: "cfg".to_string(),
        workflow_type_id: None,
        run_id: "run-stale".to_string(),
        repo: "owner/repo".to_string(),
        issue_number: 62,
    };
    let mut metadata = RunMetadata::new("run-stale", "wf", "cfg");
    metadata.pr_number = Some(1);
    metadata.head_sha = Some("stale-head".to_string());

    let identity = wait_poll_identity(
        &request,
        &workflow_config(tmp.path()),
        Some(&metadata),
        WaitKind::PrChecks,
    )
    .unwrap();

    assert_eq!(identity.pr_number, Some(62));
    assert_eq!(identity.head_sha.as_deref(), Some("fresh-head"));
}
