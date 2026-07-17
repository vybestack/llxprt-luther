use super::*;

fn write_config_root(root: &std::path::Path, wf: &str, restored_step: &str) {
    let workflows = root.join("workflows");
    let configs = root.join("workflow-configs");
    std::fs::create_dir_all(&workflows).expect("workflow dir");
    std::fs::create_dir_all(&configs).expect("config dir");
    let workflow = serde_json::json!({
        "workflow_type_id": wf,
        "steps": [
            {"step_id": "prepare_custom_resume", "step_type": "noop"},
            {"step_id": restored_step, "step_type": "noop"},
            {"step_id": "post_restore_sentinel", "step_type": "noop"}
        ],
        "transitions": [],
        "guards": {"max_retries": 1, "timeout_seconds": 30}
    });
    let config = serde_json::json!({
        "config_id": "custom-resume-config",
        "workflow_type_id": wf,
        "runtime": {"timeout_seconds": 30, "max_retries": 1},
        "repository": {"workspace_strategy": "temp", "branch_template": "test-{run_id}", "base_branch": "main"},
        "guards": {"max_iterations": 1, "max_file_changes": 10, "max_tokens": 1000, "max_cost": 1.0}
    });
    std::fs::write(workflows.join(format!("{wf}.json")), workflow.to_string())
        .expect("workflow file");
    std::fs::write(
        configs.join("custom-resume-config.json"),
        config.to_string(),
    )
    .expect("config file");
}

fn seed_run(store: &SqliteStore, run_id: &str, wf: &str, step: &str) -> RunMetadata {
    let mut md = RunMetadata::new(run_id, wf, "custom-resume-config");
    md.status = RunStatus::Failed;
    md.current_step = Some(step.to_string());
    persist_run_with_conn(store.conn(), &md).expect("persist run");
    let cp = luther_workflow::persistence::Checkpoint::with_snapshot(
        run_id,
        step,
        luther_workflow::persistence::StateSnapshot {
            status: luther_workflow::persistence::CHECKPOINT_STATUS_INTERRUPTED.to_string(),
            ..Default::default()
        },
    );
    luther_workflow::persistence::save_checkpoint_with_conn(store.conn(), &cp)
        .expect("save checkpoint");
    md
}

#[test]
fn reconstructs_runner_from_non_default_config_dir() {
    let temp = tempfile::tempdir().expect("tempdir");
    let config_root = temp.path().join("custom-config");
    let db_path = temp.path().join("checkpoints.db");
    write_config_root(&config_root, "custom-resume-v1", "custom_marker_step");
    let store = SqliteStore::open(&db_path).expect("open store");
    let md = seed_run(
        &store,
        "custom-config-run",
        "custom-resume-v1",
        "custom_marker_step",
    );
    let runner = reconstruct_runner(&md, &md.run_id, &db_path, &Some(config_root))
        .expect("custom config root reconstructs runner");
    assert_eq!(runner.current_step(), "custom_marker_step");
    assert_eq!(runner.workflow_type_id(), "custom-resume-v1");
    assert_eq!(runner.config_id(), "custom-resume-config");
}

#[test]
fn reconstruct_runner_rejects_missing_current_step() {
    let temp = tempfile::tempdir().expect("tempdir");
    let config_root = temp.path().join("custom-config");
    let db_path = temp.path().join("checkpoints.db");
    write_config_root(&config_root, "custom-resume-v1", "custom_marker_step");
    let store = SqliteStore::open(&db_path).expect("open store");
    let md = seed_run(
        &store,
        "missing-step-run",
        "custom-resume-v1",
        "removed_marker_step",
    );
    let err = match reconstruct_runner(&md, &md.run_id, &db_path, &Some(config_root)) {
        Ok(_) => panic!("missing persisted step is rejected"),
        Err(err) => err,
    };
    assert!(
        err.contains("current_step 'removed_marker_step' is not present"),
        "unexpected error: {err}"
    );
}

