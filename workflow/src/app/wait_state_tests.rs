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
        daemon_managed_claim: false,
        claim_assignment_added: false,
        claim_label_added: false,
        work_dir: None,
        artifact_dir: None,
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
        daemon_managed_claim: false,
        claim_assignment_added: false,
        claim_label_added: false,
        work_dir: None,
        artifact_dir: None,
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
        daemon_managed_claim: false,
        claim_assignment_added: false,
        claim_label_added: false,
        work_dir: None,
        artifact_dir: None,
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
        daemon_managed_claim: false,
        claim_assignment_added: false,
        claim_label_added: false,
        work_dir: None,
        artifact_dir: None,
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
        daemon_managed_claim: false,
        claim_assignment_added: false,
        claim_label_added: false,
        work_dir: None,
        artifact_dir: None,
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
        daemon_managed_claim: false,
        claim_assignment_added: false,
        claim_label_added: false,
        work_dir: None,
        artifact_dir: None,
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

#[test]
fn string_field_returns_none_for_empty_or_missing() {
    let value = serde_json::json!({"a": "x", "b": "", "c": 5});
    assert_eq!(string_field(&value, "a").as_deref(), Some("x"));
    assert!(string_field(&value, "b").is_none());
    assert!(string_field(&value, "c").is_none());
    assert!(string_field(&value, "missing").is_none());
}

#[test]
fn read_json_path_reads_and_reports_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let good = tmp.path().join("good.json");
    std::fs::write(&good, "{\"k\":1}").unwrap();
    let parsed = read_json_path(&good).unwrap();
    assert_eq!(parsed.get("k").unwrap(), 1);

    let missing = tmp.path().join("missing.json");
    let err = read_json_path(&missing).unwrap_err();
    assert!(err.contains("failed to read"));

    let bad = tmp.path().join("bad.json");
    std::fs::write(&bad, "not json").unwrap();
    let err = read_json_path(&bad).unwrap_err();
    assert!(err.contains("failed to parse JSON"));
}

#[test]
fn metadata_pr_number_converts_valid_and_rejects_negative() {
    let mut md = RunMetadata::new("run", "wf", "cfg");
    md.pr_number = Some(42);
    assert_eq!(metadata_pr_number(Some(&md)), Some(42));
    md.pr_number = Some(-1);
    assert_eq!(metadata_pr_number(Some(&md)), None);
    assert_eq!(metadata_pr_number(None), None);
}

#[test]
fn max_wait_seconds_for_wait_only_for_child_kinds() {
    let tmp = tempfile::tempdir().unwrap();
    let config = workflow_config(tmp.path());
    assert_eq!(
        max_wait_seconds_for_wait(&config, WaitKind::DependencyChildMerge),
        Some(DEFAULT_MAX_CHILD_MERGE_WAIT_SECONDS)
    );
    assert_eq!(
        max_wait_seconds_for_wait(&config, WaitKind::DependencyChildWorkflow),
        Some(DEFAULT_MAX_CHILD_MERGE_WAIT_SECONDS)
    );
    assert_eq!(max_wait_seconds_for_wait(&config, WaitKind::PrChecks), None);
    assert_eq!(max_wait_seconds_for_wait(&config, WaitKind::PrMerge), None);
}

#[test]
fn validate_wait_poll_identity_enforces_pr_check_requirements() {
    let missing = WaitPollIdentity {
        pr_number: None,
        head_sha: None,
    };
    let err = validate_wait_poll_identity(WaitKind::PrChecks, &missing).unwrap_err();
    assert!(err.contains("PR checks"));

    let complete = WaitPollIdentity {
        pr_number: Some(1),
        head_sha: Some("sha".to_string()),
    };
    assert!(validate_wait_poll_identity(WaitKind::PrChecks, &complete).is_ok());
}

