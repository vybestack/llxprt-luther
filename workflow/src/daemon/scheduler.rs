//! Daemon scheduler loop: discover -> claim+launch up to the concurrency limit.
//!
//! `run_once` performs a single discovery/launch pass; `run_loop` recovers
//! stale leases at startup then repeats `run_once` on the configured poll
//! interval until a shutdown flag is set.
//!
//! @plan:PLAN-20260415-DAEMON-DISCOVERY.P06
//! @requirement:REQ-DAEMON-DISCOVERY-006,REQ-DAEMON-DISCOVERY-007

use std::borrow::Cow;
use std::collections::{btree_map::Entry, BTreeMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rusqlite::Connection;

use crate::adapters::github_issues::GithubIssueQuery;
use crate::daemon::discovery::discover;
use crate::daemon::launcher::{
    claim_for_launch, finish_lease_after_result, prepare_resume_lease, DaemonPathBases,
    LaunchOutcome, LaunchRequest, WorkflowLauncher,
};
use crate::daemon::poller::{apply_poll_decision, ExternalWaitPoller, SystemExternalWaitPoller};
use crate::persistence::leases::{
    count_active_leases, count_active_leases_for_config, count_active_leases_for_repository,
    list_ready_to_resume_leases, mark_stale_leases, mark_stale_ready_to_resume_leases, IssueLease,
};
use crate::persistence::wait_state::list_pollable_wait_states;
use crate::workflow::schema::DiscoveryConfig;

/// Summary of a single scheduler pass.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P06
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunSummary {
    pub eligible: usize,
    pub launched: usize,
    pub resumed: usize,
    pub suspended: usize,
    pub failed: usize,
    pub skipped: usize,
    pub pollable_waits: usize,
    pub polls_applied: usize,
}

#[derive(Debug, Clone)]
pub struct SchedulerTarget {
    pub config_id: String,
    pub discovery: DiscoveryConfig,
    /// Resolved daemon base roots for this target's launch config, used to
    /// construct isolated per-run work/artifact directories. @plan:issue-117
    pub path_bases: DaemonPathBases,
    /// Parent-routed base roots keyed by config id, used when a routed parent
    /// issue launches under a parent config id. @plan:issue-117
    pub parent_path_bases: BTreeMap<String, DaemonPathBases>,
}

impl SchedulerTarget {
    #[must_use]
    pub fn new(
        config_id: String,
        discovery: DiscoveryConfig,
        path_bases: DaemonPathBases,
        parent_path_bases: BTreeMap<String, DaemonPathBases>,
    ) -> Self {
        Self {
            config_id,
            discovery,
            path_bases,
            parent_path_bases,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CapacityLimits {
    pub global: usize,
    pub per_config: usize,
    pub per_repository: usize,
}

#[derive(Debug, Clone)]
struct DispatchUnit {
    lease_id: String,
    request: LaunchRequest,
    resume: bool,
}

/// Execute a single discovery + launch pass.
///
/// Discovers eligible issues (accounting for already-active leases), then for
/// each eligible issue attempts `claim_and_launch`, stopping when launches
/// reach the per-config concurrency budget.
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P06
/// @requirement:REQ-DAEMON-DISCOVERY-006
pub fn run_once_with_bases(
    cfg: &DiscoveryConfig,
    q: &dyn GithubIssueQuery,
    conn: &Connection,
    launcher: &dyn WorkflowLauncher,
    config_id: &str,
    path_bases: DaemonPathBases,
    parent_path_bases: BTreeMap<String, DaemonPathBases>,
) -> Result<RunSummary, rusqlite::Error> {
    let poller = SystemExternalWaitPoller::new();
    let target = SchedulerTarget::new(
        config_id.to_string(),
        cfg.clone(),
        path_bases,
        parent_path_bases,
    );
    run_multi_target_once_with_poller(&[target], &[q], conn, launcher, &poller)
}

pub fn run_multi_target_once(
    targets: &[SchedulerTarget],
    queries: &[&dyn GithubIssueQuery],
    conn: &Connection,
    launcher: &dyn WorkflowLauncher,
) -> Result<RunSummary, rusqlite::Error> {
    let poller = SystemExternalWaitPoller::new();
    run_multi_target_once_with_poller(targets, queries, conn, launcher, &poller)
}

pub fn run_multi_target_once_with_poller(
    targets: &[SchedulerTarget],
    queries: &[&dyn GithubIssueQuery],
    conn: &Connection,
    launcher: &dyn WorkflowLauncher,
    poller: &dyn ExternalWaitPoller,
) -> Result<RunSummary, rusqlite::Error> {
    if targets.len() != queries.len() {
        eprintln!(
            "scheduler error: targets len ({}) != queries len ({})",
            targets.len(),
            queries.len()
        );
        return Ok(RunSummary::default());
    }
    let mut summary = poll_due_waits(conn, poller)?;
    let limits = capacity_limits(targets);
    let mut units = collect_resume_units(targets, conn, &limits)?;
    collect_launch_units(targets, queries, conn, &limits, &mut units, &mut summary)?;
    let max_parallel = dispatch_parallelism(&limits, units.len());
    dispatch_units(conn, launcher, units, max_parallel, &mut summary)?;
    Ok(summary)
}

fn collect_resume_units(
    targets: &[SchedulerTarget],
    conn: &Connection,
    limits: &CapacityLimits,
) -> Result<Vec<DispatchUnit>, rusqlite::Error> {
    let mut units = Vec::new();
    for (resume_config_id, discovery) in resume_config_targets(targets, limits) {
        let ready_leases = match list_ready_to_resume_leases(conn, &resume_config_id) {
            Ok(leases) => leases,
            Err(e) => {
                eprintln!("resume discovery error for config={resume_config_id}: {e}");
                continue;
            }
        };
        for lease in ready_leases {
            if !has_capacity(
                conn,
                &discovery,
                &resume_config_id,
                &lease.issue_repo,
                limits,
            )? {
                continue;
            }
            match prepare_resume_unit(&lease, conn) {
                Ok(Some(unit)) => units.push(unit),
                Ok(None) => eprintln!(
                    "resume claim skipped for config={} issue={}#{}: invalid lease state",
                    resume_config_id, lease.issue_repo, lease.issue_number
                ),
                Err(e) => eprintln!(
                    "resume claim skipped for config={} issue={}#{}: {e}",
                    resume_config_id, lease.issue_repo, lease.issue_number
                ),
            }
        }
    }
    Ok(units)
}

fn resume_config_targets(
    targets: &[SchedulerTarget],
    limits: &CapacityLimits,
) -> Vec<(String, DiscoveryConfig)> {
    let mut config_targets: BTreeMap<String, (DiscoveryConfig, bool)> = BTreeMap::new();
    for target in targets {
        upsert_resume_config_target(
            &mut config_targets,
            target.config_id.clone(),
            target.discovery.clone(),
            true,
        );
        if let Some(parent_config_id) = target.discovery.parent_config_id.as_ref() {
            match parent_capacity_discovery(&target.discovery, limits) {
                Ok(discovery) => upsert_resume_config_target(
                    &mut config_targets,
                    parent_config_id.clone(),
                    discovery,
                    false,
                ),
                Err(err) => eprintln!(
                    "parent resume discovery capacity error for config={parent_config_id}: {err}"
                ),
            }
        }
    }
    config_targets
        .into_iter()
        .map(|(config_id, (discovery, _))| (config_id, discovery))
        .collect()
}

fn upsert_resume_config_target(
    config_targets: &mut BTreeMap<String, (DiscoveryConfig, bool)>,
    config_id: String,
    discovery: DiscoveryConfig,
    direct: bool,
) {
    match config_targets.entry(config_id) {
        Entry::Occupied(mut entry) => {
            let (existing_discovery, existing_direct) = entry.get_mut();
            if direct && !*existing_direct {
                *existing_discovery = discovery;
                *existing_direct = true;
            }
        }
        Entry::Vacant(entry) => {
            entry.insert((discovery, direct));
        }
    }
}

fn prepare_resume_unit(
    lease: &IssueLease,
    conn: &Connection,
) -> Result<Option<DispatchUnit>, rusqlite::Error> {
    let Ok(claimed) = prepare_resume_lease(lease, conn)? else {
        return Ok(None);
    };
    Ok(Some(DispatchUnit {
        lease_id: claimed.lease_id,
        request: claimed.request,
        resume: true,
    }))
}

fn collect_launch_units(
    targets: &[SchedulerTarget],
    queries: &[&dyn GithubIssueQuery],
    conn: &Connection,
    limits: &CapacityLimits,
    units: &mut Vec<DispatchUnit>,
    summary: &mut RunSummary,
) -> Result<(), rusqlite::Error> {
    let parent_discoveries = parent_launch_discoveries(targets, limits);
    for (target, query) in targets.iter().zip(queries.iter()) {
        let repo = target.discovery.repo.as_deref().unwrap_or("");
        let active = count_active_leases_for_config(conn, &target.config_id)?;
        let result = match discover(&target.discovery, *query, conn, active) {
            Ok(result) => result,
            Err(e) => {
                eprintln!("discovery error: {e}");
                continue;
            }
        };
        summary.eligible += result.eligible.len();
        for routed in &result.eligible {
            let launch_config_id = routed.config_id.as_deref().unwrap_or(&target.config_id);
            let launch_discovery =
                launch_discovery_for(target, launch_config_id, limits, &parent_discoveries)?;
            if !has_capacity(conn, &launch_discovery, launch_config_id, repo, limits)? {
                summary.skipped += 1;
                continue;
            }
            let launch_path_bases = path_bases_for(target, launch_config_id);
            match claim_for_launch(
                &routed.issue,
                &launch_discovery,
                conn,
                launch_config_id,
                launch_path_bases.as_ref(),
            ) {
                Ok(Ok(mut claimed)) => {
                    claimed.request.workflow_type_id = routed.workflow_type_id.clone();
                    units.push(DispatchUnit {
                        lease_id: claimed.lease_id,
                        request: claimed.request,
                        resume: false,
                    })
                }
                Ok(Err(_)) => summary.skipped += 1,
                Err(e) => {
                    eprintln!(
                        "claim error for config={} issue={}#{}: {e}",
                        launch_config_id, repo, routed.issue.number
                    );
                    summary.failed += 1;
                }
            }
        }
    }
    Ok(())
}

fn launch_discovery_for<'a>(
    target: &'a SchedulerTarget,
    launch_config_id: &str,
    limits: &CapacityLimits,
    parent_discoveries: &'a BTreeMap<String, DiscoveryConfig>,
) -> Result<Cow<'a, DiscoveryConfig>, rusqlite::Error> {
    if launch_config_id == target.config_id {
        return Ok(Cow::Borrowed(&target.discovery));
    }
    if let Some(discovery) = parent_discoveries.get(launch_config_id) {
        return Ok(Cow::Borrowed(discovery));
    }
    parent_capacity_discovery(&target.discovery, limits).map(Cow::Owned)
}

/// Select the daemon path bases for a routed launch.
///
/// When the launch config id matches the target's own config, use the target's
/// own bases. When the launch config id is a parent config, use the
/// parent-routed bases registered for that parent config id, falling back to
/// empty bases (engine fallbacks apply) with a warning. @plan:issue-117
fn path_bases_for<'a>(
    target: &'a SchedulerTarget,
    launch_config_id: &str,
) -> Cow<'a, DaemonPathBases> {
    if launch_config_id == target.config_id {
        return Cow::Borrowed(&target.path_bases);
    }
    match target.parent_path_bases.get(launch_config_id) {
        Some(bases) => Cow::Borrowed(bases),
        None => {
            tracing::warn!(
                launch_config_id,
                "no daemon path bases found for routed parent config; using empty path bases"
            );
            Cow::Owned(DaemonPathBases::default())
        }
    }
}