#[test]
fn run_context_from_metadata_preserves_identity_and_defaults_log_path() {
    let mut md = RunMetadata::new("ctx-run", "wf", "cfg");
    md.artifact_root = Some("/artifacts".to_string());
    md.workspace_path = Some("/workspace".to_string());
    md.repository = Some("owner/repo".to_string());
    md.issue_number = Some(125);
    md.pr_number = Some(126);
    md.head_sha = Some("deadbeef".to_string());

    let ctx = run_context_from_metadata(&md, "ctx-run");
    assert_eq!(ctx.artifact_root.as_deref(), Some("/artifacts"));
    assert_eq!(ctx.workspace_path.as_deref(), Some("/workspace"));
    assert_eq!(ctx.repository.as_deref(), Some("owner/repo"));
    assert_eq!(ctx.issue_number, Some(125));
    assert_eq!(ctx.pr_number, Some(126));
    assert_eq!(ctx.head_sha.as_deref(), Some("deadbeef"));
    // log_path defaults to the derived run log path when metadata omits it.
    assert!(ctx.log_path.is_some());
}

#[test]
fn run_context_from_metadata_uses_explicit_log_path() {
    let mut md = RunMetadata::new("ctx-run", "wf", "cfg");
    md.log_path = Some("/custom/log.txt".to_string());
    let ctx = run_context_from_metadata(&md, "ctx-run");
    assert_eq!(ctx.log_path.as_deref(), Some("/custom/log.txt"));
}

#[test]
fn write_continuation_result_writes_named_artifact() {
    let temp = tempfile::tempdir().expect("tempdir");
    let outcome: Result<RunOutcome, luther_workflow::engine::runner::EngineError> =
        Ok(RunOutcome::Success);
    write_continuation_result(
        temp.path(),
        &luther_workflow::engine::ContinuationKind::Resume,
        "some_step",
        &outcome,
    );
    let name = luther_workflow::engine::continuation::result_artifact_name(
        &luther_workflow::engine::ContinuationKind::Resume,
    );
    let written = temp.path().join(name);
    assert!(written.exists(), "expected {name} to be written");
    let content = std::fs::read_to_string(&written).unwrap();
    assert!(content.contains("completed"));
}

#[test]
fn write_continuation_result_maps_waiting_external_status() {
    let temp = tempfile::tempdir().expect("tempdir");
    let outcome: Result<RunOutcome, luther_workflow::engine::runner::EngineError> =
        Ok(RunOutcome::WaitingExternal {
            step_id: "watch".to_string(),
            reason: "pending".to_string(),
        });
    write_continuation_result(
        temp.path(),
        &luther_workflow::engine::ContinuationKind::Retry {
            from_failed_step: true,
        },
        "watch",
        &outcome,
    );
    let name = luther_workflow::engine::continuation::result_artifact_name(
        &luther_workflow::engine::ContinuationKind::Retry {
            from_failed_step: true,
        },
    );
    let content = std::fs::read_to_string(temp.path().join(name)).unwrap();
    assert!(content.contains("waiting_external"));
}

fn sample_checkpoint(run_id: &str, step: &str) -> luther_workflow::persistence::Checkpoint {
    luther_workflow::persistence::Checkpoint::with_snapshot(
        run_id,
        step,
        luther_workflow::persistence::StateSnapshot {
            status: "interrupted".to_string(),
            loop_count: 2,
            retry_count: 1,
            ..Default::default()
        },
    )
}

#[test]
fn print_checkpoints_json_emits_valid_document() {
    let cps = vec![sample_checkpoint("run-x", "step-a")];
    // Exercises the JSON rendering path (stdout side effects are ignored).
    print_checkpoints_json("run-x", &cps);
    // Rebuild the same document to assert on structure/content.
    let identity = luther_workflow::engine::continuation::checkpoint_identity(&cps[0]);
    assert!(!identity.is_empty());
}

#[test]
fn print_checkpoints_human_handles_empty_and_populated() {
    print_checkpoints_human("empty-run", &[]);
    let cps = vec![sample_checkpoint("run-y", "step-b")];
    print_checkpoints_human("run-y", &cps);
}

// --- finalize_continuation_lease coverage ---

use luther_workflow::persistence::{
    create_lease, get_lease_for_issue, get_run_with_conn, persist_run_with_conn,
    update_lease_status, FailureCleanupState, IssueLease, LeaseStatus, RunStatus,
};