#[test]
fn validate_wait_poll_identity_pr_only_kinds_require_pr_number() {
    let only_sha = WaitPollIdentity {
        pr_number: None,
        head_sha: Some("sha".to_string()),
    };
    for kind in [
        WaitKind::CoderabbitReview,
        WaitKind::HumanReview,
        WaitKind::PrMerge,
        WaitKind::DependencyChildMerge,
    ] {
        let err = validate_wait_poll_identity(kind, &only_sha).unwrap_err();
        assert!(err.contains("missing PR number"));
    }
    let with_pr = WaitPollIdentity {
        pr_number: Some(9),
        head_sha: None,
    };
    assert!(validate_wait_poll_identity(WaitKind::PrMerge, &with_pr).is_ok());
}

#[test]
fn validate_wait_poll_identity_rate_limit_always_ok() {
    let empty = WaitPollIdentity {
        pr_number: None,
        head_sha: None,
    };
    assert!(validate_wait_poll_identity(WaitKind::RateLimitBackoff, &empty).is_ok());
}

#[test]
fn wait_artifact_root_none_when_unset() {
    // A config without artifact_dir variable and no metadata yields None.
    let mut config = {
        let tmp = tempfile::tempdir().unwrap();
        let c = workflow_config(tmp.path());
        c
    };
    config.variables.remove("artifact_dir");
    assert!(wait_artifact_root(&config, None).unwrap().is_none());
}

#[test]
fn wait_artifact_root_resolves_absolute_path() {
    let tmp = tempfile::tempdir().unwrap();
    let config = workflow_config(tmp.path());
    let root = wait_artifact_root(&config, None).unwrap();
    assert_eq!(root, Some(tmp.path().to_path_buf()));
}

#[test]
fn read_child_workflow_wait_artifact_absent_is_none() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(read_child_workflow_wait_artifact(tmp.path())
        .unwrap()
        .is_none());
}

#[test]
fn read_child_merge_wait_artifact_absent_and_present() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(read_child_merge_wait_artifact(tmp.path())
        .unwrap()
        .is_none());

    std::fs::write(
        tmp.path().join("child-merge-wait.json"),
        "{\"pr\":{\"number\":321}}",
    )
    .unwrap();
    assert_eq!(
        read_child_merge_wait_artifact(tmp.path()).unwrap(),
        Some(321)
    );
}

#[test]
fn read_child_merge_wait_artifact_malformed_errors() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("child-merge-wait.json"), "{\"pr\":{}}").unwrap();
    let err = read_child_merge_wait_artifact(tmp.path()).unwrap_err();
    assert!(err.contains("missing numeric pr.number"));
}

#[test]
fn read_pr_identity_artifact_absent_is_none() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(read_pr_identity_artifact(tmp.path(), "run-x")
        .unwrap()
        .is_none());
}

#[test]
fn resolve_parameter_value_interpolates_and_recurses() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = workflow_config(tmp.path());
    config
        .variables
        .insert("head_ref".to_string(), "issue125".to_string());
    let value = serde_json::json!({
        "branch": "{head_ref}",
        "list": ["{head_ref}", 5],
        "num": 7,
    });
    let resolved = resolve_parameter_value(&config, value).unwrap();
    assert_eq!(resolved.get("branch").unwrap(), "issue125");
    assert_eq!(resolved.get("list").unwrap()[0], "issue125");
    assert_eq!(resolved.get("list").unwrap()[1], 5);
    assert_eq!(resolved.get("num").unwrap(), 7);
}

#[test]
fn resolve_step_parameters_handles_null_and_object() {
    let tmp = tempfile::tempdir().unwrap();
    let config = workflow_config(tmp.path());
    let null_step = StepDef {
        step_id: "s".to_string(),
        step_type: "noop".to_string(),
        description: None,
        parameters: None,
        produces: None,
        consumes: None,
        terminal: None,
    };
    assert_eq!(
        resolve_step_parameters(&config, &null_step).unwrap(),
        serde_json::Value::Null
    );
}

