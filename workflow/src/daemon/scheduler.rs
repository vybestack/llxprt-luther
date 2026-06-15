//! Daemon scheduler loop: discover -> claim+launch up to the concurrency limit.
//!
//! `run_once` performs a single discovery/launch pass; `run_loop` recovers
//! stale leases at startup then repeats `run_once` on the configured poll
//! interval until a shutdown flag is set.
//!
//! @plan:PLAN-20260415-DAEMON-DISCOVERY.P06
//! @requirement:REQ-DAEMON-DISCOVERY-006,REQ-DAEMON-DISCOVERY-007

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rusqlite::Connection;

use crate::adapters::github_issues::GithubIssueQuery;
use crate::daemon::discovery::discover;
use crate::daemon::launcher::{claim_and_launch, LaunchOutcome, WorkflowLauncher};
use crate::persistence::leases::{count_active_leases_for_config, mark_stale_leases};
use crate::workflow::schema::DiscoveryConfig;

/// Summary of a single scheduler pass.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P06
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunSummary {
    pub eligible: usize,
    pub launched: usize,
    pub failed: usize,
    pub skipped: usize,
}

/// Execute a single discovery + launch pass.
///
/// Discovers eligible issues (accounting for already-active leases), then for
/// each eligible issue attempts `claim_and_launch`, stopping when launches
/// reach the per-config concurrency budget.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P06
/// @requirement:REQ-DAEMON-DISCOVERY-006
pub fn run_once(
    cfg: &DiscoveryConfig,
    q: &dyn GithubIssueQuery,
    conn: &Connection,
    launcher: &dyn WorkflowLauncher,
    config_id: &str,
) -> Result<RunSummary, rusqlite::Error> {
    let active = count_active_leases_for_config(conn, config_id)?;
    let result = match discover(cfg, q, conn, active) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("discovery error: {e}");
            return Ok(RunSummary::default());
        }
    };

    let mut summary = RunSummary {
        eligible: result.eligible.len(),
        ..RunSummary::default()
    };

    for issue in &result.eligible {
        match claim_and_launch(issue, cfg, conn, launcher, config_id)? {
            LaunchOutcome::Launched { success: true, .. } => summary.launched += 1,
            LaunchOutcome::Launched { success: false, .. } => summary.failed += 1,
            LaunchOutcome::Skipped(_) => summary.skipped += 1,
        }
    }
    Ok(summary)
}

/// Run the scheduler loop until `shutdown` is set.
///
/// Recovers stale leases once at startup (so a crashed previous instance does
/// not permanently block issues), then repeats `run_once` and sleeps the
/// configured poll interval, checking the shutdown flag frequently for
/// responsiveness.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P06
/// @requirement:REQ-DAEMON-DISCOVERY-007
pub fn run_loop(
    cfg: &DiscoveryConfig,
    q: &dyn GithubIssueQuery,
    conn: &Connection,
    launcher: &dyn WorkflowLauncher,
    config_id: &str,
    shutdown: Arc<AtomicBool>,
    stale_timeout_secs: u64,
) -> Result<(), rusqlite::Error> {
    let recovered = mark_stale_leases(conn, stale_timeout_secs)?;
    if recovered > 0 {
        println!("recovered {recovered} stale lease(s) on startup");
    }

    let poll = cfg.poll_interval_secs.unwrap_or(300);
    while !shutdown.load(Ordering::SeqCst) {
        let summary = run_once(cfg, q, conn, launcher, config_id)?;
        if summary.launched > 0 || summary.failed > 0 {
            println!(
                "scheduler pass: {} launched, {} failed, {} skipped",
                summary.launched, summary.failed, summary.skipped
            );
        }
        sleep_with_shutdown(poll, &shutdown);
    }
    Ok(())
}