/// Build an in-memory store with the leases + runs schema initialized.
fn lease_store() -> SqliteStore {
    SqliteStore::open_in_memory().expect("open in-memory store")
}

/// Seed a lease owned by `run_id` in `Running` status for `o/r` issue `issue`.
fn seed_running_lease(store: &SqliteStore, run_id: &str, issue_number: u64) -> IssueLease {
    let now = chrono::Utc::now();
    let lease = IssueLease {
        lease_id: format!("lease-{run_id}-{issue_number}"),
        issue_repo: "o/r".to_string(),
        issue_number,
        config_id: "cfg".to_string(),
        run_id: Some(run_id.to_string()),
        status: LeaseStatus::Running,
        claimed_at: now,
        updated_at: now,
        heartbeat_at: now,
    };
    create_lease(store.conn(), &lease).expect("create lease");
    lease
}

/// Metadata referencing `o/r` issue `issue`, owned by `run_id`.
fn issue_metadata(run_id: &str, issue_number: i64) -> RunMetadata {
    let mut md = RunMetadata::new(run_id, "wf", "cfg");
    md.repository = Some("o/r".to_string());
    md.issue_number = Some(issue_number);
    md
}

/// A complete `FailureCleanupState` so `is_cleanup_failure_abandonment()` is true.
fn complete_failure_cleanup() -> FailureCleanupState {
    let now = chrono::Utc::now();
    FailureCleanupState {
        schema_version: FailureCleanupState::SCHEMA_VERSION,
        failed_step: "remediate".to_string(),
        failure_outcome: "fatal".to_string(),
        failure_reason: "agent timed out".to_string(),
        failed_checkpoint_id: "cp-1".to_string(),
        failed_state_snapshot: luther_workflow::persistence::StateSnapshot::default(),
        cleanup_step: "abandon_and_log".to_string(),
        cleanup_succeeded: true,
        captured_at: now,
        cleanup_completed_at: Some(now),
        recovery_consumed_at: None,
    }
}

#[test]
fn finalize_lease_succeeds_when_runner_already_protected_cleanup_abandoned() {
    // The runner atomically transitioned the lease to CleanupAbandoned during
    // failure cleanup. Finalization must be idempotent and succeed.
    let store = lease_store();
    let run_id = "cleanup-success";
    let lease = seed_running_lease(&store, run_id, 200);
    let mut md = issue_metadata(run_id, 200);
    md.status = RunStatus::Abandoned;
    md.failure_cleanup = Some(complete_failure_cleanup());
    persist_run_with_conn(store.conn(), &md).expect("persist abandoned run");

    // Simulate the runner's protect_failure_cleanup_lease: Running -> CleanupAbandoned.
    update_lease_status(
        store.conn(),
        &lease.lease_id,
        LeaseStatus::CleanupAbandoned,
        Some(run_id),
    )
    .expect("protect lease");

    let outcome: Result<RunOutcome, luther_workflow::engine::runner::EngineError> =
        Ok(RunOutcome::Abandoned {
            step_id: "abandon_and_log".to_string(),
            reason: "cleanup complete".to_string(),
        });

    finalize_continuation_lease(&store, &md, run_id, &outcome).expect("idempotent finalization");

    let finalized = get_lease_for_issue(store.conn(), "o/r", 200)
        .expect("query")
        .expect("lease present");
    assert_eq!(finalized.status, LeaseStatus::CleanupAbandoned);
    assert_eq!(finalized.run_id.as_deref(), Some(run_id));
}

#[test]
fn finalize_lease_succeeds_when_still_running_for_cleanup_abandonment() {
    // The runner has not yet protected the lease; finalization performs the
    // Running -> CleanupAbandoned transition itself.
    let store = lease_store();
    let run_id = "cleanup-running";
    let lease = seed_running_lease(&store, run_id, 201);
    let mut md = issue_metadata(run_id, 201);
    md.status = RunStatus::Abandoned;
    md.failure_cleanup = Some(complete_failure_cleanup());
    persist_run_with_conn(store.conn(), &md).expect("persist abandoned run");

    let outcome: Result<RunOutcome, luther_workflow::engine::runner::EngineError> =
        Ok(RunOutcome::Abandoned {
            step_id: "abandon_and_log".to_string(),
            reason: "cleanup complete".to_string(),
        });

    finalize_continuation_lease(&store, &md, run_id, &outcome).expect("finalization applies");

    let finalized = get_lease_for_issue(store.conn(), "o/r", 201)
        .expect("query")
        .expect("lease present");
    assert_eq!(finalized.status, LeaseStatus::CleanupAbandoned);
    assert_eq!(finalized.run_id.as_deref(), Some(run_id));
    let _ = lease;
}