fn parent_launch_discoveries(
    targets: &[SchedulerTarget],
    limits: &CapacityLimits,
) -> BTreeMap<String, DiscoveryConfig> {
    let mut discoveries = BTreeMap::new();
    let mut derived = BTreeMap::new();
    for target in targets {
        discoveries.insert(target.config_id.clone(), target.discovery.clone());
        if let Some(parent_config_id) = target.discovery.parent_config_id.as_ref() {
            match parent_capacity_discovery(&target.discovery, limits) {
                Ok(discovery) => {
                    derived.entry(parent_config_id.clone()).or_insert(discovery);
                }
                Err(err) => eprintln!(
                    "parent launch discovery capacity error for config={}: parent_config={parent_config_id}: {err}",
                    target.config_id
                ),
            }
        }
    }
    for (config_id, discovery) in derived {
        discoveries.entry(config_id).or_insert(discovery);
    }
    discoveries
}

fn parent_capacity_discovery(
    discovery: &DiscoveryConfig,
    limits: &CapacityLimits,
) -> Result<DiscoveryConfig, rusqlite::Error> {
    let mut parent = discovery.clone();
    let limit = discovery
        .max_concurrent_runs_per_config
        .or(discovery.max_concurrent_runs)
        .map_or(limits.per_config, |value| value as usize);
    let limit = u32::try_from(limit).map_err(|_| {
        rusqlite::Error::IntegralValueOutOfRange(0, i64::try_from(limit).unwrap_or(i64::MAX))
    })?;
    parent.max_concurrent_runs = Some(limit);
    parent.max_concurrent_runs_per_config = Some(limit);
    Ok(parent)
}

