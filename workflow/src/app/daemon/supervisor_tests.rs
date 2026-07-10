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