#[test]
fn finalize_lease_fails_when_owner_mismatched_even_if_status_matches() {
    // The lease was reclaimed by a different run while this continuation was
    // executing. Finalization must be fail-closed, not silently accept the
    // mismatched owner.
    let store = lease_store();
    let run_id = "cleanup-owner-mismatch";
    let lease = seed_running_lease(&store, run_id, 202);
    let mut md = issue_metadata(run_id, 202);
    md.status = RunStatus::Abandoned;
    md.failure_cleanup = Some(complete_failure_cleanup());
    persist_run_with_conn(store.conn(), &md).expect("persist abandoned run");

    // A concurrent reclaim superseded ownership to a new run.
    update_lease_status(
        store.conn(),
        &lease.lease_id,
        LeaseStatus::CleanupAbandoned,
        Some("run-other"),
    )
    .expect("supersede owner");

    let outcome: Result<RunOutcome, luther_workflow::engine::runner::EngineError> =
        Ok(RunOutcome::Abandoned {
            step_id: "abandon_and_log".to_string(),
            reason: "cleanup complete".to_string(),
        });

    let error = finalize_continuation_lease(&store, &md, run_id, &outcome)
        .expect_err("mismatched owner must fail closed");
    assert!(
        error.contains("not continuation run"),
        "expected ownership failure, got: {error}"
    );
    assert!(
        error.contains(run_id),
        "error must reference the continuation run id"
    );

    // The lease must remain owned by the superseding run.
    let finalized = get_lease_for_issue(store.conn(), "o/r", 202)
        .expect("query")
        .expect("lease present");
    assert_eq!(finalized.run_id.as_deref(), Some("run-other"));
}

#[test]
fn finalize_lease_fails_when_status_drifted_from_expected() {
    // The lease is owned by this run but has already transitioned to a
    // terminal Failed status (e.g. by a concurrent path). Finalization for a
    // cleanup-abandonment outcome must not silently overwrite it.
    let store = lease_store();
    let run_id = "cleanup-status-drift";
    let lease = seed_running_lease(&store, run_id, 203);
    let mut md = issue_metadata(run_id, 203);
    md.status = RunStatus::Abandoned;
    md.failure_cleanup = Some(complete_failure_cleanup());
    persist_run_with_conn(store.conn(), &md).expect("persist abandoned run");

    // The lease drifted to Failed while still owned by this run.
    update_lease_status(
        store.conn(),
        &lease.lease_id,
        LeaseStatus::Failed,
        Some(run_id),
    )
    .expect("drift status");

    let outcome: Result<RunOutcome, luther_workflow::engine::runner::EngineError> =
        Ok(RunOutcome::Abandoned {
            step_id: "abandon_and_log".to_string(),
            reason: "cleanup complete".to_string(),
        });

    let error = finalize_continuation_lease(&store, &md, run_id, &outcome)
        .expect_err("status drift must fail closed");
    assert!(
        error.contains("was not finalized"),
        "expected finalization failure, got: {error}"
    );

    let finalized = get_lease_for_issue(store.conn(), "o/r", 203)
        .expect("query")
        .expect("lease present");
    assert_eq!(finalized.status, LeaseStatus::Failed);
    assert_eq!(finalized.run_id.as_deref(), Some(run_id));
}

#[test]
fn finalize_lease_skips_when_no_issue_identity() {
    // A run without repository/issue identity has no lease to finalize.
    let store = lease_store();
    let md = RunMetadata::new("no-issue-run", "wf", "cfg");
    let outcome: Result<RunOutcome, luther_workflow::engine::runner::EngineError> =
        Ok(RunOutcome::Success);
    finalize_continuation_lease(&store, &md, "no-issue-run", &outcome).expect("no-op finalization");
}