fn dispatch_units(
    conn: &Connection,
    launcher: &dyn WorkflowLauncher,
    units: Vec<DispatchUnit>,
    max_parallel: usize,
    summary: &mut RunSummary,
) -> Result<(), rusqlite::Error> {
    let max_parallel = max_parallel.max(1);
    for chunk in units.chunks(max_parallel) {
        dispatch_unit_chunk(conn, launcher, chunk, summary)?;
    }
    Ok(())
}

fn dispatch_unit_chunk(
    conn: &Connection,
    launcher: &dyn WorkflowLauncher,
    units: &[DispatchUnit],
    summary: &mut RunSummary,
) -> Result<(), rusqlite::Error> {
    thread::scope(|scope| {
        let handles: Vec<_> = units
            .iter()
            .map(|unit| {
                let lease_id = unit.lease_id.clone();
                let run_id = unit.request.run_id.clone();
                let resume = unit.resume;
                let handle = scope.spawn(move || {
                    if resume {
                        launcher.resume(&unit.request)
                    } else {
                        launcher.launch(&unit.request)
                    }
                });
                (lease_id, run_id, resume, handle)
            })
            .collect();
        for (lease_id, run_id, resume, handle) in handles {
            let result = match handle.join() {
                Ok(result) => result,
                Err(_) => Err("launcher thread panicked".to_string()),
            };
            let outcome = finish_lease_after_result(conn, &lease_id, &run_id, result)?;
            record_outcome(outcome, resume, summary);
        }
        Ok(())
    })
}

