use super::*;
use crate::persistence::leases::count_active_leases;

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
    seed_claim_receipt(&c, &lease.lease_id);
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

#[test]
fn run_once_launches_up_to_limit() {
    // Capacity test: with max_concurrent_runs=2 and three eligible issues,
    // exactly two must launch and the third must NOT be launched or claimed
    // active. This proves the per-config ceiling genuinely stops over-launch,
    // not just that discovery happened to return two.
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
    assert_eq!(
        summary.eligible, 2,
        "discovery applies its capacity limit before returning eligible issues"
    );
    assert_eq!(
        summary.skipped, 0,
        "discovery-level capacity skips are not scheduler dispatch skips"
    );
    assert_eq!(summary.launched, 2, "exactly two issues launched");
    let launched_issues = l.launched.lock().unwrap().clone();
    assert_eq!(launched_issues.len(), 2);
    assert!(
        !launched_issues.contains(&3),
        "issue 3 must not be launched when capacity is exhausted"
    );
    assert!(
        get_lease_for_issue(&c, "o/r", 3).unwrap().is_none(),
        "capacity is checked before claim_for_launch, so issue 3 must have no lease"
    );
    // MockLauncher completes launches synchronously, so both claimed leases
    // must already be terminal when this scheduler pass returns.
    assert_eq!(
        count_active_leases(&c).unwrap(),
        0,
        "no active leases remain after both runs complete"
    );
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
fn resumed_waits_participate_in_capacity_accounting() {
    let c = conn();
    let lease = try_claim(&c, "o/r", 99, "cfg").unwrap().unwrap();
    seed_claim_receipt(&c, &lease.lease_id);
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
    assert_eq!(
        summary.eligible, 0,
        "the prepared resume occupies the only slot before discovery"
    );
    assert_eq!(l.launched.lock().unwrap().as_slice(), &[99]);
    assert!(
        get_lease_for_issue(&c, "o/r", 1).unwrap().is_none(),
        "issue 1 must not be claimed when capacity is exhausted"
    );
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
    let mut launched = l.launched.lock().unwrap().clone();
    launched.sort_unstable();
    assert_eq!(
        launched,
        vec![1, 3],
        "one issue from each repository must launch"
    );
    // MockLauncher completes each launch synchronously, so no active lease
    // remains after the scheduler returns.
    assert_eq!(count_active_leases(&c).unwrap(), 0);
}

#[test]
fn mismatched_targets_and_queries_returns_structured_error() {
    // The targets.len() != queries.len() guard is an internal invariant that
    // can never fire under correct usage (every production caller builds the
    // query slice 1:1 from the target slice). When it does fire, the scheduler
    // must surface a structured SchedulerError::TargetsQueriesMismatch rather
    // than silently returning an empty summary, so the degraded pass is not
    // mistaken for "no eligible issues".
    let c = conn();
    let target = SchedulerTarget::new(
        "cfg".to_string(),
        cfg(1),
        DaemonPathBases::default(),
        BTreeMap::new(),
    );
    let q = MockQuery { issues: vec![] };
    let l = MockLauncher {
        launched: Mutex::new(vec![]),
    };
    // Two targets but only one query — mismatch.
    let targets = vec![target.clone(), target];
    let queries: Vec<&dyn GithubIssueQuery> = vec![&q];

    let result = run_multi_target_once(&targets, &queries, &c, &l);

    match result {
        Err(SchedulerError::TargetsQueriesMismatch { targets, queries }) => {
            assert_eq!(targets, 2);
            assert_eq!(queries, 1);
        }
        other => panic!("expected TargetsQueriesMismatch, got: {other:?}"),
    }
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
fn parent_routed_launch_without_parent_bases_uses_empty_fallback() {
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