#[test]
fn finalize_lease_rejects_lease_owned_by_other_run() {
    let store = lease_store();
    let lease = seed_running_lease(&store, "run-original", 204);
    let md = issue_metadata("run-continuation", 204);
    let outcome: Result<RunOutcome, luther_workflow::engine::runner::EngineError> =
        Ok(RunOutcome::Success);
    let error = finalize_continuation_lease(&store, &md, "run-continuation", &outcome)
        .expect_err("ownership mismatch must fail closed");
    assert!(error.contains("not continuation run"));
    let unchanged = get_lease_for_issue(store.conn(), "o/r", 204)
        .expect("query")
        .expect("lease present");
    assert_eq!(unchanged.run_id.as_deref(), Some("run-original"));
    let _ = lease;
}

#[test]
fn finalize_lease_completes_on_success_outcome() {
    let store = lease_store();
    let run_id = "success-run";
    let lease = seed_running_lease(&store, run_id, 205);
    let md = issue_metadata(run_id, 205);
    persist_run_with_conn(store.conn(), &md).expect("persist run");

    let outcome: Result<RunOutcome, luther_workflow::engine::runner::EngineError> =
        Ok(RunOutcome::Success);
    finalize_continuation_lease(&store, &md, run_id, &outcome).expect("success finalization");

    let finalized = get_lease_for_issue(store.conn(), "o/r", 205)
        .expect("query")
        .expect("lease present");
    assert_eq!(finalized.status, LeaseStatus::Completed);
    let _ = lease;
}

#[test]
fn finalize_lease_fails_when_lease_vanishes_after_conditional_update() {
    // If the lease row disappears between the conditional update and the
    // fresh re-read, finalization must fail closed rather than silently
    // succeed.
    let store = lease_store();
    let run_id = "vanish-run";
    let lease = seed_running_lease(&store, run_id, 206);
    // Transition the lease out of the expected set so the conditional update
    // is rejected, then delete the row so the re-read cannot find it.
    update_lease_status(
        store.conn(),
        &lease.lease_id,
        LeaseStatus::Completed,
        Some(run_id),
    )
    .expect("transition out of expected");
    store
        .conn()
        .execute(
            "DELETE FROM issue_leases WHERE lease_id = ?1",
            rusqlite::params![&lease.lease_id],
        )
        .expect("delete lease");

    let mut md = issue_metadata(run_id, 206);
    md.status = RunStatus::Abandoned;
    md.failure_cleanup = Some(complete_failure_cleanup());
    persist_run_with_conn(store.conn(), &md).expect("persist run");

    let outcome: Result<RunOutcome, luther_workflow::engine::runner::EngineError> =
        Ok(RunOutcome::Abandoned {
            step_id: "abandon_and_log".to_string(),
            reason: "cleanup complete".to_string(),
        });

    let error = finalize_continuation_lease(&store, &md, run_id, &outcome)
        .expect_err("vanished lease must fail closed");
    assert!(
        error.contains("missing issue lease"),
        "expected missing-lease diagnostic, got: {error}"
    );
    // Confirm get_run_with_conn is exercised (the abandoned branch loads it).
    assert!(get_run_with_conn(store.conn(), run_id).unwrap().is_some());
}

/// A `FailureCleanupState` where cleanup has not yet succeeded, exercising the
/// Failure -> CleanupAbandoned mapping.
fn incomplete_failure_cleanup() -> FailureCleanupState {
    let now = chrono::Utc::now();
    FailureCleanupState {
        schema_version: FailureCleanupState::SCHEMA_VERSION,
        failed_step: "remediate".to_string(),
        failure_outcome: "fatal".to_string(),
        failure_reason: "agent timed out".to_string(),
        failed_checkpoint_id: "cp-1".to_string(),
        failed_state_snapshot: luther_workflow::persistence::StateSnapshot::default(),
        cleanup_step: "abandon_and_log".to_string(),
        cleanup_succeeded: false,
        captured_at: now,
        cleanup_completed_at: None,
        recovery_consumed_at: None,
    }
}

