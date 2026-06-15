//! Integration tests for `luther monitor` (issue #52).
//!
//! These exercise the pure snapshot/filter API against real persisted rows in
//! a temporary `checkpoints.db`, plus a Ctrl-C/SIGINT lifecycle test asserting
//! the monitor exits cleanly without mutating daemon state.
//!
//! @plan:issue-52
use luther_workflow::monitor::snapshot::{MonitorFilter, RunCounts};
use luther_workflow::persistence::{RunMetadata, RunStatus, SqliteStore};

/// Build a run record with config/issue identifiers set for filtering.
fn seed_run(run_id: &str, config_id: &str, issue: Option<i64>, status: RunStatus) -> RunMetadata {
    let mut md = RunMetadata::new(run_id, "monitor-v1", config_id);
    md.status = status;
    md.current_step = Some("implement".to_string());
    md.issue_number = issue;
    md
}

/// `--config` narrows persisted runs to a single config id.
#[test]
fn monitor_filters_by_config_against_db() {
    let store = SqliteStore::open_in_memory().expect("store");
    store
        .persist_run(&seed_run(
            "run-a",
            "llxprt-code",
            Some(1),
            RunStatus::Running,
        ))
        .expect("persist a");
    store
        .persist_run(&seed_run("run-b", "other", Some(2), RunStatus::Running))
        .expect("persist b");

    let runs = store.list_runs().expect("list");
    let filter = MonitorFilter {
        config: Some("llxprt-code".to_string()),
        ..Default::default()
    };
    let filtered = filter.apply(&runs);
    assert_eq!(filtered.runs.len(), 1);
    assert_eq!(filtered.runs[0].run_id, "run-a");
}

/// `--issue` narrows persisted runs by issue_number.
#[test]
fn monitor_filters_by_issue_against_db() {
    let store = SqliteStore::open_in_memory().expect("store");
    store
        .persist_run(&seed_run("run-a", "cfg", Some(1801), RunStatus::Running))
        .expect("persist a");
    store
        .persist_run(&seed_run("run-b", "cfg", Some(7), RunStatus::Running))
        .expect("persist b");

    let runs = store.list_runs().expect("list");
    let filter = MonitorFilter {
        issue: Some(1801),
        ..Default::default()
    };
    let filtered = filter.apply(&runs);
    assert_eq!(filtered.runs.len(), 1);
    assert_eq!(filtered.runs[0].run_id, "run-a");
}

/// `--run` selects a single run from persisted rows.
#[test]
fn monitor_selects_run_against_db() {
    let store = SqliteStore::open_in_memory().expect("store");
    store
        .persist_run(&seed_run("run-a", "cfg", Some(1), RunStatus::Running))
        .expect("persist a");
    store
        .persist_run(&seed_run("run-b", "cfg", Some(2), RunStatus::Completed))
        .expect("persist b");

    let runs = store.list_runs().expect("list");
    let filter = MonitorFilter {
        run: Some("run-b".to_string()),
        ..Default::default()
    };
    let filtered = filter.apply(&runs);
    assert_eq!(filtered.runs.len(), 1);
    assert_eq!(filtered.selected.as_ref().unwrap().run_id, "run-b");
}

/// Counts derived from persisted rows classify states correctly.
#[test]
fn monitor_counts_from_db_rows() {
    let store = SqliteStore::open_in_memory().expect("store");
    store
        .persist_run(&seed_run("r1", "cfg", None, RunStatus::Running))
        .expect("persist r1");
    store
        .persist_run(&seed_run("r2", "cfg", None, RunStatus::Queued))
        .expect("persist r2");
    store
        .persist_run(&seed_run("r3", "cfg", None, RunStatus::Completed))
        .expect("persist r3");
    store
        .persist_run(&seed_run("r4", "cfg", None, RunStatus::Failed))
        .expect("persist r4");

    let runs = store.list_runs().expect("list");
    let counts = RunCounts::from_runs(&runs);
    assert_eq!(counts.active, 1);
    assert_eq!(counts.queued, 1);
    assert_eq!(counts.completed, 1);
    assert_eq!(counts.failed, 1);
}

/// Ctrl-C/SIGINT stops a continuous monitor cleanly without mutating daemon
/// state (read-only guarantee).
#[cfg(unix)]
#[test]
fn monitor_sigint_exits_cleanly_without_mutation() {
    use std::process::{Command, Stdio};
    use std::time::Duration;

    // Run continuous monitor (no --once/--times) with a small interval.
    let mut child = Command::new(env!("CARGO_BIN_EXE_luther-workflow"))
        .args(["monitor", "--no-clear", "--interval", "1"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn monitor");

    // Allow at least one snapshot to render.
    std::thread::sleep(Duration::from_millis(500));

    // Send SIGINT (Ctrl-C) to the monitor process.
    #[allow(unsafe_code)]
    let rc = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };
    assert_eq!(rc, 0, "SIGINT should be delivered");

    // The monitor should exit promptly and cleanly.
    let mut waited = false;
    for _ in 0..50 {
        if let Ok(Some(_status)) = child.try_wait() {
            waited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if !waited {
        let _ = child.kill();
        panic!("monitor did not exit after SIGINT");
    }
    let status = child.wait().expect("wait monitor");
    assert!(
        status.success() || status.code().is_none(),
        "monitor should exit cleanly on SIGINT, got {status:?}"
    );
}

/// `--times N` renders exactly N snapshots and then exits cleanly (issue #53).
///
/// In `--no-clear` mode each snapshot is prefixed with a separator line, so we
/// count those to assert bounded termination. stdout is captured (not a TTY),
/// so the separator path is taken regardless of `--no-clear`.
#[cfg(unix)]
#[test]
fn monitor_times_renders_exactly_n_snapshots() {
    use std::process::Command;

    let output = Command::new(env!("CARGO_BIN_EXE_luther-workflow"))
        .args(["monitor", "--no-clear", "--times", "3", "--interval", "1"])
        .output()
        .expect("run monitor --times 3");

    assert!(
        output.status.success(),
        "monitor --times 3 should exit 0, got {:?}",
        output.status
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let snapshots = stdout
        .lines()
        .filter(|line| line.contains("===== monitor snapshot @"))
        .count();
    assert_eq!(
        snapshots, 3,
        "monitor --times 3 should render exactly 3 snapshots, got {snapshots}"
    );
}

/// `--once` is sugar for `--times 1`: exactly one snapshot, then exit (issue #53).
#[cfg(unix)]
#[test]
fn monitor_once_renders_single_snapshot() {
    use std::process::Command;

    let output = Command::new(env!("CARGO_BIN_EXE_luther-workflow"))
        .args(["monitor", "--no-clear", "--once"])
        .output()
        .expect("run monitor --once");

    assert!(
        output.status.success(),
        "monitor --once should exit 0, got {:?}",
        output.status
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let snapshots = stdout
        .lines()
        .filter(|line| line.contains("===== monitor snapshot @"))
        .count();
    assert_eq!(
        snapshots, 1,
        "monitor --once should render exactly 1 snapshot, got {snapshots}"
    );
}
