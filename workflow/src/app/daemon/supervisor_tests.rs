use super::*;
use luther_workflow::daemon::launcher::DaemonPathBases;
use luther_workflow::daemon::{DaemonState, DaemonStatus, DaemonStore};
use luther_workflow::workflow::schema::WorkflowConfig;

fn base_config_toml() -> String {
    r#"
config_id = "cfg"
workflow_type_id = "wf"

[runtime]
timeout_seconds = 1
max_retries = 0

[repository]
workspace_strategy = "reuse"
branch_template = "issue{issue_number}"
base_branch = "main"

[guards]
"#
    .to_string()
}

fn config_with_vars(pairs: &[(&str, &str)]) -> WorkflowConfig {
    let mut toml = base_config_toml();
    if !pairs.is_empty() {
        toml.push_str("\n[variables]\n");
        for (key, value) in pairs {
            toml.push_str(&format!("{key} = \"{value}\"\n"));
        }
    }
    luther_workflow::workflow::config_loader::parse_workflow_config_toml(&toml)
        .expect("parse test workflow config")
}

#[test]
fn daemon_path_bases_extracts_both_roots_when_present() {
    let cfg = config_with_vars(&[("work_dir", "/tmp/work"), ("artifact_dir", "/tmp/art")]);
    let bases = daemon_path_bases_from_config(&cfg);
    assert_eq!(
        bases.work_dir_base,
        Some(std::path::PathBuf::from("/tmp/work"))
    );
    assert_eq!(
        bases.artifact_dir_base,
        Some(std::path::PathBuf::from("/tmp/art"))
    );
}

#[test]
fn daemon_path_bases_absent_variables_yield_none() {
    let cfg = config_with_vars(&[]);
    let bases = daemon_path_bases_from_config(&cfg);
    assert!(bases.work_dir_base.is_none());
    assert!(bases.artifact_dir_base.is_none());
}

#[test]
fn parent_path_bases_empty_when_no_discovery_parent_config() {
    let cfg = config_with_vars(&[]);
    let map = parent_path_bases_from_config(&cfg, std::path::Path::new("config"));
    assert!(map.is_empty());
}

#[test]
fn write_daemon_heartbeat_resets_failures_on_success() {
    let tmp = tempfile::tempdir().unwrap();
    let store = DaemonStore::at(tmp.path());
    let state = DaemonState::new("cfg-success");
    let mut failures = 2;
    let result = write_daemon_heartbeat(&store, &state, &mut failures);
    assert!(result.is_none());
    assert_eq!(failures, 0);
    // The state file should now exist on disk.
    assert!(store.state_path("cfg-success").exists());
}

#[test]
fn write_daemon_heartbeat_reports_after_max_consecutive_failures() {
    // Point the store at a path that cannot be created (a file where a
    // directory is expected) so every write fails deterministically.
    let tmp = tempfile::tempdir().unwrap();
    let blocker = tmp.path().join("blocker");
    std::fs::write(&blocker, b"not a directory").unwrap();
    let store = DaemonStore::at(&blocker);
    let state = DaemonState::new("cfg-failing");
    let mut failures = 0;

    // The first (MAX - 1) failures accumulate without surfacing an error.
    for _ in 0..(MAX_HEARTBEAT_WRITE_FAILURES - 1) {
        assert!(write_daemon_heartbeat(&store, &state, &mut failures).is_none());
    }
    let surfaced = write_daemon_heartbeat(&store, &state, &mut failures);
    assert!(surfaced.is_some());
    let message = surfaced.unwrap();
    assert!(message.contains("cfg-failing"));
    assert_eq!(failures, MAX_HEARTBEAT_WRITE_FAILURES);
}

#[test]
fn reset_scheduler_failures_zeroes_counter() {
    let mut failures = 7;
    reset_scheduler_failures(&mut failures);
    assert_eq!(failures, 0);
}

#[test]
fn scheduler_join_error_describes_cancelled_and_failed() {
    // A cancelled join produces a cancellation-flavored message.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let cancelled = rt.block_on(async {
        let handle = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        });
        handle.abort();
        handle.await.unwrap_err()
    });
    let message = scheduler_join_error(cancelled);
    assert!(message.contains("cancelled") || message.contains("failed"));
}