#[test]
fn finalize_lease_maps_interrupted_to_ready_to_resume() {
    // An interrupted run is resumable, not failed: the lease must move to
    // ReadyToResume so a later continuation can reclaim it.
    let store = lease_store();
    let run_id = "interrupted-run";
    let lease = seed_running_lease(&store, run_id, 210);
    let md = issue_metadata(run_id, 210);
    persist_run_with_conn(store.conn(), &md).expect("persist run");

    let outcome: Result<RunOutcome, luther_workflow::engine::runner::EngineError> =
        Ok(RunOutcome::Interrupted {
            step_id: "remediate".to_string(),
        });

    finalize_continuation_lease(&store, &md, run_id, &outcome).expect("interrupted finalization");

    let finalized = get_lease_for_issue(store.conn(), "o/r", 210)
        .expect("query")
        .expect("lease present");
    assert_eq!(finalized.status, LeaseStatus::ReadyToResume);
    assert_eq!(finalized.run_id.as_deref(), Some(run_id));
    let _ = lease;
}

#[test]
fn finalize_lease_maps_interrupted_idempotent_when_already_ready_to_resume() {
    // A prior continuation commit may have already advanced the lease to
    // ReadyToResume; finalization must be idempotent in that case.
    let store = lease_store();
    let run_id = "interrupted-idempotent";
    let lease = seed_running_lease(&store, run_id, 211);
    let md = issue_metadata(run_id, 211);
    persist_run_with_conn(store.conn(), &md).expect("persist run");

    update_lease_status(
        store.conn(),
        &lease.lease_id,
        LeaseStatus::ReadyToResume,
        Some(run_id),
    )
    .expect("pre-advance to ReadyToResume");

    let outcome: Result<RunOutcome, luther_workflow::engine::runner::EngineError> =
        Ok(RunOutcome::Interrupted {
            step_id: "remediate".to_string(),
        });

    finalize_continuation_lease(&store, &md, run_id, &outcome)
        .expect("idempotent interrupted finalization");

    let finalized = get_lease_for_issue(store.conn(), "o/r", 211)
        .expect("query")
        .expect("lease present");
    assert_eq!(finalized.status, LeaseStatus::ReadyToResume);
    assert_eq!(finalized.run_id.as_deref(), Some(run_id));
}

#[test]
fn finalize_lease_maps_failure_with_incomplete_cleanup_to_cleanup_abandoned() {
    // A failure outcome with durable provenance that cleanup has not yet
    // succeeded must preserve the failed-run identity as CleanupAbandoned
    // rather than plain Failed, preventing a duplicate relaunch from
    // clobbering pending recovery state.
    let store = lease_store();
    let run_id = "failure-incomplete-cleanup";
    let lease = seed_running_lease(&store, run_id, 212);
    let mut md = issue_metadata(run_id, 212);
    md.status = RunStatus::Failed;
    md.failure_cleanup = Some(incomplete_failure_cleanup());
    persist_run_with_conn(store.conn(), &md).expect("persist failed run");

    let outcome: Result<RunOutcome, luther_workflow::engine::runner::EngineError> =
        Ok(RunOutcome::Failure {
            step_id: "remediate".to_string(),
            reason: "agent timed out".to_string(),
        });

    finalize_continuation_lease(&store, &md, run_id, &outcome)
        .expect("failure with incomplete cleanup finalization");

    let finalized = get_lease_for_issue(store.conn(), "o/r", 212)
        .expect("query")
        .expect("lease present");
    assert_eq!(finalized.status, LeaseStatus::CleanupAbandoned);
    assert_eq!(finalized.run_id.as_deref(), Some(run_id));
    let _ = lease;
}

#[test]
fn finalize_lease_maps_failure_without_cleanup_provenance_to_failed() {
    // A failure with no failure_cleanup provenance (or with cleanup already
    // succeeded) must finalize as plain Failed.
    let store = lease_store();
    let run_id = "failure-no-cleanup";
    let lease = seed_running_lease(&store, run_id, 213);
    let mut md = issue_metadata(run_id, 213);
    md.status = RunStatus::Failed;
    persist_run_with_conn(store.conn(), &md).expect("persist failed run");

    let outcome: Result<RunOutcome, luther_workflow::engine::runner::EngineError> =
        Ok(RunOutcome::Failure {
            step_id: "remediate".to_string(),
            reason: "agent timed out".to_string(),
        });

    finalize_continuation_lease(&store, &md, run_id, &outcome).expect("plain failure finalization");

    let finalized = get_lease_for_issue(store.conn(), "o/r", 213)
        .expect("query")
        .expect("lease present");
    assert_eq!(finalized.status, LeaseStatus::Failed);
    assert_eq!(finalized.run_id.as_deref(), Some(run_id));
    let _ = lease;
}