/// Sleep up to `secs` seconds, waking early if shutdown is requested.
fn sleep_with_shutdown(secs: u64, shutdown: &Arc<AtomicBool>) {
    let ticks = secs.saturating_mul(5); // 200ms granularity
    for _ in 0..ticks {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::github::GithubError;
    use crate::adapters::github_issues::GithubIssue;
    use crate::daemon::launcher::LaunchRequest;
    use crate::persistence::leases::{
        get_lease_for_issue, init_leases_table, try_claim, update_lease_status, LeaseStatus,
    };
    use std::cell::RefCell;

    fn cfg(max: u32) -> DiscoveryConfig {
        DiscoveryConfig {
            enabled: true,
            repo: Some("o/r".to_string()),
            include_labels: vec!["ok".to_string()],
            exclude_labels: vec![],
            issue_states: vec!["open".to_string()],
            assignee_filter: None,
            milestone_order: Some("none".to_string()),
            max_concurrent_runs: Some(max),
            poll_interval_secs: Some(300),
        }
    }

    fn issue(number: u64) -> GithubIssue {
        GithubIssue {
            number,
            title: format!("Issue {number}"),
            state: "open".to_string(),
            labels: vec!["ok".to_string()],
            assignee: None,
            milestone: None,
        }
    }

    struct MockQuery {
        issues: Vec<GithubIssue>,
    }
    impl GithubIssueQuery for MockQuery {
        fn list_issues(
            &self,
            _r: &str,
            _l: &[String],
            _s: &[String],
        ) -> Result<Vec<GithubIssue>, GithubError> {
            Ok(self.issues.clone())
        }
        fn has_open_pr_for_issue(&self, _r: &str, _n: u64) -> Result<bool, GithubError> {
            Ok(false)
        }
        fn list_milestones(&self, _r: &str) -> Result<Vec<String>, GithubError> {
            Ok(vec![])
        }
    }

    struct MockLauncher {
        launched: RefCell<Vec<u64>>,
    }
    impl WorkflowLauncher for MockLauncher {
        fn launch(&self, request: &LaunchRequest) -> Result<bool, String> {
            self.launched.borrow_mut().push(request.issue_number);
            Ok(true)
        }
    }

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        init_leases_table(&c).unwrap();
        c
    }

    #[test]
    fn run_once_launches_up_to_limit() {
        let c = conn();
        let q = MockQuery {
            issues: vec![issue(1), issue(2), issue(3)],
        };
        let l = MockLauncher {
            launched: RefCell::new(vec![]),
        };
        let summary = run_once(&cfg(2), &q, &c, &l, "cfg").unwrap();
        assert_eq!(summary.eligible, 2);
        assert_eq!(summary.launched, 2);
        assert_eq!(l.launched.borrow().len(), 2);
    }

    #[test]
    fn second_pass_prevents_duplicate_launch() {
        let c = conn();
        let q = MockQuery {
            issues: vec![issue(1)],
        };
        let l = MockLauncher {
            launched: RefCell::new(vec![]),
        };
        // First pass launches and completes issue 1.
        run_once(&cfg(2), &q, &c, &l, "cfg").unwrap();
        // Manually re-mark the completed lease active to emulate a still-open
        // claim; a second pass must not relaunch it.
        let lease = get_lease_for_issue(&c, "o/r", 1).unwrap().unwrap();
        update_lease_status(&c, &lease.lease_id, LeaseStatus::Running, None).unwrap();
        let summary2 = run_once(&cfg(2), &q, &c, &l, "cfg").unwrap();
        assert_eq!(
            summary2.eligible, 0,
            "active lease should suppress eligibility"
        );
        assert_eq!(l.launched.borrow().len(), 1);
    }

    #[test]
    fn run_loop_recovers_stale_then_stops() {
        let c = conn();
        // Insert a stale running lease (old heartbeat).
        let stale = try_claim(&c, "o/r", 9, "cfg").unwrap().unwrap();
        update_lease_status(&c, &stale.lease_id, LeaseStatus::Running, None).unwrap();
        let old = (chrono::Utc::now() - chrono::Duration::seconds(10_000)).to_rfc3339();
        c.execute(
            "UPDATE issue_leases SET heartbeat_at = ?1 WHERE lease_id = ?2",
            rusqlite::params![old, stale.lease_id],
        )
        .unwrap();

        let q = MockQuery { issues: vec![] };
        let l = MockLauncher {
            launched: RefCell::new(vec![]),
        };
        let shutdown = Arc::new(AtomicBool::new(true)); // stop immediately after startup sweep
        run_loop(&cfg(1), &q, &c, &l, "cfg", shutdown, 300).unwrap();
        let recovered = get_lease_for_issue(&c, "o/r", 9).unwrap().unwrap();
        assert_eq!(recovered.status, LeaseStatus::Stale);
    }

    #[test]
    fn sleep_with_shutdown_returns_early() {
        let shutdown = Arc::new(AtomicBool::new(true));
        let start = std::time::Instant::now();
        sleep_with_shutdown(300, &shutdown);
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