#[test]
fn set_optional_wait_parameter_sets_null_when_absent() {
    let mut payload = serde_json::json!({});
    let params = serde_json::json!({"artifact_root": "/root"});
    set_optional_wait_parameter(&mut payload, &params, "artifact_root");
    set_optional_wait_parameter(&mut payload, &params, "missing");
    assert_eq!(payload.get("artifact_root").unwrap(), "/root");
    assert!(payload.get("missing").unwrap().is_null());
}

#[test]
fn set_required_wait_parameter_errors_when_missing_or_unresolved() {
    let mut payload = serde_json::json!({});
    let params = serde_json::json!({"head_ref": "issue125", "unresolved": "{token}"});
    set_required_wait_parameter(&mut payload, &params, "head_ref").unwrap();
    assert_eq!(payload.get("head_ref").unwrap(), "issue125");

    let err = set_required_wait_parameter(&mut payload, &params, "base_ref").unwrap_err();
    assert!(err.contains("missing resolved PR check wait parameter"));

    let err = set_required_wait_parameter(&mut payload, &params, "unresolved").unwrap_err();
    assert!(err.contains("unresolved PR check wait parameter"));
}

#[test]
fn add_optional_wait_parameters_fills_all_keys_with_null_default() {
    let mut payload = serde_json::json!({});
    let params = serde_json::json!({"artifact_root": "/root", "head_ref": "issue125"});
    add_optional_wait_parameters(&mut payload, &params);
    assert_eq!(payload.get("artifact_root").unwrap(), "/root");
    assert_eq!(payload.get("head_ref").unwrap(), "issue125");
    assert!(payload.get("base_sha").unwrap().is_null());
}

#[test]
fn add_required_pr_check_wait_parameters_requires_refs() {
    let mut payload = serde_json::json!({});
    let complete = serde_json::json!({
        "artifact_root": "/root",
        "head_ref": "issue125",
        "base_ref": "main",
        "base_sha": "abc",
    });
    add_required_pr_check_wait_parameters(&mut payload, &complete).unwrap();
    assert_eq!(payload.get("base_ref").unwrap(), "main");

    let mut payload = serde_json::json!({});
    let incomplete = serde_json::json!({"artifact_root": "/root"});
    assert!(add_required_pr_check_wait_parameters(&mut payload, &incomplete).is_err());
}

#[test]
fn wait_condition_payload_pr_checks_requires_params() {
    let request = luther_workflow::daemon::launcher::LaunchRequest {
        config_id: "cfg".to_string(),
        repo: "owner/repo".to_string(),
        issue_number: 125,
        daemon_managed_claim: false,
        claim_assignment_added: false,
        claim_label_added: false,
        run_id: "run-1".to_string(),
        workflow_type_id: None,
        work_dir: None,
        artifact_dir: None,
    };
    let params = serde_json::json!({
        "artifact_root": "/root",
        "head_ref": "issue125",
        "base_ref": "main",
        "base_sha": "abc",
    });
    let payload = wait_condition_payload(
        "watch_pr_checks",
        "waiting",
        &request,
        WaitKind::PrChecks,
        &params,
    )
    .unwrap();
    assert_eq!(payload.get("repository").unwrap(), "owner/repo");
    assert_eq!(payload.get("issue_number").unwrap(), 125);
    assert_eq!(payload.get("base_ref").unwrap(), "main");
}

#[test]
fn wait_condition_payload_optional_kind_allows_missing_params() {
    let request = luther_workflow::daemon::launcher::LaunchRequest {
        config_id: "cfg".to_string(),
        repo: "owner/repo".to_string(),
        issue_number: 7,
        daemon_managed_claim: false,
        claim_assignment_added: false,
        claim_label_added: false,
        run_id: "run-2".to_string(),
        workflow_type_id: None,
        work_dir: None,
        artifact_dir: None,
    };
    let params = serde_json::json!({});
    let payload = wait_condition_payload(
        "wait_for_merge",
        "waiting",
        &request,
        WaitKind::PrMerge,
        &params,
    )
    .unwrap();
    assert!(payload.get("artifact_root").unwrap().is_null());
}