fn dispatch_parallelism(limits: &CapacityLimits, unit_count: usize) -> usize {
    unit_count.min(limits.global).max(1)
}

fn capacity_limits(targets: &[SchedulerTarget]) -> CapacityLimits {
    CapacityLimits {
        global: targets
            .iter()
            .filter_map(|target| target.discovery.max_concurrent_active_runs)
            .max()
            .unwrap_or(u32::MAX) as usize,
        per_config: targets
            .iter()
            .filter_map(|target| {
                target
                    .discovery
                    .max_concurrent_runs_per_config
                    .or(target.discovery.max_concurrent_runs)
            })
            .max()
            .unwrap_or(1) as usize,
        per_repository: targets
            .iter()
            .filter_map(|target| target.discovery.max_concurrent_runs_per_repository)
            .max()
            .unwrap_or(u32::MAX) as usize,
    }
}

fn has_capacity(
    conn: &Connection,
    cfg: &DiscoveryConfig,
    config_id: &str,
    repo: &str,
    limits: &CapacityLimits,
) -> Result<bool, rusqlite::Error> {
    let config_limit = cfg
        .max_concurrent_runs_per_config
        .or(cfg.max_concurrent_runs)
        .map_or(limits.per_config, |v| v as usize);
    let repo_limit = cfg
        .max_concurrent_runs_per_repository
        .map_or(limits.per_repository, |v| v as usize);
    Ok(count_active_leases(conn)? < limits.global
        && count_active_leases_for_config(conn, config_id)? < config_limit
        && count_active_leases_for_repository(conn, repo)? < repo_limit)
}