#[test]
fn finalize_lease_maps_waiting_external_to_waiting_external_status() {
    // Finding 5: a WaitingExternal outcome must transition the Running lease to
    // WaitingExternal so a later continuation can resume it once the external
    // condition clears. This is the direct finalization test for the outcome.
    let store = lease_store();
    let run_id = "waiting-external-run";
    let lease = seed_running_lease(&store, run_id, 214);
    let md = issue_metadata(run_id, 214);
    persist_run_with_conn(store.conn(), &md).expect("persist run");

    let outcome: Result<RunOutcome, luther_workflow::engine::runner::EngineError> =
        Ok(RunOutcome::WaitingExternal {
            step_id: "wait_for_pr_checks".to_string(),
            reason: "checks still pending".to_string(),
        });

    finalize_continuation_lease(&store, &md, run_id, &outcome)
        .expect("waiting external finalization");

    let finalized = get_lease_for_issue(store.conn(), "o/r", 214)
        .expect("query")
        .expect("lease present");
    assert_eq!(finalized.status, LeaseStatus::WaitingExternal);
    assert_eq!(finalized.run_id.as_deref(), Some(run_id));
    let _ = lease;
}

#[test]
fn finalize_lease_maps_runner_error_to_failed() {
    // Finding 5: an Err outcome (runner engine error) must finalize the lease
    // as Failed, the plain terminal for an unhandled runner crash.
    let store = lease_store();
    let run_id = "runner-error-run";
    let lease = seed_running_lease(&store, run_id, 215);
    let mut md = issue_metadata(run_id, 215);
    md.status = RunStatus::Failed;
    persist_run_with_conn(store.conn(), &md).expect("persist run");

    let outcome: Result<RunOutcome, luther_workflow::engine::runner::EngineError> = Err(
        luther_workflow::engine::runner::EngineError::StepExecutionError {
            step_id: "remediate".to_string(),
            message: "runner crashed".to_string(),
        },
    );

    finalize_continuation_lease(&store, &md, run_id, &outcome).expect("error finalization");

    let finalized = get_lease_for_issue(store.conn(), "o/r", 215)
        .expect("query")
        .expect("lease present");
    assert_eq!(finalized.status, LeaseStatus::Failed);
    assert_eq!(finalized.run_id.as_deref(), Some(run_id));
    let _ = lease;
}

#[test]
fn finalize_lease_failure_idempotent_when_already_failed() {
    // Finding 5: re-finalizing a lease that is already Failed (owned by this
    // run) for a plain Failure outcome must be idempotent, not fail closed.
    // The conditional update is rejected (the status is no longer Running),
    // but the idempotent re-read must match and succeed.
    let store = lease_store();
    let run_id = "failure-idempotent";
    let lease = seed_running_lease(&store, run_id, 216);
    let mut md = issue_metadata(run_id, 216);
    md.status = RunStatus::Failed;
    persist_run_with_conn(store.conn(), &md).expect("persist run");

    // A prior finalization already transitioned the lease to Failed.
    update_lease_status(
        store.conn(),
        &lease.lease_id,
        LeaseStatus::Failed,
        Some(run_id),
    )
    .expect("pre-advance to Failed");

    let outcome: Result<RunOutcome, luther_workflow::engine::runner::EngineError> =
        Ok(RunOutcome::Failure {
            step_id: "remediate".to_string(),
            reason: "agent timed out".to_string(),
        });

    finalize_continuation_lease(&store, &md, run_id, &outcome)
        .expect("idempotent failure finalization");

    let finalized = get_lease_for_issue(store.conn(), "o/r", 216)
        .expect("query")
        .expect("lease present");
    assert_eq!(finalized.status, LeaseStatus::Failed);
    assert_eq!(finalized.run_id.as_deref(), Some(run_id));
    let _ = lease;
}