#[test]
fn backoff_after_scheduler_failure_grows_and_wakes_on_shutdown() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let mut failures = 0;
    // With shutdown already set, the sleep returns almost immediately while the
    // failure counter still advances and saturates at the max exponent.
    rt.block_on(async {
        for _ in 0..(SCHEDULER_FAILURE_BACKOFF_MAX_EXPONENT + 3) {
            backoff_after_scheduler_failure(&mut failures, &shutdown).await;
        }
    });
    assert_eq!(failures, SCHEDULER_FAILURE_BACKOFF_MAX_EXPONENT + 3);
}

#[test]
fn sleep_secs_with_shutdown_returns_immediately_when_flagged() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let start = std::time::Instant::now();
    rt.block_on(sleep_secs_with_shutdown(30, &shutdown));
    // Because shutdown is set before the first tick, this must not block for
    // anything close to the requested 30 seconds.
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
}

#[test]
fn sleep_secs_with_shutdown_zero_seconds_is_noop() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    rt.block_on(sleep_secs_with_shutdown(0, &shutdown));
}

#[test]
fn heartbeat_loop_exits_promptly_when_shutdown_already_set() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let store = DaemonStore::at(tmp.path());
    let mut state = DaemonState::new("cfg-heartbeat").with_status(DaemonStatus::Running);
    let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let result = rt.block_on(run_daemon_heartbeat_loop(&store, &mut state, &shutdown));
    assert!(result.is_none());
}

#[test]
fn discovery_scheduler_target_carries_config_id_and_path_bases() {
    let cfg = config_with_vars(&[("work_dir", "/tmp/w"), ("artifact_dir", "/tmp/a")]);
    let discovery = luther_workflow::workflow::schema::DiscoveryConfig {
        enabled: true,
        ..Default::default()
    };
    let target = discovery_scheduler_target(
        "my-config",
        &discovery,
        &cfg,
        std::path::Path::new("config"),
    );
    assert_eq!(target.config_id, "my-config");
    assert_eq!(
        target.path_bases.work_dir_base,
        Some(std::path::PathBuf::from("/tmp/w"))
    );
    let _: DaemonPathBases = target.path_bases;
}

/// Seed a pollable wait-state on the given connection whose backing run
/// metadata is deliberately missing, so the next scheduler pass
/// deterministically hits `PollApplyError::RunMissing`.
/// Uses `RateLimitBackoff` so the poller does not invoke `gh`.
fn seed_orphaned_pollable_wait(conn: &rusqlite::Connection) {
    use luther_workflow::persistence::leases::{
        init_leases_table, try_claim, update_lease_status, LeaseStatus,
    };
    use luther_workflow::persistence::sqlite::init_runs_schema;
    use luther_workflow::persistence::wait_state::{
        init_wait_states_table, upsert_wait_state, WaitKind, WaitStateRecord,
    };

    init_runs_schema(conn).expect("init runs schema for orphaned-wait fixture");
    init_leases_table(conn).expect("init leases table for orphaned-wait fixture");
    init_wait_states_table(conn).expect("init wait-states table for orphaned-wait fixture");

    let lease = try_claim(conn, "o/r", 42, "test-cfg")
        .expect("claim lease for orphaned-wait fixture")
        .expect("claim must succeed for a fresh issue");
    update_lease_status(
        conn,
        &lease.lease_id,
        LeaseStatus::WaitingExternal,
        Some("run-orphan"),
    )
    .expect("transition lease to WaitingExternal for orphaned-wait fixture");

    let mut record = WaitStateRecord::new("run-orphan", "test-cfg");
    record.lease_id = Some(lease.lease_id);
    record.repository = "o/r".to_string();
    record.issue_number = 42;
    record.wait_kind = WaitKind::RateLimitBackoff;
    record.poll_interval_seconds = 1;
    record.resume_step = "wait_step".to_string();
    upsert_wait_state(conn, &record).expect("upsert wait-state for orphaned-wait fixture");

    luther_workflow::persistence::checkpoint::save_checkpoint_with_conn(
        conn,
        &luther_workflow::persistence::checkpoint::Checkpoint::new(
            &record.run_id,
            &record.resume_step,
        ),
    )
    .expect("save checkpoint for orphaned-wait fixture");
}