fn record_outcome(outcome: LaunchOutcome, was_resume: bool, summary: &mut RunSummary) {
    match outcome {
        LaunchOutcome::Launched { success: true, .. } if was_resume => summary.resumed += 1,
        LaunchOutcome::Launched { success: true, .. } => summary.launched += 1,
        LaunchOutcome::Launched { success: false, .. } => summary.failed += 1,
        LaunchOutcome::WaitingExternal { .. } => summary.suspended += 1,
        LaunchOutcome::Skipped(_) => summary.skipped += 1,
    }
}

fn poll_due_waits(
    conn: &Connection,
    poller: &dyn ExternalWaitPoller,
) -> Result<RunSummary, rusqlite::Error> {
    let waits = list_pollable_wait_states(conn, chrono::Utc::now())?;
    let mut summary = RunSummary {
        pollable_waits: waits.len(),
        ..RunSummary::default()
    };
    for wait in waits {
        let decision = poller.poll(&wait);
        apply_poll_decision(conn, &wait, &decision)?;
        summary.polls_applied += 1;
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
    target: SchedulerTarget,
    q: &dyn GithubIssueQuery,
    conn: &Connection,
    launcher: &dyn WorkflowLauncher,
    shutdown: Arc<AtomicBool>,
    stale_timeout_secs: u64,
) -> Result<(), rusqlite::Error> {
    let recovered = mark_stale_leases(conn, stale_timeout_secs)?;
    let ready_recovered = mark_stale_ready_to_resume_leases(conn, stale_timeout_secs)?;
    if recovered > 0 || ready_recovered > 0 {
        println!(
            "recovered {recovered} active stale lease(s) and {ready_recovered} ready-to-resume stale lease(s) on startup"
        );
    }

    let poll = target.discovery.poll_interval_secs.unwrap_or(300);
    while !shutdown.load(Ordering::SeqCst) {
        let summary = run_multi_target_once(std::slice::from_ref(&target), &[q], conn, launcher)?;
        if summary.launched > 0
            || summary.resumed > 0
            || summary.suspended > 0
            || summary.failed > 0
        {
            println!(
                "scheduler pass: {} launched, {} resumed, {} suspended, {} failed, {} skipped",
                summary.launched,
                summary.resumed,
                summary.suspended,
                summary.failed,
                summary.skipped
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
    use crate::daemon::launcher::{LaunchRequest, WorkflowLaunchResult};
    use crate::daemon::poller::{ExternalWaitPoller, PollDecision};
    use crate::persistence::leases::{
        count_active_leases, get_lease_for_issue, init_leases_table, try_claim,
        update_lease_status, LeaseStatus,
    };
    use crate::persistence::wait_state::WaitStateRecord;
    use std::sync::Mutex;

    fn cfg(max: u32) -> DiscoveryConfig {
        DiscoveryConfig {
            enabled: true,
            repo: Some("o/r".to_string()),
            include_labels: vec!["ok".to_string()],
            exclude_labels: vec![],
            active_parent_label: None,
            issue_states: vec!["open".to_string()],
            assignee_filter: None,
            milestone_order: Some("none".to_string()),
            max_concurrent_runs: Some(max),
            poll_interval_secs: Some(300),
            max_concurrent_active_runs: None,
            max_concurrent_runs_per_repository: None,
            max_concurrent_runs_per_config: None,
            route_parent_issues: false,
            parent_workflow_type_id: Some("parent-issue-orchestrator-v1".to_string()),
            parent_config_id: None,
            skip_children_of_active_parents: false,
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
            body: None,
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
        launched: Mutex<Vec<u64>>,
    }
    impl WorkflowLauncher for MockLauncher {
        fn launch(&self, request: &LaunchRequest) -> Result<WorkflowLaunchResult, String> {
            self.launched.lock().unwrap().push(request.issue_number);
            Ok(WorkflowLaunchResult::CompletedSuccess)
        }
    }
    struct ReadyPoller;

    impl ExternalWaitPoller for ReadyPoller {
        fn poll(&self, record: &WaitStateRecord) -> PollDecision {
            PollDecision::ready(record, serde_json::json!({ "state": "ready" }))
        }
    }

    #[test]
    fn due_wait_states_are_polled_and_resumed_before_new_discovery() {
        let c = conn();
        let lease = try_claim(&c, "o/r", 99, "cfg").unwrap().unwrap();
        update_lease_status(
            &c,
            &lease.lease_id,
            LeaseStatus::WaitingExternal,
            Some("run-wait"),
        )
        .unwrap();
        let mut wait = WaitStateRecord::new("run-wait", "cfg");
        wait.lease_id = Some(lease.lease_id);
        wait.repository = "o/r".to_string();
        wait.issue_number = 99;
        wait.resume_step = "watch_pr_checks".to_string();
        crate::persistence::wait_state::upsert_wait_state(&c, &wait).unwrap();
        let q = MockQuery {
            issues: vec![issue(1)],
        };
        let l = MockLauncher {
            launched: Mutex::new(vec![]),
        };

        let target = SchedulerTarget::new(
            "cfg".to_string(),
            cfg(1),
            DaemonPathBases::default(),
            BTreeMap::new(),
        );
        let summary = run_multi_target_once_with_poller(
            &[target],
            &[&q as &dyn GithubIssueQuery],
            &c,
            &l,
            &ReadyPoller,
        )
        .unwrap();

        assert_eq!(summary.pollable_waits, 1);
        assert_eq!(summary.polls_applied, 1);
        assert_eq!(summary.resumed, 1);
        assert_eq!(summary.launched, 0);
        assert_eq!(l.launched.lock().unwrap().as_slice(), &[99]);
    }

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::persistence::sqlite::init_runs_schema(&c).unwrap();
        init_leases_table(&c).unwrap();
        crate::persistence::wait_state::init_wait_states_table(&c).unwrap();
        crate::persistence::checkpoint::save_checkpoint_with_conn(
            &c,
            &crate::persistence::checkpoint::Checkpoint::new("run-wait", "watch_pr_checks"),
        )
        .unwrap();
        let mut metadata = crate::persistence::RunMetadata::new("run-wait", "wf", "cfg");
        metadata.set_current_step("watch_pr_checks");
        crate::persistence::persist_run_with_conn(&c, &metadata).unwrap();
        c
    }

    #[test]
    fn run_once_launches_up_to_limit() {
        let c = conn();
        let q = MockQuery {
            issues: vec![issue(1), issue(2), issue(3)],
        };
        let l = MockLauncher {
            launched: Mutex::new(vec![]),
        };
        let summary = run_once_with_bases(
            &cfg(2),
            &q,
            &c,
            &l,
            "cfg",
            DaemonPathBases::default(),
            BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(summary.eligible, 2);
        assert_eq!(summary.launched, 2);
        assert_eq!(l.launched.lock().unwrap().len(), 2);
    }

    #[test]
    fn second_pass_prevents_duplicate_launch() {
        let c = conn();
        let q = MockQuery {
            issues: vec![issue(1)],
        };
        let l = MockLauncher {
            launched: Mutex::new(vec![]),
        };
        // First pass launches and completes issue 1.
        run_once_with_bases(
            &cfg(2),
            &q,
            &c,
            &l,
            "cfg",
            DaemonPathBases::default(),
            BTreeMap::new(),
        )
        .unwrap();
        // Manually re-mark the completed lease active to emulate a still-open
        // claim; a second pass must not relaunch it.
        let lease = get_lease_for_issue(&c, "o/r", 1).unwrap().unwrap();
        update_lease_status(&c, &lease.lease_id, LeaseStatus::Running, None).unwrap();
        let summary2 = run_once_with_bases(
            &cfg(2),
            &q,
            &c,
            &l,
            "cfg",
            DaemonPathBases::default(),
            BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(
            summary2.eligible, 0,
            "active lease should suppress eligibility"
        );
        assert_eq!(l.launched.lock().unwrap().len(), 1);
    }

    #[test]
    fn due_wait_states_are_reported_without_consuming_capacity() {
        let c = conn();
        let lease = try_claim(&c, "o/r", 99, "cfg").unwrap().unwrap();
        update_lease_status(
            &c,
            &lease.lease_id,
            LeaseStatus::WaitingExternal,
            Some("run-wait"),
        )
        .unwrap();
        let mut wait = crate::persistence::wait_state::WaitStateRecord::new("run-wait", "cfg");
        wait.lease_id = Some(lease.lease_id);
        wait.repository = "o/r".to_string();
        wait.issue_number = 99;
        wait.resume_step = "watch_pr_checks".to_string();
        crate::persistence::wait_state::upsert_wait_state(&c, &wait).unwrap();
        let q = MockQuery {
            issues: vec![issue(1)],
        };
        let l = MockLauncher {
            launched: Mutex::new(vec![]),
        };
        let target = SchedulerTarget::new(
            "cfg".to_string(),
            cfg(1),
            DaemonPathBases::default(),
            BTreeMap::new(),
        );
        let summary = run_multi_target_once_with_poller(
            &[target],
            &[&q as &dyn GithubIssueQuery],
            &c,
            &l,
            &ReadyPoller,
        )
        .unwrap();
        assert_eq!(summary.pollable_waits, 1);
        assert_eq!(summary.resumed, 1);
        assert_eq!(summary.launched, 0);
    }

    #[test]
    fn multi_target_respects_global_and_repository_limits() {
        let c = conn();
        let targets = vec![
            SchedulerTarget::new(
                "cfg-a".to_string(),
                DiscoveryConfig {
                    max_concurrent_active_runs: Some(2),
                    max_concurrent_runs_per_repository: Some(1),
                    max_concurrent_runs: Some(2),
                    ..cfg(2)
                },
                DaemonPathBases::default(),
                BTreeMap::new(),
            ),
            SchedulerTarget::new(
                "cfg-b".to_string(),
                DiscoveryConfig {
                    repo: Some("o/other".to_string()),
                    max_concurrent_active_runs: Some(2),
                    max_concurrent_runs_per_repository: Some(1),
                    max_concurrent_runs: Some(2),
                    ..cfg(2)
                },
                DaemonPathBases::default(),
                BTreeMap::new(),
            ),
        ];
        let q1 = MockQuery {
            issues: vec![issue(1), issue(2)],
        };
        let q2 = MockQuery {
            issues: vec![issue(3), issue(4)],
        };
        let queries: Vec<&dyn GithubIssueQuery> = vec![&q1, &q2];
        let l = MockLauncher {
            launched: Mutex::new(vec![]),
        };

        let summary = run_multi_target_once(&targets, &queries, &c, &l).unwrap();

        assert_eq!(summary.launched, 2);
        assert_eq!(l.launched.lock().unwrap().len(), 2);
        assert_eq!(count_active_leases(&c).unwrap(), 0);
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
            launched: Mutex::new(vec![]),
        };
        let shutdown = Arc::new(AtomicBool::new(true)); // stop immediately after startup sweep
        let target = SchedulerTarget::new(
            "cfg".to_string(),
            cfg(1),
            DaemonPathBases::default(),
            BTreeMap::new(),
        );
        run_loop(target, &q, &c, &l, shutdown, 300).unwrap();
        let recovered = get_lease_for_issue(&c, "o/r", 9).unwrap().unwrap();
        assert_eq!(recovered.status, LeaseStatus::Stale);
    }

    #[test]
    fn parent_routed_launch_without_parent_bases_warns_and_uses_empty_fallback() {
        let target = SchedulerTarget::new(
            "child-cfg".to_string(),
            DiscoveryConfig {
                parent_config_id: Some("parent-cfg".to_string()),
                ..cfg(1)
            },
            DaemonPathBases {
                work_dir_base: Some(std::path::PathBuf::from("/tmp/child-work")),
                artifact_dir_base: Some(std::path::PathBuf::from("/tmp/child-artifacts")),
            },
            BTreeMap::new(),
        );

        let bases = path_bases_for(&target, "parent-cfg");

        assert_eq!(bases.as_ref(), &DaemonPathBases::default());
    }

    #[test]
    fn sleep_with_shutdown_returns_early() {
        let shutdown = Arc::new(AtomicBool::new(true));
        let start = std::time::Instant::now();
        sleep_with_shutdown(300, &shutdown);
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
