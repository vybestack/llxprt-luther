//! Aggregate multi-config status integration tests (issue #53).
//!
//! These verify that, across two independent daemon configs (`daemon-config-a`
//! and `daemon-config-b`), the building blocks behind `status` /
//! `daemon status` aggregate both configs, scope to one config via `--config`,
//! and filter to one run via `--run-id`.
//!
//! State is seeded through the public library APIs (`DaemonStore::at`,
//! `SqliteStore`, `MonitorFilter`) using isolated temp roots so the real data
//! directory is never touched. This mirrors the pattern used by
//! `daemon_lifecycle_integration.rs` and `monitor_integration.rs`.

use luther_workflow::daemon::{DaemonState, DaemonStatus, DaemonStore};
use luther_workflow::monitor::snapshot::MonitorFilter;
use luther_workflow::persistence::{RunMetadata, RunStatus, SqliteStore};
use tempfile::TempDir;

const CONFIG_A: &str = "daemon-config-a";
const CONFIG_B: &str = "daemon-config-b";

fn seed_run(run_id: &str, config_id: &str, status: RunStatus) -> RunMetadata {
    let mut md = RunMetadata::new(run_id, "hello-world-v1", config_id);
    md.status = status;
    md.current_step = Some("implement".to_string());
    md
}

#[test]
fn aggregate_daemon_status_lists_both_configs() {
    let tmp = TempDir::new().expect("tempdir");
    let store = DaemonStore::at(tmp.path());

    store
        .write(
            &DaemonState::new(CONFIG_A)
                .with_pid(101)
                .with_status(DaemonStatus::Running),
        )
        .expect("write a");
    store
        .write(
            &DaemonState::new(CONFIG_B)
                .with_pid(202)
                .with_status(DaemonStatus::Running),
        )
        .expect("write b");

    let all = store.read_all();
    let ids: Vec<&str> = all.iter().map(|s| s.config_id.as_str()).collect();
    assert!(ids.contains(&CONFIG_A), "config a should be visible");
    assert!(ids.contains(&CONFIG_B), "config b should be visible");
    assert_eq!(all.len(), 2, "aggregate should list exactly both configs");
}

#[test]
fn daemon_status_scopes_to_single_config() {
    let tmp = TempDir::new().expect("tempdir");
    let store = DaemonStore::at(tmp.path());

    store.write(&DaemonState::new(CONFIG_A)).expect("write a");
    store.write(&DaemonState::new(CONFIG_B)).expect("write b");

    let only_a = store.read(CONFIG_A).expect("config a present");
    assert_eq!(only_a.config_id, CONFIG_A);
    assert!(store.read("nonexistent-config").is_none());
}

#[test]
fn aggregate_run_view_includes_both_configs() {
    let tmp = TempDir::new().expect("tempdir");
    let db_path = tmp.path().join("checkpoints.db");
    let store = SqliteStore::open(&db_path).expect("open store");

    store
        .persist_run(&seed_run("run-a1", CONFIG_A, RunStatus::Running))
        .expect("persist a1");
    store
        .persist_run(&seed_run("run-b1", CONFIG_B, RunStatus::Completed))
        .expect("persist b1");

    let runs = store.list_runs().expect("list runs");
    let configs: Vec<&str> = runs.iter().map(|r| r.config_id.as_str()).collect();
    assert!(configs.contains(&CONFIG_A));
    assert!(configs.contains(&CONFIG_B));
    assert_eq!(runs.len(), 2, "aggregate run view should include both runs");
}

#[test]
fn run_view_scopes_by_config() {
    let tmp = TempDir::new().expect("tempdir");
    let db_path = tmp.path().join("checkpoints.db");
    let store = SqliteStore::open(&db_path).expect("open store");

    store
        .persist_run(&seed_run("run-a1", CONFIG_A, RunStatus::Running))
        .expect("persist a1");
    store
        .persist_run(&seed_run("run-b1", CONFIG_B, RunStatus::Running))
        .expect("persist b1");

    let runs = store.list_runs().expect("list runs");
    let filter = MonitorFilter {
        config: Some(CONFIG_A.to_string()),
        ..Default::default()
    };
    let filtered = filter.apply(&runs);
    assert_eq!(
        filtered.runs.len(),
        1,
        "--config should scope to one config"
    );
    assert_eq!(filtered.runs[0].config_id, CONFIG_A);
}

#[test]
fn run_view_filters_by_run_id() {
    let tmp = TempDir::new().expect("tempdir");
    let db_path = tmp.path().join("checkpoints.db");
    let store = SqliteStore::open(&db_path).expect("open store");

    store
        .persist_run(&seed_run("run-a1", CONFIG_A, RunStatus::Running))
        .expect("persist a1");
    store
        .persist_run(&seed_run("run-b1", CONFIG_B, RunStatus::Running))
        .expect("persist b1");

    let runs = store.list_runs().expect("list runs");
    let filter = MonitorFilter {
        run: Some("run-b1".to_string()),
        ..Default::default()
    };
    let filtered = filter.apply(&runs);
    assert_eq!(filtered.runs.len(), 1, "--run-id should filter to one run");
    assert_eq!(filtered.runs[0].run_id, "run-b1");
    assert_eq!(
        filtered.selected.as_ref().expect("selected run").run_id,
        "run-b1"
    );
}