#[test]
fn scheduler_diagnostic_plan_formats_summary_and_bounds_details() {
    use luther_workflow::daemon::poller::ArtifactPhase;
    use luther_workflow::daemon::scheduler::{
        ArtifactWarningDetail, LeaseStatePreservedDetail, RunSummary, SkippedPollDetail,
        SkippedPollReason,
    };

    let skipped_poll_details = (0..12)
        .map(|index| SkippedPollDetail {
            run_id: format!("run-skipped-{index}"),
            lease_id: Some(format!("lease-skipped-{index}")),
            step_id: "watch_pr_checks".to_string(),
            reason: SkippedPollReason::LeaseTransitionRejected,
            lease_transition_reason: Some("lease owner changed"),
        })
        .collect();
    let artifact_warnings = (0..12)
        .map(|index| ArtifactWarningDetail {
            run_id: format!("run-warning-{index}"),
            phase: ArtifactPhase::PollResult,
            error: "disk full".to_string(),
        })
        .collect();
    let lease_state_preserved_details = (0..12)
        .map(|index| LeaseStatePreservedDetail {
            run_id: format!("run-preserved-{index}"),
            current_status: Some(luther_workflow::persistence::leases::LeaseStatus::Completed),
            current_run_id: Some(format!("run-current-{index}")),
        })
        .collect();
    let summary = RunSummary {
        lease_states_preserved: 15,
        lease_state_preserved_details,
        lease_state_preserved_details_dropped: 3,
        skipped_polls: 15,
        skipped_poll_details,
        skipped_poll_details_dropped: 3,
        artifact_warnings,
        artifact_warnings_dropped: 3,
        ..RunSummary::default()
    };

    let plan = scheduler_diagnostic_plan(&summary);
    assert_eq!(plan.preserved_details_to_log, 10);
    assert_eq!(plan.preserved_details_dropped, 5);
    assert_eq!(plan.skipped_details_to_log, 10);
    assert_eq!(plan.skipped_details_dropped, 5);
    assert_eq!(plan.artifact_warnings_to_log, 10);
    assert_eq!(plan.artifact_warnings_dropped, 5);
    assert!(plan
        .summary
        .contains("15 lease states preserved, 5 preserved details dropped"));
    assert!(plan
        .summary
        .contains("15 polls skipped, 5 skip details dropped"));
    assert!(plan
        .summary
        .contains("15 artifact warnings, 5 warning details dropped"));
}

#[test]
fn supervisor_scheduler_pass_reports_orphaned_wait_without_failing() {
    use luther_workflow::daemon::scheduler::{RunSummary, SchedulerError, SkippedPollReason};

    let conn =
        rusqlite::Connection::open_in_memory().expect("open in-memory db for orphaned-wait test");
    seed_orphaned_pollable_wait(&conn);

    let target = SchedulerTarget::new(
        "test-cfg".to_string(),
        luther_workflow::workflow::schema::DiscoveryConfig {
            enabled: false,
            ..Default::default()
        },
        DaemonPathBases::default(),
        std::collections::BTreeMap::new(),
        std::path::PathBuf::from("config"),
    );

    let result: Result<RunSummary, SchedulerError> =
        run_supervisor_scheduler_pass(&[target], &conn);
    let summary = result.expect("orphaned wait must degrade the pass, not abort it");

    assert_eq!(summary.skipped_polls, 1);
    assert_eq!(summary.skipped_poll_details_dropped, 0);
    assert_eq!(summary.skipped_poll_details.len(), 1);
    let detail = &summary.skipped_poll_details[0];
    assert_eq!(detail.run_id, "run-orphan");
    assert_eq!(detail.reason, SkippedPollReason::RunMissing);
    assert_eq!(detail.step_id, "wait_step");
}
